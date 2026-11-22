use fab_elec_mcu_avr::asm::{Asm, Target};
use fab_elec_mcu_avr::insn::Insn::*;
use fab_elec_mcu_avr::machine::AvrMachine;
use fab_elec_mcu_cosim::{
    device, CoMachine, Config, FwState, KeyPress, LogicKey, LogicKind, LogicMachine, LogicNet, LogicPart, LogicPin, LogicPlan,
    NetDriver,
};
use fab_elec_mcu_emu::{time, ChipState, Drive, DriveEdge, Machine, PcSet, PinUse, Ran, Until};
use fab_elec_sim::cosim::{Bus, CoRun, CoSim, CoSimDevice, CoSimPin, CoState, StepTo, Watch};
use fab_elec_sim::live::{LiveCmd, ValuePlan};
use fab_elec_sim::livesim::{LiveSim, LiveSpec, Snapshots};
use fab_elec_sim::trace::LogicEdge;
use fab_elec_sim::{GpioParams, PinMode, PinPull, Record, SimCircuit, SimDiag, Sink, Source, Wave};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const HZ: u64 = 16_000_000;
const WORD: u32 = 0xa3_5cd6;
const END: u64 = 1000;

fn plan() -> LogicPlan {
    let mut nets = ["PB3", "PB5", "PB2"].into_iter().map(|name| LogicNet {
        name: name.into(), driver: NetDriver::Machine(name.into()),
    }).collect::<Vec<_>>();
    for part in 0..2 {
        nets.push(LogicNet { name: format!("serial{part}"), driver: NetDriver::Part { part, pin: 13 } });
    }
    let parts = (0..3).map(|part| {
        let mut pins = vec![
            LogicPin::Net(if part == 0 { 0 } else { part + 2 }),
            LogicPin::Net(1), LogicPin::Net(2), LogicPin::Tied(false), LogicPin::Tied(true),
        ];
        pins.extend((0..8).map(|bit| LogicPin::Out(format!("Q{}", part * 8 + bit))));
        pins.push(if part < 2 { LogicPin::Net(part + 3) } else { LogicPin::Open });
        LogicPart { name: format!("latch{part}"), kind: LogicKind::ShiftLatch8, pins }
    }).collect();
    LogicPlan { parts, nets, ..LogicPlan::default() }
}

fn program(traffic: bool) -> (Vec<u16>, u32) {
    let mut a = Asm::new();
    a.emit(Ldi(16, 0x2c));
    a.emit(Sts(0x24, 16)); // PB2, PB3 and PB5 drive.
    a.emit(Ldi(16, 0x40));
    a.emit(Sts(0x2a, 16)); // PD6 drives.
    a.emit(Sbi(0x0b, 6));
    for bit in (0..24).rev() {
        if traffic {
            a.emit(if WORD & (1 << bit) == 0 { Cbi(0x05, 3) } else { Sbi(0x05, 3) });
            a.emit(Sbi(0x05, 5));
            a.emit(Cbi(0x05, 5));
        } else {
            for _ in 0..6 { a.emit(Nop); }
        }
    }
    let latch = a.here();
    a.emit(Sbi(0x05, 2));
    a.emit(Cbi(0x05, 2));
    a.emit(Cbi(0x0b, 6));
    a.emit(Break);
    (a.finish().expect("the bit-banging program assembles"), latch)
}

fn run(m: &mut impl Machine, tick: u64, on_drive: bool) -> Ran {
    m.run(&Until { tick, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive })
}

fn pin(m: &impl Machine, name: &str) -> usize {
    m.pins().iter().position(|p| *p == name).expect("the named pin is exposed")
}

#[test]
fn emitted_shift_out_latches_three_chained_parts() {
    use fab_elec_mcu_emit_avr::firmware_with_watch;
    use fab_elec_mcu_rt::watch::Level;
    let source = format!("let latch = output(PB2)\nlet data = output(PB3)\nlet clock = output(PB5)\nvar v: i32 = {WORD}\nfn setup() {{\nshift_out(data, clock, v, 24)\nwrite(latch, high)\nwrite(latch, low)\n}}\n");
    let fw = firmware_with_watch(&source, HZ as f64, &[], Level::Off).expect("real shift firmware builds");
    assert!(fw.debug.runtime.iter().any(|r| r.1 == "shift_out_spi"), "firmware links the hardware SPI stub");
    let mut m = LogicMachine::new(AvrMachine::new(&fw.words, HZ), plan()).expect("the existing chain plan accepts an emitted image");
    let result = run(&mut m, 30_000, false);
    assert_eq!(result, Ran::Tick, "firmware reaches its idle scheduler and the requested deadline");
    let mut edges = Vec::new();
    m.take_drives(&mut edges);
    let q_edges: Vec<_> = edges.iter().filter(|e| m.pins()[e.pin as usize].starts_with('Q')).collect();
    assert_eq!(q_edges.len(), WORD.count_ones() as usize, "only set bits change the initially low latch outputs");
    assert!(q_edges.iter().all(|e| e.tick == q_edges[0].tick && e.drive == Drive::High), "all 24 outputs commit together at the latch pulse");
    for bit in 0..24 {
        assert_eq!(m.drive(pin(&m, &format!("Q{bit}"))), if WORD & (1 << bit) == 0 { Drive::Low } else { Drive::High }, "chained Q{bit} holds value bit {bit}");
    }
}

#[test]
fn chained_latches_hide_shift_edges_and_rewind_exactly() {
    let (image, latch_pc) = program(true);
    let mut raw = AvrMachine::new(&image, HZ);
    let mut m = LogicMachine::new(raw.clone(), plan()).expect("the chain is valid");
    let mut replay = m.clone();
    let inner_names = raw.pins().to_vec();
    assert_eq!(run(&mut raw, END, false), Ran::Halted, "the reference program completes");
    let mut inner_edges = Vec::new();
    raw.take_drives(&mut inner_edges);
    let latch_tick = inner_edges.iter().find(|e| inner_names[e.pin as usize] == "PB2" && e.drive == Drive::High)
        .expect("the reference pulses the latch").tick;
    let expected_names = inner_names.iter().copied().filter(|p| !["PB2", "PB3", "PB5"].contains(p)).collect::<Vec<_>>();
    let first_q = expected_names.len();
    assert_eq!(&m.pins()[..first_q], expected_names, "unconnected inner pins retain their order");
    assert_eq!(m.pins().len(), first_q + 24, "all output pins follow the inner pins");
    for name in ["PB2", "PB3", "PB5"] {
        assert!(!m.pins().contains(&name), "digital driver {name} is hidden from the circuit");
    }
    for bit in 0..24 {
        assert_eq!(m.pins()[first_q + bit], format!("Q{bit}"), "outputs follow plan order");
        assert_eq!(m.drive(first_q + bit), Drive::Low, "output Q{bit} starts low");
    }
    let mut outward = Vec::new();
    let mut returns = Vec::new();
    let mut batch = Vec::new();
    loop {
        let generation = m.drive_gen();
        let ran = run(&mut m, END, true);
        m.take_drives(&mut batch);
        if let Ran::Drive { at } = ran {
            assert!(!batch.is_empty(), "each Drive return has outward edges");
            assert_eq!(at, batch[0].tick, "Drive reports the first outward edge's tick");
            assert_ne!(m.drive_gen(), generation, "outward edges advance the wrapper generation");
            returns.push(at);
        } else {
            assert_eq!(ran, Ran::Halted, "only outward edges interrupt the program");
        }
        outward.append(&mut batch);
        if ran == Ran::Halted { break; }
    }
    assert!(outward.windows(2).all(|e| e[0].tick <= e[1].tick), "outward edges are in tick order");
    let q_edges = outward.iter().filter(|e| usize::from(e.pin) >= first_q).copied().collect::<Vec<_>>();
    assert_eq!(q_edges.len(), WORD.count_ones() as usize, "only bits changing low to high emit edges");
    assert!(q_edges.iter().all(|e| e.tick == latch_tick && e.drive == Drive::High), "every output change occurs at the latch, none during shifting");
    for bit in 0..24 {
        assert_eq!(m.drive(first_q + bit), if WORD & (1 << bit) == 0 { Drive::Low } else { Drive::High }, "word bit {bit} is on Q{bit}");
    }
    let pd6 = pin(&m, "PD6") as u16;
    let passthrough = inner_edges.iter().filter(|e| inner_names[e.pin as usize] == "PD6")
        .map(|e| DriveEdge { pin: pd6, ..*e }).collect::<Vec<_>>();
    assert_eq!(outward.iter().filter(|e| e.pin == pd6).copied().collect::<Vec<_>>(), passthrough, "PD6 edges pass through with remapped pin indices");
    let mut expected_returns = passthrough.iter().map(|e| e.tick).collect::<Vec<_>>();
    expected_returns.push(latch_tick);
    expected_returns.sort_unstable();
    expected_returns.dedup();
    assert_eq!(returns, expected_returns, "shift data and clock traffic cause no Drive returns");
    assert_eq!(outward.len(), passthrough.len() + q_edges.len(), "only PD6 and latched outputs escape");
    assert_eq!(m.drive_gen(), outward.len() as u64, "hidden edges never advance the wrapper's generation");
    println!("logic counts: inner edges={}, outward edges={}, Ran::Drive returns={}", inner_edges.len(), outward.len(), returns.len());

    let mut marks = PcSet::new(latch_pc, 1);
    marks.insert(latch_pc);
    assert_eq!(replay.run(&Until { tick: END, marks: &marks, stops: &[latch_pc], stops_after: 0, on_drive: true }),
        Ran::Drive { at: passthrough[0].tick }, "the first pass-through edge is a stop");
    let mut marked_edges = Vec::new();
    replay.take_drives(&mut marked_edges);
    replay.mark();
    let generation = replay.drive_gen();
    assert_eq!(replay.run(&Until { tick: END, marks: &marks, stops: &[latch_pc], stops_after: 0, on_drive: false }), Ran::Pc, "a PC stop survives hidden traffic");
    // PD6's high edge remains untaken at this mark; all serial stages already hold the word.
    replay.mark();
    let marked_generation = replay.drive_gen();
    assert_ne!(marked_generation, generation, "PD6 changed before the latch mark");
    let marked_drives = (0..replay.pins().len()).map(|p| replay.drive(p)).collect::<Vec<_>>();
    marked_edges.clear();
    replay.take_drives(&mut marked_edges);
    assert_eq!(marked_edges, [passthrough[1]], "the mark includes an untaken edge");
    assert_eq!(run(&mut replay, END, false), Ran::Halted, "run past the latch without edge stops");
    let mut after = Vec::new();
    replay.take_drives(&mut after);
    replay.rewind();
    assert_eq!(replay.drive_gen(), marked_generation, "rewind restores the drive generation");
    assert_eq!((0..replay.pins().len()).map(|p| replay.drive(p)).collect::<Vec<_>>(), marked_drives, "rewind restores every output");
    let mut restored = Vec::new();
    replay.take_drives(&mut restored);
    assert_eq!(restored, marked_edges, "rewind restores untaken edges even after a drain");
    assert_eq!(run(&mut replay, END, false), Ran::Halted, "the repeated run completes");
    restored.clear();
    replay.take_drives(&mut restored);
    assert_eq!(restored, after, "the repeated latch produces identical edges at identical ticks");
    let mut chip = ChipState::default();
    replay.chip(&mut chip);
    assert_eq!(chip.pins.len(), replay.pins().len(), "chip pin metadata uses wrapper indices");
    assert_eq!(chip.pins[pd6 as usize].0, PinUse::Output, "PD6 metadata follows its remapped index");
    assert!(chip.pins[first_q..].iter().all(|p| p.0 == PinUse::Output), "latch outputs report their own use");

    let rebuilt = LogicMachine::new(AvrMachine::new(&image, HZ), plan()).expect("the same plan rebuilds");
    for bit in 0..24 {
        assert!(std::ptr::eq(m.pins()[first_q + bit], rebuilt.pins()[first_q + bit]), "Q{bit}'s name is interned once across builds");
    }
    let mut bad = plan();
    bad.parts[0].pins.pop();
    assert!(LogicMachine::new(AvrMachine::new(&image, HZ), bad).is_err(), "wrong part pin counts are rejected");
    let mut bad = plan();
    bad.parts[0].pins[0] = LogicPin::Net(99);
    assert!(LogicMachine::new(AvrMachine::new(&image, HZ), bad).is_err(), "out-of-range nets are rejected");
    let mut bad = plan();
    bad.parts[0].pins[5] = LogicPin::Net(0);
    assert!(LogicMachine::new(AvrMachine::new(&image, HZ), bad).is_err(), "a second driver is rejected");
    let mut bad = plan();
    bad.nets[0].driver = NetDriver::Machine("missing".into());
    assert!(LogicMachine::new(AvrMachine::new(&image, HZ), bad).is_err(), "unknown machine drivers are rejected");
    let mut bad = plan();
    bad.parts[0].pins[13] = LogicPin::Open;
    assert!(LogicMachine::new(AvrMachine::new(&image, HZ), bad).is_err(), "a disconnected declared driver is rejected");
}

#[derive(Default)]
struct Analog {
    t: f64,
    writes: Vec<(f64, u32, PinMode)>,
}

impl Bus for Analog {
    fn t(&self) -> f64 { self.t }
    fn v(&self, _pin: u32) -> f64 { 0.0 }
    fn supply(&self, _pin: u32) -> f64 { 5.0 }
    fn advance(&mut self, t: f64) -> Result<(), SimDiag> { self.t = self.t.max(t); Ok(()) }
    fn write(&mut self, pin: u32, mode: PinMode) -> bool {
        self.writes.push((self.t, pin, mode));
        true
    }
    fn hits(&self) -> &[(u32, f64)] { &[] }
}

#[test]
fn the_adapter_only_wakes_the_analog_for_exposed_effects() {
    let mut runs = Vec::new();
    for traffic in [false, true] {
        let (image, _) = program(traffic);
        let mut reference = AvrMachine::new(&image, HZ);
        assert_eq!(run(&mut reference, END, false), Ran::Halted, "the reference completes");
        let mut edges = Vec::new();
        reference.take_drives(&mut edges);
        let latch = edges.iter().find(|e| reference.pins()[e.pin as usize] == "PB2" && e.drive == Drive::High)
            .expect("the reference latches").tick;
        let m = LogicMachine::new(AvrMachine::new(&image, HZ), plan()).expect("the chain is valid");
        let pins = (0..24).map(|bit| format!("Q{bit}")).chain(["PD6".into()]).map(|name| CoSimPin {
            gpio: format!("logic#{name}.io"), pad: name.clone(), name,
            vss: "0".into(), vdd: "vdd".into(), vih: 0.7, vil: 0.3,
        }).collect();
        let mut d = device(m, Config { stretch: END, debug: None }, pins).expect("the wrapper is an ordinary co-simulation device");
        let mut analog = Analog::default();
        let mut visits = Vec::new();
        while d.dev.next().is_finite() {
            let next = d.dev.next();
            assert!(next >= analog.t, "the adapter never asks for the analog's past");
            analog.advance(next).expect("the analog advances to the requested instant");
            visits.push(next.to_bits());
            assert_eq!(d.dev.run(&mut analog).expect("the adapter runs"), CoRun::Go, "unarmed execution never stops the analog");
        }
        let latch_time = time(0.0, latch, HZ);
        let changes = analog.writes.iter().filter(|(t, row, _)| *t > 0.0 && *row < 24).collect::<Vec<_>>();
        assert_eq!(changes.len(), if traffic { WORD.count_ones() as usize } else { 0 }, "only latched word bits cause analog writes");
        assert!(changes.iter().all(|(t, _, mode)| t.to_bits() == latch_time.to_bits() && *mode == PinMode::OutHigh), "the analog sees every changed output exactly at the latch's tick");
        let snapshot = d.dev.state(analog.t).expect("the device publishes its state");
        let state = snapshot.get::<FwState>().expect("the wrapper uses the adapter's existing state");
        for bit in 0..24 {
            let output = state.pins.iter().find(|p| p.pin == format!("Q{bit}")).expect("the output has a state row");
            assert_eq!(output.level, Some(traffic && WORD & (1 << bit) != 0), "the analog publication holds Q{bit}");
        }
        let mut expected = vec![0.0f64.to_bits()];
        expected.extend(edges.iter().filter(|e| reference.pins()[e.pin as usize] == "PD6").map(|e| time(0.0, e.tick, HZ).to_bits()));
        if traffic { expected.push(latch_time.to_bits()); }
        expected.push(time(0.0, reference.now(), HZ).to_bits());
        expected.sort_unstable();
        expected.dedup();
        assert_eq!(visits, expected, "analog runs occur only at attach, PD6 edges, a changed latch and halt");
        runs.push((visits, latch_time.to_bits()));
    }
    assert_eq!(runs[1].0.len(), runs[0].0.len() + 1, "shift traffic adds only the one changed latch to the adapter's run count");
    let without_latch = runs[1].0.iter().copied().filter(|t| *t != runs[1].1).collect::<Vec<_>>();
    assert_eq!(without_latch, runs[0].0, "removing shift traffic leaves every other analog instant unchanged");
    println!("logic adapter runs: without shift traffic={}, with shift traffic={} (one changed latch)", runs[0].0.len(), runs[1].0.len());
}

const OTHER: u32 = 0x5c_a329;
const VDD: f64 = 5.0;
// The ATmega328P's typical thresholds at 5 V (the part's gpio card).
const VIH: f64 = 0.524;
const VIL: f64 = 0.418;
/// The button's press: during the fourth word's shifting, about 9.5 us a word.
const PRESS: f64 = 31e-6;
const LIVE_END: f64 = 62e-6;

/// Shifts a word to the chain and latches it, over and over: `WORD` while PD2 (pulled up) reads
/// high, `OTHER` once it reads low.
fn button_program() -> Vec<u16> {
    let mut a = Asm::new();
    a.emit(Ldi(16, 0x2c));
    a.emit(Sts(0x24, 16)); // PB2, PB3 and PB5 drive.
    a.emit(Ldi(16, 0x04));
    a.emit(Sts(0x2b, 16)); // PD2 pulled up.
    let shift = |a: &mut Asm, word: u32| {
        for bit in (0..24).rev() {
            a.emit(if word & (1 << bit) == 0 { Cbi(0x05, 3) } else { Sbi(0x05, 3) });
            a.emit(Sbi(0x05, 5));
            a.emit(Cbi(0x05, 5));
        }
        a.emit(Sbi(0x05, 2));
        a.emit(Cbi(0x05, 2));
    };
    let (top, other) = (a.label(), a.label());
    a.bind(top);
    a.emit(Sbis(0x09, 2)); // PIND2 high: skip the jump
    a.jump(Target::Rjmp, other);
    shift(&mut a, WORD);
    a.jump(Target::Rjmp, top);
    a.bind(other);
    shift(&mut a, OTHER);
    a.jump(Target::Rjmp, top);
    a.finish().expect("the button program assembles")
}

/// The adapter's re-runs: all of them, and those a sync made (a publication's read).
#[derive(Default)]
struct Counts {
    reruns: AtomicU64,
    syncs: AtomicU64,
}

/// The adapter as the live worker runs it, its re-runs counted; `settles` false drops the
/// worker's settle requests (`CoSim::settle`), so every read takes the unsettled path.
#[derive(Clone)]
struct Counted {
    dev: CoMachine<LogicMachine<AvrMachine>>,
    counts: Arc<Counts>,
    settles: bool,
}

impl Counted {
    fn counted(&self) {
        let stats = self.dev.stats();
        self.counts.reruns.store(stats.reruns, Ordering::Relaxed);
        self.counts.syncs.store(stats.sync_reruns, Ordering::Relaxed);
    }
}

impl CoSim for Counted {
    fn next(&self) -> f64 { self.dev.next() }
    fn fork(&self) -> Option<Box<dyn CoSim>> { Some(Box::new(self.clone())) }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        let ran = self.dev.run(bus);
        self.counted();
        ran
    }
    fn watches(&self) -> &[Watch] { self.dev.watches() }
    fn watch_gen(&self) -> u32 { self.dev.watch_gen() }
    fn arm(&mut self, t: f64, to: Option<StepTo>) -> bool { self.dev.arm(t, to) }
    fn settle(&mut self, want: bool) {
        if self.settles { self.dev.settle(want); }
    }
    fn settled(&self) -> bool { !self.settles || self.dev.settled() }
    fn state(&mut self, t: f64) -> Option<CoState> {
        let state = self.dev.state(t);
        self.counted();
        state
    }
    fn logic_nets(&self) -> &[String] { self.dev.logic_nets() }
    fn take_logic(&mut self, t: f64, sync: bool, out: &mut Vec<(u32, LogicEdge)>) {
        self.dev.take_logic(t, sync, out);
        self.counted();
    }
    fn restart_logic(&mut self, t: f64, out: &mut Vec<(u32, LogicEdge)>) { self.dev.restart_logic(t, out); }
}

/// What the live worker published of the chain's hidden nets (SER, SRCLK, RCLK), with the
/// analog's record, the adapter's re-runs and the steps the solver rejected at a crossing; after a
/// `Replace`, the new record's edges apart, and the instant it started.
struct LiveChain {
    edges: [Vec<LogicEdge>; 3],
    after: [Vec<LogicEdge>; 3],
    replaced: f64,
    record: Record,
    reruns: u64,
    crossings: u64,
}

/// The button program under the live worker, the button pressed at `PRESS`: a trace delta taken
/// after each of `slices` in turn (none: one delta at the end), and the same circuit and plan
/// replaced as a new record once past `replace_at`. Every delta's edges lie at or before its last
/// point.
fn live_chain(slices: &[f64], replace_at: Option<f64>) -> LiveChain {
    let (mut sim, mut record, counts, c, values) = chain(true, 400);
    let (mut edges, mut after): ([Vec<LogicEdge>; 3], [Vec<LogicEdge>; 3]) = Default::default();
    let mut take = |sim: &mut LiveSim| {
        let Some(d) = sim.take_delta() else { return };
        let head = *d.time.last().expect("a delta holds points");
        for (id, found) in &d.logic {
            let Some(net) = (id.0 as usize).checked_sub(1).filter(|&net| net < 3) else { continue };
            assert!(found.iter().all(|e| e.t <= head), "a delta's edges lie at or before its end {head}");
            let record = if d.generation == 1 { &mut edges } else { &mut after };
            record[net].extend_from_slice(found);
        }
    };
    let mut replaced = f64::NAN;
    if slices.is_empty() {
        sim.advance(LIVE_END, None, &mut record).expect("the bench steps");
    } else {
        for &slice in slices.iter().cycle() {
            if sim.t() >= LIVE_END {
                break;
            }
            let target = (sim.t() + slice).min(LIVE_END);
            sim.advance(target, Some(Duration::ZERO), &mut record).expect("the bench steps in slices");
            take(&mut sim);
            if replace_at.is_some_and(|at| sim.t() >= at) && replaced.is_nan() {
                replaced = sim.t();
                sim.apply(LiveCmd::Replace { generation: 2, circuit: Box::new(c.clone()), plan: Box::new(values.clone()) });
            }
        }
    }
    take(&mut sim);
    LiveChain { edges, after, replaced, record, reruns: counts.reruns.load(Ordering::Relaxed), crossings: sim.solver().stats.rejected_crossing }
}

/// The button program's live bench, the button pressed at `PRESS`: the analog's record from the
/// operating point, the adapter (`stretch` ticks; settling as `settles`) and its re-run counts,
/// with the circuit and plan; a snapshot is due at every loop top it is asked for.
fn chain(settles: bool, stretch: u64) -> (LiveSim, Record, Arc<Counts>, SimCircuit, ValuePlan) {
    let params = GpioParams { r_on_h: 26.0, r_on_l: 23.5, r_pull: 36e3, r_leak: 5.5e6, t_edge: 5e-9, vih: VIH, vil: VIL };
    let mut c = SimCircuit::new();
    c.v("supply", "vdd", "0", Source::dc(VDD));
    c.gpio("u1#PD2.io", "btn", "vdd", "0", params, PinMode::Input(PinPull::None));
    c.c("cbtn", "btn", "0", 5e-12);
    c.v("key", "k", "0", Source::wave(Wave::Pwl(vec![(0.0, VDD), (PRESS, VDD), (PRESS + 0.2e-6, 0.0)])));
    c.r("button", "btn", "k", 1e3);
    c.opts.chgtol = 1e-20;
    c.opts.vntol = 1e-9;
    c.opts.abstol = 1e-15;
    let snapshots = Snapshots { span: 1.0, every: Duration::ZERO, share: f64::INFINITY, ..Snapshots::default() };
    // a settle request waits for the device whatever the wall clock does
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, reltol: Some(1e-5), max_step: 2e-6, trace: true, snapshots, slice: Duration::from_secs(3600), ..LiveSpec::default() };
    // the hidden nets are signals 1..=3 of the plan, by name; no circuit node carries them
    let values = ValuePlan { nets: ["btn", "PB3", "PB5", "PB2"].map(String::from).to_vec(), ..ValuePlan::default() };
    let mut sim = LiveSim::new(c.clone(), values.clone(), 1, spec).expect("the operating point converges");
    sim.values(1.0);
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    record.point(sim.t(), None, &sim.solver().x);
    let m = LogicMachine::new(AvrMachine::new(&button_program(), HZ), plan()).expect("the chain is valid");
    let pins = vec![CoSimPin { name: "PD2".into(), gpio: "u1#PD2.io".into(), pad: "btn".into(), vss: "0".into(), vdd: "vdd".into(), vih: VIH, vil: VIL }];
    let dev = CoMachine::new(m, Config { stretch, debug: None }, &pins).expect("a valid device");
    let counts = Arc::new(Counts::default());
    sim.attach(Some(CoSimDevice { dev: Box::new(Counted { dev, counts: counts.clone(), settles }), pins }));
    (sim, record, counts, c, values)
}

/// A chain's hidden nets reach the live worker's trace committed: the analog cuts a step to the
/// button's crossing and the adapter rewinds the CPU, which had run past it, and replays the
/// stretch, yet each net's edges come out once, in time order, each a change; the words they spell
/// are `WORD` until the press is read and `OTHER` after it, never a word of the discarded run; a
/// delta taken at any cadence holds the same edges, and leaves the analog's points as they are;
/// and a record replaced mid-run starts with every hidden net at its level there, then goes on with
/// the same edges.
#[test]
fn hidden_nets_reach_the_trace_once_and_only_from_committed_runs() {
    let whole = live_chain(&[], None);
    let slices = [0.7e-6, 1.3e-6, 0.31e-6];
    let sliced = live_chain(&slices, None);
    for run in [&whole, &sliced] {
        assert!(run.reruns >= 1, "the CPU had run past the button's crossing and re-ran to it");
        assert!(run.crossings >= 1, "the analog rejected a step across the button's crossing and retook it");
    }
    assert_eq!(sliced.edges, whole.edges, "a delta at any cadence publishes the same edges");
    assert_eq!(sliced.record.at, whole.record.at, "the same accepted times however often a delta goes out");
    assert_eq!(sliced.record.x, whole.record.x, "the same solutions however often a delta goes out");
    for (net, name) in ["SER", "SRCLK", "RCLK"].into_iter().enumerate() {
        let e = &whole.edges[net];
        assert_eq!(e.first().map(|e| (e.t, e.high)), Some((0.0, false)), "{name} starts low at the attach");
        assert!(e.windows(2).all(|w| w[0].t < w[1].t && w[0].high != w[1].high), "{name}: each edge once, in time order, each a change");
    }
    let [ser, clk, latch] = &whole.edges;
    let level = |t: f64| ser[..ser.partition_point(|e| e.t <= t)].last().is_some_and(|e| e.high);
    let mut words = Vec::new();
    let (mut word, mut bits, mut clocks) = (0u32, 0, clk.iter().filter(|e| e.high).peekable());
    for rise in latch.iter().filter(|e| e.high) {
        while let Some(c) = clocks.next_if(|c| c.t < rise.t) {
            word = word << 1 | u32::from(level(c.t));
            bits += 1;
        }
        assert_eq!(bits, 24, "a latch follows 24 clocks");
        words.push(word & 0xff_ffff);
        (word, bits) = (0, 0);
    }
    let first_other = words.iter().position(|&w| w == OTHER).expect("the press is read");
    assert!(first_other >= 2, "the words before the press: {words:06x?}");
    assert!(words[..first_other].iter().all(|&w| w == WORD) && words[first_other..].iter().all(|&w| w == OTHER),
        "WORD until the press is read, OTHER after it: {words:06x?}");
    assert!(words.len() - first_other >= 2, "the words after the press: {words:06x?}");

    let replaced = live_chain(&slices, Some(PRESS / 2.0));
    let at = replaced.replaced;
    for (net, name) in ["SER", "SRCLK", "RCLK"].into_iter().enumerate() {
        let (before, after) = (&replaced.edges[net], &replaced.after[net]);
        let level = whole.edges[net][..whole.edges[net].partition_point(|e| e.t <= at)].last().expect("a level before the replace").high;
        assert_eq!(after.first().copied(), Some(LogicEdge { t: at, high: level }), "{name}: the new record starts at its level at the replace");
        assert_eq!(before[..], whole.edges[net][..whole.edges[net].partition_point(|e| e.t <= at)], "{name}: the old record holds every edge up to the replace");
        assert_eq!(after[1..], whole.edges[net][whole.edges[net].partition_point(|e| e.t <= at)..], "{name}: then the same edges as an unreplaced run");
    }
}

/// What the chain published at a cadence: the hidden nets' edges, the analog's record, the device's
/// state at the end, its sync re-runs (all, and those of publications after an advance that ended
/// short of the run's end), the settle requests the worker withdrew, and the publications made.
struct Cadence {
    edges: [Vec<LogicEdge>; 3],
    record: Record,
    end: (u64, u32, Vec<(&'static str, u32)>),
    syncs: u64,
    settled_syncs: u64,
    /// The most re-runs one publication made.
    most: u64,
    unsettled: u64,
    publications: u64,
}

fn take_chain(sim: &mut LiveSim, edges: &mut [Vec<LogicEdge>; 3]) {
    let Some(d) = sim.take_delta() else { return };
    for (id, found) in &d.logic {
        let Some(net) = (id.0 as usize).checked_sub(1).filter(|&net| net < 3) else { continue };
        edges[net].extend_from_slice(found);
    }
}

/// The chain played to `LIVE_END` by advances with no wall budget; after each return `publish`
/// says whether a publication follows: values (its device state of its own instant), a trace
/// delta and a kept snapshot. The device's stretch is 100 ticks, several in the run.
fn cadence(settles: bool, publish: &mut dyn FnMut() -> bool) -> Cadence {
    let (mut sim, mut record, counts, _, _) = chain(settles, 100);
    assert!(sim.apply(LiveCmd::Play), "the run plays");
    let mut edges: [Vec<LogicEdge>; 3] = Default::default();
    let (mut settled_syncs, mut most, mut publications) = (0, 0, 0);
    while sim.t() < LIVE_END {
        let from = sim.t();
        sim.advance(LIVE_END, Some(Duration::ZERO), &mut record).expect("the bench steps");
        // a read leaves the device on its mark, to run at the present: the next advance still moves
        assert!(sim.t() > from, "an advance with no wall budget moves the run (at {from})");
        if !publish() {
            continue;
        }
        publications += 1;
        let before = counts.syncs.load(Ordering::Relaxed);
        let v = sim.values(1.0);
        let state = v.co.as_ref().and_then(|c| c.get::<FwState>()).expect("a publication carries the device's state");
        assert_eq!(state.t.to_bits(), v.t.to_bits(), "a publication's state is of its own instant");
        take_chain(&mut sim, &mut edges);
        sim.keep_snapshot(&record);
        let syncs = counts.syncs.load(Ordering::Relaxed) - before;
        most = most.max(syncs);
        if sim.t() < LIVE_END {
            settled_syncs += syncs;
        }
    }
    take_chain(&mut sim, &mut edges);
    let v = sim.values(1.0);
    let s = v.co.as_ref().and_then(|c| c.get::<FwState>()).expect("the last publication carries the device's state");
    Cadence {
        edges, record, end: (s.cycle, s.pc, s.regs.clone()), syncs: counts.syncs.load(Ordering::Relaxed), settled_syncs, most,
        unsettled: sim.stats().co_unsettled, publications,
    }
}

/// Publications at any cadence (none until the end, after every return of an advance with no
/// wall budget, after a seeded pattern of one to four returns), each a values with the device's
/// state, a trace delta and a kept snapshot, leave the run as it is with the device's settling
/// (`CoSim::settle`) on and off: the same hidden-net edges, accepted points and final device
/// state, by bits. Settling, an advance ends where the device stands committed, so a publication
/// re-runs nothing and only the run's end withdraws a request; unsettled, a publication that finds
/// the device ahead re-runs its private stretch once: the first read's sync settles it.
#[test]
fn publications_at_any_cadence_settle_the_device_and_change_nothing() {
    let bits = |r: &Record| -> Vec<u64> { r.at.iter().chain(r.x.iter().flatten()).map(|v| v.to_bits()).collect() };
    let mut runs = Vec::new();
    for settles in [true, false] {
        for arm in 0..3 {
            let (mut n, mut wait, mut seed) = (0u32, 1u32, 0x2545_f491_u32);
            let mut publish = || match arm {
                0 => false,
                1 => true,
                _ => {
                    n += 1;
                    if n < wait {
                        return false;
                    }
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (n, wait) = (0, 1 + (seed >> 30));
                    true
                }
            };
            runs.push((settles, arm, cadence(settles, &mut publish)));
        }
    }
    let want = &runs[0].2;
    let want_bits = bits(&want.record);
    for (settles, arm, run) in &runs {
        let name = format!("{} arm {arm}", if *settles { "settling" } else { "unsettled" });
        assert_eq!(run.edges, want.edges, "{name}: the same hidden-net edges");
        assert!(bits(&run.record) == want_bits, "{name}: the same accepted points ({} against {})", run.record.at.len(), want.record.at.len());
        assert_eq!(run.end, want.end, "{name}: the device ends in the same state");
        if *arm > 0 {
            assert!(run.publications >= 4, "{name}: {} publications", run.publications);
        }
        assert!(run.most <= 1, "{name}: a publication re-runs the device at most once ({})", run.most);
        if *settles {
            assert_eq!(run.settled_syncs, 0, "{name}: a publication after a settled advance re-runs nothing");
            assert!(run.unsettled <= 1, "{name}: only the run's end withdraws a settle request ({})", run.unsettled);
        } else if *arm == 1 {
            assert!(run.syncs > 0, "{name}: unsettled, publications re-run ({} for {})", run.syncs, run.publications);
        }
    }
}

/// A key matrix of one key: PB0 reads a row on its pull-up, PB1 holds a column low, and a key
/// with its diode (anode on the row) joins them; the firmware mirrors PB0 on PD6. Pressed, the
/// row reads low at once; released, it rises on its pull after its release ticks; a key whose
/// diode faces the other way never pulls the row low; and a rewind undoes a press.
#[test]
fn a_pressed_key_pulls_its_row_low_and_a_release_rises_on_the_pull() {
    let mut a = Asm::new();
    a.emit(Ldi(16, 0x02));
    a.emit(Sts(0x24, 16)); // PB1 drives
    a.emit(Ldi(16, 0x01));
    a.emit(Sts(0x25, 16)); // PB0 pulled up, PB1 low
    a.emit(Ldi(16, 0x40));
    a.emit(Sts(0x2a, 16)); // PD6 drives
    let (top, low) = (a.label(), a.label());
    a.bind(top);
    a.emit(Sbis(0x03, 0)); // PINB0 high: skip the jump
    a.jump(Target::Rjmp, low);
    a.emit(Sbi(0x0b, 6));
    a.jump(Target::Rjmp, top);
    a.bind(low);
    a.emit(Cbi(0x0b, 6));
    a.jump(Target::Rjmp, top);
    let words = a.finish().expect("the mirror program assembles");
    const RELEASE: u64 = 400;
    let machine = |diode: bool| {
        let nets = ["PB0", "PB1"].into_iter().map(|name| LogicNet { name: name.into(), driver: NetDriver::Machine(name.into()) }).collect();
        let keys = vec![LogicKey { name: "k".into(), a: 0, b: 1, diode: Some(diode), pressed: false }];
        let plan = LogicPlan { nets, keys, release: vec![(0, RELEASE)], ..LogicPlan::default() };
        LogicMachine::new(AvrMachine::new(&words, HZ), plan).expect("a one-key matrix")
    };
    let high = |m: &LogicMachine<AvrMachine>| m.drive(pin(m, "PD6")) == Drive::High;
    let mut m = machine(true);
    assert!(m.pins().iter().all(|p| *p != "PB0" && *p != "PB1"), "the matrix's pins are hidden");
    // the pull-up charges the row from power-up: a release too
    run(&mut m, RELEASE / 2, false);
    assert!(!high(&m), "the row has not risen on its pull-up before its release ticks");
    run(&mut m, RELEASE + 200, false);
    assert!(high(&m), "the row reads its pull-up");
    m.mark();
    let marked = m.now();
    assert!(m.host_input(&KeyPress { key: "k".into(), down: true }), "the plan's key takes a press");
    run(&mut m, marked + 60, false);
    assert!(!high(&m), "pressed, the row reads the column's low through the key");
    assert!(m.host_input(&KeyPress { key: "k".into(), down: false }));
    let up = m.now();
    run(&mut m, up + RELEASE - 40, false);
    assert!(!high(&m), "released, the row still reads low before its release ticks");
    run(&mut m, up + RELEASE + 60, false);
    assert!(high(&m), "after its release ticks the row reads its pull-up again");
    m.rewind();
    run(&mut m, marked + 60, false);
    assert!(high(&m), "a rewind to before the press undoes it");
    assert!(!m.host_input(&KeyPress { key: "other".into(), down: true }), "a key the plan lacks is not taken");
    let mut m = machine(false);
    run(&mut m, RELEASE + 200, false);
    assert!(m.host_input(&KeyPress { key: "k".into(), down: true }));
    let pressed = m.now();
    run(&mut m, pressed + 60, false);
    assert!(high(&m), "a diode facing the row passes no current from it to the low column");
}

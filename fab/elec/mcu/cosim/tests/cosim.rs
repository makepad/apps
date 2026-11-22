//! The adapter on the ATmega328P oracle, against a scripted analog and the live worker: drive edges
//! at their own ticks, an input re-executed to its cycle, steps that land bit-exact, a fault that
//! never stops the analog, an image swapped in mid-run (Q6 8A.6; E2E A3, A4, A5, A9).

use fab_elec_mcu_avr::asm::{Asm, Target};
use fab_elec_mcu_avr::insn::Insn::*;
use fab_elec_mcu_avr::machine::AvrMachine;
use fab_elec_mcu_avr::sim::{Cpu, Event};
use fab_elec_mcu_cosim::{device, pin_rows, At, CoMachine, Config, FaultCell, FaultCount, FwCond, FwDebug, FwFault, FwFrame, FwState, FwValue, PinUse, RadioStarts, StepMap};
use fab_elec_mcu_emu::{FwFaultCell, FwStmt, FwTy, FwVar};
use fab_elec_mcu_rt::watch;
use fab_elec_mcu_emu::{tick_at, time, Drive, ExecCounts, HaltKind, Machine, PcSet, Ran, Until};
use fab_elec_sim::cosim::{Bus, CoRun, CoSim, CoSimPin, StepTo};
use fab_elec_sim::live::{LiveCmd, ValuePlan};
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::{GpioParams, ModelKind, PinEdge, PinMode, PinPull, Record, SimCircuit, SimDiag, Sink, Source, Wave};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HZ: u64 = 16_000_000;
const VDD: f64 = 5.0;
// Package GPIO indices (`AvrMachine::pins`).
const PB3: usize = 3;
const PB5: usize = 5;
const PD2: usize = 14;
const PD6: usize = 18;
// The ATmega328P's typical thresholds at 5 V (the part's gpio card).
const VIH: f64 = 0.524;
const VIL: f64 = 0.418;

fn at(t_reset: f64, tick: u64) -> f64 {
    time(t_reset, tick, HZ)
}

fn store(a: &mut Asm, addr: u16, value: u8) {
    a.emit(Ldi(16, value));
    a.emit(Sts(addr, 16));
}

fn row(name: &str, pad: &str) -> CoSimPin {
    CoSimPin { name: name.into(), gpio: format!("u1#{name}.io"), pad: pad.into(), vss: "0".into(), vdd: "vdd".into(), vih: VIH, vil: VIL }
}

/// Every machine pin a row, in the machine's order, so a row is its pin index.
fn dev(image: &[u16], map: Option<StepMap>) -> CoMachine<AvrMachine> {
    let m = AvrMachine::new(image, HZ);
    let pins: Vec<CoSimPin> = m.pins().iter().map(|&name| row(name, &name.to_lowercase())).collect();
    CoMachine::new(m, Config { stretch: 4000, debug: map.map(table) }, &pins).expect("a valid device")
}

/// An image table for a hand-assembled program: statement `k` begins at the map's `k`-th address,
/// and the one variable is the i32 at data address 0x100.
fn table(map: StepMap) -> std::sync::Arc<FwDebug> {
    let stmts = map.statements.iter().map(|&addr| FwStmt { addr: Some(addr), span: (0, 0), line: 0, col: 0, owner: fab_elec_mcu_emu::Owner::Handler(0) }).collect();
    let starts = map.statements.iter().enumerate().map(|(k, &addr)| (addr, vec![k as u32])).collect();
    let vars = vec![FwVar { name: "n".into(), ty: FwTy::I32, addr: 0x100, span: (0, 0) }];
    std::sync::Arc::new(FwDebug { map: Some(map), stmts, starts, vars, ..FwDebug::default() })
}

fn snapshot(dev: &mut dyn CoSim, t: f64) -> FwState {
    dev.state(t).expect("an attached device has a state").get::<FwState>().expect("the adapter's state").clone()
}

/// A scripted analog in place of the live worker: each pad sits at its scripted voltage, the rails
/// at 5 V. It runs the device at its instants and reports a scripted input step that crosses a
/// watch `lag` after the crossing (never past the device's next instant), as the stepper does.
struct Script {
    t: f64,
    v: Vec<f64>,
    /// (time, row, volts), in time order.
    steps: VecDeque<(f64, u32, f64)>,
    hits: Vec<(u32, f64)>,
    writes: Vec<(f64, u32, PinMode)>,
    lag: f64,
}

impl Bus for Script {
    fn t(&self) -> f64 {
        self.t
    }
    fn v(&self, pin: u32) -> f64 {
        self.v[pin as usize]
    }
    fn supply(&self, _pin: u32) -> f64 {
        VDD
    }
    fn advance(&mut self, t: f64) -> Result<(), SimDiag> {
        self.t = self.t.max(t);
        Ok(())
    }
    fn write(&mut self, pin: u32, mode: PinMode) -> bool {
        self.writes.push((self.t, pin, mode));
        true
    }
    fn hits(&self) -> &[(u32, f64)] {
        &self.hits
    }
}

impl Script {
    fn new(t: f64) -> Script {
        Script { t, v: vec![VDD; 22], steps: VecDeque::new(), hits: Vec::new(), writes: Vec::new(), lag: 0.0 }
    }

    /// The worker's attach: the device runs at once.
    fn attach(&mut self, dev: &mut dyn CoSim) {
        assert_eq!(dev.run(self).expect("the attach runs"), CoRun::Go, "an attach is no stop");
    }

    /// Runs the device until its next instant passes `end` (None) or a step stops it (its time).
    fn play(&mut self, dev: &mut dyn CoSim, end: f64) -> Option<f64> {
        loop {
            let next = dev.next();
            assert!(next >= self.t, "the device never asks for the analog's past: {next:e} < {:e}", self.t);
            match self.steps.front().copied() {
                Some((ts, row, volts)) if ts < next && ts <= end => {
                    self.steps.pop_front();
                    let old = std::mem::replace(&mut self.v[row as usize], volts);
                    let crossed = |level: f64, edge: PinEdge| match edge {
                        PinEdge::Rising => old < level && volts >= level,
                        PinEdge::Falling => old > level && volts <= level,
                        PinEdge::Both => (old < level) != (volts < level),
                    };
                    self.hits = dev.watches().iter().filter(|w| w.pin == row && crossed(w.level, w.edge)).map(|w| (w.id, ts)).collect();
                    if self.hits.is_empty() {
                        continue;
                    }
                    self.t = (ts + self.lag).min(next);
                }
                _ if next <= end => self.t = next,
                _ => {
                    // The analog reaches the end; the device is consulted at its next instant.
                    if end.is_finite() {
                        self.t = self.t.max(end);
                    }
                    return None;
                }
            }
            let ran = dev.run(self).expect("the device runs");
            self.hits.clear();
            if ran == CoRun::Stopped {
                return Some(self.t);
            }
        }
    }

    fn row(&self, pin: usize) -> Vec<(f64, PinMode)> {
        self.writes.iter().filter(|w| w.1 == pin as u32).map(|w| (w.0, w.2)).collect()
    }
}

/// Timer0 and Timer2 drive OC0A (PD6) and OC2A (PB3) at clk/1, Timer2 started three cycles later
/// with a compare two lower, so their compare edges fall one cycle apart; then a delay loop, a
/// mid-period OCR0A change, and an idle loop of three-cycle JMPs.
fn pwm_program(phase_correct: bool) -> Vec<u16> {
    let mut a = Asm::new();
    store(&mut a, 0x2a, 0x40); // DDRD: PD6 drives
    store(&mut a, 0x24, 0x08); // DDRB: PB3 drives
    store(&mut a, 0x47, 100); // OCR0A
    store(&mut a, 0xb3, 98); // OCR2A
    let wgm = if phase_correct { 0x81 } else { 0x83 }; // COMxA1 and WGM 1 (phase correct) or 3 (fast)
    store(&mut a, 0x44, wgm);
    store(&mut a, 0xb0, wgm);
    store(&mut a, 0x45, 1); // TCCR0B: clk/1
    store(&mut a, 0xb1, 1); // TCCR2B: clk/1
    a.emit(Ldi(18, 250));
    let delay = a.label();
    a.bind(delay);
    a.emit(Dec(18));
    a.jump(Target::Brbc(1), delay);
    store(&mut a, 0x47, 40);
    let idle = a.label();
    a.bind(idle);
    a.jump(Target::Jmp, idle);
    a.finish().expect("the PWM program assembles")
}

/// PB5 follows PD2 (pulled up) in a polling loop; the address of the branch that drives it low.
fn poll_program() -> (Vec<u16>, u32) {
    let mut a = Asm::new();
    store(&mut a, 0x24, 0x20); // DDRB: PB5 drives
    store(&mut a, 0x2b, 0x04); // PORTD: PD2 pulled up
    let (top, low) = (a.label(), a.label());
    a.bind(top);
    a.emit(Sbis(0x09, 2)); // PIND2 high: skip the jump
    a.jump(Target::Rjmp, low);
    a.emit(Sbi(0x05, 5));
    a.jump(Target::Rjmp, top);
    a.bind(low);
    let low_addr = a.here();
    a.emit(Cbi(0x05, 5));
    a.jump(Target::Rjmp, top);
    (a.finish().expect("the polling program assembles"), low_addr)
}

/// Timer0 overflows (every 16384 cycles at clk/64) wake a sleeping loop; the ISR sets a flag and
/// the loop calls a three-statement handler (the last toggles PB5) from one call site.
fn step_program() -> (Vec<u16>, StepMap) {
    let mut a = Asm::new();
    let (setup, isr, handler, top) = (a.label(), a.label(), a.label(), a.label());
    a.jump(Target::Jmp, setup);
    a.org(32); // TIMER0_OVF
    a.jump(Target::Jmp, isr);
    a.org(64);
    a.bind(setup);
    store(&mut a, 0x24, 0x20); // DDRB: PB5 drives
    store(&mut a, 0x53, 0x01); // SMCR: idle sleep enabled
    store(&mut a, 0x6e, 0x01); // TIMSK0: overflow interrupt
    store(&mut a, 0x45, 0x03); // TCCR0B: clk/64
    a.emit(Bset(7));
    a.bind(top);
    a.emit(Sleep);
    a.emit(And(20, 20));
    a.jump(Target::Brbs(1), top); // BREQ: no flag
    a.emit(Ldi(20, 0));
    a.jump(Target::Call, handler);
    let ret = a.here();
    a.jump(Target::Rjmp, top);
    a.bind(handler);
    let h = a.here();
    a.emit(Inc(17));
    a.emit(Sts(0x100, 17));
    a.emit(Sbi(0x03, 5)); // PINB: toggles PB5
    a.emit(Ret);
    a.bind(isr);
    a.emit(Ldi(20, 1));
    a.emit(Reti);
    let map = StepMap { funcs: Vec::new(), calls: Vec::new(), owners: Vec::new(), statements: vec![h, h + 1, h + 3], entries: vec![(h, 0)], returns: vec![ret] };
    (a.finish().expect("the step program assembles"), map)
}

/// The image on a bare machine, every pin sampled high at its reset as the attach samples a 5 V pad.
fn direct(image: &[u16]) -> AvrMachine {
    let mut m = AvrMachine::new(image, HZ);
    for pin in 0..m.pins().len() {
        m.set_input(pin, true);
    }
    m
}

fn run_to(m: &mut AvrMachine, tick: u64) {
    assert_eq!(m.run(&Until { tick, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false }), Ran::Tick);
}

/// Q6 8A.6: every PWM edge, fast and phase-correct (with a duty change mid-period), reaches the
/// analog at its own tick's time, bit-exact, including two edges inside one three-cycle JMP.
#[test]
fn drive_edges_land_on_their_own_ticks() {
    let end = 3000;
    for (phase_correct, t0) in [(false, 0.0), (true, 3.7e-6)] {
        let image = pwm_program(phase_correct);
        let mut raw = Cpu::new(&image);
        let _ = raw.run(end);
        let edges: Vec<(u64, usize, bool)> = raw.events.iter().filter_map(|e| match *e {
            Event::Pin { cycle, port: 'B', bit: 3, high } if cycle <= end => Some((cycle, PB3, high)),
            Event::Pin { cycle, port: 'D', bit: 6, high } if cycle <= end => Some((cycle, PD6, high)),
            _ => None,
        }).collect();
        let mut d = dev(&image, None);
        let mut bus = Script::new(t0);
        bus.attach(&mut d);
        assert_eq!(bus.play(&mut d, at(t0, end)), None, "no step is armed");
        for pin in [PB3, PD6] {
            let got = bus.row(pin);
            assert_eq!(got[0], (t0, PinMode::Input(PinPull::None)), "pin {pin}: at the attach, its reset drive");
            assert_eq!(got[1].1, PinMode::OutLow, "pin {pin}: its DDR write drives it low");
            let want: Vec<(u64, PinMode)> = edges.iter().filter(|e| e.1 == pin)
                .map(|e| (at(t0, e.0).to_bits(), if e.2 { PinMode::OutHigh } else { PinMode::OutLow })).collect();
            let got: Vec<(u64, PinMode)> = got[2..].iter().map(|w| (w.0.to_bits(), w.1)).collect();
            assert!(want.len() >= 10, "pin {pin}: the oracle toggles it: {want:?}");
            assert_eq!(got, want, "pin {pin} (phase correct {phase_correct}): every edge at its own tick, bit-exact");
        }
        assert!(edges.windows(2).any(|w| w[0].1 != w[1].1 && w[1].0 - w[0].0 < 3),
            "two edges of different pins fall inside one three-cycle instruction: {edges:?}");
        if phase_correct {
            let pd6: Vec<&(u64, usize, bool)> = edges.iter().filter(|e| e.1 == PD6).collect();
            let mut widths: Vec<u64> = pd6.windows(2).filter(|w| w[0].2 && !w[1].2).map(|w| w[1].0 - w[0].0).collect();
            widths.sort_unstable();
            widths.dedup();
            assert!(widths.len() >= 2, "the duty change moved both edges: high widths {widths:?}");
        }
    }
}

/// Q6 8A.6 and ESIM rule 3: a falling edge on PD2 reaches the CPU at the first boundary at or
/// after `ceil(max(hit, bus.t()))`, after the CPU had run past it privately, both with the analog
/// standing on the crossing and past it; PB5 follows exactly as when the edge is scheduled ahead.
#[test]
fn an_input_edge_reaches_the_cpu_at_its_cycle_after_a_rerun() {
    let (image, _) = poll_program();
    let t0 = 0.0;
    let h = at(t0, 1234) + 0.4 / HZ as f64;
    for lag in [0.0, 7.3] {
        let mut d = dev(&image, None);
        let mut bus = Script::new(t0);
        bus.lag = lag / HZ as f64;
        bus.steps.push_back((h, PD2 as u32, 0.0));
        bus.attach(&mut d);
        assert_eq!(bus.play(&mut d, at(t0, 3000)), None, "no step is armed");
        let c = tick_at(t0, h + bus.lag, HZ);
        let mut m = direct(&image);
        run_to(&mut m, c);
        m.take_drives(&mut Vec::new()); // the DDR write's drive, before the edge
        m.set_input(PD2, false);
        let mut edges = Vec::new();
        let low = loop {
            let _ = m.run(&Until { tick: c + 100, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: true });
            m.take_drives(&mut edges);
            if let Some(e) = edges.iter().find(|e| e.pin == PB5 as u16 && e.drive == Drive::Low) {
                break e.tick;
            }
            assert!(m.now() < c + 100, "the scheduled edge drives PB5 low");
        };
        let pb5 = bus.row(PB5);
        assert_eq!(pb5.last().map(|w| (w.0.to_bits(), w.1)), Some((at(t0, low).to_bits(), PinMode::OutLow)),
            "lag {lag} cycles: PB5 falls where the scheduled run has it: {pb5:?}");
        assert_eq!(pb5.iter().filter(|w| w.1 == PinMode::OutLow).count(), 2, "lag {lag}: the DDR write and the one fall, never twice");
        assert!(d.stats().reruns >= 1, "lag {lag}: the CPU had run past the edge and re-ran to it");
        let state = d.state(bus.t).expect("an attached device publishes");
        let mut rows = Vec::new();
        pin_rows(&state, &mut |name, pin_use, level, duty| rows.push((name.to_string(), pin_use, level, duty)));
        assert_eq!(rows.len(), 22, "lag {lag}: one row per machine pin");
        assert!(rows.contains(&("PB5".into(), PinUse::Output, Some(false), 0.0)), "lag {lag}: PB5 drives low: {rows:?}");
        assert!(rows.contains(&("PD2".into(), PinUse::PullUp, Some(false), 0.0)), "lag {lag}: PD2 samples its pressed pad: {rows:?}");
    }
}

/// A sink that keeps what it takes (dropping the void events an offer names first), refusing the
/// first of every `refuse` offers (0: never).
fn sink(refuse: usize) -> (fab_elec_mcu_cosim::EventSink, Arc<Mutex<Vec<(f64, FwFrame)>>>, Arc<Mutex<usize>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let refused = Arc::new(Mutex::new(0));
    let (g, r) = (got.clone(), refused.clone());
    let mut offers = 0;
    let sink: fab_elec_mcu_cosim::EventSink = Box::new(move |void: u64, batch: &mut Vec<(f64, FwFrame)>| {
        offers += 1;
        if refuse != 0 && (offers - 1) % refuse == 0 {
            *r.lock().unwrap() += 1;
            return false;
        }
        let mut got = g.lock().unwrap();
        let kept = got.len() - (void as usize).min(got.len());
        got.truncate(kept);
        got.append(batch);
        true
    });
    (sink, got, refused)
}

/// Rule 8 and the no-loss rule: the trace's events come from committed execution only, once each
/// at their tick's time even across a re-run, and a sink that refuses batches gets exactly the
/// same events in the same order while the device waits for it.
#[test]
fn events_are_committed_once_in_order_and_never_dropped() {
    let (image, low) = poll_program();
    let map = StepMap { funcs: Vec::new(), calls: Vec::new(), owners: Vec::new(), statements: vec![low], entries: Vec::new(), returns: Vec::new() };
    let t0 = 0.0;
    let h = at(t0, 1234) + 0.4 / HZ as f64;
    let end = at(t0, 1600);
    // the oracle: every execution of the low branch up to the end, from a direct run
    let c = tick_at(t0, h + 7.3 / HZ as f64, HZ);
    let mut m = direct(&image);
    run_to(&mut m, c);
    m.set_input(PD2, false);
    let mut marks = PcSet::new(low, 1);
    marks.insert(low);
    let _ = m.run(&Until { tick: tick_at(t0, end, HZ), marks: &marks, stops: &[], stops_after: 0, on_drive: false });
    let mut trace = Vec::new();
    m.take_trace(&mut trace);
    let want: Vec<(u64, FwFrame)> = trace.iter().map(|&(tick, pc)| (at(t0, tick).to_bits(), FwFrame::Statement { pc })).collect();
    assert!(want.len() >= 10, "the low branch loops: {}", want.len());
    let mut runs = Vec::new();
    for refuse in [0, 2] {
        let mut d = dev(&image, Some(map.clone()));
        let (s, got, refused) = sink(refuse);
        d.set_events(Some(s), true);
        let mut bus = Script::new(t0);
        bus.lag = 7.3 / HZ as f64;
        bus.steps.push_back((h, PD2 as u32, 0.0));
        bus.attach(&mut d);
        // a refused batch holds the device at the analog's instant (next() == t) until taken
        assert_eq!(bus.play(&mut d, end), None, "nothing is armed");
        let _ = d.state(end);
        while d.backpressure() {
            assert_eq!(d.next(), bus.t, "refuse {refuse}: the device waits at the analog's instant");
            let _ = d.run(&mut bus);
        }
        assert!(d.stats().reruns >= 1, "refuse {refuse}: the CPU re-ran to the edge");
        let got: Vec<(u64, FwFrame)> = got.lock().unwrap().iter().map(|(t, f)| (t.to_bits(), f.clone())).collect();
        assert_eq!(got, want, "refuse {refuse}: each committed statement once, at its tick, in order");
        if refuse != 0 {
            assert!(*refused.lock().unwrap() >= 1, "refuse {refuse}: the sink refused batches");
        }
        runs.push(got);
    }
    assert_eq!(runs[0], runs[1], "refusals change pacing, never the events");

    // The image's watch stream: one frame sent over the UART decodes as a board's reader would,
    // at the tick its delimiter starts.
    let frame = watch::Frame { kind: watch::WRITE, seq: 3, tick: 0x1234, event: watch::Event::Write { pin: 5, level: true, site: 7 } };
    let mut wire = [0u8; watch::MAX_WIRE];
    let n = watch::encode(&frame, &mut wire);
    let mut a = Asm::new();
    store(&mut a, 0xc4, 0); // UBRR0L: 1 Mbit/s
    store(&mut a, 0xc1, 0x08); // UCSR0B: TXEN0
    for &b in &wire[..n] {
        let wait = a.label();
        a.bind(wait);
        a.emit(Lds(17, 0xc0)); // UCSR0A
        a.emit(Sbrs(17, 5)); // UDRE0
        a.jump(Target::Rjmp, wait);
        store(&mut a, 0xc6, b); // UDR0
    }
    let idle = a.label();
    a.bind(idle);
    a.jump(Target::Rjmp, idle);
    let image = a.finish().expect("the UART program assembles");
    let mut m = direct(&image);
    run_to(&mut m, 4000);
    let mut out = Vec::new();
    m.take_out(&mut out);
    assert_eq!(out.len(), n, "the program sends the whole frame");
    let mut d = dev(&image, None);
    let (s, got, _) = sink(0);
    d.set_events(Some(s), false);
    let mut bus = Script::new(t0);
    bus.attach(&mut d);
    assert_eq!(bus.play(&mut d, at(t0, 4000)), None, "nothing is armed");
    let _ = d.state(at(t0, 4000));
    let got = got.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "one image frame: {got:?}");
    assert_eq!((got[0].0.to_bits(), &got[0].1), (at(t0, out[n - 1].tick).to_bits(), &FwFrame::Image(frame)),
        "the frame, at its delimiter's tick");
    assert!(got[0].1.is_image(), "an image frame is the image's source");
}

#[test]
fn radio_starts_track_the_latest_marker_and_degrading_report_in_order() {
    let fault = |cycle, code, arg| FwFault { cycle, code, site: 2, arg };
    let boot = fault(10, watch::FAULT_RADIO_STARTED, 1);
    let lost = fault(20, watch::FAULT_KERNEL_TIMER_LOST, 0x1234);
    let request = fault(30, watch::FAULT_RADIO_STARTED, 0x0101_0002);
    let overwritten = fault(40, watch::FAULT_KERNEL_TIMER_HANDLE_INVALID, 0x5678);
    let refused = fault(50, watch::FAULT_KERNEL_TIMER_REFUSED, 7);
    let again = fault(60, watch::FAULT_RADIO_STARTED, 0x0201_0003);
    let first = Some((watch::RadioStart { generation: 1, cause: 0, consecutive: 0 }, boot.cycle));
    let second = Some((watch::RadioStart { generation: 2, cause: 1, consecutive: 1 }, request.cycle));
    let third = Some((watch::RadioStart { generation: 3, cause: 1, consecutive: 2 }, again.cycle));
    let mut starts = RadioStarts::default();
    for (event, marker, markers, latest, degraded) in [
        (boot, true, 1, first, None),
        (lost, false, 1, first, Some(lost)),
        (request, true, 2, second, None),
        (overwritten, false, 2, second, Some(overwritten)),
        (refused, false, 2, second, Some(overwritten)),
        (again, true, 3, third, None),
    ] {
        assert_eq!(starts.observe(&event), marker, "marker disposition for {event:?}");
        assert_eq!(starts, RadioStarts { markers, latest, degraded }, "radio state after {event:?}");
    }

    let mut legacy = RadioStarts::default();
    for (event, newest) in [(lost, lost), (overwritten, overwritten), (refused, overwritten)] {
        assert!(!legacy.observe(&event), "an ordinary fault stays in the tally");
        assert_eq!(legacy, RadioStarts { markers: 0, latest: None, degraded: Some(newest) },
            "a markerless image keeps its newest degrading report after {event:?}");
    }
}

/// A state names the faults since the previous publication, and the device's tally of every fault
/// by code and site: a reader that misses a publication (the newest replaces an unread one) still
/// shows its faults.
#[test]
fn the_device_tallies_every_fault_by_code_and_site_across_publications() {
    let frames = [(watch::FAULT_TIMER, 2, 10), (watch::FAULT_TIMER, 2, 11), (watch::FAULT_PIN, 5, -1),
        (watch::FAULT_RADIO_STARTED, watch::SITE_RADIO_SCHEDULER, 1)];
    let mut a = Asm::new();
    store(&mut a, 0xc4, 0); // UBRR0L: 1 Mbit/s
    store(&mut a, 0xc1, 0x08); // UCSR0B: TXEN0
    let mut ends = Vec::new();
    for (seq, &(code, site, arg)) in frames.iter().enumerate() {
        let frame = watch::Frame { kind: watch::FAULT, seq: seq as u8, tick: 0, event: watch::Event::Fault { code, site, arg } };
        let mut wire = [0u8; watch::MAX_WIRE];
        let n = watch::encode(&frame, &mut wire);
        for &b in &wire[..n] {
            let wait = a.label();
            a.bind(wait);
            a.emit(Lds(17, 0xc0)); // UCSR0A
            a.emit(Sbrs(17, 5)); // UDRE0
            a.jump(Target::Rjmp, wait);
            store(&mut a, 0xc6, b); // UDR0
        }
        ends.push(n + ends.last().copied().unwrap_or(0));
    }
    let idle = a.label();
    a.bind(idle);
    a.jump(Target::Rjmp, idle);
    let image = a.finish().expect("the UART program assembles");
    let mut m = direct(&image);
    run_to(&mut m, 20_000);
    let mut out = Vec::new();
    m.take_out(&mut out);
    assert_eq!(out.len(), ends[3], "the program sends three faults and a start marker");
    // between the first frame's delimiter and the second's
    let mid = (out[ends[0] - 1].tick + out[ends[1] - 1].tick) / 2;
    let t0 = 0.0;
    let mut d = dev(&image, None);
    let mut bus = Script::new(t0);
    bus.attach(&mut d);
    assert_eq!(bus.play(&mut d, at(t0, mid)), None, "nothing is armed");
    let first = snapshot(&mut d, at(t0, mid));
    let timer = |count, arg, end: usize| FaultCount {
        code: watch::FAULT_TIMER, site: 2, count, last: FwFault { cycle: out[end - 1].tick, code: watch::FAULT_TIMER, site: 2, arg },
    };
    assert_eq!(first.faults.len(), 1, "the first publication names the first fault: {:?}", first.faults);
    assert_eq!(first.fault_counts, [timer(1, 10, ends[0])], "and the tally holds it");
    assert_eq!(bus.play(&mut d, at(t0, 20_000)), None, "nothing is armed");
    let second = snapshot(&mut d, at(t0, 20_000));
    assert_eq!(second.faults.iter().map(|f| (f.code, f.site, f.arg)).collect::<Vec<_>>(), [(watch::FAULT_TIMER, 2, 11), (watch::FAULT_PIN, 5, -1)],
        "the second publication names the faults since the first");
    let pin = FaultCount { code: watch::FAULT_PIN, site: 5, count: 1, last: FwFault { cycle: out[ends[2] - 1].tick, code: watch::FAULT_PIN, site: 5, arg: -1 } };
    assert_eq!(second.fault_counts, [timer(2, 11, ends[1]), pin],
        "the tally counts the timer fault twice with its latest report, then the pin fault, in the order each first came");
    assert_eq!(second.radio_starts, RadioStarts { markers: 1, latest: Some((
        watch::RadioStart { generation: 1, cause: 0, consecutive: 0 }, out[ends[3] - 1].tick)), degraded: None },
        "the publication carries the marker separately from both fault lists");
}

/// The same firmware at either watch level: setup raises two pin faults at distinct sites.
fn pin_fault_device(level: watch::Level) -> (CoMachine<AvrMachine>, [u16; 2]) {
    let source = "\
var missing_pin: i32 = -1
var bad_read: i32 = 7
fn setup() {
    write(missing_pin, high)
    bad_read = read(missing_pin)
}
";
    let fw = fab_elec_mcu_emit_avr::firmware_with_watch(source, HZ as f64, &[], level).expect("the pin-fault firmware builds");
    let sites = ["write(missing_pin, high)", "read(missing_pin)"].map(|text| {
        fw.debug.sites.iter().position(|&(start, end)| &source[start as usize..end as usize] == text)
            .expect("the port call has a site") as u16
    });
    assert_ne!(sites[0], sites[1], "the write and read have different sites");
    let m = AvrMachine::new(&fw.words, HZ);
    let pins: Vec<_> = m.pins().iter().map(|&name| row(name, &name.to_lowercase())).collect();
    let d = CoMachine::new(m, Config { stretch: 4000, debug: Some(Arc::new(fw.debug)) }, &pins).expect("a valid device");
    (d, sites)
}

#[test]
fn off_fault_cells_are_read_at_each_publication() {
    let (mut d, sites) = pin_fault_device(watch::Level::Off);
    let mut bus = Script::new(0.0);
    bus.attach(&mut d);
    let initial = snapshot(&mut d, bus.t);
    assert_eq!(initial.fault_cells, [FaultCell { code: watch::FAULT_PIN, count: 0, site: 0 }], "before setup the counter is zero");
    for tick in [160_000, 320_000] {
        assert_eq!(bus.play(&mut d, at(0.0, tick)), None, "no step is armed");
        let s = snapshot(&mut d, bus.t);
        assert_eq!(s.vars, [FwValue::I32(-1), FwValue::I32(0)], "setup has completed both bad pin calls");
        assert_eq!(s.fault_cells, [FaultCell { code: watch::FAULT_PIN, count: 2, site: sites[1] }],
            "both firings count once, the read is latest, and publication does not clear the cell");
        assert!(s.faults.is_empty() && s.fault_counts.is_empty(), "RAM counters do not become stream reports");
    }
}

#[test]
fn faults_watch_keeps_exact_stream_reports_without_fault_cells() {
    let (mut d, sites) = pin_fault_device(watch::Level::Faults);
    let mut bus = Script::new(0.0);
    bus.attach(&mut d);
    assert_eq!(bus.play(&mut d, at(0.0, 160_000)), None, "no step is armed");
    let s = snapshot(&mut d, bus.t);
    assert!(s.fault_cells.is_empty(), "a streaming image has no RAM fault cells");
    assert_eq!(s.faults.iter().map(|f| (f.code, f.site, f.arg)).collect::<Vec<_>>(),
        sites.map(|site| (watch::FAULT_PIN, site, -1)), "each bad pin call has its own exact report");
    assert_eq!(s.fault_counts.iter().map(|f| (f.code, f.site, f.count, f.last)).collect::<Vec<_>>(),
        sites.into_iter().zip(&s.faults).map(|(site, &last)| (watch::FAULT_PIN, site, 1, last)).collect::<Vec<_>>(),
        "stream counts remain per code and site with their latest report");
}

#[test]
fn unread_fault_cells_are_omitted_without_losing_readable_cells() {
    let mut a = Asm::new();
    for (offset, byte) in [0xff, 0xff, 0x34, 0x12, 0x02, 0x01, 0xcd, 0xab].into_iter().enumerate() {
        store(&mut a, 0x100 + offset as u16, byte);
    }
    let idle = a.label();
    a.bind(idle);
    a.jump(Target::Rjmp, idle);
    let image = a.finish().expect("the RAM program assembles");
    let debug = FwDebug { fault_cells: vec![
        FwFaultCell { code: watch::FAULT_TIMER, addr: 0x104 },
        FwFaultCell { code: watch::FAULT_OVERRUN, addr: 0x8fe }, // only two readable bytes
        FwFaultCell { code: watch::FAULT_PIN, addr: 0x100 },
    ], ..FwDebug::default() };
    let mut d = CoMachine::new(AvrMachine::new(&image, HZ), Config { stretch: 4000, debug: Some(Arc::new(debug)) }, &[])
        .expect("a valid device");
    let mut bus = Script::new(0.0);
    bus.attach(&mut d);
    assert_eq!(bus.play(&mut d, at(0.0, 100)), None, "no step is armed");
    assert_eq!(snapshot(&mut d, bus.t).fault_cells, [
        FaultCell { code: watch::FAULT_TIMER, count: 0x0102, site: 0xabcd },
        FaultCell { code: watch::FAULT_PIN, count: 0xffff, site: 0x1234 },
    ], "readable cells retain table order and both little-endian words, including saturation");
}

/// E2E A3 and A4: a handler entry from sleep, statements, and the handler's exit each stop before
/// their instruction at the device's own tick, bit-exact (`t == t_reset + tick / f_clk`), with the
/// state standing there; an exit is refused on the return address and while sleeping.
#[test]
fn steps_land_bit_exact_on_statements_and_handlers() {
    let (image, map) = step_program();
    let (h, ret) = (map.entries[0].0, map.returns[0]);
    let t0 = 1.5e-6;
    let mut d = dev(&image, Some(map.clone()));
    let mut bus = Script::new(t0);
    bus.attach(&mut d);
    assert_eq!(bus.play(&mut d, at(t0, 200)), None, "no step is armed");
    let mut m = direct(&image);
    let mut marks = PcSet::new(h.min(ret), h.max(ret) + 4);
    for pc in map.statements.iter().chain([&h, &ret]) {
        marks.insert(*pc);
    }
    let mut step = |d: &mut CoMachine<AvrMachine>, bus: &mut Script, to: StepTo, stops: &[u32], pc: u32, what: &str| {
        let c = tick_at(t0, bus.t, HZ);
        assert!(d.arm(bus.t, Some(to)), "{what}: the step is armed");
        if m.now() < c {
            run_to(&mut m, c);
        }
        assert_eq!(m.run(&Until { tick: c + 40_000, marks: &marks, stops, stops_after: c, on_drive: false }), Ran::Pc, "{what}: the direct run stops");
        let t = bus.play(d, f64::INFINITY).expect("the step stops");
        assert_eq!(t.to_bits(), at(t0, m.now()).to_bits(), "{what}: lands bit-exact on tick {}", m.now());
        let syncs = d.stats().sync_reruns;
        assert!(d.settled(), "{what}: a stop leaves the device settled");
        let s = snapshot(d, t);
        assert_eq!(d.stats().sync_reruns, syncs, "{what}: the stop's publication re-runs nothing");
        assert_eq!((s.pc, s.cycle, s.stopped, s.t.to_bits(), s.t_reset.to_bits()), (pc, m.now(), Some(to), t.to_bits(), t0.to_bits()),
            "{what}: the state stands on it, not yet executed");
        s
    };
    let s = step(&mut d, &mut bus, StepTo::HandlerEntry, &[h], h, "the timer handler's entry, from sleep");
    assert_eq!(s.handler, None, "standing on the entry, the handler has not begun");
    let s = step(&mut d, &mut bus, StepTo::Statement, &map.statements, h + 1, "the second statement");
    assert_eq!((s.handler, s.last, s.at), (Some(0), Some(0), At::Statement(0)), "the first statement ran inside the handler");
    assert_eq!(s.vars, [FwValue::I32(0)], "the counter is not stored yet");
    let s = step(&mut d, &mut bus, StepTo::Statement, &map.statements, h + 3, "the third statement");
    assert_eq!((s.last, s.ran), (Some(1), vec![(1, 1)]), "one statement began since the previous publication");
    assert_eq!(s.ran_counts, [(0, 1), (1, 1)], "the tally still counts the statement of the publication before, for a reader that missed it");
    assert_eq!(s.vars, [FwValue::I32(1)], "the counter read back from RAM through the layout");
    step(&mut d, &mut bus, StepTo::HandlerExit, &[ret], ret, "the dispatcher after the call");
    assert!(!d.arm(bus.t, Some(StepTo::HandlerExit)), "on the return address no handler runs: an exit is refused");
    assert_eq!(bus.play(&mut d, bus.t + at(0.0, 100)), None, "the refused step leaves nothing armed");
    let s = snapshot(&mut d, bus.t);
    assert!(matches!(s.at, At::Sleeping), "the loop sleeps again: {:?} at pc {:#x}, cycle {}, ret {ret:#x}", s.at, s.pc, s.cycle);
    assert!(!d.arm(bus.t, Some(StepTo::HandlerExit)), "while sleeping an exit is refused");
    step(&mut d, &mut bus, StepTo::Statement, &map.statements, h, "through sleep to the next handler's first statement");
    let toggles = bus.row(PB5).iter().filter(|w| w.1 == PinMode::OutHigh).count();
    assert_eq!(toggles, 1, "the first handler's toggle reached the analog once");
}

/// E2E A5: an input edge inside the private stretch of an armed statement step gives the stop a
/// run with that edge scheduled in advance gives, and the same pin writes.
/// The step map's addresses are the block map's roots (`CoMachine::new` gives them), so each
/// statement starts a block: its traced runs and its block's entries are one tally, through
/// speculative stretches that are rewound and run again.
#[test]
fn each_statement_starts_a_block_and_its_entries_are_its_runs() {
    let (image, map) = step_program();
    let mut m = direct(&image);
    let mut roots: Vec<u32> = map.statements.iter().copied()
        .chain(map.entries.iter().map(|e| e.0)).chain(map.returns.iter().copied())
                .chain(map.funcs.iter().map(|f| f.0)).chain(map.calls.iter().map(|c| c.0)).collect();
    roots.sort_unstable();
    roots.dedup();
    m.set_code_roots(&roots);
    let marks = map.marks();
    let (mut trace, mut counts) = (Vec::new(), ExecCounts::default());
    m.mark();
    for slice in 1..=24u64 {
        let until = Until { tick: slice * 20_000, marks: &marks, stops: &[], stops_after: 0, on_drive: false };
        assert_eq!(m.run(&until), Ran::Tick);
        m.rewind();
        assert_eq!(m.run(&until), Ran::Tick);
        m.take_trace(&mut trace);
        m.take_counts(&mut counts);
        m.mark();
    }
    let code = m.code_map().expect("the AVR keeps a block map");
    let mut entries = vec![0u64; code.blocks.len() + 1];
    for &(id, n) in &counts.blocks {
        entries[id as usize] += n;
    }
    for &pc in &map.statements {
        let k = code.blocks.binary_search_by_key(&pc, |b| b.start).expect("a statement starts a block");
        let runs = trace.iter().filter(|&&(_, at)| at == pc).count() as u64;
        assert!(runs > 0, "statement at {pc} ran");
        assert_eq!(entries[k + 1], runs, "statement at {pc}: block entries against traced runs");
    }
}

/// A source condition's branch (`FwDebug::conds`) is tallied by its sense: taken counts as true
/// for a branch taken when the condition holds and as false for one taken when it fails, and the
/// newest outcome is the condition's value. The tally grows across publications and counts a
/// rewound stretch once: the side the branch jumps to begins as often as it was taken (once more
/// at most, when a commit falls between the two).
#[test]
fn a_condition_tallies_its_branch_by_its_sense_across_publications() {
    let mut a = Asm::new();
    let (top, odd) = (a.label(), a.label());
    a.bind(top);
    a.emit(Inc(17));
    a.emit(Mov(18, 17));
    a.emit(Andi(18, 1));
    let branch = a.here();
    a.jump(Target::Brbc(1), odd); // BRNE: the count is odd
    a.jump(Target::Rjmp, top);
    a.bind(odd);
    let odd_at = a.here();
    a.emit(Sts(0x100, 17));
    a.jump(Target::Rjmp, top);
    let image = a.finish().expect("the parity program assembles");
    let map = StepMap { funcs: Vec::new(), calls: Vec::new(), owners: Vec::new(), statements: vec![odd_at], entries: vec![], returns: vec![] };
    let mut lasts: Vec<Vec<bool>> = Vec::new();
    for taken_true in [true, false] {
        let cond = FwCond { pc: branch, span: (0, 9), line: 0, owner: fab_elec_mcu_emu::Owner::Handler(0), taken_true };
        let debug = Arc::new(FwDebug { conds: vec![cond], ..(*table(map.clone())).clone() });
        let m = AvrMachine::new(&image, HZ);
        let pins: Vec<CoSimPin> = m.pins().iter().map(|&name| row(name, &name.to_lowercase())).collect();
        let mut d = CoMachine::new(m, Config { stretch: 4000, debug: Some(debug) }, &pins).expect("a valid device");
        let mut bus = Script::new(0.0);
        bus.attach(&mut d);
        let (mut seen, mut last) = (0, Vec::new());
        for end in [3_001u64, 10_007, 25_013] {
            assert_eq!(bus.play(&mut d, at(0.0, end)), None, "nothing is armed");
            let state = snapshot(&mut d, at(0.0, end));
            let [c] = state.cond_counts[..] else { panic!("the one condition is tallied: {:?}", state.cond_counts) };
            assert_eq!(c.cond, 0, "by its index in the image's table");
            let (taken, fell) = if taken_true { (c.trues, c.falses) } else { (c.falses, c.trues) };
            let odd_runs = state.ran_counts.iter().find(|r| r.0 == 0).map_or(0, |r| r.1);
            assert!(taken == odd_runs || taken == odd_runs + 1,
                "taken as often as the odd side began, rewinds counted once ({taken} taken, {odd_runs} runs; taken means true: {taken_true})");
            assert!(fell > 0 && taken.abs_diff(fell) <= 1, "the parity alternates: {taken} taken, {fell} fell through");
            assert!(c.trues + c.falses > seen, "the tally grows across publications");
            seen = c.trues + c.falses;
            last.push(c.last);
        }
        lasts.push(last);
    }
    assert!(lasts[0].iter().zip(&lasts[1]).all(|(a, b)| a != b),
        "the newest outcome reads as the condition's value by the branch's sense: {lasts:?}");
}

#[test]
fn an_edge_inside_an_armed_step_stops_as_if_scheduled() {
    let (image, low) = poll_program();
    let map = StepMap { funcs: Vec::new(), calls: Vec::new(), owners: Vec::new(), statements: vec![low], entries: Vec::new(), returns: Vec::new() };
    let t0 = 0.0;
    let mut d = dev(&image, Some(map));
    let mut bus = Script::new(t0);
    bus.attach(&mut d);
    assert_eq!(bus.play(&mut d, at(t0, 300)), None, "no step is armed");
    let c_arm = tick_at(t0, bus.t, HZ);
    assert!(d.arm(bus.t, Some(StepTo::Statement)), "the step is armed");
    let h = at(t0, 2000) + 0.25 / HZ as f64;
    bus.steps.push_back((h, PD2 as u32, 0.0));
    let t = bus.play(&mut d, f64::INFINITY).expect("the low branch stops the step");
    let c = tick_at(t0, h, HZ);
    let mut m = direct(&image);
    run_to(&mut m, c);
    m.set_input(PD2, false);
    let mut marks = PcSet::new(low, 1);
    marks.insert(low);
    assert_eq!(m.run(&Until { tick: c + 100, marks: &marks, stops: &[low], stops_after: c_arm, on_drive: false }), Ran::Pc);
    assert_eq!(t.to_bits(), at(t0, m.now()).to_bits(), "the stop is the scheduled run's, bit-exact");
    assert_eq!(snapshot(&mut d, t).pc, low, "standing on the branch that drives PB5 low");
    let pb5: Vec<PinMode> = bus.row(PB5).iter().map(|w| w.1).collect();
    assert_eq!(pb5, [PinMode::Input(PinPull::None), PinMode::OutLow, PinMode::OutHigh], "PB5's writes: the reset, its DDR, the high branch; the low branch has not run");
    assert!(d.stats().reruns >= 1, "the CPU had run past the edge and re-ran to it");
}

/// A fault halts the CPU, never the analog: the device asks for nothing more and shows the halt.
/// E2E A9: an image swapped in mid-run starts at the swap, its pins in their reset drive, its
/// ticks from zero at the new `t_reset`.
#[test]
fn a_fault_halts_the_cpu_and_a_swapped_image_starts_at_the_swap() {
    let mut a = Asm::new();
    store(&mut a, 0x24, 0x20);
    a.emit(Sbi(0x05, 5));
    for _ in 0..10 {
        a.emit(Nop);
    }
    a.emit(Break);
    let fault = a.finish().expect("the fault program assembles");
    let t0 = 0.0;
    let mut d = dev(&fault, None);
    let mut bus = Script::new(t0);
    bus.attach(&mut d);
    assert_eq!(bus.play(&mut d, at(t0, 100)), None, "nothing is armed");
    assert_eq!(d.next(), f64::INFINITY, "past the fault the device asks for no instant");
    let s = snapshot(&mut d, at(t0, 100));
    assert!(matches!(s.at, At::Halted(halt) if halt.kind == HaltKind::Break), "the state shows the halt: {:?}", s.at);
    assert_eq!(bus.row(PB5).last().map(|w| w.1), Some(PinMode::OutHigh), "the halt leaves PB5 driven");
    let t1 = at(t0, 100) + 0.3e-6;
    let (image, _) = poll_program();
    let mut swapped = dev(&image, None);
    bus.t = t1;
    bus.writes.clear();
    bus.attach(&mut swapped);
    assert!(bus.writes.len() == 22 && bus.writes.iter().all(|w| w.0 == t1 && w.2 == PinMode::Input(PinPull::None)),
        "at the swap every pin is in its reset drive: {:?}", bus.writes);
    assert_eq!(bus.play(&mut swapped, t1 + at(0.0, 50)), None, "nothing is armed");
    let mut m = direct(&image);
    let Ran::Drive { at: ddr } = m.run(&Until { tick: 50, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: true }) else {
        panic!("the swapped image's first drive change");
    };
    assert_eq!(bus.row(PB5)[1].0.to_bits(), at(t1, ddr).to_bits(), "its first edge counts ticks from the swap");
    let s = snapshot(&mut swapped, bus.t);
    assert_eq!(s.t_reset.to_bits(), t1.to_bits(), "the new image's reset is the swap");
    assert_eq!(s.cycle, tick_at(t1, bus.t, HZ), "its ticks count from the swap");
    assert!(s.cycle <= 51, "its ticks restart: {}", s.cycle);
}

/// The live worker: a button pressed through 1 kOhm pulls PD2 below VIL and PB5 follows into its
/// RC load; only PB5 and PD2 are rows. Runs with different worker budgets give bit-identical
/// records (Q6 8A.6), and so does a run that publishes the device's state after every slice: a
/// publication brings the device to its instant and commits there, which must not move the
/// stretch deadlines the analog steps to. The stretch is short enough for several in the run.
#[test]
fn the_live_worker_couples_real_pads_deterministically() {
    let image = poll_program().0;
    let params = GpioParams { r_on_h: 26.0, r_on_l: 23.5, r_pull: 36e3, r_leak: 5.5e6, t_edge: 5e-9, vih: VIH, vil: VIL };
    let run = |slice: Option<f64>, publish: bool| {
        let mut c = SimCircuit::new();
        c.v("supply", "vdd", "0", Source::dc(VDD));
        c.gpio("u1#PB5.io", "led", "vdd", "0", params, PinMode::Input(PinPull::None));
        c.r("rled", "led", "0", 10e3);
        c.c("cled", "led", "0", 100e-12);
        c.gpio("u1#PD2.io", "btn", "vdd", "0", params, PinMode::Input(PinPull::None));
        c.c("cbtn", "btn", "0", 5e-12);
        c.v("key", "k", "0", Source::wave(Wave::Pwl(vec![(0.0, VDD), (50e-6, VDD), (50.2e-6, 0.0)])));
        c.r("button", "btn", "k", 1e3);
        c.opts.chgtol = 1e-20;
        c.opts.vntol = 1e-9;
        c.opts.abstol = 1e-15;
        let spec = LiveSpec { rate: f64::INFINITY, playing: false, reltol: Some(1e-5), max_step: 2e-6, trace: false, ..LiveSpec::default() };
        let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("the operating point converges");
        sim.values(1.0);
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        record.point(sim.t(), None, &sim.solver().x);
        let pins = vec![row("PB5", "led"), row("PD2", "btn")];
        let m = AvrMachine::new(&image, HZ);
        sim.attach(Some(device(m, Config { stretch: 400, debug: None }, pins).expect("a valid device")));
        let end = 100e-6;
        match slice {
            None => sim.advance(end, None, &mut record).expect("the bench steps"),
            Some(dt) => {
                while sim.t() < end {
                    let target = (sim.t() + dt).min(end);
                    sim.advance(target, Some(Duration::ZERO), &mut record).expect("the bench steps in slices");
                    if publish {
                        assert!(sim.values(1.0).co.is_some(), "a publication carries the device's state");
                    }
                }
            }
        }
        record
    };
    let record = run(None, false);
    assert!(record.at_time("led", 40e-6) > 0.9 * VDD, "PB5 follows the released button high");
    assert!(record.at_time("led", 90e-6) < 0.1 * VDD, "the press reaches the CPU and PB5 follows it low");
    let sliced = run(Some(1.3e-6), false);
    assert_eq!(sliced.at, record.at, "the same accepted times however the worker slices time");
    assert_eq!(sliced.x, record.x, "the same solutions however the worker slices time");
    let published = run(Some(0.7e-6), true);
    assert_eq!(published.at, record.at, "the same accepted times however often the device's state is published");
    assert_eq!(published.x, record.x, "the same solutions however often the device's state is published");
}

/// A small seeded generator (xorshift), so a "random" slicing is the same each run.
fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// The run is a function of its timeline, never of the wall clock (R0, test 1). A bench with a
/// nonlinear voice (a diode into an RC), a noise source, a PWM pin and the CPU polling a pulled-up
/// input watched through a switch: a press at 30 us, a pot move at 45 us, a PWM duty change at
/// 60 us and the release at 70 us, all on the timeline. Run A steps straight through; run B in
/// slices with seeded random targets and wall budgets, publishing values and taking the trace's
/// deltas between them, with a firmware event feed that refuses every other batch; run C is A
/// again after B. Equal by bits: the accepted times and solutions, the firmware's statements, and
/// the times each event applied at.
#[test]
fn the_run_is_a_function_of_its_timeline_not_the_wall_clock() {
    let (image, low) = poll_program();
    let params = GpioParams { r_on_h: 26.0, r_on_l: 23.5, r_pull: 36e3, r_leak: 5.5e6, t_edge: 5e-9, vih: VIH, vil: VIL };
    let end = 100e-6;
    let set = |device: &str, key: &str, value: f64| LiveCmd::SetParam { generation: 1, device: device.into(), key: key.into(), value };
    let duty = LiveCmd::PinMode { generation: 1, device: "pwm.io".into(), mode: PinMode::Pwm { period: 9e-6, high: 6e-6, origin: 0.0 } };
    let timeline = vec![(30e-6, set("press", "dc", 1.0)), (45e-6, set("rled", "value", 4.7e3)), (60e-6, duty), (70e-6, set("press", "dc", 0.0))];
    // `late`: the events are sent once the run has passed this time, as a live edit is
    let run = |sliced: Option<u64>, timeline: &[(f64, LiveCmd)], late: Option<f64>| {
        let mut c = SimCircuit::new();
        c.v("supply", "vdd", "0", Source::dc(VDD));
        c.gpio("u1#PB5.io", "led", "vdd", "0", params, PinMode::Input(PinPull::None));
        c.model("dmod", ModelKind::Diode, &[("is", 1e-14), ("n", 1.8)]);
        c.d("dled", "led", "dk", "dmod");
        c.r("rled", "dk", "0", 10e3);
        c.c("cled", "led", "0", 100e-12);
        c.v("hiss", "hn", "0", Source::wave(Wave::Noise { seed: 7, sigma: 0.05, dt: 2e-6, base: 2e-6 }));
        c.r("rhiss", "hn", "led", 200e3);
        c.gpio("u1#PD2.io", "btn", "vdd", "0", params, PinMode::Input(PinPull::None));
        c.c("cbtn", "btn", "0", 5e-12);
        c.v("press", "pc", "0", Source::dc(0.0));
        c.model("swm", ModelKind::Sw, &[("ron", 100.0), ("roff", 1e9), ("vt", 0.5), ("vh", 0.1), ("vband", 0.05)]);
        c.sw("key", "btn", "0", "pc", "0", "swm", false);
        c.gpio("pwm.io", "pw", "vdd", "0", params, PinMode::Pwm { period: 9e-6, high: 4e-6, origin: 0.0 });
        c.r("rpwm", "pw", "mix", 47e3);
        c.c("cmix", "mix", "0", 1e-9);
        c.opts.chgtol = 1e-20;
        c.opts.vntol = 1e-9;
        c.opts.abstol = 1e-15;
        let spec = LiveSpec { rate: f64::INFINITY, playing: false, reltol: Some(1e-5), max_step: 2e-6, trace: true, ..LiveSpec::default() };
        let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("the operating point converges");
        sim.values(1.0);
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        record.point(sim.t(), None, &sim.solver().x);
        let map = StepMap { funcs: Vec::new(), calls: Vec::new(), owners: Vec::new(), statements: vec![low], entries: Vec::new(), returns: Vec::new() };
        let pins = vec![row("PB5", "led"), row("PD2", "btn")];
        let mut d = CoMachine::new(AvrMachine::new(&image, HZ), Config { stretch: 400, debug: Some(table(map)) }, &pins).expect("a valid device");
        let (events, got, refused) = sink(if sliced.is_some() { 2 } else { 0 });
        d.set_events(Some(events), true);
        sim.attach(Some(fab_elec_sim::cosim::CoSimDevice { dev: Box::new(d), pins }));
        while let Some(t0) = late.filter(|&t0| sim.t() < t0) {
            sim.advance(t0, None, &mut record).expect("the bench steps to the late edit");
        }
        for (t, cmd) in timeline {
            assert!(sim.apply(LiveCmd::At(*t, Box::new(cmd.clone()))), "a timed command is taken");
        }
        match sliced {
            // a value change asks for a publication and ends the call, as in the worker's loop
            None => {
                while sim.t() < end {
                    sim.advance(end, None, &mut record).expect("the bench steps");
                }
            }
            Some(mut seed) => {
                while sim.t() < end {
                    let target = (sim.t() + (xorshift(&mut seed) % 4000) as f64 * 1e-9).min(end);
                    let budget = Duration::from_micros(xorshift(&mut seed) % 300);
                    sim.advance(target, Some(budget), &mut record).expect("the bench steps in slices");
                    if xorshift(&mut seed) % 3 == 0 {
                        let _ = sim.values(1.0);
                    }
                    if xorshift(&mut seed) % 2 == 0 {
                        let _ = sim.take_delta();
                    }
                }
            }
        }
        let events: Vec<(u64, FwFrame)> = got.lock().unwrap().iter().map(|(t, f)| (t.to_bits(), f.clone())).collect();
        let applied: Vec<(f64, LiveCmd)> = sim.timeline().to_vec();
        let refused = *refused.lock().unwrap();
        (record, events, applied, refused)
    };
    let bits = |applied: &[(f64, LiveCmd)]| applied.iter().map(|e| e.0.to_bits()).collect::<Vec<u64>>();
    let (a, a_events, a_timeline, _) = run(None, &timeline, None);
    let a_applied = bits(&a_timeline);
    assert!(a.at_time("btn", 20e-6) > 0.9 * VDD && a.at_time("btn", 50e-6) < 0.1 * VDD, "the press pulls the input low");
    assert!(a.at_time("led", 20e-6) > 0.5 * VDD && a.at_time("led", 60e-6) < 0.1 * VDD, "the CPU follows the press on its output");
    assert!(a.at_time("btn", 95e-6) > 0.9 * VDD, "the release lets the input up again");
    assert!(a_applied.len() == 4 && a_applied.iter().zip([30e-6, 45e-6, 60e-6, 70e-6]).all(|(&at, t)| f64::from_bits(at) >= t),
        "each event applied at the first step boundary at or after its time");
    assert!(!a_events.is_empty(), "the firmware's low branch ran");
    let (b, b_events, b_timeline, refused) = run(Some(0x9e37_79b9_7f4a_7c15), &timeline, None);
    assert!(refused >= 1, "the event feed refused batches");
    let (c, c_events, c_timeline, _) = run(None, &timeline, None);
    // a live edit comes after its time: it applies at the next boundary, and the timeline the
    // worker keeps replays it from the start as the same run
    let (d, d_events, d_timeline, _) = run(None, &timeline[..1], Some(33e-6));
    assert!(d_timeline[0].0 >= 33e-6, "a late event applies when it comes, not in the past");
    let (e, e_events, e_timeline, _) = run(None, &d_timeline, None);
    assert_eq!(bits(&e_timeline), bits(&d_timeline), "the replay applies at the same boundary");
    assert_eq!((e.at.len(), e_events.len()), (d.at.len(), d_events.len()), "the replay runs alike");
    assert!(e.at.iter().zip(&d.at).all(|(x, y)| x.to_bits() == y.to_bits()) && e.x == d.x && e_events == d_events, "a run replayed from its kept timeline is the same run");
    let (b_applied, c_applied) = (bits(&b_timeline), bits(&c_timeline));
    for (name, other, events, applied) in [("sliced", &b, &b_events, &b_applied), ("again", &c, &c_events, &c_applied)] {
        assert_eq!(applied, &a_applied, "{name}: the events apply at the same step boundaries");
        assert_eq!(other.at.len(), a.at.len(), "{name}: as many accepted points");
        let first = other.at.iter().zip(&a.at).position(|(x, y)| x.to_bits() != y.to_bits());
        assert_eq!(first, None, "{name}: the same accepted times (first differing at {:?})", first.map(|i| a.at[i]));
        let first = other.x.iter().zip(&a.x).position(|(x, y)| x.iter().zip(y).any(|(p, q)| p.to_bits() != q.to_bits()));
        assert_eq!(first, None, "{name}: the same solutions by bits (first differing at {:?})", first.map(|i| a.at[i]));
        assert_eq!(events, &a_events, "{name}: the same firmware statements at the same times");
    }
}

#[test]
fn function_marks_track_nested_calls_without_loop_or_branch_aliases() {
    use fab_elec_mcu_emu::Owner;
    let mut a = Asm::new();
    let (handler, outer, inner, looping, exit, idle) = (a.label(), a.label(), a.label(), a.label(), a.label(), a.label());
    a.jump(Target::Call, handler);
    let returned = a.here();
    a.bind(idle);
    a.emit(Nop);
    a.jump(Target::Rjmp, idle);
    a.bind(handler);
    let entry = a.here();
    a.jump(Target::Call, outer);
    let outer_return = a.here();
    a.emit(Nop);
    a.emit(Ret);
    a.bind(outer);
    let outer_start = a.here();
    a.emit(Nop); // unambiguous function start
    a.emit(Ldi(16, 1));
    a.emit(And(16, 16));
    a.jump(Target::Brbs(1), exit);
    a.jump(Target::Call, inner); // if c { inner() }, with nothing after it
    let inner_return = a.here();
    a.emit(Nop); // the conditional skip targets exit, not this return mark
    a.bind(exit);
    a.emit(Ret);
    let outer_end = a.here();
    a.bind(inner);
    let inner_start = a.here();
    a.emit(Nop); // a function opening with a loop: back edge skips the start mark
    a.emit(Ldi(17, 3));
    a.bind(looping);
    let loop_pc = a.here();
    a.emit(Dec(17));
    a.jump(Target::Brbc(1), looping);
    a.emit(Ret);
    let inner_end = a.here();
    let map = StepMap { statements: vec![loop_pc], entries: vec![(entry, 0)], returns: vec![returned],
        funcs: vec![(outer_start, outer_end, 0), (inner_start, inner_end, 1)],
        calls: vec![(outer_return, 0), (inner_return, 1)], owners: vec![Owner::Func(1)] };
    map.check().unwrap();
    let marks = map.marks();
    for pc in [outer_start, inner_start, outer_return, inner_return] { assert!(marks.contains(pc)); }
    let mut d = dev(&a.finish().unwrap(), Some(map));
    let mut bus = Script::new(0.0);
    bus.attach(&mut d);
    let mut seen = Vec::new();
    for cycle in 1..100 {
        bus.play(&mut d, at(0.0, cycle));
        let state = snapshot(&mut d, bus.t);
        assert!(state.chain.len() <= 2, "a loop back edge must not push another function");
        if seen.last() != Some(&state.chain) { seen.push(state.chain); }
    }
    assert_eq!(seen, [vec![], vec![0], vec![0, 1], vec![0], vec![]], "call return marks pop only the callee, then the handler returns");
}

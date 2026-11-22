//! ADC sample-and-hold against real circuit voltages and a rewound speculative stretch.
use fab_elec_mcu_avr::{asm::Asm, insn::{Insn::*, Ptr}, machine::AvrMachine};
use fab_elec_mcu_cosim::{At, CoMachine, Config, FwDebug, FwState, FwValue,
    LogicMachine, LogicNet, LogicPlan, NetDriver};
use fab_elec_mcu_emu::{time, FwTy, FwVar, Machine, PcSet, Ran, SampleAt, Until};
use fab_elec_sim::cosim::{Bus, CoRun, CoSim, CoSimDevice, CoSimPin, CoState, StepTo, Watch};
use fab_elec_sim::live::ValuePlan;
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::{GpioParams, PinMode, PinPull, Record, SimCircuit, SimDiag, Sink, Source, Wave};
use std::sync::{Arc, Mutex};

const HZ: u64 = 16_000_000;
const VDD: f64 = 3.3;
const WRITE: u64 = 11;
const FIRST_SAMPLE: u64 = WRITE + 1792;
const PERIOD: u64 = 13 * 128;
const END: u64 = WRITE + 3264 + 2 * PERIOD + 64;

fn store(a: &mut Asm, addr: u16, value: u8) {
    a.emit(Ldi(16, value)); a.emit(Sts(addr, 16));
}

// Free running at clk/128; the ADC ISR stores consecutive results in four-byte SRAM slots.
// The idle JMPs straddle the first sample, so the mark stays before it until the input hit.
fn image() -> Vec<u16> {
    let mut a = Asm::new();
    a.emit(Jmp(64));
    a.org(42); a.emit(Jmp(128));
    a.org(64);
    a.emit(Ldi(26, 0)); a.emit(Ldi(27, 1));
    store(&mut a, 0x7c, 0x46);
    store(&mut a, 0x7a, 0xef); // Completes at WRITE=11; enable, start, free running, interrupt.
    a.emit(Bset(7)); a.emit(Nop);
    a.emit(Jmp(a.here()));
    a.org(128);
    a.emit(Lds(18, 0x78)); a.emit(Lds(19, 0x79));
    a.emit(St(Ptr::XInc, 18)); a.emit(St(Ptr::XInc, 19));
    a.emit(Adiw(26, 2)); a.emit(Reti);
    a.finish().expect("ADC recorder assembles")
}

fn row(name: &str, pad: &str) -> CoSimPin {
    CoSimPin { name: name.into(), gpio: format!("u1#{name}.io"), pad: pad.into(),
        vss: "0".into(), vdd: "vdd".into(), vih: 0.524, vil: 0.418 }
}

fn run(m: &mut impl Machine, tick: u64, on_drive: bool) -> Ran {
    m.run(&Until { tick, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive })
}

#[test]
fn logic_wrapper_preserves_sample_events_and_maps_adc6_rows_both_ways() {
    // Hiding PB3 shifts ADC6 from inner row 20 to outward row 19.
    let plan = LogicPlan { nets: vec![LogicNet { name: "clock".into(), driver: NetDriver::Machine("PB3".into()) }], ..LogicPlan::default() };
    let mut m = LogicMachine::new(AvrMachine::new(&image(), HZ), plan).unwrap();
    let pin = m.pins().iter().position(|p| *p == "ADC6").unwrap();
    assert_eq!(pin, 19, "the digital wrapper really reindexes ADC6");
    assert!(m.analog_only(pin), "analog-only property follows the outward row");
    assert!(!m.analog_only(0), "GPIO keeps its digital side");
    m.mark();
    let at = SampleAt { tick: FIRST_SAMPLE, pin: pin as u16 };
    let generation = m.drive_gen();
    assert_eq!(run(&mut m, 4000, true), Ran::Drive { at: FIRST_SAMPLE }, "sample requests escape the wrapper without a drive edge");
    assert!(m.drive_gen() > generation, "an outward sample changes drive generation");
    let mut requests = Vec::new(); m.take_samples(&mut requests);
    assert_eq!(requests, [at], "request maps inner ADC6 to its outward row");
    m.set_sample(at, 1.25, 2.5);
    assert_eq!(run(&mut m, 3350, false), Ran::Tick, "the remapped answer reaches completion and the ISR");
    let mut bytes = [0; 4]; assert!(m.read_data(0x100, &mut bytes));
    assert_eq!(u32::from_le_bytes(bytes), 512, "the ISR stores the sample given to the outward row");
    m.rewind();
    m.set_sample(at, 1.25, 2.5);
    assert_eq!(run(&mut m, 4000, true), Ran::Drive { at: FIRST_SAMPLE });
    m.mark();
    requests.clear(); m.take_samples(&mut requests);
    m.rewind();
    let mut again = Vec::new(); m.take_samples(&mut again);
    assert_eq!(again, requests, "wrapper rewind restores its untaken requests");
    assert_eq!(run(&mut m, 3350, false), Ran::Tick);
    let mut replay = [0; 4]; assert!(m.read_data(0x100, &mut replay));
    assert_eq!(replay, bytes, "early replayed answer maps back to the inner pin");
}

#[derive(Default)]
struct Seen {
    reads: Vec<(f64, f64, f64)>,
    writes: Vec<(f64, u32, PinMode)>,
    codes: Vec<(u64, u16)>,
    reruns: u64,
}

// As in tests/logic.rs's Counted, observe the adapter running inside the real live worker.
struct Counted {
    dev: CoMachine<AvrMachine>,
    seen: Arc<Mutex<Seen>>,
}

struct ProbeBus<'a> {
    bus: &'a mut dyn Bus,
    seen: &'a Mutex<Seen>,
}

impl Bus for ProbeBus<'_> {
    fn t(&self) -> f64 { self.bus.t() }
    fn v(&self, pin: u32) -> f64 {
        let volts = self.bus.v(pin);
        if pin == 0 && self.bus.t() > 0.0 {
            self.seen.lock().unwrap().reads.push((self.bus.t(), volts, self.bus.supply(pin)));
        }
        volts
    }
    fn supply(&self, pin: u32) -> f64 { self.bus.supply(pin) }
    fn advance(&mut self, t: f64) -> Result<(), SimDiag> { self.bus.advance(t) }
    fn write(&mut self, pin: u32, mode: PinMode) -> bool {
        self.seen.lock().unwrap().writes.push((self.bus.t(), pin, mode));
        self.bus.write(pin, mode)
    }
    fn hits(&self) -> &[(u32, f64)] { self.bus.hits() }
}

impl CoSim for Counted {
    fn next(&self) -> f64 { self.dev.next() }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        let result = self.dev.run(&mut ProbeBus { bus, seen: &self.seen });
        assert!(self.dev.watches().iter().all(|w| w.pin != 0), "ADC6 has no digital level watches");
        let mut seen = self.seen.lock().unwrap();
        seen.reruns = self.dev.stats().reruns;
        let cpu = self.dev.machine().cpu();
        let code = u16::from_le_bytes([cpu.io(0x78), cpu.io(0x79)]);
        seen.codes.push((cpu.cycles, code));
        result
    }
    fn watches(&self) -> &[Watch] { self.dev.watches() }
    fn watch_gen(&self) -> u32 { self.dev.watch_gen() }
    fn arm(&mut self, t: f64, to: Option<StepTo>) -> bool { self.dev.arm(t, to) }
    fn state(&mut self, t: f64) -> Option<CoState> { self.dev.state(t) }
}

fn ramp(t: f64) -> f64 { 0.23 + (3.0 - 0.23) * (t / 500e-6) }

fn circuit_codes(hit: bool) -> (Vec<FwValue>, Seen) {
    let params = GpioParams { r_on_h: 26.0, r_on_l: 23.5, r_pull: 36e3, r_leak: 5.5e6,
        t_edge: 5e-9, vih: 0.524, vil: 0.418 };
    let mut c = SimCircuit::new();
    c.v("supply", "vdd", "0", Source::dc(VDD));
    c.gpio("u1#ADC6.io", "pot", "vdd", "0", params, PinMode::Input(PinPull::None));
    c.v("ramp", "pot", "0", Source::wave(Wave::Pwl(vec![(0.0, ramp(0.0)), (500e-6, ramp(500e-6))])));
    c.gpio("u1#PD2.io", "button", "vdd", "0", params, PinMode::Input(PinPull::None));
    // The hit is after the first sample but before its completion. The CPU has privately run
    // ahead to the second sample, so this rewinds across the first logged sample.
    let press = time(0.0, 3000, HZ);
    let key = if hit { Source::wave(Wave::Pwl(vec![(0.0, VDD), (press, VDD), (press + 0.1e-6, 0.0)])) }
        else { Source::dc(VDD) };
    c.v("key", "button", "0", key);
    c.opts.vntol = 1e-9;
    c.opts.abstol = 1e-15;
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, reltol: Some(1e-5),
        max_step: 5e-6, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("ramp circuit operating point converges");
    sim.values(1.0);
    let debug = FwDebug { vars: (0..3).map(|n| FwVar { name: format!("adc{n}"), ty: FwTy::I32,
        addr: 0x100 + n * 4, span: (0, 0) }).collect(), ..FwDebug::default() };
    let pins = vec![row("ADC6", "pot"), row("PD2", "button")];
    let dev = CoMachine::new(AvrMachine::new(&image(), HZ), Config { stretch: 10_000, debug: Some(Arc::new(debug)) }, &pins).unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    sim.attach(Some(CoSimDevice { dev: Box::new(Counted { dev, seen: seen.clone() }), pins }));
    let mut record = Record::default(); record.begin(&sim.solver().unknowns);
    sim.advance(time(0.0, END, HZ), None, &mut record).expect("ramp circuit reaches three conversions");
    let values = sim.values(1.0);
    let state = values.co.as_ref().unwrap().get::<FwState>().unwrap();
    assert!(!matches!(state.at, At::Halted(_)), "ADC recorder continues running: {:?}", state.at);
    let codes = state.vars.clone();
    let seen = std::mem::take(&mut *seen.lock().unwrap());
    for tick in [FIRST_SAMPLE, WRITE + 3264 + 192, WRITE + 3264 + PERIOD + 192] {
        assert!(record.at.contains(&time(0.0, tick, HZ)), "solver lands bit-exactly on sample tick {tick}");
    }
    (codes, seen)
}

#[test]
fn circuit_ramp_is_sampled_at_exact_ticks_once_even_after_rewind() {
    let ticks = [FIRST_SAMPLE, WRITE + 3264 + 192, WRITE + 3264 + PERIOD + 192];
    let expected = ticks.map(|tick| FwValue::I32((ramp(time(0.0, tick, HZ)) / VDD * 1024.0).floor() as i32));
    let (straight, before) = circuit_codes(false);
    let (rewound, after) = circuit_codes(true);
    assert_eq!(straight, expected, "each code uses the ramp at sample-and-hold and the circuit's 3.3 V supply");
    assert_eq!(rewound, expected, "a digital hit rewinds without changing any ADC result");
    assert_eq!(before.reruns, 0, "the baseline has no input-triggered rerun");
    assert!(after.reruns > 0, "the input hit really rewinds speculative CPU execution");
    for reads in [&before.reads, &after.reads] {
        assert_eq!(reads.len(), 3, "one pad read per sample, none during replay or digital input handling");
        for (&(t, volts, supply), tick) in reads.iter().zip(ticks) {
            assert_eq!(t.to_bits(), time(0.0, tick, HZ).to_bits(), "pad read at exact sample tick {tick}");
            assert!((volts - ramp(t)).abs() < 1e-8, "sample {tick} uses the solved ramp voltage");
            assert!((supply - VDD).abs() < 1e-8, "sample {tick} carries the actual circuit supply");
        }
    }
}

#[test]
fn compiled_pot_drives_pb5_on_the_first_sample_above_half_supply() {
    let source = "\
let pot = analog(ADC6)
let led = output(PB5)
let tick = every(ms(1), fn() { write(led, if analog_read(pot) > 511 { high } else { low }) })
";
    let fw = fab_elec_mcu_emit_avr::firmware(source, HZ as f64, &[]).expect("real pot firmware builds");
    let params = GpioParams { r_on_h: 26.0, r_on_l: 23.5, r_pull: 36e3, r_leak: 5.5e6,
        t_edge: 5e-9, vih: 0.524, vil: 0.418 };
    let mut c = SimCircuit::new();
    c.v("supply", "vdd", "0", Source::dc(5.0));
    c.gpio("u1#ADC6.io", "pot", "vdd", "0", params, PinMode::Input(PinPull::None));
    c.v("ramp", "pot", "0", Source::wave(Wave::Pwl(vec![(0.0, 0.0), (4e-3, 5.0), (6e-3, 5.0)])));
    c.gpio("u1#PB5.io", "led", "vdd", "0", params, PinMode::Input(PinPull::None));
    c.r("load", "led", "0", 10e3);
    c.opts.vntol = 1e-9; c.opts.abstol = 1e-15;
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, reltol: Some(1e-5),
        max_step: 20e-6, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("pot circuit converges");
    sim.values(1.0);
    let pins = vec![row("ADC6", "pot"), row("PB5", "led")];
    let dev = CoMachine::new(AvrMachine::new(&fw.words, HZ),
        Config { stretch: 16_000, debug: Some(Arc::new(fw.debug)) }, &pins).unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    sim.attach(Some(CoSimDevice { dev: Box::new(Counted { dev, seen: seen.clone() }), pins }));
    let mut record = Record::default(); record.begin(&sim.solver().unknowns);
    sim.advance(6e-3, None, &mut record).expect("pot firmware runs for several timer firings");
    let values = sim.values(1.0);
    let state = values.co.as_ref().unwrap().get::<FwState>().unwrap();
    assert!(!matches!(state.at, At::Halted(_)), "compiled firmware continues running: {:?}", state.at);
    let seen = seen.lock().unwrap();
    let codes: Vec<_> = seen.reads.iter().map(|&(t, volts, supply)| {
        assert!(record.at.contains(&t), "solver lands exactly on the firmware's sample-and-hold instant");
        assert!((supply - 5.0).abs() < 1e-8 && (volts - 5.0 * (t / 4e-3).min(1.0)).abs() < 1e-8,
            "firmware samples ADC6's solved ramp against the actual supply");
        (t, (volts / supply * 1024.0).floor().clamp(0.0, 1023.0) as u16)
    }).collect();
    let first = codes.iter().position(|&(_, code)| code >= 512).expect("ramp reaches half supply");
    assert!(first > 0 && first + 1 < codes.len(), "samples straddle the crossing and continue afterwards: {codes:?}");
    for &(t, code) in &codes {
        assert_eq!(code >= 512, t >= 2e-3, "sample code crosses 512 only at half the ramp supply");
        assert!(seen.codes.iter().any(|&(cycle, actual)| time(0.0, cycle, HZ) >= t && actual == code),
            "emulator converts the solved pad sample at {t} to code {code}");
    }
    let led: Vec<_> = seen.writes.iter().filter(|&&(_, pin, _)| pin == 1).copied().collect();
    let highs: Vec<_> = led.iter().filter(|&&(_, _, mode)| mode == PinMode::OutHigh).collect();
    assert_eq!(highs.len(), 1, "PB5 rises once and stays high: {led:?}");
    let high = highs[0].0;
    assert!(high >= codes[first].0 + time(0.0, 1472, HZ) && high < codes[first + 1].0,
        "PB5 rises after completion of the first above-half sample, before the next read: {high}, {codes:?}");
    assert!(led.iter().any(|&(t, _, mode)| t < codes[first].0 && mode == PinMode::OutLow),
        "PB5 is low while the sampled pot code is below half supply");
    assert!(led.iter().filter(|&&(t, _, _)| t > high).all(|&(_, _, mode)| mode == PinMode::OutHigh),
        "PB5 never returns low after the crossing");
}

//! The co-simulation hook: a process in the live worker, exact instants, pin writes, watches.

use fab_elec_sim::cosim::{Bus, CoRun, CoSim, CoSimDevice, CoSimPin, CoSimSlot, CoState, StepTo, Watch};
use fab_elec_sim::live::{Latest, LiveCmd, LiveValues, ValuePlan};
use fab_elec_sim::livesim::{run_live, LiveSim, LiveSpec};
use fab_elec_sim::trace::LogicEdge;
use fab_elec_sim::{GpioParams, PinEdge, PinMode, PinPull, Record, SimCircuit, SimDiag, Sink, Source, Wave};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

const PARAMS: GpioParams = GpioParams { r_on_h: 25.0, r_on_l: 25.0, r_pull: 40e3, r_leak: 1e12, t_edge: 5e-9, vih: 0.55, vil: 0.45 };

/// What a test device saw: per run, the time it asked for, the bus time, and the hits it got.
type Log = Arc<Mutex<Vec<(f64, f64, Vec<(u32, f64)>)>>>;

fn pin(gpio: &str, pad: &str) -> CoSimPin {
    CoSimPin {
        name: gpio.to_uppercase(), gpio: gpio.into(), pad: pad.into(), vss: "0".into(), vdd: "vdd".into(),
        vih: PARAMS.vih, vil: PARAMS.vil,
    }
}

fn live(c: SimCircuit, max_step: f64) -> (LiveSim, Record) {
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, reltol: Some(1e-5), max_step, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("the operating point converges");
    // a new simulation asks for its first publication; take it, so `advance` runs to its target
    sim.values(1.0);
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    record.point(sim.t(), None, &sim.solver().x);
    (sim, record)
}

fn pad_circuit(mode: PinMode, cap: f64) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.v("supply", "vdd", "0", Source::dc(3.3));
    c.gpio("pa0", "pad", "vdd", "0", PARAMS, mode);
    c.c("cap", "pad", "0", cap);
    c.opts.chgtol = 1e-20;
    c.opts.vntol = 1e-9;
    c.opts.abstol = 1e-15;
    c
}

/// Toggles pin 0 every `half` seconds, from high at t = 0.
struct Toggle {
    half: f64,
    k: u64,
    log: Log,
}

impl CoSim for Toggle {
    fn next(&self) -> f64 {
        self.k as f64 * self.half
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        self.log.lock().unwrap().push((self.next(), bus.t(), bus.hits().to_vec()));
        assert!((bus.supply(0) - 3.3).abs() < 1e-9, "the pin's supply is its vdd above its vss: {}", bus.supply(0));
        assert!(bus.supply(1).is_nan(), "a row past the pin table has no supply");
        assert!(bus.write(0, if self.k % 2 == 0 { PinMode::OutHigh } else { PinMode::OutLow }), "the toggle writes its pin");
        self.k += 1;
        Ok(CoRun::Go)
    }
    fn watches(&self) -> &[Watch] {
        &[]
    }
    fn watch_gen(&self) -> u32 {
        0
    }
    fn arm(&mut self, _t: f64, _to: Option<StepTo>) -> bool {
        false
    }
    fn state(&mut self, _t: f64) -> Option<CoState> {
        None
    }
}

fn toggled(slices: Option<f64>) -> (Record, Vec<(f64, f64, Vec<(u32, f64)>)>) {
    let tau = PARAMS.r_on_h * 1e-6;
    let (mut sim, mut record) = live(pad_circuit(PinMode::OutLow, 1e-6), tau / 20.0);
    let log = Log::default();
    sim.attach(Some(CoSimDevice { dev: Box::new(Toggle { half: 37e-6, k: 0, log: log.clone() }), pins: vec![pin("pa0", "pad")] }));
    let end = 10.0 * 37e-6;
    match slices {
        None => sim.advance(end, None, &mut record).expect("the toggled pad steps"),
        Some(dt) => {
            // many short calls, each with a wall budget that runs out at once
            while sim.t() < end {
                let target = (sim.t() + dt).min(end);
                sim.advance(target, Some(Duration::ZERO), &mut record).expect("the toggled pad steps in slices");
            }
        }
    }
    let log = log.lock().unwrap().clone();
    (record, log)
}

/// A pin toggled every 37 us by a process lands its writes on the exact instants it names; the
/// pad's RC response follows the closed form; the record does not depend on how the worker slices
/// its time.
#[test]
fn cosim_toggle_lands_exactly_and_is_deterministic() {
    let tau = PARAMS.r_on_h * 1e-6;
    let (record, log) = toggled(None);
    assert_eq!(log.len(), 11, "the process runs at t = 0 and at each of ten 37 us instants: {log:?}");
    for (k, &(asked, at, ref hits)) in log.iter().enumerate() {
        let want = k as f64 * 37e-6;
        assert_eq!(asked.to_bits(), want.to_bits(), "run {k} is for the instant the process named");
        assert_eq!(at.to_bits(), asked.to_bits(), "run {k}'s bus time is exactly that instant");
        assert!(hits.is_empty(), "no watch, no hits");
        assert!(record.at.iter().any(|&t| t.to_bits() == want.to_bits()), "the record has a point at exactly {want:e}");
    }
    for k in 1..10 {
        let t0 = k as f64 * 37e-6;
        let (v0, target) = if k % 2 == 0 { (record.at_time("pad", t0), 3.3) } else { (record.at_time("pad", t0), 0.0) };
        for frac in [0.25, 0.5, 0.9] {
            let t = t0 + frac * 37e-6;
            let want = target + (v0 - target) * (-(t - t0 - PARAMS.t_edge / 2.0) / tau).exp();
            let got = record.at_time("pad", t);
            assert!((got - want).abs() < 2e-3 * 3.3, "half period {k} at {frac}: the pad follows the RC response, {got} versus {want}");
        }
    }
    let (sliced, sliced_log) = toggled(Some(1.3e-6));
    assert_eq!(sliced_log, log, "the process runs at the same instants however the worker slices time");
    assert_eq!(sliced.at, record.at, "the same accepted times however the worker slices time");
    assert_eq!(sliced.x, record.x, "the same solutions however the worker slices time");
}

/// Watches a pull-up input's falling edge; optionally releases a second pin at 10 us and advances
/// through its rise inside that run.
struct Button {
    watches: Vec<Watch>,
    gen: u32,
    next: f64,
    nested: bool,
    log: Log,
}

impl CoSim for Button {
    fn next(&self) -> f64 {
        self.next
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        let t = bus.t();
        if t == 0.0 {
            assert!(bus.write(0, PinMode::Input(PinPull::Up)), "the button pin starts as a pulled-up input");
            if self.nested {
                assert!(bus.write(1, PinMode::OutLow), "the second pin starts low");
                self.watches.push(Watch { id: 2, pin: 1, level: 1.65, edge: PinEdge::Rising });
            }
            self.watches.push(Watch { id: 1, pin: 0, level: 1.65, edge: PinEdge::Falling });
            self.gen += 1;
            self.next = if self.nested { 10e-6 } else { f64::INFINITY };
        } else if self.nested && t == 10e-6 {
            assert!(bus.write(1, PinMode::Input(PinPull::Up)), "the second pin is released");
            bus.advance(30e-6)?;
            assert_eq!(bus.t(), 30e-6, "a nested advance ends exactly where it was asked to");
            bus.advance(20e-6)?;
            assert_eq!(bus.t(), 30e-6, "an advance to the past is a no-op");
            self.next = f64::INFINITY;
        }
        self.log.lock().unwrap().push((self.next, t, bus.hits().to_vec()));
        Ok(CoRun::Go)
    }
    fn watches(&self) -> &[Watch] {
        &self.watches
    }
    fn watch_gen(&self) -> u32 {
        self.gen
    }
    fn arm(&mut self, _t: f64, _to: Option<StepTo>) -> bool {
        false
    }
    fn state(&mut self, _t: f64) -> Option<CoState> {
        None
    }
}

/// A pulled-up input whose button (a 1 kOhm path to a key that falls at 50 us) pulls it low fires
/// the process's watch: it runs at the hit, once, with the interpolated crossing time.
#[test]
fn cosim_watch_hit_runs_the_process_at_the_crossing() {
    let mut c = pad_circuit(PinMode::Input(PinPull::None), 5e-12);
    c.v("key", "k", "0", Source::wave(Wave::Pulse { v1: 3.3, v2: 0.0, td: 50e-6, tr: 1e-6, tf: 1e-6, pw: 1.0, per: 2.0 }));
    c.r("button", "pad", "k", 1e3);
    let (mut sim, mut record) = live(c, 0.1e-6);
    let log = Log::default();
    let dev = Button { watches: Vec::new(), gen: 0, next: 0.0, nested: false, log: log.clone() };
    sim.attach(Some(CoSimDevice { dev: Box::new(dev), pins: vec![pin("pa0", "pad")] }));
    sim.advance(60e-6, None, &mut record).expect("the button bench steps");
    let log = log.lock().unwrap().clone();
    assert_eq!(log.len(), 2, "the process runs at t = 0 and at the hit, nowhere else: {log:?}");
    let (_, at, hits) = &log[1];
    assert_eq!(hits.len(), 1, "the hit run carries one hit");
    let (id, hit) = hits[0];
    assert_eq!(id, 1, "the falling watch fired");
    // the pad divides the key between the 40 kOhm pull-up and the 1 kOhm button: 1.65 V when
    // the key is at (1.65 - 3.3/41) * 41/40 V on its 1 us fall
    let key = (1.65 - 3.3 / 41.0) * 41.0 / 40.0;
    let want = 50e-6 + (3.3 - key) / 3.3 * 1e-6;
    assert!((hit - want).abs() < 10e-9, "the hit is at the divider's crossing: {hit:e} versus {want:e}");
    assert!(*at >= hit && *at - hit < 1e-3 * 0.1e-6, "the process runs at the accepted point just past the crossing: {at:e}");
    assert!((record.at_time("pad", hit) - 1.65).abs() < 1e-3, "the record crosses the level at the hit");
}

/// A crossing inside a nested advance is delivered through `hits()` in the same run and never
/// starts another run.
#[test]
fn cosim_nested_advance_delivers_hits_without_reentry() {
    let mut c = pad_circuit(PinMode::Input(PinPull::None), 5e-12);
    c.gpio("pa1", "pad1", "vdd", "0", PARAMS, PinMode::Input(PinPull::None));
    c.c("cap1", "pad1", "0", 100e-12);
    let (mut sim, mut record) = live(c, 0.2e-6);
    let log = Log::default();
    let dev = Button { watches: Vec::new(), gen: 0, next: 0.0, nested: true, log: log.clone() };
    sim.attach(Some(CoSimDevice { dev: Box::new(dev), pins: vec![pin("pa0", "pad"), pin("pa1", "pad1")] }));
    sim.advance(40e-6, None, &mut record).expect("the nested bench steps");
    let log = log.lock().unwrap().clone();
    assert_eq!(log.len(), 2, "two runs (t = 0 and 10 us): the nested crossing starts none: {log:?}");
    let (_, at, hits) = &log[1];
    assert_eq!(*at, 10e-6, "the second run is at its instant");
    assert_eq!(hits.len(), 1, "the nested advance's crossing is in hits(): {hits:?}");
    let (id, hit) = hits[0];
    let rc = PARAMS.r_pull * 100e-12;
    let want = 10e-6 + rc * 2.0_f64.ln();
    assert_eq!(id, 2, "the second pin's rising watch fired");
    assert!((hit - want).abs() < 0.02 * rc, "the nested hit is at the RC crossing: {hit:e} versus {want:e}");
    assert_eq!(sim.stats().co_runs, 2, "the worker counts the same two runs");
    assert!(record.at.iter().any(|&t| t == 30e-6), "the nested advance's end is a recorded point");
}

/// Writes a 30 % PWM at t = 0 and a 60 % one at 12 us, inside period 1's high phase.
struct Pwm {
    next: f64,
}

impl CoSim for Pwm {
    fn next(&self) -> f64 {
        self.next
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        let high = if bus.t() == 0.0 { 3e-6 } else { 6e-6 };
        assert!(bus.write(0, PinMode::Pwm { period: 10e-6, high, origin: 0.0 }), "the PWM write is accepted");
        self.next = if bus.t() == 0.0 { 12e-6 } else { f64::INFINITY };
        Ok(CoRun::Go)
    }
    fn watches(&self) -> &[Watch] {
        &[]
    }
    fn watch_gen(&self) -> u32 {
        0
    }
    /// Nothing ever runs again after its last write (an infinite `next()` and no watches): a step
    /// could never end, so it is refused.
    fn arm(&mut self, _t: f64, to: Option<StepTo>) -> bool {
        to.is_none() || self.next.is_finite()
    }
    fn state(&mut self, _t: f64) -> Option<CoState> {
        None
    }
}

/// A duty change written mid-period is a preload: the running period keeps its duty, the next
/// starts on the same grid with the new one. A firmware step on a device that will never run again
/// is refused.
#[test]
fn cosim_pwm_duty_change_takes_effect_at_the_period_start() {
    let (mut sim, mut record) = live(pad_circuit(PinMode::OutLow, 10e-12), 0.5e-6);
    sim.attach(Some(CoSimDevice { dev: Box::new(Pwm { next: 0.0 }), pins: vec![pin("pa0", "pad")] }));
    sim.advance(40e-6, None, &mut record).expect("the PWM bench steps");
    for (period, high) in [(0, 3e-6), (1, 3e-6), (2, 6e-6), (3, 6e-6)] {
        let start = period as f64 * 10e-6;
        assert!(record.at.iter().any(|&t| t == start), "period {period} starts on a recorded grid point");
        assert!(record.at_time("pad", start + high - 0.2e-6) > 3.2, "period {period} is high just before {high:e}");
        assert!(record.at_time("pad", start + high + 0.2e-6) < 0.1, "period {period} is low just after {high:e}");
    }
    // after its last write nothing runs again (no next instant, no watches): a step is refused
    let t = sim.t();
    assert!(sim.apply(LiveCmd::CoStep(StepTo::Statement)), "a refused step keeps the worker");
    assert_eq!(sim.stats().refused, 1, "a step that could never end is refused");
    assert!(!sim.running() && sim.t() == t, "a refused step runs nothing");
}

/// A process attached by command mid-run runs at once (its first instant is past), a `Replace`
/// keeps it (its pins resolve again in the new circuit, its instants go on), and an empty slot
/// detaches it.
#[test]
fn cosim_attach_by_command_survives_a_replace_and_detaches() {
    let half = 10e-6;
    let (mut sim, mut record) = live(pad_circuit(PinMode::OutLow, 1e-9), 0.5e-6);
    sim.advance(25e-6, None, &mut record).expect("the bare pad steps");
    let t_attach = sim.t();
    let log = Log::default();
    // its first instant (20 us) is past at the attach
    let dev = CoSimDevice { dev: Box::new(Toggle { half, k: 2, log: log.clone() }), pins: vec![pin("pa0", "pad")] };
    assert!(sim.apply(LiveCmd::CoSim(CoSimSlot::new(Some(dev)))), "an attach keeps the worker");
    sim.advance(45e-6, None, &mut record).expect("the toggled pad steps");
    // a new lowering: the same pad with a load on it
    let mut c = pad_circuit(PinMode::OutLow, 1e-9);
    c.r("load", "pad", "0", 10e3);
    let plan = ValuePlan::default();
    assert!(sim.apply(LiveCmd::Replace { generation: 2, circuit: Box::new(c), plan: Box::new(plan) }), "the Replace keeps the worker");
    assert_eq!(sim.stats().replaced, 1, "the Replace took");
    sim.values(1.0);
    sim.advance(75e-6, None, &mut record).expect("the replaced pad steps");
    assert!(sim.apply(LiveCmd::CoSim(CoSimSlot::new(None))), "a detach keeps the worker");
    sim.advance(100e-6, None, &mut record).expect("the detached pad steps");
    let log = log.lock().unwrap().clone();
    let at: Vec<f64> = log.iter().map(|r| r.1).collect();
    // the first run is at the attach, then at 30, 40, ... us until the detach at 75 us: across
    // the Replace at 45 us
    assert!(t_attach > 20e-6 && t_attach < 30e-6, "the attach is between the process's instants: {t_attach:e}");
    assert_eq!(at[0], t_attach, "the attached process runs at once: {at:?}");
    let instants: Vec<f64> = (3..=7).map(|k| k as f64 * half).collect();
    assert_eq!(&at[1..], &instants[..], "the process runs at its instants, across the Replace, until the detach");
    // its writes reach the new circuit's GPIO: after the Replace the pad still toggles
    let k_high = instants.iter().position(|&t| t > 45e-6 && ((t / half).round() as u64) % 2 == 0).expect("a high write after the Replace");
    let t_high = instants[k_high];
    assert!(record.at_time("pad", t_high + 0.5 * half) > 3.0, "a write after the Replace drives the new circuit's pad high");
    assert_eq!(sim.stats().co_runs, at.len() as u64, "the worker counts each run");
}

/// Drives DATA, a net it keeps off the circuit (`CoSim::logic_nets`: a line to a chip it emulates
/// with it, as an MCU's to a shift register), high for the first `high` of every `period` from
/// t = 0; its edges are handed over as the analog reaches them.
struct OffCircuit {
    period: f64,
    high: f64,
    k: u64,
    edges: Vec<(u32, LogicEdge)>,
    nets: Vec<String>,
}

impl CoSim for OffCircuit {
    fn next(&self) -> f64 {
        (self.k / 2) as f64 * self.period + if self.k % 2 == 1 { self.high } else { 0.0 }
    }
    fn run(&mut self, _bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        self.edges.push((0, LogicEdge { t: self.next(), high: self.k % 2 == 0 }));
        self.k += 1;
        Ok(CoRun::Go)
    }
    fn watches(&self) -> &[Watch] {
        &[]
    }
    fn watch_gen(&self) -> u32 {
        0
    }
    fn arm(&mut self, _t: f64, _to: Option<StepTo>) -> bool {
        false
    }
    fn state(&mut self, _t: f64) -> Option<CoState> {
        None
    }
    fn logic_nets(&self) -> &[String] {
        &self.nets
    }
    fn take_logic(&mut self, t: f64, _sync: bool, out: &mut Vec<(u32, LogicEdge)>) {
        let n = self.edges.iter().take_while(|e| e.1.t <= t).count();
        out.extend(self.edges.drain(..n));
    }
}

/// A net the firmware drives off the circuit is valued from its logic record at the process's
/// rails, where the circuit has no unknown for it (it read a flat 0 V): each grid frame's mean is
/// the time it stood high times the supply, its minimum and maximum the levels it took there, and
/// the instant its level now. DATA high for 0.3 ms of every 1.1 ms on a 3.3 V supply, in frames of
/// 0.25 ms (no edge on a frame's bound but the first): wholly high, falling inside, low, and
/// rising inside again.
#[test]
fn a_net_the_firmware_drives_off_the_circuit_reads_its_levels_at_the_rails() {
    const PERIOD: f64 = 1.1e-3;
    const HIGH: f64 = 0.3e-3;
    const GRID: f64 = 0.25e-3;
    const VDD: f64 = 3.3;
    let plan = ValuePlan { nets: vec!["vdd".into(), "pad".into(), "DATA".into()], ..ValuePlan::default() };
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: 20e-6, trace: true, grid: GRID, ..LiveSpec::default() };
    let mut sim = LiveSim::new(pad_circuit(PinMode::OutLow, 1e-9), plan, 1, spec).expect("the operating point converges");
    sim.values(1.0);
    let nets = vec!["DATA".to_owned()];
    let dev = CoSimDevice { dev: Box::new(OffCircuit { period: PERIOD, high: HIGH, k: 0, edges: Vec::new(), nets }), pins: vec![pin("pa0", "pad")] };
    sim.attach(Some(dev));
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    // half way into the third period's high stretch
    let end = 2.0 * PERIOD + 0.5 * HIGH;
    sim.advance(end, None, &mut record).expect("the pad steps");
    let v = sim.values(1.0);
    assert!(v.frames.len() >= 8, "the run's frames: {}", v.frames.len());
    let close = |a: f32, b: f64| (f64::from(a) - b).abs() < 1e-4;
    for f in &v.frames {
        // the stretch of [since, t) DATA stands high, by period
        let high: f64 = (0..3).map(|p| {
            let (a, b) = (p as f64 * PERIOD, p as f64 * PERIOD + HIGH);
            (f.t.min(b) - f.since.max(a)).max(0.0)
        }).sum();
        let span = f.t - f.since;
        let (mean, min, max) = (VDD * high / span, if high < span - 1e-12 { 0.0 } else { VDD }, if high > 1e-12 { VDD } else { 0.0 });
        assert!(close(f.net_mean[2], mean) && close(f.net_min[2], min) && close(f.net_max[2], max),
            "DATA over [{:.3e}, {:.3e}) s reads {:.4} V mean, {:.4}..{:.4} V, its levels at the rails {mean:.4} V, {min}..{max} V",
            f.since, f.t, f.net_mean[2], f.net_min[2], f.net_max[2]);
    }
    assert!(close(v.net_v[2], VDD), "DATA now (high, at {end:.3e} s) reads the supply: {} V", v.net_v[2]);
}

const F_CLK: f64 = 1e6;

/// A cycle's simulated time: the toy's one conversion (`t_reset + cycle / f_clk`, reset at 0).
fn at(cycle: u64) -> f64 {
    cycle as f64 / F_CLK
}

/// The first cycle at or after `t`, through `at` (no second conversion).
fn cycle_at(t: f64) -> u64 {
    let mut c = (t * F_CLK).floor() as u64;
    while at(c) < t {
        c += 1;
    }
    while c > 0 && at(c - 1) >= t {
        c -= 1;
    }
    c
}

const PERIOD: u64 = 50;
const TIMER: [u64; 3] = [0, 4, 9];
const TIMER_RET: u64 = 12;
const BUTTON: [u64; 2] = [0, 3];
const BUTTON_RET: u64 = 6;
const LATENCY: u64 = 2;

/// A device's frames: (cycle, what), in order.
type Frames = Arc<Mutex<Vec<(u64, &'static str)>>>;

/// A toy MCU at 1 MHz with the stepping rules of the real adapter. A timer handler enters every 50
/// cycles: statements at +0, +4 and +9, the one at +4 toggles pin 0, and it returns at +12. A
/// falling edge on pin 1 enters the button handler 2 cycles after the edge's cycle: statements at
/// +0 and +3, the one at +3 drives pin 0 high, and it returns at +6. It sleeps in between. Only the
/// pin writes are instants the analog must land on; the other statements matter to an armed step.
struct Tiny {
    /// Every point before this cycle has run.
    cycle: u64,
    button: Option<u64>,
    led: bool,
    /// (kind, arming cycle, the return cycle a `HandlerExit` waits for).
    armed: Option<(StepTo, u64, Option<u64>)>,
    watches: Vec<Watch>,
    frames: Frames,
}

/// What `Tiny` shows of itself at a publication.
struct TinyState {
    t: f64,
    cycle: u64,
    led: bool,
}

impl Tiny {
    fn device(frames: Frames) -> CoSimDevice {
        let watches = vec![Watch { id: 1, pin: 1, level: 1.65, edge: PinEdge::Falling }];
        let dev = Tiny { cycle: 0, button: None, led: false, armed: None, watches, frames };
        CoSimDevice { dev: Box::new(dev), pins: vec![pin("pa0", "pad"), pin("pa1", "btn")] }
    }

    /// The handlers around cycle `c`: (entry, statement offsets, return offset).
    fn handlers(&self, c: u64) -> impl Iterator<Item = (u64, &'static [u64], u64)> + '_ {
        let k = c / PERIOD;
        let timers = (k.max(2) - 1..k + 2).map(|k| (k * PERIOD, &TIMER[..], TIMER_RET));
        timers.chain(self.button.map(|b| (b, &BUTTON[..], BUTTON_RET)))
    }

    /// The handler running at cycle `c`: its entry and its return cycle.
    fn running(&self, c: u64) -> Option<(u64, u64)> {
        self.handlers(c).find(|&(e, _, ret)| e <= c && c < e + ret).map(|(e, _, ret)| (e, e + ret))
    }

    fn stops(&self, c: u64) -> bool {
        let Some((to, after, exit)) = self.armed else { return false };
        c > after && match to {
            StepTo::Statement => self.handlers(c).any(|(e, stmts, _)| c >= e && stmts.contains(&(c - e))),
            StepTo::HandlerEntry => self.handlers(c).any(|(e, _, _)| e == c),
            StepTo::HandlerExit => exit == Some(c),
        }
    }

    /// The pin 0 level a write at cycle `c` drives, if one is there.
    fn write_at(&self, c: u64) -> Option<bool> {
        if self.button.is_some_and(|b| c == b + BUTTON[1]) {
            Some(true)
        } else if c >= PERIOD && c % PERIOD == TIMER[1] {
            Some(!self.led)
        } else {
            None
        }
    }

    /// The next cycle the analog must land on: a write, or the armed step's stop.
    fn due(&self) -> u64 {
        (self.cycle..).find(|&c| self.write_at(c).is_some() || self.stops(c)).expect("a timer write every 50 cycles")
    }
}

impl CoSim for Tiny {
    fn next(&self) -> f64 {
        at(self.due())
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        for &(_, edge) in bus.hits() {
            // the CPU had run privately past the edge: it re-runs from the stretch start to the
            // edge's cycle and takes the interrupt there
            let c = cycle_at(edge);
            self.frames.lock().unwrap().push((c, "rerun"));
            self.button = Some(c + LATENCY);
        }
        let c = self.due();
        if at(c) != bus.t() {
            // a hit between cycles: nothing is due yet
            return Ok(CoRun::Go);
        }
        if self.stops(c) {
            self.armed = None;
            self.cycle = c;
            self.frames.lock().unwrap().push((c, "stop"));
            return Ok(CoRun::Stopped);
        }
        if let Some(high) = self.write_at(c) {
            assert!(bus.write(0, if high { PinMode::OutHigh } else { PinMode::OutLow }), "the toy writes its LED pin");
            self.led = high;
            self.frames.lock().unwrap().push((c, if high { "on" } else { "off" }));
        }
        self.cycle = c + 1;
        Ok(CoRun::Go)
    }
    fn watches(&self) -> &[Watch] {
        &self.watches
    }
    fn watch_gen(&self) -> u32 {
        1
    }
    fn arm(&mut self, t: f64, to: Option<StepTo>) -> bool {
        let Some(to) = to else {
            self.armed = None;
            return true;
        };
        let after = cycle_at(t);
        let exit = match to {
            StepTo::HandlerExit => match self.running(after) {
                Some((_, ret)) => Some(ret),
                None => return false,
            },
            _ => None,
        };
        self.armed = Some((to, after, exit));
        true
    }
    fn state(&mut self, t: f64) -> Option<CoState> {
        Some(CoState(Arc::new(TinyState { t, cycle: self.cycle, led: self.led })))
    }
}

/// The LED pad, and a pulled-up button pin a key pulls low through 1 kOhm.
fn tiny_circuit(key: Source) -> SimCircuit {
    let mut c = pad_circuit(PinMode::OutLow, 1e-9);
    c.gpio("pa1", "btn", "vdd", "0", PARAMS, PinMode::Input(PinPull::Up));
    c.c("cbtn", "btn", "0", 5e-12);
    c.v("key", "k", "0", key);
    c.r("button", "btn", "k", 1e3);
    c
}

/// Advances until the run stops (a step's end), or to `t` (`f64::INFINITY`: until it stops), taking
/// each publication a command or a stop asks for (`advance` returns at one), in slices of `slice`
/// simulated seconds with no wall budget when given.
fn run_until(sim: &mut LiveSim, record: &mut Record, t: f64, slice: Option<f64>) {
    while sim.running() && sim.t() < t {
        let target = slice.map_or(t, |dt| (sim.t() + dt).min(t));
        sim.advance(target, slice.map(|_| Duration::ZERO), record).expect("the toy bench converges");
        sim.values(1.0);
    }
}

/// Steps to the toy's stops from t = 0, either at once or in slices whose wall budget runs out at
/// once: per stop, its time and the published state's (time, cycle, LED).
fn stepped(slices: Option<f64>) -> (Record, Vec<(u64, &'static str)>, Vec<(f64, f64, u64, bool)>) {
    let (mut sim, mut record) = live(tiny_circuit(Source::dc(3.3)), 2e-6);
    let frames = Frames::default();
    sim.attach(Some(Tiny::device(frames.clone())));
    let mut stops = Vec::new();
    for (k, to) in [StepTo::HandlerEntry, StepTo::Statement, StepTo::Statement, StepTo::Statement, StepTo::HandlerExit].into_iter().enumerate() {
        assert!(sim.apply(LiveCmd::CoStep(to)), "step {k}: the command keeps the worker");
        assert!(sim.running() && sim.stepping(), "step {k}: an armed step runs the paused bench");
        run_until(&mut sim, &mut record, f64::INFINITY, slices);
        assert!(!sim.running() && !sim.stepping(), "step {k}: the stop pauses the run");
        let v = sim.values(1.0);
        assert!(!v.stepping, "step {k}: the publication says the step is over");
        let state = v.co.as_ref().and_then(|c| c.get::<TinyState>()).expect("every publication with a device carries its state");
        stops.push((sim.t(), state.t, state.cycle, state.led));
    }
    assert_eq!(sim.stats().co_stops, 5, "five steps, five stops");
    // the handler at 100 has returned: no handler runs, so a handler exit cannot be stepped to
    assert!(sim.apply(LiveCmd::CoStep(StepTo::HandlerExit)), "a refused step keeps the worker");
    assert_eq!(sim.stats().refused, 1, "a handler exit while sleeping is refused");
    assert!(!sim.running(), "a refused step runs nothing");
    // Pause disarms: playing on passes the timer handler at 150 without a stop
    assert!(sim.apply(LiveCmd::CoStep(StepTo::Statement)), "the statement step is armed");
    assert!(sim.apply(LiveCmd::Pause), "Pause keeps the worker");
    assert!(!sim.running() && !sim.stepping(), "Pause cancels the step");
    assert!(sim.apply(LiveCmd::Play), "Play keeps the worker");
    run_until(&mut sim, &mut record, at(160), slices);
    assert_eq!(sim.stats().co_stops, 5, "a cancelled step never stops the run");
    // and with no device a step is refused
    assert!(sim.apply(LiveCmd::CoSim(CoSimSlot::new(None))), "a detach keeps the worker");
    assert!(sim.apply(LiveCmd::CoStep(StepTo::Statement)), "a refused step keeps the worker");
    assert_eq!(sim.stats().refused, 2, "a step with no device is refused");
    let frames = frames.lock().unwrap().clone();
    (record, frames, stops)
}

/// A firmware step (`LiveCmd::CoStep`) runs to the device's next stop of its kind and lands on it
/// bit-exact (the device's own cycle time), pauses, and publishes the device's state at that very
/// instant. Handler exits are refused while sleeping, Pause cancels a step, and the record does not
/// depend on how the worker slices its time.
#[test]
fn a_firmware_step_stops_bit_exact_and_publishes_its_state() {
    let (record, frames, stops) = stepped(None);
    // entry 50; statements 54 (its toggle not yet run), 59 (it ran), 100 (the next handler's
    // first); that handler's exit at 112 (its toggle at 104 ran on the way)
    for (k, (&(t, state_t, cycle, led), (want, lit))) in stops.iter().zip([(50, false), (54, false), (59, true), (100, true), (112, false)]).enumerate() {
        assert_eq!(t.to_bits(), at(want).to_bits(), "stop {k} lands bit-exact on cycle {want}'s time: {t:e}");
        assert_eq!(state_t.to_bits(), t.to_bits(), "stop {k}: the state is of the published instant");
        assert_eq!((cycle, led), (want, lit), "stop {k}: the device stands at cycle {want}, LED {lit}");
    }
    assert!(record.at_time("pad", at(58)) > 3.2, "the toggle at 54 drove the LED pad high before the stop at 59");
    assert!(!frames.contains(&(150, "stop")) && frames.contains(&(154, "on")), "the cancelled step ran on through 154: {frames:?}");
    let (sliced, sliced_frames, sliced_stops) = stepped(Some(1.3e-6));
    assert_eq!(sliced_stops, stops, "the same stops however the worker slices time");
    assert_eq!(sliced_frames, frames, "the same frames however the worker slices time");
    assert_eq!(sliced.at, record.at, "the same accepted times however the worker slices time");
    assert_eq!(sliced.x, record.x, "the same solutions however the worker slices time");
}

/// A button pressed by command while a handler-entry step waits (the key falls at 73.2 us) reaches
/// the circuit at once (commands are not held behind a firmware step); the toy re-runs to the edge
/// and stops at the button handler's entry, with the same stop and frames as a run where the press
/// was scheduled from the start.
#[test]
fn a_press_during_a_step_reaches_the_circuit_and_stops_as_if_scheduled() {
    // a piecewise-linear key keeps its own time origin through a retune (a periodic one would start
    // its period at the retune)
    let press = Source::wave(Wave::Pwl(vec![(0.0, 3.3), (73.2e-6, 3.3), (73.4e-6, 0.0)]));
    let run = |scheduled: bool| {
        let (mut sim, mut record) = live(tiny_circuit(if scheduled { press.clone() } else { Source::dc(3.3) }), 2e-6);
        let frames = Frames::default();
        sim.attach(Some(Tiny::device(frames.clone())));
        // play past the first timer handler (it returns at 62), then step to the next handler entry
        assert!(sim.apply(LiveCmd::Play), "Play keeps the worker");
        run_until(&mut sim, &mut record, at(65), None);
        assert!(sim.apply(LiveCmd::Pause), "Pause keeps the worker");
        assert!(sim.apply(LiveCmd::CoStep(StepTo::HandlerEntry)), "the step command keeps the worker");
        if !scheduled {
            run_until(&mut sim, &mut record, at(70), None);
            assert!(sim.stepping() && sim.t() >= at(70), "at 70 us the step still waits for a handler");
            let retuned = sim.stats().retuned;
            assert!(sim.apply(LiveCmd::Retune { generation: sim.generation(), device: "key".into(), source: press.clone() }), "the press keeps the worker");
            assert_eq!(sim.stats().retuned, retuned + 1, "the press reaches the circuit at once, not held behind the step");
        }
        run_until(&mut sim, &mut record, f64::INFINITY, None);
        assert!(!sim.running(), "the step stopped");
        let frames = frames.lock().unwrap().clone();
        (sim.t(), frames, record)
    };
    let (t, frames, record) = run(false);
    let (t_scheduled, frames_scheduled, _) = run(true);
    // the key crosses the button's 1.65 V threshold near 73.3 us: the edge's cycle is 74, the
    // handler enters 2 cycles later
    assert_eq!(t.to_bits(), at(76).to_bits(), "the step stops at the button handler's entry, bit-exact: {t:e}");
    assert_eq!(frames, frames_scheduled, "the same frames as a press scheduled from the start");
    assert_eq!(t.to_bits(), t_scheduled.to_bits(), "the same stop as a press scheduled from the start");
    assert!(frames.contains(&(74, "rerun")), "the toy re-ran to the edge: {frames:?}");
    assert!(record.at_time("btn", at(75)) < 0.5, "the press pulled the button pin low before the stop");
}

/// In the live loop a firmware step is not paced: at 1 us of simulated time per wall second, a paced
/// run to cycle 50 would take 50 s, and the step ends at once. Its stop is published with the
/// device's state, and every publication seen carries the state at its own instant.
#[test]
fn a_firmware_step_in_the_live_loop_runs_unpaced_and_publishes_its_stop() {
    let spec = LiveSpec { rate: 1e-6, playing: false, reltol: Some(1e-5), max_step: 2e-6, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(tiny_circuit(Source::dc(3.3)), ValuePlan::default(), 1, spec).expect("the operating point converges");
    sim.attach(Some(Tiny::device(Frames::default())));
    let latest = Arc::new(Latest::<LiveValues>::new());
    let (commands, command_rx) = mpsc::sync_channel(4);
    let (mut traces, _trace_rx) = mpsc::sync_channel(64);
    let published = Arc::clone(&latest);
    let worker = thread::spawn(move || run_live(sim, &command_rx, &published, &mut traces, &mut Record::default(), &|| {}));
    commands.try_send(LiveCmd::CoStep(StepTo::HandlerEntry)).expect("the step reaches the worker");
    // no deadline: a paced step shows as a stalled test
    loop {
        if let Some((_, v)) = latest.latest() {
            let state = v.co.as_ref().and_then(|c| c.get::<TinyState>()).expect("every publication with a device carries its state");
            assert_eq!(state.t.to_bits(), v.t.to_bits(), "a publication's state is of its own instant");
            if v.t == at(50) && !v.stepping {
                assert_eq!(state.cycle, 50, "the published stop is the handler's entry");
                break;
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    commands.try_send(LiveCmd::Stop).expect("Stop reaches the worker");
    let sim = worker.join().expect("the worker joins without a panic").expect("the run ends without a solver diagnostic");
    assert_eq!(sim.stats().co_stops, 1, "one step, one stop");
}

/// Writes pins 0 and 1 together at each `half` instant (a timer that sets both of its outputs at
/// one tick), both high at t = 0, both low at the next instant, and so on.
struct Pair {
    half: f64,
    k: u64,
}

impl CoSim for Pair {
    fn next(&self) -> f64 {
        self.k as f64 * self.half
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        let mode = if self.k % 2 == 0 { PinMode::OutHigh } else { PinMode::OutLow };
        assert!(bus.write(0, mode) && bus.write(1, mode), "the pair writes both of its pins");
        self.k += 1;
        Ok(CoRun::Go)
    }
    fn watches(&self) -> &[Watch] {
        &[]
    }
    fn watch_gen(&self) -> u32 {
        0
    }
    fn arm(&mut self, _t: f64, _to: Option<StepTo>) -> bool {
        false
    }
    fn state(&mut self, _t: f64) -> Option<CoState> {
        None
    }
}

/// An edge instant of a process restarts the stepper's history once per cause it has: each pin
/// written there changes a pad (one change per pin), and the pads' ramps end together (one
/// corner). The step that lands on the instant (the advance's end) is no restart. The instant
/// itself counts once however many pins change at it. These are the per-edge counts a time
/// resolution is measured against.
#[test]
fn an_edge_instant_restarts_once_per_cause_and_counts_once() {
    let tau = PARAMS.r_on_h * 1e-6;
    let mut c = pad_circuit(PinMode::OutLow, 1e-6);
    c.gpio("pa1", "pad1", "vdd", "0", PARAMS, PinMode::OutLow);
    c.c("cap1", "pad1", "0", 1e-6);
    let (mut sim, mut record) = live(c, tau / 20.0);
    sim.attach(Some(CoSimDevice { dev: Box::new(Pair { half: 37e-6, k: 0 }), pins: vec![pin("pa0", "pad"), pin("pa1", "pad1")] }));
    let (before, instants) = (sim.restarts(), sim.stats().write_instants);
    // the instants at t = 0 and ten more; the run stops between the tenth and the eleventh
    sim.advance(10.5 * 37e-6, None, &mut record).expect("the pair steps");
    let r = sim.restarts();
    assert_eq!(sim.stats().co_runs, 11, "the process runs at t = 0 and at ten 37 us instants");
    assert_eq!(sim.stats().write_instants - instants, 11, "each instant counts once, with both of its pins written");
    assert_eq!(r.change - before.change, 22, "one change per pin written: two at each of the 11 instants");
    assert_eq!(r.corner - before.corner, 11, "the two ramps of an instant end together: one corner each");
    assert_eq!((r.crossing - before.crossing, r.rescue - before.rescue), (0, 0), "no thresholds, no rescues");
    assert_eq!(r.total() - before.total(), 33, "the total is the causes' sum");
}

/// Sinks an LED into pin 0 for `low` at the start of each 128 us `period` (breathe's 7.8 kHz
/// timer PWM, written as pin writes), and with `rc` drives pin 1 into a 10 kOhm, 1 nF RC whose node pin 2
/// watches at half the supply, flipping it every 100 us. It writes for 39 periods (78 edges, and 50
/// for the RC), then rests. Logs each run's time and hits.
struct Blink {
    period: f64,
    low: f64,
    rc: bool,
    k: u64,
    j: u64,
    watches: Vec<Watch>,
    log: Log,
}

const RC_EVERY: f64 = 100e-6;
const LED_EDGES: u64 = 78;
const RC_EDGES: u64 = 50;

impl Blink {
    fn new(rc: bool, low: f64, log: Log) -> Blink {
        Blink { period: 128e-6, low, rc, k: 0, j: 0, watches: Vec::new(), log }
    }
    fn led_edge(&self) -> f64 {
        if self.k >= LED_EDGES {
            return f64::INFINITY;
        }
        (self.k / 2) as f64 * self.period + if self.k % 2 == 1 { self.low } else { 0.0 }
    }
    fn rc_edge(&self) -> f64 {
        if self.rc && self.j < RC_EDGES { self.j as f64 * RC_EVERY } else { f64::INFINITY }
    }
}

impl CoSim for Blink {
    fn next(&self) -> f64 {
        self.led_edge().min(self.rc_edge())
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        let t = bus.t();
        if t == self.led_edge() {
            assert!(bus.write(0, if self.k % 2 == 0 { PinMode::OutLow } else { PinMode::OutHigh }), "the LED pin is written");
            self.k += 1;
        }
        if t == self.rc_edge() {
            assert!(bus.write(1, if self.j % 2 == 0 { PinMode::OutHigh } else { PinMode::OutLow }), "the RC's pin is written");
            if self.j == 0 {
                self.watches.push(Watch { id: 1, pin: 2, level: 1.65, edge: PinEdge::Both });
            }
            self.j += 1;
        }
        self.log.lock().unwrap().push((t, t, bus.hits().to_vec()));
        Ok(CoRun::Go)
    }
    fn watches(&self) -> &[Watch] {
        &self.watches
    }
    fn watch_gen(&self) -> u32 {
        self.watches.len() as u32
    }
    fn arm(&mut self, _t: f64, _to: Option<StepTo>) -> bool {
        false
    }
    fn state(&mut self, _t: f64) -> Option<CoState> {
        None
    }
}

struct Blinked {
    record: Record,
    log: Vec<(f64, f64, Vec<(u32, f64)>)>,
    stats: fab_elec_sim::newton::Stats,
    restarts: fab_elec_sim::analysis::tran::Restarts,
    instants: u64,
}

/// `Blink` at time resolution `res`, on to 5.2 ms (past its last edge, at most 4.904 ms).
fn blinked(rc: bool, low: f64, res: f64) -> Blinked {
    blinked_warm(rc, low, res, true)
}

/// `blinked`, with warm starts on or off.
fn blinked_warm(rc: bool, low: f64, res: f64, warm: bool) -> Blinked {
    let mut c = pad_circuit(PinMode::OutHigh, 5e-12);
    // an LED whose own tails (2 pF, no stored charge) are far below the shortest pulse here
    c.model("led", fab_elec_sim::ModelKind::Diode, &[("is", 1e-18), ("n", 2.0), ("rs", 10.0), ("cjo", 2e-12)]);
    c.r("rl", "vdd", "a", 330.0);
    c.d("led", "a", "pad", "led");
    let mut pins = vec![pin("pa0", "pad")];
    if rc {
        c.gpio("pa1", "p1", "vdd", "0", PARAMS, PinMode::OutLow);
        c.r("rr", "p1", "w", 10e3);
        c.c("cw", "w", "0", 1e-9);
        c.gpio("pa2", "w", "vdd", "0", PARAMS, PinMode::Input(PinPull::None));
        pins.extend([pin("pa1", "p1"), pin("pa2", "w")]);
    }
    let (mut sim, mut record) = live(c, 1e-3);
    sim.set_res(res);
    sim.set_warm_start(warm);
    let log = Log::default();
    sim.attach(Some(CoSimDevice { dev: Box::new(Blink::new(rc, low, log.clone())), pins }));
    sim.advance(5.2e-3, None, &mut record).expect("the blinking LED steps");
    let log = log.lock().unwrap().clone();
    Blinked { record, log, stats: sim.solver().stats, restarts: sim.restarts(), instants: sim.stats().write_instants }
}

fn bits(v: &[f64]) -> Vec<u64> {
    v.iter().map(|x| x.to_bits()).collect()
}

/// A time resolution leaves an edge's nanosecond ramp unresolved: at a quarter of a 150 us pixel
/// column an LED's 7.8 kHz PWM takes at most 3 points per edge (Exact takes dozens), with every
/// edge on its exact instant and every settled level where Exact has it. A resolution below
/// Exact's own floor (`f64::MIN_POSITIVE`) is Exact, bit for bit: the same stepper. A watched RC
/// (tau 10 us, below the resolution) is still resolved: its threshold is crossed where Exact
/// crosses it, never rounded to the resolution, also when an edge comes a few us before the
/// process's next instant (a first step that lands there unchecked would miss it).
#[test]
fn a_time_resolution_takes_an_edge_in_a_few_points_and_keeps_instants_levels_and_hits() {
    let exact = blinked(false, 40e-6, 0.0);
    let tiny = blinked(false, 40e-6, f64::MIN_POSITIVE);
    assert!(bits(&tiny.record.at) == bits(&exact.record.at), "MIN_POSITIVE accepts Exact's points, bit for bit");
    assert!(tiny.record.x.iter().zip(&exact.record.x).all(|(a, b)| bits(a) == bits(b)), "and Exact's solutions, bit for bit");
    assert_eq!((tiny.stats, tiny.restarts, &tiny.log), (exact.stats, exact.restarts, &exact.log), "and Exact's stats, restarts and runs");

    let fast = blinked(false, 40e-6, 37.5e-6);
    let edges = exact.instants;
    assert_eq!((fast.instants, edges), (78, 78), "two edges per 128 us period, 39 periods");
    // accepted solves (an edge point is one more point per edge, not a solve)
    let per_edge = |b: &Blinked| (b.stats.steps - b.stats.rejected) as f64 / edges as f64;
    assert!(per_edge(&fast) <= 3.0, "at most 3 solves per edge at res 37.5 us: {:.2} (Exact {:.2})", per_edge(&fast), per_edge(&exact));
    assert_eq!(fast.log, exact.log, "the process runs at the same instants, bit for bit");
    for &(t, ..) in &exact.log[1..] {
        for node in ["a", "pad"] {
            let (e, f) = (exact.record.at_time(node, t), fast.record.at_time(node, t));
            assert!((f - e).abs() <= 1e-4 * e.abs() + 1e-6, "{node} settles where Exact has it before the edge at {t:e}: {f} versus {e}");
        }
    }

    let tau = (10e3 + PARAMS.r_on_h) * 1e-9;
    let hits = |b: &Blinked| b.log.iter().flat_map(|(_, _, h)| h.iter().map(|&(_, t)| t)).collect::<Vec<f64>>();
    let (exact_rc, fast_rc) = (hits(&blinked(true, 40e-6, 0.0)), hits(&blinked(true, 40e-6, 37.5e-6)));
    assert_eq!((fast_rc.len(), exact_rc.len()), (50, 50), "one hit per RC edge");
    for (i, (&f, &e)) in fast_rc.iter().zip(&exact_rc).enumerate() {
        let want = i as f64 * RC_EVERY + tau * 2.0_f64.ln();
        assert!((f - e).abs() < 1e-3 * tau, "hit {i} is where Exact finds it: {f:e} versus {e:e}");
        assert!((f - want).abs() < 1e-2 * tau, "hit {i} is at the RC's crossing: {f:e} versus {want:e}");
    }
}

/// A warm start, under a time resolution only: the first step after a pin write starts Newton from
/// the solution last converged under the same pins, so at res 37.5 us each of the LED PWM's
/// post-edge steps converges in at most 2 iterations (from the predictor its junction climbs for
/// several), with the process's instants bit for bit and every settled level within Newton's
/// tolerance (the fixture's reltol 1e-5, vntol 1 uV) of the cold run's.
#[test]
fn a_warm_start_takes_a_post_edge_step_in_two_iterations() {
    let cold = blinked_warm(false, 40e-6, 37.5e-6, false);
    let warm = blinked(false, 40e-6, 37.5e-6);
    let (w, c) = (&warm.stats, &cold.stats);
    let edges = warm.instants;
    eprintln!(
        "res 37.5 us, {edges} edges: {} warm starts, {} misses, {} iterations in them; iterations in all {} (cold {}), accepted solves {} (cold {})",
        w.warm_starts, w.warm_misses, w.warm_iterations, w.iterations, c.iterations, w.steps - w.rejected, c.steps - c.rejected
    );
    assert_eq!((c.warm_starts, c.warm_misses), (0, 0), "off, nothing starts warm");
    // the first write into each of the pin's patterns (low, then high) has nothing kept yet
    assert_eq!((w.warm_starts, w.warm_misses), (edges - 2, 0), "every later edge starts warm and converges from there");
    assert!(w.warm_iterations <= 2 * w.warm_starts, "at most 2 Newton iterations per warm post-edge step: {} over {}", w.warm_iterations, w.warm_starts);
    assert_eq!(warm.log, cold.log, "the process runs at the same instants, bit for bit");
    for &(t, ..) in &cold.log[1..] {
        for node in ["a", "pad"] {
            let (c, w) = (cold.record.at_time(node, t), warm.record.at_time(node, t));
            assert!((w - c).abs() <= 1e-5 * c.abs() + 1e-6, "{node} settles where the cold run has it before the edge at {t:e}: {w} versus {c}");
        }
    }
    let exact = blinked(false, 40e-6, 0.0);
    assert_eq!((exact.stats.warm_starts, exact.stats.warm_misses), (0, 0), "Exact never starts warm");
}

/// The values an unresolved first step reaches hold from just after its edge (an edge point at
/// the next float after the edge carries them): a 0.5 us pulse (duty 1/255 at 7.8 kHz) at res
/// 37.5 us is a 0.5 us pulse in the LED's mean current, as in Exact, not a ramp over the 37.5 us
/// step after it; the edge point is the only point between an edge and its step's.
#[test]
fn an_edge_point_keeps_a_short_pulse_in_the_mean_current() {
    let (exact, fast) = (blinked(false, 0.5e-6, 0.0), blinked(false, 0.5e-6, 37.5e-6));
    // from t = 0 to the last edge, where both runs have a point
    let end = exact.log[exact.log.len() - 1].0;
    let mean = |b: &Blinked| {
        let i = |k: usize| (b.record.x[k][b.record.index("vdd")] - b.record.x[k][b.record.index("a")]) / 330.0;
        let last = b.record.at.partition_point(|&a| a <= end);
        let charge: f64 = (1..last).map(|k| 0.5 * (i(k) + i(k - 1)) * (b.record.at[k] - b.record.at[k - 1])).sum();
        charge / end
    };
    let (e, f) = (mean(&exact), mean(&fast));
    assert!(e > 0.0 && (f / e - 1.0).abs() < 0.01, "the mean LED current is Exact's within 1 %: {f:e} A versus {e:e} A");
    for &(t, ..) in &fast.log[1..] {
        let k = fast.record.at.partition_point(|&a| a <= t);
        assert_eq!(fast.record.at[k].to_bits(), t.next_up().to_bits(), "an edge point follows the edge at {t:e}");
        assert_eq!(fast.record.x[k], fast.record.x[k + 1], "it carries its step's values");
    }
}

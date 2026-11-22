//! GPIO dividers, finite edges, PWM preload and pin-change timing.

use fab_elec_sim::live::{LiveCmd, ValuePlan};
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::trace::{SignalId, TraceStore};
use fab_elec_sim::{GpioParams, PinEdge, PinMode, PinPull, Record, Retuned, SimCircuit, Sink, Solver, Source};

const PARAMS: GpioParams = GpioParams { r_on_h: 25.0, r_on_l: 20.0, r_pull: 40e3, r_leak: 1e12, t_edge: 5e-9, vih: 0.55, vil: 0.45 };

fn circuit(mode: PinMode, load: bool, cap: f64) -> SimCircuit {
    circuit_with_params(mode, load, cap, PARAMS)
}

fn circuit_with_params(mode: PinMode, load: bool, cap: f64, params: GpioParams) -> SimCircuit {
    let mut c = SimCircuit::new();
    c.v("supply", "vdd", "0", Source::dc(3.3));
    c.gpio("pin", "pad", "vdd", "0", params, mode);
    if load {
        c.r("load", "pad", "0", 10e3);
    }
    if cap > 0.0 {
        c.c("cap", "pad", "0", cap);
    }
    // The small capacitors need charge tolerance below their ramp charge change.
    c.opts.chgtol = 1e-20;
    c.opts.vntol = 1e-9;
    c.opts.abstol = 1e-15;
    c
}

fn live(c: SimCircuit, max_step: f64) -> (LiveSim, Record) {
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, reltol: Some(1e-5), max_step, trace: false, ..LiveSpec::default() };
    let sim = LiveSim::new(c, ValuePlan::default(), 1, spec).expect("GPIO operating point converges");
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    record.point(sim.t(), None, &sim.solver().x);
    (sim, record)
}

fn step(sim: &mut LiveSim, record: &mut Record, dt: f64) {
    assert!(sim.apply(LiveCmd::Step(dt)), "GPIO Step keeps the worker alive");
    while sim.running() {
        let before = sim.t();
        sim.advance(f64::INFINITY, None, record).expect("GPIO transient step converges");
        assert!(sim.t() > before, "GPIO Step advances simulated time");
        sim.values(1.0);
    }
}

fn at(record: &Record, t: f64) -> f64 {
    assert!(record.at.first().is_some_and(|&start| start <= t), "GPIO record starts before sample {t:e}");
    assert!(record.at.last().is_some_and(|&end| end >= t), "GPIO record reaches sample {t:e}");
    record.at_time("pad", t)
}

fn cross(record: &Record, node: &str, level: f64, after_t: f64, rising: bool) -> f64 {
    let i = record.index(node);
    for (ts, xs) in record.at.windows(2).zip(record.x.windows(2)) {
        let (v0, v1) = (xs[0][i], xs[1][i]);
        let crosses = if rising { v0 < level && v1 >= level } else { v0 > level && v1 <= level };
        if crosses {
            let t = ts[0] + (level - v0) / (v1 - v0) * (ts[1] - ts[0]);
            if t >= after_t {
                return t;
            }
        }
    }
    panic!("GPIO {node} never crosses {level:e} after {after_t:e} (rising={rising})");
}

#[test]
fn gpio_pin_modes_edges_pwm_and_watches() {
    let gh = 1.0 / PARAMS.r_on_h;
    let gl = 1.0 / PARAMS.r_on_l;
    let gp = 1.0 / PARAMS.r_pull;
    let shunt = 1.0 / 10e3 + 1.0 / PARAMS.r_leak;
    for (mode, want) in [
        (PinMode::OutHigh, 3.3 * gh / (gh + shunt)),
        (PinMode::OutLow, 0.0),
        (PinMode::Input(PinPull::Up), 3.3 * gp / (gp + shunt)),
        (PinMode::Input(PinPull::Down), 0.0),
        (PinMode::Input(PinPull::None), 0.0),
        (PinMode::OpenDrain { low: true, pull: PinPull::Up }, 3.3 * gp / (gp + gl + shunt)),
    ] {
        let (sim, record) = live(circuit(mode, true, 0.0), 1e-9);
        let got = at(&record, 0.0);
        assert!((got - want).abs() < 1e-6, "GPIO {mode:?} DC divider: {got} versus {want}");
        let s = sim.solver();
        let (b, i) = s.find_device("pin").expect("GPIO is found by device name");
        assert_eq!(s.banks[b].terminals(), 3, "GPIO exposes pad, vdd and vss terminals");
        let mut currents = [0.0; 3];
        s.banks[b].terminal_currents(i, &s.x, &[], &s.base_cx(), &mut currents);
        assert!(currents.iter().sum::<f64>().abs() < 1e-14, "GPIO {mode:?} terminal currents conserve charge");
        assert!((currents[0] + got / 10e3).abs() < 1e-12, "GPIO {mode:?} pad current balances its load");
    }

    // Twenty samples across the write ramp and tight LTE bounds resolve its quadratic exponent.
    let cap = 1e-9;
    let (mut sim, mut record) = live(circuit(PinMode::Input(PinPull::None), false, cap), PARAMS.t_edge / 20.0);
    step(&mut sim, &mut record, 1e-9);
    let dev = sim.solver().find_device("pin").expect("edge GPIO has a handle");
    let tw = sim.t();
    assert!(at(&record, tw).abs() < 1e-9, "floating GPIO starts discharged");
    assert!(sim.pin_write(dev, PinMode::OutHigh), "output-high GPIO write is accepted");
    step(&mut sim, &mut record, 70e-9);
    for tau in [1e-9, 2.5e-9, 5e-9, 10e-9, 25e-9, 60e-9] {
        let exponent = if tau <= PARAMS.t_edge {
            gh * tau * tau / (2.0 * cap * PARAMS.t_edge)
        } else {
            gh * (tau - PARAMS.t_edge / 2.0) / cap
        };
        let want = 3.3 * (1.0 - (-exponent).exp());
        let got = at(&record, tw + tau);
        assert!((got - want).abs() < 0.01 * 3.3, "GPIO finite-edge RC response at tau={tau:e}: {got} versus {want}");
    }
    assert!(record.at.iter().any(|&t| (t - tw - PARAMS.t_edge).abs() < 1e-18), "GPIO write-ramp end is a recorded breakpoint");

    {
        let params = GpioParams { r_on_h: 40.0, r_on_l: 10.0, ..PARAMS };
        let max_step = params.t_edge / 50.0;
        // Each side's conductance ramps on its own, so the open-circuit level is
        // s rl / (s rl + (1 - s) rh) on a rising edge: it reaches y at s = y rh / ((1 - y) rl + y rh),
        // and a falling edge mirrors it. With rh = 4 rl both 10-90 % times are 0.665 t_edge (0.8 t_edge
        // only when rh == rl). The 1 pF pad's 13-37 ps RC lag costs at most 0.5 %.
        let (rh, rl) = (params.r_on_h, params.r_on_l);
        let s_at = |y: f64| y * rh / ((1.0 - y) * rl + y * rh);
        let want = (s_at(0.9) - s_at(0.1)) * params.t_edge;
        let c = circuit_with_params(PinMode::OutLow, false, 1e-12, params);
        let (mut sim, mut record) = live(c, max_step);
        step(&mut sim, &mut record, 10e-9);
        let dev = sim.solver().find_device("pin").expect("unequal-drive GPIO has a handle");
        let rise_at = sim.t();
        assert!(sim.pin_write(dev, PinMode::OutHigh), "unequal-drive GPIO high write is accepted");
        step(&mut sim, &mut record, 30e-9);
        let fall_at = sim.t();
        assert!(sim.pin_write(dev, PinMode::OutLow), "unequal-drive GPIO low write is accepted");
        step(&mut sim, &mut record, 30e-9);
        let rise = cross(&record, "pad", 0.9 * 3.3, rise_at, true) - cross(&record, "pad", 0.1 * 3.3, rise_at, true);
        let fall = cross(&record, "pad", 0.1 * 3.3, fall_at, false) - cross(&record, "pad", 0.9 * 3.3, fall_at, false);
        for (edge, got) in [("write rising 10-90%", rise), ("write falling 90-10%", fall)] {
            assert!((got - want).abs() <= 0.01 * params.t_edge, "GPIO {edge} time: {got:e} versus {want:e}");
        }

        let period = 1e-6;
        let high = 0.3e-6;
        let c = circuit_with_params(PinMode::Pwm { period, high, origin: 0.0 }, false, 1e-12, params);
        let (mut sim, mut record) = live(c, max_step);
        step(&mut sim, &mut record, 1.1e-6);
        // Measure both edges of period zero.
        let rise = cross(&record, "pad", 0.9 * 3.3, 0.0, true) - cross(&record, "pad", 0.1 * 3.3, 0.0, true);
        let fall = cross(&record, "pad", 0.1 * 3.3, high, false) - cross(&record, "pad", 0.9 * 3.3, high, false);
        for (edge, got) in [("PWM rising 10-90%", rise), ("PWM falling 90-10%", fall)] {
            assert!((got - want).abs() <= 0.01 * params.t_edge, "GPIO {edge} time: {got:e} versus {want:e}");
        }
    }

    let period = 1e-6;
    let old_high = 0.3e-6;
    let new_high = 0.6e-6;
    let c = circuit(PinMode::Pwm { period, high: old_high, origin: 0.0 }, true, 10e-12);
    let mut preload = Solver::new(&c).expect("GPIO preload solver sets up");
    let preload_dev = preload.find_device("pin").expect("preload GPIO has a handle");
    // The grid corners resolve each 5 ns ramp; 20 ns bounds interpolation in the flat phases.
    let (mut sim, mut record) = live(c, 20e-9);
    step(&mut sim, &mut record, period + 0.15e-6);
    let tw = sim.t();
    assert!(tw >= period + 0.15e-6 && tw < period + 0.25e-6, "GPIO PWM preload is written inside the old high phase");
    let dev = sim.solver().find_device("pin").expect("PWM GPIO has a handle");
    for high in [0.4e-6, new_high] {
        let mode = PinMode::Pwm { period, high, origin: 123.0 };
        assert_eq!(preload.pin_write(preload_dev, tw, mode), Some(Retuned::At(2.0 * period)), "GPIO PWM writes preload on the same next running period start");
        assert!(sim.pin_write(dev, mode), "GPIO PWM preload write is accepted");
        assert_eq!(sim.t(), tw, "GPIO preload preserves simulated time");
    }
    let remaining = 3.9e-6 - sim.t();
    step(&mut sim, &mut record, remaining);
    assert!(at(&record, period + 0.29e-6) > 3.0, "GPIO preload retains the current period's high phase");
    assert!(at(&record, period + 0.32e-6) < 0.03, "GPIO preload retains the current period's 0.3 us falling edge");
    assert!(at(&record, 2.0 * period + 0.45e-6) > 3.0, "GPIO next period uses the last preloaded duty");
    for k in 0..=3 {
        let start = f64::from(k) * period;
        let high = if k < 2 { old_high } else { new_high };
        for corner in [0.0, PARAMS.t_edge, high, high + PARAMS.t_edge] {
            let t = start + corner;
            assert!(record.at.iter().any(|&point| (point - t).abs() < 1e-18), "GPIO PWM period {k} records grid corner {corner:e} at {t:e}");
        }
        assert!(at(&record, start + high / 2.0) > 3.0, "GPIO PWM period {k} is high at mid-high");
        assert!(at(&record, start + (high + period) / 2.0) < 0.03, "GPIO PWM period {k} is low at mid-low");
        assert!(at(&record, start + PARAMS.t_edge + 2e-9) > 3.0, "GPIO PWM period {k} rises on the original grid");
        assert!(at(&record, start + high - PARAMS.t_edge) > 3.0, "GPIO PWM period {k} stays high until its falling edge");
        assert!(at(&record, start + high + 2.0 * PARAMS.t_edge) < 0.03, "GPIO PWM period {k} falls at the requested duty");
    }
    assert!(record.at.windows(2).all(|ts| ts[0] < ts[1]), "GPIO PWM points advance strictly across preload switches");

    let cap = 10e-12;
    let rc = PARAMS.r_pull * cap;
    // Tight LTE bounds and a step below RC/80 keep interpolation error below the watch bound.
    let (mut sim, mut record) = live(circuit(PinMode::OutLow, false, cap), 4e-9);
    step(&mut sim, &mut record, 10e-9);
    let dev = sim.solver().find_device("pin").expect("watched GPIO has a handle");
    assert!(sim.pin_watch(dev, 7, 1.65, PinEdge::Rising), "GPIO rising watch registers");
    assert!(sim.pin_watch(dev, 8, 1.65, PinEdge::Falling), "GPIO falling watch registers");
    let tw = sim.t();
    assert!(sim.pin_write(dev, PinMode::Input(PinPull::Up)), "GPIO pull-up release write is accepted");
    step(&mut sim, &mut record, 3.0 * rc);
    let mut hits = Vec::new();
    sim.pin_hits(&mut hits);
    assert_eq!(hits.len(), 1, "GPIO pull-up release fires exactly one watch");
    let (id, hit) = hits[0];
    assert_eq!(id, 7, "GPIO pull-up release fires only the rising watch");
    let want = tw + rc * 2.0_f64.ln();
    assert!((hit - want).abs() < 0.02 * rc, "GPIO pull-up watch time follows RC ln(2), including the short write ramp: {hit:e} versus {want:e}");
    assert!(record.at.iter().any(|&t| (t - hit).abs() < 0.002 * rc), "GPIO watch crossing cuts the step at its interpolated hit time");
    assert!((at(&record, hit) - 1.65).abs() < 1e-6, "GPIO watch interpolation lands at its voltage level");
    sim.pin_hits(&mut hits);
    assert_eq!(hits.len(), 1, "GPIO hit drain appends no duplicate events");
    sim.clear_watches();
    assert!(sim.pin_write(dev, PinMode::OutLow), "GPIO output-low write after clearing watches is accepted");
    step(&mut sim, &mut record, 20e-9);
    sim.pin_hits(&mut hits);
    assert_eq!(hits.len(), 1, "cleared GPIO falling watch emits no hit");
}

/// A `Replace` carries each GPIO's running mode (a co-simulated process may have written it),
/// except where the new lowering changed the pin's mode: that is the bench's new stimulus, written
/// as a `PinMode` command would be.
#[test]
fn replace_takes_a_changed_pin_mode_and_keeps_an_unchanged_one() {
    let two = |a: PinMode, b: PinMode| {
        let mut c = circuit(a, true, 0.0);
        c.gpio("pin2", "pad2", "vdd", "0", PARAMS, b);
        c.r("load2", "pad2", "0", 10e3);
        c
    };
    let high = 3.3 * 10e3 / (10e3 + PARAMS.r_on_h);
    let (mut sim, mut record) = live(two(PinMode::OutLow, PinMode::OutLow), 1e-8);
    step(&mut sim, &mut record, 20e-9);
    let pin = sim.solver().find_device("pin").expect("GPIO pin is a device");
    assert!(sim.pin_write(pin, PinMode::OutHigh), "GPIO firmware write is accepted");
    step(&mut sim, &mut record, 50e-9);
    assert!(sim.apply(LiveCmd::Replace {
        generation: 2,
        circuit: Box::new(two(PinMode::OutLow, PinMode::OutHigh)),
        plan: Box::new(ValuePlan::default()),
    }));
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    record.point(sim.t(), None, &sim.solver().x);
    let t = sim.t();
    step(&mut sim, &mut record, 50e-9);
    let end = t + 50e-9;
    let (pad, pad2) = (record.at_time("pad", end), record.at_time("pad2", end));
    assert!((pad - high).abs() < 1e-3 * high,
        "GPIO pin keeps its written high across a Replace that lowers it unchanged: {pad} V, expected {high} V");
    assert!((pad2 - high).abs() < 1e-3 * high,
        "GPIO pin2 takes the high its new lowering gives it: {pad2} V, expected {high} V");
    assert_eq!(sim.stats().replaced, 1, "GPIO Replace is applied once");
}

/// A net a pad reads is traced as logic: its edges at the input's thresholds, at their crossing
/// times, for the whole run, while the samples keep only the record's last stretch.
#[test]
fn a_pad_net_is_traced_as_logic_edges_past_the_record() {
    let (period, high, periods) = (1e-6, 0.4e-6, 400);
    let c = circuit(PinMode::Pwm { period, high, origin: 0.0 }, true, 10e-12);
    let plan = ValuePlan { nets: vec!["vdd".into(), "pad".into()], ..ValuePlan::default() };
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, reltol: Some(1e-5), max_step: period / 16.0, trace: true, record: 5.0 * period, ..LiveSpec::default() };
    let mut sim = LiveSim::new(c, plan, 1, spec).expect("the PWM pad's operating point converges");
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    record.point(sim.t(), None, &sim.solver().x);
    let end = periods as f64 * period;
    step(&mut sim, &mut record, end);

    let store = sim.store();
    assert!(store.logic(SignalId(0)).is_none(), "no pad reads the supply: it has no logic trace");
    let logic = store.logic(SignalId(1)).expect("the pad's net is traced as logic");
    assert!(store.time.at(0) > 0.25 * end, "the samples keep the record's last stretch, from {:e}", store.time.at(0));
    let entry = |i: usize| logic.get(i).expect("an entry in memory");
    assert!(entry(0).t < PARAMS.t_edge && !entry(0).high, "the logic starts with the run, low before the first edge's ramp");
    let (vdd, vih, vil) = (3.3, PARAMS.vih, PARAMS.vil);
    let (mut rising, mut falling) = (0, 0);
    for i in 1..logic.len() {
        let edge = entry(i);
        assert_ne!(edge.high, entry(i - 1).high, "the levels alternate at entry {i}");
        let (nominal, level) = if edge.high { ((edge.t / period).round() * period, vih) } else { ((edge.t / period).floor() * period + high, vil) };
        assert!(edge.t >= nominal && edge.t <= nominal + 2.0 * PARAMS.t_edge, "edge {i} at {:e} follows its PWM edge at {nominal:e}", edge.t);
        let crossing = cross(&record, "pad", level * vdd, edge.t - PARAMS.t_edge, edge.high);
        assert!((crossing - edge.t).abs() <= 1e-15 + 1e-9 * edge.t, "edge {i} at {:e} is the pad's crossing of its threshold at {crossing:e}", edge.t);
        if edge.high { rising += 1 } else { falling += 1 }
    }
    assert_eq!((rising, falling), (periods, periods), "every PWM edge of the run is kept");
    for k in 0..periods {
        let t = k as f64 * period;
        assert_eq!((logic.level_at(t + 0.5 * high), logic.level_at(t + 0.5 * (high + period))), (Some(true), Some(false)), "period {k} reads high, then low");
    }

    let mut consumer = TraceStore::default();
    consumer.apply(&sim.take_delta().expect("the run's delta"));
    consumer.keep_last(5.0 * period);
    assert_eq!(consumer.logic, sim.store().logic, "the delta carries the logic to a consumer");
}

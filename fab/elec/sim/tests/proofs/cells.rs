//! Timing cells (Logic mode) on everyday circuits: a clock under a load, a PWM generator whose duty
//! a control voltage sets, a button's debounce and an RC delay between two sections. The
//! recogniser takes them all, and its calibration refuses a section whose soft band no delay a
//! flip can match; a taken cell keeps its switch pair's period and duty, or its edges, with its
//! capacitor halved and doubled; a driven cell follows a driver that flips at a step's start with
//! no step's lag; and a run handed between the lowerings mid-period (`analysis::handover`)
//! carries the cell's state both ways and back to the same bits, with no step at the boundary and
//! the period going on. A hand-over gives what only the new circuit has its DC value with every
//! carried voltage held. A pot in series with the timing resistor is one resistor to the cell, and
//! turning it retimes the cell as it retimes the pair, the input node reading the cell's value, the
//! chain's inner node standing where the chain's current puts it and each absorbed part reading the
//! current it carries.

use fab_elec_sim::analysis::logic::with_cells;
use fab_elec_sim::live::{LiveCmd, ValuePlan};
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::stamp::Given;
use fab_elec_sim::{run, Analysis, Control, ModelKind, Record, SimCircuit, Sink, Source, TranSpec, Wave};

/// A 5 V supply and the Schmitt section's switch models: thresholds 2.0 V and 3.0 V, 50 ohm on.
fn board() -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("lo", ModelKind::Sw, &[("vt", 2.5), ("vh", 0.5), ("vband", 1e-4), ("ron", 50.0), ("roff", 1e9)]);
    c.model("hi", ModelKind::Sw, &[("vt", -2.5), ("vh", 0.5), ("vband", 1e-4), ("ron", 50.0), ("roff", 1e9)]);
    c.v("v5", "vdd", "0", Source::dc(5.0));
    c.declare_supply("vdd");
    c
}

/// A Schmitt inverter section from `input` to `out`: a mirrored switch pair on the supply.
fn section(c: &mut SimCircuit, name: &str, input: &str, out: &str) {
    section_of(c, name, input, out, ("lo", "hi"));
}

fn section_of(c: &mut SimCircuit, name: &str, input: &str, out: &str, (lo, hi): (&str, &str)) {
    c.sw(&format!("{name}lo"), out, "0", input, "0", lo, false);
    c.sw(&format!("{name}hi"), "vdd", out, "0", input, hi, true);
    c.c(&format!("{name}co"), out, "0", 10e-12);
}

/// A clock: 100 kohm from the section's output back to its input, `ct` to ground, the output
/// into a 10 kohm load. About 2 * 100k * ct * ln(3/2) a period.
fn clock(ct: f64) -> SimCircuit {
    let mut c = board();
    section(&mut c, "u1", "i", "clk");
    c.r("rf", "clk", "i", 100e3);
    c.c("ct", "i", "0", ct);
    c.r("rl", "clk", "0", 10e3);
    c.set_ic("i", 0.0);
    c
}

/// A PWM generator: the oscillator with a second 100 kohm from its input to a control voltage
/// `vc` (a reference supply). The input heads for (5 + vc) / 2 while the output is high and for
/// vc / 2 while it is low, so vc sets the duty: about 73 % at 1.5 V and 27 % at 3.5 V, the
/// period 2.2 * 50k * ct either way.
fn pwm(ct: f64, vc: f64) -> SimCircuit {
    let mut c = board();
    c.v("vref", "vc", "0", Source::dc(vc));
    c.declare_supply("vc");
    section(&mut c, "u1", "i", "pwm");
    c.r("rf", "pwm", "i", 100e3);
    c.r("rc", "vc", "i", 100e3);
    c.c("ct", "i", "0", ct);
    c.set_ic("i", 0.0);
    c
}

/// A button's debounce: a 10 kohm pull-up, the button to ground (closed while `press` is high:
/// 20 ms from 1 ms, every 40 ms), 100 kohm into the section's input with `cd` to ground.
fn debounce(cd: f64) -> SimCircuit {
    let mut c = board();
    c.model("btn", ModelKind::Sw, &[("vt", 0.5), ("vh", 0.1), ("ron", 1.0), ("roff", 1e9)]);
    c.v("vpress", "press", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 1.0, td: 1e-3, tr: 1e-6, tf: 1e-6, pw: 20e-3, per: 40e-3 }));
    c.r("rpull", "vdd", "b", 10e3);
    c.sw("button", "b", "0", "press", "0", "btn", false);
    c.r("rd", "b", "i", 100e3);
    c.c("cd", "i", "0", cd);
    section(&mut c, "u1", "i", "clean");
    c
}

/// The clock (10 nF) through an RC delay (33 kohm, `cdl`) into a second section: a late, clean
/// copy.
fn delayed(cdl: f64) -> SimCircuit {
    let mut c = clock(10e-9);
    c.r("rdl", "clk", "d", 33e3);
    c.c("cdl", "d", "0", cdl);
    section(&mut c, "u2", "d", "late");
    c
}

/// The delay into a soft section: its band (1 V) wider than twice its hysteresis (0.1 V), so its
/// switches move partway through the band before they commit, where a cell flips. The pair's
/// output turns at 2.1 V rising and 1.9 V falling, the cell's at 2.4 V and 1.6 V, and the input
/// crosses those 0.3 V at different slopes rising and falling: no delay a flip makes the cell
/// agree.
fn soft_delay() -> SimCircuit {
    let mut c = clock(10e-9);
    c.model("softlo", ModelKind::Sw, &[("vt", 2.0), ("vh", 0.1), ("vband", 1.0), ("ron", 50.0), ("roff", 1e9)]);
    c.model("softhi", ModelKind::Sw, &[("vt", -2.0), ("vh", 0.1), ("vband", 1.0), ("ron", 50.0), ("roff", 1e9)]);
    c.r("rdl", "clk", "d", 33e3);
    c.c("cdl", "d", "0", 10e-9);
    section_of(&mut c, "u2", "d", "late", ("softlo", "softhi"));
    c
}

/// The rising and falling crossings of 2.5 V, each at the point where the step that crossed
/// started (where a switch commits, or a cell flips): an output that jumps is not interpolated
/// over the step after it.
fn edges(at: &[f64], v: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let ups = (1..at.len()).filter(|&k| v[k - 1] < 2.5 && v[k] >= 2.5).map(|k| at[k - 1]).collect();
    let downs = (1..at.len()).filter(|&k| v[k - 1] >= 2.5 && v[k] < 2.5).map(|k| at[k - 1]).collect();
    (ups, downs)
}

/// An oscillator's period and duty (the output's high share) over a dozen periods of about
/// `about`, from its second rising edge, at a tolerance of 1e-5: the difference measured is the
/// cell's, not the stepper's.
fn period_and_duty(c: &SimCircuit, out: &str, about: f64) -> (f64, f64) {
    let mut c = c.clone();
    c.opts.reltol = 1e-5;
    let tran = Analysis::Tran(TranSpec { tstop: 12.0 * about, max_step: Some(about / 200.0), uic: false });
    let mut record = Record::default();
    run(&c, &tran, &mut record, &Control::default()).unwrap_or_else(|d| panic!("the oscillator runs: {d}"));
    let (ups, downs) = edges(&record.at, &record.v(out));
    assert!(ups.len() >= 10, "{} rising edges at {ups:?}", ups.len());
    let ups = &ups[1..];
    let period = (ups[ups.len() - 1] - ups[0]) / (ups.len() - 1) as f64;
    let highs: Vec<f64> = ups[..ups.len() - 1].iter().map(|&u| downs.iter().find(|&&d| d > u).expect("a falling edge after each rising one") - u).collect();
    (period, highs.iter().sum::<f64>() / highs.len() as f64 / period)
}

#[test]
fn the_recogniser_takes_the_clock_the_pwm_the_debounce_and_the_delay_and_refuses_a_soft_section() {
    for (name, c, want) in [
        ("the clock", clock(10e-9), vec!["u1lo.cell"]),
        ("the PWM generator", pwm(10e-9, 1.5), vec!["u1lo.cell"]),
        ("the debounce", debounce(100e-9), vec!["u1lo.cell"]),
        ("the delay", delayed(10e-9), vec!["u1lo.cell", "u2lo.cell"]),
    ] {
        let (cells, taken, analog) = with_cells(&c);
        assert_eq!(taken.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(), want, "{name}: its sections and their inputs are cells (left analog: {analog:?})");
        assert!(analog.is_empty(), "{name}: nothing is left analog: {analog:?}");
        for k in &taken {
            println!("{name}: {} calibrated: the pair {:e} s, the cell {:e} s, delay {:e} s a flip", k.name, k.period, k.cell_period, k.delay);
            assert!(k.delay.abs() < 1e-4 * k.period, "{name}: {} needs a delay of {:e} s a flip", k.name, k.delay);
            // the input stays a node, the cell's alone: it sets it to its form's value
            let n = cells.find_node(&k.input).unwrap_or_else(|| panic!("{name}: {}'s input is a node", k.name));
            let mut on = 0;
            cells.devices.for_each_node(|m| on += usize::from(m == n));
            assert_eq!(on, 1, "{name}: {}'s input has its cell on it and nothing else", k.name);
        }
        assert!(cells.devices.sw.iter().all(|s| !s.name.starts_with('u')), "{name}: the sections' switches leave the solve");
    }
    // the soft section's calibration must refuse it: the clock is still a cell
    let (_, taken, analog) = with_cells(&soft_delay());
    assert_eq!(taken.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(), vec!["u1lo.cell"], "the clock is a cell, the soft section is not");
    assert_eq!(analog.len(), 1, "the soft section stays analog: {analog:?}");
    assert!(analog[0].0 == "d" && analog[0].1.starts_with("its calibration: its flips are up to"), "the soft section says why: {analog:?}");
}

#[test]
fn a_clock_and_a_pwm_generator_keep_their_pairs_period_and_duty() {
    let circuits: [(&str, &str, fn(f64) -> SimCircuit, f64); 3] = [
        ("the clock", "clk", clock, 2.0 * 100e3 * 1.5f64.ln()),
        ("the PWM generator at 1.5 V", "pwm", |ct| pwm(ct, 1.5), 2.2 * 50e3),
        ("the PWM generator at 3.5 V", "pwm", |ct| pwm(ct, 3.5), 2.2 * 50e3),
    ];
    for (name, out, build, per_farad) in circuits {
        let mut periods = Vec::new();
        for ct in [10e-9, 5e-9, 20e-9] {
            let c = build(ct);
            let (cells, taken, analog) = with_cells(&c);
            assert_eq!(taken.len(), 1, "{name}, ct {ct:e} F: one cell (left analog: {analog:?})");
            let about = per_farad * ct;
            assert!(taken[0].delay.abs() < 1e-4 * taken[0].period, "{name}, ct {ct:e} F: the closed form needs a delay of {:e} s a flip", taken[0].delay);
            let (pair, cell) = (period_and_duty(&c, out, about), period_and_duty(&cells, out, about));
            println!("{name}, ct {ct:e} F: pair {:e} s at {:.4} high, cell {:e} s at {:.4} high", pair.0, pair.1, cell.0, cell.1);
            assert!((cell.0 / pair.0 - 1.0).abs() < 1e-3, "{name}, ct {ct:e} F: the cell's period {:e} s against the pair's {:e} s", cell.0, pair.0);
            assert!((cell.1 - pair.1).abs() < 1e-3, "{name}, ct {ct:e} F: the cell's duty {} against the pair's {}", cell.1, pair.1);
            periods.push((pair.0, cell.0));
        }
        // a change of the right size: the period follows the capacitor
        for (k, scale) in [(1, 0.5), (2, 2.0)] {
            for (which, base, scaled) in [("pair", periods[0].0, periods[k].0), ("cell", periods[0].1, periods[k].1)] {
                assert!((scaled / base / scale - 1.0).abs() < 1e-3, "{name}: the {which}'s period scales {} with the capacitor at x{scale}", scaled / base);
            }
        }
    }
}

/// An output's edges in a run of `span` at a tolerance of 1e-5.
fn edge_times(c: &SimCircuit, out: &str, span: f64, max_step: f64) -> Vec<f64> {
    let mut c = c.clone();
    c.opts.reltol = 1e-5;
    let tran = Analysis::Tran(TranSpec { tstop: span, max_step: Some(max_step), uic: false });
    let mut record = Record::default();
    run(&c, &tran, &mut record, &Control::default()).unwrap_or_else(|d| panic!("the circuit runs: {d}"));
    let (mut ups, downs) = edges(&record.at, &record.v(out));
    ups.extend(downs);
    ups.sort_by(f64::total_cmp);
    ups
}

#[test]
fn a_debounce_and_a_delay_keep_their_pairs_edges() {
    let mut first = Vec::new();
    for scale in [1.0, 0.5, 2.0] {
        // the debounce: four presses
        let c = debounce(100e-9 * scale);
        let tau = 100e3 * 100e-9 * scale;
        let (cells, taken, analog) = with_cells(&c);
        assert_eq!(taken.len(), 1, "the debounce at x{scale}: one cell (left analog: {analog:?})");
        let (pair, cell) = (edge_times(&c, "clean", 160e-3, tau / 200.0), edge_times(&cells, "clean", 160e-3, tau / 200.0));
        println!("the debounce at x{scale}: the pair's edges {pair:?}, the cell's {cell:?}");
        assert!(pair.len() >= 6 && cell.len() == pair.len(), "the debounce at x{scale}: the cell's edges {cell:?}, the pair's {pair:?}");
        for (q, p) in cell.iter().zip(&pair) {
            assert!((q - p).abs() < 1e-3 * tau, "the debounce at x{scale}: an edge at {q:e} s, the pair's at {p:e} s");
        }
        // the first press finds the input settled: its response scales with the capacitor
        first.push((pair[0] - 1e-3, cell[0] - 1e-3));
        // the delay: the clock's edges, late
        let c = delayed(10e-9 * scale);
        let period = 0.815e-3;
        let (cells, taken, analog) = with_cells(&c);
        assert_eq!(taken.len(), 2, "the delay at x{scale}: two cells (left analog: {analog:?})");
        let (pair, cell) = (edge_times(&c, "late", 10.0 * period, period / 500.0), edge_times(&cells, "late", 10.0 * period, period / 500.0));
        assert!(pair.len() >= 12 && cell.len() == pair.len(), "the delay at x{scale}: the cell's edges {cell:?}, the pair's {pair:?}");
        for (q, p) in cell.iter().zip(&pair) {
            assert!((q - p).abs() < 1e-3 * period, "the delay at x{scale}: an edge at {q:e} s, the pair's at {p:e} s");
        }
    }
    for (k, scale) in [(1, 0.5), (2, 2.0)] {
        for (which, base, scaled) in [("pair", first[0].0, first[k].0), ("cell", first[0].1, first[k].1)] {
            assert!((scaled / base / scale - 1.0).abs() < 1e-3, "the debounce's {which}: its first response scales {} with the capacitor at x{scale}", scaled / base);
        }
    }
}

#[test]
fn a_driven_cell_follows_a_driver_that_flips_with_no_steps_lag() {
    // the delay at Fast's numerics with steps of a twentieth of the clock's period: the clock's
    // flips are corners, and the delay's input heads for the new level from the step that starts
    // there, as the pair's does (a step late, its edges would come 40 us late)
    let period = 0.815e-3;
    let pair = edge_times(&delayed(10e-9), "late", 10.0 * period, period / 500.0);
    let (cells, _, _) = with_cells(&delayed(10e-9));
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: period / 20.0, res: period / 20.0, reltol: Some(1e-2), ..LiveSpec::default() };
    let mut sim = LiveSim::new(cells, ValuePlan::default(), 1, spec).unwrap_or_else(|d| panic!("the operating point: {d}"));
    let mut record = Record::default();
    run_to(&mut sim, &mut record, 10.0 * period);
    let (mut fast, downs) = edges(&record.at, &record.v("late"));
    fast.extend(downs);
    fast.sort_by(f64::total_cmp);
    assert!(pair.len() >= 12 && fast.len() == pair.len(), "the cells' edges at Fast {fast:?}, the pair's {pair:?}");
    for (q, p) in fast.iter().zip(&pair) {
        assert!((q - p).abs() < 2e-3 * period, "an edge at {q:e} s at Fast, the pair's at {p:e} s");
    }
}

/// A run at Fast's numerics (a tolerance of 1e-2 and a resolution of a 400th of the period).
fn live(c: &SimCircuit, period: f64) -> LiveSim {
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: period / 200.0, res: period / 400.0, reltol: Some(1e-2), ..LiveSpec::default() };
    LiveSim::new(c.clone(), ValuePlan::default(), 1, spec).unwrap_or_else(|d| panic!("the operating point: {d}"))
}

fn run_to(sim: &mut LiveSim, record: &mut Record, t: f64) {
    record.begin(&sim.solver().unknowns);
    assert!(sim.apply(LiveCmd::Step(t - sim.t())), "the Step is taken");
    while sim.running() {
        sim.advance(f64::INFINITY, None, record).unwrap_or_else(|d| panic!("the run steps: {d}"));
    }
}

/// Hands the run to `c`'s lowering.
fn replace(sim: &mut LiveSim, c: &SimCircuit) {
    let generation = sim.generation() + 1;
    assert!(sim.apply(LiveCmd::Replace { generation, circuit: Box::new(c.clone()), plan: Box::new(ValuePlan::default()) }), "the Replace is taken");
    assert_eq!(sim.generation(), generation, "the Replace set up: {:?}", sim.stats().replace_error);
    let h = sim.stats().handover.clone().expect("the Replace handed the run over");
    assert!(h.fill_failed.is_none() && h.unadopted.is_empty(), "the hand-over: {h:?}");
}

fn state(sim: &LiveSim, device: &str) -> Vec<f64> {
    let s = sim.solver();
    let (b, i) = s.find_device(device).unwrap_or_else(|| panic!("no device {device}"));
    s.banks[b].state(i)
}

fn x(sim: &LiveSim, unknown: &str) -> f64 {
    let s = sim.solver();
    s.x[s.unknown(unknown).unwrap_or_else(|| panic!("no unknown {unknown}")) as usize]
}

/// The section's input at the run's time: the node's voltage, or what its cell stands for.
fn input(sim: &LiveSim, cell: &str) -> f64 {
    let s = sim.solver();
    let Some((b, i)) = s.find_device(cell) else { return x(sim, "i") };
    let mut given = Vec::new();
    s.banks[b].stands_for(i, sim.t(), &mut given);
    match given.first() {
        Some(Given::Unknown(name, v)) if name == "i" => *v,
        other => panic!("the cell stands for its input first: {other:?}"),
    }
}

#[test]
fn a_run_handed_from_logic_to_the_switch_pair_and_back_keeps_the_cells_state() {
    for (name, pair, out, period) in [("the clock", clock(10e-9), "clk", 0.81e-3), ("the PWM generator", pwm(10e-9, 3.5), "pwm", 1.1e-3)] {
        let (cells, taken, _) = with_cells(&pair);
        let cell = taken[0].name.clone();
        let mut sim = live(&cells, period);
        run_to(&mut sim, &mut Record::default(), 3.4 * period);
        let t = sim.t();
        let logic = state(&sim, &cell);
        let at_input = input(&sim, &cell);
        replace(&mut sim, &pair);
        let fast = (x(&sim, "i"), x(&sim, out), state(&sim, "u1lo"), state(&sim, "u1hi"));
        assert_eq!(fast.0, at_input, "{name}: the pair's input is the cell's at the hand-over");
        assert_eq!((fast.2.clone(), fast.3.clone()), (vec![1.0 - logic[0]], vec![logic[0]]), "{name}: the pair stands as the cell's output did");
        replace(&mut sim, &cells);
        assert_eq!(state(&sim, &cell), logic, "{name}: Logic -> Fast -> Logic gives the cell's state back");
        replace(&mut sim, &pair);
        assert_eq!((x(&sim, "i"), x(&sim, out), state(&sim, "u1lo"), state(&sim, "u1hi")), fast, "{name}: and Fast -> Logic -> Fast the pair's");
        assert_eq!(sim.t(), t, "{name}: a hand-over leaves the run's time");
        // Logic to Fast wherever the switch falls along the period: the pair's input is the cell's
        // form there, not the cell's input node (Newton's value of it, a rounding apart)
        replace(&mut sim, &cells);
        for k in 1..=6 {
            run_to(&mut sim, &mut Record::default(), (3.4 + 0.1 * k as f64) * period);
            let at_input = input(&sim, &cell);
            replace(&mut sim, &pair);
            assert_eq!(x(&sim, "i"), at_input, "{name}: the pair's input is the cell's form at {:e} s", sim.t());
            replace(&mut sim, &cells);
        }
    }
}

#[test]
fn a_mode_switch_mid_period_is_continuous_and_the_period_goes_on() {
    for (name, pair, out, period) in [("the clock", clock(10e-9), "clk", 0.81e-3), ("the PWM generator", pwm(10e-9, 1.5), "pwm", 1.1e-3)] {
        let (cells, taken, _) = with_cells(&pair);
        let cell = taken[0].name.clone();
        let (ts, end) = (3.4 * period, 9.9 * period);
        for (from, to, way) in [(&cells, &pair, "Logic to Fast"), (&pair, &cells, "Fast to Logic")] {
            let mut reference = Record::default();
            run_to(&mut live(from, period), &mut reference, end);
            let mut sim = live(from, period);
            let mut before = Record::default();
            run_to(&mut sim, &mut before, ts);
            let t = sim.t();
            let (at_input, at_out) = (input(&sim, &cell), x(&sim, out));
            replace(&mut sim, to);
            assert_eq!((input(&sim, &cell), x(&sim, out)), (at_input, at_out), "{name}, {way}: the input and the output carry over unchanged");
            let mut after = Record::default();
            run_to(&mut sim, &mut after, end);
            let (ups, downs) = edges(&reference.at, &reference.v(out));
            let want: Vec<f64> = ups.into_iter().filter(|&e| e > t).collect();
            // no glitch on the output in the switch's wake, up to the unswitched run's next edge
            let next = want.iter().chain(downs.iter().filter(|&&e| e > t)).fold(f64::INFINITY, |a, &b| a.min(b));
            let o = after.v(out);
            for (k, &tk) in after.at.iter().enumerate().take_while(|&(_, &tk)| tk < (t + 0.05 * period).min(next - 0.01 * period)) {
                assert!((o[k] - at_out).abs() < 0.05, "{name}, {way}: the output at {tk:e} s is {} V, {at_out} V at the switch", o[k]);
            }
            let got = edges(&after.at, &o).0;
            assert!(want.len() >= 5 && got.len().abs_diff(want.len()) <= 1, "{name}, {way}: rising edges after the switch {got:?}, unswitched {want:?}");
            for (g, w) in got.iter().zip(&want) {
                assert!((g - w).abs() < 1e-3 * period, "{name}, {way}: a rising edge at {g:e} s, unswitched at {w:e} s");
            }
        }
    }
}

#[test]
fn a_pot_in_series_is_one_resistor_and_turning_it_retimes_the_cell_as_the_pair() {
    // the clock timed through r_t and a pot's B-W half (its A end on its wiper, a half that
    // carries nothing): 50k + 50k as the clock's 100k, then the pot turned to 150k
    let mut pair = board();
    section(&mut pair, "u1", "i", "clk");
    pair.r("rt", "clk", "span", 50e3);
    pair.r("pa", "span", "span", 200e3);
    pair.r("pb", "i", "span", 50e3);
    pair.c("ct", "i", "0", 10e-9);
    pair.r("rl", "clk", "0", 10e3);
    pair.set_ic("i", 0.0);
    let (cells, taken, analog) = with_cells(&pair);
    assert_eq!(taken.len(), 1, "the clock is a cell (left analog: {analog:?})");
    let cell = &cells.devices.cell[0];
    assert_eq!(cell.res.iter().map(|r| (r.0.as_str(), cells.nodes[r.1 as usize].name.as_str(), r.2)).collect::<Vec<_>>(), [("pb", "clk", 100e3)],
        "the chain is one resistor from the input to the output");
    let mut parts: Vec<_> = cell.parts.iter().map(|p| (p.0.as_str(), p.1, p.2)).collect();
    parts.sort_by(|a, b| a.0.cmp(b.0));
    assert_eq!(parts, [("pa", u32::MAX, 200e3), ("pb", 0, 50e3), ("rt", 0, 50e3)], "made of its parts, the wiper's half carrying nothing");
    let span = cells.find_node("span").expect("the chain's inner node stays a node");
    let mut on = 0;
    cells.devices.for_each_node(|m| on += usize::from(m == span));
    assert_eq!(on, 1, "the chain's inner node has its cell on it and nothing else");

    let period = 0.81e-3;
    let (turn, end) = (3.4 * period, 12.0 * period);
    let mut after = Vec::new();
    for (c, way) in [(&pair, "the pair"), (&cells, "the cell")] {
        let mut sim = live(c, period);
        let mut before = Record::default();
        run_to(&mut sim, &mut before, turn);
        if way == "the cell" {
            let (probe, form) = (x(&sim, "i"), input(&sim, &taken[0].name));
            assert!((probe - form).abs() < 1e-9, "the input node reads the cell's value: {probe} V, the form's {form} V");
            // the chain's current from the output into the input, through rt then pb: the inner
            // node halfway along, and each part's ends reading it (rt's a end, on the output,
            // takes it in, pb's, on the input, gives it out; ct's a end, on the input, takes it
            // all; pa carries none)
            let (clk, span) = (x(&sim, "clk"), x(&sim, "span"));
            let i = (clk - probe) / 100e3;
            assert!(i.abs() > 1e-6, "the chain carries current at the turn: {i} A");
            assert!((span - 0.5 * (clk + probe)).abs() < 1e-9, "the inner node stands halfway: {span} V between {clk} V and {probe} V");
            let s = sim.solver();
            let (b, k) = s.find_device(&taken[0].name).expect("the cell runs");
            let mut cx = s.base_cx();
            cx.tran = true;
            let mut currents = vec![f64::NAN; s.banks[b].terminals()];
            s.banks[b].terminal_currents(k, &s.x, &vec![0.0; s.x.len()], &cx, &mut currents);
            let cell = &cells.devices.cell[0];
            let at = |part: &str, end: u32| currents[cell.terminal_of(part, end).unwrap_or_else(|| panic!("the cell took {part}")) as usize];
            for (part, into_a) in [("rt", i), ("pb", -i), ("pa", 0.0), ("ct", i)] {
                assert!((at(part, 0) - into_a).abs() < 1e-9 * i.abs() && (at(part, 1) + into_a).abs() < 1e-9 * i.abs(),
                    "{part} reads {:e} A into its a end and {:e} A into its b end, {into_a:e} A and back wanted", at(part, 0), at(part, 1));
            }
            assert!(cell.terminal_of("rl", 0).is_none(), "a part the cell did not take is none of its terminals");
        }
        let generation = sim.generation();
        assert!(sim.apply(LiveCmd::SetParam { generation, device: "pb".into(), key: "value".into(), value: 150e3 }), "{way}: the pot's half is turned");
        assert_eq!(sim.stats().unknown, 0, "{way}: the run knows the pot's half");
        let mut rest = Record::default();
        run_to(&mut sim, &mut rest, end);
        let ups: Vec<f64> = edges(&rest.at, &rest.v("clk")).0.into_iter().filter(|&e| e > turn + 2.0 * period).collect();
        assert!(ups.len() >= 3, "{way}: rising edges after the turn {ups:?}");
        after.push((ups[ups.len() - 1] - ups[0]) / (ups.len() - 1) as f64);
    }
    println!("the pot turned to 150k: the pair's period {:e} s, the cell's {:e} s", after[0], after[1]);
    assert!(after[0] > 1.8 * period, "the pair slows with the timing resistance doubled: {:e} s", after[0]);
    assert!((after[1] / after[0] - 1.0).abs() < 2e-3, "the cell's period {:e} s against the pair's {:e} s after the turn", after[1], after[0]);
}

#[test]
fn a_hand_over_fills_what_only_the_new_circuit_has_by_dc_with_the_carried_held() {
    // an RC charging through 10 kohm into 1 uF (tau 10 ms); the new circuit hangs a divider with
    // its own capacitor from the charging node
    let rc = |more: bool| {
        let mut c = SimCircuit::new();
        c.v("v5", "vdd", "0", Source::dc(5.0));
        c.r("r1", "vdd", "a", 10e3);
        c.c("c1", "a", "0", 1e-6);
        c.set_ic("a", 0.0);
        if more {
            c.r("r2", "a", "n", 10e3);
            c.c("c2", "n", "0", 1e-6);
            c.r("r3", "n", "0", 10e3);
        }
        c
    };
    let mut sim = live(&rc(false), 10e-3);
    run_to(&mut sim, &mut Record::default(), 3e-3);
    let a = x(&sim, "a");
    assert!(a > 1.0 && a < 1.5, "a charges: {a} V at 3 ms");
    replace(&mut sim, &rc(true));
    assert_eq!(sim.stats().handover.as_ref().map(|h| h.filled.clone()), Some(vec!["n".to_string()]), "the new node is filled");
    assert_eq!(x(&sim, "a"), a, "the charging node stays where the run had it, not at the new circuit's DC");
    assert!((x(&sim, "n") - a / 2.0).abs() < 1e-6, "the new node takes its DC value from the held one: {} V, {} V wanted", x(&sim, "n"), a / 2.0);
    let mut after = Record::default();
    run_to(&mut sim, &mut after, 3.2e-3);
    let n = after.v("n");
    assert!((n[0] - a / 2.0).abs() < 1e-3, "the run goes on from there: n {} V at the first point", n[0]);
}

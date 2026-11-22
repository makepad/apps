//! tonebox: one key, one knob, one LED and one audio jack. Nothing in the app or the simulator
//! knows it: its documents alone make it a board that plays. Holding the key lights the LED and
//! sounds the RC oscillator at its parts' formula pitch on the jack, and a changed resistor in the
//! circuit document changes the note as the formula says.

use fab_elec_bench::input::Taper;
use fab_elec_bench::{lower, ModelLibs, SimCmd};
use fab_elec_core::ids::CompRef;
use fab_elec_lang::{evaluate, std_libs, Evaluated, Sources, UnitState};
use fab_elec_sim::fft::{band_rms, resample_hermite, Window};
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::stamp::UnknownInfo;
use fab_elec_sim::{Options, Sink};

const R_T: f64 = 100e3;
const C_T: f64 = 10e-9;
/// The pitch pot: 250k linear at mid travel, wired as its B-W half in series with r_t.
const POT: f64 = 250e3;
const POS: f64 = 0.5;

fn sources(r_t: &str) -> Sources {
    let mut sources = fab_elec_fixtures::docs("tonebox").expect("tonebox is a stock project");
    if r_t != "100kohm" {
        let id = sources.find("tonebox.ckt.splash").expect("tonebox has its circuit");
        let text = sources.doc(id).expect("its document").text.replace("e.r(100kohm, osc, span", &format!("e.r({r_t}, osc, span"));
        sources.set_present(id, false).expect("the circuit document is the project's");
        sources.add("tonebox.ckt.splash", text).expect("the edited circuit takes its place");
    }
    sources
}

fn tonebox(sources: &Sources) -> Evaluated {
    let ev = evaluate(sources, &std_libs()).unwrap_or_else(|diags| panic!("tonebox evaluates: {diags:?}"));
    for state in ev.units.iter().map(|u| &u.state).chain(ev.benches.iter().map(|b| &b.state)) {
        assert_eq!(*state, UnitState::Built, "the tone box and its bench build: {:?}", state.diags());
    }
    ev
}

/// Some nets' voltages at every accepted point of a run. It hears the nets in `hearing` at the
/// sample rate, as a Listen tap does (`Sink::heard`).
struct Scope {
    nets: Vec<&'static str>,
    at: Vec<usize>,
    t: Vec<f64>,
    v: Vec<Vec<f64>>,
    hearing: Vec<&'static str>,
}

impl Scope {
    fn new(nets: &[&'static str]) -> Scope {
        Scope { nets: nets.to_vec(), at: Vec::new(), t: Vec::new(), v: vec![Vec::new(); nets.len()], hearing: Vec::new() }
    }

    /// The net's (t, v) points from `from` to `to` seconds.
    fn trace(&self, net: &str, from: f64, to: f64) -> Vec<(f64, f64)> {
        let k = self.nets.iter().position(|n| *n == net).expect("a recorded net");
        self.t.iter().copied().zip(self.v[k].iter().copied()).filter(|p| p.0 >= from && p.0 <= to).collect()
    }
}

impl Sink for Scope {
    fn begin(&mut self, unknowns: &[UnknownInfo]) {
        self.at = self.nets.iter().map(|net| {
            unknowns.iter().position(|u| u.name == *net)
                .unwrap_or_else(|| panic!("no unknown is net {net}: {:?}", unknowns.iter().map(|u| &u.name).collect::<Vec<_>>()))
        }).collect();
    }
    fn point(&mut self, t: f64, _outer: Option<f64>, x: &[f64]) {
        self.t.push(t);
        for (v, &i) in self.v.iter_mut().zip(&self.at) {
            v.push(x[i]);
        }
    }
    fn heard(&mut self, out: &mut Vec<u32>) {
        for (net, &i) in self.nets.iter().zip(&self.at) {
            if self.hearing.contains(net) {
                out.push(i as u32);
            }
        }
    }
}

/// The box's bench, live, with the key's press commands: the key held from `down` to `up`
/// seconds, the run to `end`.
fn play(ev: &Evaluated, scope: &mut Scope, down: f64, up: f64, end: f64) {
    let bench = ev.benches.iter().find(|b| b.name == "box").expect("tonebox has its bench");
    let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    assert!(lowered.diags.is_empty(), "the box lowers without diagnostics: {:?}", lowered.diags);
    let unit = ev.design.unit(bench.scope_id()).expect("the bench runs the tone box");
    let key = CompRef { board: unit.id, comp: unit.circuit.comp("key").expect("the box has its key").id };
    let spec = LiveSpec { rate: f64::INFINITY, reltol: Some(Options::default().reltol), trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .unwrap_or_else(|e| panic!("the box's operating point: {}", e.message));
    scope.begin(&sim.solver().unknowns);
    let mut run_to = |sim: &mut LiveSim, t: f64| {
        sim.apply(fab_elec_sim::live::LiveCmd::Step(t - sim.t()));
        while sim.t() < t {
            let before = sim.t();
            sim.advance(f64::INFINITY, None, scope).unwrap_or_else(|e| panic!("the run goes on: {}", e.message));
            assert!(sim.t() > before || sim.t() >= t, "the run moves (stuck at {before} s)");
        }
    };
    for (at, held) in [(down, true), (up, false)] {
        run_to(&mut sim, at);
        let cmds = lowered.index.command(&SimCmd::press(key, held));
        assert!(!cmds.is_empty(), "the key is a momentary input of the bench");
        for cmd in cmds {
            sim.apply(cmd);
        }
    }
    run_to(&mut sim, end);
}

/// Rising crossings of mid-supply, interpolated.
fn rises(points: &[(f64, f64)]) -> Vec<f64> {
    points.windows(2).filter(|w| w[0].1 < 2.5 && w[1].1 >= 2.5)
        .map(|w| w[0].0 + (w[1].0 - w[0].0) * (2.5 - w[0].1) / (w[1].1 - w[0].1)).collect()
}

/// The longest time the oscillator's square wave stays high or low over `points`, between its
/// mid-supply crossings.
fn longer_half(points: &[(f64, f64)]) -> f64 {
    let cross = |up: bool| -> Vec<f64> {
        points.windows(2).filter(|w| (w[0].1 < 2.5) == up && (w[1].1 < 2.5) != up)
            .map(|w| w[0].0 + (w[1].0 - w[0].0) * (2.5 - w[0].1) / (w[1].1 - w[0].1)).collect()
    };
    let mut edges = [cross(true), cross(false)].concat();
    edges.sort_by(f64::total_cmp);
    assert!(edges.len() > 5, "the oscillator runs: {} edges", edges.len());
    edges.windows(2).map(|w| w[1] - w[0]).fold(0.0, f64::max)
}

/// The oscillator's mean frequency over `points`, from its rising edges after the first.
fn pitch(points: &[(f64, f64)]) -> f64 {
    let r = rises(points);
    assert!(r.len() > 5, "the oscillator runs: {} rising edges", r.len());
    (r.len() - 2) as f64 / (r[r.len() - 1] - r[1])
}

/// A Schmitt inverter RC oscillator's pitch with the generic card's thresholds, its timing
/// resistance `r_t` in series with the pitch pot's B-W half at `POS`.
fn formula(r_t: f64, c: f64) -> f64 {
    let libs = ModelLibs::generic();
    let r = r_t + Taper::Linear.halves(POT, POS, libs.pot_end).1;
    let card = libs.schmitt;
    let (vp, vm, vdd) = (card.vt_plus, card.vt_minus, card.vdd);
    1.0 / (r * c * ((vp * (vdd - vm)) / (vm * (vdd - vp))).ln())
}

/// The box is still with its key up; held, the LED lights and the jack plays the oscillator's
/// formula pitch at its divider's 0.87 V peak to peak; released, both stop. With r_t edited from 100k to
/// 22k in the circuit document, the same press plays the new formula pitch.
#[test]
fn holding_the_key_lights_the_led_and_plays_the_formula_pitch_and_a_resistor_changes_it() {
    let ev = tonebox(&sources("100kohm"));
    let unit = ev.design.units.iter().find(|u| u.name == "tonebox").expect("the board unit");
    let board = unit.board.as_ref().expect("tonebox has its board");
    assert_eq!(board.placed.len(), unit.circuit.comps.len(), "every part of the circuit is placed");

    let mut scope = Scope::new(&["OSC", "OUT", "QUIET", "lamp.d.A"]);
    play(&ev, &mut scope, 0.01, 0.06, 0.075);
    let quiet = |from: f64, to: f64| scope.trace("QUIET", from, to);
    // the LED's current through its resistor: VBUS over the anode
    let r_led = unit.circuit.comp("lamp.r").and_then(|c| c.value.as_ref()).and_then(|v| v.qty).map(|q| q.si)
        .expect("the LED's resistor has its value");
    let led_ma = |at: f64| {
        let anode = scope.trace("lamp.d.A", 0.0, at).last().expect("a point before").1;
        (5.0 - anode) / r_led * 1e3
    };
    assert!(rises(&scope.trace("OSC", 0.001, 0.01)).is_empty(), "the key up, the oscillator is still");
    assert!(quiet(0.001, 0.01).iter().all(|p| p.1 > 4.5) && led_ma(0.0099) < 0.01,
        "the key up, QUIET is high and the LED dark: {:.3} mA", led_ma(0.0099));
    assert!(quiet(0.012, 0.06).iter().all(|p| p.1 < 0.5) && led_ma(0.03) > 3.0,
        "the key held, QUIET sinks the LED: {:.3} mA", led_ma(0.03));
    let f = pitch(&scope.trace("OSC", 0.015, 0.06));
    let want = formula(R_T, C_T);
    assert!((f / want - 1.0).abs() < 0.01, "the tone is {f:.1} Hz, the formula's {want:.1} Hz within 1 %");
    // one period at the end: the coupling capacitor's slow settling (55 ms) adds little to it
    let line = scope.trace("OUT", 0.06 - 1.0 / want, 0.06);
    let swing = line.iter().map(|p| p.1).fold(f64::MIN, f64::max) - line.iter().map(|p| p.1).fold(f64::MAX, f64::min);
    // the divider 39k over 10k, loaded through the capacitor by the 47k bleed
    let low = 10e3 * 47e3 / 57e3;
    let divided = 5.0 * low / (39e3 + low);
    assert!((swing / divided - 1.0).abs() < 0.03, "the jack swings {swing:.3} V, the divider's {divided:.3} V within 3 %");
    assert!(rises(&scope.trace("OSC", 0.062, 0.075)).is_empty() && led_ma(0.075) < 0.01,
        "released, the tone stops and the LED goes dark: {:.3} mA", led_ma(0.075));

    // the resistor changed in the circuit document: the note follows it
    let edited = tonebox(&sources("22kohm"));
    let mut scope = Scope::new(&["OSC"]);
    play(&edited, &mut scope, 0.002, 0.03, 0.03);
    let higher = pitch(&scope.trace("OSC", 0.006, 0.03));
    let want = formula(22e3, C_T);
    assert!((higher / want - 1.0).abs() < 0.01, "with r_t 22k the tone is {higher:.1} Hz, the formula's {want:.1} Hz within 1 %");
    assert!(higher > f * 1.4, "a smaller r_t raises the note: {f:.1} Hz to {higher:.1} Hz");
}

/// In rounds the oscillator's block, wherever it sits in the plan, steps in Fast on its own bound
/// (`LiveSpec::own_bound`): the block is found by its net (`LiveSim::block_of`). At 48 and 96 kHz
/// alike, a round being 32 samples at either: with the key up the block crosses no threshold of
/// its own and takes long steps, fewer than half the round's samples, but while its oscillator's
/// net is heard (`Sink::heard`) it keeps the sample period, so a listened net keeps its highs; so
/// it does while only the output it feeds is heard (a block a heard block reads is heard too).
/// With the key held its oscillator's crossings shorten its steps to a fiftieth of their spacing
/// (`rounds::CROSSING_SHARE`), a time: the sample period at 48 kHz, two at 96 kHz. The pitch is
/// the whole run's within 0.1 cents at both rates (a bound of four 48 kHz periods would put it
/// about half a cent flat).
#[test]
fn an_oscillator_keeps_its_pitch_in_rounds_and_a_quiet_one_takes_its_own_bound_unless_heard() {
    let ev = tonebox(&sources("100kohm"));
    let bench = ev.benches.iter().find(|b| b.name == "box").expect("tonebox has its bench");
    let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    assert!(lowered.diags.is_empty(), "the box lowers without diagnostics: {:?}", lowered.diags);
    let unit = ev.design.unit(bench.scope_id()).expect("the bench runs the tone box");
    let key = CompRef { board: unit.id, comp: unit.circuit.comp("key").expect("the box has its key").id };

    for rate in [48000.0, 96000.0] {
        let new_sim = || {
            let spec = LiveSpec {
                rate: f64::INFINITY, reltol: Some(Options::default().reltol), trace: false,
                res: 100e-6, sample_rate: rate, ..LiveSpec::default()
            };
            LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
                .unwrap_or_else(|e| panic!("the box's operating point: {}", e.message))
        };
        let mut whole = new_sim();
        let mut rounds = new_sim();
        rounds.set_planner(Some(Box::new(fab_elec_sim::analysis::round::BlockPlanner::for_run(&rounds, f64::INFINITY))))
            .unwrap_or_else(|e| panic!("could not set the tone box's round planner: {}", e.message));
        assert!(rounds.blocks() >= 2, "at {rate} Hz the tone box runs in rounds: {} blocks", rounds.blocks());
        // a round is `LiveSpec::round` sample periods at any rate
        let samples = (rounds.round_len() * rate).round();
        let osc = rounds.block_of("OSC").expect("in rounds a block solves the oscillator's net");

        let run_to = |sim: &mut LiveSim, scope: &mut Scope, t: f64| {
            sim.apply(fab_elec_sim::live::LiveCmd::Step(t - sim.t()));
            while sim.t() < t {
                let before = sim.t();
                sim.advance(f64::INFINITY, None, scope).unwrap_or_else(|e| panic!("the run goes on: {}", e.message));
                assert!(sim.t() > before || sim.t() >= t, "the run moves (stuck at {before} s)");
            }
        };
        let mut whole_scope = Scope::new(&["OSC", "OUT"]);
        let mut rounds_scope = Scope::new(&["OSC", "OUT"]);
        for (sim, scope) in [(&mut whole, &mut whole_scope), (&mut rounds, &mut rounds_scope)] {
            scope.begin(&sim.solver().unknowns);
            scope.hearing = vec!["OSC"];
            run_to(sim, scope, 0.01);
            scope.hearing.clear();
        }
        let (heard_rounds, _, heard_work) = rounds.block_work().expect("the rounds report their work while the oscillator is heard");
        assert_eq!(rounds.block_of("OSC"), Some(osc), "at {rate} Hz, the plan holds while the oscillator is heard");
        let heard_steps = heard_work[osc].stats.steps;
        let heard_per_round = heard_steps as f64 / heard_rounds as f64;
        assert!(heard_per_round >= samples,
            "at {rate} Hz, key up and heard, the oscillator's block {osc} should keep the sample period: {heard_steps} steps in {heard_rounds} rounds ({heard_per_round:.2} per round)");

        for (sim, scope) in [(&mut whole, &mut whole_scope), (&mut rounds, &mut rounds_scope)] {
            run_to(sim, scope, 0.03);
        }
        let (quiet_rounds, _, quiet_work) = rounds.block_work().expect("the rounds report their work before the press");
        assert!(quiet_rounds > heard_rounds, "the rounds advance with the key up: {heard_rounds} heard, {quiet_rounds} after");
        assert_eq!(rounds.block_of("OSC"), Some(osc), "at {rate} Hz, the plan holds with the key up");
        let quiet_steps = quiet_work[osc].stats.steps;
        let quiet_per_round = (quiet_steps - heard_steps) as f64 / (quiet_rounds - heard_rounds) as f64;
        assert!(quiet_per_round < samples / 2.0,
            "at {rate} Hz, key up and no longer heard, the oscillator's block {osc} should take fewer than half the round's {samples} samples in steps a round: {} steps in {} rounds ({quiet_per_round:.2} per round)",
            quiet_steps - heard_steps, quiet_rounds - heard_rounds);

        // only the output heard: the oscillator's block feeds it, so it is heard too and keeps the
        // sample period with the key up: at least a step a sample period, counted from a round in
        // (what is heard is told as a round opens, so the round the hearing began in is partly
        // unheard) and over the time the run took
        let out = rounds.block_of("OUT").expect("in rounds a block solves the output");
        assert_ne!(out, osc, "the output is solved in a block of its own, which reads the oscillator's");
        for (sim, scope) in [(&mut whole, &mut whole_scope), (&mut rounds, &mut rounds_scope)] {
            scope.hearing = vec!["OUT"];
            run_to(sim, scope, 0.032);
        }
        let (_, _, from_work) = rounds.block_work().expect("the rounds report their work as the output is heard");
        let from_t = rounds.t();
        for (sim, scope) in [(&mut whole, &mut whole_scope), (&mut rounds, &mut rounds_scope)] {
            run_to(sim, scope, 0.05);
            scope.hearing.clear();
        }
        let (_, _, fed_work) = rounds.block_work().expect("the rounds report their work while the output is heard");
        assert_eq!(rounds.block_of("OSC"), Some(osc), "at {rate} Hz, the plan holds while the output is heard");
        let fed_steps = fed_work[osc].stats.steps - from_work[osc].stats.steps;
        let periods = (rounds.t() - from_t) * rate;
        let fed_per_period = fed_steps as f64 / periods;
        assert!(fed_per_period >= 1.0,
            "at {rate} Hz, key up and only the output heard, the oscillator's block {osc}, which the output's block {out} reads, should keep the sample period: {fed_steps} steps in {periods:.2} sample periods from {from_t} s to {} s ({fed_per_period:.3} a period; {} of its rounds)",
            rounds.t(), fed_work[osc].first_n - from_work[osc].first_n);

        for (sim, scope) in [(&mut whole, &mut whole_scope), (&mut rounds, &mut rounds_scope)] {
            let cmds = lowered.index.command(&SimCmd::press(key, true));
            assert!(!cmds.is_empty(), "the key is a momentary input of the bench");
            for cmd in cmds {
                sim.apply(cmd);
            }
            run_to(sim, scope, 0.07);
        }
        // counted once the oscillator runs: from the press to its first edge the block has no
        // crossings of its own yet and takes its longest bound
        let (settled_rounds, _, settled_work) = rounds.block_work().expect("the rounds report their work as the oscillator starts");
        for (sim, scope) in [(&mut whole, &mut whole_scope), (&mut rounds, &mut rounds_scope)] {
            run_to(sim, scope, 0.15);
        }
        let (total_rounds, _, held_work) = rounds.block_work().expect("the rounds report their work with the key held");
        assert!(total_rounds > settled_rounds, "at {rate} Hz, the rounds advance with the key held: {settled_rounds} as it starts, {total_rounds} after");
        let held_rounds = total_rounds - settled_rounds;
        assert_eq!(rounds.block_of("OSC"), Some(osc), "at {rate} Hz, the plan holds with the key held");
        let held_steps = held_work[osc].stats.steps - settled_work[osc].stats.steps;
        let held_per_round = held_steps as f64 / held_rounds as f64;
        // a step spans at most a fiftieth of the spacing of the block's own crossings
        // (`rounds::CROSSING_SHARE`) in a power of two of sample periods, never under one; that
        // spacing is at most the oscillator's longer half period
        let half = longer_half(&rounds_scope.trace("OSC", 0.07, 0.15));
        let mut span = 1.0;
        while 2.0 * span / rate <= half / 50.0 {
            span *= 2.0;
        }
        let least = samples / span;
        assert!(held_per_round >= least,
            "at {rate} Hz, key held, the oscillator's block {osc} should step within a fiftieth of its crossings' spacing ({half:e} s at most), {span} sample period(s), so at least {least} steps a round: {held_steps} steps in {held_rounds} rounds ({held_per_round:.2} per round)");

        let before_press = rises(&whole_scope.trace("OSC", 0.001, 0.05));
        assert!(before_press.is_empty(), "at {rate} Hz, the whole run's oscillator should be still before the press: {} rising edges", before_press.len());
        let f_whole = pitch(&whole_scope.trace("OSC", 0.07, 0.15));
        let f_rounds = pitch(&rounds_scope.trace("OSC", 0.07, 0.15));
        let cents = 1200.0 * (f_rounds / f_whole).log2();
        assert!(cents.abs() < 0.1,
            "at {rate} Hz, the pitch in rounds should be within 0.1 cents of the whole run: {f_rounds:.4} Hz in rounds, {f_whole:.4} Hz whole ({cents:.4} cents)");
    }
}

/// The placed board: no DRC finding but its unrouted nets.
#[test]
fn the_board_is_placed_with_no_finding_but_its_unrouted_nets() {
    let ev = tonebox(&sources("100kohm"));
    let unit = ev.design.units.iter().find(|u| u.name == "tonebox").expect("the board unit");
    let results = fab_elec_design::engines::full_with(&ev.design, &fab_elec_board::Factories::new());
    let drc = &results.boards.iter().find(|b| b.unit == unit.id).expect("the board's results").drc;
    let found: Vec<_> = drc.iter().filter(|v| v.kind != fab_elec_circuit::check::CheckKind::Unconnected).collect();
    assert!(found.is_empty(), "the placed board has no finding but its unrouted nets: {found:#?}");
}

/// OUT's third-octave bands from 25 Hz to the sample rate's Nyquist, as (centre Hz, dB), from
/// `from` to `to` seconds on the sample grid at `rate` (its mean taken out, a Hann window).
fn bands(scope: &Scope, from: f64, to: f64, rate: f64) -> Vec<(f64, f64)> {
    let (t, v): (Vec<f64>, Vec<f64>) = scope.trace("OUT", from, to).into_iter().unzip();
    let n = ((to - from) * rate) as usize;
    let y = resample_hermite(&t, &v, None, from, 1.0 / rate, n);
    let mean = y.iter().sum::<f64>() / n as f64;
    let y: Vec<f64> = y.iter().map(|x| x - mean).collect();
    let edge = 2f64.powf(1.0 / 6.0);
    (0..30).map(|i| 25.0 * 2f64.powf(i as f64 / 3.0)).filter(|&fc| fc * edge <= rate / 2.0)
        .map(|fc| (fc, 20.0 * band_rms(&y, 1.0 / rate, Window::Hann, fc / edge, fc * edge).max(1e-12).log10()))
        .collect()
}

/// Planned with the key up, where the key's logic holds the timing node still, and pressed 10 ms
/// in: in rounds the oscillator's loop stays whole through the press, at 48 and 96 kHz. Over the
/// 0.1 s after it the pitch is within 0.1 % of the whole run's, and the output's third-octave
/// bands within 30 dB of its loudest are within 3 dB of the whole run's. A plan judged in the
/// key-up state alone cut the loop there, read three rounds late, and played 61 to 75 % flat
/// until it planned again.
#[test]
fn a_press_after_a_plan_made_at_rest_keeps_the_pitch_and_bands_in_rounds() {
    const PRESS: f64 = 0.01;
    let ev = tonebox(&sources("100kohm"));
    let bench = ev.benches.iter().find(|b| b.name == "box").expect("tonebox has its bench");
    let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    assert!(lowered.diags.is_empty(), "the box lowers without diagnostics: {:?}", lowered.diags);
    let unit = ev.design.unit(bench.scope_id()).expect("the bench runs the tone box");
    let key = CompRef { board: unit.id, comp: unit.circuit.comp("key").expect("the box has its key").id };
    let run_to = |sim: &mut LiveSim, scope: &mut Scope, t: f64| {
        sim.apply(fab_elec_sim::live::LiveCmd::Step(t - sim.t()));
        while sim.t() < t {
            let before = sim.t();
            sim.advance(f64::INFINITY, None, scope).unwrap_or_else(|e| panic!("the run goes on: {}", e.message));
            assert!(sim.t() > before || sim.t() >= t, "the run moves (stuck at {before} s)");
        }
    };
    for rate in [48000.0, 96000.0] {
        let new_sim = || {
            let spec = LiveSpec {
                rate: f64::INFINITY, reltol: Some(Options::default().reltol), trace: false,
                res: 100e-6, sample_rate: rate, ..LiveSpec::default()
            };
            LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
                .unwrap_or_else(|e| panic!("the box's operating point: {}", e.message))
        };
        let mut whole = new_sim();
        let mut rounds = new_sim();
        rounds.set_planner(Some(Box::new(fab_elec_sim::analysis::round::BlockPlanner::for_run(&rounds, f64::INFINITY))))
            .unwrap_or_else(|e| panic!("could not set the tone box's round planner: {}", e.message));
        assert!(rounds.blocks() >= 2, "at {rate} Hz the tone box runs in rounds: {} blocks", rounds.blocks());
        let mut whole_scope = Scope::new(&["OSC", "OUT"]);
        let mut rounds_scope = Scope::new(&["OSC", "OUT"]);
        for (sim, scope) in [(&mut whole, &mut whole_scope), (&mut rounds, &mut rounds_scope)] {
            scope.begin(&sim.solver().unknowns);
            scope.hearing = vec!["OUT"];
            run_to(sim, scope, PRESS);
            for cmd in lowered.index.command(&SimCmd::press(key, true)) {
                sim.apply(cmd);
            }
            run_to(sim, scope, PRESS + 0.1);
        }
        let f_whole = pitch(&whole_scope.trace("OSC", PRESS, PRESS + 0.1));
        let f_rounds = pitch(&rounds_scope.trace("OSC", PRESS, PRESS + 0.1));
        let off = 100.0 * (f_rounds / f_whole - 1.0);
        assert!(off.abs() < 0.1,
            "at {rate} Hz, over the 0.1 s after the press, the pitch in rounds should be within 0.1 % of the whole run's: {f_rounds:.3} Hz in rounds, {f_whole:.3} Hz whole ({off:+.3} %)");
        let (bw, br) = (bands(&whole_scope, PRESS, PRESS + 0.1, rate), bands(&rounds_scope, PRESS, PRESS + 0.1, rate));
        let top = bw.iter().map(|b| b.1).fold(f64::NEG_INFINITY, f64::max);
        let (fc, worst) = bw.iter().zip(&br).filter(|(w, _)| w.1 > top - 30.0).map(|(w, r)| (w.0, r.1 - w.1))
            .fold((0.0, 0.0), |m: (f64, f64), b| if b.1.abs() > m.1.abs() { b } else { m });
        assert!(worst.abs() <= 3.0,
            "at {rate} Hz, over the 0.1 s after the press, the output's bands in rounds should be within 3 dB of the whole run's: {worst:+.2} dB in the band at {fc:.0} Hz");
    }
}

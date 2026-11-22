//! organ8: an eight-voice organ, eight copies of one block (a Schmitt inverter RC oscillator that
//! runs all the time, let through to the mix while its key is held) summed to a line out. Nothing in
//! it is a drum's: it is the live simulator's second round-machinery load beside drum16, enough
//! switching that one stepper falls behind real time on the Mac, and its cut at the host's listening
//! step is pinned (`LIVE_BLOCKS`) so the test follows the planner as it learns. In Logic its eight
//! oscillators are timing cells, and every pin still reads a device the lowering has.

use fab_elec_bench::input::Taper;
use fab_elec_bench::{lower, ModelLibs, SimCmd};
use fab_elec_core::ids::CompRef;
use fab_elec_lang::{evaluate, std_libs, UnitState};
use fab_elec_sim::analysis::round::BlockPlanner;
use fab_elec_sim::live::LiveCmd;
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::stamp::UnknownInfo;
use fab_elec_sim::{Options, Sink};

/// The host's listening run: its time resolution and the rate it hears the line out at, which set
/// its step and so the band the round planner proves a cut over (to half the step rate, 24 kHz).
const LISTEN_RES: f64 = 100e-6;
const LISTEN_RATE: f64 = 48_000.0;

/// The blocks the round planner cuts the organ into at the host's listening step: ten, a block a
/// voice and two for the supplies and the summer. Each voice (its key's node, its sections, its
/// output and its coupling node) is a block; the supplies (VBUS's, with the VA rail's island) are
/// block 0 and the summer (VREF's, with the line out) the last. What a voice's switches draw from
/// the rail is the voice's own return, and each mix resistor is judged at the summer by its test
/// (a share there anchors only at half or more), which it passes, so neither the rail nor the
/// summer ties the voices together.
const LIVE_BLOCKS: usize = 10;




/// Voice 8's timing: r_t, its capacitor and the pitch pot (250k linear at mid travel, its B-W
/// half in series with r_t).
const R_T: f64 = 100e3;
const C_T: f64 = 2.7e-9;
const POT: f64 = 250e3;
const POS: f64 = 0.5;

/// Some nets' voltages at every accepted point of a run.
struct Scope {
    nets: Vec<&'static str>,
    at: Vec<usize>,
    t: Vec<f64>,
    v: Vec<Vec<f64>>,
}

impl Scope {
    fn trace(&self, net: &str, from: f64, to: f64) -> Vec<(f64, f64)> {
        let k = self.nets.iter().position(|n| *n == net).expect("a recorded net");
        self.t.iter().copied().zip(self.v[k].iter().copied()).filter(|p| p.0 >= from && p.0 <= to).collect()
    }

    /// The net's peak-to-peak swing from `from` to `to` seconds.
    fn swing(&self, net: &str, from: f64, to: f64) -> f64 {
        let v: Vec<f64> = self.trace(net, from, to).iter().map(|p| p.1).collect();
        v.iter().copied().fold(f64::MIN, f64::max) - v.iter().copied().fold(f64::MAX, f64::min)
    }

    /// The largest change of the net over `lag` seconds, for t from `from` to `to`: a square wave's
    /// full step at half its period, where a slow drift moves it by its rate times `lag` only.
    fn step(&self, net: &str, from: f64, to: f64, lag: f64) -> f64 {
        let all = self.trace(net, 0.0, f64::INFINITY);
        let at = |t: f64| {
            let i = all.partition_point(|p| p.0 < t).clamp(1, all.len() - 1);
            let (a, b) = (all[i - 1], all[i]);
            if b.0 > a.0 { a.1 + (b.1 - a.1) * (t - a.0) / (b.0 - a.0) } else { b.1 }
        };
        self.trace(net, from, to).iter().map(|p| (p.1 - at(p.0 - lag)).abs()).fold(0.0, f64::max)
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
}

/// The oscillator's mean frequency over `points`, from its rising crossings of mid-supply after
/// the first.
fn pitch(points: &[(f64, f64)]) -> f64 {
    let r: Vec<f64> = points.windows(2).filter(|w| w[0].1 < 2.5 && w[1].1 >= 2.5)
        .map(|w| w[0].0 + (w[1].0 - w[0].0) * (2.5 - w[0].1) / (w[1].1 - w[0].1)).collect();
    assert!(r.len() > 5, "the oscillator runs: {} rising crossings", r.len());
    (r.len() - 2) as f64 / (r[r.len() - 1] - r[1])
}

/// A Schmitt inverter RC oscillator's pitch with the generic card's thresholds.
fn formula(r_t: f64, c: f64) -> f64 {
    let libs = ModelLibs::generic();
    let r = r_t + Taper::Linear.halves(POT, POS, libs.pot_end).1;
    let card = libs.schmitt;
    let (vp, vm, vdd) = (card.vt_plus, card.vt_minus, card.vdd);
    1.0 / (r * c * ((vp * (vdd - vm)) / (vm * (vdd - vp))).ln())
}

/// The organ evaluates and builds; at the host's listening step the round planner cuts it into
/// `LIVE_BLOCKS` blocks; and with every key up the voices run and the line out is still, while voice 8's
/// key held lets its tone through: its oscillator at the formula pitch, its gated output following
/// it and the line out stepping by the summer's 5 V rf / r_mix at each of its edges, on one stepper
/// and in rounds alike.
#[test]
fn the_organ_cuts_as_pinned_at_the_listening_step_and_a_held_key_lets_its_voice_through() {
    let sources = fab_elec_fixtures::docs("organ8").expect("organ8 is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|diags| panic!("organ8 evaluates: {diags:?}"));
    for state in ev.units.iter().map(|u| &u.state).chain(ev.benches.iter().map(|b| &b.state)) {
        assert_eq!(*state, UnitState::Built, "the organ and its bench build: {:?}", state.diags());
    }
    let bench = ev.benches.iter().find(|b| b.name == "organ").expect("organ8 has its bench");
    let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    assert!(lowered.diags.is_empty(), "the organ lowers without diagnostics: {:?}", lowered.diags);
    let unit = ev.design.unit(bench.scope_id()).expect("the bench runs the organ");
    let comp = |name: &str| unit.circuit.comp(name).unwrap_or_else(|| panic!("no part {name}: {:?}", unit.circuit.comps.iter().map(|c| &c.path).collect::<Vec<_>>()));
    let key = CompRef { board: unit.id, comp: comp("v8.key").id };
    let new_sim = || {
        let spec = LiveSpec { rate: f64::INFINITY, reltol: Some(Options::default().reltol), trace: false, ..LiveSpec::default() };
        LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
            .unwrap_or_else(|e| panic!("the organ's operating point: {}", e.message))
    };

    let spec = LiveSpec { res: LISTEN_RES, sample_rate: LISTEN_RATE, rate: f64::INFINITY, reltol: Some(Options::default().reltol), trace: false, ..LiveSpec::default() };
    let mut rounds = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .unwrap_or_else(|e| panic!("the organ's operating point: {}", e.message));
    rounds.set_planner(Some(Box::new(BlockPlanner::for_run(&rounds, f64::INFINITY)))).unwrap_or_else(|e| panic!("the organ's round plan: {}", e.message));
    let names: Vec<String> = rounds.block_work().map_or_else(|| vec!["(whole)".into()], |(_, _, work)| work.iter().map(|w| w.name.clone()).collect());
    assert_eq!(rounds.blocks(), LIVE_BLOCKS, "at the listening step ({:e} s) the planner cuts the organ into {LIVE_BLOCKS} block(s): {names:?}", rounds.step_cap());

    // the operating point holds the timing capacitors empty and no section's output at 0 V: the
    // voices' shared rail stands at the supply's 5 V less what its 10 ohms drop
    let mut sim = new_sim();
    let va = sim.solver().unknown("VA").map(|u| sim.solver().x[u as usize]).expect("VA is a net");
    assert!((4.9..5.0).contains(&va), "VA at the operating point: {va} V, the supply's 5 V less a quiescent drop");
    let mut scope = Scope { nets: vec!["LINE", "v8.osc", "v8.tone"], at: Vec::new(), t: Vec::new(), v: vec![Vec::new(); 3] };
    scope.begin(&sim.solver().unknowns);
    let run_to = |sim: &mut LiveSim, scope: &mut Scope, t: f64| {
        sim.apply(LiveCmd::Step(t - sim.t()));
        while sim.t() < t {
            let before = sim.t();
            sim.advance(f64::INFINITY, None, scope).unwrap_or_else(|e| panic!("the run goes on: {}", e.message));
            assert!(sim.t() > before || sim.t() >= t, "the run moves (stuck at {before} s)");
        }
    };
    run_to(&mut sim, &mut scope, 0.01);
    for cmd in lowered.index.command(&SimCmd::press(key, true)) {
        sim.apply(cmd);
    }
    run_to(&mut sim, &mut scope, 0.03);
    let want = formula(R_T, C_T);
    let f = pitch(&scope.trace("v8.osc", 0.002, 0.01));
    assert!((f / want - 1.0).abs() < 0.01, "the key up, voice 8 runs at {f:.1} Hz, the formula's {want:.1} Hz within 1 %");
    // the line settles from power-up (the signal ground and the output capacitor charging) for
    // much longer than the run: a voice on it is a step of the summer's swing at each half period
    let half = 0.5 / want;
    let summed = 5.0 * 15e3 / 220e3;
    let quiet = scope.step("LINE", 0.004, 0.01, half);
    assert!(scope.swing("v8.tone", 0.002, 0.01) < 0.05 && quiet < 0.1 * summed,
        "every key up, no voice is let through: voice 8 out swings {:.4} V, the line moves {quiet:.4} V in half a period", scope.swing("v8.tone", 0.002, 0.01));
    let held = pitch(&scope.trace("v8.tone", 0.012, 0.03));
    assert!((held / want - 1.0).abs() < 0.01, "held, voice 8 out follows its oscillator: {held:.1} Hz, the formula's {want:.1} Hz within 1 %");
    let line = scope.step("LINE", 0.015, 0.03, half);
    assert!((line / summed - 1.0).abs() < 0.05, "the line steps {line:.3} V each half period, the summer's {summed:.3} V within 5 %");

    // in rounds, from the plan pinned above, the same press: voice 8 held keeps the formula's pitch
    // and the line its step, and the run stays in rounds, no value past what the circuit can hold
    // (the growth rule) and every late reader's round, run again with what it read late on time,
    // within what the plan promised it (the shadow rounds)
    let mut heard = Scope { nets: vec!["LINE", "v8.osc", "v8.tone"], at: Vec::new(), t: Vec::new(), v: vec![Vec::new(); 3] };
    heard.begin(&rounds.solver().unknowns);
    run_to(&mut rounds, &mut heard, 0.01);
    for cmd in lowered.index.command(&SimCmd::press(key, true)) {
        rounds.apply(cmd);
    }
    run_to(&mut rounds, &mut heard, 0.03);
    let st = rounds.stats();
    assert!(st.breaches == 0 && st.shadow_acts == 0 && st.chain_acts == 0 && st.round_split.shadows > 0 && rounds.blocks() > 1,
        "in rounds the organ stays in rounds: {} blocks at the end, {} shadow rounds; {:?}; {:?}; {:?}", rounds.blocks(), st.round_split.shadows, st.last_breach, st.last_shadow_act, st.last_chain_act);
    let f = pitch(&heard.trace("v8.tone", 0.012, 0.03));
    let line = heard.step("LINE", 0.015, 0.03, half);
    assert!((f / want - 1.0).abs() < 0.01 && (line / summed - 1.0).abs() < 0.05,
        "in rounds, held, voice 8 out runs at {f:.1} Hz (the formula's {want:.1} Hz within 1 %) and the line steps {line:.3} V (the summer's {summed:.3} V within 5 %)");

    // Logic mode takes every voice's oscillator as a timing cell, calibrated to its switch pair
    // (its pitch pot's wiper on r_t one resistor to the output); the key, hold and gate sections
    // stay analog (a switch, another section's output, a diode on their inputs)
    let (_, cells, analog) = fab_elec_sim::analysis::logic::with_cells(&lowered.circuit);
    let mut taken: Vec<&str> = cells.iter().map(|c| c.input.as_str()).collect();
    taken.sort();
    let voices: Vec<String> = (1..=8).map(|v| format!("v{v}.timing")).collect();
    assert_eq!(taken, voices, "Logic takes the eight oscillators; left analog: {analog:?}");

    // and every pin reads in Logic what it reads in Fast: a pin of a part a cell took (the timing
    // resistor, the pitch pot, the timing capacitor, the section's input) reads the cell, which
    // gives what that part carries, never a device the lowering lacks (a zero)
    let unread = |l: &fab_elec_bench::Lowered| {
        let s = fab_elec_sim::Solver::new(&l.circuit).unwrap_or_else(|e| panic!("the organ's solver: {e:?}"));
        l.plan.pins.iter().flatten().filter(|t| s.find_device(&t.device).is_none_or(|(b, _)| t.terminal as usize >= s.banks[b].terminals()))
            .map(|t| format!("{}:{}", t.device, t.terminal)).collect::<Vec<_>>()
    };
    let mut logic = lowered.clone();
    let (cells, _) = logic.with_cells();
    assert_eq!(cells.len(), 8, "the bench's Logic lowering takes the eight oscillators");
    assert_eq!(unread(&logic), unread(&lowered), "in Logic, pins read devices the lowering lacks");
}

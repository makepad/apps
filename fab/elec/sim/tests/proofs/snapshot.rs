//! Snapshots of a live run (`LiveSim::snapshot`, `restore`): a run brought back to one goes on
//! exactly as it did from there.

use fab_elec_sim::circuit::{Boundary, BoundaryKind, CtlKind};
use fab_elec_sim::cosim::{Bus, CoRun, CoSim, CoSimDevice, CoSimPin, CoState, StepTo, Watch};
use fab_elec_sim::live::{LiveCmd, ValuePlan};
use fab_elec_sim::livesim::{LiveSim, LiveSpec, Replan};
use fab_elec_sim::rounds::{seed_from, Block, Coupling, Helpers, Lag, Planner, Post, RoundPlan};
use fab_elec_sim::{GpioParams, ModelKind, PinEdge, PinMode, PinPull, Record, SimCircuit, SimDiag, Sink, Solver, Source, Wave};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

pub(crate) const SAMPLE: f64 = 1.0 / 96e3;
pub(crate) const PARAMS: GpioParams = GpioParams { r_on_h: 25.0, r_on_l: 25.0, r_pull: 40e3, r_leak: 1e12, t_edge: 5e-9, vih: 0.55, vil: 0.45 };
/// The firmware's PWM period.
const PWM: f64 = 100e-6;
const END: f64 = 4e-3;
pub(crate) const LEAD: f64 = 0.5e-3;
/// When the firmware arms its watch: after the run's first plan of blocks went in, so the
/// handler's pad first moves in rounds.
const ARM: f64 = 1.2e-3;
/// How far a restored run is followed: past a re-plan's wait and lead.
const HORIZON: f64 = 1.5e-3;

/// The control island: a 3.3 V supply through 1 ohm into 10 uF, three pads with their loads, a
/// 1.5 kHz drive on the input pad (from its low peak) and the PWM pad's RC filter.
fn control(c: &mut SimCircuit) {
    c.v("supply", "vs", "0", Source::dc(3.3));
    c.r("rs", "vs", "vdd", 1.0);
    c.c("cvdd", "vdd", "0", 10e-6);
    // the driven pads lowered as inputs, as a processor holds them at reset; the input pad driven
    // low until the firmware releases it
    for (gpio, pad, mode) in [("pa0", "pa", PinMode::Input(PinPull::None)), ("pb0", "pb", PinMode::OutLow), ("pc0", "pc", PinMode::Input(PinPull::None))] {
        c.gpio(gpio, pad, "vdd", "0", PARAMS, mode);
    }
    c.r("rf", "pa", "f", 10e3);
    c.c("cf", "f", "0", 100e-9);
    c.v("drive", "k", "0", Source::wave(Wave::Sin { vo: 1.65, va: 1.65, freq: 1.5e3, td: 0.0, theta: 0.0, phase: -90.0 }));
    c.r("rk", "k", "pb", 1e3);
    c.r("rlc", "pc", "0", 10e3);
    c.c("clc", "pc", "0", 1e-9);
}

/// A diode-clipped 1 kHz voice, biased from the rail through 2.2 kohm.
fn voice(c: &mut SimCircuit) {
    c.model("dm", ModelKind::Diode, &[("cjo", 1e-12)]);
    c.v("tone", "t", "0", Source::wave(Wave::Sin { vo: 0.0, va: 2.0, freq: 1e3, td: 0.0, theta: 0.0, phase: 0.0 }));
    c.r("rt", "t", "v", 1e3);
    c.r("rb", "vdd", "v", 2.2e3);
    c.d("dv", "v", "0", "dm");
    c.c("cv", "v", "0", 100e-9);
}

/// Noise into an RC, with 10 uS of the PWM filter's voltage.
fn hiss(c: &mut SimCircuit) {
    c.i("noise", "0", "n", Source::wave(Wave::Noise { seed: 7, sigma: 1e-5, dt: SAMPLE, base: SAMPLE }));
    c.r("rn", "n", "0", 10e3);
    c.c("cn", "n", "0", 10e-9);
    c.vctl("gf", CtlKind::Vccs, "0", "n", "f", "0", 1e-5);
}

pub(crate) fn rig() -> SimCircuit {
    let mut c = SimCircuit::new();
    control(&mut c);
    voice(&mut c);
    hiss(&mut c);
    c
}

/// The firmware: a PWM on pad 0 (duty stepping 20 % to 80 %), and from `ARM` a watch on pad 1's
/// rise whose handler toggles pad 2. It forks (a copy goes on as it would).
#[derive(Clone, Default)]
pub(crate) struct Pwm {
    released: bool,
    watches: Vec<Watch>,
    gen: u32,
    k: u32,
    start: f64,
    high: bool,
    next: f64,
    toggled: bool,
}

impl CoSim for Pwm {
    fn next(&self) -> f64 {
        self.next
    }
    fn fork(&self) -> Option<Box<dyn CoSim>> {
        Some(Box::new(self.clone()))
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        let t = bus.t();
        if !self.released {
            assert!(bus.write(1, PinMode::Input(PinPull::None)), "the input pad is released");
            self.released = true;
        }
        if self.gen == 0 && t >= ARM {
            self.watches.push(Watch { id: 1, pin: 1, level: 1.65, edge: PinEdge::Rising });
            self.gen = 1;
        }
        if !bus.hits().is_empty() {
            self.toggled = !self.toggled;
            assert!(bus.write(2, if self.toggled { PinMode::OutHigh } else { PinMode::OutLow }), "the handler toggles its pad");
        }
        if t >= self.next {
            if self.high {
                assert!(bus.write(0, PinMode::OutLow), "the PWM pad falls");
                (self.high, self.next) = (false, self.start + PWM);
            } else {
                let duty = 0.2 + 0.1 * f64::from(self.k % 7);
                self.k += 1;
                assert!(bus.write(0, PinMode::OutHigh), "the PWM pad rises");
                (self.start, self.high, self.next) = (t, true, t + duty * PWM);
            }
        }
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

/// One stepper at first (the pads as lowered), then three blocks in the form of the S4 plans:
/// the control island as block 0, taking the voice's draw on the rail a round late; the voice and
/// the hiss read the rail and the filter as block 0 has them in the same round; each re-plan after
/// the first as `recut` says.
#[derive(Clone)]
pub(crate) struct Thirds {
    pub(crate) calls: u32,
    pub(crate) recut: Recut,
}

/// What a re-plan after the first cuts.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Recut {
    /// The same three blocks: the cut the run runs.
    Same,
    /// The other cut by turns: the voice and the hiss in one block, or apart; the control block
    /// is a block of both cuts.
    Join,
    /// The same three blocks named anew: not the cut the run runs (`RoundPlan::same_cut`), but
    /// every block a running one.
    Rename,
}

impl Planner for Thirds {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, _t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        self.calls += 1;
        if self.calls == 1 {
            return Ok(None);
        }
        let joined = self.recut == Recut::Join && self.calls % 2 == 1;
        let at = |net: &str| whole.x[whole.unknown(net).expect("the rig's net") as usize];
        let mut parts = vec![SimCircuit::new(), SimCircuit::new()];
        control(&mut parts[0]);
        voice(&mut parts[1]);
        if !joined {
            parts.push(SimCircuit::new());
        }
        let h = parts.len() - 1;
        hiss(&mut parts[h]);
        let node = parts[0].node("vdd");
        parts[0].devices.boundary.push(Boundary { name: "draw".into(), node, kind: BoundaryKind::Current, v0: -(at("vdd") - at("v")) / 2.2e3 });
        let node = parts[1].node("vdd");
        parts[1].devices.boundary.push(Boundary { name: "rail".into(), node, kind: BoundaryKind::Voltage, v0: at("vdd") });
        let node = parts[h].node("f");
        parts[h].devices.boundary.push(Boundary { name: "filter".into(), node, kind: BoundaryKind::Voltage, v0: at("f") });
        let reads: &[&[&str]] = if joined { &[&[], &["vdd", "f"]] } else { &[&[], &["vdd"], &["f"]] };
        let mut plan = RoundPlan::default();
        let mut solvers = Vec::new();
        for (k, (mut c, read)) in parts.into_iter().zip(reads).enumerate() {
            c.opts = ckt.opts.clone();
            let mut s = Solver::new(&c)?;
            let mut own = seed_from(&mut s, whole);
            own.retain(|&(l, _)| !read.contains(&s.unknowns[l as usize].name.as_str()));
            let name = if self.recut == Recut::Rename && self.calls > 2 { format!("kept {k}") } else { format!("block {k}") };
            plan.blocks.push(Block { name, stage: u32::from(k > 0), own });
            solvers.push(s);
        }
        let of = |k: usize, name: &str| (k as u32, solvers[k].unknown(name).expect("the block solves it"));
        plan.couplings.push(Coupling { from: of(1, "rail#branch"), to: vec![(0, "draw".into())], lag: Lag::Round, cuts: Vec::new(), rho: 0.0 });
        plan.couplings.push(Coupling { from: of(0, "vdd"), to: vec![(1, "rail".into())], lag: Lag::Same, cuts: Vec::new(), rho: 0.0 });
        plan.couplings.push(Coupling { from: of(0, "f"), to: vec![(h as u32, "filter".into())], lag: Lag::Same, cuts: Vec::new(), rho: 0.0 });
        Ok(Some((plan, solvers)))
    }
}

/// How long a plan takes on the late helper: many times the wall time the rig steps a lead in.
const LATE: Duration = Duration::from_millis(20);

/// `Thirds`, but a plan made on the late helper's thread says it has started, takes `LATE` of wall
/// time, as a cold start's plan can, or with a `Gate` says it waits there and goes on when the
/// test lets it, and steps the run whole again: when it goes in changes the run.
#[derive(Clone)]
struct Late {
    thirds: Thirds,
    started: mpsc::Sender<()>,
    gate: Option<Gate>,
}

/// Where a late helper's plan waits for the test: it says so on `at` and goes on at a message on
/// `open`; the helper says on `ran` each time a job it ran is done, the plan back with the run.
#[derive(Clone)]
struct Gate {
    at: mpsc::Sender<()>,
    open: Arc<Mutex<mpsc::Receiver<()>>>,
    ran: mpsc::Sender<()>,
}

impl Planner for Late {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        if thread::current().name() == Some("late helper") {
            let _ = self.started.send(());
            match &self.gate {
                Some(gate) => {
                    let _ = gate.at.send(());
                    let _ = gate.open.lock().unwrap().recv();
                }
                None => thread::sleep(LATE),
            }
            return Ok(None);
        }
        self.thirds.plan(ckt, whole, t)
    }
}

/// A helper core on a long-lived thread whose post returns once the plan it was given has started
/// there (`Late`): the worker meets the plan's time with the plan still in the making.
fn late_helper(started: mpsc::Receiver<()>, ran: Option<mpsc::Sender<()>>) -> Post {
    let (tx, rx) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
    thread::Builder::new()
        .name("late helper".into())
        .spawn(move || {
            for job in rx {
                job();
                if let Some(ran) = &ran {
                    let _ = ran.send(());
                }
            }
        })
        .expect("the helper's thread starts");
    let (tx, started) = (Mutex::new(tx), Mutex::new(started));
    Arc::new(move |job| {
        tx.lock().unwrap().send(job).expect("the helper takes the plan");
        started.lock().unwrap().recv().expect("the helper starts the plan");
    })
}

/// The rig with its firmware and planner: a re-plan falls due 0.5 ms after a pad first leaves its
/// lowered mode and goes in 0.5 ms later; posted, it is made by a post that runs each job as it is
/// posted (in Fast with the packs on the worker: its pipeline would leave stages on a helper past a
/// loop top, and a snapshot is taken between rounds), or with `late` on the late helper; its
/// re-plans as `recut` says (`Thirds`).
fn live(res: f64, posted: bool, late: bool, recut: Recut, gate: Option<Gate>) -> LiveSim {
    let slow = if posted { -1.0 } else { f64::INFINITY };
    let replan = Replan { edit: 1e-3, pins: 0.5e-3, lead: LEAD, slow };
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, max_step: SAMPLE, res, round: 16, trace: false, replan, ..LiveSpec::default() };
    let mut sim = LiveSim::new(rig(), ValuePlan::default(), 1, spec).expect("the rig's operating point converges");
    let pin = |name: &str, gpio: &str, pad: &str| CoSimPin { name: name.into(), gpio: gpio.into(), pad: pad.into(), vss: "0".into(), vdd: "vdd".into(), vih: PARAMS.vih, vil: PARAMS.vil };
    sim.attach(Some(CoSimDevice { dev: Box::new(Pwm::default()), pins: vec![pin("PA0", "pa0", "pa"), pin("PB0", "pb0", "pb"), pin("PC0", "pc0", "pc")] }));
    // in Fast with a helper core the first plan waits for the pads: it cuts the run in three
    let thirds = Thirds { calls: u32::from(res > 0.0 && posted), recut };
    let (started, on_helper) = mpsc::channel();
    if posted {
        let post: Post = if late { late_helper(on_helper, gate.as_ref().map(|g| g.ran.clone())) } else { Arc::new(|job| job()) };
        // its plans and checks on the same cores (`set_checks`), as a host gives its background jobs
        let helpers = Helpers { post, count: usize::from(res == 0.0) };
        sim.set_helpers(Some(helpers.clone()));
        sim.set_checks(Some(helpers), false);
    }
    let planner: Box<dyn Planner> = if late { Box::new(Late { thirds, started, gate }) } else { Box::new(thirds) };
    sim.set_planner(Some(planner)).expect("the first plan keeps one stepper or waits");
    sim
}

fn to_end(sim: &mut LiveSim, record: &mut Record) {
    while sim.t() < END {
        sim.advance(END, None, record).unwrap_or_else(|d| panic!("the rig steps: {d}"));
    }
}

/// Where `got` and `want` from its `from`-th point part, if they do: the first point whose time
/// or any value differs in its bits, or `got` running past `want`'s end (or, `whole`, either
/// ending first).
pub(crate) fn parted(got: &Record, want: &Record, from: usize, whole: bool) -> Option<String> {
    let (at, x) = (&want.at[from..], &want.x[from..]);
    for (k, (t, v)) in got.at.iter().zip(&got.x).enumerate() {
        let Some(&w) = at.get(k) else { return Some(format!("point {k} at {t:e} s past the run's end")) };
        let same = w.to_bits() == t.to_bits() && x[k].len() == v.len() && x[k].iter().zip(v).all(|(a, b)| a.to_bits() == b.to_bits());
        if !same {
            let off = x[k].iter().zip(v).enumerate().find(|(_, (a, b))| a.to_bits() != b.to_bits()).map(|(u, (a, b))| format!(", {} {a:e} against {b:e}", got.names[u]));
            return Some(format!("point {k} at {t:e} s against {w:e} s{}", off.unwrap_or_default()));
        }
    }
    (whole && got.at.len() != at.len()).then(|| format!("{} points against {}", got.at.len(), at.len()))
}

/// A snapshot taken between `advance` calls (a loop top: in rounds, between rounds) brings the
/// run back exactly. On a rig with a diode, a noise source, a PWM pad, a watch whose handler
/// writes a pad, a run on one stepper that a plan moves into three blocks of the S4 form, and a
/// re-plan in rounds when the handler's pad first moves, in Exact (plans posted), in Fast (plans on
/// the worker) and in Fast with a helper core (the first plan waiting for the pads, made on the
/// worker, and the re-plans posted, or made late on a helper core, waited for at their time and
/// stepping the run whole again), and heard with that re-plan back three rounds past its time
/// (`LiveSpec::heard`: it goes in there, and that round is written), the second run made from
/// the heard run's timeline (`LiveSim::follow`), and in Fast with the re-plan a new cut that keeps
/// the control block running (the voice and the hiss joined) or every block (named anew):
/// - every block kept, the run is the straight run of the re-plan that keeps the cut whole, bit for
///   bit: the control block runs on with the firmware's pads, its watch and its warm starts;
/// - passive: a run stepped a loop top at a time with a snapshot at every 11th (so they fall
///   mid-PWM, mid-stretch, around restarts) and in rounds at each (rounds of 16 samples) is the
///   straight run, bit for bit;
/// - each snapshot restored in turn (on one stepper, between rounds, with a plan on its way) goes
///   on as the straight run did from there for 1.5 ms, every point's time and values by bits;
/// - a snapshot stays as it was: the first, restored again last, goes on as it did the first time.
#[test]
fn a_restored_snapshot_goes_on_as_the_run_did() {
    let cases = [
        ("Exact, plans posted", 0.0, true, false, false, Recut::Same),
        ("Fast, plans on the worker", SAMPLE, false, false, false, Recut::Same),
        ("Fast, plans posted", SAMPLE, true, false, false, Recut::Same),
        ("Fast, plans late on a helper", SAMPLE, true, true, false, Recut::Same),
        ("Fast, heard, a plan late on a helper", SAMPLE, true, true, true, Recut::Same),
        ("Fast, plans posted, the re-plan a new cut", SAMPLE, true, false, false, Recut::Join),
        ("Fast, plans posted, the re-plan's blocks named anew", SAMPLE, true, false, false, Recut::Rename),
    ];
    let mut kept_whole = None;
    for (name, res, posted, late, heard, recut) in cases {
        // heard, the late helper's plan waits at a gate the test opens once the run is three
        // rounds past its time, and is back before the run steps on: it goes in there, late
        let (at, at_gate) = mpsc::channel();
        let (open, gate) = mpsc::channel();
        let (ran, done) = mpsc::channel();
        let mut straight = live(res, posted, late, recut, heard.then(|| Gate { at, open: Arc::new(Mutex::new(gate)), ran }));
        let mut want = Record::default();
        want.begin(&straight.solver().unknowns);
        if heard {
            assert!(straight.apply(LiveCmd::SetHeard(true)), "{name}: heard is a command");
            let mut waiting = None;
            while straight.t() < END {
                if at_gate.try_recv().is_ok() {
                    waiting = Some(straight.t());
                }
                if waiting.is_some_and(|w| straight.t() >= w + LEAD + 3.0 * 16.0 * SAMPLE) {
                    // the jobs the helper finished before are behind; the gated one is back with the
                    // run once the helper says it ran, however late the box schedules it
                    while done.try_recv().is_ok() {}
                    open.send(()).expect("the plan waits at the gate");
                    done.recv().expect("the helper runs the gated plan");
                    waiting = None;
                }
                straight.advance(END, Some(Duration::ZERO), &mut want).unwrap_or_else(|d| panic!("{name}: the rig steps: {d}"));
            }
        } else {
            to_end(&mut straight, &mut want);
        }
        let st = straight.stats();
        if heard {
            assert!(st.plan_waits == 0 && st.plans_late > 0, "{name}: the run took the plan late, never waiting ({} waits, {} late)", st.plan_waits, st.plans_late);
        } else {
            assert!(!late || st.plan_waits > 0, "{name}: the run waited for the late helper's plan at its time");
        }
        match recut {
            Recut::Same if res > 0.0 && posted && !late => kept_whole = Some(want.clone()),
            Recut::Rename => {
                let whole = kept_whole.as_ref().expect("the run that keeps its cut whole goes first");
                if let Some(d) = parted(&want, whole, 0, true) {
                    panic!("{name}: every block kept, the run is the one that keeps its cut whole: {d}");
                }
            }
            _ => {}
        }
        let (timeline, go_ins) = (straight.timeline().to_vec(), straight.go_ins().to_vec());

        let mut sim = live(res, posted, late, recut, None);
        if heard {
            sim.apply(LiveCmd::SetHeard(true));
            sim.follow(&timeline, &go_ins, &[], &[], None);
        }
        let mut got = Record::default();
        got.begin(&sim.solver().unknowns);
        let mut snaps = Vec::new();
        let mut top = 0;
        while sim.t() < END {
            if top % 11 == 0 || sim.blocks() > 1 {
                snaps.push((got.at.len(), sim.snapshot().expect("the firmware forks")));
            }
            top += 1;
            sim.advance(END, Some(Duration::ZERO), &mut got).unwrap_or_else(|d| panic!("{name}: the rig steps: {d}"));
        }
        let st = sim.stats();
        // the late helper's re-plan steps the run whole
        assert_eq!(sim.blocks(), if late { 1 } else if recut == Recut::Join { 2 } else { 3 }, "{name}: the run ends in rounds, or whole after the late re-plan");
        let kept = match recut {
            Recut::Same => 0,
            Recut::Join => 1,
            Recut::Rename => 3,
        };
        assert_eq!(st.blocks_kept, kept, "{name}: the blocks a re-plan's new cut keeps running");
        // in Fast with a helper core the first plan waits for the pads: their plan is that one
        let plans = if res > 0.0 && posted { 2 } else { 3 };
        assert!(st.plans >= plans, "{name}: the run planned at its start (once its pads moved, in Fast with a helper core) and again when pads moved ({} plans)", st.plans);
        if let Some(d) = parted(&got, &want, 0, true) {
            panic!("{name}: a run taking snapshots a loop top at a time is the straight run: {d}");
        }
        let flight = snaps.iter().filter(|(_, s)| s.t() > st.last_plan - LEAD && s.t() < st.last_plan).count();
        let rounds = snaps.iter().filter(|(_, s)| s.t() > st.last_plan).count();
        assert!(flight > 0 && rounds > 0, "{name}: of {} snapshots, {flight} hold a plan on its way and {rounds} are between rounds", snaps.len());
        eprintln!("{name}: {} points, {} loop tops, {} snapshots ({flight} with a plan on its way, {rounds} between rounds after it), {} plans", want.at.len(), top, snaps.len(), st.plans);
        for (k, (from, snap)) in snaps.iter().chain(snaps.first()).enumerate() {
            sim.restore(snap);
            let mut again = Record::default();
            again.begin(&sim.solver().unknowns);
            let until = (snap.t() + HORIZON).min(END);
            while sim.t() < until {
                sim.advance(until, None, &mut again).unwrap_or_else(|d| panic!("{name}: the restored rig steps: {d}"));
            }
            if let Some(d) = parted(&again, &want, *from, false) {
                panic!("{name}: restored at snapshot {k} ({:e} s), the run goes on as it did: {d}", snap.t());
            }
        }
    }
}

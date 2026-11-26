//! Fast's rounds as a pipeline (`LiveSpec::pipeline`): the stages after block 0 run their rounds
//! behind it on the helpers, a read back from stage s by stage e < s `s - e + 1` rounds late, the
//! same lags on the worker alone; and edge restarts (`LiveSpec::edge_restarts`): a block is fed
//! another block's switching instants split onto its grid, and lands on none.

use fab_elec_sim::circuit::{Boundary, BoundaryKind, CtlKind};
use fab_elec_sim::cosim::{Bus, CoRun, CoSim, CoSimDevice, CoSimPin, CoState, StepTo, Watch};
use fab_elec_sim::live::{Latest, LiveCmd, Terminal, ValuePlan};
use fab_elec_sim::livesim::{run_live, LiveSim, LiveSpec, Replan, Snapshots};
use fab_elec_sim::analysis::op::operating_point;
use fab_elec_sim::analysis::partition::Land;
use fab_elec_sim::rounds::{seed_from, Block, Chain as Tree, Coupling, Helpers, HolderPort, Lag, Planner, Post, RoundPlan};
use fab_elec_sim::stamp::UnknownInfo;
use fab_elec_sim::{GpioParams, ModelKind, PinMode, PinPull, Record, SimCircuit, SimDiag, Sink, SinkState, Solver, Source, Wave};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

const SAMPLE: f64 = 1.0 / 96e3;
/// The round: 32 samples.
const ROUND: f64 = 32.0 * SAMPLE;
const EDIT: f64 = 4e-3;
const STOP: f64 = 10e-3;
const PARAMS: GpioParams = GpioParams { r_on_h: 25.0, r_on_l: 25.0, r_pull: 40e3, r_leak: 1e12, t_edge: 5e-9, vih: 0.55, vil: 0.45 };

/// Block 0: a toggled GPIO pad on its supply and a Schmitt-trigger RC oscillator (timing node
/// `i0`, 390 k onto 10 nF), with two echo nodes `e1` and `e2`, each 10 k and 1 nF.
fn control() -> SimCircuit {
    let mut c = SimCircuit::new();
    c.model("lo", ModelKind::Sw, &[("vt", 2.35), ("vh", 0.5), ("ron", 50.0), ("roff", 1e9)]);
    c.model("hi", ModelKind::Sw, &[("vt", -2.35), ("vh", 0.5), ("ron", 50.0), ("roff", 1e9)]);
    c.v("supply", "vdd", "0", Source::dc(3.3));
    // lowered as an input, as a processor holds its pads at reset: its first write drives it
    c.gpio("pa0", "pad", "vdd", "0", PARAMS, PinMode::Input(PinPull::None));
    c.r("rload", "pad", "0", 10e3);
    c.c("cpad", "pad", "0", 1e-9);
    c.v("vs0", "v0", "0", Source::wave(Wave::Pulse { v1: 0.0, v2: 5.0, td: 0.0, tr: 10e-6, tf: 10e-6, pw: 1.0, per: 2.0 }));
    c.sw("slo0", "o0", "0", "i0", "0", "lo", false);
    c.sw("shi0", "v0", "o0", "0", "i0", "hi", true);
    c.r("rf0", "o0", "i0", 390e3);
    c.c("ct0", "i0", "0", 10e-9);
    c.c("co0", "o0", "0", 10e-12);
    for e in ["e1", "e2"] {
        c.r(&format!("r{e}"), e, "0", 10e3);
        c.c(&format!("c{e}"), e, "0", 1e-9);
    }
    c
}

/// A listener at `out`: a VCCS reading `src` (without loading it) into 10 k and 1 nF, unity gain
/// behind 10 us.
fn listener(c: &mut SimCircuit, name: &str, src: &str, out: &str) {
    c.vctl(&format!("g{name}"), CtlKind::Vccs, "0", out, src, "0", 1e-4);
    c.r(&format!("r{name}"), out, "0", 10e3);
    c.c(&format!("c{name}"), out, "0", 1e-9);
}

/// The whole chain: block 0, a listener `l` on the timing node, a listener `m` on `l`, and the
/// echoes: `l` drives `e1` and `m` drives `e2` (1e-4 S each, into block 0's loads).
fn chain() -> SimCircuit {
    let mut c = control();
    listener(&mut c, "l", "i0", "l");
    listener(&mut c, "m", "l", "m");
    c.vctl("ge1", CtlKind::Vccs, "0", "e1", "l", "0", 1e-4);
    c.vctl("ge2", CtlKind::Vccs, "0", "e2", "m", "0", 1e-4);
    c
}

/// `chain` in three blocks, one a stage: block 0 the control block, taking the echoes' drive
/// currents late (a round late in plain rounds, two and three in the pipeline); block 1 the
/// listener `l`, reading `i0` and `e1` in the same round; block 2 the listener `m`, reading `l`
/// and `e2` in the same round. A plan from a snapshot after `EDIT` cuts the chain in two: block 0,
/// and both listeners in one block at stage 1.
#[derive(Clone)]
struct Chain;

impl Planner for Chain {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        let held = |c: &mut SimCircuit, name: &str, node: &str| {
            let v0 = whole.x[whole.unknown(node).expect("the cut node is in the whole circuit") as usize];
            let node = c.node(node);
            c.devices.boundary.push(Boundary { name: name.into(), node, kind: BoundaryKind::Voltage, v0 });
        };
        let mut b0 = control();
        for (e, src) in [("e1", "l"), ("e2", "m")] {
            let drive = 1e-4 * whole.x[whole.unknown(src).expect("the listener's node") as usize];
            let node = b0.node(e);
            b0.devices.boundary.push(Boundary { name: format!("i{e}"), node, kind: BoundaryKind::Current, v0: drive });
        }
        let mut b1 = SimCircuit::new();
        held(&mut b1, "bi", "i0");
        listener(&mut b1, "l", "i0", "l");
        held(&mut b1, "be1", "e1");
        b1.vctl("ge1", CtlKind::Vccs, "0", "e1", "l", "0", 1e-4);
        let two = t > EDIT;
        let mut b2 = SimCircuit::new();
        if !two {
            held(&mut b2, "bl", "l");
        }
        listener(if two { &mut b1 } else { &mut b2 }, "m", "l", "m");
        held(if two { &mut b1 } else { &mut b2 }, "be2", "e2");
        (if two { &mut b1 } else { &mut b2 }).vctl("ge2", CtlKind::Vccs, "0", "e2", "m", "0", 1e-4);
        let blocks: Vec<(SimCircuit, &[&str])> = if two { vec![(b0, &[][..]), (b1, &["i0", "e1", "e2"][..])] } else { vec![(b0, &[][..]), (b1, &["i0", "e1"][..]), (b2, &["l", "e2"][..])] };
        let mut plan = RoundPlan::default();
        let mut solvers = Vec::new();
        for (k, (mut c, cut)) in blocks.into_iter().enumerate() {
            c.opts = ckt.opts.clone();
            let mut s = Solver::new(&c)?;
            let mut own = seed_from(&mut s, whole);
            own.retain(|&(l, _)| !cut.contains(&s.unknowns[l as usize].name.as_str()));
            plan.blocks.push(Block { name: format!("block {k}"), stage: k as u32, own });
            solvers.push(s);
        }
        let at = |k: usize, name: &str| solvers[k].unknown(name).unwrap_or_else(|| panic!("block {k} solves {name}"));
        let same = |from: (u32, u32), to: (u32, &str)| Coupling { from, to: vec![(to.0, to.1.into())], lag: Lag::Same, cuts: Vec::new(), rho: 0.0 };
        let late = |from: (u32, u32), to: (u32, &str)| Coupling { from, to: vec![(to.0, to.1.into())], lag: Lag::Round, cuts: Vec::new(), rho: 0.0 };
        let m = if two { 1 } else { 2 };
        plan.couplings.push(same((0, at(0, "i0")), (1, "bi")));
        plan.couplings.push(same((0, at(0, "e1")), (1, "be1")));
        if !two {
            plan.couplings.push(same((1, at(1, "l")), (2, "bl")));
        }
        plan.couplings.push(same((0, at(0, "e2")), (m, "be2")));
        plan.couplings.push(late((1, at(1, "be1#branch")), (0, "ie1")));
        plan.couplings.push(late((m, at(m as usize, "be2#branch")), (0, "ie2")));
        Ok(Some((plan, solvers)))
    }
}

/// `Chain` with block 2 unpipelined (`RoundPlan::unpipelined`): its round runs right after block
/// 1's, in block 1's place in the pipeline, so block 0 reads `e2` two rounds late, as it reads `e1`.
#[derive(Clone)]
struct Joined;

impl Planner for Joined {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        Ok(Chain.plan(ckt, whole, t)?.map(|(mut plan, solvers)| {
            if plan.blocks.len() == 3 {
                plan.unpipelined = vec![2];
            }
            (plan, solvers)
        }))
    }
}

/// `Chain` with block 2 a leaf: block 0 takes no drive back from it (its echo `e2` driven as the
/// plan found it), so an edit of block 2 reaches no loop of the map and the run stays in rounds
/// through it, block 2 behind block 0.
#[derive(Clone)]
struct Leaf;

impl Planner for Leaf {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        Ok(Chain.plan(ckt, whole, t)?.map(|(mut plan, solvers)| {
            if plan.blocks.len() == 3 {
                plan.couplings.retain(|c| c.from.0 != 2);
            }
            (plan, solvers)
        }))
    }
}

/// `Chain` with a chain shadow's tree on block 0's `i0` into block 1 (`RoundPlan::chains`) until it
/// learns that landing (`Planner::land_chain`); its plans from then on land that import
/// (`RoundPlan::lands`) and keep no tree: the same cut, its imports landing otherwise.
#[derive(Clone, Default)]
struct Landing {
    landed: bool,
}

impl Planner for Landing {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        Ok(Chain.plan(ckt, whole, t)?.map(|(mut plan, solvers)| {
            if self.landed {
                plan.lands = vec![(0, 1)];
                plan.land_steps = vec![0.0];
            } else {
                plan.chains = vec![Tree { born: 0, name: "i0".into(), roots: vec![1], pairs: vec![(0, 1)], labels: vec!["i0".into()], members: vec![1], cut_late: 0 }];
            }
            (plan, solvers)
        }))
    }

    fn land_chain(&mut self, land: &Land) {
        self.landed |= land.born == "i0";
    }
}

/// `fed_back`'s load on the timing node: block 1 draws this many siemens of its control out of
/// `i0`. Weak, so the shadow rounds accept the read back (at 1 uS a late read moves the
/// oscillator's edges across a round's end, and the shadow refuses the cut on the crossing count).
const FED: f64 = 2e-7;
/// Where `fed_back`'s runs stop: a few of its periods past the plan after `EDIT`.
const FED_STOP: f64 = 16e-3;

/// What `fed_back`'s block 1 draws from the timing node, and so what a new cut can carry over.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Draw {
    /// `ge1` draws `FED` of `l`, in block 1 under either cut: the old block's window has the
    /// same devices behind it, and the new cut takes its history.
    Follows,
    /// `gx` draws `FED` of a 2.5 V source `vb`, both in block 0 until the new cut moves them into
    /// block 1: the old window behind `bi` has fewer devices than the new one, and the new cut
    /// seeds it from its devices at the go-in.
    Steady,
    /// `cb`, 50 pF from `i0`, in block 1 under either cut: its draw is its charge's rate, which
    /// only the timing node's rate gives.
    Charge,
}

/// Whether `fed_back`'s block 1 stands beside `i0`'s holder through a holder port (`held_port`),
/// and so what a new cut can carry of its memory.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Port {
    Bare,
    /// The devices behind the port are the same under either cut: the new port takes the old
    /// one's memory.
    Same,
    /// With `Draw::Steady`, whose devices the new cut moves into block 1: the devices behind the
    /// new port are more than the old one's, and its memory starts at its equilibrium with what
    /// it reads at the go-in. `ge1` sits in block 0, reading `l` a round late, so what block 1
    /// hears of `i0` through its port reaches the oscillator.
    Moved,
}

/// `Port`'s holder port in block 1: `i0`'s holder as a fit sees it, 390 k onto 10 nF, with 1 mH
/// in series.
fn held_port() -> HolderPort {
    HolderPort { block: 1, name: "i0#port0".into(), rb: 390e3, tau: 3.9e-3, l: 1e-3, ..HolderPort::default() }
}

/// Block 0, a listener `l` on the timing node, a listener `m` on `l` that drives nothing, and the
/// draw out of `i0` (with `Port::Moved`, `ge1`'s too), so the oscillator's timing hangs on a
/// current block 0 takes back late.
fn fed_back(draw: Draw, port: Port) -> SimCircuit {
    let mut c = control();
    listener(&mut c, "l", "i0", "l");
    listener(&mut c, "m", "l", "m");
    drawn(&mut c, draw);
    if port == Port::Moved {
        drawn(&mut c, Draw::Follows);
    }
    c
}

/// `draw`'s devices in `c`.
fn drawn(c: &mut SimCircuit, draw: Draw) {
    match draw {
        Draw::Follows => {
            c.vctl("ge1", CtlKind::Vccs, "i0", "0", "l", "0", FED);
        }
        Draw::Steady => {
            c.v("vb", "vb", "0", Source::dc(2.5));
            c.vctl("gx", CtlKind::Vccs, "i0", "0", "vb", "0", FED);
        }
        Draw::Charge => {
            c.c("cb", "i0", "0", 50e-12);
        }
    }
}

/// `fed_back` in three blocks, one a stage: block 0 the control block, taking block 1's draw from
/// `i0` back late at `ii0`; block 1 `l`, reading `i0` in the same round; block 2 `m`, reading `l`
/// in the same round. Block 2 is a leaf: an edit of it reaches no loop, so the run stays in rounds
/// through it (an edit on a loop, with this plan's no proof, takes the run out of rounds and back
/// in fresh, which hides a go-in). A plan from a snapshot after `EDIT` puts `m` in block 1 beside
/// `l` (and `Draw::Steady`'s devices), unless the cut is `kept`: a new block 1 at stage 1 still,
/// whose draw block 0 reads a round or two back. With a port, the new cut's block 1 stands at an
/// operating point at the plan's instant, as a planner that measured it leaves it: its port not
/// yet fed, so its memory holds the whole draw.
#[derive(Clone)]
struct FedBack {
    kept: bool,
    draw: Draw,
    port: Port,
}

impl Planner for FedBack {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        let v = |node: &str| whole.x[whole.unknown(node).expect("the cut node is in the whole circuit") as usize];
        let held = |c: &mut SimCircuit, name: &str, node: &str| {
            let (v0, node) = (v(node), c.node(node));
            c.devices.boundary.push(Boundary { name: name.into(), node, kind: BoundaryKind::Voltage, v0 });
        };
        let two = !self.kept && t > EDIT;
        // `Draw::Steady`'s devices: in block 0 under the old cut, in block 1 under the new
        let steady_in_1 = two && self.draw == Draw::Steady;
        let mut b0 = control();
        if self.draw == Draw::Steady && !steady_in_1 {
            drawn(&mut b0, Draw::Steady);
        }
        if self.port == Port::Moved {
            held(&mut b0, "bl0", "l");
            drawn(&mut b0, Draw::Follows);
        }
        let drive = match self.draw {
            Draw::Follows => -FED * v("l"),
            Draw::Steady if steady_in_1 => -FED * v("vb"),
            Draw::Steady | Draw::Charge => 0.0,
        };
        let node = b0.node("i0");
        b0.devices.boundary.push(Boundary { name: "ii0".into(), node, kind: BoundaryKind::Current, v0: drive });
        let mut b1 = SimCircuit::new();
        held(&mut b1, "bi", "i0");
        if self.port != Port::Bare {
            let node = b1.node("i0");
            b1.devices.boundary.push(Boundary { name: held_port().name, node, kind: BoundaryKind::Port { of: 0, second: false }, v0: 0.0 });
        }
        listener(&mut b1, "l", "i0", "l");
        if matches!(self.draw, Draw::Follows | Draw::Charge) || steady_in_1 {
            drawn(&mut b1, self.draw);
        }
        let mut b2 = SimCircuit::new();
        if two {
            listener(&mut b1, "m", "l", "m");
        } else {
            held(&mut b2, "bl", "l");
            listener(&mut b2, "m", "l", "m");
        }
        let cut0: &[&str] = if self.port == Port::Moved { &["l"] } else { &[] };
        let blocks: Vec<(SimCircuit, &[&str])> = if two { vec![(b0, cut0), (b1, &["i0"][..])] } else { vec![(b0, cut0), (b1, &["i0"][..]), (b2, &["l"][..])] };
        let mut plan = RoundPlan::default();
        let mut solvers = Vec::new();
        for (k, (mut c, cut)) in blocks.into_iter().enumerate() {
            c.opts = ckt.opts.clone();
            let mut s = Solver::new(&c)?;
            let mut own = seed_from(&mut s, whole);
            own.retain(|&(l, _)| !cut.contains(&s.unknowns[l as usize].name.as_str()));
            if k == 1 && self.port != Port::Bare {
                let p = held_port();
                for (key, v) in [("r", p.r), ("rb", p.rb), ("tau", p.tau), ("l", p.l)] {
                    assert!(s.set_param(&p.name, key, v), "the port takes its {key}");
                }
                if two {
                    operating_point(&mut s, false)?;
                }
            }
            plan.blocks.push(Block { name: format!("block {k}"), stage: k as u32, own });
            solvers.push(s);
        }
        let at = |k: usize, name: &str| solvers[k].unknown(name).unwrap_or_else(|| panic!("block {k} solves {name}"));
        let coupling = |from: (u32, u32), to: (u32, &str), lag: Lag| Coupling { from, to: vec![(to.0, to.1.into())], lag, cuts: Vec::new(), rho: 0.0 };
        plan.couplings.push(coupling((0, at(0, "i0")), (1, "bi"), Lag::Same));
        if !two {
            plan.couplings.push(coupling((1, at(1, "l")), (2, "bl"), Lag::Same));
        }
        if self.port == Port::Moved {
            plan.couplings.push(coupling((1, at(1, "l")), (0, "bl0"), Lag::Round));
        }
        plan.couplings.push(coupling((1, at(1, "bi#branch")), (0, "ii0"), Lag::Round));
        if self.port != Port::Bare {
            plan.ports.push(HolderPort { coupling: plan.couplings.len() as u32 - 1, ..held_port() });
        }
        Ok(Some((plan, solvers)))
    }
}

/// Toggles pin 0 every 50 us from high at t = 0; armed (`LiveCmd::CoStep`), its next run stops.
#[derive(Clone)]
struct Toggle {
    k: u64,
    armed: bool,
}

impl CoSim for Toggle {
    fn next(&self) -> f64 {
        self.k as f64 * 50e-6
    }
    // a process that forks, as firmware does: a run reading late can go back
    fn fork(&self) -> Option<Box<dyn CoSim>> {
        Some(Box::new(self.clone()))
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        assert!(bus.write(0, if self.k % 2 == 0 { PinMode::OutHigh } else { PinMode::OutLow }), "the toggle writes its pin");
        self.k += 1;
        Ok(if std::mem::take(&mut self.armed) { CoRun::Stopped } else { CoRun::Go })
    }
    fn watches(&self) -> &[Watch] {
        &[]
    }
    fn watch_gen(&self) -> u32 {
        0
    }
    fn arm(&mut self, _t: f64, to: Option<StepTo>) -> bool {
        self.armed = to.is_some();
        true
    }
    fn state(&mut self, _t: f64) -> Option<CoState> {
        None
    }
}

fn toggle() -> CoSimDevice {
    let pin = CoSimPin { name: "PA0".into(), gpio: "pa0".into(), pad: "pad".into(), vss: "0".into(), vdd: "vdd".into(), vih: PARAMS.vih, vil: PARAMS.vil };
    CoSimDevice { dev: Box::new(Toggle { k: 0, armed: false }), pins: vec![pin] }
}

/// Long-lived helper threads fed by a channel, as the host's task pool is.
fn pool(threads: usize) -> Helpers {
    let (tx, rx) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..threads {
        let rx = rx.clone();
        thread::spawn(move || loop {
            let job = rx.lock().unwrap().recv();
            match job {
                Ok(job) => job(),
                Err(_) => return,
            }
        });
    }
    let tx = Mutex::new(tx);
    let post: Post = Arc::new(move |job| {
        let _ = tx.lock().unwrap().send(job);
    });
    Helpers { post, count: threads }
}

/// An unpaced run's spec at resolution `res` (0 Exact), the pipeline and edge restarts as given;
/// plans made on the worker (`Replan::slow`), so a plan's time never depends on a helper's.
fn spec(res: f64, pipeline: bool, edge_restarts: bool) -> LiveSpec {
    let replan = Replan { slow: f64::INFINITY, ..Replan::default() };
    LiveSpec { rate: f64::INFINITY, playing: false, max_step: SAMPLE, res, trace: false, replan, pipeline, edge_restarts, ..LiveSpec::default() }
}

/// What a run does at `EDIT`.
#[derive(Clone, Copy, PartialEq)]
enum Edit {
    None,
    /// `rl` (block 1's listener) halves on the timeline (`LiveCmd::At`).
    Timed,
    /// `rload` (block 0's pad load) halves on the timeline.
    Ctl,
    /// The run returns from an advance with a zero budget just short of `EDIT` (its stages
    /// behind block 0) and `rl` halves there.
    Behind,
    /// As `Behind`, the firmware armed to stop at its next run instead (`LiveCmd::CoStep`): it
    /// stops inside block 0's round, the stages behind it.
    Stop,
    /// As `Behind`, `rload` halved instead (block 0 alone); `settled`, a `LiveCmd::SetRate` at the
    /// run's own rate just before it, which catches the stages up first.
    CtlBehind { settled: bool },
}

/// The chain in the rounds of `planner` (or whole without one), toggled, with `helpers`; `edit`
/// at `EDIT`. Each advance has a wall budget it never reaches, so only the stop (and the toggle's
/// writes, each a publication) ends one: the stages stay behind block 0 between advances. Returns
/// the record and the run.
fn run(planner: Option<Box<dyn Planner>>, spec: LiveSpec, helpers: Option<Helpers>, shuffle: Option<u64>, edit: Edit) -> (Record, LiveSim) {
    let c = chain();
    // every net valued: the interval's means (`means`) cover the whole chain
    let plan = ValuePlan { nets: c.nodes.iter().map(|n| n.name.clone()).collect(), ..ValuePlan::default() };
    let mut sim = LiveSim::new(c, plan, 1, spec).expect("the operating point converges");
    sim.attach(Some(toggle()));
    // the planner before the helpers: the first plan is made and goes in now (`first_plan`), at
    // the same instant on any helpers
    sim.set_planner(planner).expect("the plan installs");
    sim.set_helpers(helpers);
    sim.set_pack_shuffle(shuffle);
    let halve = || LiveCmd::SetParam { generation: 1, device: "rl".into(), key: "value".into(), value: 5e3 };
    let load = || LiveCmd::SetParam { generation: 1, device: "rload".into(), key: "value".into(), value: 5e3 };
    if edit == Edit::Timed {
        assert!(sim.apply(LiveCmd::At(EDIT, Box::new(halve()))), "the timed edit keeps the run");
    }
    if edit == Edit::Ctl {
        assert!(sim.apply(LiveCmd::At(EDIT, Box::new(load()))), "the timed edit keeps the run");
    }
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    let to = |sim: &mut LiveSim, record: &mut Record, at: f64| {
        while sim.t() < at {
            sim.advance(at, Some(Duration::MAX), record).unwrap_or_else(|d| panic!("the chain steps: {d}"));
        }
    };
    if matches!(edit, Edit::Behind | Edit::Stop | Edit::CtlBehind { .. }) {
        to(&mut sim, &mut record, EDIT - 3.5 * ROUND);
        // one round at a time, each advance ending with the stages where they stand
        while sim.t() < EDIT - 0.5 * ROUND {
            sim.advance(EDIT, Some(Duration::ZERO), &mut record).unwrap_or_else(|d| panic!("the chain steps: {d}"));
        }
        if edit == (Edit::CtlBehind { settled: true }) {
            // the rate `spec` runs at: nothing changes but the stages, caught up
            assert!(sim.apply(LiveCmd::SetRate(f64::INFINITY)), "the rate keeps the run");
        }
        let cmd = match edit {
            Edit::Stop => LiveCmd::CoStep(StepTo::Statement),
            Edit::CtlBehind { .. } => load(),
            _ => halve(),
        };
        assert!(sim.apply(cmd), "the command keeps the run");
    }
    to(&mut sim, &mut record, STOP);
    (record, sim)
}

/// `v` over the times `at`, linear between them, at `t`.
fn lerp(at: &[f64], v: &[f64], t: f64) -> f64 {
    let n = at.partition_point(|&a| a < t).clamp(1, at.len() - 1);
    v[n - 1] + (v[n] - v[n - 1]) * (t - at[n - 1]) / (at[n] - at[n - 1])
}

/// The largest difference of `name` in `run` from `name` in `truth` `lag` earlier, on a grid of an
/// eighth of a sample from `from` to the stop.
fn deviation(run: &Record, truth: &Record, name: &str, lag: f64, from: f64) -> f64 {
    let (r, w) = (run.v(name), truth.v(name));
    let n = ((STOP - from) / (SAMPLE / 8.0)) as usize;
    (0..=n).map(|k| from + k as f64 * SAMPLE / 8.0).map(|t| (lerp(&run.at, &r, t) - lerp(&truth.at, &w, t - lag)).abs()).fold(0.0, f64::max)
}

fn same(a: &Record, b: &Record) -> bool {
    a.at.len() == b.at.len()
        && a.at.iter().zip(&b.at).all(|(x, y)| x.to_bits() == y.to_bits())
        && a.x.iter().zip(&b.x).all(|(p, q)| p.len() == q.len() && p.iter().zip(q).all(|(x, y)| x.to_bits() == y.to_bits()))
}

/// The interval's means and extremes of every net over the run, bit for bit (`LiveSim::values`).
fn means(sim: &mut LiveSim) -> (f64, Vec<u32>) {
    let v = sim.values(1.0);
    (v.since, v.net_mean.iter().chain(&v.net_min).chain(&v.net_max).map(|m| m.to_bits()).collect())
}

/// The chain in Fast at the live settings, the pipeline and edge restarts on (the defaults):
/// - the run is the same, bit for bit, on the worker alone, on one helper, on two, and packed at
///   random: the helpers run the stages behind block 0 (packs reach them), and what a block is fed
///   is fixed by its round and its lags, never by how far another has run;
/// - so are the interval's means of every net, a helper taking each round's share of them
///   (`IntervalJob`);
/// - its trace holds each point's net values as the sinks got them, put in a round's rows at a
///   time (`TraceStore::push_rows`);
/// - the echoes are as late as the pipeline says: against a fine run of the whole chain, `e1`
///   (block 1's drive, read back by block 0) follows it best read two rounds back, and `e2`
///   (block 2's) three, within three times the whole chain's own error (and 0.1 mV); with the
///   pipeline off both are a round late, as in plain rounds;
/// - block 2 unpipelined (`Joined`), it runs its round right after block 1's: the run is the same,
///   bit for bit, on the worker alone and on two helpers, packed in order or at random, and `e2`
///   is read two rounds late, as `e1` is;
/// - block 1 and block 2 are fed block 0's switching instants split onto their grids and land on
///   none of them (no `Restarts::corner_edge`, under a hundredth of the corner landings there are
///   with edge restarts off, when they land on each).
#[test]
fn the_pipeline_runs_the_same_on_any_helpers_with_its_lags() {
    let chain_plan = || Some(Box::new(Chain) as Box<dyn Planner>);
    let fast = LiveSpec { trace: true, ..spec(SAMPLE, true, true) };
    let (base, mut base_sim) = run(chain_plan(), fast.clone(), None, None, Edit::None);
    assert_eq!(base_sim.blocks(), 3, "the run steps three blocks");
    let base_means = means(&mut base_sim);
    assert!(base_means.1.len() >= 3 * 4, "every net of the chain has a mean, a minimum and a maximum");
    for (name, helpers, shuffle) in [("one helper", 1, None), ("two helpers", 2, None), ("two helpers, packed at random", 2, Some(7))] {
        let (record, mut sim) = run(chain_plan(), fast.clone(), Some(pool(helpers)), shuffle, Edit::None);
        let st = sim.stats();
        eprintln!("{name}: {} rounds, {} packs on a helper, {} taken back by the worker, {} flushes", st.rounds, st.packs_helped, st.packs_claimed, st.round_split.flushes);
        assert!(st.packs_helped + st.packs_claimed > 0, "{name}: the stages are posted to the helpers");
        assert!(same(&record, &base), "{name}: the same points and values as on the worker alone");
        assert!(means(&mut sim) == base_means, "{name}: the same interval means as on the worker alone, a helper taking each round's share of them");
        let d = sim.take_delta().expect("the traced run has points pending");
        assert!(d.time[..] == record.at[..], "{name}: the trace holds each point the sinks got ({} against {})", d.time.len(), record.at.len());
        let nets: Vec<String> = chain().nodes.iter().map(|n| n.name.clone()).collect();
        for (k, net) in nets.iter().enumerate().filter(|(_, n)| sim.solver().unknown(n).is_some()) {
            let want: Vec<f32> = record.v(net).iter().map(|&v| v as f32).collect();
            assert!(d.values[k][..] == want[..], "{name}: the trace holds {net} at each point as the sinks got it");
        }
    }
    // a host that must keep up names the rows its views show (`LiveSpec::fine`): a round's rows
    // put the named net's points alone, the others none, and the run is the same
    let nets: Vec<String> = chain().nodes.iter().map(|n| n.name.clone()).collect();
    let shown = nets.iter().position(|n| base_sim.solver().unknown(n).is_some()).expect("a net of the chain is an unknown");
    let mut mask = vec![false; shown + 1];
    mask[shown] = true;
    let (record, mut sim) = run(chain_plan(), LiveSpec { fine: Some(Arc::from(mask)), ..fast.clone() }, Some(pool(2)), None, Edit::None);
    assert!(same(&record, &base), "naming the rows a view shows changes no point");
    let d = sim.take_delta().expect("the traced run has points pending");
    let want: Vec<f32> = record.v(&nets[shown]).iter().map(|&v| v as f32).collect();
    assert!(d.time[..] == record.at[..] && d.values[shown][..] == want[..], "the trace holds the named net {} at each point as the sinks got it", nets[shown]);
    assert!(d.values.iter().enumerate().all(|(k, v)| k == shown || v.is_empty()), "and no point of another row");
    let mut c = chain();
    c.opts.reltol = 1e-5;
    let truth = {
        let fine = LiveSpec { max_step: SAMPLE / 16.0, ..spec(0.0, false, false) };
        let mut sim = LiveSim::new(c, ValuePlan::default(), 1, fine).expect("the operating point converges");
        sim.attach(Some(toggle()));
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        while sim.t() < STOP {
            sim.advance(STOP, None, &mut record).unwrap_or_else(|d| panic!("the fine chain steps: {d}"));
        }
        record
    };
    let joined_plan = || Some(Box::new(Joined) as Box<dyn Planner>);
    let (joined, _) = run(joined_plan(), fast.clone(), None, None, Edit::None);
    for (name, shuffle) in [("two helpers", None), ("two helpers, packed at random", Some(7))] {
        let (record, sim) = run(joined_plan(), fast.clone(), Some(pool(2)), shuffle, Edit::None);
        let st = sim.stats();
        assert!(st.packs_helped + st.packs_claimed > 0, "block 2 unpipelined, {name}: block 1 is posted to the helpers");
        assert!(same(&record, &joined), "block 2 unpipelined, {name}: the same points and values as on the worker alone");
    }
    let (whole, _) = run(None, fast.clone(), None, None, Edit::None);
    let (plain, plain_sim) = run(chain_plan(), spec(SAMPLE, false, false), None, None, Edit::None);
    for (mode, record, lags) in [("the pipeline", &base, [2, 3]), ("block 2 unpipelined", &joined, [2, 2]), ("plain rounds", &plain, [1, 1])] {
        for (name, want) in [("e1", lags[0]), ("e2", lags[1])] {
            let own = deviation(&whole, &truth, name, 0.0, 1e-3);
            let off: Vec<f64> = (0..=4).map(|k| deviation(record, &truth, name, k as f64 * ROUND, 1e-3)).collect();
            eprintln!("{mode}: {name} from the fine run read 0..4 rounds back: {off:.3?} V; whole {own:e} V");
            let best = (0..off.len()).min_by(|&a, &b| off[a].total_cmp(&off[b])).expect("lags");
            assert_eq!(best, want, "{mode}: {name} is {want} rounds late: {off:.3?} V read 0..4 rounds back");
            assert!(off[want] <= 3.0 * own + 1e-4, "{mode}: {name} read {want} rounds back follows the fine run within three times the whole chain's error (and 0.1 mV): {:e} V, whole {own:e} V", off[want]);
        }
    }
    let (on, off) = (base_sim.restarts(), plain_sim.restarts());
    eprintln!("edge restarts: {} of {} corner landings on, {} of {} off", on.corner_edge, on.corner, off.corner_edge, off.corner);
    assert!(on.corner_edge == 0 && 100 * on.corner < off.corner, "with edge restarts on, blocks land on none of block 0's switching instants, with them off on each: {on:?} on, {off:?} off");
    assert_eq!(off.corner_edge, 0, "with edge restarts off, no landing is an edge");
}

/// An edit and a plan handover in the pipeline, on the chain in Fast on two helpers: block 1's
/// listener halves at 4 ms, on the timeline or right after an advance that left the stages behind
/// block 0, and the run plans again 1 ms after it (`Replan::edit`), cutting the chain in two.
/// - Each run is the same, bit for bit, as on the worker alone: an edit and a handover find every
///   block at one instant, the stages caught up.
/// - The edit is heard at its instant: `l` follows `i0` whole up to the edit and half from 50 us
///   after it (its listener's 5 us), never earlier, though block 1 ran rounds behind.
/// - The new plan goes in: the run ends in its two blocks.
/// - A firmware stop inside block 0's round, the stages behind it, changes every block's step cap
///   at one instant too: the run is the same on two helpers as on the worker alone.
/// - A landed chain (`Landing`'s, on the timeline as a run made again from it meets it) is a plan
///   of the same cut whose imports land otherwise: made at the round start after the one that met
///   the landing, it goes in a lead later, the run in rounds throughout (it never steps whole for the plan's lead), its
///   three blocks running on.
/// - Through `run_live`, one round a pass, block 2's listener halved once the run is past `EDIT`:
///   in `Leaf`'s rounds (block 2 behind block 0, and the edit on no loop, so the run stays in
///   rounds), on one stepper, and on one stepper up to the edit with `Leaf`'s plan going in at its
///   instant (`LiveCmd::SetRounds` taken with it: the blocks start from the run's state there).
///   Each way, values that say they were stepped with it (`LiveValues::stepped_with`) read it in
///   the nets and in the pin currents: `m` at half of `l`, and so the current `ge2` drives from
///   `m`. In rounds, one published past its front with block 2 behind, still the old circuit,
///   says it was not.
#[test]
fn an_edit_and_a_handover_find_the_pipeline_at_one_instant() {
    let chain_plan = || Some(Box::new(Chain) as Box<dyn Planner>);
    let mut fast = spec(SAMPLE, true, true);
    (fast.replan.edit, fast.replan.lead) = (1e-3, 0.5e-3);
    for (how, edit) in [("on the timeline", Edit::Timed), ("behind the front", Edit::Behind)] {
        let (alone, alone_sim) = run(chain_plan(), fast.clone(), None, None, edit);
        let (helped, sim) = run(chain_plan(), fast.clone(), Some(pool(2)), None, edit);
        let st = sim.stats();
        eprintln!("{how}: {} plans, {} rounds, {} packs on a helper, {} flushes", st.plans, st.rounds, st.packs_helped, st.round_split.flushes);
        assert!(same(&helped, &alone), "{how}: the same points and values on two helpers as on the worker alone");
        assert_eq!((alone_sim.blocks(), sim.blocks()), (2, 2), "{how}: the edit's plan went in, two blocks");
        let (l, i0) = (helped.v("l"), helped.v("i0"));
        let gain = |from: f64, to: f64, g: f64| {
            helped.at.iter().enumerate().filter(|&(_, &t)| t >= from && t < to).map(|(n, _)| (l[n] - g * i0[n]).abs()).fold(0.0, f64::max)
        };
        let (before, after) = (gain(EDIT - 4.0 * ROUND, EDIT, 1.0), gain(EDIT + 50e-6, EDIT + 2e-3, 0.5));
        eprintln!("{how}: l from i0 before the edit {before:e} V, from half of it after {after:e} V");
        assert!(before < 0.02, "{how}: l follows i0 whole up to the edit: {before:e} V off");
        assert!(after < 0.02, "{how}: l follows half of i0 from 50 us after the edit: {after:e} V off");
    }
    let (alone, _) = run(chain_plan(), fast.clone(), None, None, Edit::Stop);
    let (helped, sim) = run(chain_plan(), fast.clone(), Some(pool(2)), None, Edit::Stop);
    assert_eq!(sim.stats().co_stops, 1, "the firmware stopped once");
    assert!(same(&helped, &alone), "a firmware stop: the same points and values on two helpers as on the worker alone");

    // a landed chain: a re-plan in rounds, its plan going in at a round start
    const LANDED: f64 = 2e-3;
    let mut sim = LiveSim::new(chain(), ValuePlan::default(), 1, fast.clone()).expect("the operating point converges");
    sim.attach(Some(toggle()));
    sim.follow(&[], &[], &[], &[(LANDED, Land { born: "i0".into(), to: "i0".into() })], None);
    sim.set_planner(Some(Box::new(Landing::default()))).expect("the plan installs");
    sim.set_helpers(Some(pool(2)));
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    let mut before = None;
    while sim.t() < STOP {
        let to = if sim.t() < LANDED - ROUND { LANDED - ROUND } else { STOP };
        sim.advance(to, Some(Duration::MAX), &mut record).unwrap_or_else(|d| panic!("the landed chain steps: {d}"));
        if sim.t() >= LANDED - ROUND && before.is_none() {
            before = Some(sim.stats().clone());
        }
    }
    let (st, was) = (sim.stats(), before.expect("the run passed the landing"));
    let went_in: Vec<(f64, f64)> = sim.go_ins().into_iter().filter(|g| g.0 >= LANDED).collect();
    let since = (st.plans - was.plans, st.plans_kept - was.plans_kept, st.blocks_kept - was.blocks_kept, sim.blocks());
    eprintln!("a landed chain: {} plans, {} kept the cut, {} blocks kept, {:e} s whole ({:?} since the landing: plans, kept, blocks kept, blocks), go-ins {went_in:?}",
        st.plans, st.plans_kept, st.blocks_kept, st.off_rounds, since);
    assert!(st.plan_error.is_none(), "a landed chain: {:?}", st.plan_error);
    assert_eq!(since, (1, 0, 3, 3),
        "a landed chain: one plan after it, which goes in (no cut the run already runs: its imports land otherwise), the three blocks running on");
    assert_eq!(st.off_rounds, 0.0, "a landed chain: the run never steps whole for the plan's lead");
    assert!(went_in.len() == 1 && went_in[0].0 < LANDED + 2.0 * ROUND && (went_in[0].1 - went_in[0].0 - fast.replan.lead).abs() < ROUND,
        "a landed chain: its plan is made at the round start after the one that met it and goes in a lead later: {went_in:?}");

    // the live runs: Play is the first command, block 2's edit the second (and rounds on the third)
    let pins = vec![vec![Terminal { device: "ge2".into(), terminal: 1 }]];
    let plan = ValuePlan { nets: vec!["l".into(), "m".into()], pins, pin_net: vec![None], ..ValuePlan::default() };
    for how in ["in rounds", "whole", "at a go-in"] {
        let live = LiveSpec { until: STOP, values_every: Duration::ZERO, slice: Duration::ZERO, ..spec(SAMPLE, true, true) };
        let mut sim = LiveSim::new(chain(), plan.clone(), 1, live).expect("the operating point converges");
        sim.attach(Some(toggle()));
        if how != "whole" {
            sim.set_planner(Some(Box::new(Leaf))).expect("the plan installs");
        }
        if how == "at a go-in" {
            sim.set_rounds(false).expect("the run leaves rounds, its planner kept aside");
        }
        sim.set_helpers(Some(pool(2)));
        let (tx, cmds) = mpsc::channel();
        tx.send(LiveCmd::Play).unwrap();
        let values = Latest::new();
        values.keep_all(true);
        let (mut traces, _) = mpsc::sync_channel(0);
        let sent = std::cell::Cell::new(None);
        let mut points = Points::default();
        run_live(sim, &cmds, &values, &mut traces, &mut points, &|| {
            let Some((_, v)) = values.latest() else { return };
            if sent.get().is_none() && v.t > EDIT {
                tx.send(LiveCmd::SetParam { generation: 1, device: "rm".into(), key: "value".into(), value: 5e3 }).unwrap();
                if how == "at a go-in" {
                    tx.send(LiveCmd::SetRounds(true)).unwrap();
                }
                sent.set(Some(v.t));
            }
            // the run rests at its end (`LiveSpec::until`)
            if v.t >= STOP - 1e-9 {
                tx.send(LiveCmd::Stop).unwrap();
            }
        }).expect("the live chain runs to its end");
        let sent = sent.get().expect("the edit was sent");
        let mut publications = Vec::new();
        values.take_kept(&mut publications);
        let (with, behind): (Vec<_>, Vec<_>) = publications.iter().filter(|v| v.t > sent).partition(|v| v.stepped_with >= 2);
        // m over l, one before the edit and a half after it; and ge2's current over what it drives
        // at m's place at one
        let gain = |v: &fab_elec_sim::live::LiveValues| v.net_v[1] / v.net_v[0];
        let gain_i = |v: &fab_elec_sim::live::LiveValues| (v.branch_i[0] / (1e-4 * v.net_v[0])).abs();
        eprintln!("live {how}: the edit sent at {sent:e} s; {} publications past it with it, {} without", with.len(), behind.len());
        assert!(publications.iter().any(|v| v.t < sent && (gain_i(v) - 1.0).abs() < 0.1), "{how}: before the edit ge2 drives what m reads, m at l");
        assert_eq!(publications.iter().any(|v| v.t > sent && v.in_rounds), how != "whole", "{how}: the run past the edit is in rounds but on one stepper");
        assert!(!with.is_empty(), "{how}: the live run published with the edit");
        for v in &with {
            assert!(gain(v) < 0.75 && gain_i(v) < 0.75, "{how}: values stepped with the edit read it in the nets and the pin currents, m and ge2's current at half of l: {} and {} of l at {:e} s", gain(v), gain_i(v), v.t);
        }
        if how == "in rounds" {
            assert!(behind.iter().any(|v| gain(v) > 0.9), "{how}: a publication past the edit's front with the stages behind, still the old circuit, says it was not stepped with it: {:?}",
                behind.iter().map(|v| (v.t, gain(v))).collect::<Vec<_>>());
        }
    }
}

/// `name`'s rising crossings of `level` after `from`, interpolated between the accepted points.
fn rising(record: &Record, name: &str, level: f64, from: f64) -> Vec<f64> {
    let o = record.v(name);
    (1..record.at.len())
        .filter(|&n| record.at[n] > from && o[n - 1] < level && o[n] >= level)
        .map(|n| record.at[n - 1] + (record.at[n] - record.at[n - 1]) * (level - o[n - 1]) / (o[n] - o[n - 1]))
        .collect()
}

/// A new cut reads its couplings as the old cut left them: in `fed_back`, `m`'s load halves at
/// `EDIT` and the plan after it puts `m` in a new block 1 beside `l`, whose draw from the timing
/// node block 0 reads back a round late (two in the pipeline). From the edit on, the oscillator's
/// rising edges keep within 1 us of a run whose plans keep the old cut (`FedBack::kept`), the
/// pipeline on and off, whichever the draw:
/// - `Draw::Follows`: block 0 reads, for the rounds it reads back across the go-in, what the old
///   block 1 drew, as through a re-cut that keeps its blocks.
/// - `Draw::Steady`: the devices behind the new block 1's draw are more than the old one's, so no
///   old history is that draw; block 0 reads it as the devices draw it at the go-in, never a value
///   no block solved.
/// - `Port::Same` and `Port::Moved`: block 1 stands beside `i0`'s holder through a port, whose
///   memory the new block 1 takes from the old one, or with other devices behind it starts at its
///   equilibrium, its stepper's first point taken again from that state: never the plan's state.
#[test]
fn a_new_cut_reads_its_couplings_as_the_old_cut_left_them() {
    let mut figures = Vec::new();
    for (draw, port) in [(Draw::Follows, Port::Bare), (Draw::Steady, Port::Bare), (Draw::Follows, Port::Same), (Draw::Steady, Port::Moved)] {
        for pipeline in [true, false] {
            let mut live = spec(SAMPLE, pipeline, true);
            (live.replan.edit, live.replan.lead) = (1e-3, 0.5e-3);
            let go = |kept: bool| {
                let mut sim = LiveSim::new(fed_back(draw, port), ValuePlan::default(), 1, live.clone()).expect("the operating point converges");
                sim.set_planner(Some(Box::new(FedBack { kept, draw, port }))).expect("the plan installs");
                let halve = LiveCmd::SetParam { generation: 1, device: "rm".into(), key: "value".into(), value: 5e3 };
                assert!(sim.apply(LiveCmd::At(EDIT, Box::new(halve))), "the timed edit keeps the run");
                let mut record = Record::default();
                record.begin(&sim.solver().unknowns);
                while sim.t() < FED_STOP {
                    sim.advance(FED_STOP, None, &mut record).unwrap_or_else(|d| panic!("the fed-back chain steps: {d}"));
                }
                (record, sim.blocks())
            };
            let ((cut, cut_blocks), (kept, kept_blocks)) = (go(false), go(true));
            let (a, b) = (rising(&cut, "o0", 2.5, EDIT), rising(&kept, "o0", 2.5, EDIT));
            let worst = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max);
            let what = format!("{draw:?}, {port:?}, pipeline {pipeline}");
            eprintln!("{what}: {cut_blocks} blocks at the stop with the new cut, {kept_blocks} kept; o0 rises at {a:?} s with the new cut, {b:?} s kept: worst {:.3} us", worst * 1e6);
            figures.push((what, cut_blocks, kept_blocks, a, b, worst));
        }
    }
    for (what, cut_blocks, kept_blocks, a, b, worst) in figures {
        assert_eq!((cut_blocks, kept_blocks), (2, 3), "{what}: the new cut goes in, the kept one runs on");
        assert!(a.len() == b.len() && a.len() >= 3, "{what}: o0 rises as often after the edit with the new cut as kept: {a:?}, {b:?}");
        assert!(worst < 1e-6, "{what}: o0's edges keep within 1 us of the kept cut's through the go-in: {:.3} us off", worst * 1e6);
    }
}

/// Where `a_go_in_from_whole_stepping_reads_a_charge_as_it_flows` plans again after stepping
/// whole: four samples into a round, on a charging ramp of `i0` further than the longest read back
/// (two rounds) from either of its switches (o0 rises at 4.99 ms and falls at 6.48 ms).
const GO_IN: f64 = 548.0 * SAMPLE;

/// Where it plans again at the instant it leaves rounds: a round end on the same ramp.
const AT_LEAVE: f64 = 576.0 * SAMPLE;

/// A go-in from a run stepping whole reads a boundary as its devices draw it: `fed_back` with
/// `Draw::Charge` in rounds, planned again from the run stepping whole. Either its planner is
/// taken away at the round end before `GO_IN` (`LiveSim::set_planner`), it steps whole for four
/// samples and it plans again at `GO_IN`; or its planner comes again at `AT_LEAVE`, in rounds,
/// so that it leaves them and goes in at that instant, its whole stepper holding only the rates
/// the blocks hand it as it resumes. Block 0 reads `cb`'s draw a round back (two in the
/// pipeline). For those rounds the go-in holds the draw as it flows then, at the timing node's
/// rate, never at the plan's 0. The oscillator's rising edges after it keep within 1 us of a run
/// that stays in rounds, the pipeline on and off. At the plan's 0 they are 4.4 us off after the
/// whole steps (1.9 us without the pipeline), and at the leave at a resume's zero rates 2.0 us
/// (1.9 us). What is left (0.4 us and 0.3 us, 0.1 us without the pipeline) is the hold itself: a
/// run stepping whole has no history for the reads back across the go-in.
#[test]
fn a_go_in_from_whole_stepping_reads_a_charge_as_it_flows() {
    let mut figures = Vec::new();
    for (whole, at) in [(true, GO_IN), (false, AT_LEAVE)] {
        for pipeline in [true, false] {
            let live = spec(SAMPLE, pipeline, true);
            let go = |again: bool| {
                let planner = || Some(Box::new(FedBack { kept: true, draw: Draw::Charge, port: Port::Bare }) as Box<dyn Planner>);
                let mut sim = LiveSim::new(fed_back(Draw::Charge, Port::Bare), ValuePlan::default(), 1, live.clone()).expect("the operating point converges");
                sim.set_planner(planner()).expect("the plan installs");
                let mut record = Record::default();
                record.begin(&sim.solver().unknowns);
                let stops = if whole { vec![at - 12.0 * SAMPLE, at, FED_STOP] } else { vec![at, FED_STOP] };
                for stop in stops {
                    while sim.t() < stop {
                        sim.advance(stop, None, &mut record).unwrap_or_else(|d| panic!("the charged chain steps: {d}"));
                    }
                    if again && stop < FED_STOP {
                        sim.set_planner(if stop == at { planner() } else { None }).expect("the planner goes, and comes again");
                    }
                }
                (record, sim.blocks())
            };
            let ((again, again_blocks), (stays, stays_blocks)) = (go(true), go(false));
            let (a, b) = (rising(&again, "o0", 2.5, at), rising(&stays, "o0", 2.5, at));
            let worst = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max);
            let how = if whole { "after four samples whole" } else { "at the leave" };
            eprintln!("{how}, pipeline {pipeline}: {again_blocks} blocks planned again, {stays_blocks} staying; o0 rises at {a:?} s planned again, {b:?} s staying: worst {:.3} us", worst * 1e6);
            figures.push((how, pipeline, again_blocks, stays_blocks, a, b, worst));
        }
    }
    for (how, pipeline, again_blocks, stays_blocks, a, b, worst) in figures {
        assert_eq!((again_blocks, stays_blocks), (3, 3), "{how}, pipeline {pipeline}: both runs end in the plan's rounds");
        assert!(a.len() == b.len() && a.len() >= 3, "{how}, pipeline {pipeline}: o0 rises as often after the go-in as staying: {a:?}, {b:?}");
        assert!(worst < 1e-6, "{how}, pipeline {pipeline}: o0's edges keep within 1 us of the run staying in rounds through a go-in from whole stepping: {:.3} us off", worst * 1e6);
    }
}

/// An edit that reaches block 0 alone (its pad's load halved on the timeline, as a key's press
/// sets a switch in block 0) leaves the stages running behind block 0 where they are: they see it
/// only through what block 0 exports from its instant on. On two helpers the run is the same, bit
/// for bit, as on the worker alone; the edit is heard; and it forces no catch-up of the stages
/// (`RoundSplit::flushes` as in the run without it). Made right after an advance that left the
/// stages behind, it gives the same run, bit for bit, as with a command that catches them up
/// first (`LiveCmd::SetRate` at the run's rate) just before it: the stages compute the same
/// whether they caught up or not.
#[test]
fn an_edit_of_block_0_alone_leaves_the_stages_behind() {
    let chain_plan = || Some(Box::new(Chain) as Box<dyn Planner>);
    let mut fast = spec(SAMPLE, true, true);
    // no plan after the edit: its handover would catch the stages up
    fast.replan.edit = f64::INFINITY;
    let (alone, _) = run(chain_plan(), fast.clone(), None, None, Edit::Ctl);
    let (helped, sim) = run(chain_plan(), fast.clone(), Some(pool(2)), None, Edit::Ctl);
    let (quiet, quiet_sim) = run(chain_plan(), fast.clone(), Some(pool(2)), None, Edit::None);
    let (st, quiet_st) = (sim.stats(), quiet_sim.stats());
    eprintln!("block 0's edit: {} packs on a helper, {} flushes; without it {} flushes", st.packs_helped, st.round_split.flushes, quiet_st.round_split.flushes);
    assert!(st.packs_helped > 0, "the stages run on the helpers");
    assert!(same(&helped, &alone), "the same points and values on two helpers as on the worker alone");
    assert!(!same(&helped, &quiet), "the edit is heard");
    assert_eq!(st.round_split.flushes, quiet_st.round_split.flushes, "the edit of block 0 alone catches no stage up");
    let (behind, _) = run(chain_plan(), fast.clone(), Some(pool(2)), None, Edit::CtlBehind { settled: false });
    let (settled, _) = run(chain_plan(), fast.clone(), Some(pool(2)), None, Edit::CtlBehind { settled: true });
    assert!(same(&behind, &settled), "made with the stages behind, the same points and values as with the stages caught up first");
}

/// Exact is never a pipeline: with the pipeline and edge restarts asked for (the defaults), the
/// chain in Exact runs exactly as with both off, bit for bit, on the worker alone and on two
/// helpers, through the edit and the plan after it; no landing is an edge and no stage runs behind
/// block 0.
#[test]
fn exact_rounds_are_plain_whatever_the_settings() {
    let chain_plan = || Some(Box::new(Chain) as Box<dyn Planner>);
    let exact = |pipeline: bool| {
        let mut spec = spec(0.0, pipeline, pipeline);
        (spec.replan.edit, spec.replan.lead) = (1e-3, 0.5e-3);
        spec
    };
    let (plain, plain_sim) = run(chain_plan(), exact(false), None, None, Edit::Timed);
    assert_eq!(plain_sim.blocks(), 2, "Exact: the edit's plan went in, two blocks");
    for (name, helpers) in [("on the worker alone", None), ("on two helpers", Some(pool(2)))] {
        let (record, sim) = run(chain_plan(), exact(true), helpers, None, Edit::Timed);
        assert!(same(&record, &plain), "Exact {name}: the same points and values as with the pipeline and edge restarts off");
        assert_eq!(sim.restarts().corner_edge, 0, "Exact {name}: no landing is an edge");
        assert_eq!(sim.stats().round_split.flushes, 0, "Exact {name}: no stage runs behind block 0");
    }
}

/// A sink that keeps every point it is given; its state at a snapshot is how many it held, and a
/// rewind goes back to that.
#[derive(Default)]
struct Points {
    at: Vec<f64>,
    x: Vec<Vec<f64>>,
    rewinds: Vec<f64>,
}

impl Sink for Points {
    fn point(&mut self, at: f64, _outer: Option<f64>, x: &[f64]) {
        self.at.push(at);
        self.x.push(x.to_vec());
    }
    fn save(&self) -> Option<Box<dyn SinkState>> {
        Some(Box::new(self.at.len()))
    }
    fn rewind(&mut self, at: f64, saved: Option<&dyn SinkState>, _unknowns: &[UnknownInfo]) {
        let n = *saved.and_then(|s| s.downcast_ref::<usize>()).expect("the state this sink saved");
        assert!(self.at[..n].iter().all(|&t| t <= at) && self.at[n..].iter().all(|&t| t > at), "a rewind to {at:e} s goes back to the points up to it");
        self.at.truncate(n);
        self.x.truncate(n);
        self.rewinds.push(at);
    }
}

/// The chain in Fast on two helpers (no firmware: a snapshot forks the run), playing, one round an
/// advance (each returning with the stages behind block 0), a snapshot due at every return when
/// `span` is above 0; block 1's listener halves at `EDIT`, sent before the run (`ahead`) or once
/// it is 0.4 ms past it.
fn played(span: f64, ahead: bool) -> (Points, LiveSim) {
    let snapshots = Snapshots { span, every: Duration::ZERO, share: f64::INFINITY, ..Snapshots::default() };
    let mut sim = LiveSim::new(chain(), ValuePlan::default(), 1, LiveSpec { snapshots, ..spec(SAMPLE, true, true) }).expect("the operating point converges");
    sim.set_planner(Some(Box::new(Chain))).expect("the plan installs");
    sim.set_helpers(Some(pool(2)));
    let edit = || LiveCmd::Edit(vec![(EDIT, LiveCmd::SetParam { generation: 1, device: "rl".into(), key: "value".into(), value: 5e3 })]);
    if ahead {
        assert!(sim.apply(edit()), "the edit keeps the run");
    }
    assert!(sim.apply(LiveCmd::Play), "the run plays");
    let mut points = Points::default();
    sim.keep_snapshot(&points);
    let mut sent = ahead;
    while sim.t() < STOP {
        sim.advance(STOP, Some(Duration::ZERO), &mut points).unwrap_or_else(|d| panic!("the chain steps: {d}"));
        sim.keep_snapshot(&points);
        if !sent && sim.t() > EDIT + 0.4e-3 {
            assert!(sim.apply(edit()), "the edit keeps the run");
            sent = true;
        }
    }
    (points, sim)
}

/// Snapshots and an edit behind the front in the pipeline: the chain in Fast on two helpers,
/// playing, every advance returning with the stages behind block 0, a snapshot due at each return.
/// - Without snapshots the stages run behind block 0 on the helpers (packs reach them).
/// - A due snapshot is of every block at one instant, kept all along, not only the run's start:
///   in rounds taken as the stages pass a round start, none caught up for it (no flush but the
///   edit's), a snapshot about every three rounds.
/// - An edit sent 0.4 ms after its time sends the run back once, to a snapshot less than three
///   rounds before the edit, never late.
/// - The sink's points are the straight run's by bits, the run that had the edit from its start
///   and kept no snapshots.
#[test]
fn snapshots_and_rewinds_find_the_pipeline_at_one_instant() {
    let (straight, straight_sim) = played(0.0, true);
    let posted = straight_sim.stats();
    assert!(posted.packs_helped + posted.packs_claimed > 0, "without snapshots the stages are posted to the helpers");
    let (back, sim) = played(1.0, false);
    let st = sim.stats();
    eprintln!("{} snapshots ({} marked, {} kept), {} rewinds to {:?}, {} flushes (edit {}, go-in {}, stop {}); without snapshots {} packs posted", st.snapshots, st.snapshots_marked, st.snapshots_kept, st.rewinds, back.rewinds,
        st.round_split.flushes, st.edit_flushes, st.go_in_flushes, st.stop_catchups, posted.packs_helped + posted.packs_claimed);
    assert_eq!(st.round_split.flushes, st.edit_flushes + st.go_in_flushes + st.stop_catchups, "no flush for a snapshot: {} flushes", st.round_split.flushes);
    assert!(st.snapshots_marked as f64 >= EDIT / (3.0 * ROUND), "in rounds a snapshot is taken as the stages pass each due round start: {} to the edit", st.snapshots_marked);
    assert_eq!((st.rewinds, st.late_edits, back.rewinds.len()), (1, 0, 1), "the edit sends the run back once");
    assert!(back.rewinds[0] <= EDIT && EDIT - back.rewinds[0] < 3.0 * ROUND, "the run goes back to a snapshot just before the edit: {:e} s", back.rewinds[0]);
    let same = back.at.len() == straight.at.len()
        && back.at.iter().zip(&straight.at).all(|(a, b)| a.to_bits() == b.to_bits())
        && back.x.iter().zip(&straight.x).all(|(p, q)| p.iter().zip(q).all(|(a, b)| a.to_bits() == b.to_bits()));
    assert!(same, "the sink's points are the straight run's: {} points against {}", back.at.len(), straight.at.len());
}

/// A paced run that reaches the wall clock leaves the stages behind block 0 posted and goes on
/// with them at its next advance: none of its stops at the clock catches them up on the worker
/// (each would run them there and merge inline, and the further ahead the run, the more stops:
/// the headroom went into catch-ups). A `Step`'s end and the take's end each still catch them
/// up, once. The chain in the pipeline on two helpers, advanced as `run_live` advances a paced
/// run (`advance_to_clock`), its clock stepped in simulated time: three quarters of a round past
/// where the run stands at each advance, a run well ahead of its pace, so it stops at the clock
/// every round and no load on the machine moves a stop.
#[test]
fn a_paced_run_catches_its_stages_up_at_a_step_and_the_take_end_never_at_the_clock() {
    const TAKE: f64 = 12e-3;
    const STEP: f64 = 2e-3;
    let c = chain();
    let plan = ValuePlan { nets: c.nodes.iter().map(|n| n.name.clone()).collect(), ..ValuePlan::default() };
    let spec = LiveSpec { rate: 0.2, until: TAKE, ..spec(SAMPLE, true, false) };
    let mut sim = LiveSim::new(c, plan, 1, spec).expect("the operating point converges");
    sim.attach(Some(toggle()));
    sim.set_planner(Some(Box::new(Chain))).expect("the plan installs");
    sim.set_helpers(Some(pool(2)));
    let mut points = Points::default();
    for (cmd, end) in [(LiveCmd::Step(STEP), STEP), (LiveCmd::Play, TAKE)] {
        assert!(sim.apply(cmd), "the run takes the command");
        while sim.running() {
            sim.advance_to_clock(sim.t() + 0.75 * ROUND, Some(Duration::MAX), &mut points).unwrap_or_else(|d| panic!("the chain steps: {d}"));
        }
        assert!((sim.t() - end).abs() < 1e-12, "the run rests at {end}: {}", sim.t());
    }
    let st = sim.stats();
    assert!(st.rounds > 0, "the chain runs in rounds");
    assert!(st.clock_stops as f64 >= (TAKE - STEP) / ROUND - 2.0, "the paced run stops at the clock every round on its way to the take's end: {} stops", st.clock_stops);
    assert_eq!(st.stop_catchups, 2, "the stages are caught up at the Step's end and at the take's end only, never at the clock's {} stops", st.clock_stops);
    assert_eq!(st.round_split.flushes, st.stop_catchups + st.edit_flushes + st.go_in_flushes, "no other flush catches the stages up");
}

/// Firmware that sets its pad high once, at `at`, and then does nothing.
#[derive(Clone)]
struct Once {
    at: f64,
    done: bool,
}

impl CoSim for Once {
    fn next(&self) -> f64 {
        if self.done { f64::INFINITY } else { self.at }
    }
    fn fork(&self) -> Option<Box<dyn CoSim>> {
        Some(Box::new(self.clone()))
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        assert!(bus.write(0, PinMode::OutHigh), "the firmware sets its pad");
        self.done = true;
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

/// `Chain`, its second plan (the first re-plan) held on its core until the test lets it go
/// (`gate`), and saying when it is made (`made`).
#[derive(Clone)]
struct Gated {
    calls: u32,
    gate: Arc<Mutex<mpsc::Receiver<()>>>,
    made: mpsc::Sender<()>,
}

impl Planner for Gated {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        self.calls += 1;
        let held = self.calls == 2;
        if held {
            let _ = self.gate.lock().unwrap().recv();
        }
        let got = Chain.plan(ckt, whole, t);
        if held {
            let _ = self.made.send(());
        }
        got
    }
}

/// A re-plan that falls due while a plan is on its way waits for that one to go in: it flushes
/// no round meanwhile (each flush catches the stages up on the worker and merges there, a
/// pipeline run serial), and starts at the first round start after the plan went in. The chain
/// in the pipeline on two helpers, heard (a plan not back is never waited for), no snapshots: an
/// edit of block 0's pad load (on no loop of the round map, so the run
/// stays in rounds) has its re-plan made on a helper and held there; meanwhile the firmware's
/// first change of its pad makes another re-plan due. Over the rounds the held plan spans past
/// that time, the only flush is each advance's own catch-up at its stop.
#[test]
fn a_re_plan_due_while_a_plan_is_on_its_way_flushes_no_round() {
    const WRITE: f64 = 2e-3;
    let replan = Replan { edit: 0.5e-3, pins: 0.5e-3, lead: 0.5e-3, slow: -1.0 };
    let snapshots = Snapshots { span: 0.0, ..Snapshots::default() };
    let spec = LiveSpec { replan: replan.clone(), heard: true, snapshots, ..spec(SAMPLE, true, false) };
    let mut sim = LiveSim::new(chain(), ValuePlan::default(), 1, spec).expect("the operating point converges");
    let pin = CoSimPin { name: "PA0".into(), gpio: "pa0".into(), pad: "pad".into(), vss: "0".into(), vdd: "vdd".into(), vih: PARAMS.vih, vil: PARAMS.vil };
    sim.attach(Some(CoSimDevice { dev: Box::new(Once { at: WRITE, done: false }), pins: vec![pin] }));
    let (open, gate) = mpsc::channel();
    let (made_tx, made) = mpsc::channel();
    sim.set_planner(Some(Box::new(Gated { calls: 0, gate: Arc::new(Mutex::new(gate)), made: made_tx }))).expect("the first plan installs");
    // its plans and checks on the same cores (`set_checks`), as a host gives its background jobs
    let cores = pool(2);
    sim.set_helpers(Some(cores.clone()));
    sim.set_checks(Some(cores), false);
    let mut record = Record::default();
    record.begin(&sim.solver().unknowns);
    let to = |sim: &mut LiveSim, record: &mut Record, at: f64| {
        while sim.t() < at {
            sim.advance(at, Some(Duration::MAX), record).unwrap_or_else(|d| panic!("the chain steps: {d}"));
        }
    };
    // an edit of block 0's pad load (on no loop of the round map: the run stays in rounds): its
    // re-plan starts `edit` later on a helper, which holds it
    to(&mut sim, &mut record, 1e-3);
    assert!(sim.apply(LiveCmd::SetParam { generation: 1, device: "rload".into(), key: "value".into(), value: 5e3 }), "the edit keeps the run");
    to(&mut sim, &mut record, WRITE + 2.0 * ROUND);
    assert_eq!((sim.stats().plans, sim.stats().edit_leaves), (2, 0), "the edit's re-plan has started, the run in rounds");
    // the pad's write made a re-plan due `pins` after it; the held plan is on its way past that
    let due = WRITE + replan.pins;
    to(&mut sim, &mut record, due + ROUND);
    let before = sim.stats().clone();
    to(&mut sim, &mut record, due + 4.0 * ROUND);
    let after = sim.stats();
    let flushes = (after.round_split.flushes - before.round_split.flushes) - (after.stop_catchups - before.stop_catchups);
    assert_eq!(flushes, 0, "no round flushed while a re-plan was due and a plan on its way ({} flushes, {} stop catch-ups)",
        after.round_split.flushes - before.round_split.flushes, after.stop_catchups - before.stop_catchups);
    assert_eq!(after.plans, 2, "the due re-plan waits for the plan on its way");
    // let go: the plan is back and goes in at the next round start, and the due re-plan starts at
    // the one after
    open.send(()).expect("the helper waits at the gate");
    made.recv().expect("the helper makes the plan");
    thread::sleep(Duration::from_millis(20));
    let back = sim.t();
    to(&mut sim, &mut record, back + 3.0 * ROUND);
    let st = sim.stats();
    assert!(st.plan_error.is_none(), "{:?}", st.plan_error);
    assert!(st.last_plan >= back && st.last_plan <= back + ROUND, "the held plan went in where it was back: at {:e} s, back at {back:e} s", st.last_plan);
    assert_eq!(st.plans, 3, "the due re-plan started once the held plan went in");
}

/// `Chain`, keeping each plan's snapshot as it was given: its time and the whole circuit's
/// values, by bits.
#[derive(Clone)]
struct Seen {
    seen: Arc<Mutex<Vec<(u64, Vec<u64>)>>>,
}

impl Planner for Seen {
    fn plan(&mut self, ckt: &SimCircuit, whole: &Solver, t: f64) -> Result<Option<(RoundPlan, Vec<Solver>)>, SimDiag> {
        self.seen.lock().unwrap().push((t.to_bits(), whole.x.iter().map(|v| v.to_bits()).collect()));
        Chain.plan(ckt, whole, t)
    }
}

/// A re-plan's snapshot is the rounds' marker (`Rounds::mark`): every block at the round start
/// where the re-plan falls due, block 0's taken there and each later stage's as it passes that
/// round, no stage caught up for it. It is the snapshot a flush to that instant composes, bit for
/// bit, and so the run is the same. The chain in the pipeline on two helpers, plans made there, no
/// snapshots; four timed edits of block 0's pad load, each giving a re-plan that goes in before
/// the next edit (the last two from after `EDIT`, the first of those a new cut), made twice: once a round at a time, each advance catching the stages up at its stop (so
/// each snapshot is whole at its start, as a flush has it), and once in one long advance, the
/// stages behind block 0 (so the snapshots are taken as they pass). The plans' snapshots and the
/// runs' points are the same, bit for bit, and no plan went in before its snapshot was whole. A plan
/// that keeps the cut goes in with the stages behind too: only the new cut's go-in flushes.
#[test]
fn a_re_plans_snapshot_and_a_kept_cuts_go_in_leave_the_stages_behind() {
    let edits = [1e-3, 3e-3, 5e-3, 7e-3];
    let made = |a_round_at_a_time: bool| {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let base = spec(SAMPLE, true, false);
        let replan = Replan { edit: 0.5e-3, lead: 1e-3, slow: -1.0, ..base.replan.clone() };
        let snapshots = Snapshots { span: 0.0, ..Snapshots::default() };
        let mut sim = LiveSim::new(chain(), ValuePlan::default(), 1, LiveSpec { replan, snapshots, ..base }).expect("the operating point converges");
        sim.set_planner(Some(Box::new(Seen { seen: seen.clone() }))).expect("the plan installs");
        sim.set_helpers(Some(pool(2)));
        for (n, &at) in edits.iter().enumerate() {
            let value = if n % 2 == 0 { 5e3 } else { 10e3 };
            let load = LiveCmd::SetParam { generation: 1, device: "rload".into(), key: "value".into(), value };
            assert!(sim.apply(LiveCmd::At(at, Box::new(load))), "the timed edit keeps the run");
        }
        let mut record = Record::default();
        record.begin(&sim.solver().unknowns);
        while sim.t() < STOP {
            let (to, budget) = if a_round_at_a_time { (sim.t() + ROUND, None) } else { (STOP, Some(Duration::MAX)) };
            sim.advance(to, budget, &mut record).unwrap_or_else(|d| panic!("the chain steps: {d}"));
        }
        let seen = seen.lock().unwrap().clone();
        (record, seen, sim.stats().clone())
    };
    let (flushed, flushed_seen, a) = made(true);
    let (marked, marked_seen, b) = made(false);
    for (name, st) in [("a round at a time", &a), ("behind", &b)] {
        eprintln!("{name}: {} plans, {} marked, {} fallbacks, flushes {} (go-in {}, edit {}, stop {})", st.plans, st.plans_marked,
            st.marker_fallbacks, st.round_split.flushes, st.go_in_flushes, st.edit_flushes, st.stop_catchups);
        assert!(st.plan_error.is_none(), "{name}: {:?}", st.plan_error);
        assert_eq!((st.plans, st.marker_fallbacks), (5, 0), "{name}: the first plan and four re-plans, each in with its snapshot whole");
    }
    assert_eq!(a.plans_marked, 0, "a round at a time: each snapshot is whole at its re-plan's start");
    assert!(b.plans_marked > 0, "behind: the stages were behind at a re-plan's start, its snapshot taken as they passed");
    assert_eq!((a.plans_kept, b.plans_kept), (3, 3), "both runs: three re-plans keep the cut, one cuts anew");
    assert_eq!((a.go_in_flushes, b.go_in_flushes), (0, 1), "a go-in flushes only for the new cut, and only with the stages behind");
    assert_eq!(flushed_seen.len(), 5, "every plan's snapshot was seen");
    for (n, (got, want)) in marked_seen.iter().zip(&flushed_seen).enumerate() {
        assert!(got == want, "plan {n}: the snapshot taken as the stages pass is the one at its start, by bits");
    }
    assert!(same(&marked, &flushed), "the run is the same, bit for bit: {} points against {}", marked.at.len(), flushed.at.len());
}

/// A snapshot taken as the stages pass, with a re-plan on its way, goes back to the same run: the
/// chain in the pipeline on two helpers, plans made there, playing one round an advance (each
/// returning with the stages behind block 0), a snapshot due at every return (taken about every
/// three rounds); three timed edits of block 0's pad load, each giving a re-plan; a retune of the
/// supply sent 0.4 ms after its time, between the third re-plan's start and its go-in (a retune
/// calls no plan back). The run goes back once, to a snapshot taken with that re-plan on its way
/// (here the one at its start, its plan's marker on its way with it, the stages behind), the plan
/// goes in from it, and the points are by bits those of the run that had the retune from its
/// start. No flush is for a snapshot.
#[test]
fn a_snapshot_taken_with_a_plan_on_its_way_goes_back_to_the_same_run() {
    const LATE: f64 = 6.8e-3;
    const THIRD: f64 = 5.4e-3;
    let load = |value: f64| LiveCmd::SetParam { generation: 1, device: "rload".into(), key: "value".into(), value };
    let played = |ahead: bool| {
        let base = spec(SAMPLE, true, false);
        let replan = Replan { edit: 0.5e-3, lead: 1e-3, slow: -1.0, ..base.replan.clone() };
        let snapshots = Snapshots { span: 1.0, every: Duration::ZERO, share: f64::INFINITY, ..Snapshots::default() };
        let mut sim = LiveSim::new(chain(), ValuePlan::default(), 1, LiveSpec { replan, snapshots, ..base }).expect("the operating point converges");
        sim.set_planner(Some(Box::new(Chain))).expect("the plan installs");
        sim.set_helpers(Some(pool(2)));
        for (at, value) in [(1e-3, 5e3), (3e-3, 10e3), (THIRD, 5e3)] {
            assert!(sim.apply(LiveCmd::At(at, Box::new(load(value)))), "the timed edit keeps the run");
        }
        // a retune: the plan on its way takes it (`PlanInputs::retunes`), it calls none back
        let late = || LiveCmd::Edit(vec![(LATE, LiveCmd::Retune { generation: 1, device: "supply".into(), source: Source::dc(3.0) })]);
        if ahead {
            assert!(sim.apply(late()), "the edit keeps the run");
        }
        assert!(sim.apply(LiveCmd::Play), "the run plays");
        let mut points = Points::default();
        sim.keep_snapshot(&points);
        let mut sent = ahead;
        while sim.t() < STOP {
            sim.advance(STOP, Some(Duration::ZERO), &mut points).unwrap_or_else(|d| panic!("the chain steps: {d}"));
            sim.keep_snapshot(&points);
            if !sent && sim.t() > LATE + 0.4e-3 {
                assert!(sim.apply(late()), "the edit keeps the run");
                sent = true;
            }
        }
        (points, sim)
    };
    let (straight, _) = played(true);
    let (back, sim) = played(false);
    let st = sim.stats();
    eprintln!("{} plans ({} marked, {} kept the cut), {} snapshots ({} marked), rewinds to {:?}, {} flushes (edit {}, go-in {}, stop {})", st.plans, st.plans_marked, st.plans_kept,
        st.snapshots, st.snapshots_marked, back.rewinds, st.round_split.flushes, st.edit_flushes, st.go_in_flushes, st.stop_catchups);
    assert!(st.plan_error.is_none(), "{:?}", st.plan_error);
    assert_eq!(st.round_split.flushes, st.edit_flushes + st.go_in_flushes + st.stop_catchups, "no flush for a snapshot");
    assert_eq!((st.rewinds, back.rewinds.len()), (1, 1), "the late retune sends the run back once");
    // the third re-plan starts at the first round start at or after its edit's time and `edit`
    let started = ((THIRD + 0.5e-3) / ROUND).ceil() * ROUND;
    assert!(back.rewinds[0] >= started - 1e-9 && back.rewinds[0] <= LATE, "the run goes back to a snapshot taken with the re-plan on its way (from {started:e} s): {:e} s", back.rewinds[0]);
    let same = back.at.len() == straight.at.len()
        && back.at.iter().zip(&straight.at).all(|(a, b)| a.to_bits() == b.to_bits())
        && back.x.iter().zip(&straight.x).all(|(p, q)| p.iter().zip(q).all(|(a, b)| a.to_bits() == b.to_bits()));
    assert!(same, "the sink's points are the run's with the retune from its start: {} points against {}", back.at.len(), straight.at.len());
}

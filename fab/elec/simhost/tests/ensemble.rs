use fab_elec_bench::fixture::ledctl_bench;
use fab_elec_bench::tolerance::member_values;
use fab_elec_bench::{lower, ModelLibs, SimCmd};
use fab_elec_core::ids::{BoardId, CompRef};
use fab_elec_fixtures::ledctl;
use fab_elec_sim::analysis::Sink;
use fab_elec_sim::livesim::LiveSpec;
use fab_elec_sim::stamp::UnknownInfo;
use fab_elec_simhost::{SimHost, Spawn};
use fab_elec_view::live::sim::{FeedSlot, SimAction, SimLive, SinkFeed, SinkSlot};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const NOMINAL: u32 = 2;
type Point = (u32, f64, f64);
type Points = Arc<Mutex<Vec<Point>>>;

struct Threads(Arc<AtomicUsize>);

impl Spawn for Threads {
    fn worker(&self, _name: &'static str, f: Box<dyn FnOnce() + Send>) {
        self.0.fetch_add(1, Ordering::Relaxed);
        let active = self.0.clone();
        std::thread::spawn(move || {
            f();
            active.fetch_sub(1, Ordering::Release);
        });
    }

    fn job(&self, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(f);
    }
}

#[derive(Default)]
struct State {
    begins: [AtomicUsize; 3],
    idle_at: [AtomicU64; 3],
    dropped: [AtomicUsize; 3],
}

struct Voltage {
    member: u32,
    points: Points,
    state: Arc<State>,
    index: Option<usize>,
    at: f64,
}

impl Voltage {
    fn new(member: u32, points: &Points, state: &Arc<State>) -> Voltage {
        Voltage { member, points: points.clone(), state: state.clone(), index: None, at: 0.0 }
    }
}

impl Sink for Voltage {
    fn begin(&mut self, unknowns: &[UnknownInfo]) {
        self.index = unknowns.iter().position(|u| u.name == "lamps[0].d.A");
        assert!(self.index.is_some(), "the sink must resolve LED 0's anode by name");
        self.state.begins[self.member as usize].fetch_add(1, Ordering::Release);
    }

    fn point(&mut self, at: f64, _outer: Option<f64>, x: &[f64]) {
        let k = self.index.expect("begin resolves the anode before its points");
        self.points.lock().expect("point recording lock").push((self.member, at, x[k]));
        self.at = at;
    }

    fn idle(&mut self) {
        self.state.idle_at[self.member as usize].store(self.at.to_bits(), Ordering::Release);
    }
}

impl Drop for Voltage {
    fn drop(&mut self) {
        self.state.dropped[self.member as usize].fetch_add(1, Ordering::Release);
    }
}

struct Feed {
    points: Points,
    state: Arc<State>,
}

impl SinkFeed for Feed {
    fn member(&mut self, member: u32) -> Box<dyn Sink + Send> {
        assert!(member < NOMINAL, "the host starts the two configured members");
        Box::new(Voltage::new(member, &self.points, &self.state))
    }
}

const PHASE_FRAMES: u32 = 50_000;

/// Frames at 2 ms until `done` holds. The bound is the frames the test drives, not wall time: a
/// busy box makes each frame wait longer, and only a worker that stopped making progress (a
/// hang) runs out of them; a phase here takes a few hundred.
fn frames(host: &mut SimHost, live: &mut SimLive, start: Instant, what: &str, mut done: impl FnMut(&SimHost) -> bool) {
    for _ in 0..PHASE_FRAMES {
        host.step(start.elapsed().as_secs_f64(), live);
        assert!(host.error().is_none(), "{what}: nominal worker failed: {:?}", host.error());
        if done(host) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("{what}: not reached within {PHASE_FRAMES} frames");
}

fn advance(host: &mut SimHost, live: &mut SimLive, start: Instant, points: &Points, state: &State) -> Vec<Point> {
    let from = points.lock().expect("point recording lock").len();
    let targets: [f64; 3] = std::array::from_fn(|m| f64::from_bits(state.idle_at[m].load(Ordering::Acquire)) + 2e-3);
    assert!(host.command(&SimCmd::Step(2e-3)), "Step reaches the live bench");
    frames(host, live, start, "all three workers finish the step", |_| {
        targets.iter().enumerate().all(|(m, &t)| f64::from_bits(state.idle_at[m].load(Ordering::Acquire)) >= t)
    });
    points.lock().expect("point recording lock")[from..].to_vec()
}

fn last(points: &[Point], member: u32) -> (f64, f64) {
    points.iter().rev().find(|p| p.0 == member).map(|p| (p.1, p.2))
        .unwrap_or_else(|| panic!("member {member} must record points"))
}

#[test]
fn ensemble_members_follow_the_nominal() {
    let start = Instant::now();
    let board = BoardId(0);
    let design = Arc::new(ledctl());
    let circuit = &design.unit(board).expect("ledctl has board 0").circuit;
    let r0 = circuit.comp("lamps[0].r").expect("LED 0 has its resistor");
    let comp = CompRef { board, comp: r0.id };
    let ohms = r0.value.as_ref().and_then(|v| v.qty).expect("the resistor has a value").si;
    // A low MCU output lights each LED steadily for comparing noisy members with the nominal run.
    let bench = ledctl_bench(10e3, [0.0; 8]);
    let libs = ModelLibs::generic();
    let before = lower(&design, board, &bench, &libs);
    let points = Arc::new(Mutex::new(Vec::new()));
    let state = Arc::new(State::default());
    let active = Arc::new(AtomicUsize::new(0));
    let mut host = SimHost::with_spawn(Box::new(Threads(active.clone())));
    host.set_members(Some(2));
    host.set_live_spec(LiveSpec { rate: f64::INFINITY, ..LiveSpec::default() });
    host.set_bench(bench.clone());
    assert!(host.command(&SimCmd::Pause), "start all workers paused");
    host.act(&SimAction::ShowScope { id: 7, sink: SinkSlot::new(Box::new(Voltage::new(NOMINAL, &points, &state))), column: 0.0 });
    host.act(&SimAction::Ensemble { id: 7, feed: FeedSlot::new(Box::new(Feed { points: points.clone(), state: state.clone() })) });
    host.commit(&design);
    let mut live = SimLive::default();
    frames(&mut host, &mut live, start, "the first lowering", |h| !h.lowering() && h.index().counts().0 > 0);

    let initial = advance(&mut host, &mut live, start, &points, &state);
    assert_eq!(active.load(Ordering::Acquire), 3, "the nominal and two members own workers");
    let a: Vec<_> = initial.iter().filter(|p| p.0 == 0 && p.1 >= 1e-3).collect();
    let b: Vec<_> = initial.iter().filter(|p| p.0 == 1 && p.1 >= 1e-3).collect();
    let nominal: Vec<_> = initial.iter().filter(|p| p.0 == NOMINAL).collect();
    let mut common = 0;
    for p in a {
        if let Some(q) = b.iter().find(|q| (q.1 - p.1).abs() < 1e-12) {
            let n = nominal.iter().min_by(|x, y| (x.1 - p.1).abs().total_cmp(&(y.1 - p.1).abs()))
                .expect("the nominal sink records the same run");
            assert!((n.1 - p.1).abs() <= 1e-4, "the nominal sample is within one maximum step");
            assert!((p.2 - q.2).abs() > 1e-9, "different tolerance draws give different anode voltages at {}", p.1);
            for v in [p.2, q.2] {
                assert!((v - n.2).abs() < 0.1 * n.2.abs(), "member voltage {v} stays within 10% of nominal {}", n.2);
            }
            common += 1;
        }
    }
    assert!(common > 0, "both members record common simulated times");

    host.act(&SimAction::Cmd(SimCmd::SetParam { comp, value: 2.0 * ohms }));
    let turned = advance(&mut host, &mut live, start, &points, &state);
    let nominal_delta = last(&turned, NOMINAL).1 - last(&initial, NOMINAL).1;
    assert!(nominal_delta.abs() > 1e-3, "doubling the resistor changes the nominal anode voltage");
    for m in 0..NOMINAL {
        let delta = last(&turned, m).1 - last(&initial, m).1;
        assert!(delta * nominal_delta > 0.0, "member {m} follows the nominal knob direction: {delta} vs {nominal_delta}");
        assert!((delta - nominal_delta).abs() < 0.1 * nominal_delta.abs(), "member {m} keeps a nearby knob response");
    }

    let begins: [usize; 3] = std::array::from_fn(|m| state.begins[m].load(Ordering::Acquire));
    let mut edited = (*design).clone();
    edited.generation += 1;
    let unit = edited.units.iter_mut().find(|u| u.id == board).expect("ledctl has board 0");
    let r1 = Arc::make_mut(&mut unit.circuit).comps.iter_mut().find(|c| c.path == "lamps[1].r").expect("LED 1 has its resistor");
    r1.value.as_mut().and_then(|v| v.qty.as_mut()).expect("LED 1's resistor has a value").si = 330.0;
    let after = lower(&edited, board, &bench, &libs);
    let draw = |lowered: &fab_elec_bench::Lowered| {
        let k = lowered.tol.iter().position(|s| s.path == "lamps[0].r").expect("LED 0 has a tolerance site");
        member_values(&lowered.tol, 1)[k]
    };
    assert_eq!(draw(&before).to_bits(), draw(&after).to_bits(), "seed 1 keeps LED 0's resistor draw across the other resistor's edit");
    let edited = Arc::new(edited);
    host.commit(&edited);
    // a value edited in the circuit goes into every worker's run as a value: none starts over
    frames(&mut host, &mut live, start, "all workers take the circuit's value", |h| h.generation() == edited.generation && !h.lowering());
    let replaced = advance(&mut host, &mut live, start, &points, &state);
    let now: [usize; 3] = std::array::from_fn(|m| state.begins[m].load(Ordering::Acquire));
    assert_eq!(now, begins, "a value edit starts no worker over");
    for m in 0..=NOMINAL {
        let first = replaced.iter().find(|p| p.0 == m).expect("every worker keeps recording after the edit");
        assert!(first.1 > last(&turned, m).0, "worker {m} continues simulated time through the edit");
    }

    host.act(&SimAction::HideScope { id: 7 });
    frames(&mut host, &mut live, start, "the hidden ensemble stops", |_| {
        active.load(Ordering::Acquire) == 1 && (0..2).all(|m| state.dropped[m].load(Ordering::Acquire) == 1)
    });
    let member_points = || points.lock().expect("point recording lock").iter().filter(|p| p.0 < NOMINAL).count();
    let hidden = member_points();
    let t = host.values().expect("the nominal has published values").t;
    assert!(host.command(&SimCmd::Step(2e-3)), "the nominal can advance after hiding the scope");
    frames(&mut host, &mut live, start, "more nominal time after hiding", |h| h.values().is_some_and(|v| v.t >= t + 2e-3));
    assert_eq!(member_points(), hidden, "hidden members receive no points while nominal time advances");
    assert_eq!(state.dropped[NOMINAL as usize].load(Ordering::Acquire), 1, "HideScope also removes the nominal sink");
    assert!(host.command(&SimCmd::Stop), "stop the nominal worker");
    frames(&mut host, &mut live, start, "all owned workers exit", |_| active.load(Ordering::Acquire) == 0);
}

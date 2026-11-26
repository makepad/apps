use super::*;
use fab_elec_board::Board;
use fab_elec_core::{Angle, BBox, Nm, PadIx, Side, P};
use std::sync::Arc;
use std::time::Instant;

type TestTask = Box<dyn FnOnce() + Send>;

struct TestPool {
    sender: Option<std::sync::mpsc::Sender<TestTask>>,
    threads: Vec<std::thread::JoinHandle<()>>,
    _pending: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<TestTask>>>,
}

impl crate::worker::Spawn for TestPool {
    fn spawn(&self, task: TestTask) {
        self.sender.as_ref().unwrap().send(task).expect("the test pool is running");
    }
}

impl Drop for TestPool {
    fn drop(&mut self) {
        self.sender.take();
        for worker in self.threads.drain(..) { worker.join().expect("test worker finished") }
    }
}

fn test_pool(helpers: usize) -> Arc<dyn crate::worker::Spawn> {
    let (sender, receiver) = std::sync::mpsc::channel::<TestTask>();
    let receiver = Arc::new(std::sync::Mutex::new(receiver));
    let threads = (0..helpers).map(|_| {
        let receiver = receiver.clone();
        std::thread::spawn(move || loop {
            let next = receiver.lock().unwrap().recv();
            match next { Ok(task) => task(), Err(_) => break }
        })
    }).collect();
    Arc::new(TestPool { sender: Some(sender), threads, _pending: receiver })
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }

    fn pose(&mut self, b: BBox) -> Pose {
        Pose {
            at: P::new(
                (i64::from(b.min.x) + self.below(b.width() as u64 + 1) as i64) as i32,
                (i64::from(b.min.y) + self.below(b.height() as u64 + 1) as i64) as i32,
            ),
            rot: Angle::from_udeg(self.below(4) as i64 * 90_000_000),
            side: if self.below(2) == 0 { Side::Top } else { Side::Bottom },
        }
    }
}

fn board(name: &str) -> Arc<Board> {
    fixture(name).0
}

/// A fixture's hand-placed board with its circuit.
fn fixture(name: &str) -> (Arc<Board>, Arc<fab_elec_circuit::Circuit>) {
    let design = if name == "ledctl" {
        fab_elec_fixtures::ledctl()
    } else {
        let mut sources = fab_elec_fixtures::docs(name).expect("fixture has stock sources");
        if name == "ledavr" {
            // The stock circuit/firmware bundle omits its checked-in hand-placed board.
            sources.add("ledavr.pcb.splash", include_str!("../../../fixtures/ledavr.pcb.splash"))
                .expect("add ledavr's board");
        }
        fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs())
            .unwrap_or_else(|diags| panic!("{name} evaluates: {diags:?}")).design
    };
    design.units.iter().find(|unit| unit.board.as_ref().is_some_and(|b| !b.placed.is_empty()))
        .map(|unit| (unit.board.clone().expect("a board"), unit.circuit.clone()))
        .unwrap_or_else(|| panic!("{name} contains a hand-placed board"))
}

/// Two independent LED anode nets give an analytic check on both costs, including same-side
/// semantics. Put their pad centres on a 6 nm horizontal and a 4 nm vertical segment.
fn check_crossing(board: &Board, mut problem: PlaceProblem) {
    let nets = ["lamps[0].d.A", "lamps[1].d.A"].map(|name| board.net(name).expect("an LED anode net"));
    problem.nets.retain(|n| nets.contains(&n.net));
    assert_eq!(problem.nets.len(), 2, "both anode nets are in the problem");
    let mut poses: Vec<_> = problem.movables.iter().map(|m| m.start).collect();
    for side in [Side::Top, Side::Bottom] {
        for (i, n) in problem.nets.iter().enumerate() {
            let targets = if i == 0 { [P::new(-3, 0), P::new(3, 0)] } else { [P::new(0, -2), P::new(0, 2)] };
            assert_eq!(n.pins.len(), 2, "an anode net joins one LED and one resistor");
            for (&pin, target) in n.pins.iter().zip(targets) {
                let PinRef::Mov(m, pad) = pin else { panic!("LED string pads are movable") };
                let pose = Pose { at: P::ZERO, rot: Angle::ZERO, side: if i == 0 { Side::Top } else { side } };
                poses[m] = Pose { at: target.sub(problem.movables[m].pad_at(pad, pose)), ..pose };
            }
        }
        let terms = Cost::new(&problem, &poses).terms();
        assert_eq!(terms.wirelength, 10, "two perpendicular two-pin nets span 6 + 4 nm");
        assert_eq!(terms.crossings, i64::from(side == Side::Top), "only coplanar nets cross");
    }
}

/// A halo is routing room on both layers and the vias between them: a part on the other side
/// keeps out of it. Where no part keeps a halo, a part may sit right under another on the other
/// side, as the courtyard rule allows.
fn check_halo_sides(board: &Board) {
    use super::free::Free;
    use fab_elec_board::placement::{body_at, placed_courtyard};
    use std::collections::BTreeSet;
    let top = |p: &&fab_elec_board::Placed| p.side == Side::Top && !p.footprint.pads.iter().any(|pad| pad.drill.is_some());
    let big = board.placed.iter().filter(top).max_by_key(|p| (p.footprint.pads.len(), std::cmp::Reverse(p.id))).expect("an SMD part on top");
    let small = board.placed.iter().filter(top).filter(|p| p.id != big.id)
        .min_by_key(|p| (placed_courtyard(p).bbox().width() * placed_courtyard(p).bbox().height(), p.id)).expect("a second SMD part");
    let under = placed_courtyard(big).bbox();
    let pose = Pose { at: under.center(), rot: small.rot, side: Side::Bottom };
    let moving = BTreeSet::from([small.id]);
    let free = Free::new(board, &moving, BTreeSet::new());
    assert_eq!(free.nearest(small.id, pose, 50_000), Some(pose.at), "no halo: {} fits under {} on the other side", small.refdes, big.refdes);
    // A room meets only a different owner's room. The two bodies already fit on opposite sides.
    let square = fab_elec_core::PolySet::rect(BBox::point(pose.at).inflate(1_000_000));
    let room = |comp, area| fab_elec_board::PortRoom { comp, room: Some(area), keepout: None };
    let fixed = room(big.comp, square.clone()).relative_to(big.at);
    for (depth, blocked) in [(0, false), (1_000, false), (1_001, true)] {
        let other = square.translated(P::new(2_000_000 - depth, 0));
        let candidate = room(small.comp, other).relative_to(pose.at);
        for rooms in [vec![fixed.clone()], vec![candidate.clone()], vec![fixed.clone(), candidate.clone()]] {
            let free = Free::with_rooms(board, &moving, BTreeSet::new(), &rooms);
            assert_eq!(free.fits(small.id, pose), rooms.len() == 1 || !blocked);
        }
        let rooms = [fixed.clone(), candidate];
        let both = BTreeSet::from([small.id, big.id]);
        let mut free = Free::with_rooms(board, &both, BTreeSet::new(), &rooms);
        assert!(free.fits(small.id, pose), "a mover's old room is clear");
        free.put(big.id, big.into());
        assert_eq!(free.fits(small.id, pose), !blocked, "a put restores the room at its pose");
    }
    let (halo, big_id) = (1_000_000, big.id);
    let spaced = Free::new(board, &moving, BTreeSet::new()).with_halo(move |p: &fab_elec_board::Placed| if p.id == big_id { halo } else { 0 }, |p: &fab_elec_board::Placed| p.id);
    let at = spaced.nearest_spaced(small.id, pose, 50_000).expect("room on the board");
    let court = body_at(small, at, pose.rot, Side::Bottom, true).bbox();
    let gap = (court.min.x - under.max.x).max(under.min.x - court.max.x).max(court.min.y - under.max.y).max(under.min.y - court.max.y);
    assert!(gap >= halo, "{}'s halo keeps {} out on the other side too: {gap} nm from its courtyard", big.refdes, small.refdes);
}

fn check_fixed_parts(mut board: Board) {
    use fab_elec_board::Origin;
    use fab_elec_core::{BlockId, FeatureRef};
    let scope: Vec<_> = board.placed[..4].iter().map(|p| p.comp).collect();
    board.placed[0].locked = true;
    board.placed[1].origin = Origin::Block { block: BlockId(0), detached: false };
    board.placed[2].origin = Origin::Derived { from: FeatureRef::Outline(board.unit) };
    board.placed[3].origin = Origin::Block { block: BlockId(0), detached: true };
    for selected in [scope.as_slice(), &[]] {
        let problem = PlaceProblem::new(&board, selected);
        for part in &board.placed[..3] {
            assert!(problem.obstacles.iter().any(|o| o.item == part.id), "{} stays fixed in scope {selected:?}", part.refdes);
        }
        assert!(problem.movables.iter().any(|m| m.comp == scope[3]), "detached block member can move in scope {selected:?}");
    }
}

#[test]
fn problem_cost_legalise_and_seed() {
    let pool = test_pool(3);
    for name in ["ledavr", "ledctl", "tonebox", "handheld", "drum16-placed"] {
        let started = Instant::now();
        let (board, circuit) = fixture(name);
        eprintln!("{name} evaluation {:.3}s", started.elapsed().as_secs_f64());
        let problem = PlaceProblem::new(&board, &[]);
        assert_eq!(problem, PlaceProblem::new(&board, &[]), "{name}: deterministic problem build");
        if name == "ledctl" {
            check_crossing(&board, problem.clone());
            check_fixed_parts((*board).clone());
            check_halo_sides(&board);
            check_zero_overflow_round(&board, &circuit);
            let scope: Vec<_> = board.placed.iter()
                .filter(|p| circuit.comps[p.comp.ix()].part.kind != fab_elec_parts::PartKind::Led)
                .map(|p| p.comp).collect();
            let held = PlaceProblem::new(&board, &scope);
            let mut poses: Vec<_> = held.movables.iter().map(|m| m.start).collect();
            let mut told = Told::default();
            let report = seed_watched(&board, &circuit, &held, &mut poses, &SeedOpts { demand: true, ..SeedOpts::default() },
                &mut told, Some(&pool));
            print_seat_trials(name, &board, &circuit, &held, &report);
            check_unseated_end(name, &board, &held, &poses, &report, &told);
        }
        check_legalise(name, &board, &circuit, &problem);
        check_instances(name, &board, &circuit);
        check_seed(name, &board, &circuit, &problem, &pool);
        if name == "drum16-placed" { check_held_panel(&board, &circuit, &pool); }
        let mut poses: Vec<_> = problem.movables.iter().map(|m| m.start).collect();
        let mut cost = Cost::new(&problem, &poses);
        let initial = cost.terms();
        eprintln!("{name}: {} parts, {} nets, weighted HPWL {:.6} mm, {} crossings",
            board.placed.len(), problem.nets.len(), initial.wirelength as f64 / 1e6, initial.crossings);
        for m in &problem.movables {
            let mut p = board.placed_item(m.item).expect("movable has a board item").clone();
            for rot in [m.start.rot, Angle::ZERO, Angle::QUARTER, Angle::HALF, Angle::QUARTER.neg(), Angle::deg(23.5)] {
                for side in [Side::Top, Side::Bottom] {
                    p.rot = rot;
                    p.side = side;
                    for i in 0..p.footprint.pads.len() {
                        let pad = PadIx(i as u16);
                        assert_eq!(Some(m.pad_at(pad, Pose::from(&p))), p.pad_at(pad),
                            "{name}: {} pad {i} at {rot:?} on {side:?}", p.refdes);
                    }
                }
            }
        }

        // Pin identity and side must survive restricting the scope, even on mixed-side boards.
        let one = PlaceProblem::new(&board, &[problem.movables[0].comp]);
        assert_eq!(one.movables.len(), 1, "{name}: only the scoped part may move");
        assert_eq!(one.obstacles.len() + 1, board.placed.len(), "{name}: every other part is an obstacle");
        assert_eq!(Cost::new(&one, &[one.movables[0].start]).terms(), initial,
            "{name}: replacing movable pins by fixed pins preserves both terms");

        let mut rng = Rng(0x6fd9_c317_b051_3ac5);
        let bounds = board.outline_bbox();
        for step in 0..200 {
            let m = rng.below(poses.len() as u64) as usize;
            let pose = rng.pose(bounds);
            let before = cost.terms();
            let delta = cost.delta(m, pose);
            poses[m] = pose;
            let fresh = Cost::new(&problem, &poses).terms();
            assert_eq!(delta, CostTerms {
                wirelength: fresh.wirelength - before.wirelength, crossings: fresh.crossings - before.crossings,
            }, "{name}: delta equals full cost difference at step {step}, movable {m}, {pose:?}");
            cost.apply(m, pose);
            assert_eq!(cost.terms(), fresh, "{name}: applied cost equals fresh cost at step {step}, movable {m}");
        }
    }
}

/// A single LED and resistor fit with spare routing capacity. Observe round zero
/// in the actual run so the pose comparison needs no control seed or golden poses.
fn check_zero_overflow_round(board: &Board, circuit: &fab_elec_circuit::Circuit) {
    let mut board = board.clone();
    let lamp = circuit.block_comps(circuit.comp("lamps[0].d").unwrap().block);
    board.placed.retain(|p| lamp.contains(&p.comp));
    board.reindex();
    let problem = PlaceProblem::new(&board, &[]);
    assert_eq!(problem.movables.len(), 2);
    let mut poses: Vec<_> = problem.movables.iter().map(|m| m.start).collect();
    let mut round_zero = None;
    let report = seed_watched(&board, circuit, &problem, &mut poses, &SeedOpts { demand: true, ..SeedOpts::default() },
        &mut |round: &SeedRound| {
            assert_eq!((round.round, round.overflow), (0, Some(0)));
            round_zero.get_or_insert_with(|| round.poses.to_vec());
            true
        }, None);
    assert!(report.unplaced.is_empty());
    assert_eq!(poses, round_zero.unwrap(), "zero overflow preserves every round-zero pose");
}

#[test]
fn quiet_region_spends_at_most_the_courtyard_overflow_on_each_side() {
    // Duplicate endpoints, fractional weights and nets that shorten before they stretch.
    // Find the affordable integer coordinates independently using each net's actual span.
    let unit = PNet::UNIT_WEIGHT;
    let bounds = [(-7, -5, unit), (0, 0, unit), (0, 4, unit / 2), (5, 5, unit)];
    let mut ends: Vec<_> = bounds.iter().flat_map(|&(a, b, w)| [(a, w), (b, w)]).collect();
    let length = |x: Nm| bounds.iter().map(|&(a, b, w)| i64::from(b.max(x) - a.min(x)) * w).sum::<i64>();
    let (min, max) = (-13, 16);
    let least = (min..=max).map(length).min().unwrap();
    for over in 0..=40 {
        let mut affordable = (min..=max).filter(|&x| length(x) - least <= over * unit);
        let lo = affordable.next().unwrap();
        let hi = affordable.next_back().unwrap_or(lo);
        assert_eq!(super::seed::quiet_interval(&mut ends, over, min, max), (lo, hi), "overflow {over} nm");
    }
}

#[test]
fn lamps_conform_to_their_standing_leds() {
    let (board, circuit) = fixture("ledctl");
    let scope: Vec<_> = board.placed.iter().filter(|p| circuit.comps[p.comp.ix()].part.kind != fab_elec_parts::PartKind::Led)
        .map(|p| p.comp).collect();
    assert_eq!(board.placed.len() - scope.len(), 8);
    let problem = PlaceProblem::new(&board, &scope);
    let mut poses: Vec<_> = problem.movables.iter().map(|m| m.start).collect();
    let mut serial = poses.clone();
    let opts = SeedOpts { demand: true, ..SeedOpts::default() };
    use crate::progress::{CompareStep, JobPhase, Progress};
    // On replay the comparison has finished on both workers. Any earlier offer fails here.
    struct Watching(Arc<Progress>);
    impl SeedWatch for Watching {
        fn comparison_progress(&self) -> Option<Arc<Progress>> { Some(self.0.clone()) }
        fn kept(&mut self, _: &SeedRound) -> bool {
            assert!(matches!(self.0.status().phase, JobPhase::Compare { seated: CompareStep::Done, plain: CompareStep::Done, .. }));
            true
        }
        fn matching(&mut self, _: usize, _: usize, _: fab_elec_core::BlockId) -> bool {
            assert!(matches!(self.0.status().phase, JobPhase::Compare { seated: CompareStep::Done, plain: CompareStep::Done, .. }));
            true
        }
    }
    let pool = test_pool(3);
    let phases = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = phases.clone();
    let mut watch = Watching(Arc::new(Progress::new().with_status(move |s| seen.lock().unwrap().push(s.phase))));
    let report = decisions(seed_watched(&board, &circuit, &problem, &mut poses, &opts, &mut watch, Some(&pool)));
    let serial_phases = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = serial_phases.clone();
    let mut serial_watch = Watching(Arc::new(Progress::new().with_status(move |s| seen.lock().unwrap().push(s.phase))));
    assert_eq!(report, decisions(seed_watched(&board, &circuit, &problem, &mut serial, &opts, &mut serial_watch, None)));
    assert_eq!(poses, serial);
    // A pool whose single worker nests exemplar trials inside a comparison run, and one with
    // no worker at all, finish the same layouts and decisions as the serial order.
    for helpers in [0, 1] {
        let mut small = problem.movables.iter().map(|m| m.start).collect::<Vec<_>>();
        let again = decisions(seed_watched(&board, &circuit, &problem, &mut small, &opts, &mut |_: &SeedRound| true, Some(&test_pool(helpers))));
        assert_eq!(again, report, "{helpers}-worker pool decides as the serial order");
        assert_eq!(small, serial, "{helpers}-worker pool places as the serial order");
    }
    assert!(report.unplaced.is_empty(), "held LEDs: {:?}", report.unplaced);
    let phases = phases.lock().unwrap();
    assert_eq!(phases.first(), Some(&JobPhase::Compare { seated: CompareStep::Waiting, plain: CompareStep::Waiting, of: 1 }));
    assert_eq!(phases.last(), Some(&JobPhase::Compare { seated: CompareStep::Done, plain: CompareStep::Done, of: 1 }));
    // Every serial status is observable. Concurrent notifications may coalesce.
    let serial_phases = serial_phases.lock().unwrap();
    for seated_run in [true, false] {
        let steps: Vec<_> = serial_phases.iter().map(|phase| match phase {
            JobPhase::Compare { seated, plain, of: 1 } => if seated_run { seated } else { plain },
            other => panic!("unexpected comparison phase {other:?}"),
        }).collect();
        assert!(steps.iter().any(|s| matches!(s, CompareStep::Round(n) if *n > 0)));
        assert!(steps.contains(&&CompareStep::Group(0)));
    }
    // Cancel at the start of matching. A stopped comparison cannot replay either run.
    struct Stop(Arc<Progress>);
    impl SeedWatch for Stop {
        fn comparison_progress(&self) -> Option<Arc<Progress>> { Some(self.0.clone()) }
        fn kept(&mut self, _: &SeedRound) -> bool { panic!("offered a private placement") }
        fn matching(&mut self, _: usize, _: usize, _: fab_elec_core::BlockId) -> bool { panic!("offered a private group") }
    }
    let hand: Vec<_> = problem.movables.iter().map(|m| m.start).collect();
    // With no helper to take a queued task, the caller must also finish the plain run.
    let busy = test_pool(0);
    for helpers in [&busy, &pool] {
        let parallel = Arc::ptr_eq(helpers, &pool);
        let progress = Arc::new_cyclic(|weak: &std::sync::Weak<Progress>| {
            let weak = weak.clone();
            Progress::new().with_status(move |s| {
                if let JobPhase::Compare { plain, of: 1, .. } = s.phase {
                    // A concurrent status can coalesce the last group and completion.
                    if plain == CompareStep::Group(0) || (parallel && plain == CompareStep::Done) {
                        weak.upgrade().unwrap().cancel();
                    }
                }
            })
        });
        let mut stopped = hand.clone();
        let partial = seed_watched(&board, &circuit, &problem, &mut stopped, &opts, &mut Stop(progress.clone()), Some(helpers));
        assert!(progress.cancelled() && partial.seat_guard.is_none() && partial.conformed.is_empty());
        assert_eq!(stopped, hand);
    }
    let after = posed(&board, &problem, &poses, &[]);
    legal("ledctl", "held LEDs", &after, &problem, &edge_parts(&circuit));

    let groups = identical_instances(&board, &circuit);
    assert_eq!(report.conformed.len(), 1);
    let row = &report.conformed[0];
    let frame = |copy: &Instance| {
        let anchor = after.placed_of(copy.members[0]).unwrap();
        assert_eq!(circuit.comps[anchor.comp.ix()].part.kind, fab_elec_parts::PartKind::Led);
        assert_eq!(Pose::from(anchor), Pose::from(board.placed_of(anchor.comp).unwrap()));
        let p = after.placed_of(copy.members[1]).unwrap();
        let at = p.at.sub(anchor.at).rotated(anchor.rot.neg());
        let rot = p.rot.sub(anchor.rot);
        (if anchor.side == Side::Bottom { (at.mirrored_x(0), rot.neg()) } else { (at, rot) }, p.side == anchor.side)
    };
    let offsets: Vec<_> = groups[0].iter().map(frame).collect();
    assert_eq!((row.status, row.copies, row.members_per_copy), (report.seat_guard.as_ref().unwrap().decision(), 8, 2), "{row:?}");
    assert!(offsets.iter().all(|p| *p == offsets[0]), "every resistor matches in its LED's frame");
}

#[test]
fn two_held_leds_report_the_part_blocking_the_common_seat() {
    use fab_elec_board::placement::{body_at, pads_box, placed_courtyard};
    use fab_elec_core::Contour;

    // A narrow strip fits a single row. The left LED has room only to its right;
    // the right LED's matching space is filled by a standing resistor, up to the edge.
    let (stock, circuit) = fixture("ledctl");
    let mut board = (*stock).clone();
    let [left, right, r0, r1, blocker] = ["lamps[0].d", "lamps[1].d", "lamps[0].r", "lamps[1].r", "usb.cc[0]"]
        .map(|path| circuit.comp(path).unwrap().id);
    board.placed.retain(|p| [left, right, r0, r1, blocker].contains(&p.comp));
    board.tracks.clear();
    board.vias.clear();
    board.holes.clear();
    board.reindex();
    let right_x = 12_000_000;
    let bounds = |c| body_at(board.placed_of(c).unwrap(), P::ZERO, Angle::ZERO, Side::Top, true).bbox();
    let stop_x = right_x + bounds(right).max.x - bounds(blocker).min.x;
    for p in &mut board.placed {
        p.side = Side::Top;
        p.rot = Angle::ZERO;
        let x = if p.comp == left { 0 } else if p.comp == right { right_x } else if p.comp == blocker { stop_x }
            else if p.comp == r0 { 4_000_000 } else { 8_000_000 };
        p.at = P::new(x, 0);
    }
    let edge = fab_elec_board::resolve_rules(&board, None).edge();
    let clearance = P::new(edge, edge);
    let strip = board.placed.iter().fold(BBox::EMPTY, |b, p| {
        let pads = pads_box(p, p.rot, p.side);
        b.union(placed_courtyard(p).bbox()).union(BBox::new(p.at.add(pads.min).sub(clearance), p.at.add(pads.max).add(clearance)))
    });
    board.outline = vec![Contour::rect(strip.min, strip.max, 0)];

    let problem = PlaceProblem::new(&board, &[r0, r1]);
    let mut poses: Vec<_> = problem.movables.iter().map(|m| m.start).collect();
    let report = seed(&board, &circuit, &problem, &mut poses, &SeedOpts { demand: true, ..SeedOpts::default() });
    assert!(report.unplaced.is_empty() && report.seat_guard.is_none());
    assert_eq!(report.conformed.len(), 1);
    let row = &report.conformed[0];
    assert_eq!((row.status, row.copies, row.members_per_copy, row.passed), (ConformStatus::NoSpot, 2, 2, 0), "{row:?}");
    let trial = row.seat_trial.as_ref().unwrap();
    let best = &trial.candidates[trial.best.unwrap()];
    let id = |c| board.placed_of(c).unwrap().id;
    assert_eq!(best.copies_fit, 1);
    assert_eq!(best.blocker, Some(SeatBlocker::Item(id(r1), id(blocker))), "{row:?}");

    // Removing just the named obstacle opens this exact candidate at both anchors.
    let free = Free::new(&board, &[id(r0), id(r1), id(blocker)].into_iter().collect(), Default::default());
    for (led, resistor) in [(left, r0), (right, r1)] {
        let local = best.offsets[0];
        let at = board.placed_of(led).unwrap().at.add(local.at);
        assert!(free.fits(id(resistor), Pose { at, ..local }));
    }
    let after = posed(&board, &problem, &poses, &[]);
    legal("two lamps", "unseated", &after, &problem, &[]);
    assert!(after.placed_of(r0).unwrap().at.x > after.placed_of(left).unwrap().at.x);
    assert!(after.placed_of(r1).unwrap().at.x < after.placed_of(right).unwrap().at.x);
    eprintln!("two lamps: {}/2 copies fit, {} hits {}, both resistors placed", best.copies_fit,
        board.placed_of(r1).unwrap().refdes, board.placed_of(blocker).unwrap().refdes);
}

/// A changed room covers the first pose, so checking the actual offered pose distinguishes
/// a winning re-place from metrics accidentally attached to the original proposal.
#[test]
fn case_guard_keeps_the_best_checked_arrangement() {
    use crate::{JobId, PlaceJob, PlaceMode, PlacePlan, PlacePlanner, PlanVerdict, Progress};
    use fab_elec_board::PortRoom;
    use fab_elec_circuit::{Circuit, Intent};
    use fab_elec_core::{CompId, PolySet};
    use std::sync::Mutex;

    #[derive(Clone)]
    struct Stub {
        moving: CompId,
        fixed: CompId,
        counts: &'static [Result<u32, &'static str>],
        seen: Arc<Mutex<Vec<(P, Vec<PortRoom>)>>>,
    }
    impl PlacePlan for Stub {
        fn open(&self) -> Result<Box<dyn PlacePlanner>, String> {
            assert!(self.seen.lock().unwrap().is_empty(), "the worker keeps one planner across passes");
            Ok(Box::new(self.clone()))
        }
    }
    impl PlacePlanner for Stub {
        fn plan(&mut self, board: &Board, _: &Circuit) -> Result<PlanVerdict, String> {
            let mut seen = self.seen.lock().unwrap();
            let at = board.placed_of(self.moving).unwrap().at;
            let room = PolySet::rect(BBox::point(at).inflate(500_000));
            let rooms = [self.fixed, self.moving].map(|comp|
                PortRoom { comp, room: Some(room.clone()), keepout: None }.relative_to(board.placed_of(comp).unwrap().at)
            ).to_vec();
            let call = seen.len();
            seen.push((at, rooms.clone()));
            let refused_pipes = self.counts[call]?;
            Ok(PlanVerdict { rooms, made_pipes: 3 - refused_pipes, refused_pipes,
                refused: if refused_pipes == 0 { vec![] } else { vec![format!("tight channel group on call {}", call + 1)] } })
        }
    }

    let (board, circuit) = fixture("ledctl");
    let mut circuit = (*circuit).clone();
    circuit.intents.retain(|i| !matches!(i, Intent::Near { .. }));
    let circuit = Arc::new(circuit);
    let moving = board.placed.iter().find(|p| p.refdes == "D1").unwrap().comp;
    let fixed = board.placed.iter().find(|p| p.refdes == "D2").unwrap().comp;
    let pool = test_pool(1);
    const ERROR: &str = "case.splash:2:9: no plan result";
    for counts in [&[Ok(0)][..], &[Err(ERROR)],
        &[Ok(2), Ok(0)], &[Ok(3), Ok(1), Ok(2)], &[Ok(3), Ok(1), Err(ERROR)]] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let job = PlaceJob { job: JobId(1), unit: board.unit, scope: vec![moving], mode: PlaceMode::Place,
            held: 0, rooms: vec![], add: vec![], circuit: circuit.clone(),
            plan: Some(Arc::new(Stub { moving, fixed, counts, seen: seen.clone() })) };
        let progress = Arc::new(Progress::new());
        let (mut last, mut partial) = (None, None);
        super::job::run(&board, &job, &progress, &mut |_, offer, final_offer| {
            if final_offer { last = Some(offer) }
            else {
                assert!(offer.rooms.is_none(), "a private re-place was offered before the comparison");
                partial = Some(offer);
            }
        }, pool.clone()).unwrap();
        let last = last.unwrap();
        let report = last.metrics.case_plan.as_ref().unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), counts.len());
        assert_eq!((report.calls, report.replaces), (counts.len() as u32, counts.len() as u32 - 1));
        if counts.len() == 1 {
            assert_eq!(last.ops, partial.unwrap().ops, "a single check preserves the chosen placement");
            assert_eq!(report.refused_pipes, counts[0].ok());
            if counts[0].is_err() { assert!(report.note.as_ref().unwrap().contains(ERROR)); }
            else { assert!(report.note.is_none()); }
            continue
        }
        assert_ne!(seen[0].0, seen[1].0, "the first verdict's rooms require a different pose");
        let mut kept = (*board).clone();
        kept.apply_all(&last.ops).unwrap();
        assert_eq!(kept.placed_of(moving).unwrap().at, seen[1].0);
        assert_eq!(last.rooms.as_ref().unwrap(), &seen[0].1, "the winner carries the rooms it used");
        assert!(last.metrics.no_room.is_empty());
        let refused = counts[1].unwrap();
        assert_eq!((report.made_pipes, report.refused_pipes), (Some(3 - refused), Some(refused)));
        let note = report.note.as_ref().unwrap();
        assert!(note.contains("placed again with the case's rooms"));
        assert_eq!(note.contains(ERROR), counts.last().unwrap().is_err());
        assert_eq!(note.contains("tight channel group on call 2"), refused != 0);
        assert!(!note.contains("on call 1") && !note.contains("on call 3"));
    }
}

#[test]
fn seat_guard_compares_unplaced_then_broken_nears_then_groups_alike_then_overflow() {
    use ConformStatus::{Kept, RefusedAlike, RefusedNear, RefusedOver, SkippedUnplaced};
    let score = |missing, broken, alike, over| RoundScore { passes: DemandPasses::default(), unplaced: (0..missing).collect(), broken, alike, over };
    for (seated, expected) in [
        (score(2, 0, 2, 0), SkippedUnplaced), (score(0, 2, 0, 20), Kept),
        (score(1, 2, 2, 0), RefusedNear), (score(1, 0, 0, 20), Kept),
        (score(1, 1, 0, 0), RefusedAlike), (score(1, 1, 2, 20), Kept),
        (score(1, 1, 1, 11), RefusedOver), (score(1, 1, 1, 10), Kept),
    ] {
        let guard = SeatGuard { plain: score(1, 1, 1, 10), seated };
        assert_eq!(guard.decision(), expected, "{guard:?}");
        assert!(guard.explanation().starts_with(if expected == Kept { "seats kept" } else { "plain kept" }));
    }
}

/// Compare the bare-panel candidates with the layout by hand. Only the packed seed supplies
/// candidates; the authored poses below are read after the seed returns.
fn print_seat_trials(
    name: &str, board: &Board, circuit: &fab_elec_circuit::Circuit, problem: &PlaceProblem, report: &SeedReport,
) {
    let format_pose = |p: Pose| format!("({:+.6},{:+.6},{:.0}°,{:?})",
        p.at.x as f64 / 1e6, p.at.y as f64 / 1e6, p.rot.degrees(), p.side);
    let part = |id| board.placed_item(id).map_or_else(|| format!("{id:?}"), |p| format!("{} ({})", p.path, p.refdes));
    for row in &report.conformed {
        let Some(trial) = &row.seat_trial else { continue };
        let path = &circuit.tree[row.blocks[0].ix()].path;
        let mut hand = Vec::new();
        for &block in &row.blocks {
            let mut members = circuit.block_comps(block);
            members.sort_by_key(|c| &circuit.comps[c.ix()].path);
            let (free, held): (Vec<_>, Vec<_>) = members.into_iter().map(|c| board.placed_of(c).unwrap())
                .partition(|p| problem.movables.iter().any(|m| m.comp == p.comp));
            let anchor = held[0];
            let poses: Vec<_> = free.iter().map(|p| {
                let offset = p.at.sub(anchor.at).rotated(anchor.rot.neg());
                let rot = p.rot.sub(anchor.rot);
                let local = if anchor.side == Side::Top { (offset, rot, p.side) } else { (offset.mirrored_x(0), rot.neg(), p.side.flipped()) };
                format_pose(Pose { at: local.0, rot: local.1, side: local.2 })
            }).collect();
            if !hand.contains(&poses) { hand.push(poses) }
        }
        let passed = trial.candidates.iter().filter(|c| c.wirelength.is_some()).count();
        let best = trial.best.map(|i| &trial.candidates[i]);
        let chosen = best.filter(|c| c.wirelength.is_some()).map(|c| c.exemplar);
        eprintln!("seat {name} {path} {} x {}: {passed}/{} distinct pass; chosen {chosen:?}; {:?}, delta_over {:?} nm; hand {hand:?}",
            row.copies, row.members_per_copy, trial.candidates.len(), row.status, row.delta_over);
        for c in &trial.candidates {
            let blocker = match &c.blocker {
                Some(SeatBlocker::Item(m, hit)) => format!("{} hits {}", part(*m), part(*hit)),
                Some(SeatBlocker::Edge(m)) => format!("{} hits board edge", part(*m)),
                Some(SeatBlocker::Side(m)) => format!("{} changes side", part(*m)),
                Some(SeatBlocker::Near) => "broken near".into(),
                None => String::new(),
            };
            let offsets: Vec<_> = c.offsets.iter().copied().map(format_pose).collect();
            eprintln!("seat {name} {path} copy {}: {offsets:?} fit {}/{} wire_nm {:?} {blocker}",
                c.exemplar, c.copies_fit, row.copies, c.wirelength);
        }
    }
}

/// Check the published winner against its actual final offer and the two finished layouts.
fn check_unseated_end<'a>(
    name: &str, board: &Board, problem: &PlaceProblem, poses: &[Pose], report: &'a SeedReport, told: &Told,
) -> &'a RoundScore {
    use super::congest::{congestion, default_cell};
    let guard = report.seat_guard.as_ref().expect("the held mates have an end comparison");
    let (winner, other) = if guard.decision() == ConformStatus::Kept { (&guard.seated, &guard.plain) } else { (&guard.plain, &guard.seated) };
    let rank = |s: &RoundScore| (s.unplaced.len(), s.broken, std::cmp::Reverse(s.alike), s.over);
    assert!(rank(winner) <= rank(other));
    assert_eq!(winner.unplaced, report.unplaced);
    assert_eq!(winner.over, congestion(board, problem, poses, default_cell(board)).over());
    assert_eq!(told.last_poses, poses, "the final offer has the winning poses");
    assert_eq!(told.matched.iter().map(|e| e.2).collect::<Vec<_>>(), report.conformed.iter().map(|r| r.blocks[0]).collect::<Vec<_>>(),
        "the watch sees each winning group once");
    for row in report.conformed.iter().filter(|r| r.seat_trial.is_some() && r.passed > 0) {
        assert_eq!(row.status, guard.decision());
        assert_eq!(row.delta_over, Some(guard.seated.over - guard.plain.over));
    }
    eprintln!("end {name} seated {:?}, plain {:?}, {}", guard.seated, guard.plain, guard.explanation());
    winner
}

/// The frozen board has no derived keepouts. Its fifty panel parts already stand.
fn check_held_panel(board: &Board, circuit: &fab_elec_circuit::Circuit, pool: &Arc<dyn crate::worker::Spawn>) {
    let (held, free): (Vec<_>, Vec<_>) = board.placed.iter().partition(|p|
        circuit.comps[p.comp.ix()].part.body.as_ref().is_some_and(|b| !b.ports.is_empty()));
    let scope: Vec<_> = free.iter().map(|p| p.comp).collect();
    let problem = PlaceProblem::new(board, &scope);
    assert_eq!(problem.movables.len(), free.len());
    let hand: Vec<_> = problem.movables.iter().map(|m| m.start).collect();
    let opts = SeedOpts { demand: true, ..SeedOpts::default() };
    let mut poses = hand.clone();
    let mut told = Told::default();
    let report = decisions(seed_watched(board, circuit, &problem, &mut poses, &opts, &mut told, Some(pool)));
    print_seat_trials("drum16-placed", board, circuit, &problem, &report);
    let end = check_unseated_end("drum16-placed", board, &problem, &poses, &report, &told);
    assert_eq!(end.broken, 0, "every near holds on the seated panel");
    assert!(report.unplaced.is_empty(), "panel held: {:?}", report.unplaced);
    let after = posed(board, &problem, &poses, &[]);
    legal("drum16-placed", "panel held", &after, &problem, &edge_parts(circuit));
    for p in held {
        assert_eq!(Pose::from(after.placed_item(p.id).unwrap()), Pose::from(p), "{} moved", p.path);
    }
    let broken = broken_nears(&after, circuit);
    let hand_broken = broken_nears(board, circuit);
    assert!(broken.len() <= hand_broken.len(), "panel held: {broken:?}, hand: {hand_broken:?}");
    let wire = Cost::new(&problem, &poses).terms().wirelength;
    let hand_wire = Cost::new(&problem, &hand).terms().wirelength;
    assert!(wire * 10 <= hand_wire * 16, "panel held: {wire} nm, hand: {hand_wire} nm");

    for (path, count, held_suffix, free_suffix) in [
        ("keys.keys[0][0]", 20, "sw", "d"), ("chain.steps[0]", 16, "d", "r"), ("chain.funcs[0]", 4, "d", "r"),
    ] {
        let row = report.conformed.iter().find(|r| circuit.tree[r.blocks[0].ix()].path == path)
            .unwrap_or_else(|| panic!("{path}: held group missing"));
        assert_eq!((row.status, row.copies, row.members_per_copy), (report.seat_guard.as_ref().unwrap().decision(), count, 2), "{path}");
        let mut offsets = Vec::new();
        for &block in &row.blocks {
            let member = |suffix| {
                let path = format!("{}.{suffix}", circuit.tree[block.ix()].path);
                after.placed_of(circuit.comp(&path).unwrap().id).unwrap()
            };
            let (anchor, mate) = (member(held_suffix), member(free_suffix));
            assert!(!scope.contains(&anchor.comp) && scope.contains(&mate.comp));
            let offset = mate.at.sub(anchor.at).rotated(anchor.rot.neg());
            let turn = mate.rot.sub(anchor.rot);
            let local = if anchor.side == Side::Bottom { (offset.mirrored_x(0), turn.neg()) } else { (offset, turn) };
            offsets.push((local, mate.side == anchor.side));
        }
        assert!(offsets.iter().all(|p| *p == offsets[0]), "{path}: mate poses in their held frames: {offsets:?}");
    }
    let (_, _, rounds_over) = told.last_round.unwrap();
    let final_over = told.told_over.last().copied().unwrap_or(rounds_over);
    eprintln!("panel held overflow (nm): after rounds {rounds_over}, after conform {final_over}");

    let mut again = hand;
    // Both pipelines can occupy the only two workers during nested exemplar trials.
    let repeat = seed_watched(board, circuit, &problem, &mut again, &opts, &mut |_: &SeedRound| true, Some(&test_pool(1)));
    assert_eq!(poses, again, "serial and parallel seat comparisons with pooled exemplar trials agree on every pose");
    assert_eq!(report, decisions(repeat));
}

fn decisions(mut report: SeedReport) -> SeedReport {
    for row in &mut report.conformed {
        row.wall_time = std::time::Duration::ZERO;
        row.trial_time = std::time::Duration::ZERO;
    }
    report
}

/// Compare the other item's identity and the intersection witness as well as legality.
/// Copper keepouts have their own DRC checks (a pad, for example, names a pad feature).
fn agrees(
    board: &Board,
    index: &fab_elec_board::placement::PlacementIndex,
    p: &fab_elec_board::Placed,
    violations: &[fab_elec_circuit::Violation],
) {
    use fab_elec_board::placement::Conflict;
    use fab_elec_circuit::CheckKind;
    use fab_elec_core::Sel;
    let mut expected = Vec::new();
    for v in violations {
        let [Sel::Item(_, a), Sel::Item(_, b)] = v.items.as_slice() else { continue };
        let other = if *a == p.id { *b } else if *b == p.id { *a } else { continue };
        let message = match v.kind {
            CheckKind::CourtyardOverlap => None,
            CheckKind::Keepout if board.placed_item(other).is_some() || board.keepouts.iter().any(|k| k.id == other) => {
                Some(v.message.clone())
            }
            _ => continue,
        };
        expected.push((other, v.at.expect("placement finding has a witness"), message));
    }
    let mut actual: Vec<_> = index.pose_conflicts(p.id, p.at, p.rot, p.side).map(|hit| match hit {
        Conflict::Courtyard { other, at } => (other, at, None),
        Conflict::Through { other, at } => (other, at, None),
        Conflict::Hole { hole, at } => (hole, at, None),
        Conflict::Keepout { owner, at, message } => (owner, at, Some(message)),
    }).collect();
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected, "{} at {:?}: placement conflicts match DRC", p.refdes, Pose::from(p));
}

#[test]
fn placement_agrees_with_drc() {
    use fab_elec_board::{drc, resolve_rules, BoardDelta, BoardKeepout, Connectivity, Origin, PieceIndex};
    use fab_elec_board::drc::drc_indexed;
    use fab_elec_board::placement::PlacementIndex;
    use fab_elec_core::{LayerId, LayerSet, PolySet, Shape};
    use fab_elec_parts::{Forbid, Keepout};

    let mut board = (*board("ledctl")).clone();
    let bounds = board.outline_bbox();
    let rules = resolve_rules(&board, None);
    // Placement findings do not depend on copper connectivity or zone fills.
    let conn = Connectivity::default();
    let mut rng = Rng(0x9b1d_314a_9f71_a921);
    for with_keepouts in [false, true] {
        if with_keepouts {
            // Exercise both directions: a body enters a board keepout, or its own keepout
            // sweeps over another body's courtyard. ledctl has no authored board keepouts.
            let id = board.alloc_id();
            board.keepouts.push(BoardKeepout {
                id, area: PolySet::rect(BBox::point(bounds.center()).inflate(5_000_000)),
                layers: LayerSet::one(LayerId::F_CU), forbid: Forbid::FOOTPRINTS,
                reason: String::new(), origin: Origin::Authored, z: Some((500_000, 3_000_000)),
            });
            let owner = &mut board.placed[0];
            owner.body_z = Some((0, 1_000_000));
            Arc::make_mut(&mut owner.footprint).keepouts.push(Keepout {
                area: PolySet::rect(BBox::new(P::new(2_000_000, -2_000_000), P::new(6_000_000, 2_000_000))),
                layers: LayerSet::one(LayerId::F_CU), forbid: Forbid::FOOTPRINTS, reason: String::new(),
            });
            board.reindex();
        }

        // The empty-publication job makes exactly the former fit decision at every step:
        // the region and the ordinary index, including parts already put back. No seed replay.
        let moving = board.placed.iter().map(|p| p.id).collect();
        let edges = board.placed.iter().filter(|p| board.edge_parts.contains(&p.comp)).map(|p| p.id).collect();
        let mut empty = Free::with_rooms(&board, &moving, edges, &[]);
        let region = fab_elec_board::placement::Region::new(&board, rules.edge()).unwrap();
        let mut ordinary = PlacementIndex::without(&board, &moving);
        let mut samples = Rng(0xd7d5_994b);
        for p in &board.placed {
            for pose in [p.into(), samples.pose(bounds), samples.pose(bounds)] {
                let fits = region.holds(p, pose.at, pose.rot, pose.side, board.edge_parts.contains(&p.comp))
                    && ordinary.pose_conflicts(p.id, pose.at, pose.rot, pose.side).next().is_none();
                assert_eq!(empty.fits(p.id, pose), fits, "empty rooms, {} at {pose:?}", p.refdes);
            }
            ordinary.place(p.id, p.at, p.rot, p.side);
            empty.put(p.id, p.into());
        }

        let placement = PlacementIndex::new(&board, None);
        let hand = drc(&board, &conn, &rules, &[], None);
        for p in &board.placed {
            agrees(&board, &placement, p, &hand.violations);
        }
        let pieces = PieceIndex::build(&board, &[]);
        let mut posed = board.clone();
        for _ in 0..if with_keepouts { 32 } else { 224 } {
            let i = if with_keepouts { 0 } else { rng.below(board.placed.len() as u64) as usize };
            let pose = rng.pose(bounds);
            let p = &mut posed.placed[i];
            (p.at, p.rot, p.side) = (pose.at, pose.rot, pose.side);
            // All placement findings for this pose have a witness inside its courtyard or an
            // owned keepout. Ask regional DRC for that window, with the actual moved copper.
            let window = p.footprint.keepouts.iter().fold(p.bbox(), |bb, k| {
                bb.union(p.shape_to_board(&Shape::Poly(k.area.clone())).bbox())
            });
            let delta = BoardDelta { changed: vec![p.id], bbox: window, ..BoardDelta::default() };
            let mut current = pieces.clone();
            current.update(&posed, &[], &delta, &[]);
            let findings = drc_indexed(&[], &current, &posed, &conn, &rules, &delta, &[]);
            agrees(&posed, &placement, &posed.placed[i], &findings);
            posed.placed[i].at = board.placed[i].at;
            posed.placed[i].rot = board.placed[i].rot;
            posed.placed[i].side = board.placed[i].side;
        }
    }
}


/// The movables at `poses` on a copy of the board; `gone` movables are taken off it.
fn posed(board: &Board, problem: &PlaceProblem, poses: &[Pose], gone: &[usize]) -> Board {
    let mut posed = board.clone();
    for (m, pose) in problem.movables.iter().zip(poses) {
        let p = posed.placed.iter_mut().find(|p| p.id == m.item).expect("a movable is on the board");
        (p.at, p.rot, p.side) = (pose.at, pose.rot, pose.side);
    }
    let gone: Vec<_> = gone.iter().map(|&i| problem.movables[i].item).collect();
    posed.placed.retain(|p| !gone.contains(&p.id));
    posed.reindex();
    posed
}

/// No courtyard or keepout finding (as DRC), and every movable in the region.
fn legal(name: &str, case: &str, board: &Board, problem: &PlaceProblem, edge: &[fab_elec_core::CompId]) {
    use fab_elec_board::placement::{PlacementIndex, Region};
    let index = PlacementIndex::new(board, None);
    let pairs: Vec<_> = index.courtyard_pairs().iter().map(|(a, b, _)| format!("{}/{}", a.refdes, b.refdes)).collect();
    assert!(pairs.is_empty(), "{name} {case}: legal: no courtyards overlap: {pairs:?}");
    assert!(index.keepout_pairs().is_empty(), "{name} {case}: legal: no body in a keepout");
    let through: Vec<_> = index.through_pairs().iter().map(|(a, b, _)| format!("{}/{}", a.refdes, b.refdes)).collect();
    assert!(through.is_empty(), "{name} {case}: legal: no through-hole pad under a body on the far side: {through:?}");
    let holes: Vec<_> = index.hole_pairs().iter().map(|(p, _, _)| p.refdes.clone()).collect();
    assert!(holes.is_empty(), "{name} {case}: legal: no body over a board hole: {holes:?}");
    let region = Region::new(board, fab_elec_board::resolve_rules(board, None).edge()).expect("the fixture has an outline");
    for m in &problem.movables {
        let Some(p) = board.placed_item(m.item) else { continue };
        assert!(region.holds(p, p.at, p.rot, p.side, edge.contains(&m.comp)), "{name} {case}: legal: {} sits in the outline, pads clear of the edge", p.refdes);
    }
}

/// Identity, jitter, pinning and no room, on one fixture's hand placement.
fn check_legalise(name: &str, board: &Board, circuit: &fab_elec_circuit::Circuit, problem: &PlaceProblem) {
    let opts = LegalOpts { edge: edge_parts(circuit), ..LegalOpts::default() };
    let hand: Vec<_> = problem.movables.iter().map(|m| m.start).collect();
    let mut poses = hand.clone();
    assert_eq!(legalise(board, problem, &mut poses, &opts), LegalReport::default(), "{name}: identity: a legal hand placement moves nothing");

    // Every part jittered by up to 2 mm, then legalised: legal, and each part near where it was.
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut jittered = hand.clone();
    let mut jitter = 0.0;
    for pose in &mut jittered {
        let (dx, dy) = (rng.below(4_000_001) as f64 - 2e6, rng.below(4_000_001) as f64 - 2e6);
        let scale = (2e6 / dx.hypot(dy)).min(1.0);
        let d = P::new((dx * scale) as i32, (dy * scale) as i32);
        pose.at = pose.at.add(d);
        jitter += (d.x as f64).hypot(d.y as f64);
    }
    let mut poses = jittered.clone();
    let report = legalise(board, problem, &mut poses, &opts);
    assert!(report.unplaced.is_empty(), "{name} jittered: every part finds room: {:?}", report.unplaced);
    legal(name, "jittered", &posed(board, problem, &poses, &[]), problem, &opts.edge);
    let moved: f64 = poses.iter().zip(&jittered).map(|(a, b)| (a.at.x as f64 - b.at.x as f64).hypot(a.at.y as f64 - b.at.y as f64)).sum();
    let n = poses.len() as f64;
    assert!(moved <= 1.5 * jitter, "{name}: minimal movement: mean move {:.3} mm over 1.5 x mean jitter {:.3} mm", moved / n / 1e6, jitter / n / 1e6);
    if name != "ledctl" { return }

    // A small part dropped on the largest: unpinned, the small one moves off; pinned, it stays
    // and the large one makes room.
    let area = |i: usize| match &problem.movables[i].shape { BodyShape::Courtyard(c) => c.area(), BodyShape::Bounds(_) => 0.0 };
    let big = (0..hand.len()).max_by(|&a, &b| area(a).total_cmp(&area(b))).expect("movables");
    let small = (0..hand.len()).filter(|&i| area(i) > 0.0).min_by(|&a, &b| area(a).total_cmp(&area(b))).expect("movables");
    for pinned in [false, true] {
        let mut poses = hand.clone();
        poses[small].at = hand[big].at;
        let dropped = poses[small];
        let opts = LegalOpts { pin: if pinned { vec![problem.movables[small].comp] } else { vec![] }, ..opts.clone() };
        let report = legalise(board, problem, &mut poses, &opts);
        assert!(report.unplaced.is_empty(), "drop pinned {pinned}: every part finds room");
        let (stays, moves) = if pinned { (small, big) } else { (big, small) };
        assert_eq!(poses[stays].at, if pinned { dropped.at } else { hand[big].at }, "drop pinned {pinned}: the {} part stays", if pinned { "pinned" } else { "large" });
        assert!(report.moved.iter().any(|m| m.0 == moves), "drop pinned {pinned}: the other part moves");
        legal(name, &format!("drop pinned {pinned}"), &posed(board, problem, &poses, &[]), problem, &opts.edge);
    }

    // No room: a 20 x 15 mm board holds what fits, legally; the rest is listed.
    let mut small = board.clone();
    let at = small.outline_bbox().min;
    small.outline = vec![fab_elec_core::Contour::rect(at, at.add(P::new(20_000_000, 15_000_000)), 0)];
    let mut poses = hand.clone();
    let report = legalise(&small, problem, &mut poses, &opts);
    assert!(!report.unplaced.is_empty() && report.unplaced.len() < poses.len(), "no room: some parts fit and some do not: {} unplaced", report.unplaced.len());
    legal(name, "no room", &posed(&small, problem, &poses, &report.unplaced), problem, &opts.edge);
}

/// The constructive seed from the circuit alone, on each hand-placed fixture: every part finds
/// room, legally; the circuit's intents hold (a part fixed to a spot keeps its pose, one fixed to
/// an edge sits flush on it facing out from any start, a `side` intent sets the side, every other
/// part keeps its side, a locked part stays); the nets come out
/// no longer than 1.6 x the hand placement's; each inferred decoupling capacitor lands by its IC;
/// and the same input gives the same placement.
/// Repeated blocks group as copies of one another: each group's copies list their members in one
/// relative path order with the same footprints, in a deterministic order. ledctl's eight lamps
/// are one group; drum16's voices, keys and gates are seven; handheld and tonebox repeat nothing.
fn check_instances(name: &str, board: &Board, circuit: &fab_elec_circuit::Circuit) {
    let groups = identical_instances(board, circuit);
    assert_eq!(groups, identical_instances(board, circuit), "{name}: the grouping is deterministic");
    let comp = |c: fab_elec_core::CompId| &circuit.comps[c.ix()];
    for (g, copies) in groups.iter().enumerate() {
        let shape = |copy: &Instance| copy.members.iter().map(|&c| comp(c).part.footprint.as_ref().map(|f| f.name.as_str())).collect::<Vec<_>>();
        assert!(copies.len() >= 2 && copies.iter().all(|copy| !copy.members.is_empty() && shape(copy) == shape(&copies[0])),
            "{name}: group {g}'s copies pair member by member with the same footprints");
    }
    let sizes: Vec<_> = groups.iter().map(|copies| (copies.len(), copies[0].members.len())).collect();
    match name {
        "ledctl" | "ledavr" => {
            assert_eq!(sizes, [(8, 2)], "{name}: the eight lamps are one group of two parts each");
            for (i, copy) in groups[0].iter().enumerate() {
                let paths: Vec<_> = copy.members.iter().map(|&m| comp(m).path.as_str()).collect();
                assert_eq!(paths, [format!("lamps[{i}].d"), format!("lamps[{i}].r")], "{name}: lamp {i}'s members in relative path order");
            }
        }
        "drum16-placed" => assert_eq!(sizes, [(16, 2), (4, 2), (20, 2), (7, 3), (3, 6), (3, 4), (4, 7)],
            "drum16: its voices, keys and gates group as (copies, parts per copy)"),
        "handheld" | "tonebox" => assert_eq!(sizes, [], "{name}: nothing repeats"),
        _ => {}
    }
}

// Each `near` holds: its ends (a pin's pad, else the part's courtyard box) no farther apart
// than its max.
fn broken_nears(b: &Board, circuit: &fab_elec_circuit::Circuit) -> Vec<(usize, String)> {
    use fab_elec_circuit::{Intent, Target};
    use fab_elec_board::placement::placed_courtyard;
    let end = |t: &Target| -> Option<BBox> {
        match *t {
            Target::Comp(c) => b.placed_of(c).map(|p| placed_courtyard(p).bbox()),
            Target::Pin(c, pin) => {
                let p = b.placed_of(c)?;
                let no = circuit.comps[c.ix()].part.pins.get(pin.ix())?.pads.first()?;
                let k = p.footprint.pads.iter().position(|pad| &pad.no == no)?;
                p.pad_at(PadIx(k as u16)).map(BBox::point)
            }
            Target::Block(_) => None,
        }
    };
    circuit.intents.iter().enumerate().filter_map(|(id, i)| match i {
        Intent::Near { a, b: other, max } => {
            let (x, y) = (end(a)?, end(other)?);
            let dx = (x.min.x - y.max.x).max(y.min.x - x.max.x).max(0);
            let dy = (x.min.y - y.max.y).max(y.min.y - x.max.y).max(0);
            let d = f64::from(dx).hypot(f64::from(dy));
            (d > f64::from(*max)).then(|| (id, format!("{a:?}-{other:?} {:.2} mm > {:.2} mm", d / 1e6, f64::from(*max) / 1e6)))
        }
        _ => None,
    }).collect()
}

/// The last demand round's poses and overflow, the overflow of each group kept after it, and
/// the groups as conform starts them.
#[derive(Default)]
struct Told {
    /// Populated only for the small plain-prefix comparison.
    round_poses: Option<Vec<Vec<Pose>>>,
    last_poses: Vec<Pose>,
    last_round: Option<(usize, Vec<Pose>, i64)>,
    told_over: Vec<i64>,
    matched: Vec<(usize, usize, fab_elec_core::BlockId)>,
}
impl SeedWatch for Told {
    fn kept(&mut self, s: &SeedRound) -> bool {
        let over = s.overflow.expect("demand has a map");
        match &self.last_round {
            None => assert_eq!(s.round, 0),
            Some((r, ..)) if *r == s.round => assert!(!self.matched.is_empty(), "only a conform decision repeats a round"),
            Some((r, ..)) => assert!(self.matched.is_empty() && s.round == r + 1, "offers advance in one pipeline"),
        }
        s.poses.clone_into(&mut self.last_poses);
        if self.last_round.as_ref().is_none_or(|(r, _, _)| *r != s.round) {
            if let Some(rounds) = &mut self.round_poses { rounds.push(s.poses.to_vec()); }
            self.last_round = Some((s.round, s.poses.to_vec(), over));
        } else {
            self.told_over.push(over);
        }
        true
    }
    fn matching(&mut self, group: usize, of: usize, first: fab_elec_core::BlockId) -> bool {
        self.matched.push((group, of, first));
        true
    }
}

fn check_seed(name: &str, board: &Board, circuit: &fab_elec_circuit::Circuit, problem: &PlaceProblem, pool: &Arc<dyn crate::worker::Spawn>) {
    use fab_elec_circuit::{Edge, Intent, Target};
    use fab_elec_board::placement::placed_courtyard;
    use fab_elec_board::resolve_rules;
    use fab_elec_parts::BodyPortKind;
    let comps = |t: &Target| match *t {
        Target::Comp(c) | Target::Pin(c, _) => vec![c],
        Target::Block(b) => circuit.block_comps(b),
    };
    // Fixed to a spot: the part stands on it. Fixed to nothing more: it keeps its pose. Fixed to an
    // edge: it takes a slot there.
    let fixed: Vec<_> = circuit.intents.iter().filter_map(|i| match i {
        Intent::Fixed { t, edge, at, .. } if edge.is_none() || at.is_some() => Some(comps(t)), _ => None,
    }).flatten().collect();
    // handheld fixes its boot button where it stands. Here that part is fixed to its spot as well,
    // starts in the board's middle turned a quarter, and the seed puts it back on its spot.
    let spotted = (name == "handheld").then(|| {
        let m = problem.movables.iter().find(|m| fixed.contains(&m.comp)).expect("handheld: a part fixed in place moves in a whole-board problem");
        let mut spotted = circuit.clone();
        spotted.intents.push(Intent::Fixed { t: Target::Comp(m.comp), edge: None, at: Some(m.start.at), rot: Some(m.start.rot) });
        (m.comp, spotted)
    });
    let (spot, circuit) = match &spotted {
        Some((comp, spotted)) => (Some(*comp), spotted),
        None => (None, circuit),
    };
    let slots = super::seed::edge_slots(board, circuit);
    let slotted: Vec<_> = slots.iter().map(|s| s.0).collect();
    for intent in &circuit.intents {
        let Intent::Fixed { t, edge: Some(_), at: None, .. } = intent else { continue };
        assert!(comps(t).iter().any(|c| slotted.contains(c)), "{name}: what {t:?} fixes to an edge has a part for the edge");
    }
    let sided: Vec<_> = circuit.intents.iter().filter_map(|i| match i {
        Intent::Side { t, side } => Some(comps(t).into_iter().map(|c| (c, *side)).collect::<Vec<_>>()), _ => None,
    }).flatten().collect();
    // Every fixture fixes its USB connector to an edge; ledctl and handheld put a part on the
    // bottom by intent. Without them the checks below would hold for nothing.
    assert!(!["ledctl", "handheld"].contains(&name) || !sided.is_empty(), "{name}: the fixture has a side intent");
    assert!(!slots.is_empty(), "{name}: the fixture fixes a part to an edge");

    let hand: Vec<_> = problem.movables.iter().map(|m| m.start).collect();
    // Each sided part starts on the wrong side, and each edge part turned a quarter in the board's
    // middle: the intents, not the start, decide.
    let side_of = |c| sided.iter().find(|s| s.0 == c).map(|s| s.1);
    let middle = board.outline_bbox().center();
    let start: Vec<_> = problem.movables.iter().zip(&hand).map(|(m, &pose)| {
        let pose = Pose { side: side_of(m.comp).map_or(pose.side, Side::flipped), ..pose };
        if slotted.contains(&m.comp) || spot == Some(m.comp) { Pose { at: middle, rot: pose.rot.add(Angle::QUARTER), ..pose } } else { pose }
    }).collect();
    let mut poses = start.clone();
    let report = seed(board, circuit, problem, &mut poses, &SeedOpts::default());
    assert!(report.unplaced.is_empty(), "{name}: seed: every part finds room: {:?}", report.unplaced);
    legal(name, "seed", &posed(board, problem, &poses, &[]), problem, &edge_parts(circuit));
    for (i, m) in problem.movables.iter().enumerate() {
        let p = board.placed_item(m.item).expect("a movable is on the board");
        assert_eq!(poses[i].side, side_of(m.comp).unwrap_or(hand[i].side), "{name}: seed: {} sits on its intent's side, else its own", p.refdes);
        if spot == Some(m.comp) {
            assert_eq!(poses[i], hand[i], "{name}: seed: {}, fixed to a spot, is put back on it from the board's middle", p.refdes);
        }
        if fixed.contains(&m.comp) {
            assert_eq!(poses[i], hand[i], "{name}: seed: {}, fixed by the circuit, keeps its pose", p.refdes);
        }
    }
    // Each edge part sits flush on its edge (its pads may hold it off by up to the edge rule), and
    // its plug faces out across it.
    let seeded = posed(board, problem, &poses, &[]);
    let out = seeded.outline_bbox();
    let edge_rule = resolve_rules(board, None).edge();
    for &(comp, edge, _) in &slots {
        let p = seeded.placed_of(comp).expect("an edge part is placed");
        let court = placed_courtyard(p).bbox();
        let (gap, n) = match edge {
            Edge::Left => (court.min.x - out.min.x, (-1.0, 0.0)),
            Edge::Right => (out.max.x - court.max.x, (1.0, 0.0)),
            Edge::Bottom => (court.min.y - out.min.y, (0.0, -1.0)),
            Edge::Top => (out.max.y - court.max.y, (0.0, 1.0)),
        };
        assert!((0..=edge_rule).contains(&gap), "{name}: seed: {} sits flush on its {edge:?} edge ({gap} nm in, edge rule {edge_rule})", p.refdes);
        let Some(body) = circuit.comps[comp.ix()].part.body.as_ref() else { continue };
        for port in body.ports.iter().filter(|port| port.kind == BodyPortKind::Plug) {
            let d = body.offset.rotate(port.dir);
            let x = if p.side == Side::Bottom { -d[0] } else { d[0] };
            let (sin, cos) = p.rot.radians().sin_cos();
            let dir = (x * cos - d[1] * sin, x * sin + d[1] * cos);
            assert!(dir.0 * n.0 + dir.1 * n.1 > 0.7, "{name}: seed: {}'s plug faces out across its {edge:?} edge: {dir:?}", p.refdes);
        }
    }
    if name == "ledctl" {
        // A locked part is no movable: it stays, and the rest pack round it.
        let mut locked = board.clone();
        let k = (0..locked.placed.len()).filter(|&k| !fixed.contains(&locked.placed[k].comp) && !slotted.contains(&locked.placed[k].comp))
            .max_by_key(|&k| (locked.placed[k].footprint.pads.len(), std::cmp::Reverse(k))).expect("parts");
        locked.placed[k].locked = true;
        let kept = locked.placed[k].clone();
        let problem = PlaceProblem::new(&locked, &[]);
        assert!(problem.movables.iter().all(|m| m.item != kept.id), "{name}: seed: a locked part is no movable");
        let mut poses: Vec<_> = problem.movables.iter().map(|m| m.start).collect();
        let report = seed(&locked, circuit, &problem, &mut poses, &SeedOpts::default());
        assert!(report.unplaced.is_empty(), "{name}: seed round a locked part: every part finds room");
        let b = posed(&locked, &problem, &poses, &[]);
        legal(name, "seed round a locked part", &b, &problem, &edge_parts(circuit));
        let p = b.placed_item(kept.id).expect("the locked part");
        assert_eq!((p.at, p.rot, p.side), (kept.at, kept.rot, kept.side), "{name}: seed: the locked {} stays", kept.refdes);
    }
    let (ours, theirs) = (Cost::new(problem, &poses).terms().wirelength, Cost::new(problem, &hand).terms().wirelength);
    assert!(ours * 10 <= theirs * 16, "{name}: seed: wirelength {ours} within 1.6 x the hand placement's {theirs}");

    // A capacitor's pad to its IC's pad on the same net, nearest pair.
    let index = |c| problem.movables.iter().position(|m| m.comp == c).expect("a decoupling part is movable");
    let gap = |poses: &[Pose], cap: usize, ic: usize| {
        let (a, b) = (&problem.movables[cap], &problem.movables[ic]);
        let mut best = f64::MAX;
        for (i, net) in a.pad_nets.iter().enumerate() {
            for (j, other) in b.pad_nets.iter().enumerate() {
                if net.is_some() && net == other {
                    best = best.min(a.pad_at(PadIx(i as u16), poses[cap]).dist(b.pad_at(PadIx(j as u16), poses[ic])));
                }
            }
        }
        best
    };
    assert!(!report.decoupling.is_empty(), "{name}: seed: decoupling capacitors are inferred");
    let gaps: Vec<_> = report.decoupling.iter().map(|&(c, u)| (gap(&poses, index(c), index(u)), gap(&hand, index(c), index(u)))).collect();
    let worst_hand = gaps.iter().map(|g| g.1).fold(2e6, f64::max);
    assert!(gaps.iter().all(|g| g.0 <= worst_hand), "{name}: seed: each decoupling capacitor sits no farther from its IC than the hand placement's farthest (or 2 mm): (ours, hand) nm {gaps:?}");

    let mut again = start.clone();
    assert_eq!(seed(board, circuit, problem, &mut again, &SeedOpts::default()), report, "{name}: seed: the same report twice");
    assert_eq!(again, poses, "{name}: seed: the same placement twice");

    let near_broken = |poses: &[Pose]| broken_nears(&posed(board, problem, poses, &[]), circuit);
    // The small fixtures hold every `near`. On drum16 a voice's op-amp can take the last pocket
    // of room before its capacitors go on in the packed seed.
    let small = name != "drum16-placed";
    assert!(!small || near_broken(&poses).is_empty(), "{name}: seed: every near intent holds: {:?}", near_broken(&poses));

    // Spacing on (a 1 mm halo round every part): every part still finds room legally, each
    // decoupling capacitor still sits by its IC, and every `near` still holds.
    let halos = board.placed.iter().map(|p| (p.id, 1_000_000)).collect();
    let mut spaced = start.clone();
    let report = seed(board, circuit, problem, &mut spaced, &SeedOpts { halos, ..SeedOpts::default() });
    assert!(report.unplaced.is_empty(), "{name}: spaced seed: every part finds room: {:?}", report.unplaced);
    legal(name, "spaced seed", &posed(board, problem, &spaced, &[]), problem, &edge_parts(circuit));
    assert_ne!(spaced, poses, "{name}: spacing moves parts");
    let gaps: Vec<_> = report.decoupling.iter().map(|&(c, u)| (gap(&spaced, index(c), index(u)), gap(&hand, index(c), index(u)))).collect();
    assert!(gaps.iter().all(|g| g.0 <= worst_hand), "{name}: spaced seed: each decoupling capacitor sits no farther from its IC than the hand placement's farthest (or 2 mm): (ours, hand) nm {gaps:?}");
    assert!(!small || near_broken(&spaced).is_empty(), "{name}: spaced seed: every near intent holds: {:?}", near_broken(&spaced));

    // By demand: rounds that read the routing demand map of the round before. Every part still
    // finds room legally, within the wirelength bound, and the map's overflow is no higher than
    // the packed seed's. On handheld, whose packed seed leaves nets open in an overflowing band,
    // it falls.
    use super::congest::{congestion, default_cell};
    let mut quiet = start.clone();
    let demand = SeedOpts { demand: true, ..SeedOpts::default() };
    let mut told = Told { round_poses: (name == "ledavr").then(Vec::new), ..Told::default() };
    let took = Instant::now();
    let report = seed_watched(board, circuit, problem, &mut quiet, &demand, &mut told, Some(pool));
    let took = took.elapsed();
    let Told { last_round, told_over, matched, round_poses, .. } = told;
    assert_eq!(report.rounds, report.passes.plain + report.passes.grown);
    assert_eq!(report.passes.failed, report.passes.grown + 2, "one plain stall per grown success, then both fail");
    if let Some(fallback) = round_poses {
        let mut control = start.clone();
        let mut plain = Told { round_poses: Some(Vec::new()), ..Told::default() };
        let opts = SeedOpts { plain_only: true, ..demand.clone() };
        let report = seed_watched(board, circuit, problem, &mut control, &opts, &mut plain, Some(pool));
        assert_eq!((report.passes.grown, report.passes.failed), (0, 1));
        let plain = plain.round_poses.unwrap();
        assert!(plain.len() > 1, "the prefix includes a kept demand round after round zero");
        assert!(fallback.starts_with(&plain), "every pose through the first plain stall agrees");
    }
    assert!(report.unplaced.is_empty(), "{name}: demand seed: every part finds room: {:?}", report.unplaced);
    legal(name, "demand seed", &posed(board, problem, &quiet, &[]), problem, &edge_parts(circuit));
    let cell = default_cell(board);
    let (over, packed_over) = (congestion(board, problem, &quiet, cell).over(), congestion(board, problem, &poses, cell).over());
    let wirelength = Cost::new(problem, &quiet).terms().wirelength;
    // The gate's map row (seeded overflow no higher than the hand placement's), printed until the
    // measured routes show it separates the boards that route from those that do not.
    let hand_over = congestion(board, problem, &hand, cell).over();
    eprintln!("{name}: demand seed in {took:.1?}: {} rounds, overflow {:.3} mm (packed {:.3}, hand {:.3}), wirelength {:.3} mm (packed {:.3}, hand {:.3})",
        report.rounds, over as f64 / 1e6, packed_over as f64 / 1e6, hand_over as f64 / 1e6, wirelength as f64 / 1e6, ours as f64 / 1e6, theirs as f64 / 1e6);
    assert!(wirelength * 10 <= theirs * 16, "{name}: demand seed: wirelength {wirelength} within 1.6 x the hand placement's {theirs}");
    let (_, before, mut previous_over) = last_round.expect("the seed reported round zero");
    let groups = identical_instances(board, circuit);
    assert_eq!(report.conformed.len(), groups.len(), "{name}: every group has a decision");
    assert_eq!(matched, report.conformed.iter().enumerate().map(|(i, row)| (i, groups.len(), row.blocks[0])).collect::<Vec<_>>(),
        "{name}: watch hears each group as conform starts it, with its first copy's block");
    assert!(over <= previous_over, "{name}: conform must preserve or reduce the last demand round's overflow");
    let mut told = told_over.iter();
    for (group, row) in groups.iter().zip(&report.conformed) {
        let part = &circuit.comps[group[0].members[0].ix()].path;
        eprintln!("CONFORM {name} {part} {row:?}");
        assert_eq!((row.copies, row.members_per_copy), (group.len(), group[0].members.len()), "{name}: reported dimensions of {part}");
        if row.status != ConformStatus::Kept { continue }
        previous_over += row.delta_over.expect("a kept group has an overflow delta");
        assert_eq!(told.next(), Some(&previous_over), "{name}: watch must receive the kept overflow for {part}");
        let exemplar = &group[row.exemplar.expect("a kept group names its exemplar")];
        let a = (0..exemplar.members.len()).min_by_key(|&k| std::cmp::Reverse(problem.movables[index(exemplar.members[k])].pads.len())).unwrap();
        let origin = before[index(exemplar.members[a])];
        for copy in group {
            let anchor = quiet[index(copy.members[a])];
            for (&m, &e) in copy.members.iter().zip(&exemplar.members) {
                let (to, from) = (quiet[index(m)], before[index(e)]);
                let turn = anchor.rot.sub(origin.rot);
                assert_eq!(to.at.sub(anchor.at), from.at.sub(origin.at).rotated(turn), "{name}: {part} member {m:?} must match the exemplar's offset");
                assert_eq!(to.rot, from.rot.add(turn), "{name}: {part} member {m:?} must match the exemplar's rotation");
            }
        }
    }
    assert_eq!(told.next(), None, "{name}: watch only repeats a round for a kept group");
    assert_eq!(over, previous_over, "{name}: kept groups' deltas must reproduce final overflow");
    if matches!(name, "ledctl" | "ledavr") {
        assert_eq!(report.conformed[0].status, ConformStatus::Kept, "{name}: the lamps must conform");
        let blocks: Vec<_> = report.conformed[0].blocks.iter().map(|b| circuit.tree[b.ix()].path.as_str()).collect();
        assert_eq!(blocks, (0..8).map(|i| format!("lamps[{i}]")).collect::<Vec<_>>(), "{name}: each copy is named by its lamp's block");
    }
    if matches!(name, "handheld" | "tonebox") {
        assert!(report.conformed.is_empty(), "{name}: no repeated groups to conform");
        assert_eq!(quiet, before, "{name}: conform must preserve the last round bit for bit");
    }
    let (broken_before, broken_after) = (near_broken(&before), near_broken(&quiet));
    assert!(broken_after.iter().all(|(id, _)| broken_before.iter().any(|(old, _)| id == old)),
        "{name}: conform broke a held near: {broken_before:?} before, {broken_after:?} after");
    eprintln!("{name}: broken nears before {broken_before:?}, after {broken_after:?}");
    if report.rounds == 0 && told_over.is_empty() {
        assert_eq!(over, packed_over, "{name}: zero rounds and no kept group preserves packed overflow");
    } else {
        assert!(over < packed_over, "{name}: demand seed with a kept round or group must beat packed overflow");
    }
    if name == "handheld" {
        assert!(over < packed_over, "{name}: demand seed: the overflow falls against the packed seed's ({over} against {packed_over})");
    }

    // With no held group no seat is tried, so the seed takes the path it took before seats
    // (measured once over these five fixtures: every pose and decision the same).
    assert!(report.seat_guard.is_none() && report.conformed.iter().all(|r| r.seat_trial.is_none()), "{name}: no held mates");

    // Serial and pooled schedules agree. drum16's serial conform alone takes seconds, so the
    // lamps carry the check: their trials share the pool as drum16's do.
    if name != "drum16-placed" {
        let mut serial = start;
        let serial_report = seed(board, circuit, problem, &mut serial, &demand);
        assert_eq!(quiet, serial, "{name}: pooled and serial demand seeding give identical poses");
        assert_eq!(decisions(report), decisions(serial_report), "{name}: every decision and placement metric must agree across schedules");
    }
}

#[test]
fn congestion_map_readout() {
    use super::congest::{congestion, default_cell, lattice};
    use fab_elec_core::Contour;

    let board = board("ledctl");
    let mut problem = PlaceProblem::new(&board, &[]);
    let poses: Vec<_> = problem.movables.iter().map(|m| m.start).collect();
    let cell = default_cell(&board);
    let corner = board.outline_bbox().min;
    let map = lattice(&board, &problem, &poses, cell, corner);
    let free = congestion(&board, &problem, &poses, cell);
    assert_eq!(free, congestion(&board, &problem, &poses, cell), "a second map must be identical");

    let (mut pins, mut hpwl) = (0, 0);
    for net in problem.nets.iter().filter(|n| n.weight > 0) {
        let mut bb = BBox::EMPTY;
        for pin in &net.pins {
            let p = match *pin {
                PinRef::Fixed(p) => p,
                PinRef::Mov(m, pad) => problem.movables[m].pad_at(pad, poses[m]),
            };
            pins += 1;
            bb = bb.with(p);
        }
        hpwl += (i128::from(bb.width() + bb.height()) * i128::from(net.weight)
            / i128::from(PNet::UNIT_WEIGHT)) as i64;
    }
    let wire: i64 = (0..map.w * map.h).flat_map(|i| map.wire_in(i)).map(|&(_, nm)| nm).sum();
    assert_eq!(wire, hpwl, "wire demand must conserve the original weighted half-perimeters");
    assert_eq!(map.demand.iter().sum::<i64>() - wire, pins * i64::from(cell), "each pin costs one cell edge, all of it on the board");

    let old_len = problem.nets.len();
    problem.nets.retain(|n| n.weight != 0);
    assert!(problem.nets.len() < old_len, "ledctl must exercise a zero-weight zone net");
    assert_eq!(free, congestion(&board, &problem, &poses, cell), "zero-weight nets must add no wire or pin demand");

    // Ledctl's rectangular outline fills its box. Cut away a cell in its empty lower left
    // corner and half of its neighbour, retaining the original grid and all hand poses.
    let mut cut = (*board).clone();
    let at = corner.add(P::new(cell, cell));
    cut.outline.push(Contour::new(fab_elec_core::Path::new(at)
        .line(at.add(P::new(2 * cell, 0))).line(at.add(P::new(cell, cell)))
        .line(at.add(P::new(0, cell)))));
    // Two overlapping track keepouts together fill the next cell on the front layer.
    let near = at.add(P::new(2 * cell, 0));
    for (lo, hi) in [(0, 3 * cell / 4), (cell / 4, cell)] {
        let id = cut.alloc_id();
        cut.keepouts.push(fab_elec_board::BoardKeepout {
            id, area: fab_elec_core::PolySet::rect(BBox::new(near.add(P::new(lo, 0)), near.add(P::new(hi, cell)))),
            layers: fab_elec_core::LayerSet::one(fab_elec_core::LayerId::F_CU),
            forbid: fab_elec_parts::Forbid::TRACKS, reason: String::new(), origin: fab_elec_board::Origin::Authored, z: None,
        });
    }
    let outside = lattice(&cut, &problem, &poses, cell, corner);
    let i = map.w + 1;
    assert!(map.capacity[i] > 0, "cutout starts in a cell with routing capacity");
    assert!(map.capacity[i + 2] > 0, "keepouts start in a cell with routing capacity");
    assert_eq!(2 * outside.capacity[i + 2], map.capacity[i + 2], "overlapping keepouts consume their union on one layer");
    assert_eq!(outside.capacity[i], 0, "a cell wholly outside the outline has zero capacity");
    assert!(map.capacity[i+1] > 0 && (2 * outside.capacity[i+1] - map.capacity[i+1]).abs()
        <= board.stackup.copper().len() as i64, "a diagonal cutout removes half its neighbour's capacity, up to layer rounding");
    check_phase_free("ledctl cut", &cut, &problem, &poses, corner);
}

/// The map reads the same wherever its grid falls (one lattice swings 21 to 47 mm on the hand
/// handheld under 0.3 mm shifts; pins charged to one cell still alias at the MCU's pin pitch, 9%),
/// and it is its definition, built fast.
#[test]
fn phase_free_congestion_map() {
    use super::congest::{congestion, congestion_through, default_cell, PHASES};
    let board = board("handheld");
    let problem = PlaceProblem::new(&board, &[]);
    let hand: Vec<_> = problem.movables.iter().map(|m| m.start).collect();
    let (cell, corner) = (default_cell(&board), board.outline_bbox().min);
    let step = cell / PHASES as Nm;
    check_phase_free("handheld", &board, &problem, &hand, corner);
    check_phase_free("handheld shifted", &board, &problem, &hand, P::new(corner.x - step / 3, corner.y - 2 * step / 3));
    let over = congestion(&board, &problem, &hand, cell).over();
    for (dx, dy) in [(step / 2, 0), (0, step / 2), (step / 2, step / 2)] {
        let moved = congestion_through(&board, &problem, &hand, cell, P::new(corner.x - dx, corner.y - dy)).over();
        assert!(20 * (moved - over).abs() <= over, "handheld: overflow {moved} with the grid moved by ({dx}, {dy}) nm, against {over}: within 5%");
    }
}

/// `congestion` against its definition: each quarter cell holds the mean of the `PHASES`^2
/// lattices' cells covering it, here each lattice built directly. The two round each spread share
/// differently. The carried remainder keeps every cell within 1 nm of its exact share, so a
/// lattice's cell (one share) and the window of `PHASES`^2 raster cells standing for it (as many
/// shares) differ by at most `PHASES`^2 nm for each spread touching it: a net's wire or a pin's
/// escape. Mean over the lattices, that is at most the count of spreads touching the covering
/// cells. Free area sums exactly (the rectangular outline, and the cutout's 45 degree edge, cross
/// grid lines at whole nm), so capacity agrees exactly, and overflow by no more than demand.
fn check_phase_free(name: &str, board: &Board, problem: &PlaceProblem, poses: &[Pose], at: P) {
    use super::congest::{congestion_through, default_cell, lattice, pitch, CongestionMap, PHASES};
    use fab_elec_core::NetId;
    use std::collections::BTreeMap;
    let n = PHASES;
    let (cell, pitch) = (default_cell(board), pitch(board));
    let step = cell / n as Nm;
    let fast = congestion_through(board, problem, poses, cell, at);
    let lattices: Vec<CongestionMap> = (0..n * n)
        .map(|k| lattice(board, problem, poses, cell, P::new(at.x - (k % n) as Nm * step, at.y - (k / n) as Nm * step)))
        .collect();
    // Boxes holding each spread, a little larger than the spreads themselves.
    let mut spreads = Vec::new();
    for net in problem.nets.iter().filter(|net| net.weight > 0) {
        let mut pins = BBox::EMPTY;
        for pin in &net.pins {
            let p = match *pin { PinRef::Fixed(p) => p, PinRef::Mov(m, pad) => problem.movables[m].pad_at(pad, poses[m]) };
            spreads.push(BBox::point(p).inflate(cell / 4 + 1));
            pins = pins.with(p);
        }
        spreads.push(pins.inflate(pitch / 2 + 1));
    }
    let touching: Vec<Vec<i64>> = lattices.iter().map(|l| {
        let mut count = vec![0; l.w * l.h];
        for &s in &spreads {
            l.cells_under(s).for_each(|i| count[i] += 1);
        }
        count
    }).collect();
    let mean = |sum: i64| (sum + (n * n / 2) as i64).div_euclid((n * n) as i64);
    let total: i64 = touching.iter().flatten().sum();
    let naive = mean(lattices.iter().map(|l| l.over()).sum());
    assert!((fast.over() - naive).abs() <= total, "{name}: over {} against the lattices' mean {naive}, within {total}", fast.over());
    for j in 0..fast.w * fast.h {
        let corner = fast.cell_box(j).min;
        let cells: Vec<usize> = lattices.iter().map(|l| l.cell_at(corner)).collect();
        let sum = |of: &dyn Fn(&CongestionMap, usize) -> i64| mean(lattices.iter().zip(&cells).map(|(l, &i)| of(l, i)).sum());
        let within: i64 = touching.iter().zip(&cells).map(|(t, &i)| t[i]).sum();
        assert_eq!(fast.capacity[j], sum(&|l, i| l.capacity[i]), "{name}: capacity of quarter cell {j} at {corner:?}");
        let (demand, overflow) = (sum(&|l, i| l.demand[i]), sum(&|l, i| l.overflow(i)));
        assert!((fast.demand[j] - demand).abs() <= within && (fast.overflow(j) - overflow).abs() <= within,
            "{name}: quarter cell {j} at {corner:?}: demand {} against {demand}, overflow {} against {overflow}, within {within}",
            fast.demand[j], fast.overflow(j));
        let mut nets: BTreeMap<NetId, (i64, i64)> = BTreeMap::new();
        for (l, &i) in lattices.iter().zip(&cells) {
            l.wire_in(i).iter().for_each(|&(net, nm)| nets.entry(net).or_default().1 += nm);
        }
        fast.wire_in(j).iter().for_each(|&(net, nm)| nets.entry(net).or_default().0 += nm);
        for (net, (ours, theirs)) in nets {
            let theirs = mean(theirs);
            assert!((ours - theirs).abs() <= (n * n) as i64, "{name}: quarter cell {j}: net {net:?}'s wire {ours} against {theirs}");
        }
    }
}

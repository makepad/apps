//! Exercise the worker channel against three nets of the LED controller fixture: a stream of
//! verified proposals, replayed byte-identically step by step, and cancellation.

use fab_elec_board::zone::fill_all;
use fab_elec_board::{connect, drc, resolve_rules, Anchor, Board, BoardDelta, BoardOp};
use fab_elec_core::{mm, BoardId, Canon, ItemId, LayerId, P};
use fab_elec_fixtures::ledctl;
use fab_elec_route::check::{new_or_worse, FindingNets};
use fab_elec_route::{
    replay, route_channel, scope_nets, Base, Effort, Engine, Gesture, GestureId, GestureKind, GestureSample, JobId, JobStatus,
    Method, ProposalSource, RouteEvent, RouteHandle, RouteJob, RouteProposal, RouteRequest, RouteScope, RouteWorker,
    SampleCause, Spawn, Tier, WidthRule,
};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Default)]
struct ThreadSpawn {
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Spawn for ThreadSpawn {
    fn spawn(&self, task: Box<dyn FnOnce() + Send>) {
        let task = thread::spawn(task);
        self.tasks.lock().unwrap().push(task);
    }
}

/// A pool that runs its tasks only when the test says so, on the test's thread.
#[derive(Default)]
struct Queue(Mutex<Vec<Box<dyn FnOnce() + Send>>>);

impl Spawn for Queue {
    fn spawn(&self, task: Box<dyn FnOnce() + Send>) {
        self.0.lock().unwrap().push(task);
    }
}

impl Queue {
    fn run(&self) {
        loop {
            let task = self.0.lock().unwrap().pop();
            let Some(task) = task else {
                return;
            };
            task();
        }
    }
}

fn next_event(handle: &RouteHandle, wakes: &Receiver<()>) -> RouteEvent {
    loop {
        if let Some(event) = handle.try_recv() {
            return event;
        }
        wakes.recv_timeout(Duration::from_secs(30)).expect("worker event timed out or its wake channel closed");
    }
}

/// Keep status publications while waiting for an answer.
fn next_non_status(handle: &RouteHandle, wakes: &Receiver<()>, statuses: &mut Vec<JobStatus>) -> RouteEvent {
    loop {
        match next_event(handle, wakes) {
            RouteEvent::Status(status) => statuses.push(status),
            event => return event,
        }
    }
}

/// The job's taken proposals through the last, and statuses received before that last proposal.
fn job_stream(handle: &RouteHandle, wakes: &Receiver<()>, job: JobId) -> (Vec<RouteProposal>, Vec<JobStatus>) {
    let mut stream = Vec::new();
    let mut statuses = Vec::new();
    loop {
        let RouteEvent::Proposal(proposal) = next_non_status(handle, wakes, &mut statuses) else {
            panic!("routing must publish a proposal");
        };
        assert!(
            matches!(proposal.source, ProposalSource::Job { job: source, .. } if source == job),
            "a proposal answers the requested job"
        );
        let last = proposal.last;
        stream.push(proposal);
        if last {
            return (stream, statuses);
        }
    }
}

/// A base of `board` with its own fills and connectivity.
fn base_of(board: Arc<Board>) -> Base {
    let rules = Arc::new(resolve_rules(&board, None));
    let fills = Arc::new(fill_all(&board, &rules));
    let conn = Arc::new(connect(&board, &fills));
    Base::new(board, rules, conn, fills)
}

/// `board` with a part far from the drawn gesture moved: a new generation and content hash.
fn moved(board: &Board) -> Arc<Board> {
    let mut board = board.clone();
    let far = board.placed.iter().find(|p| p.path == "lamps[0].r").expect("fixture setup: a far part exists").id;
    board.apply_all(&[BoardOp::Move { item: far, by: P::mm(0.5, 0.0) }]).expect("fixture setup: the far part moves");
    Arc::new(board)
}

#[test]
fn samples_are_answered_on_the_held_base_parked_ahead_of_it_and_again_after_each_base() {
    let design = ledctl();
    let board: Arc<Board> = design.units[0].board.clone().expect("fixture setup: ledctl unit 0 has a board");
    let unit = BoardId(0);
    assert_eq!(board.unit, unit, "fixture setup: the sample's unit is ledctl unit 0");
    let generation = board.generation;
    let (wake, wakes) = mpsc::channel();
    let (handle, inbox) = route_channel(move || {
        let _ = wake.send(());
    });
    let worker = RouteWorker::new(inbox, Arc::new(ThreadSpawn::default()));
    let worker = thread::spawn(move || worker.run());
    // A head from the outer resistor's signal pad round its power pad, in open space.
    let placed = board.placed.iter().find(|p| p.path == "lamps[7].r").expect("fixture setup: the outer resistor exists");
    let (signal, power) = (placed.footprint.pad("2").expect("its signal pad"), placed.footprint.pad("1").expect("its power pad"));
    let s = placed.pad_at(signal).expect("the signal pad has a position");
    let centre = placed.pad_at(power).expect("the power pad has a position");
    let start = Anchor::Pad { placed: placed.id, pad: signal };
    let id = GestureId(41);
    let sample = |seq, generation, kind| {
        let mut gesture = Gesture::new(kind, unit, generation, start, s, LayerId::F_CU);
        gesture.cursor = centre.add(P::mm(2.0, 0.0));
        gesture.trail = vec![s, centre.add(P::mm(1.0, 1.0)), gesture.cursor];
        gesture.width = WidthRule::Fixed(mm(0.25));
        GestureSample { id, seq, gesture }
    };
    let gnd = board.net("GND").expect("fixture setup: GND exists");
    // Explaining a joined net is published at once: the next event after it shows nothing else was.
    let nothing_else = |what: &str| {
        handle.request(RouteRequest::Explain { unit, net: gnd }).expect("the explanation fits the queue");
        assert!(matches!(next_event(&handle, &wakes), RouteEvent::Explain(e) if e.net == gnd), "{what}");
    };
    let answered = |what: &str| match next_event(&handle, &wakes) {
        RouteEvent::Proposal(p) => p,
        other => panic!("{what}: expected an answer, got {other:?}"),
    };

    handle.sample(sample(1, generation, GestureKind::Route));
    let RouteEvent::Refused(refused) = next_event(&handle, &wakes) else {
        panic!("no base: the worker answers the sample with a refusal");
    };
    assert_eq!((refused.id, refused.seq, refused.unit, refused.cause), (id, 1, unit, SampleCause::NoBase),
        "no base: the refusal names the sample's gesture, seq and unit, and the missing base");
    assert_eq!(refused.text, "no board for this unit yet", "no base: the refusal provides the canvas text");

    let b0 = base_of(board.clone());
    handle.request(RouteRequest::Base(b0.clone())).expect("the base fits the queue");
    let p = answered("a base answers the live gesture's newest sample again");
    assert_eq!((p.source, p.base_generation, p.base_hash), (ProposalSource::Gesture { id, seq: 1, tier: Tier::Stretch }, generation, b0.hash),
        "the answer names its sample and the base it was made on");
    assert!(p.verified && p.head.as_ref().is_some_and(|h| h.blocked.is_none()), "the head round the pad is legal: {:?}", p.collisions);

    let b1 = base_of(moved(&board));
    handle.sample(sample(2, b1.board.generation, GestureKind::Route));
    nothing_else("a sample drawn on a newer board than the worker's is parked until its base arrives");
    handle.request(RouteRequest::Base(b1.clone())).expect("the base fits the queue");
    let p = answered("the parked sample is answered when its base arrives");
    assert_eq!((p.source, p.base_generation, p.base_hash), (ProposalSource::Gesture { id, seq: 2, tier: Tier::Stretch }, b1.board.generation, b1.hash),
        "the parked sample is answered on the base it waited for");

    // The cursor rests: no new sample. A base that changes under it answers the rested sample again.
    let b2 = base_of(moved(&b1.board));
    handle.request(RouteRequest::Base(b2.clone())).expect("the base fits the queue");
    let p = answered("a base that changes under a rested cursor answers its newest sample again");
    assert_eq!((p.source, p.base_generation, p.base_hash), (ProposalSource::Gesture { id, seq: 2, tier: Tier::Stretch }, b2.board.generation, b2.hash),
        "the re-answer carries the new base's generation and hash, for a release to commit on");
    let refilled = Base::new(b2.board.clone(), b2.rules.clone(), b2.conn.clone(), Arc::new((*b2.fills).clone()));
    handle.request(RouteRequest::Base(refilled)).expect("the base fits the queue");
    nothing_else("a base of the same board generation is covered by the answer already sent");

    handle.sample(sample(3, b2.board.generation, GestureKind::Fanout(ItemId(0))));
    let RouteEvent::Refused(refused) = next_event(&handle, &wakes) else {
        panic!("a gesture kind the worker cannot answer is refused");
    };
    assert_eq!((refused.seq, refused.cause), (3, SampleCause::Unsupported), "a fanout is refused as unsupported until its tier exists");

    handle.request(RouteRequest::End(id)).expect("the end fits the queue");
    handle.request(RouteRequest::Base(base_of(moved(&b2.board)))).expect("the base fits the queue");
    nothing_else("an ended gesture is not answered after a base");

    drop(handle);
    worker.join().expect("sample answers: dropping the handle stops the worker without panicking");
}

#[test]
fn worker_routes_and_cancels() {
    let design = ledctl();
    let board: Arc<Board> = design.units[0].board.clone().expect("fixture setup: ledctl unit 0 has a board");
    assert_eq!(board.unit, BoardId(0), "fixture setup: use unit 0");
    let rules = Arc::new(resolve_rules(&board, None));
    let fills = Arc::new(fill_all(&board, &rules));
    let conn = Arc::new(connect(&board, &fills));
    let original = Base::new(board, rules, conn, fills);
    let nets: Vec<_> = scope_nets(&original.board, &original.conn, &RouteScope::All).into_iter().take(3).collect();
    assert_eq!(nets.len(), 3, "fixture setup: three unrouted nets");
    let gnd = original.board.net("GND").expect("fixture setup: GND exists");
    assert!(!original.conn.unrouted.contains(&gnd), "fixture setup: GND is joined by its pour");
    assert!(original.conn.unrouted.contains(&nets[0]), "fixture setup: the first route net is open");
    let generation = original.board.generation;
    let mut newest = (*original.board).clone();
    newest.generation = generation + 1;
    let base = Base::new(Arc::new(newest), original.rules.clone(), original.conn.clone(), original.fills.clone());

    let (wake, wakes) = mpsc::channel();
    let (handle, inbox) = route_channel(move || {
        let _ = wake.send(());
    });
    let spawn = Arc::new(ThreadSpawn::default());
    let worker = RouteWorker::new(inbox, spawn.clone());
    let worker = thread::spawn(move || worker.run());
    handle.request(RouteRequest::Base(original)).unwrap();
    handle.request(RouteRequest::Base(base.clone())).unwrap();
    let mut job = RouteJob {
        job: JobId(1),
        unit: base.unit,
        scope: RouteScope::Nets(nets.clone()),
        method: Method::Interactive,
        engine: Engine::Grid,
        style: None,
        effort: Effort::default(),
        layers: None,
        reroute: false,
    };
    handle.request(RouteRequest::Route(job.clone())).unwrap();
    let (stream, statuses) = job_stream(&handle, &wakes, job.job);
    assert!(
        statuses.iter().any(|status| status.job == job.job && status.total == nets.len() as u32),
        "status: the running job publishes its net count before its last proposal"
    );
    let first = stream.last().expect("a job ends with its final proposal").clone();
    assert_eq!(first.base_generation, generation + 1, "the newest base answers");
    if !first.verified {
        let had = drc(&base.board, &base.conn, &base.rules, &base.fills, None).violations;
        let was = FindingNets::of(&base.board, &base.conn);
        let new: Vec<_> = first
            .checked
            .iter()
            .flat_map(|checked| {
                let now = FindingNets::of(&checked.board, &checked.conn);
                new_or_worse(&had, &was, &checked.violations, &now).into_iter().map(|i| checked.violations[i].clone())
            })
            .collect();
        panic!("the route is verified: {} ({:?}); new findings {new:#?}", first.label, first.metrics);
    }
    assert!(!first.ops.is_empty(), "the fixture route adds copper");
    let mut applied = (*base.board).clone();
    applied.apply_all(&first.ops).expect("the final ops apply to the base board");
    let checked = first.checked.as_ref().expect("a verified proposal has a checked board");
    assert_eq!(checked.board.content_hash(), applied.content_hash(), "checked is the applied board");
    let fills = fill_all(&checked.board, &base.rules);
    assert_eq!(
        *checked.conn,
        connect(&checked.board, &fills),
        "the indexed connectivity check equals the full check"
    );
    assert_eq!(
        *checked.violations,
        drc(&checked.board, &checked.conn, &base.rules, &fills, None).violations,
        "the indexed DRC check equals the full check"
    );

    for proposal in &stream {
        assert!(
            proposal.verified,
            "every proposal of the stream is verified: {} at {:?}",
            proposal.label,
            proposal.source
        );
    }
    let steps = stream
        .iter()
        .filter(|proposal| !proposal.last)
        .map(|proposal| match proposal.source {
            ProposalSource::Job { step, .. } => step,
            ProposalSource::Gesture { .. } => panic!("a job's proposal names its step"),
        })
        .collect();
    let replayed = replay(base.clone(), &job, steps);
    assert_eq!(replayed.len(), stream.len(), "the replay rebuilds the recorded partial steps and the final one");
    for (recorded, replayed) in stream.iter().zip(&replayed) {
        assert!(
            recorded.canon_bytes() == replayed.canon_bytes(),
            "the proposal at {:?} replays byte-identically",
            recorded.source
        );
    }

    job.job = JobId(2);
    handle.request(RouteRequest::Route(job.clone())).unwrap();
    handle.request(RouteRequest::Cancel(job.job)).unwrap();
    let (mut stream, _) = job_stream(&handle, &wakes, job.job);
    let cancelled = stream.pop().expect("a cancelled job ends with its final proposal");
    let mut applied = (*base.board).clone();
    applied.apply_all(&cancelled.ops).expect("a cancelled job ends legal");

    handle.request(RouteRequest::Explain { unit: base.unit, net: gnd }).unwrap();
    handle.request(RouteRequest::Explain { unit: base.unit, net: nets[0] }).unwrap();
    for (net, message) in [(gnd, "a connected net has no reason"), (nets[0], "a routable net has no reason")] {
        let RouteEvent::Explain(explanation) = next_event(&handle, &wakes) else {
            panic!("an explain request must publish an explanation");
        };
        assert_eq!((explanation.unit, explanation.net), (base.unit, net), "explanations answer in order");
        assert_eq!(explanation.reason, None, "{message}");
    }

    drop(handle);
    worker.join().expect("dropping the handle stops the worker");
    let tasks = std::mem::take(&mut *spawn.tasks.lock().unwrap());
    for task in tasks {
        task.join().expect("route jobs finish without panicking");
    }
}

#[test]
fn a_base_without_a_delta_updates_like_a_fresh_unit() {
    let design = ledctl();
    let board: Arc<Board> = design.units[0].board.clone().expect("fixture setup: ledctl unit 0 has a board");
    assert_eq!(board.unit, BoardId(0), "fixture setup: use unit 0");
    let rules = Arc::new(resolve_rules(&board, None));
    let fills = Arc::new(fill_all(&board, &rules));
    let conn = Arc::new(connect(&board, &fills));
    let b0 = Base::new(board, rules, conn, fills);
    let placed = b0.board.placed.iter().find(|placed| placed.pad_nets.iter().any(Option::is_some))
        .expect("fixture setup: a placed part has a pad on a net");
    let mut board = (*b0.board).clone();
    board.apply_all(&[BoardOp::Move { item: placed.id, by: P::mm(0.5, 0.0) }])
        .expect("the fixture part moves");
    let board = Arc::new(board);
    let fills = Arc::new(fill_all(&board, &b0.rules));
    let conn = Arc::new(connect(&board, &fills));
    let b1 = Base::new(board, b0.rules.clone(), conn, fills);
    let nets: Vec<_> = scope_nets(&b1.board, &b1.conn, &RouteScope::All).into_iter().take(3).collect();
    assert_eq!(nets.len(), 3, "fixture setup: three unrouted nets");
    let job = RouteJob {
        job: JobId(1),
        unit: b1.unit,
        scope: RouteScope::Nets(nets),
        method: Method::Interactive,
        engine: Engine::Grid,
        style: None,
        effort: Effort::Full,
        layers: None,
        reroute: false,
    };

    let spawn = Arc::new(ThreadSpawn::default());
    let (wake_a, wakes_a) = mpsc::channel();
    let (handle_a, inbox_a) = route_channel(move || {
        let _ = wake_a.send(());
    });
    let worker_a = RouteWorker::new(inbox_a, spawn.clone());
    let worker_a = thread::spawn(move || worker_a.run());
    handle_a.request(RouteRequest::Base(b0)).unwrap();
    handle_a.request(RouteRequest::Base(b1.clone())).unwrap();
    handle_a.request(RouteRequest::Route(job.clone())).unwrap();
    let (stream_a, _) = job_stream(&handle_a, &wakes_a, job.job);

    let (wake_b, wakes_b) = mpsc::channel();
    let (handle_b, inbox_b) = route_channel(move || {
        let _ = wake_b.send(());
    });
    let worker_b = RouteWorker::new(inbox_b, spawn.clone());
    let worker_b = thread::spawn(move || worker_b.run());
    handle_b.request(RouteRequest::Base(b1.clone())).unwrap();
    handle_b.request(RouteRequest::Route(job.clone())).unwrap();
    let (stream_b, _) = job_stream(&handle_b, &wakes_b, job.job);
    assert_eq!(
        stream_a.last().expect("a job ends with its final proposal").canon_bytes(),
        stream_b.last().expect("a job ends with its final proposal").canon_bytes(),
        "a base without a delta updates the held unit exactly as a fresh unit builds"
    );

    drop((handle_a, handle_b));
    worker_a.join().expect("dropping the handle stops the worker");
    worker_b.join().expect("dropping the handle stops the worker");
    let tasks = std::mem::take(&mut *spawn.tasks.lock().unwrap());
    for task in tasks {
        task.join().expect("route jobs finish without panicking");
    }
}


#[test]
fn bases_cost_nothing_on_the_worker_and_jobs_build_their_basis() {
    let design = ledctl();
    let board = design.units[0].board.clone().expect("fixture setup: ledctl unit 0 has a board");
    assert_eq!(board.unit, BoardId(0), "fixture setup: use unit 0");
    let rules = Arc::new(resolve_rules(&board, None));
    let fills = Arc::new(fill_all(&board, &rules));
    let conn = Arc::new(connect(&board, &fills));
    let b0 = Base::new(board, rules, conn, fills);
    let moved = b0.board.placed.iter().find(|part| part.path == "lamps[7].r")
        .expect("fixture setup: the last LED has a resistor").id;
    let move_base = |previous: &Base, by: P| {
        let mut board = (*previous.board).clone();
        board.apply_all(&[BoardOp::Move { item: moved, by }]).expect("the fixture part moves");
        let board = Arc::new(board);
        let fills = Arc::new(fill_all(&board, &previous.rules));
        let conn = Arc::new(connect(&board, &fills));
        let mut base = Base::new(board, previous.rules.clone(), conn, fills);
        let delta = BoardDelta::between(&previous.board, &base.board);
        assert!(!delta.global && delta.changed.contains(&moved), "a move has a regional delta");
        base.delta = Some(delta);
        base
    };
    let b1 = move_base(&b0, P::mm(0.5, 0.0));
    let nets: Vec<_> = scope_nets(&b1.board, &b1.conn, &RouteScope::All).into_iter().take(3).collect();
    assert_eq!(nets.len(), 3, "fixture setup: three unrouted nets");
    let mut job = RouteJob {
        job: JobId(1),
        unit: b1.unit,
        scope: RouteScope::Nets(nets.clone()),
        method: Method::Interactive,
        engine: Engine::Grid,
        style: None,
        effort: Effort::Full,
        layers: None,
        reroute: false,
    };

    let (wake, wakes) = mpsc::channel();
    let (handle, inbox) = route_channel(move || {
        let _ = wake.send(());
    });
    let spawn = Arc::new(Queue::default());
    let worker = RouteWorker::new(inbox, spawn.clone());
    let worker = thread::spawn(move || worker.run());
    let round_trip = |base: &Base| {
        let net = base.board.net("GND").expect("fixture setup: GND exists");
        assert!(!base.conn.unrouted.contains(&net), "fixture setup: GND is joined by its pour");
        handle.request(RouteRequest::Explain { unit: base.unit, net }).unwrap();
        let RouteEvent::Explain(explanation) = next_event(&handle, &wakes) else {
            panic!("the worker round-trip must publish an explanation before any tasks run");
        };
        assert_eq!((explanation.unit, explanation.net), (base.unit, net));
        assert_eq!(explanation.reason, None, "the round-trip needs no routing task");
    };
    handle.request(RouteRequest::Base(b0)).unwrap();
    handle.request(RouteRequest::Base(b1.clone())).unwrap();
    round_trip(&b1);
    assert!(spawn.0.lock().unwrap().is_empty(), "a base spawns nothing and builds no basis on the worker");

    let run_job = |base: &Base, job: &RouteJob, message: &str| {
        handle.request(RouteRequest::Route(job.clone())).unwrap();
        round_trip(base);
        assert_eq!(spawn.0.lock().unwrap().len(), 1, "the worker queued the route before the round-trip");
        // Draining also runs check tasks queued by the route, outside the queue lock.
        spawn.run();
        let (stream, _) = job_stream(&handle, &wakes, job.job);
        let steps = stream.iter().filter(|proposal| !proposal.last).map(|proposal| match proposal.source {
            ProposalSource::Job { step, .. } => step,
            ProposalSource::Gesture { .. } => panic!("a job's proposal names its step"),
        }).collect();
        let replayed = replay(base.clone(), job, steps);
        let final_proposal = stream.last().expect("a job ends with its final proposal");
        assert_eq!(final_proposal.base_hash, base.hash, "the job answers the latest base");
        assert_eq!(
            final_proposal.canon_bytes(),
            replayed.last().expect("a replay ends with its final proposal").canon_bytes(),
            "{message}"
        );
        final_proposal.clone()
    };
    run_job(&b1, &job, "a lazily built basis checks like a fresh unit");

    // Overlap two resistors so the new base has findings the previous basis cannot supply.
    let target = b1.board.placed.iter().find(|part| part.path == "lamps[6].r")
        .expect("fixture setup: the neighbouring LED has a resistor").at;
    let current = b1.board.placed_item(moved).expect("the moved resistor exists").at;
    let b2 = move_base(&b1, target.sub(current));
    let pair = |short: &fab_elec_board::Short| short.witness.map(|w| w.items);
    let short = b2.conn.shorts.iter()
        .find(|short| !b1.conn.shorts.iter().any(|old| pair(old) == pair(short)))
        .and_then(pair)
        .expect("fixture setup: the second move introduces a short between foreign pads");
    let nets: Vec<_> = scope_nets(&b2.board, &b2.conn, &RouteScope::All).into_iter().take(3).collect();
    assert_eq!(nets.len(), 3, "fixture setup: three unrouted nets after the second move");
    job.job = JobId(2);
    job.scope = RouteScope::Nets(nets.clone());
    handle.request(RouteRequest::Base(b2.clone())).unwrap();
    let final_proposal = run_job(&b2, &job, "an updated basis checks like a fresh unit");
    assert!(
        final_proposal.checked.as_ref().expect("the final proposal is checked").conn.shorts.iter()
            .any(|finding| pair(finding) == Some(short)),
        "the second base's new short survives routing and must already belong to its basis"
    );

    drop(handle);
    worker.join().expect("dropping the handle stops the worker");
    spawn.run();
}

/// Why: `elec.place` runs as a job off the worker's thread, as routing does. The parts the board
/// does not place yet come with the job; each seeding it keeps is a checked proposal that adds
/// them on legal spots inside the outline, its status counts them, and a cancel still ends it
/// with a legal final proposal.
#[test]
fn place_job_brings_parts_onto_legal_spots_and_cancels() {
    use fab_elec_board::placement::PlacementIndex;
    use fab_elec_board::{Item, Placed};
    use fab_elec_route::{JobPhase, PlaceJob, PlaceMode};
    let design = ledctl();
    let unit = &design.units[0];
    let mut board = (**unit.board.as_ref().expect("fixture setup: ledctl unit 0 has a board")).clone();
    // Three resistors leave the board for the tray, with the copper: the host brings them in the
    // row under the outline, as the tray's own layout puts them.
    board.tracks.clear();
    board.vias.clear();
    let below = board.outline_bbox().min.y - mm(10.0);
    let mut add: Vec<Placed> = Vec::new();
    for path in ["lamps[0].r", "lamps[1].r", "lamps[2].r"] {
        let at = board.placed.iter().position(|p| p.path == path).expect("fixture setup: the resistors are placed");
        let mut part = board.placed.remove(at);
        part.at = P::new(part.at.x, below);
        add.push(part);
    }
    board.reindex();
    let base = base_of(Arc::new(board));
    let (wake, wakes) = mpsc::channel();
    let (handle, inbox) = route_channel(move || {
        let _ = wake.send(());
    });
    let worker = RouteWorker::new(inbox, Arc::new(ThreadSpawn::default()));
    let worker = thread::spawn(move || worker.run());
    handle.request(RouteRequest::Base(base.clone())).unwrap();
    let mut job = PlaceJob {
        job: JobId(1), unit: base.unit, scope: add.iter().map(|p| p.comp).collect(), mode: PlaceMode::Place, held: 0,
        rooms: Vec::new(), plan: None, add: add.clone(), circuit: unit.circuit.clone(),
    };
    handle.request(RouteRequest::Place(job.clone())).unwrap();
    let (stream, statuses) = job_stream(&handle, &wakes, job.job);
    let last = stream.last().expect("a job ends with its final proposal");
    let had = drc(&base.board, &base.conn, &base.rules, &base.fills, None).violations;
    let was = FindingNets::of(&base.board, &base.conn);
    for proposal in &stream {
        let checked = proposal.checked.as_ref().expect("a place proposal applies");
        let now = FindingNets::of(&checked.board, &checked.conn);
        let new: Vec<_> = new_or_worse(&had, &was, &checked.violations, &now).into_iter()
            .map(|i| format!("{:?} {}", checked.violations[i].kind, checked.violations[i].message)).collect();
        assert!(proposal.verified, "{} is verified; new findings {new:#?}", proposal.label);
    }
    let place = last.metrics.place.as_ref().expect("a place proposal carries its metrics");
    assert_eq!((place.placed, place.no_room.len(), place.ripped), (3, 0, 0), "the three parts find room and rip nothing");
    let added: Vec<_> = last.ops.iter().filter_map(|op| match op {
        BoardOp::Add(Item::Placed(p)) => Some(p),
        _ => None,
    }).collect();
    assert_eq!(added.len(), 3, "the proposal adds each brought part once: {:?}", last.ops);
    assert!(added.iter().zip(0..).all(|(p, k)| p.id == ItemId(base.board.next_item + k)), "brought parts take ids from the base's next_item on");
    let checked = last.checked.as_ref().expect("a verified proposal has a checked board");
    let outside: Vec<_> = PlacementIndex::new(&checked.board, None).outside_outline().into_iter().map(|(p, _)| p.refdes.clone()).collect();
    assert!(outside.is_empty(), "every placed part lies inside the outline: {outside:?}");
    assert!(statuses.iter().any(|s| matches!(s.phase, JobPhase::Seed { .. }) && s.total == 3 && s.routed == 3),
        "the status counts the parts placed of those in scope: {statuses:?}");

    job.job = JobId(2);
    handle.request(RouteRequest::Place(job.clone())).unwrap();
    handle.request(RouteRequest::Cancel(job.job)).unwrap();
    let (stream, _) = job_stream(&handle, &wakes, job.job);
    let cancelled = stream.last().expect("a cancelled job ends with its final proposal");
    assert!(cancelled.verified && cancelled.metrics.place.is_some(), "a cancelled place job's final is legal: {}", cancelled.label);

    drop(handle);
    worker.join().expect("dropping the handle stops the worker");
}

/// A part placed again takes with it only the unlocked copper that served its pads, and the
/// router's copper its new spot collides with: the rest of its nets and all locked copper stay,
/// the hand-drawn pieces are counted against the part, and the proposal finds nothing new (no end
/// left anchored to nothing, no clearance broken). Handheld's MCU, which nearly every net of the
/// board meets (ripping its whole nets would take most of the copper) and which has hand-drawn
/// tracks to its expander.
#[test]
fn placing_a_part_again_rips_only_the_copper_it_needs() {
    use fab_elec_board::{BoardKeepout, CarriedKeepout, Origin, PortRoom};
    use fab_elec_core::{BBox, FeatureRef, LayerSet, PolySet, Side};
    use fab_elec_parts::Forbid;
    use fab_elec_route::{PlaceJob, PlaceMode};
    use std::collections::BTreeSet;
    let sources = fab_elec_fixtures::docs("handheld").expect("handheld is a stock project");
    let design = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs())
        .unwrap_or_else(|diags| panic!("handheld evaluates: {diags:?}")).design;
    let unit = design.units.iter().find(|u| u.board.as_ref().is_some_and(|b| !b.placed.is_empty()))
        .expect("fixture setup: handheld has a placed board");
    let mut board = unit.board.clone().expect("a board");
    let mcu = board.placed.iter().find(|p| p.refdes == "U1").expect("fixture setup: handheld's U1").clone();
    let unlocked = |locked: bool, origin: &Origin| !locked && matches!(origin, Origin::Solved { .. } | Origin::Authored);
    let nets: BTreeSet<_> = mcu.pad_nets.iter().flatten().copied().collect();
    let whole = board.tracks.iter().filter(|t| unlocked(t.locked, &t.origin) && nets.contains(&t.net)).count()
        + board.vias.iter().filter(|v| unlocked(v.locked, &v.origin) && nets.contains(&v.net)).count();
    let at_mcu = |t: &&fab_elec_board::Track| t.ends.iter().any(|e| matches!(e, Anchor::Pad { placed, .. } if *placed == mcu.id));
    let anchored: Vec<ItemId> = board.tracks.iter().filter(|t| unlocked(t.locked, &t.origin)).filter(at_mcu).map(|t| t.id).collect();
    let hand_anchored = board.tracks.iter().filter(|t| !t.locked && matches!(t.origin, Origin::Authored)).filter(at_mcu).count();
    assert!(!anchored.is_empty() && hand_anchored > 0, "fixture setup: router and hand-drawn tracks reach U1's pads");

    // A small band at the MCU's centre must follow it through the worker's copper retries.
    let from = FeatureRef::Generated("Cover at U1".into());
    let port = PortRoom { comp: mcu.comp, room: None, keepout: Some(CarriedKeepout {
        from: from.clone(), area: PolySet::rect(BBox::new(P::mm(-0.05, 0.0), P::mm(0.1, 0.08))),
        band: (mm(1.0), mm(2.0)), side: Side::Top,
    }) };
    let published = port.placed(mcu.at).keepout.unwrap();
    let scratch = Arc::make_mut(&mut board);
    let cover = scratch.alloc_id();
    scratch.keepouts.push(BoardKeepout {
        id: cover, area: published.area, layers: LayerSet::one(LayerId::F_CU.on(published.side)), forbid: Forbid::FOOTPRINTS,
        reason: "Cover".into(), origin: Origin::Derived { from }, z: Some(published.band),
    });
    scratch.reindex();
    let base = base_of(board.clone());
    let (wake, wakes) = mpsc::channel();
    let (handle, inbox) = route_channel(move || {
        let _ = wake.send(());
    });
    let worker = RouteWorker::new(inbox, Arc::new(ThreadSpawn::default()));
    let worker = thread::spawn(move || worker.run());
    handle.request(RouteRequest::Base(base.clone())).unwrap();
    let job = PlaceJob {
        job: JobId(1), unit: base.unit, scope: vec![mcu.comp], mode: PlaceMode::Place, held: 0, rooms: vec![port], plan: None, add: Vec::new(), circuit: unit.circuit.clone(),
    };
    handle.request(RouteRequest::Place(job.clone())).unwrap();
    let (stream, _) = job_stream(&handle, &wakes, job.job);
    let last = stream.last().expect("a job ends with its final proposal");
    for proposal in &stream {
        let checked = &proposal.checked.as_ref().expect("a place proposal applies").board;
        let owner = checked.placed_of(mcu.comp).unwrap();
        let source = board.keepouts.iter().find(|k| k.id == cover).unwrap();
        let actual = checked.keepouts.iter().find(|k| k.id == cover).unwrap();
        assert_eq!(actual.area, source.area.translated(owner.at.sub(mcu.at)), "the worker checks the cover at the offered MCU pose");
        assert_eq!(actual.layers, source.layers);
        assert_eq!(actual.z, source.z);
    }
    let checked = last.checked.as_ref().expect("a place proposal applies");
    let had = drc(&base.board, &base.conn, &base.rules, &base.fills, None).violations;
    let new: Vec<_> = new_or_worse(&had, &FindingNets::of(&base.board, &base.conn), &checked.violations, &FindingNets::of(&checked.board, &checked.conn))
        .into_iter().map(|i| &checked.violations[i]).filter(|v| v.kind != fab_elec_circuit::CheckKind::Unconnected)
        .map(|v| format!("{:?} {} {:?}", v.kind, v.message, v.items)).collect();
    assert!(last.verified && new.is_empty(), "placing U1 again finds nothing new but unrouted connections: {new:#?}");
    assert!(last.ops.iter().any(|op| matches!(op, BoardOp::SetField { item, .. } if *item == mcu.id)), "the placer moves U1: {:?}", last.ops);
    let removed: BTreeSet<ItemId> = last.ops.iter().filter_map(|op| match op { BoardOp::Remove(id) => Some(*id), _ => None }).collect();
    let place = last.metrics.place.as_ref().expect("a place proposal carries its metrics");
    assert_eq!(place.ripped as usize, removed.len(), "the metrics count the copper the proposal removes");
    assert!(anchored.iter().all(|id| removed.contains(id)), "every unlocked track that U1's pads anchor goes with it");
    assert!(removed.len() < whole, "the rest of U1's nets stays: {} removed of the {whole} unlocked items on its nets", removed.len());
    let locked = |id: &ItemId| match base.board.locate(*id) {
        Some((fab_elec_board::ItemKind::Track, i)) => base.board.tracks[i].locked,
        Some((fab_elec_board::ItemKind::Via, i)) => base.board.vias[i].locked,
        _ => true,
    };
    assert!(!removed.iter().any(locked), "only unlocked tracks and vias are removed");
    let hand_removed = removed.iter().filter(|id| base.board.locate(**id).is_some_and(|(kind, i)| match kind {
        fab_elec_board::ItemKind::Track => matches!(base.board.tracks[i].origin, Origin::Authored),
        fab_elec_board::ItemKind::Via => matches!(base.board.vias[i].origin, Origin::Authored),
        _ => false,
    })).count();
    assert!(matches!(place.hand.as_slice(), [h] if h.comp == mcu.comp && (h.tracks + h.vias) as usize == hand_removed && h.tracks as usize >= hand_anchored),
        "the hand-drawn copper removed is counted against U1 alone: {:?}, {hand_removed} removed, {hand_anchored} at its pads", place.hand);
    drop(handle);
    worker.join().expect("dropping the handle stops the worker");
}

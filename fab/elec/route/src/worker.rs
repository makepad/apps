//! R0 route worker (E6 8). The host runs `RouteWorker::run` on one long-lived thread; it takes
//! queued bases first, then the newest sample and one other command in turn ([`RouteInbox::next`]),
//! and owns only the newest committed base of each unit. Jobs use that base when dequeued, never a
//! retained older generation. Running jobs keep their own `Arc` snapshots and publish the
//! generation they used for the host to compare.
//!
//! Gestures (E6 3, `head`): one live gesture at a time, its newest sample, its (generation, seq)
//! guard and its hull cache; `End`, or a sample of another gesture, drops it. A gesture survives
//! a base change: a sample drawn on generation g, with W the worker's, is answered on the worker's
//! base when g <= W (the head re-verifies the start and the target there) and parked when g > W,
//! its base being on its way (bases are taken first). After every base of the live gesture's unit
//! its newest sample is answered again, parked or already answered: a cursor at rest sends no new
//! sample, so a release would otherwise wait on a board that changed under it. An answer the
//! sample's (generation, seq) already covers is not sent again. A finishing route sample
//! (`Gesture::finish`) is answered twice: its stretch answer at once, then its Finish
//! (`head::finish`) from `Spawn`, published after it. A newer Finish, a new gesture or `End`
//! cancels a Finish still running, and a cancelled one publishes nothing. A part or via drag's
//! stretch answer may leave a shove wave (`head::shove_step_until`): the thread steps it one track
//! at a time whenever nothing waits in the inbox, and serves routes, cancels and explanations
//! between steps. A newer sample of the gesture, a base of another generation for its unit or
//! `End` drops it, unpublished; a newer sample in the inbox stops the step in hand between its
//! parts, so its tier 1 starts at once. A finished wave publishes the Full answer for its sample.
//!
//! The thread does only what a gesture needs it for: the inbox, routing-world updates, the
//! gesture answers, and O(1) snapshot and progress bookkeeping. `Spawn` (the host's pool or long-lived
//! threads) does the rest: a job's problem and its route, the lazy job basis (the piece index and
//! DRC findings), proposal checks, the route behind an explanation, and a gesture's Finish. A job's partials and its
//! final proposal are checked one at a time per job, the newest waiting offer replacing an older
//! one, so the router never waits for a check and nothing is published after the final. An
//! explanation for a missing unit or a joined net is published at once.
//! The job-map mutex covers only inserts, lookups and removals; progress locks cover field reads
//! or swaps, the basis recipe mutex covers only taking the recipe, and the outbox lock covers
//! superseding and pushing events. Builds, routes and checks run outside these locks, as does the
//! UI wake callback. Jobs that need one basis at once wait on its `ComputeOnce`, on pool threads
//! only and without their CPU permit, so the job building it paces like any other.
//! The UI never waits for work.
//!
//! Cost on the thread: a non-global base updates the unit's world in O(delta log n); the first
//! base, or a global delta, builds the world in O(n log n); a base without a delta is first diffed
//! against the held one (`BoardDelta::between`, O(items)). The world update copies the world, O(n),
//! only while a job's snapshot of it is still queued or being copied: each job copies its world on
//! the pool and drops the shared one before it routes. On the pool: the first job that needs a
//! basis builds the index and full DRC, or updates the previous base's basis when that one was
//! built (unbuilt bases never chain); a check costs a board clone, applying the ops, refilling the
//! zones they touch, index and DRC work near the change (O(k log n) for the k pieces there) and the
//! connectivity list passes (O(n log n)).
//!
//! `replay` reruns a job and rebuilds its proposals at the steps a recorded stream published,
//! through the same code: each has its recorded proposal's canonical bytes (E6 9.4).

use crate::auto::GridRouter;
use crate::channel::{Publisher, RouteInbox, Work};
use crate::check::{check, refilled, Basis, Outcome, State};
use crate::gesture::{GestureKind, GestureSample};
use crate::head::{self, Answer, GestureState};
use crate::problem::{problem, scope_nets, Autorouter, RouteProblem};
use crate::progress::{NetStatus, Progress};
use crate::proposal::{
    Checked, Explanation, JobId, JobStatus, ProposalId, ProposalSource, RouteEvent, RouteProposal, SampleCause, SampleRefused,
};
use crate::place::job::{branches, PlaceOffer, Takes};
use crate::request::{Base, Effort, Engine, Method, PlaceJob, PlaceMode, RouteJob, RouteRequest, RouteScope};
use crate::solution::{Reason, RouteMetrics, RouteSolution};
use crate::solvers;
use crate::world::RBase;
use fab_elec_board::drc::drc_indexed;
use fab_elec_board::{drc, Board, BoardDelta, BoardOp, IndexChange, OpError, Origin, PieceIndex, ZoneFill};
use fab_elec_circuit::{CheckKind, Violation};
use fab_elec_core::{BoardId, ContentHash, ItemId, NetId, Sel};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU32, Ordering};
use makepad_core_util::once::ComputeOnce;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

/// Where jobs run: the host's pool or one of its long-lived threads.
pub trait Spawn: Send + Sync {
    /// Schedule a job without creating a temporary thread per job in product code.
    fn spawn(&self, task: Box<dyn FnOnce() + Send>);
}

/// The live gesture: its newest sample and its order guard and hull cache, and the progress of its
/// Finish while that runs on `Spawn` (cancelled when the gesture goes).
struct Live {
    sample: GestureSample,
    state: GestureState,
    finishing: Option<Arc<Progress>>,
}

impl Drop for Live {
    fn drop(&mut self) {
        if let Some(progress) = &self.finishing {
            progress.cancel();
        }
    }
}

struct Unit {
    base: Base,
    world: Arc<RBase>,
    basis: Arc<BasisCell>,
}

/// A unit's job basis (piece index and findings) for one base: built once, by the first job that
/// needs it, on that job's pool thread, never on the worker's thread.
struct BasisCell {
    recipe: Mutex<Option<Recipe>>,
    built: ComputeOnce<Basis>,
}

enum Recipe {
    /// Build the piece index and run full DRC.
    Full(Base),
    /// Update the previous base's built basis by the delta; the old fills say which zones refilled.
    Update { from: Arc<BasisCell>, previous_fills: Arc<Vec<ZoneFill>>, base: Base, delta: BoardDelta },
}

impl BasisCell {
    fn new(recipe: Recipe) -> BasisCell {
        BasisCell { recipe: Mutex::new(Some(recipe)), built: ComputeOnce::new() }
    }

    fn get(&self) -> &Basis {
        self.built.get_or_init(|| {
            heavy("basis");
            let recipe = self.recipe.lock().unwrap_or_else(|e| e.into_inner()).take().expect("an unbuilt basis has its recipe");
            // the recipe's guard is gone before any build, or any wait on the previous basis; other
            // route jobs wait for this build off the CPU, so it gives the host's pool its turns
            match recipe {
                Recipe::Full(base) => {
                    let index = PieceIndex::build(&base.board, &base.fills);
                    let violations = drc(&base.board, &base.conn, &base.rules, &base.fills, None).violations;
                    Basis { index, violations: Arc::new(violations) }
                }
                Recipe::Update { from, previous_fills, base, delta } => {
                    let previous = from.get();
                    let mut index = previous.index.clone();
                    let refilled = refilled(&previous_fills, &base.fills);
                    index.update(&base.board, &base.fills, &delta, &refilled);
                    let violations =
                        drc_indexed(&previous.violations, &index, &base.board, &base.conn, &base.rules, &delta, &[]);
                    Basis { index, violations: Arc::new(violations) }
                }
            }
        })
    }
}

/// Tests only: the thread each heavy piece of work ran on (a basis build, a job's problem, an
/// explanation's route), so a test can tell the worker's thread from the pool's.
#[cfg(test)]
static HEAVY: Mutex<Vec<(std::thread::ThreadId, &'static str)>> = Mutex::new(Vec::new());

fn heavy(_what: &'static str) {
    #[cfg(test)]
    HEAVY.lock().unwrap_or_else(|e| e.into_inner()).push((std::thread::current().id(), _what));
}

impl Unit {
    /// A unit with its routing world built and its job basis left for the first job that needs it.
    fn new(base: Base) -> Unit {
        let world = Arc::new(RBase::build(&base.board));
        let basis = Arc::new(BasisCell::new(Recipe::Full(base.clone())));
        Unit { base, world, basis }
    }

    /// Routing `job`'s scope on this unit's base (a replay's, on its caller's thread).
    fn problem(&self, job: &RouteJob) -> RouteProblem {
        let nets = scope_nets(&self.base.board, &self.base.conn, &job.scope);
        let mut problem = problem(&self.base, self.world.clone(), &nets, job.style.unwrap_or_default(), job.effort);
        problem.reroute = job.reroute;
        problem
    }

    /// What checks and emits `job`'s proposals against this unit's snapshots.
    fn output(&self, job: JobId, next_proposal: Arc<AtomicU32>, emit: Emit) -> JobOutput {
        JobOutput {
            job,
            base: self.base.clone(),
            basis: self.basis.clone(),
            emit,
            next_proposal,
        }
    }
}

/// Where a job's checked proposals go: the channel's publisher, or a replay's list.
type Emit = Arc<dyn Fn(RouteProposal) + Send + Sync>;

/// Replay a grid job (E6 9.4): run `job` on a unit built from `base` as the worker builds a new
/// one, and rebuild its proposals through the code the worker publishes with, at exactly the
/// partial `steps` a recorded stream published and then the final one. A proposal's content is a
/// function of the problem and its step, and wall time picks only which steps are published, so
/// each replayed proposal has its recorded one's canonical bytes (its `Canon` leaves out the
/// publication number `id`). Runs on the caller's thread.
pub fn replay(base: Base, job: &RouteJob, steps: Vec<u32>) -> Vec<RouteProposal> {
    let unit = Unit::new(base);
    let replayed = Arc::new(Mutex::new(Vec::new()));
    let list = replayed.clone();
    let emit: Emit = Arc::new(move |proposal| list.lock().unwrap_or_else(|e| e.into_inner()).push(proposal));
    let output = unit.output(job.job, Arc::new(AtomicU32::new(1)), emit);
    let partials = output.clone();
    let progress = Progress::at_steps(steps, move |step, solution| partials.publish(step, &solution, false));
    let (step, solution) = output.solve(&unit.problem(job), &progress, job.method);
    output.publish(step, &solution, true);
    drop((output, progress));
    // A named value: the lock guard drops before `replayed` does.
    let proposals = std::mem::take(&mut *replayed.lock().unwrap_or_else(|e| e.into_inner()));
    proposals
}

/// A release can commit any final gesture proposal that carries ops. A track-to-mate
/// collision already found by the verifier must therefore reach the pane as a refusal.
/// Intermediate shove and Finish tiers may still clear it before release.
pub(crate) fn gesture_event(base: &Base, proposal: RouteProposal) -> RouteEvent {
    if let ProposalSource::Gesture { id, seq, .. } = proposal.source {
        if let Some(reason) = proposal.metrics.pair_refusal.as_ref().or_else(|| proposal.metrics.tune.as_ref()?.as_ref().err()) {
            return RouteEvent::Refused(SampleRefused { id, seq, unit: base.unit, cause: SampleCause::Invalid, text: reason.clone() });
        }
    }
    if proposal.last && !proposal.verified && !proposal.collisions.is_empty() && !base.board.pairs.is_empty() {
        let track_net = |sel: &Sel| {
            let Sel::Item(unit, id) = sel else { return None };
            if *unit != base.unit { return None; }
            for op in &proposal.ops {
                if let BoardOp::Add(fab_elec_board::Item::Track(t)) | BoardOp::Replace(fab_elec_board::Item::Track(t)) = op {
                    if t.id == *id { return Some(t.net); }
                }
            }
            let (fab_elec_board::ItemKind::Track, i) = base.board.locate(*id)? else { return None };
            base.board.tracks.get(i).map(|t| t.net)
        };
        if let ProposalSource::Gesture { id, seq, .. } = proposal.source {
            for hit in &proposal.collisions {
                if let (Some(a), Some(b)) = (track_net(&hit.a), track_net(&hit.b)) {
                    if base.rules.partner(a).is_some_and(|(mate, gap)| mate == b && gap > 0) {
                        return RouteEvent::Refused(SampleRefused { id, seq, unit: base.unit, cause: SampleCause::Invalid,
                            text: format!("the tracks would violate their pair gap ({:.6} mm, need {:.6} mm)",
                                f64::from(hit.gap)/1e6, f64::from(hit.need)/1e6) });
                    }
                }
            }
        }
    }
    RouteEvent::Proposal(proposal)
}

/// The route worker: one long-lived thread owns each unit's newest committed base, takes bases
/// first and samples in turn with the other commands, and runs jobs on `Spawn` over `Arc` snapshots.
pub struct RouteWorker {
    inbox: RouteInbox,
    publisher: Publisher,
    spawn: Arc<dyn Spawn>,
    units: BTreeMap<BoardId, Unit>,
    gesture: Option<Live>,
    jobs: Arc<Mutex<BTreeMap<JobId, Arc<Progress>>>>,
    next_proposal: Arc<AtomicU32>,
    #[cfg(feature = "test-hooks")]
    offered: Option<Arc<dyn Fn(JobId, u32) + Send + Sync>>,
}

impl RouteWorker {
    /// Attach a channel to the host's job executor; the host supplies the worker thread.
    pub fn new(inbox: RouteInbox, spawn: Arc<dyn Spawn>) -> RouteWorker {
        RouteWorker {
            publisher: inbox.publisher(),
            inbox,
            spawn,
            units: BTreeMap::new(),
            gesture: None,
            jobs: Arc::new(Mutex::new(BTreeMap::new())),
            next_proposal: Arc::new(AtomicU32::new(1)),
            #[cfg(feature = "test-hooks")]
            offered: None,
        }
    }

    /// Tests only (feature `test-hooks`): `hook(job, step)` runs on a job's engine thread right
    /// after each partial the job hands its checker, before the engine goes on, so a test that
    /// blocks in it holds the job at a known partial.
    #[cfg(feature = "test-hooks")]
    pub fn with_offered(mut self, hook: impl Fn(JobId, u32) + Send + Sync + 'static) -> RouteWorker {
        self.offered = Some(Arc::new(hook));
        self
    }

    /// Handle work until the channel closes, on the host's long-lived worker thread. A live
    /// gesture's shove wave takes a step whenever nothing waits in the inbox.
    pub fn run(mut self) {
        loop {
            if self.gesture.as_ref().is_some_and(|live| live.state.waving()) && !self.inbox.preempted() {
                self.step_wave();
                continue;
            }
            match self.inbox.next() {
                Work::Closed => return,
                Work::Sample(sample) => self.sample(sample),
                Work::Request(RouteRequest::End(id)) => {
                    if self.gesture.as_ref().is_some_and(|live| live.sample.id == id) {
                        self.gesture = None;
                    }
                }
                Work::Request(RouteRequest::Base(base)) => {
                    let unit = base.unit;
                    // A wave is laid on its base's world: a new generation drops it before the
                    // world changes (the sample is answered again on the new base below).
                    if let Some(live) = self.gesture.as_mut().filter(|live| live.sample.gesture.unit == unit) {
                        let held = self.units.get(&unit).map(|held| held.base.board.generation);
                        if held != Some(base.board.generation) {
                            live.state.drop_wave();
                        }
                    }
                    self.base(base);
                    if self.gesture.as_ref().is_some_and(|live| live.sample.gesture.unit == unit) {
                        self.answer_live();
                    }
                }
                Work::Request(RouteRequest::Route(job)) => self.route(job),
                Work::Request(RouteRequest::Cancel(job)) => {
                    let progress = self.jobs.lock().unwrap_or_else(|e| e.into_inner()).get(&job).cloned();
                    if let Some(progress) = progress {
                        progress.cancel();
                    }
                }
                Work::Request(RouteRequest::Explain { unit, net }) => self.explain(unit, net),
                Work::Request(RouteRequest::Restyle(job)) => {
                    self.unavailable(job.job, job.unit, "not available yet");
                }
                Work::Request(RouteRequest::Tune(job)) => self.tune(job),
                Work::Request(RouteRequest::Pair(job)) => self.pair(job),
                Work::Request(RouteRequest::Place(job)) => self.place(job),
            }
        }
    }

    /// The newest sample of the live gesture, or the first of a new one (which drops the old).
    fn sample(&mut self, sample: GestureSample) {
        match &mut self.gesture {
            Some(live) if live.sample.id == sample.id => {
                if sample.seq >= live.sample.seq {
                    live.sample = sample;
                }
            }
            _ => self.gesture = Some(Live { sample, state: GestureState::default(), finishing: None }),
        }
        self.answer_live();
    }

    /// Answer the live gesture's newest sample on its unit's base, or park it while that base is
    /// behind the sample's generation.
    fn answer_live(&mut self) {
        let Some(live) = self.gesture.as_mut() else { return };
        let sample = &live.sample;
        let refuse = |cause, text: &str| SampleRefused {
            id: sample.id, seq: sample.seq, unit: sample.gesture.unit, cause, text: text.into(),
        };
        let Some(unit) = self.units.get(&sample.gesture.unit) else {
            self.publisher.publish(RouteEvent::Refused(refuse(SampleCause::NoBase, "no board for this unit yet")));
            return;
        };
        if sample.gesture.base_generation > unit.base.board.generation {
            return;
        }
        let id = ProposalId(self.next_proposal.fetch_add(1, Ordering::Relaxed));
        match head::answer(&unit.base, &unit.world, sample, &mut live.state, id) {
            Answer::Proposal(proposal) => {
                let hand = (sample.gesture.finish && matches!(sample.gesture.kind, GestureKind::Route)).then(|| proposal.clone());
                // The stretch answer goes first: the Finish is published after it.
                self.publisher.publish(gesture_event(&unit.base, proposal));
                if let Some(hand) = hand {
                    let progress = Arc::new(Progress::new());
                    if let Some(old) = live.finishing.replace(progress.clone()) {
                        old.cancel();
                    }
                    let (base, world, basis, sample) = (unit.base.clone(), unit.world.clone(), unit.basis.clone(), sample.clone());
                    let (publisher, next_proposal) = (self.publisher.clone(), self.next_proposal.clone());
                    self.spawn.spawn(Box::new(move || {
                        heavy("finish");
                        let id = ProposalId(next_proposal.fetch_add(1, Ordering::Relaxed));
                        let finished = head::finish(&base, &world, basis.get(), &sample, &hand, id, &progress);
                        if !progress.cancelled() {
                            publisher.publish(gesture_event(&base, finished));
                        }
                    }));
                }
            }
            Answer::Refused(cause, text) => self.publisher.publish(RouteEvent::Refused(refuse(cause, text))),
            Answer::Covered => {}
        }
    }

    /// One step of the live gesture's shove wave; its Full answer when that step ends it.
    fn step_wave(&mut self) {
        let Some(live) = self.gesture.as_mut() else { return };
        let Some(unit) = self.units.get(&live.sample.gesture.unit) else {
            live.state.drop_wave();
            return;
        };
        let next = &self.next_proposal;
        // A newer sample of the gesture, or another gesture's, drops the wave when it is answered:
        // the step stops for it between its parts.
        let (id, seq, inbox) = (live.sample.id, live.sample.seq, &self.inbox);
        let stale = || inbox.waiting_sample().is_some_and(|(other, newer)| other != id || newer > seq);
        if let Some((full, _)) = head::shove_step_until(&unit.base, &unit.world, &mut live.state, || ProposalId(next.fetch_add(1, Ordering::Relaxed)), &stale) {
            self.publisher.publish(gesture_event(&unit.base, full));
        }
    }

    fn base(&mut self, base: Base) {
        if let Some(unit) = self.units.get_mut(&base.unit) {
            let delta = match base.delta.as_ref() {
                Some(delta) => Cow::Borrowed(delta),
                None if Arc::ptr_eq(&unit.base.board, &base.board) => Cow::Owned(BoardDelta::default()),
                None => Cow::Owned(BoardDelta::between(&unit.base.board, &base.board)),
            };
            if !delta.global {
                Arc::make_mut(&mut unit.world).update(&base.board, &delta);
                let recipe = if unit.basis.built.get().is_some() {
                    let delta = delta.into_owned();
                    Recipe::Update { from: unit.basis.clone(), previous_fills: unit.base.fills.clone(), base: base.clone(), delta }
                } else {
                    Recipe::Full(base.clone())
                };
                unit.basis = Arc::new(BasisCell::new(recipe));
                unit.base = base;
                return;
            }
        }
        self.units.insert(base.unit, Unit::new(base));
    }

    fn route(&self, job: RouteJob) {
        let Some(unit) = self.units.get(&job.unit) else {
            self.unavailable(job.job, job.unit, "no committed board for this unit");
            return;
        };
        if !matches!(job.engine, Engine::Auto | Engine::Grid) {
            self.unavailable(job.job, job.unit, "engine not available");
            return;
        }
        if matches!(job.scope, RouteScope::Parts(_)) {
            self.unavailable(job.job, job.unit, "a parts scope is not available yet");
            return;
        }
        let (base, world) = (unit.base.clone(), unit.world.clone());
        let publisher = self.publisher.clone();
        let emit: Emit = Arc::new(move |proposal| publisher.publish(RouteEvent::Proposal(proposal)));
        let output = unit.output(job.job, self.next_proposal.clone(), emit);
        let checker = Checker::new(output.clone(), self.spawn.clone());
        #[cfg(feature = "test-hooks")]
        let after = self.offered.clone().map(|hook| {
            let id = job.job;
            Arc::new(move |step: u32| hook(id, step)) as Arc<dyn Fn(u32) + Send + Sync>
        });
        #[cfg(not(feature = "test-hooks"))]
        let after = None;
        let progress = checker.progress(PARTIAL_EVERY, PARTIAL_FLOOR, after, status_sink(self.publisher.clone(), job.job, job.unit));
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).insert(job.job, progress.clone());
        let jobs = self.jobs.clone();
        self.spawn.spawn(Box::new(move || {
            heavy("problem");
            let mut problem = problem_on_pool(&base, world, &job.scope, job.style.unwrap_or_default(), job.effort);
            problem.reroute = job.reroute;
            let (step, solution) = output.solve(&problem, &progress, job.method);
            checker.offer(step, Offer::Route(Arc::new(solution)), true);
            let removed = jobs.lock().unwrap_or_else(|e| e.into_inner()).remove(&job.job);
            drop(removed);
        }));
    }

    /// `elec.place` with `@place`: off the thread, as a route job runs, with its progress in the job
    /// map so `Cancel` stops it between seeding rounds. Each seeding kept is checked and published
    /// as a proposal (`place::job`), the newest waiting offer replacing an older one.
    fn place(&self, job: PlaceJob) {
        let Some(unit) = self.units.get(&job.unit) else {
            self.unavailable(job.job, job.unit, "no committed board for this unit");
            return;
        };
        if job.mode != PlaceMode::Place {
            self.unavailable(job.job, job.unit, "legalising is an edit (elec.place with mode @legalise), not a job");
            return;
        }
        let base = unit.base.clone();
        let publisher = self.publisher.clone();
        let emit: Emit = Arc::new(move |proposal| publisher.publish(RouteEvent::Proposal(proposal)));
        let output = unit.output(job.job, self.next_proposal.clone(), emit.clone());
        let checker = Checker::new(output, self.spawn.clone());
        let progress = Arc::new(Progress::new().with_status(status_sink(self.publisher.clone(), job.job, job.unit)));
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).insert(job.job, progress.clone());
        let (jobs, next_proposal) = (self.jobs.clone(), self.next_proposal.clone());
        let pool = self.spawn.clone();
        self.spawn.spawn(Box::new(move || {
            heavy("place");
            let job = Arc::new(job);
            let offer = &mut |step, offer, last| checker.offer(step, Offer::Place(job.clone(), offer), last);
            if let Err(why) = crate::place::job::run(&base.board, &job, &progress, offer, pool) {
                let id = ProposalId(next_proposal.fetch_add(1, Ordering::Relaxed));
                emit(no_proposal(id, job.job, base.unit, (base.board.generation, base.hash), why));
            }
            let removed = jobs.lock().unwrap_or_else(|e| e.into_inner()).remove(&job.job);
            drop(removed);
        }));
    }

    fn pair(&self, job: crate::request::PairJob) {
        let Some(unit) = self.units.get(&job.unit) else {
            self.unavailable(job.job, job.unit, "no committed board for this unit");
            return;
        };
        let publisher = self.publisher.clone();
        let emit: Emit = Arc::new(move |p| publisher.publish(RouteEvent::Proposal(p)));
        let output = unit.output(job.job, self.next_proposal.clone(), emit);
        let world = unit.world.clone();
        self.spawn.spawn(Box::new(move || {
            let base = &output.base;
            let id = ProposalId(output.next_proposal.fetch_add(1, Ordering::Relaxed));
            let name = base.board.nets.get(job.route.net.ix()).map_or("pair", |n| n.name.as_str());
            let mut offer = no_proposal(id, job.job, base.unit, (base.board.generation, base.hash), format!("Route pair {name}"));
            let request = crate::pair::Request { route: &job.route, connectivity: &base.conn };
            match crate::pair::plan(&base.board, &base.rules, &crate::World::new(&world), request) {
                Ok(plan) => {
                    offer.ops = plan.ops();
                    // The endpoint at a free station is deliberately open. Skew is reported
                    // on the resulting pair and can be corrected by tuning the shorter leg.
                    output.check_into(&mut offer, |v| !matches!(v.kind, CheckKind::Dangling | CheckKind::Skew | CheckKind::Length));
                    if offer.verified {
                        offer.pair = Some(plan.report);
                    } else {
                        offer.ops.clear();
                        offer.checked = None;
                        offer.metrics.pair_refusal = Some(format!("{}: the proposed pair failed board verification", offer.label));
                    }
                }
                Err(reason) => offer.metrics.pair_refusal = Some(reason),
            }
            (output.emit)(offer);
        }));
    }

    fn tune(&self, job: crate::request::TuneJob) {
        let Some(unit) = self.units.get(&job.unit) else {
            self.unavailable(job.job, job.unit, "no committed board for this unit");
            return;
        };
        let publisher = self.publisher.clone();
        let emit: Emit = Arc::new(move |p| publisher.publish(RouteEvent::Proposal(p)));
        let output = unit.output(job.job, self.next_proposal.clone(), emit);
        let world = unit.world.clone();
        self.spawn.spawn(Box::new(move || {
            let base = &output.base;
            let id = ProposalId(output.next_proposal.fetch_add(1, Ordering::Relaxed));
            let mut proposal = no_proposal(id, job.job, base.unit, (base.board.generation, base.hash), "Tune net".into());
            let request = crate::meander::Request { tune: &job.tune, circuit: &job.circuit, connectivity: &base.conn };
            let result = crate::meander::plan(&base.board, &base.rules, &crate::world::World::new(&world), request);
            drop(world);
            proposal.metrics.tune = Some(match result {
                Ok(plan) => {
                    proposal.label = format!("Tune {}", base.board.nets[job.tune.net.ix()].name);
                    proposal.ops = plan.ops;
                    output.check_into(&mut proposal, |v| !matches!(v.kind, CheckKind::Skew | CheckKind::Length));
                    if proposal.verified { Ok(plan.report) }
                    else { Err(format!("{}: the proposed folds failed board verification", proposal.label)) }
                }
                Err(reason) => Err(reason),
            });
            (output.emit)(proposal);
        }));
    }

    fn explain(&self, unit: BoardId, net: NetId) {
        let reason = match self.units.get(&unit) {
            Some(held) if held.base.conn.unrouted.contains(&net) => {
                let (base, world, publisher) = (held.base.clone(), held.world.clone(), self.publisher.clone());
                self.spawn.spawn(Box::new(move || {
                    heavy("explain");
                    let problem = problem_on_pool(&base, world, &RouteScope::Nets(vec![net]), Default::default(), Effort::default());
                    let solution = GridRouter::default().route(&problem, &Progress::new());
                    let reason = solution.open.into_iter().find_map(|(open, reason)| (open == net).then_some(reason));
                    publish_explanation(&publisher, unit, net, reason);
                }));
                return;
            }
            Some(_) => None,
            None => Some(Reason::NotTried),
        };
        publish_explanation(&self.publisher, unit, net, reason);
    }

    fn unavailable(&self, job: JobId, unit: BoardId, label: &str) {
        let id = ProposalId(self.next_proposal.fetch_add(1, Ordering::Relaxed));
        let (generation, hash) = self.units.get(&unit).map_or((0, ContentHash::default()), |held| (held.base.board.generation, held.base.hash));
        self.publisher.publish(RouteEvent::Proposal(no_proposal(id, job, unit, (generation, hash), label.into())));
    }
}

/// A job's last proposal when it has none to make: no ops, `label` saying why, against the base
/// (generation, hash) the worker holds.
fn no_proposal(id: ProposalId, job: JobId, unit: BoardId, (base_generation, base_hash): (u64, ContentHash), label: String) -> RouteProposal {
    RouteProposal {
        pair: None,
        id,
        source: ProposalSource::Job { job, step: 0 },
        unit,
        base_generation,
        base_hash,
        ops: Vec::new(),
        checked: None,
        metrics: RouteMetrics::default(),
        verified: false,
        collisions: Vec::new(),
        open: Vec::new(),
        head: None,
        solve_us: 0,
        last: true,
        label,
    }
}

/// A job's status, published as the worker's `RouteEvent::Status`.
fn status_sink(publisher: Publisher, job: JobId, unit: BoardId) -> impl Fn(NetStatus) + Send + Sync + 'static {
    move |status| {
        publisher.publish(RouteEvent::Status(JobStatus {
            job,
            unit,
            phase: status.phase,
            current: status.current,
            routed: status.routed,
            failed: status.failed,
            total: status.total,
            attempt: status.attempt,
            retry: status.retry,
            blocked_by: status.blocked_by,
        }));
    }
}

/// On a pool thread: a problem over the job's own copy of the world, so a base committed while the
/// job routes updates the shared world without copying it on the worker's thread. O(n) for the
/// copy, plus the problem's own build.
fn problem_on_pool(base: &Base, world: Arc<RBase>, scope: &RouteScope, style: crate::gesture::Style, effort: Effort) -> RouteProblem {
    let copy = Arc::new((*world).clone());
    drop(world);
    let nets = scope_nets(&base.board, &base.conn, scope);
    problem(base, copy, &nets, style, effort)
}

fn publish_explanation(publisher: &Publisher, unit: BoardId, net: NetId, reason: Option<Reason>) {
    let hints = reason
        .as_ref()
        .map(|reason| match reason {
            Reason::Blocked { .. } => "route around",
            Reason::NoRoom { .. } => "make room near the named items",
            Reason::Terminal { .. } => "move the terminal off foreign copper",
            Reason::NotTried => "not tried",
            Reason::Unjoined { .. } => "reconnect the joined track",
            Reason::SharpCorner { .. } => "make room to chamfer the corner",
        })
        .map(String::from)
        .into_iter()
        .collect();
    let blockers = match &reason {
        Some(Reason::NoRoom { by, .. }) => by.clone(),
        _ => Vec::new(),
    };
    publisher.publish(RouteEvent::Explain(Explanation { unit, net, reason, blockers, hints }));
}

struct Checker {
    output: JobOutput,
    spawn: Arc<dyn Spawn>,
    state: Mutex<CheckState>,
    /// The job's progress, once `progress` made it: a checker that is idle again asks it for the
    /// next partial.
    progress: OnceLock<Weak<Progress>>,
}

/// A running job's partials are copied at least this often, whatever the checks do.
const PARTIAL_EVERY: Duration = Duration::from_millis(100);
/// ... and at most this often when the checks keep up: about 30 a second.
const PARTIAL_FLOOR: Duration = Duration::from_millis(33);

struct CheckState {
    waiting: Option<(u32, Offer, bool)>,
    running: bool,
}

/// What a job hands its checker: a route's solution, or a place job's proposal.
enum Offer {
    Route(Arc<RouteSolution>),
    Place(Arc<PlaceJob>, Arc<PlaceOffer>),
}

impl Checker {
    fn new(output: JobOutput, spawn: Arc<dyn Spawn>) -> Arc<Checker> {
        Arc::new(Checker {
            output,
            spawn,
            state: Mutex::new(CheckState { waiting: None, running: false }),
            progress: OnceLock::new(),
        })
    }

    /// The job's progress: it hands this checker a partial every `every`, and the next one as soon
    /// as the checker is idle again, at most one per `floor`. So partials follow the checks instead
    /// of the cadence. `after(step)` runs on the engine's thread right after each hand-over (the
    /// `test-hooks` hold). The progress holds the checker; the checker holds it only weakly.
    fn progress(
        self: &Arc<Self>,
        every: Duration,
        floor: Duration,
        after: Option<Arc<dyn Fn(u32) + Send + Sync>>,
        status: impl Fn(NetStatus) + Send + Sync + 'static,
    ) -> Arc<Progress> {
        let partials = self.clone();
        let progress = Arc::new(
            Progress::with_sink(every, move |step, solution| {
                partials.offer(step, Offer::Route(solution), false);
                if let Some(after) = &after {
                    after(step);
                }
            })
            .with_floor(floor)
            .with_status(status),
        );
        let set = self.progress.set(Arc::downgrade(&progress));
        debug_assert!(set.is_ok(), "a checker belongs to one job's progress");
        progress
    }

    fn offer(self: &Arc<Self>, step: u32, offer: Offer, last: bool) {
        let (replaced, start) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let replaced = state.waiting.replace((step, offer, last));
            let start = !state.running;
            if start {
                state.running = true;
            }
            (replaced, start)
        };
        drop(replaced);
        if start {
            let checker = self.clone();
            self.spawn.spawn(Box::new(move || checker.drain()));
        }
    }

    fn drain(&self) {
        loop {
            let offer = {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let offer = state.waiting.take();
                if offer.is_none() {
                    state.running = false;
                }
                offer
            };
            let Some((step, offer, last)) = offer else {
                // Idle again: ask for the router's next partial, outside the lock.
                if let Some(progress) = self.progress.get().and_then(Weak::upgrade) {
                    progress.want_offer();
                }
                return;
            };
            match &offer {
                Offer::Route(solution) => self.output.publish(step, solution, last),
                Offer::Place(job, place) => self.output.publish_place(step, job, place, last),
            }
        }
    }
}

#[derive(Clone)]
struct JobOutput {
    job: JobId,
    base: Base,
    basis: Arc<BasisCell>,
    emit: Emit,
    next_proposal: Arc<AtomicU32>,
}

impl JobOutput {
    /// Route `problem` by `method` and return the final step and solution.
    fn solve(&self, problem: &RouteProblem, progress: &Progress, method: Method) -> (u32, RouteSolution) {
        match method {
            Method::Interactive => {
                let solution = GridRouter::default().route(problem, progress);
                (solution.metrics.work.transactions, solution)
            }
            Method::Organic => crate::organic::job::route(problem, self.basis.get(), progress),
        }
    }

    fn publish(&self, step: u32, solution: &RouteSolution, last: bool) {
        let mut proposal = RouteProposal {
            pair: None,
            id: ProposalId(self.next_proposal.fetch_add(1, Ordering::Relaxed)),
            source: ProposalSource::Job { job: self.job, step },
            unit: self.base.unit,
            base_generation: self.base.board.generation,
            base_hash: self.base.hash,
            ops: solution.ops(),
            checked: None,
            metrics: solution.metrics.clone(),
            verified: false,
            collisions: Vec::new(),
            open: solution.open.clone(),
            head: None,
            solve_us: 0,
            last,
            label: if last {
                "route"
            } else if solution
                .add
                .iter()
                .any(|it| matches!(it.origin(), Origin::Solved { by } if *by == solvers::ORGANIC))
            {
                // the organic method relaxing: legal copper that is still moving
                "route (settling)"
            } else {
                "route (partial)"
            }
            .into(),
        };
        self.check_into(&mut proposal, |_| true);
        (self.emit)(proposal);
    }

    /// Apply `proposal`'s ops to a copy of the base and check them there: its `checked`, the new
    /// findings that `counts`, and whether it is verified (none), or a label saying the ops did not
    /// apply.
    fn check_into(&self, proposal: &mut RouteProposal, counts: fn(&Violation) -> bool) {
        let mut board = (*self.base.board).clone();
        let checked = check(&mut board, &proposal.ops, &self.check_state(&IndexChange::all())).map(|out| (board, out));
        Self::fill_checked(proposal, checked, counts);
    }

    /// The committed board state against which every offer and retry is checked.
    fn check_state<'a>(&'a self, since: &'a IndexChange) -> State<'a> {
        let basis = self.basis.get();
        State {
            rules: &self.base.rules,
            fills: &self.base.fills,
            index: &basis.index,
            conn: &self.base.conn,
            violations: &basis.violations,
            since,
        }
    }

    /// `proposal`'s `checked`, the new findings that `counts`, and whether it is verified (none),
    /// or a label saying the ops did not apply.
    fn fill_checked(proposal: &mut RouteProposal, checked: Result<(Board, Outcome), OpError>, counts: fn(&Violation) -> bool) {
        match checked {
            Ok((board, out)) => {
                proposal.metrics.new_violations = out.new.iter().filter(|&&i| counts(&out.violations[i])).count() as u32;
                proposal.verified = proposal.metrics.new_violations == 0;
                proposal.checked = Some(Arc::new(Checked {
                    board: Arc::new(board),
                    conn: Arc::new(out.conn),
                    violations: Arc::new(out.violations),
                }));
            }
            Err(error) => proposal.label = format!("{} application failed: {error}", proposal.label),
        }
    }

    /// A place job's proposal, checked as a route's is: its ops applied to a copy of the base and
    /// DRC run near them; `verified` means no new finding. The router's copper that a new finding
    /// names (a part's new spot collides with it, or a via the removals left joining one layer) is
    /// removed with what hangs on it (`branches`) and the proposal checked again, until no new
    /// finding names any: the router lays it again. Authored and locked copper stay, and a finding
    /// on them stays. Each round removes copper, so the rounds end; one check each, and none
    /// when the first check finds nothing new.
    fn publish_place(&self, step: u32, job: &PlaceJob, place: &PlaceOffer, last: bool) {
        let mut proposal = no_proposal(
            ProposalId(self.next_proposal.fetch_add(1, Ordering::Relaxed)), self.job, self.base.unit,
            (self.base.board.generation, self.base.hash), if last { "place" } else { "place (partial)" }.into(),
        );
        (proposal.source, proposal.last, proposal.ops) = (ProposalSource::Job { job: self.job, step }, last, place.ops.clone());
        let mut metrics = place.metrics.clone();
        // Placing makes connections for the router: an unrouted one is the job's purpose, not a fault.
        let counts: fn(&Violation) -> bool = |v| v.kind != CheckKind::Unconnected;
        let since = IndexChange::all();
        let state = self.check_state(&since);
        let rooms = place.rooms.as_deref().unwrap_or(&job.rooms);
        let check_ops = |ops: &[BoardOp]| job.check_with_rooms(&self.base.board, ops, &state, rooms);
        let mut checked = check_ops(&proposal.ops);
        let mut removed: BTreeSet<ItemId> = proposal.ops.iter().filter_map(|op| match op { BoardOp::Remove(id) => Some(*id), _ => None }).collect();
        while let Ok((_, out)) = &checked {
            let seeds: Vec<ItemId> = out.new.iter().map(|&i| &out.violations[i]).filter(|v| counts(v))
                .flat_map(|v| v.items.iter().filter_map(|s| match s { Sel::Item(_, id) => Some(*id), _ => None }))
                .collect();
            let more: Vec<ItemId> = branches(&self.base.board, seeds, Takes::Solved).into_iter().filter(|id| !removed.contains(id)).collect();
            if more.is_empty() { break }
            metrics.ripped += more.len() as u32;
            metrics.fix_rounds += 1;
            removed.extend(more.iter().copied());
            proposal.ops.extend(more.into_iter().map(BoardOp::Remove));
            checked = check_ops(&proposal.ops);
        }
        proposal.metrics.place = Some(metrics);
        Self::fill_checked(&mut proposal, checked, counts);
        (self.emit)(proposal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::route_channel;
    use crate::gesture::{Gesture, GestureId, GestureKind};
    use crate::proposal::Tier;
    use fab_elec_board::zone::fill_all;
    use fab_elec_board::{connect, resolve_rules, Anchor, BoardOp};
    use fab_elec_core::{LayerId, PadIx, P};
    use std::sync::mpsc;

    struct Queue(Mutex<Vec<Box<dyn FnOnce() + Send>>>);

    impl Spawn for Queue {
        fn spawn(&self, task: Box<dyn FnOnce() + Send>) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).push(task);
        }
    }

    impl Queue {
        fn run(&self) {
            loop {
                let task = self.0.lock().unwrap_or_else(|e| e.into_inner()).pop();
                let Some(task) = task else {
                    return;
                };
                task();
            }
        }
    }

    /// The shove answer's S2 and its finding 2: a part drag's wave steps on the worker's thread
    /// whenever its inbox is empty, and the commands served between its steps do not drop it: its
    /// Full answer follows the stretch answer for the same sample. A base of a new generation
    /// answers the sample again, wave and all, on that base.
    #[test]
    fn a_shove_wave_steps_between_other_work_and_its_full_answer_follows() {
        let sources = fab_elec_fixtures::docs("drum16").expect("fixture setup: drum16 is a stock project");
        let evaluated = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).expect("fixture setup: drum16 evaluates");
        let unit = evaluated.design.units.iter().find(|u| u.name == "drum16").expect("fixture setup: the drum16 unit");
        let board = (**unit.board.as_ref().expect("fixture setup: drum16 has a board")).clone();
        let rules = Arc::new(resolve_rules(&board, None));
        let fills = Arc::new(Vec::new());
        let conn = Arc::new(connect(&board, &fills));
        let b0 = Base::new(Arc::new(board), rules, conn, fills);
        let part = b0.board.placed.iter().find(|p| p.refdes == "C7").expect("fixture setup: C7");
        let joined = (0..b0.board.nets.len()).map(|i| NetId(i as u32)).find(|net| !b0.conn.unrouted.contains(net))
            .expect("fixture setup: drum16 has a routed net, whose explanation is published at once");
        let mut gesture = Gesture::new(GestureKind::DragParts, b0.unit, b0.board.generation, Anchor::Free, part.at, LayerId::F_CU);
        gesture.moved = vec![(part.id, crate::gesture::Xform::translate(P::mm(0.0, -1.0)))];
        let (wake, wakes) = mpsc::channel();
        let (handle, inbox) = route_channel(move || {
            let _ = wake.send(());
        });
        let next_event = || loop {
            if let Some(event) = handle.try_recv() {
                return event;
            }
            wakes.recv_timeout(Duration::from_secs(60)).expect("the worker publishes");
        };
        // Queued before the worker starts: the base, the sample, then a cancel and an explanation,
        // which wait in the inbox while the sample's wave is parked.
        handle.request(RouteRequest::Base(b0.clone())).unwrap();
        handle.sample(GestureSample { id: GestureId(1), seq: 1, gesture });
        handle.request(RouteRequest::Cancel(JobId(9))).unwrap();
        handle.request(RouteRequest::Explain { unit: b0.unit, net: joined }).unwrap();
        let queue = Arc::new(Queue(Mutex::new(Vec::new())));
        let worker = RouteWorker::new(inbox, queue.clone());
        let thread = std::thread::spawn(move || worker.run());
        let source = |seq, tier| ProposalSource::Gesture { id: GestureId(1), seq, tier };
        assert!(matches!(next_event(), RouteEvent::Proposal(p) if p.source == source(1, Tier::Stretch) && !p.last),
            "the stretch answer comes first and says a Full answer follows");
        assert!(matches!(next_event(), RouteEvent::Explain(e) if e.net == joined), "the commands are served while the wave waits");
        assert!(matches!(next_event(), RouteEvent::Proposal(p) if p.source == source(1, Tier::Full) && p.last && p.verified),
            "the wave's Full answer follows the cancel and the explanation, for the same sample");
        // A new generation of the board: the parked sample is answered again on it, wave and all.
        let mut board = (*b0.board).clone();
        let other = board.placed.iter().find(|p| p.refdes == "R22").expect("fixture setup: R22").id;
        board.apply_all(&[BoardOp::Move { item: other, by: P::mm(0.1, 0.0) }]).expect("fixture setup: a part moves");
        let generation = board.generation;
        let b1 = Base::new(Arc::new(board), b0.rules.clone(), b0.conn.clone(), b0.fills.clone());
        handle.request(RouteRequest::Base(b1)).unwrap();
        assert!(matches!(next_event(), RouteEvent::Proposal(p) if p.source == source(1, Tier::Stretch) && p.base_generation == generation && !p.last),
            "a new base answers the sample again");
        assert!(matches!(next_event(), RouteEvent::Proposal(p) if p.source == source(1, Tier::Full) && p.base_generation == generation && p.last),
            "and its wave runs on the new base");
        drop(handle);
        thread.join().expect("the worker ends when the channel closes");
    }

    #[test]
    fn the_sample_goes_first_and_heavy_work_runs_on_the_pool() {
        let design = fab_elec_fixtures::ledctl();
        let board = design.units[0].board.clone().expect("fixture setup: ledctl unit 0 has a board");
        let rules = Arc::new(resolve_rules(&board, None));
        let fills = Arc::new(fill_all(&board, &rules));
        let conn = Arc::new(connect(&board, &fills));
        let b0 = Base::new(board, rules, conn, fills);
        let mut board = (*b0.board).clone();
        let moved = board.placed[0].id;
        board.apply_all(&[BoardOp::Move { item: moved, by: P::mm(0.5, 0.0) }]).expect("fixture setup: a part moves");
        let board = Arc::new(board);
        let fills = Arc::new(fill_all(&board, &b0.rules));
        let conn = Arc::new(connect(&board, &fills));
        let mut b1 = Base::new(board, b0.rules.clone(), conn, fills);
        let delta = BoardDelta::between(&b0.board, &b1.board);
        assert!(!delta.global, "fixture setup: a move has a regional delta");
        b1.delta = Some(delta);
        let open = *b1.conn.unrouted.iter().next().expect("fixture setup: ledctl has unrouted nets");
        let joined = b1.board.net("GND").expect("fixture setup: GND exists");
        assert!(!b1.conn.unrouted.contains(&joined), "fixture setup: GND is joined by its pour");
        let job = RouteJob {
            job: JobId(1),
            unit: b1.unit,
            scope: RouteScope::Nets(vec![open]),
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
        let next_event = || loop {
            if let Some(event) = handle.try_recv() {
                return event;
            }
            wakes.recv_timeout(Duration::from_secs(60)).expect("the worker publishes");
        };
        // Everything is queued before the worker starts: the commands sit ahead of the sample.
        for request in [
            RouteRequest::Base(b0),
            RouteRequest::Base(b1.clone()),
            RouteRequest::Route(job),
            RouteRequest::Explain { unit: b1.unit, net: open },
            RouteRequest::Cancel(JobId(1)),
        ] {
            handle.request(request).unwrap();
        }
        let gesture = Gesture::new(GestureKind::Route, b1.unit, b1.board.generation, Anchor::Free, P::ZERO, LayerId::F_CU);
        handle.sample(GestureSample { id: GestureId(1), seq: 5, gesture });
        let queue = Arc::new(Queue(Mutex::new(Vec::new())));
        let worker = RouteWorker::new(inbox, queue.clone());
        let thread = std::thread::spawn(move || worker.run());
        let worker_thread = thread.thread().id();

        assert!(
            matches!(next_event(), RouteEvent::Refused(r) if r.seq == 5),
            "the sample is answered before the route, explanation and cancel queued ahead of it"
        );
        // A round trip: an explanation for a joined net needs no route, so it is published at once.
        handle.request(RouteRequest::Explain { unit: b1.unit, net: joined }).unwrap();
        assert!(
            matches!(next_event(), RouteEvent::Explain(e) if e.net == joined && e.reason.is_none()),
            "an open net's explanation waits on the pool; a joined net's is published at once"
        );
        assert_eq!(queue.0.lock().unwrap_or_else(|e| e.into_inner()).len(), 2, "the route and the explanation's route wait on the pool");
        // A finishing sample from a front pad of the open net: its stretch answer at once, its
        // Finish on the pool (a round trip later, the worker has queued it).
        let (anchor, at) = b1.board.placed.iter().find_map(|placed| {
            (0..placed.pad_nets.len()).map(|i| PadIx(i as u16))
                .find(|&pad| placed.pad_nets[pad.ix()] == Some(open)
                    && placed.pad_layers(pad, b1.board.copper_set(), b1.board.copper_count()).has(LayerId::F_CU))
                .and_then(|pad| Some((Anchor::Pad { placed: placed.id, pad }, placed.pad_at(pad)?)))
        }).expect("fixture setup: the open net has a front pad");
        let mut finishing = Gesture::new(GestureKind::Route, b1.unit, b1.board.generation, anchor, at, LayerId::F_CU);
        finishing.net = Some(open);
        finishing.finish = true;
        handle.sample(GestureSample { id: GestureId(2), seq: 1, gesture: finishing });
        assert!(
            matches!(next_event(), RouteEvent::Proposal(p)
                if p.source == ProposalSource::Gesture { id: GestureId(2), seq: 1, tier: Tier::Stretch } && !p.last),
            "a finishing sample's stretch answer comes at once and says its Finish follows"
        );
        handle.request(RouteRequest::Explain { unit: b1.unit, net: joined }).unwrap();
        assert!(matches!(next_event(), RouteEvent::Explain(e) if e.net == joined), "fixture setup: a round trip");
        assert_eq!(queue.0.lock().unwrap_or_else(|e| e.into_inner()).len(), 3, "the Finish waits on the pool too");
        let on = |thread: std::thread::ThreadId| {
            HEAVY.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(t, _)| *t == thread).map(|(_, what)| *what).collect::<Vec<_>>()
        };
        assert_eq!(on(worker_thread), Vec::<&str>::new(), "two bases, a route and an explanation do no heavy work on the worker's thread");

        queue.run();
        let pool = on(std::thread::current().id());
        for what in ["problem", "explain", "basis", "finish"] {
            assert!(pool.contains(&what), "the pool did the {what} work: {pool:?}");
        }
        let events: Vec<RouteEvent> = std::iter::from_fn(|| handle.try_recv()).collect();
        assert!(
            events.iter().any(|e| matches!(e, RouteEvent::Explain(e) if e.net == open)),
            "the open net's explanation is published from the pool"
        );
        assert!(
            events.iter().any(|e| matches!(e, RouteEvent::Proposal(p)
                if p.source == ProposalSource::Gesture { id: GestureId(2), seq: 1, tier: Tier::Full } && p.last)),
            "the Finish is published from the pool, as the sample's second tier"
        );
        assert_eq!(on(worker_thread), Vec::<&str>::new(), "nothing heavy ran on the worker's thread later either");
        drop(handle);
        thread.join().expect("dropping the handle stops the worker");
    }

    #[test]
    fn checks_run_one_at_a_time_and_the_final_is_published_last() {
        let design = fab_elec_fixtures::ledctl();
        let board = design.units[0].board.clone().expect("fixture setup: ledctl unit 0 has a board");
        let rules = Arc::new(resolve_rules(&board, None));
        let fills = Arc::new(fill_all(&board, &rules));
        let conn = Arc::new(connect(&board, &fills));
        let unit = Unit::new(Base::new(board, rules, conn, fills));
        let queue = Arc::new(Queue(Mutex::new(Vec::new())));
        let make_checker = || {
            let proposals = Arc::new(Mutex::new(Vec::new()));
            let list = proposals.clone();
            let emit: Emit = Arc::new(move |proposal| list.lock().unwrap_or_else(|e| e.into_inner()).push(proposal));
            let output = unit.output(JobId(1), Arc::new(AtomicU32::new(1)), emit);
            (Checker::new(output, queue.clone()), proposals)
        };
        let published = |proposals: &Mutex<Vec<RouteProposal>>| {
            proposals
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .map(|proposal| {
                    let ProposalSource::Job { step, .. } = proposal.source else {
                        panic!("a job's proposal names its step");
                    };
                    (step, proposal.last)
                })
                .collect::<Vec<_>>()
        };

        let (checker, proposals) = make_checker();
        checker.offer(1, Offer::Route(Arc::new(RouteSolution::default())), false);
        checker.offer(2, Offer::Route(Arc::new(RouteSolution::default())), false);
        checker.offer(3, Offer::Route(Arc::new(RouteSolution::default())), true);
        assert_eq!(queue.0.lock().unwrap_or_else(|e| e.into_inner()).len(), 1, "only one check task is queued");
        queue.run();
        assert_eq!(
            published(&proposals),
            vec![(3, true)],
            "a waiting offer is replaced by a newer one, and the final replaces a waiting partial"
        );

        let (checker, proposals) = make_checker();
        checker.offer(1, Offer::Route(Arc::new(RouteSolution::default())), false);
        queue.run();
        checker.offer(2, Offer::Route(Arc::new(RouteSolution::default())), true);
        queue.run();
        assert_eq!(
            published(&proposals),
            vec![(1, false), (2, true)],
            "offers checked one at a time are published in order, the final last"
        );

        checker.offer(4, Offer::Route(Arc::new(RouteSolution::default())), false);
        assert_eq!(
            queue.0.lock().unwrap_or_else(|e| e.into_inner()).len(),
            1,
            "an idle checker starts a new check"
        );
        let pending = queue.0.lock().unwrap_or_else(|e| e.into_inner()).pop();
        drop(pending);

        let hour = Duration::from_secs(3600);
        let (checker, _proposals) = make_checker();
        let progress = checker.progress(hour, Duration::ZERO, None, |_| {});
        progress.offer(1, &RouteSolution::default());
        assert!(!progress.due(2), "fixture setup: after its first copy the job's cadence waits an hour");
        queue.run();
        assert!(progress.due(2), "a checker that is idle again asks its job for the next partial at once");

        let (checker, _proposals) = make_checker();
        let progress = checker.progress(hour, hour, None, |_| {});
        progress.offer(1, &RouteSolution::default());
        queue.run();
        assert!(!progress.due(2), "an idle checker's request waits for the floor since the last copy");
    }
}

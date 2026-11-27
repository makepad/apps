//! The shared electronics host loop: the mailbox, engine strand, displayed snapshot, fills and router.
//! Edits, engine passes and retired snapshots, answers and samples belong to the strand; fills run on the pool.
//! Per event: a channel drain, at most one `try_take`, O(new fill jobs) submits, one short lock
//! for `is_empty` when idle and one for `waited`, plus Arc swaps/retirement on changes. When the
//! router exists, one `try_recv` drain, at most one preview push and one `ElecLive.gesture` swap
//! per answer, plus retries of queued requests; while a job runs: at most one job preview push
//! and one snapshot overlay per event. A backlog
//! flag change clones snapshot fields (O(units), not boards). No model evaluation or board drop on the UI.
//! Hosts supply their engine loader and handle their own messages on the same ordered channel.
//! A job's last proposal, or a Stop, commits its newest preview as one undo step; Discard refuses it.
//! An edit while a route job runs first stops the job and commits what it has routed (`keep_job`).
//! A snapshot hook releases the host's mirrors before the replaced snapshot is retired. Hosts
//! with work between draining and kicking call those methods separately; otherwise use `event`.

use crate::inbox::PlannerEdit;
use crate::{circuits_named, gesture_sample, job_sample, Cmd, DocSet, Engine, Inbox, Job, Mailbox, Outcome, RouteStart, Sample, Step, TaskResult, Ticket, Written};
use fab_elec_board::RuleMatrix;
use fab_elec_core::{BoardId, CompId, ContentHash, DocId, ItemId, NetId};
use fab_elec_circuit::CircuitOp;
use fab_elec_design::{By, Design, DesignOp, TextEdit};
use fab_elec_route::{route_channel, GestureId, GestureSample, JobId, JobPhase, JobStatus, Method, ProposalSource, RouteEvent, RouteHandle, RouteMetrics, RouteRequest, RouteScope, RouteWorker, Spawn};
use fab_elec_view::{DesignSnapshot, ElecLive, JobEnd, Refusal, RouteJobState, StageFlags, ViewAction};
use makepad_widgets_core::makepad_platform::thread::{
    checkpoint_when_due, CancellationToken, Class, SignalToUI, Strand, TaskHandle, TaskPool, ThreadOptions, ThreadSpawner,
    Ticket as TaskTicket, ToUIReceiver, ToUISender,
};
use makepad_widgets_core::{log, Cx};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

enum Msg<X> {
    Snapshot(Arc<DesignSnapshot>),
    Step(Step),
    Fill(TaskResult),
    Host(X),
}

/// The route worker's UI side, made at the first route gesture or job (nothing exists until then).
struct Router {
    handle: RouteHandle,
    /// Units whose base the worker holds, with the content hash of the newest base sent: a
    /// gesture answer is accepted only on it.
    held: BTreeMap<BoardId, ContentHash>,
    /// Units whose base was asked and not yet forwarded.
    asked: BTreeSet<BoardId>,
    /// The newest sample of a unit whose base is not forwarded yet; relayed when it is.
    waiting: Option<GestureSample>,
    /// The pane's live route gesture.
    gesture: Option<LiveGesture>,
    /// The host's running or ended route job, independent of the pane's gesture.
    job: Option<RunningJob>,
    /// Requests a full worker queue gave back, sent first on the next drain.
    retry: VecDeque<RouteRequest>,
    /// `ElecLive.gesture` must be cleared on the next drain (the gesture ended in `act`, which has no Cx).
    clear_shown: bool,
    /// Route invokes that arrived during the live gesture, in order: one writer per board, so a
    /// route waits until the gesture ends and then plans on what it committed.
    deferred: Vec<Cmd>,
}

impl Router {
    fn new(handle: RouteHandle) -> Self {
        Self {
            handle, held: BTreeMap::new(), asked: BTreeSet::new(), waiting: None,
            gesture: None, job: None, retry: VecDeque::new(), clear_shown: false, deferred: Vec::new(),
        }
    }
}

/// The host's route job, from its start until the next one, a discard or a reset.
struct RunningJob {
    id: JobId,
    tag: u64,
    unit: BoardId,
    /// The content hash of the base it runs on: job ids restart with each engine, so a late
    /// answer of a job from before a reset must not pass for this one.
    base: ContentHash,
    by: By,
    /// The sequence reserved at the start, for a refusal before any proposal arrives.
    reserved: u64,
    /// The sequence of its newest preview.
    seq: Option<u64>,
    ended: bool,
}

struct LiveGesture {
    circuit: Option<(BoardId, Weak<fab_elec_circuit::Circuit>)>,
    id: GestureId,
    tag: u64,
    seq: u64,
    state: GestureState,
    ended: bool,
}

enum GestureState {
    Waiting,
    Answered,
    Refused(String),
}

struct RouteSpawn(TaskPool);

impl Spawn for RouteSpawn {
    fn spawn(&self, task: Box<dyn FnOnce() + Send>) {
        match self.0.submit(Class::Background, task) {
            Ok(task) => task.detach(),
            Err(error) => log!("electronics route job: {error:?}"),
        }
    }
}

fn route_request(router: &mut Router, r: RouteRequest) {
    if !router.retry.is_empty() {
        router.retry.push_back(r);
        return;
    }
    if let Err(r) = router.handle.request(r) {
        router.retry.push_back(r);
    }
}

/// Sends a host's own message on the Driver's channel, ordered with the engine's. Clone.
pub struct HostTx<X>(ToUISender<Msg<X>>);

impl<X> Clone for HostTx<X> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<X> HostTx<X> {
    /// A closed receiver discards the message, as engine sends do.
    pub fn send(&self, msg: X) {
        let _ = self.0.send(Msg::Host(msg));
    }
}

/// What `Driver::drain` hands its host, in channel order.
pub enum Drained<'a, X> {
    /// A new snapshot, before the one it replaces is retired (a host releases what it mirrors of
    /// the old one first). `first`: no snapshot was shown before it.
    Snapshot { snap: &'a Arc<DesignSnapshot>, first: bool },
    /// The host's own message. `shown`: a snapshot is shown.
    Host { msg: X, shown: bool },
}

/// The engine strand and the UI side of its shared electronics host loop.
pub struct Driver<X: Send + 'static> {
    mailbox: Mailbox,
    strand: Strand<Option<Engine>>,
    job: Option<TaskHandle<()>>,
    rx: ToUIReceiver<Msg<X>>,
    snap: Option<Arc<DesignSnapshot>>,
    /// Per key, the planning task and the token that stops its queued tiles.
    fills: HashMap<(BoardId, ItemId, bool), (TaskHandle<()>, CancellationToken)>,
    /// Per unit, the whole-board checks task and the generation it checks.
    checks: HashMap<BoardId, (u64, TaskHandle<()>)>,
    /// Independent commit, preview and proposal evaluations. IDs identify their pool results.
    eval_task: Option<(u64, TaskHandle<()>)>,
    preview_task: Option<(u64, TaskHandle<()>)>,
    proposal_task: Option<(u64, TaskHandle<()>)>,
    /// Held commands or evaluation can still move the shared undo tops.
    waiting: bool,
    /// A late result must not remove a newer task's handle. Weak rules keep disposal on the worker.
    /// The inputs per key: generation, rules and (for a preview job) the preview's sample seq.
    fill_inputs: HashMap<(BoardId, ItemId, bool), (u64, Weak<RuleMatrix>, Option<u64>)>,
    preview_seq: Arc<AtomicU64>,
    tickets: u64,
    replies: HashMap<Ticket, Sender<Outcome>>,
    /// The tool label retained for a reload refusal, removed with its reply.
    tools: HashMap<Ticket, String>,
    /// The view's live sample (seq, tag): its commit, cancel or refusal names it.
    live_preview: Option<(u64, u64)>,
    /// The view's sample the last reset ended: its drop is refused under its tag by the reloaded
    /// engine, so the view still gets its one answer.
    ended_preview: Option<(u64, u64)>,
    /// The tag of the view's newest preview when it only sets values (a value dragged through its
    /// series): the live run takes that preview while it shows ([`Driver::sim_design`]).
    value_preview: Option<u64>,
    router: Option<Router>,
    job_state: Option<Arc<RouteJobState>>,
    parked: Option<(Ticket, JobId)>,
    /// A start can already be in the inbox when the person cancels the parked job.
    cancelled_wait: Option<JobId>,
    place_notice: Option<String>,
    /// The running or ended job's last proposal's metrics (None until its last proposal), for
    /// the caller's answer: what it routed and left open.
    job_metrics: Option<RouteMetrics>,
    pool: TaskPool,
    spawner: ThreadSpawner,
    /// A worker startup failure has already been logged.
    router_failed: bool,
    undo: Option<u64>,
    redo: Option<u64>,
    backlog: bool,
    /// Numbers come from this shared counter; a reservation takes one early.
    steps: Arc<AtomicU64>,
    /// The newest entry logged by either history.
    logged: Arc<AtomicU64>,
    /// The files of the last step that changed them, until the app hands them to the plate.
    docs_out: Option<Written>,
}

impl<X: Send + 'static> Driver<X> {
    /// An empty host loop using the project's shared number and logged-entry counters.
    pub fn new(cx: &mut Cx, steps: Arc<AtomicU64>, logged: Arc<AtomicU64>) -> Self {
        // Board computations on the engine strand (a large board's full fill) give the pool a turn
        // when due. The strand takes its state out of its lock while a job runs.
        fab_elec_core::pace::install_pace(|| { let _ = checkpoint_when_due(); });
        Self {
            mailbox: Mailbox::default(),
            strand: Strand::with_state(&cx.task_pool(), "elec engines", None::<Engine>),
            job: None,
            rx: ToUIReceiver::default(),
            snap: None,
            fills: HashMap::new(),
            checks: HashMap::new(),
            eval_task: None,
            preview_task: None,
            proposal_task: None,
            waiting: false,
            fill_inputs: HashMap::new(),
            preview_seq: Arc::new(AtomicU64::new(0)),
            tickets: 0,
            replies: HashMap::new(),
            tools: HashMap::new(),
            live_preview: None,
            ended_preview: None,
            value_preview: None,
            router: None,
            job_state: None,
            parked: None, cancelled_wait: None, place_notice: None,
            job_metrics: None,
            pool: cx.task_pool(),
            spawner: cx.thread_spawner(),
            router_failed: false,
            undo: None,
            redo: None,
            backlog: false,
            steps,
            logged,
            docs_out: None,
        }
    }

    fn router(&mut self) -> Option<&mut Router> {
        if self.router.is_none() {
            let (handle, inbox) = route_channel(SignalToUI::set_ui_signal);
            let spawn = Arc::new(RouteSpawn(self.pool.clone()));
            let worker = RouteWorker::new(inbox, spawn);
            let options = ThreadOptions { name: Some("elec route".into()), ..Default::default() };
            match self.spawner.spawn_worker(options, move || worker.run()) {
                Ok(worker) => {
                    worker.detach();
                    self.router = Some(Router::new(handle));
                }
                Err(error) => {
                    if !self.router_failed {
                        log!("electronics route worker: {error:?}");
                        self.router_failed = true;
                    }
                    return None;
                }
            }
        }
        self.router.as_mut()
    }

    fn next_seq(&self) -> u64 {
        self.preview_seq.fetch_add(1, SeqCst) + 1
    }

    /// A sender for host messages, on the engine's ordered channel.
    pub fn sender(&self) -> HostTx<X> {
        HostTx(self.rx.sender())
    }

    /// Numbers come from this shared counter; a reservation takes one early.
    pub fn steps(&self) -> &Arc<AtomicU64> {
        &self.steps
    }

    /// The newest entry logged by either history.
    pub fn logged(&self) -> &Arc<AtomicU64> {
        &self.logged
    }

    /// Run metadata work in command order with accepted edits and bed changes.
    pub fn on_strand(&self, job: impl FnOnce() + Send + 'static) {
        self.strand.submit(Class::Visible, TaskTicket::none(), move |_| job()).detach();
    }

    /// The model's first plate notification may follow the initial empty engine.
    pub fn attach_journal(&self, sink: Arc<dyn crate::JournalSink>) {
        self.strand.submit(Class::Visible, TaskTicket::none(), move |state| {
            if let Some(engine) = state.take() { *state = Some(engine.with_journal(Some(sink))); }
        }).detach();
    }

    /// The strand job that (re)loads the engine; the engine it replaces, with its log and
    /// preview, and `old` (the commands the old engine did not take and the snapshots retired
    /// for it) are dropped there, off the UI thread.
    pub fn load(&mut self, _cx: &mut Cx, old: Option<Inbox>, make: impl FnOnce(&HostTx<X>) -> Engine + Send + 'static) {
        let tx = self.sender();
        let preview_seq = self.preview_seq.clone();
        self.job = Some(self.strand.submit(Class::Visible, TaskTicket::none(), move |state| {
            drop(state.take());
            drop(old);
            let engine = make(&tx).with_preview_seq(preview_seq);
            *state = Some(engine);
            if let Some(engine) = state {
                let _ = tx.0.send(Msg::Snapshot(engine.snapshot()));
                // A design loaded board first owes its fills: a first step hands them to the pool.
                if !fab_elec_design::engines::owed(engine.results()).is_empty() {
                    let step = engine.step(Inbox::default(), &mut |snap| {
                        let _ = tx.0.send(Msg::Snapshot(snap));
                    });
                    let _ = tx.0.send(Msg::Step(step));
                }
            }
        }));
    }

    /// Cancel fills, checks and all three evaluation tasks, retire the shown snapshot and replace the channel before
    /// reloading. The returned inbox belongs to the load job, so its contents drop on the strand.
    /// Cost: O(fill jobs + pending replies) per reset.
    pub fn reset(&mut self) -> Inbox {
        for (ticket, reply) in self.replies.drain() {
            let label = self.tools.remove(&ticket).expect("each pending reply retains its tool label");
            let _ = reply.send(Outcome::Refused(Arc::new(Refusal {
                seq: 0, label, op: "invoke",
                text: "the electronics engine loaded the bed's documents again; ask again".into(),
                reason: "The electronics documents were reloaded, so try the edit again.".into(),
                site: None, sample: None,
            })));
        }
        for (_, (task, tiles)) in self.fills.drain() {
            task.cancel();
            tiles.cancel();
        }
        for (_, (_, task)) in self.checks.drain() {
            task.cancel();
        }
        self.fill_inputs.clear();
        if let Some((_, task)) = self.eval_task.take() {
            task.cancel();
        }
        if let Some((_, task)) = self.preview_task.take() {
            task.cancel();
        }
        if let Some((_, task)) = self.proposal_task.take() {
            task.cancel();
        }
        self.waiting = false;
        if let Some(old) = self.snap.take() {
            self.mailbox.retire(old);
        }
        let old = self.mailbox.take();
        self.rx = ToUIReceiver::default();
        self.ended_preview = self.live_preview.take();
        if let Some(router) = &mut self.router {
            router.held.clear();
            router.asked.clear();
            router.waiting = None;
            router.retry.clear();
            router.gesture = None;
            // Their callers were refused with every pending reply above.
            router.deferred.clear();
            // The worker stops the old engine's job; its late answers no longer match a job here.
            if let Some(job) = router.job.take().filter(|job| !job.ended) {
                route_request(router, RouteRequest::Cancel(job.id));
            }
            router.clear_shown = true;
        }
        self.job_state = None;
        self.parked = None;
        self.cancelled_wait = None;
        self.place_notice = None;
        self.undo = None;
        self.redo = None;
        self.docs_out = None;
        old
    }

    /// Drain messages in channel order, then take a completed job once and drain any sends
    /// racing its completion before the next kick.
    pub fn drain(&mut self, cx: &mut Cx, on: &mut dyn FnMut(&mut Cx, &HostTx<X>, Drained<'_, X>) -> bool) -> bool {
        let mut changed = self.drain_messages(cx, on);
        if let Some(result) = self.job.as_mut().and_then(|job| job.try_take()) {
            if let Err(error) = result {
                log!("electronics engine: {error:?}");
            }
            self.job = None;
            // Completion follows every send: consume a Step racing the first drain before
            // kick, so that Step cannot clear the next job's handle on a later event.
            changed |= self.drain_messages(cx, on);
        }
        let mut pushed = false;
        let mut events = Vec::new();
        if let Some(router) = &mut self.router {
            while let Some(request) = router.retry.pop_front() {
                if let Err(request) = router.handle.request(request) {
                    router.retry.push_front(request);
                    break;
                }
            }
            // A base deferred by a full queue can now precede its waiting sample.
            if router.retry.is_empty() && router.waiting.as_ref().is_some_and(|s| router.held.contains_key(&s.gesture.unit)) {
                router.handle.sample(router.waiting.take().expect("the waiting sample's base is forwarded"));
            }
            if router.clear_shown && cx.has_global::<ElecLive>() {
                if let Some(old) = cx.get_global::<ElecLive>().gesture.take() {
                    self.mailbox.retire_answer(old);
                    changed = true;
                    pushed = true;
                }
                router.clear_shown = false;
            }
            while let Some(event) = router.handle.try_recv() {
                events.push(event);
            }
        }
        pushed |= !events.is_empty();
        // A worker can publish again while try_recv drains. Keep one job preview push per
        // event, retiring any intermediate answer here just like a replaced mailbox sample.
        let running = self.router.as_ref().and_then(|router| router.job.as_ref()).filter(|job| !job.ended);
        let newest = events.iter().rposition(|event| matches!(event,
            RouteEvent::Proposal(p) if matches!(p.source, ProposalSource::Job { job, .. }
                if running.is_some_and(|running| running.id == job && running.unit == p.unit && running.base == p.base_hash))));
        for (i, event) in events.into_iter().enumerate() {
            if newest.is_some_and(|newest| i < newest) && matches!(&event, RouteEvent::Proposal(p)
                if matches!(p.source, ProposalSource::Job { .. })) {
                if let RouteEvent::Proposal(p) = event {
                    self.mailbox.retire_answer(Arc::new(p));
                }
                continue;
            }
            changed |= self.route_event(cx, event);
        }
        if pushed {
            self.kick();
        }
        changed |= self.overlay_job();
        changed
    }

    fn route_event(&mut self, cx: &mut Cx, event: RouteEvent) -> bool {
        match event {
            RouteEvent::Proposal(p) if matches!(p.source, ProposalSource::Job { job, .. }
                if self.router.as_ref().and_then(|router| router.job.as_ref())
                    .is_some_and(|running| running.id == job && running.unit == p.unit && running.base == p.base_hash
                        && !running.ended)) => {
                let answer = Arc::new(p);
                let last = answer.last;
                let open = if last { Arc::new(answer.open.clone()) } else { Arc::default() };
                let metrics = last.then(|| answer.metrics.clone());
                let seq = self.next_seq();
                let running = self.router.as_mut().and_then(|router| router.job.as_mut())
                    .expect("the answer matches the running job");
                let refusal = answer.metrics.tune.as_ref().and_then(|result| result.as_ref().err())
                    .or(answer.metrics.pair_refusal.as_ref()).cloned();
                if refusal.is_some() {
                    self.mailbox.retire_answer(answer);
                } else {
                    self.mailbox.preview(job_sample(answer, running.by, seq).expect("a job answer"));
                }
                running.seq = Some(seq);
                if last {
                    self.job_metrics = metrics;
                    self.mailbox.push(match refusal {
                        Some(reason) => Cmd::RefusePreview { seq, tag: running.tag, reason },
                        None => Cmd::CommitPreview { seq, tag: running.tag },
                    });
                    running.ended = true;
                    let state = self.job_state.as_ref().expect("a running job has status");
                    self.job_state = Some(Arc::new(RouteJobState { end: Some(JobEnd::Finished), open, ..(**state).clone() }));
                }
                true
            }
            // A gesture answer counts only on the newest base sent: one made on a superseded base is
            // retired below, and the worker answers the newest sample again on the new base.
            RouteEvent::Proposal(p) if matches!(p.source, ProposalSource::Gesture { id, .. }
                if self.router.as_ref().is_some_and(|router| router.held.get(&p.unit) == Some(&p.base_hash)
                    && router.gesture.as_ref().is_some_and(|gesture| gesture.id == id && !gesture.ended))) => {
                let answer = Arc::new(p);
                let seq = self.next_seq();
                let mut preview = gesture_sample(answer.clone(), By::Person, seq).expect("a gesture answer");
                let gesture = self.router.as_mut().and_then(|router| router.gesture.as_mut())
                    .expect("the answer matches the live gesture");
                preview.circuits.extend(gesture.circuit.clone());
                self.mailbox.preview(preview);
                gesture.seq = seq;
                gesture.state = GestureState::Answered;
                self.live_preview = Some((seq, gesture.tag));
                if cx.has_global::<ElecLive>() {
                    if let Some(old) = cx.get_global::<ElecLive>().gesture.replace(answer) {
                        self.mailbox.retire_answer(old);
                    }
                } else {
                    self.mailbox.retire_answer(answer);
                }
                true
            }
            RouteEvent::Refused(r) => {
                let Some(gesture) = self.router.as_mut().and_then(|router| router.gesture.as_mut())
                    .filter(|gesture| gesture.id == r.id && !gesture.ended) else { return false };
                gesture.state = GestureState::Refused(r.text.clone());
                self.mailbox.push(Cmd::RefusePreview { seq: gesture.seq, tag: gesture.tag, reason: r.text });
                true
            }
            RouteEvent::Proposal(p) => {
                self.mailbox.retire_answer(Arc::new(p));
                false
            }
            RouteEvent::Status(s) => {
                if !self.router.as_ref().and_then(|router| router.job.as_ref()).is_some_and(|running| running.id == s.job) {
                    return false;
                }
                let current_name = s.current.and_then(|net| {
                    let board = self.snap.as_ref()?.design.unit(s.unit)?.board.as_ref()?;
                    board.nets.get(net.0 as usize).map(|net| Arc::<str>::from(net.name.as_str()))
                });
                let (end, scope, method, open) = match &self.job_state {
                    Some(state) => (state.end, state.scope.clone(), state.method, state.open.clone()),
                    None => (None, Arc::default(), Method::default(), Arc::default()),
                };
                self.job_state = Some(Arc::new(RouteJobState { status: s, current_name, end, scope, method, open }));
                true
            }
            RouteEvent::Explain(_) => false,
        }
    }

    fn drain_messages(&mut self, cx: &mut Cx, on: &mut dyn FnMut(&mut Cx, &HostTx<X>, Drained<'_, X>) -> bool) -> bool {
        let mut changed = false;
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Msg::Snapshot(snap) => {
                    let first = self.snap.is_none();
                    on(cx, &self.sender(), Drained::Snapshot { snap: &snap, first });
                    if let Some(old) = self.snap.replace(snap) {
                        self.mailbox.retire(old);
                    }
                    changed = true;
                }
                Msg::Step(step) => {
                    self.job = None;
                    self.waiting = step.waiting;
                    self.undo = step.undo;
                    self.redo = step.redo;
                    if let Some((ticket, id, unit)) = step.parked {
                        self.parked = Some((ticket, id));
                        self.job_metrics = None;
                        self.job_state = Some(Arc::new(RouteJobState {
                            status: JobStatus { job: id, unit, phase: JobPhase::WaitCase, current: None,
                                routed: 0, failed: 0, total: 0, attempt: 0, retry: None, blocked_by: None },
                            current_name: None, end: None, scope: Arc::new(RouteScope::Parts(Vec::new())),
                            method: Method::default(), open: Arc::default(),
                        }));
                    }
                    self.answer(step.outcomes);
                    if let Some(docs) = step.docs {
                        self.docs_out = Some(docs);
                    }
                    for (label, reason) in step.rejected {
                        log!("electronics: {label}: {reason}");
                    }
                    for base in step.bases {
                        if let Some(router) = &mut self.router {
                            let unit = base.unit;
                            router.held.insert(unit, base.hash);
                            router.asked.remove(&unit);
                            route_request(router, RouteRequest::Base(base));
                            if router.retry.is_empty() && router.waiting.as_ref().is_some_and(|s| s.gesture.unit == unit) {
                                router.handle.sample(router.waiting.take().expect("the waiting sample is for this base"));
                            }
                        }
                    }
                    for RouteStart { ticket, by, job, base } in step.routes {
                        if self.cancelled_wait == Some(job.id()) { continue }
                        if self.parked == Some((ticket, job.id())) { self.parked = None; }
                        if self.router().is_none() {
                            self.mailbox.push(Cmd::RefusePreview {
                                seq: self.next_seq(), tag: job.id().tag(), reason: "the route worker could not start".into(),
                            });
                            continue;
                        }
                        let reserved = self.next_seq();
                        let router = self.router.as_mut().expect("the route worker started");
                        // The invoke that started this route kept the running job first (`keep_job`).
                        debug_assert!(router.job.as_ref().is_none_or(|old| old.ended), "a route starts only after the running job was kept");
                        let (id, unit, hash) = (job.id(), job.unit(), base.hash);
                        router.held.insert(base.unit, base.hash);
                        router.job = Some(RunningJob {
                            id, tag: id.tag(), unit, base: hash, by, reserved, seq: None, ended: false,
                        });
                        self.job_metrics = None;
                        // A place job's line counts parts from its first moment; its scope names them.
                        let (phase, scope, method) = match &job {
                            Job::Pair(job) => {
                                let mut nets = vec![job.route.net];
                                if let Some((mate, _)) = base.rules.partner(job.route.net) { nets.push(mate); }
                                (JobPhase::Nets, RouteScope::Nets(nets), Method::default())
                            }
                            Job::Tune(job) => (JobPhase::Nets, RouteScope::Nets(vec![job.tune.net]), Method::default()),
                            Job::Route(job) => (JobPhase::Nets, job.scope.clone(), job.method),
                            Job::Place(job) => (JobPhase::Seed { round: 0 }, RouteScope::Parts(job.scope.clone()), Method::default()),
                        };
                        route_request(router, RouteRequest::Base(base));
                        route_request(router, job.request());
                        self.job_state = Some(Arc::new(RouteJobState {
                            status: JobStatus {
                                job: id, unit, phase, current: None, routed: 0, failed: 0, total: 0,
                                attempt: 0, retry: None, blocked_by: None,
                            },
                            current_name: None, end: None, scope: Arc::new(scope), method, open: Arc::default(),
                        }));
                    }
                    for job in step.fills {
                        let key = (job.task.unit, job.task.zone, job.preview.is_some());
                        let inputs = (job.task.generation, Arc::downgrade(&job.rules), job.preview);
                        let tx = self.rx.sender();
                        // The job plans on the pool, then its tiles run as one pool task each; the
                        // task storing the last tile sends the fill. The token stops queued tiles.
                        let pool = cx.task_pool();
                        let tiles = CancellationToken::new();
                        let stop = tiles.clone();
                        match cx.task_pool().submit_named(Class::Visible, "elec fill", move || {
                            job.run_tiled(
                                move || stop.is_cancelled(),
                                |tile| match pool.submit_named(Class::Visible, "elec fill tile", tile) {
                                    Ok(task) => {
                                        task.detach();
                                        true
                                    }
                                    Err(error) => {
                                        log!("electronics fill tile: {error}");
                                        false
                                    }
                                },
                                move |result| {
                                    let _ = tx.send(Msg::Fill(result));
                                },
                            );
                        }) {
                            Ok(task) => {
                                if let Some((old, old_tiles)) = self.fills.insert(key, (task, tiles)) {
                                    old.cancel();
                                    old_tiles.cancel();
                                }
                                self.fill_inputs.insert(key, inputs);
                            }
                            Err(error) => log!("electronics fill: {error}"),
                        }
                    }
                    for job in step.checks {
                        let (unit, generation) = (job.task.unit, job.task.generation);
                        let tx = self.rx.sender();
                        // Off the strand: commands apply while a board's whole-board checks run.
                        match cx.task_pool().submit_named(Class::Visible, "elec checks", move || {
                            let _ = tx.send(Msg::Fill(job.run()));
                        }) {
                            Ok(task) => {
                                if let Some((_, old)) = self.checks.insert(unit, (generation, task)) {
                                    old.cancel();
                                }
                            }
                            Err(error) => log!("electronics checks: {error}"),
                        }
                    }
                    for job in step.evals {
                        let id = job.id;
                        let slot = if job.plan.is_some() {
                            &mut self.proposal_task
                        } else if job.preview.is_some() {
                            &mut self.preview_task
                        } else {
                            &mut self.eval_task
                        };
                        let tx = self.rx.sender();
                        match cx.task_pool().submit_named(Class::Visible, "elec evaluation", move || {
                            let _ = tx.send(Msg::Fill(job.run()));
                        }) {
                            Ok(task) => {
                                if let Some((_, old)) = slot.replace((id, task)) {
                                    old.cancel();
                                }
                            }
                            Err(error) => log!("electronics evaluation: {error}"),
                        }
                    }
                }
                Msg::Fill(result) => {
                    match &result {
                        TaskResult::Fill { unit, zone, generation, rules, preview, .. } => {
                            let key = (*unit, *zone, preview.is_some());
                            if self.fill_inputs.get(&key).is_some_and(|(g, r, p)| *g == *generation && r.ptr_eq(&Arc::downgrade(rules)) && p == preview) {
                                self.fills.remove(&key);
                                self.fill_inputs.remove(&key);
                            }
                        }
                        TaskResult::Checks { unit, generation, .. } => {
                            if self.checks.get(unit).is_some_and(|(g, _)| *g == *generation) {
                                self.checks.remove(unit);
                            }
                        }
                        TaskResult::Eval(done) => {
                            for slot in [&mut self.eval_task, &mut self.preview_task, &mut self.proposal_task] {
                                if slot.as_ref().is_some_and(|(id, _)| *id == done.id) {
                                    *slot = None;
                                    break;
                                }
                            }
                        }
                    }
                    self.mailbox.result(result);
                }
                Msg::Host(msg) => {
                    changed |= on(cx, &self.sender(), Drained::Host { msg, shown: self.snap.is_some() });
                }
            }
        }
        changed
    }

    /// No engine strand job is running.
    pub fn idle(&self) -> bool {
        self.job.is_none()
    }

    /// A command waits or a strand job runs: a job that can replace the bed document waits.
    pub fn busy(&self) -> bool {
        self.job.is_some() || self.waiting || !self.mailbox.is_empty()
    }

    /// Queue a command for the next kick.
    pub fn push(&mut self, cmd: Cmd) {
        if matches!(&cmd, Cmd::Commit { .. } | Cmd::Text { .. } | Cmd::Docs { .. } | Cmd::Decide { .. } | Cmd::Invoke { .. }
            | Cmd::SetFactories(_) | Cmd::Undo | Cmd::Redo) {
            self.keep_job();
        }
        self.mailbox.push(cmd);
    }

    /// The caller binds metadata to this ticket immediately before submitting its command.
    pub fn next_ticket(&self) -> Ticket { Ticket(self.tickets + 1) }

    /// Queue a tool call and retain its caller; O(tool bytes) plus expected O(1) per call.
    pub fn invoke(&mut self, tool: String, args: Vec<(String, String)>, preview: bool, by: By, reply: Sender<Outcome>) {
        self.keep_job();
        self.tickets += 1;
        let ticket = Ticket(self.tickets);
        self.replies.insert(ticket, reply);
        self.tools.insert(ticket, tool.clone());
        // A job waits for the drag: it starts on the board the drag leaves.
        let route = tool == "elec.route"
            || tool == "elec.place" && args.iter().any(|(key, mode)| key == "mode" && mode.trim_start_matches('@') == "place");
        let cmd = Cmd::Invoke { tool, args, preview, by, ticket };
        if let Some(router) = self.router.as_mut().filter(|router| route && router.gesture.as_ref().is_some_and(|g| !g.ended)) {
            log!("electronics route: waits for the drag on the board to end");
            router.deferred.push(cmd);
            return;
        }
        self.mailbox.push(cmd);
        self.kick();
    }

    /// Queue a caller's change of a document's text, made for revision `base_rev`, as one step
    /// of its own (never merged into a person's typing), and retain its caller. The engine
    /// refuses it when the document moved on since. O(edits) plus expected O(1).
    pub fn text(&mut self, doc: DocId, base_rev: u64, edits: Vec<TextEdit>, label: String, by: By, reply: Sender<Outcome>) {
        self.keep_job();
        self.tickets += 1;
        let ticket = Ticket(self.tickets);
        self.replies.insert(ticket, reply);
        self.tools.insert(ticket, label.clone());
        self.mailbox.push(Cmd::Text { doc, base_rev, edits, group: None, label, tag: None, by, ticket: Some(ticket) });
        self.kick();
    }

    /// Queue a document-set change as one undo step and route its outcome to its caller.
    pub fn docs(&mut self, set: Vec<DocSet>, label: String, by: By, reply: Sender<Outcome>) {
        self.keep_job();
        self.tickets += 1;
        let ticket = Ticket(self.tickets);
        self.replies.insert(ticket, reply);
        self.tools.insert(ticket, label.clone());
        self.mailbox.push(Cmd::Docs { set, label, by, ticket: Some(ticket) });
        self.kick();
    }

    /// Queue a local document proposal and route its verification outcome to its caller.
    /// O(label bytes) plus expected O(1) to retain the ticket until the check answers.
    pub fn propose(&mut self, slot: String, label: String, set: Vec<DocSet>, by: By, reply: Sender<Outcome>) {
        self.tickets += 1;
        let ticket = Ticket(self.tickets);
        self.replies.insert(ticket, reply);
        self.tools.insert(ticket, label.clone());
        self.push(Cmd::Propose { slot, label, set, by: crate::ProposalAuthor::Local(by), ticket });
        self.kick();
    }

    /// As [`Driver::text`], committed only once the changed documents evaluate with no new
    /// mistake (`Cmd::CheckedText`): a caller's change of a circuit, bench or part document.
    pub fn checked_text(&mut self, doc: DocId, base_rev: u64, edits: Vec<TextEdit>, label: String, by: By, reply: Sender<Outcome>) {
        self.keep_job();
        self.tickets += 1;
        let ticket = Ticket(self.tickets);
        self.replies.insert(ticket, reply);
        self.tools.insert(ticket, label.clone());
        self.mailbox.push(Cmd::CheckedText { doc, base_rev, edits, label, by, ticket });
        self.kick();
    }

    /// Once no gesture is live, queue the route invokes that waited for it, after its commit.
    fn send_deferred(&mut self) {
        let Some(router) = self.router.as_mut().filter(|router| router.gesture.as_ref().is_none_or(|g| g.ended)) else { return };
        for cmd in std::mem::take(&mut router.deferred) {
            self.mailbox.push(cmd);
        }
    }

    /// Deliver and forget only this step's replies; expected O(outcomes) per step.
    fn answer(&mut self, outcomes: Vec<(Ticket, Outcome)>) {
        for (ticket, outcome) in outcomes {
            if self.parked.is_some_and(|(pending, _)| pending == ticket) {
                self.parked = None;
                self.job_state = None;
            }
            if let Outcome::Done { notes, .. } = &outcome {
                let lines: Vec<_> = notes.iter().filter(|(key, _)| matches!(key.as_str(), "case" | "case_plan"))
                    .map(|(_, words)| words.as_str()).collect();
                if !lines.is_empty() { self.place_notice = Some(lines.join("\n")); }
            }
            self.tools.remove(&ticket);
            if let Some(reply) = self.replies.remove(&ticket) {
                let _ = reply.send(outcome);
            }
        }
    }

    /// Start a strand job for the waiting commands, unless one runs or nothing waits.
    pub fn kick(&mut self) {
        if self.job.is_some() || self.mailbox.is_empty() {
            return;
        }
        let mailbox = self.mailbox.clone();
        let tx = self.rx.sender();
        // A person waits on commands and previews; a step that only merges task results (fills,
        // checks, evaluations) is what the view shows next, not an answer to input.
        let class = if mailbox.has_commands() { Class::Interactive } else { Class::Visible };
        self.job = Some(self.strand.submit(class, TaskTicket::none(), move |state| {
            // A failed load leaves the inbox intact; only an engine may take it.
            if let Some(engine) = state {
                let step = engine.step(mailbox.take(), &mut |snap| {
                    let _ = tx.send(Msg::Snapshot(snap));
                });
                let _ = tx.send(Msg::Step(step));
            }
        }));
    }

    /// O(snapshot fields) clone, at most once per event while a job's state changes or a strand snapshot arrives during a job.
    fn overlay_job(&mut self) -> bool {
        let Some(snap) = &self.snap else { return false };
        let same = match (&snap.job, &self.job_state) {
            (None, None) => true,
            (Some(shown), Some(state)) => Arc::ptr_eq(shown, state),
            _ => false,
        };
        if same {
            return false;
        }
        let marked = Arc::new(DesignSnapshot { job: self.job_state.clone(), ..(**snap).clone() });
        if let Some(old) = self.snap.replace(marked) {
            self.mailbox.retire(old);
        }
        true
    }

    /// Mark the shown snapshot stale while the engine's oldest command waits over 16 ms.
    pub fn mark_backlog(&mut self) -> bool {
        let mut changed = false;
        let backlog = self.mailbox.waited(Instant::now()).is_some_and(|wait| wait > Duration::from_millis(16));
        if backlog && !self.backlog {
            log!("electronics engine catching up");
        }
        self.backlog = backlog;
        if let Some(snap) = &self.snap {
            let stale = if backlog { snap.stale.with(StageFlags::ENGINE) } else { snap.stale.without(StageFlags::ENGINE) };
            if stale != snap.stale {
                let marked = Arc::new(DesignSnapshot { stale, ..(**snap).clone() });
                if let Some(old) = self.snap.replace(marked) {
                    self.mailbox.retire(old);
                }
                changed = true;
            }
        }
        changed
    }

    /// Share the shown snapshot with the views and retire the global's old reference on the strand.
    pub fn show(&mut self, cx: &mut Cx) -> Option<Arc<DesignSnapshot>> {
        let snap = self.snap.clone()?;
        if cx.has_global::<ElecLive>() {
            // The strand may already have taken the retired host snapshot. Hand back
            // the global's reference too, so its last board reference drops there.
            let old = std::mem::replace(&mut cx.get_global::<ElecLive>().snap, snap.clone());
            self.mailbox.retire(old);
        } else {
            cx.set_global(ElecLive::new(snap.clone()));
        }
        Some(snap)
    }

    /// Once per event, before widgets, for hosts with no work between draining and kicking.
    /// Returns the newly shown snapshot when it changed.
    pub fn event(&mut self, cx: &mut Cx, on: &mut dyn FnMut(&mut Cx, &HostTx<X>, Drained<'_, X>) -> bool) -> Option<Arc<DesignSnapshot>> {
        let mut changed = self.drain(cx, on);
        self.kick();
        changed |= self.mark_backlog();
        if changed { self.show(cx) } else { None }
    }

    fn end_wait(&mut self, id: JobId, discard: bool) -> bool {
        if !self.parked.is_some_and(|(_, waiting)| waiting == id) { return false }
        self.parked = None;
        self.cancelled_wait = Some(id);
        let seq = self.next_seq();
        let tag = id.tag();
        self.mailbox.push(if discard { Cmd::DiscardJob { seq, tag } } else {
            Cmd::RefusePreview { seq, tag, reason: "placement stopped while waiting for the case; the brought parts were kept".into() }
        });
        if discard { self.job_state = None; }
        else if let Some(state) = &mut self.job_state { Arc::make_mut(state).end = Some(JobEnd::Stopped); }
        true
    }

    pub fn take_place_notice(&mut self) -> Option<String> { self.place_notice.take() }

    fn stop_job(&mut self, id: JobId) {
        if self.end_wait(id, false) { return }
        let Some(router) = &mut self.router else { return };
        let Some(job) = router.job.as_mut().filter(|job| job.id == id && !job.ended) else { return };
        let (seq, reserved, tag) = (job.seq, job.reserved, job.tag);
        job.ended = true;
        route_request(router, RouteRequest::Cancel(id));
        self.mailbox.push(match seq {
            Some(seq) => Cmd::CommitPreview { seq, tag },
            None => Cmd::RefusePreview { seq: reserved, tag, reason: "stopped before any net was routed".into() },
        });
        let state = self.job_state.as_ref().expect("a running job has status");
        self.job_state = Some(Arc::new(RouteJobState { end: Some(JobEnd::Stopped), ..(**state).clone() }));
    }

    /// Before an edit while a route job runs: stop it and keep what it has routed as one undo step,
    /// so the edit lands on top of the route (the mailbox orders the keep's commit before the edit).
    pub fn keep_job(&mut self) {
        if let Some((_, id)) = self.parked { self.stop_job(id); }
        if let Some(id) = self.router.as_ref().and_then(|router| router.job.as_ref())
            .filter(|job| !job.ended).map(|job| job.id) {
            self.stop_job(id);
        }
    }

    /// A view's action: a mailbox push, a router slot write/request, or UI-only state returned to the host.
    pub fn act(&mut self, action: ViewAction) -> Option<ViewAction> {
        let mut host_action = None;
        match action {
            ViewAction::ProposalDecision { id, shown, accept } => {
                self.tickets += 1;
                self.push(Cmd::Decide { id, shown, accept, ticket: Some(Ticket(self.tickets)) });
            }
            ViewAction::CheckProposal(id) => self.push(Cmd::CheckProposal { id, ticket: None }),
            ViewAction::Planned { call, action } => {
                let edit = match *action {
                    ViewAction::Ops { ops, place, label } => {
                        self.keep_job();
                        let circuits = self.snap.as_ref().map_or_else(Vec::new, |s| circuits_named(&s.design, &ops));
                        PlannerEdit::Direct { edit: fab_elec_design::Resolved { ops, place, label }, circuits }
                    }
                    ViewAction::Land(tag) => {
                        let seq = self.take_landing(tag).map(|(seq, _)| seq);
                        PlannerEdit::Release { seq, tag }
                    }
                    other => return self.act(other),
                };
                self.tickets += 1;
                self.mailbox.push(Cmd::Planner { call, edit, ticket: Ticket(self.tickets) });
            }
            ViewAction::PlanRefused(_) => return None,
            ViewAction::Land(tag) => {
                if self.live_preview.is_some_and(|(_, actual)| actual == tag) { return self.act(ViewAction::CommitPreview); }
                return None
            }
            ViewAction::Gesture(sample) => {
                let Some(router) = self.router() else { return None };
                let new = !router.gesture.as_ref().is_some_and(|gesture| gesture.id == sample.id && !gesture.ended);
                if new {
                    self.keep_job();
                    let seq = self.next_seq();
                    let tag = sample.id.tag();
                    self.router.as_mut().expect("the router was made for this sample").gesture = Some(LiveGesture {
                        circuit: match &sample.gesture.kind {
                            fab_elec_route::GestureKind::Tune { circuit, .. } => Some((sample.gesture.unit, Arc::downgrade(circuit))),
                            _ => None,
                        },
                        id: sample.id, tag, seq, state: GestureState::Waiting, ended: false,
                    });
                    self.live_preview = Some((seq, tag));
                    self.ended_preview = None;
                }
                let router = self.router.as_mut().expect("the router was made for this sample");
                let unit = sample.gesture.unit;
                if router.held.contains_key(&unit) && router.retry.is_empty() {
                    router.handle.sample(sample);
                } else {
                    router.waiting = Some(sample);
                    if !router.held.contains_key(&unit) && router.asked.insert(unit) {
                        self.mailbox.push(Cmd::RouteBase { unit });
                    }
                }
            }
            ViewAction::EndGesture(id) => {
                if let Some(router) = &mut self.router {
                    route_request(router, RouteRequest::End(id));
                    if let Some(gesture) = router.gesture.as_mut().filter(|gesture| gesture.id == id) {
                        gesture.ended = true;
                        router.clear_shown = true;
                    }
                    if router.waiting.as_ref().is_some_and(|sample| sample.id == id) {
                        router.waiting = None;
                    }
                }
            }
            // The app submits these presses through its catalog and binds the job's ticket there.
            ViewAction::Route { .. } | ViewAction::Place { .. } => return None,
            ViewAction::StopRoute(id) => {
                self.stop_job(id);
            }
            ViewAction::DiscardRoute(id) => {
                if self.end_wait(id, true) { return None }
                let Some(router) = &mut self.router else { return None };
                let Some(job) = router.job.as_ref().filter(|job| job.id == id && !job.ended) else { return None };
                let (seq, tag) = (job.seq.unwrap_or(job.reserved), job.tag);
                route_request(router, RouteRequest::Cancel(id));
                self.mailbox.push(Cmd::DiscardJob { seq, tag });
                router.job = None;
                self.job_state = None;
            }
            // A view plans on the snapshot shown now: its circuits stamp the ops' positional ids.
            ViewAction::Ops { ops, place, label } => {
                self.keep_job();
                let circuits = self.snap.as_ref().map_or_else(Vec::new, |snap| circuits_named(&snap.design, &ops));
                self.mailbox.push(Cmd::Commit { ops, place, label, circuits, expect: None, by: By::Person, ticket: None });
            }
            // Typed text, as every text command: a running route job keeps what it routed first.
            ViewAction::Text { doc, base_rev, edits, group, label, tag } => {
                self.keep_job();
                self.mailbox.push(Cmd::Text { doc, base_rev, edits, group, label, tag: Some(tag), by: By::Person, ticket: None });
            }
            ViewAction::Preview { tag, ops, place, label, notes } => {
                self.keep_job();
                let seq = self.next_seq();
                let circuits = self.snap.as_ref().map_or_else(Vec::new, |snap| circuits_named(&snap.design, &ops));
                let values_only = place.is_empty() && !ops.is_empty()
                    && ops.iter().all(|op| matches!(op, DesignOp::Circuit { op: CircuitOp::SetValue { .. }, .. }));
                self.value_preview = values_only.then_some(tag);
                self.mailbox.preview(Sample { seq, tag, ops, place, label, by: By::Person, proposal: None, notes, circuits, expect: None });
                self.live_preview = Some((seq, tag));
                self.ended_preview = None;
            }
            ViewAction::CommitPreview => {
                if let Some((seq, tag)) = self.live_preview.take() {
                    let gesture = self.router.as_mut().and_then(|router| {
                        let gesture = router.gesture.as_mut().filter(|gesture| gesture.seq == seq && gesture.tag == tag)?;
                        gesture.ended = true;
                        router.clear_shown = true;
                        Some(gesture)
                    });
                    let cmd = match gesture.map(|gesture| &gesture.state) {
                        Some(GestureState::Waiting) => Cmd::RefusePreview {
                            seq, tag, reason: "the router had not answered this drag: nothing was applied".into(),
                        },
                        Some(GestureState::Refused(text)) => Cmd::RefusePreview { seq, tag, reason: text.clone() },
                        Some(GestureState::Answered) | None => Cmd::CommitPreview { seq, tag },
                    };
                    self.mailbox.push(cmd);
                } else if let Some((seq, tag)) = self.ended_preview.take() {
                    self.mailbox.push(Cmd::RefusePreview {
                        seq, tag, reason: "the bed's documents loaded again during this edit: nothing was applied".into(),
                    });
                }
            }
            ViewAction::CancelPreview => {
                self.ended_preview = None;
                if let Some((seq, tag)) = self.live_preview.take() {
                    if let Some(router) = &mut self.router {
                        if let Some(gesture) = router.gesture.as_mut().filter(|gesture| gesture.seq == seq && gesture.tag == tag) {
                            gesture.ended = true;
                            router.clear_shown = true;
                        }
                    }
                    self.mailbox.push(Cmd::CancelPreview { seq });
                }
            }
            action @ (ViewAction::Focus(_) | ViewAction::Unfocus | ViewAction::Hover(_) | ViewAction::Cursor(_)) => {
                host_action = Some(action);
            }
        }
        self.send_deferred();
        self.kick();
        host_action
    }

    fn take_landing(&mut self, tag: u64) -> Option<(u64, u64)> {
        self.live_preview.take_if(|(_, actual)| *actual == tag)
            .or_else(|| self.ended_preview.take_if(|(_, actual)| *actual == tag))
    }

    /// Refuse only the sample released by this pane.
    pub fn refuse_landing(&mut self, tag: u64, reason: &str) {
        if let Some((seq, _)) = self.take_landing(tag) {
            self.mailbox.push(Cmd::RefusePreview { seq, tag, reason: reason.into() });
            self.kick();
        }
    }

    /// Refuse the view's live sample instead of committing it (the host takes no edit now): the
    /// engine ends it with one refusal naming its tag and `reason`.
    pub fn refuse_preview(&mut self, reason: &str) {
        if let Some((seq, tag)) = self.live_preview.take().or_else(|| self.ended_preview.take()) {
            self.mailbox.push(Cmd::RefusePreview { seq, tag, reason: reason.into() });
            self.kick();
        }
    }

    /// Undo the newest electronics step (the host decides that it is the project's newest).
    pub fn undo(&mut self) {
        self.keep_job();
        self.mailbox.push(Cmd::Undo);
        self.kick();
    }

    /// Redo the newest undone electronics step.
    pub fn redo(&mut self) {
        self.keep_job();
        self.mailbox.push(Cmd::Redo);
        self.kick();
    }

    /// The newest undoable electronics step (for the shared undo order).
    pub fn undo_step(&self) -> Option<u64> {
        self.undo
    }

    /// The newest redoable electronics step (for the shared undo order).
    pub fn redo_step(&self) -> Option<u64> {
        self.redo
    }

    /// True from a view's preview action until the engine's snapshot shows no preview after its
    /// commit or cancel: an edit is moving what the views draw.
    /// The route job's last proposal's metrics, once its last proposal came (None while it runs,
    /// after a stop, or before any job).
    pub fn job_metrics(&self) -> Option<&RouteMetrics> {
        self.job_metrics.as_ref()
    }

    pub fn previewing(&self) -> bool {
        self.live_preview.is_some() || self.snap.as_ref().is_some_and(|snap| snap.preview.is_some())
    }

    /// The design the live run simulates: the shown preview while it is a view's value-only edit
    /// (a value dragged through its series is heard while it moves, before its release), else the
    /// committed design. A preview that changes anything else (a move, a router proposal, a part
    /// added) never reaches the run: it would start it over. O(1).
    pub fn sim_design(&self) -> Option<&Arc<Design>> {
        let snap = self.snap.as_ref()?;
        match (&snap.preview, self.value_preview) {
            (Some(preview), Some(tag)) if snap.preview_tag == Some(tag) => Some(preview),
            _ => Some(&snap.design),
        }
    }

    /// The currently shown snapshot.
    pub fn snap(&self) -> Option<&Arc<DesignSnapshot>> {
        self.snap.as_ref()
    }

    /// The files of the last step that changed them, once.
    pub fn take_docs(&mut self) -> Option<Written> {
        self.docs_out.take()
    }
}

/// The pane's Route call, naming the selected nets from the shown design. An empty selection
/// routes every unrouted net. A selection whose board is gone cannot be submitted.
pub fn route_action(design: Option<&Design>, unit: BoardId, nets: &[NetId], method: Method, reroute: bool) -> Option<(&'static str, Vec<(String, String)>)> {
    let mut args = vec![("unit".to_owned(), unit.0.to_string())];
    if !nets.is_empty() {
        let board = design?.unit(unit)?.board.as_deref()?;
        let names: Vec<&str> = nets.iter().filter_map(|net| board.nets.get(net.ix())).map(|net| net.name.as_str()).collect();
        args.push(("nets".to_owned(), names.join(",")));
    }
    match method {
        Method::Interactive => {}
        Method::Organic => args.push(("method".to_owned(), "organic".to_owned())),
    }
    if reroute {
        args.push(("reroute".to_owned(), "true".to_owned()));
    }
    Some((crate::edit_actions::ROUTE.name, args))
}

/// The pane's Place call, naming the board and the selected component paths from the shown design.
pub fn place_action(design: Option<&Design>, unit: BoardId, parts: &[CompId], all: bool, free_panel: bool) -> Option<(&'static str, Vec<(String, String)>)> {
    let unit = design?.unit(unit)?;
    let mut args = vec![("unit".to_owned(), unit.name.clone()), ("mode".to_owned(), "@place".to_owned())];
    if !parts.is_empty() {
        let names: Vec<_> = parts.iter().filter_map(|id| unit.circuit.comps.get(id.ix())).map(|c| c.path.as_str()).collect();
        args.push(("parts".to_owned(), names.join(",")));
    }
    if all {
        args.push(("all".to_owned(), "true".to_owned()));
    }
    if free_panel {
        args.push(("panel".to_owned(), "@free".to_owned()));
    }
    Some(("elec.place", args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_board::{Anchor, BoardOp, Factories, Origin, Track};
    use fab_elec_core::{mm, Canon, ContentHash, LayerId, NetId, PadIx, Path, P};
    use fab_elec_design::{Design, DesignOp, Resolved};
    use fab_elec_fixtures::ledctl;
    use fab_elec_route::{scope_nets, Gesture, GestureKind, ProposalId, RouteInbox, RouteMetrics, RouteProposal, RouteScope, SampleCause, SampleRefused, Tier, Work};
    use std::sync::mpsc::{self, Receiver};
    use std::thread::JoinHandle;

    // Waits below are on worker events; this bound only turns a hang into a failure.
    const HANG: Duration = Duration::from_secs(60);

    fn finish(driver: &mut Driver<()>, cx: &mut Cx) {
        // Only tests join, on a separate thread; a route-only event may schedule no strand work.
        if let Some(job) = driver.job.take() {
            std::thread::spawn(move || job.join()).join().expect("the test join worker exits")
                .expect("the strand job completes without error");
        }
        driver.drain_messages(cx, &mut |_, _, _| false);
    }

    fn take_work(inbox: &RouteInbox) -> Vec<Work> {
        let mut work = Vec::new();
        while inbox.preempted() {
            work.push(inbox.next());
        }
        work
    }

    struct Held {
        driver: Driver<()>,
        wakes: Receiver<()>,
        held: Receiver<u32>,
        release: Sender<()>,
        thread: JoinHandle<()>,
        id: JobId,
        receiver: Receiver<Outcome>,
        before: ContentHash,
    }

    fn board_hash(design: &Design) -> ContentHash {
        design.unit(BoardId(0)).expect("the route fixture keeps unit 0")
            .board.as_ref().expect("the route fixture keeps unit 0's board").content_hash()
    }

    /// Runs each task of a held route job (the job, its partials' checks) on a thread of its own.
    /// The hold blocks the job's thread; on the shared pool, tests holding jobs side by side
    /// would take its workers and starve the checks that deliver the held partial.
    struct OwnThreads;

    impl Spawn for OwnThreads {
        fn spawn(&self, task: Box<dyn FnOnce() + Send>) {
            std::thread::Builder::new().name("held route task".into()).spawn(task).expect("a test thread starts");
        }
    }

    fn held_route(cx: &mut Cx) -> Held {
        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        let mut driver = Driver::<()>::new(cx, steps.clone(), logged.clone());
        driver.load(cx, None, move |_| {
            Engine::new(ledctl(), Arc::new(Factories::new()), steps, logged, "route".into())
                .with_resolver("elec.route", crate::route_plan)
        });
        finish(&mut driver, cx);
        let before = board_hash(&driver.snap().expect("the route fixture is loaded").design);
        let (woke, wakes) = mpsc::channel();
        let (handle, inbox) = route_channel(move || {
            let _ = woke.send(());
        });
        let (held_at, held) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = std::sync::Mutex::new(Some(released));
        let worker = RouteWorker::new(inbox, Arc::new(OwnThreads)).with_offered(move |_, step| {
            let first = released.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(released) = first {
                let _ = held_at.send(step);
                let _ = released.recv();
            }
        });
        let thread = std::thread::spawn(move || worker.run());
        driver.router = Some(Router::new(handle));

        let unit = BoardId(0);
        let nets = {
            let snap = driver.snap().expect("the route fixture is loaded");
            let board = snap.design.unit(unit).and_then(|found| found.board.as_ref()).expect("fixture setup: ledctl unit 0 has a board");
            let conn = snap.conn.iter().find(|conn| conn.unit == Some(unit) && conn.of == fab_elec_view::StateOf::Committed)
                .expect("fixture setup: ledctl has committed connectivity");
            scope_nets(board, &conn.value, &RouteScope::All).into_iter().take(2)
                .map(|net| board.nets[net.0 as usize].name.clone()).collect::<Vec<_>>()
        };
        assert_eq!(nets.len(), 2, "fixture setup: ledctl has two open nets");
        let (reply, receiver) = mpsc::channel();
        driver.invoke("elec.route".into(), vec![("nets".into(), nets.join(","))], false, By::Ai { turn: 1, call: 1 }, reply);
        finish(&mut driver, cx);
        let id = driver.router.as_ref().and_then(|router| router.job.as_ref()).map(|job| job.id)
            .expect("the route starts at once on ledctl's exact fills");
        Held { driver, wakes, held, release, thread, id, receiver, before }
    }

    // Show worker publications until `done` holds, retaining every shown routed-net count.
    fn show(
        driver: &mut Driver<()>,
        cx: &mut Cx,
        wakes: &Receiver<()>,
        mut done: impl FnMut(&Driver<()>, &DesignSnapshot) -> bool,
    ) -> Vec<u32> {
        let mut routed = Vec::new();
        loop {
            driver.drain(cx, &mut |_, _, _| false);
            driver.kick();
            finish(driver, cx);
            driver.drain(cx, &mut |_, _, _| false);
            let snap = driver.snap().expect("the route job keeps a shown snapshot");
            if let Some(state) = &snap.job {
                routed.push(state.status.routed);
            }
            if done(driver, snap) {
                return routed;
            }
            wakes.recv_timeout(HANG).expect("the worker publishes again");
        }
    }

    // One driver round for commands that need no worker answer (the held job only listens), so a
    // missing keep fails on the assertions that follow instead of waiting for a publication.
    fn settle(driver: &mut Driver<()>, cx: &mut Cx) {
        driver.drain(cx, &mut |_, _, _| false);
        driver.kick();
        finish(driver, cx);
        driver.drain(cx, &mut |_, _, _| false);
    }

    const ATTACHED: &str = r#"track( "P", // the hand-drawn continuation
  "F.Cu",  0.20mm, [[7mm 19mm], [7mm 25mm], "b:1"] )"#;

    fn tune_driver() -> (Driver<()>, Cx, Receiver<()>, JoinHandle<()>, DocId, String) {
        let pcb = format!(
            r#"board("tune")
outline([[[0mm 0mm] [40mm 0mm] [40mm 30mm] [0mm 30mm] [0mm 0mm]]])
place("a", [10mm 5mm])
place("b", [10mm 25mm])
track("P", "F.Cu", 0.2mm, ["a:1", [7mm 5mm], [7mm 19mm]])
{ATTACHED}
track("N", "F.Cu", 0.2mm, ["a:2", [13mm 5mm], [13mm 10mm], [16mm 13mm], [16mm 17mm], [13mm 20mm], [13mm 25mm], "b:2"])
"#
        );
        let mut sources = fab_elec_lang::Sources::new();
        sources
            .add(
                "tune.ckt.splash",
                r#"use mod.elec.*
let e = mod.elec
let p = e.net({name: "P"})
let n = e.net({name: "N"})
let x = e.net({name: "X"})
let q = e.net({name: "Q"})
let loose = e.net({name: "LOOSE"})
let a = e.r(1kohm, p, n)
let b = e.r(1kohm, p, n)
let c = e.r(1kohm, q, x)
let d = e.r(1kohm, q, x)
let f = e.r(1kohm, q, x)
e.match_length([p,n], {tol: 0.01mm})
e.match_length([n,x], {tol: 0.02mm})
"#,
            )
            .unwrap();
        let doc = sources.add("tune.pcb.splash", pcb.clone()).unwrap();
        let evaluated = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap();
        let (design, docs) = crate::Docs::from_evaluated(sources, evaluated);
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let (steps, logged) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let mut driver = Driver::<()>::new(&mut cx, steps.clone(), logged.clone());
        driver.load(&mut cx, None, move |_| {
            Engine::new(design, Arc::new(Factories::new()), steps, logged, "tune".into())
                .with_docs(docs)
                .with_app_tools()
        });
        finish(&mut driver, &mut cx);
        let (woke, wakes) = mpsc::channel();
        let (handle, inbox) = route_channel(move || {
            let _ = woke.send(());
        });
        let thread = std::thread::spawn(move || RouteWorker::new(inbox, Arc::new(OwnThreads)).run());
        driver.router = Some(Router::new(handle));
        (driver, cx, wakes, thread, doc, pcb)
    }

    #[test]
    fn tune_gesture_releases_one_step_and_refuses_a_third_unplaced_pin() {
        let (mut driver, mut cx, wakes, thread, _, _) = tune_driver();
        let before = driver.snap().unwrap().clone();
        let unit = &before.design.units[0];
        let board = unit.board.as_ref().unwrap();
        let track = &board.tracks[0];
        let point = track.path.segs[0].end();
        let start = Anchor::Track { id: track.id, at: fab_elec_core::OnTrack { seg: 1, frac: 0 } };
        let press = |driver: &mut Driver<()>, cx: &mut Cx, id: GestureId| {
            let circuit = driver.snap().unwrap().design.units[0].circuit.clone();
            let kind = GestureKind::Tune { track: track.id, from: 1, to: 2, circuit };
            let mut gesture = Gesture::new(kind, unit.id, board.generation, start, point, track.layer);
            gesture.net = Some(track.net);
            gesture.cursor = P::mm(6., 19.);
            driver.act(ViewAction::Gesture(GestureSample { id, seq: 1, gesture }));
            show(driver, cx, &wakes, |_, snap| snap.preview_tag == Some(id.tag()));
            assert_ne!(driver.snap().unwrap().live().units[0].board.as_ref().unwrap().tracks, board.tracks);
        };
        let id = GestureId(41);
        press(&mut driver, &mut cx, id);
        driver.act(ViewAction::CommitPreview);
        settle(&mut driver, &mut cx);
        assert_ne!(driver.snap().unwrap().design.units[0].board.as_ref().unwrap().tracks, board.tracks);
        driver.undo();
        settle(&mut driver, &mut cx);
        assert_eq!(driver.snap().unwrap().design.units[0].board.as_ref().unwrap().tracks, board.tracks);
        assert!(driver.undo_step().is_none());

        let id = GestureId(42);
        press(&mut driver, &mut cx, id);
        let text = before.docs.as_ref().unwrap().texts.iter().find(|d| d.name == "tune.ckt.splash").unwrap();
        let end = text.text.len() as u32;
        let edit = TextEdit { doc: text.doc, span: end..end, text: "let spare = e.r(1kohm, p, x)\n".into() };

        // Queue both strand steps before dispatching their pool jobs. Keep the pool replies
        // on the live channel until both handles are installed, regardless of worker speed.
        let live = std::mem::take(&mut driver.rx);
        let (reply, proposed) = mpsc::channel();
        let ticket = driver.next_ticket();
        driver.propose("brief".into(), "Draft brief".into(),
            vec![DocSet::Add { name: "brief.req.splash".into(), text: "// draft".into() }], By::Person, reply);
        let job = driver.job.take().expect("the proposal schedules a strand step");
        std::thread::spawn(move || job.join()).join().unwrap().expect("the proposal step queues its verification");
        let (reply, committed) = mpsc::channel();
        driver.text(text.doc, text.rev, vec![edit], "Add spare pin".into(), By::Person, reply);
        let steps = std::mem::replace(&mut driver.rx, live);
        let replies = std::mem::replace(&mut driver.rx.receiver, steps.receiver);
        finish(&mut driver, &mut cx);
        driver.rx.receiver = replies;
        assert!(driver.proposal_task.is_some() && driver.eval_task.is_some(),
            "dispatching the committed evaluation preserves the in-flight proposal task");
        let deadline = Instant::now() + HANG;
        loop {
            driver.event(&mut cx, &mut |_, _, _| false);
            let snap = driver.snap().unwrap();
            if snap.design.units[0].circuit.comp("spare").is_some()
                && snap.proposals.iter().any(|p| p.state == crate::ProposalState::Ready)
                && !driver.replies.contains_key(&ticket) { break; }
            assert!(Instant::now() < deadline, "the committed part evaluates and the proposal reaches Ready and answers its caller");
            std::thread::yield_now();
        }
        let snap = driver.snap().unwrap();
        assert!(matches!(committed.try_recv(), Ok(Outcome::Done { .. })), "the circuit edit answers its commit ticket");
        assert!(matches!(proposed.try_recv(), Ok(Outcome::Proposed { id, shown, .. })
            if snap.proposals.iter().any(|p| p.id == id && p.shown == shown && p.state == crate::ProposalState::Ready)),
            "the proposal ticket identifies the ready proposal after the committed evaluation");
        let pins = snap.design.units[0].circuit.comps.iter().flat_map(|c| &c.conns).filter(|(_, n)| *n == track.net).count();
        assert_eq!(pins, 3);
        assert_eq!(snap.design.units[0].board.as_ref().unwrap().content_hash(), board.content_hash());
        let step = driver.undo_step();
        assert!(step.is_some());
        driver.act(ViewAction::CommitPreview);
        settle(&mut driver, &mut cx);
        let snap = driver.show(&mut cx).unwrap();
        assert_eq!(snap.design.units[0].board.as_ref().unwrap().tracks, board.tracks);
        assert_eq!(driver.undo_step(), step);
        let reason = snap.last_refusal.as_ref().unwrap();
        assert_eq!(reason.sample, Some(id.tag()));
        assert!(reason.reason.contains("circuit changed after this edit was planned"), "{}", reason.reason);
        drop(driver);
        thread.join().unwrap();
    }

    #[test]
    fn catalog_tune_commits_the_meander_and_junction_and_undo_restores_both() {
        use fab_elec_circuit::CheckKind;
        let (mut driver, mut cx, wakes, thread, doc, pcb) = tune_driver();
        let before = driver.snap().unwrap().clone();
        let findings =
            |snap: &DesignSnapshot| snap.violations.value.iter().filter(|v| v.kind == CheckKind::Length).count();
        assert_eq!(findings(&before), 1);
        let invoke = |driver: &mut Driver<()>, cx: &mut Cx, net: &str, fields: &[(&str, &str)]| {
            let (reply, recv) = mpsc::channel();
            let mut args = vec![("net".into(), net.to_owned())];
            args.extend(fields.iter().map(|&(k, v)| (k.into(), v.into())));
            driver.invoke("elec.net.tune".into(), args, false, By::Ai { turn: 1, call: 1 }, reply);
            finish(driver, cx);
            let mut answer = None;
            show(driver, cx, &wakes, |_, _| {
                answer = recv.try_recv().ok();
                answer.is_some()
            });
            answer.unwrap()
        };
        for (net, fields, reason) in [
            ("LOOSE", &[][..], "no length match"),
            ("N", &[][..], "P, N / N, X"),
            ("Q", &[("target", "60")][..], "branched"),
            ("P", &[("target", "1mm")][..], "already at or above"),
        ] {
            let Outcome::Refused(r) = invoke(&mut driver, &mut cx, net, fields) else {
                panic!("{net} should be refused")
            };
            assert!(r.text.contains(reason), "{}", r.text);
            assert!(driver.undo_step().is_none());
        }
        let Outcome::Done { notes, .. } =
            invoke(&mut driver, &mut cx, "P", &[("at", "[7mm 12mm]"), ("amplitude", "1")])
        else {
            panic!("expected a tune commit")
        };
        let snap = driver.snap().unwrap();
        let board = snap.design.units[0].board.as_ref().unwrap();
        let original = before.design.units[0].board.as_ref().unwrap();
        let p = board.net("P").unwrap();
        let n = board.net("N").unwrap();
        let read = fab_elec_board::length::net_lengths(board, &snap.of(&snap.conn, board.unit).unwrap().value, &[p, n]);
        assert!((read[&p].nm - read[&n].nm).abs() <= i64::from(mm(0.01)));
        assert_eq!(findings(snap), 0);
        assert!(notes.contains(&("reached".into(), format!("{:.6}", read[&p].nm as f64 / 1e6))));
        assert_ne!(board.tracks[1].ends[0], original.tracks[1].ends[0]);
        assert_eq!(board.anchor_pos(board.tracks[1].ends[0]), Some(P::mm(7., 19.)));
        let text = |snap: &DesignSnapshot| {
            snap.docs.as_ref().unwrap().texts.iter().find(|d| d.doc == doc).unwrap().text.clone()
        };
        let saved = text(snap);
        assert_eq!(saved.matches("track(").count(), 3);
        assert!(saved.contains(ATTACHED));
        let reloaded = fab_elec_lang::board_doc::lower(doc, &saved, &snap.design.units[0].circuit, None).unwrap();
        assert_eq!(reloaded.board.tracks, board.tracks);
        driver.undo();
        settle(&mut driver, &mut cx);
        let undone = driver.snap().unwrap();
        assert_eq!(undone.design.units[0].board.as_ref().unwrap().tracks, original.tracks);
        assert_eq!(text(undone).as_ref(), pcb);
        assert_eq!(findings(undone), 1);
        assert!(driver.undo_step().is_none());
        drop(driver);
        thread.join().unwrap();
    }

    #[test]
    fn catalog_pair_route_commits_both_pad_anchored_legs_and_undo_restores_the_source() {
        use fab_elec_circuit::CheckKind;
        let mut sources = fab_elec_lang::Sources::new();
        sources.add("pair.ckt.splash", r#"use mod.elec.*
let e = mod.elec
let p = e.net({name: "P"})
let n = e.net({name: "N"})
let a = e.r(1kohm, p, n)
let b = e.r(1kohm, p, n)
e.pair(p, n, {gap: 0.6mm})
"#).unwrap();
        let pcb = r#"board("pair")
outline([[[0mm 0mm] [40mm 0mm] [40mm 30mm] [0mm 30mm] [0mm 0mm]]])
place("a", [10mm 5mm])
place("b", [25mm 20mm], {rot: -90deg})
"#;
        let doc = sources.add("pair.pcb.splash", pcb).unwrap();
        let evaluated = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap();
        let (design, docs) = crate::Docs::from_evaluated(sources, evaluated);
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let (steps, logged) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let mut driver = Driver::<()>::new(&mut cx, steps.clone(), logged.clone());
        driver.load(&mut cx, None, move |_| {
            Engine::new(design, Arc::new(Factories::new()), steps, logged, "pair".into())
                .with_docs(docs).with_app_tools()
        });
        finish(&mut driver, &mut cx);
        let (woke, wakes) = mpsc::channel();
        let (handle, inbox) = route_channel(move || { let _ = woke.send(()); });
        let thread = std::thread::spawn(move || RouteWorker::new(inbox, Arc::new(OwnThreads)).run());
        driver.router = Some(Router::new(handle));
        let route = |driver: &mut Driver<()>, cx: &mut Cx, to: &str, waypoints: &str| {
            let (reply, recv) = mpsc::channel();
            let args = [("pair", "N"), ("from", "a:1"), ("to", to), ("waypoints", waypoints)]
                .map(|(k,v)| (k.to_owned(), v.to_owned())).to_vec();
            driver.invoke("elec.pair.route".into(), args, false, By::Ai { turn: 1, call: 1 }, reply);
            finish(driver, cx);
            let mut answer = None;
            show(driver, cx, &wakes, |_, _| { answer = recv.try_recv().ok(); answer.is_some() });
            match answer {
                Some(Outcome::Done { notes, .. }) => notes,
                Some(Outcome::Refused(r)) => panic!("pair route refused: {}", r.text),
                _ => panic!("expected the pair to commit"),
            }
        };
        let notes = route(&mut driver, &mut cx, "b:2", "[[10 20]]");
        let snap = driver.snap().unwrap();
        let board = snap.design.units[0].board.as_ref().unwrap();
        assert_eq!(board.tracks.len(), 2);
        let pair = board.pairs[0];
        let spacing = pair.gap.unwrap() + (board.tracks[0].w + board.tracks[1].w) / 2;
        assert_eq!((board.tracks[0].net, board.tracks[1].net), (pair.p, pair.n));
        assert!(board.tracks.iter().all(|t| t.ends.iter().all(|a| matches!(a, fab_elec_board::Anchor::Pad { .. }))));
        let lengths = fab_elec_board::length::net_lengths(board, &snap.of(&snap.conn, board.unit).unwrap().value, &[pair.p,pair.n]);
        let skew = (lengths[&pair.p].nm - lengths[&pair.n].nm).abs();
        assert!(skew > 0);
        assert!(notes.contains(&("skew".into(), format!("{:.6}", skew as f64 / 1e6))));
        assert!(snap.violations.value.iter().all(|v| !matches!(v.kind, CheckKind::PairGap | CheckKind::Clearance | CheckKind::Short)));
        let text = |snap: &DesignSnapshot| snap.docs.as_ref().unwrap().texts.iter().find(|d| d.doc == doc).unwrap().text.clone();
        let written = text(snap);
        assert_eq!(written.matches("track(").count(), 2);
        let reloaded = fab_elec_lang::board_doc::lower(doc, &written, &snap.design.units[0].circuit, None).unwrap();
        assert_eq!(reloaded.board.tracks, board.tracks);
        driver.undo();
        settle(&mut driver, &mut cx);
        let undone = driver.snap().unwrap();
        assert!(undone.design.units[0].board.as_ref().unwrap().tracks.is_empty());
        assert_eq!(text(undone).as_ref(), pcb);
        assert!(driver.undo_step().is_none());

        // The same room, one lattice step apart. Only a verified worker offer
        // gets a report through the catalog and commits its two tracks.
        for short_by in [1, 0] {
            let x = 10. + f64::from(2 * spacing - short_by) / 1e6;
            let notes = route(&mut driver, &mut cx, &format!("[{x:.6} 13]"),
                &format!("[[10 15] [20 15] [20 7] [{x:.6} 7]]"));
            let board = driver.snap().unwrap().design.units[0].board.as_ref().unwrap();
            assert_eq!(board.tracks.len(), 2);
            let field = |key: &str| notes.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
            if short_by == 1 {
                assert_eq!(field("reason"), Some("\"the pair's own earlier run\""));
                assert!(field("blocked_leg").is_some() && field("need").is_some());
                assert!(board.tracks.iter().all(|t| t.path.end().y < fab_elec_core::mm(13.)));
            } else {
                assert!(field("blocked_leg").is_none() && field("reason").is_none());
                assert!(board.tracks.iter().all(|t| t.path.end().y == fab_elec_core::mm(13.)));
            }
            driver.undo();
            settle(&mut driver, &mut cx);
            assert_eq!(text(driver.snap().unwrap()).as_ref(), pcb);
        }
        drop(driver);
        thread.join().unwrap();
    }

    fn route_answer(source: ProposalSource, base_hash: ContentHash, ops: Vec<BoardOp>) -> RouteProposal {
        RouteProposal {
            pair: None,
            id: ProposalId(0),
            source,
            unit: BoardId(0),
            base_generation: 0,
            base_hash,
            ops,
            checked: None,
            metrics: RouteMetrics::default(),
            verified: true,
            collisions: Vec::new(),
            open: Vec::new(),
            head: None,
            solve_us: 0,
            last: false,
            label: "route drag".into(),
        }
    }

    #[test]
    fn a_panes_route_names_its_nets_method_and_reroute_for_the_resolver() {
        let design = ledctl();
        let unit = design.units.iter().find(|unit| unit.board.is_some()).expect("fixture setup: ledctl has a board unit");
        let nets = [NetId(0), NetId(1)];
        for (method, named, reroute) in [(Method::Interactive, &nets[..], false), (Method::Organic, &[][..], false), (Method::Organic, &nets[..], true)] {
            let (name, args) = route_action(Some(&design), unit.id, named, method, reroute).expect("the board is there");
            let crate::Plan::Job(Job::Route(job)) = crate::route_plan(&design, None, name, &args).expect("the pane's arguments resolve") else {
                panic!("elec.route plans a route job");
            };
            assert_eq!((job.unit, job.method, job.reroute), (unit.id, method, reroute), "the job routes the pane's board by the pane's method, re-routing when asked ({args:?})");
            let want = if named.is_empty() { RouteScope::All } else { RouteScope::Nets(named.to_vec()) };
            assert_eq!(job.scope, want, "named nets are routed by name, none means every unrouted net");
        }
        assert!(route_action(None, unit.id, &nets, Method::Organic, false).is_none(), "named nets on a board that is gone route nothing");
    }

    #[test]
    fn invoke_answers_survive_closed_callers_and_reload() {
        fn resolve(_design: &Design, _docs: Option<&crate::Docs>, tool: &str, _args: &[(String, String)]) -> Result<crate::Plan, fab_elec_core::Diag> {
            Ok(crate::Plan::Edit(Resolved { label: tool.into(), ..Resolved::default() }))
        }
        fn finish(driver: &mut Driver<()>, cx: &mut Cx) {
            // Tests wait for completion on a worker, then drain once; product UI code never joins.
            let job = driver.job.take().expect("the operation schedules a strand job");
            std::thread::spawn(move || job.join()).join().expect("the test join worker exits")
                .expect("the strand job completes without error");
            driver.drain_messages(cx, &mut |_, _, _| false);
        }
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        let mut driver = Driver::<()>::new(&mut cx, steps.clone(), logged.clone());
        let make = move |_: &HostTx<()>| {
            Engine::new(Design::default(), Arc::new(Factories::new()), steps, logged, "invoke".into())
                .with_resolver("test", resolve)
        };
        driver.load(&mut cx, None, make.clone());
        finish(&mut driver, &mut cx);
        let (reply, receiver) = std::sync::mpsc::channel();
        drop(receiver);
        driver.invoke("test.commit".into(), vec![], false, By::Ai { turn: 1, call: 2 }, reply);
        finish(&mut driver, &mut cx);
        assert_eq!(driver.undo_step(), Some(1), "a dropped receiver does not stop the invoked commit's step");
        assert!(driver.replies.is_empty() && driver.tools.is_empty(), "a closed caller is removed after its outcome");
        assert!(!driver.busy(), "the completed step leaves the driver idle despite the closed reply channel");

        let (reply, receiver) = std::sync::mpsc::channel();
        driver.docs(vec![DocSet::Add { name: "first.req.splash".into(), text: "".into() }], "Add requirements".into(), By::Person, reply);
        finish(&mut driver, &mut cx);
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Refused(r)) if r.label == "Add requirements")
            && driver.replies.is_empty() && driver.tools.is_empty(), "a Docs refusal reaches its caller and releases its ticket");

        driver.act(ViewAction::Preview { notes: vec![], tag: 101, ops: vec![], place: vec![], label: "first pane".into() });
        finish(&mut driver, &mut cx);
        let first_seq = driver.preview_seq.load(SeqCst);
        let (reply, receiver) = std::sync::mpsc::channel();
        let by = By::Ai { turn: 1, call: 3 };
        driver.invoke("test.preview".into(), vec![], true, by, reply);
        finish(&mut driver, &mut cx);
        let snapshot = driver.snap().expect("the tool preview is shown");
        assert_eq!(snapshot.preview_by, Some(by), "a delivered tool preview retains its caller's author");
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Previewed { tag }) if Some(tag) == snapshot.preview_tag),
            "Driver routes Previewed to the matching caller with the displayed tag");
        assert_eq!(driver.preview_seq.load(SeqCst), first_seq + 1, "the engine allocates above the preceding pane sample");
        driver.act(ViewAction::Preview { notes: vec![], tag: 102, ops: vec![], place: vec![], label: "next pane".into() });
        finish(&mut driver, &mut cx);
        assert_eq!(driver.preview_seq.load(SeqCst), first_seq + 2, "the next pane sample advances beyond the engine sample");
        assert_eq!(driver.snap().expect("the pane preview is shown").preview_tag, Some(102),
            "a later pane sample publishes over the tool preview");
        driver.act(ViewAction::CancelPreview);
        finish(&mut driver, &mut cx);
        assert!(driver.snap().expect("cancellation is shown").preview.is_none(), "the pane's own sequence still cancels its preview");

        // A pane's drag across a reload (Fable H3): its drop is still answered under its tag.
        driver.act(ViewAction::Preview { notes: vec![], tag: 103, ops: vec![], place: vec![], label: "dragged across a reload".into() });
        finish(&mut driver, &mut cx);
        let (reply, receiver) = std::sync::mpsc::channel();
        driver.invoke("test.pending".into(), vec![], false, by, reply);
        let old = driver.reset();
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Refused(refusal))
            if refusal.seq == 0 && refusal.label == "test.pending" && refusal.op == "invoke"
                && refusal.text == "the electronics engine loaded the bed's documents again; ask again"
                && refusal.site.is_none() && refusal.sample.is_none()),
            "reset answers a pending caller with the exact reload refusal and its tool label");
        assert!(driver.replies.is_empty() && driver.tools.is_empty(), "reset clears all pending reply state");
        finish(&mut driver, &mut cx);
        drop(old);
        driver.load(&mut cx, None, make);
        finish(&mut driver, &mut cx);
        driver.act(ViewAction::CommitPreview);
        finish(&mut driver, &mut cx);
        let refusal = driver.snap().and_then(|snap| snap.last_refusal.clone()).expect("the reloaded engine answers the drop");
        assert_eq!(refusal.sample, Some(103), "the drop of a sample the reload ended is refused under its tag");
        assert!(!driver.previewing(), "the answered drop ends the motion");
    }

    /// Why: the Driver is the loop every electronics host shares; a person's edit, its undo
    /// and its redo must reach the shown snapshot and the undo tops through `event` alone.
    #[test]
    fn a_commit_undo_and_redo_reach_the_snapshot_through_event() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        let mut driver = Driver::<()>::new(&mut cx, steps.clone(), logged.clone());
        driver.load(&mut cx, None, move |_| {
            Engine::new(ledctl(), Arc::new(Factories::new()), steps, logged, "demo".into())
        });
        let pump = |driver: &mut Driver<()>, cx: &mut Cx, ready: fn(&Driver<()>) -> bool, what: &str| {
            let deadline = Instant::now() + HANG;
            loop {
                let _ = driver.event(cx, &mut |_, _, _| false);
                if ready(driver) {
                    return cx.get_global::<ElecLive>().snap.clone();
                }
                assert!(Instant::now() < deadline, "{what}: event reaches the expected snapshot and undo tops");
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        let loaded = pump(&mut driver, &mut cx, |driver| driver.snap().is_some(), "load");
        let unit = loaded.design.units[0].id;
        let via = &loaded.design.units[0].board.as_ref().expect("the loaded fixture has a board").vias[0];
        let (item, before) = (via.id, via.at);
        let position = |snap: &DesignSnapshot| {
            snap.design.unit(unit).expect("the shown snapshot keeps the unit")
                .board.as_ref().expect("the shown unit keeps its board")
                .vias.iter().find(|via| via.id == item).expect("the shown board keeps the moved via").at
        };
        let by = P::mm(0.5, 0.0);
        let _ = driver.act(ViewAction::Ops {
            ops: vec![DesignOp::Board { unit, op: BoardOp::Move { item, by } }],
            place: vec![],
            label: "move".into(),
        });
        let committed = pump(&mut driver, &mut cx, |driver| driver.undo_step().is_some(), "commit");
        assert_eq!(position(&committed), before.add(by), "commit: the shown snapshot's via moves 0.5 mm in x");
        assert!(driver.undo_step().is_some() && driver.redo_step().is_none(), "commit: the edit is undoable with no redo");

        driver.undo();
        let undone = pump(&mut driver, &mut cx, |driver| driver.redo_step().is_some(), "undo");
        assert_eq!(position(&undone), before, "undo: the shown snapshot's via returns to its original position");
        assert!(driver.undo_step().is_none() && driver.redo_step().is_some(), "undo: the edit is redoable with no undo");

        driver.redo();
        let redone = pump(&mut driver, &mut cx, |driver| driver.redo_step().is_none(), "redo");
        assert_eq!(position(&redone), before.add(by), "redo: the shown snapshot's via moves 0.5 mm in x again");
        assert!(driver.undo_step().is_some() && driver.redo_step().is_none(), "redo: the edit is undoable again with no redo");
    }

    /// Why: route answers share the host's preview ordering and gesture tags, and every replaced
    /// or ignored answer must keep its last reference until the engine strand can dispose of it.
    #[test]
    fn a_route_gesture_is_previewed_refused_and_committed_under_its_tag_and_retired_off_the_ui() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        let mut driver = Driver::<()>::new(&mut cx, steps.clone(), logged.clone());
        driver.load(&mut cx, None, move |_| {
            Engine::new(ledctl(), Arc::new(Factories::new()), steps, logged, "demo".into())
        });
        finish(&mut driver, &mut cx);
        driver.show(&mut cx);
        let (handle, inbox) = route_channel(|| {});
        driver.router = Some(Router::new(handle));
        driver.preview_seq.store(100, SeqCst);
        let unit = BoardId(0);
        let board = driver.snap().expect("load publishes the fixture").design.unit(unit)
            .expect("the fixture has unit 0").board.as_ref().expect("the fixture unit has a board");
        let generation = board.generation;
        let via = board.vias[0].id;
        let hash = board.content_hash();
        let sample = |id, seq| GestureSample {
            id, seq, gesture: Gesture::new(GestureKind::Route, unit, generation, Anchor::Free, P::ZERO, LayerId::F_CU),
        };
        let source = |id, seq| ProposalSource::Gesture { id, seq, tier: Tier::Full };
        let id = GestureId(41);

        driver.act(ViewAction::Gesture(sample(id, 1)));
        assert!(!inbox.preempted(), "the first sample waits for its base");
        finish(&mut driver, &mut cx);
        let work = take_work(&inbox);
        let base = work.iter().find_map(|work| match work {
            Work::Request(RouteRequest::Base(base)) => Some(base),
            _ => None,
        }).expect("the first sample's step forwards a base");
        assert_eq!(base.unit, unit, "the first sample receives its own unit's base");
        assert_eq!(base.hash, hash, "the forwarded base has the shown committed board's content hash");
        assert!(matches!(work.first(), Some(Work::Request(RouteRequest::Base(_)))),
            "the base precedes the first sample in worker order");
        assert!(work.iter().any(|work| matches!(work, Work::Sample(s) if s.id == id)),
            "the first sample reaches the worker after its base");

        // drain kicks automatically. Park the strand until both answers have arrived so neither
        // can be taken before the replaced answer's references are counted in the dead lists.
        let (release, parked) = std::sync::mpsc::channel::<()>();
        driver.job = Some(driver.strand.submit(Class::Interactive, TaskTicket::none(), move |_| {
            parked.recv().expect("the test releases the strand after inspecting retirement");
        }));
        inbox.publish(RouteEvent::Proposal(route_answer(source(id, 1), base.hash, vec![])));
        driver.drain(&mut cx, &mut |_, _, _| false);
        let a = Arc::downgrade(cx.get_global::<ElecLive>().gesture.as_ref().expect("answer A is shown in its arrival frame"));
        inbox.publish(RouteEvent::Proposal(route_answer(source(id, 2), base.hash, vec![])));
        driver.drain(&mut cx, &mut |_, _, _| false);
        let shown = cx.get_global::<ElecLive>().gesture.as_ref().expect("answer B replaces answer A");
        assert_eq!(shown.source, source(id, 2), "the live gesture shows the newest answer B");
        let b = Arc::downgrade(shown);
        assert_eq!(driver.live_preview, Some((103, 41)), "router answers use the host's preview counter and the gesture's tag");
        let (in_answers, in_samples) = driver.mailbox.peek(|inbox| (
            inbox.dead_answers.iter().filter(|answer| Arc::as_ptr(answer) == a.as_ptr()).count(),
            inbox.dead_samples.iter().filter(|sample| {
                sample.proposal.as_ref().is_some_and(|answer| Arc::as_ptr(answer) == a.as_ptr())
            }).count(),
        ));
        assert_eq!((in_answers, in_samples), (1, 1),
            "the replaced shown answer A and its untaken sample are both retired to the strand, not dropped on the UI");
        assert_eq!(in_answers + in_samples, a.strong_count(), "every remaining strong reference to A is retired to the strand");
        release.send(()).expect("the parked strand is released");
        finish(&mut driver, &mut cx);
        driver.kick();
        finish(&mut driver, &mut cx);
        assert!(a.upgrade().is_none(), "the strand drops the replaced answer's last reference");
        let snapshot = driver.snap().expect("answer B publishes a preview snapshot");
        assert_eq!(snapshot.preview_tag, Some(41), "the answer is previewed under its gesture's tag");
        assert!(snapshot.proposal.as_ref().is_some_and(|answer| Arc::as_ptr(answer) == b.as_ptr()),
            "the preview snapshot carries answer B");

        let undo_before = driver.undo_step();
        inbox.publish(RouteEvent::Refused(SampleRefused {
            id, seq: 3, unit, cause: SampleCause::Invalid, text: "no such layer".into(),
        }));
        driver.drain(&mut cx, &mut |_, _, _| false);
        driver.kick();
        finish(&mut driver, &mut cx);
        let snapshot = driver.snap().expect("the router refusal is published");
        let refusal = snapshot.last_refusal.as_ref().expect("the router refusal is retained");
        assert_eq!(refusal.sample, Some(41), "a router refusal names the gesture's tag");
        assert_eq!(refusal.text, "no such layer", "a router refusal keeps the worker's reason");
        assert!(snapshot.preview.is_none(), "the refusal uses the host seq to end B's preview");
        let refused_seq = refusal.seq;

        driver.act(ViewAction::CommitPreview);
        finish(&mut driver, &mut cx);
        let refusal = driver.snap().expect("release after refusal is shown").last_refusal.as_ref()
            .expect("release after refusal has a refusal");
        assert_eq!(refusal.sample, Some(41), "release after refusal still names the gesture's tag");
        assert_eq!(refusal.text, "no such layer", "release after refusal repeats the router's reason");
        assert!(refusal.seq > refused_seq, "release after refusal publishes the newest refusal");
        assert_eq!(driver.undo_step(), undo_before, "release after refusal applies no undo step");

        driver.act(ViewAction::Gesture(sample(GestureId(42), 1)));
        driver.act(ViewAction::CommitPreview);
        finish(&mut driver, &mut cx);
        let refusal = driver.snap().expect("release before an answer is shown").last_refusal.as_ref()
            .expect("release before an answer has a refusal");
        assert_eq!(refusal.sample, Some(42), "release before an answer names the new gesture's tag");
        assert_eq!(refusal.text, "the router had not answered this drag: nothing was applied",
            "release before an answer explains why nothing was applied");
        assert_eq!(driver.undo_step(), undo_before, "release before an answer applies no undo step");

        let id = GestureId(43);
        driver.act(ViewAction::Gesture(sample(id, 1)));
        let hash = driver.snap().expect("the next drag has a committed snapshot").design.unit(unit)
            .expect("the next drag keeps unit 0").board.as_ref().expect("the next drag keeps its board").content_hash();
        inbox.publish(RouteEvent::Proposal(route_answer(source(id, 1), hash, vec![BoardOp::Move { item: via, by: P::mm(0.2, 0.0) }])));
        driver.drain(&mut cx, &mut |_, _, _| false);
        driver.kick();
        finish(&mut driver, &mut cx);
        driver.act(ViewAction::CommitPreview);
        finish(&mut driver, &mut cx);
        let snapshot = driver.snap().expect("the route commit is shown");
        assert_eq!(snapshot.last_commit.as_ref().expect("the route commit is retained").tag, Some(43),
            "the route commit is filed under its gesture's tag");
        assert_ne!(driver.undo_step(), undo_before, "committing the answered drag advances the undo step");
        let new_hash = snapshot.design.unit(unit).expect("the committed unit remains").board.as_ref()
            .expect("the committed board remains").content_hash();
        assert_ne!(new_hash, hash, "the committed via move changes the board's content hash");
        assert!(take_work(&inbox).iter().any(|work| matches!(work,
            Work::Request(RouteRequest::Base(base)) if base.unit == unit && base.hash == new_hash)),
            "a commit forwards the routed unit's new committed base");

        let shown = Arc::downgrade(cx.get_global::<ElecLive>().gesture.as_ref().expect("the committed drag's answer is still shown"));
        // act and drain kick the strand. Park it so the retired answer cannot be taken before it
        // is seen in the dead list.
        finish(&mut driver, &mut cx);
        let (release, parked) = std::sync::mpsc::channel::<()>();
        driver.job = Some(driver.strand.submit(Class::Interactive, TaskTicket::none(), move |_| {
            parked.recv().expect("the test releases the strand after inspecting retirement");
        }));
        driver.act(ViewAction::EndGesture(id));
        assert!(take_work(&inbox).iter().any(|work| matches!(work,
            Work::Request(RouteRequest::End(ended)) if *ended == id)),
            "ending the drag reaches the worker");
        inbox.publish(RouteEvent::Proposal(route_answer(source(id, 2), new_hash, vec![])));
        driver.drain(&mut cx, &mut |_, _, _| false);
        assert!(driver.mailbox.peek(|inbox| inbox.dead_answers.iter().any(|answer| Arc::as_ptr(answer) == shown.as_ptr())),
            "the ended drag's shown answer is retired to the strand, not dropped on the UI");
        release.send(()).expect("the parked strand is released");
        finish(&mut driver, &mut cx);
        driver.kick();
        finish(&mut driver, &mut cx);
        assert!(driver.snap().expect("the ended drag keeps a snapshot").preview.is_none(),
            "a late answer cannot preview an ended drag");
        assert!(cx.get_global::<ElecLive>().gesture.is_none(), "the live answer is cleared after the drag ends");
    }

    /// Why: one writer per board. An answer made on a base the host has since replaced would
    /// commit copper planned on a board nobody sees, and a route started during a drag would plan
    /// on the board the drag is about to change.
    #[test]
    fn a_drag_commits_only_on_the_newest_base_and_a_route_invoked_meanwhile_waits_for_it() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        let mut driver = Driver::<()>::new(&mut cx, steps.clone(), logged.clone());
        driver.load(&mut cx, None, move |_| {
            Engine::new(ledctl(), Arc::new(Factories::new()), steps, logged, "route".into())
                .with_resolver("elec.route", crate::route_plan)
        });
        finish(&mut driver, &mut cx);
        driver.show(&mut cx);
        let (handle, inbox) = route_channel(|| {});
        driver.router = Some(Router::new(handle));
        let unit = BoardId(0);
        let board = driver.snap().expect("load publishes the fixture").design.unit(unit)
            .expect("the fixture has unit 0").board.as_ref().expect("the fixture unit has a board");
        let (generation, via) = (board.generation, board.vias[0].id);
        let id = GestureId(61);
        let sample = GestureSample {
            id, seq: 1, gesture: Gesture::new(GestureKind::Route, unit, generation, Anchor::Free, P::ZERO, LayerId::F_CU),
        };
        let source = ProposalSource::Gesture { id, seq: 1, tier: Tier::Stretch };
        let moved = vec![BoardOp::Move { item: via, by: P::mm(0.2, 0.0) }];

        driver.act(ViewAction::Gesture(sample));
        finish(&mut driver, &mut cx);
        let hash = take_work(&inbox).iter().find_map(|work| match work {
            Work::Request(RouteRequest::Base(base)) => Some(base.hash),
            _ => None,
        }).expect("the first sample's step forwards a base");
        let mut superseded = hash;
        superseded.0[0] ^= 1;
        inbox.publish(RouteEvent::Proposal(route_answer(source, superseded, moved.clone())));
        driver.drain(&mut cx, &mut |_, _, _| false);
        assert!(cx.get_global::<ElecLive>().gesture.is_none(), "an answer made on a base the host has replaced is not shown");
        assert!(matches!(driver.router.as_ref().and_then(|router| router.gesture.as_ref()).map(|gesture| &gesture.state),
            Some(GestureState::Waiting)), "the drag waits for the worker's answer on the newest base");

        let (reply, _routed) = mpsc::channel();
        driver.invoke("elec.route".into(), vec![], false, By::Person, reply);
        assert_eq!(driver.router.as_ref().map(|router| router.deferred.len()), Some(1), "a route invoked during the drag waits for it");
        assert!(driver.mailbox.peek(|inbox| !inbox.cmds.iter().any(|(_, cmd)| matches!(cmd, Cmd::Invoke { .. }))),
            "the waiting route is not queued for the engine while the drag is live");

        inbox.publish(RouteEvent::Proposal(route_answer(source, hash, moved)));
        driver.drain(&mut cx, &mut |_, _, _| false);
        assert!(cx.get_global::<ElecLive>().gesture.as_ref().is_some_and(|shown| shown.base_hash == hash),
            "the answer on the newest base is shown");
        finish(&mut driver, &mut cx);
        // Park the strand so release's kick leaves the queued commands to be inspected.
        driver.job = Some(driver.strand.submit(Class::Interactive, TaskTicket::none(), |_| {}));
        driver.act(ViewAction::CommitPreview);
        assert!(driver.mailbox.peek(|inbox| matches!(inbox.cmds.as_slice(),
            [.., (_, Cmd::CommitPreview { .. }), (_, Cmd::Invoke { tool, .. })] if tool == "elec.route")),
            "the release commits first, then the waiting route plans on what it committed");
        assert_eq!(driver.router.as_ref().map(|router| router.deferred.len()), Some(0), "the waiting route is sent once");
        finish(&mut driver, &mut cx);
        driver.kick();
        finish(&mut driver, &mut cx);
        let snapshot = driver.snap().expect("the drag's commit is shown");
        assert_eq!(snapshot.last_commit.as_ref().expect("the drag's commit is retained").tag, Some(id.tag()),
            "the drag commits under its gesture's tag");
        let committed = snapshot.design.unit(unit).and_then(|unit| unit.board.as_ref()).expect("the committed board remains").content_hash();
        assert_ne!(committed, hash, "the drag's commit moved the via");
        // The route starts once the commit's fills are exact.
        let deadline = Instant::now() + HANG;
        while driver.router.as_ref().is_some_and(|router| router.job.is_none()) {
            let _ = driver.event(&mut cx, &mut |_, _, _| false);
            assert!(Instant::now() < deadline, "the waiting route starts after the drag's commit");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(take_work(&inbox).iter().any(|work| matches!(work, Work::Request(RouteRequest::Route(_)))),
            "the waiting route reaches the worker after the commit");
        drop(driver.reset());
    }

    /// Why: job proposals and progress must reach the shown snapshot through the real channel,
    /// while strand publications, unrelated jobs and pane actions preserve job ownership.
    #[test]
    fn a_route_job_previews_answers_and_overlays_its_status() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        let mut driver = Driver::<()>::new(&mut cx, steps.clone(), logged.clone());
        driver.load(&mut cx, None, move |_| {
            Engine::new(ledctl(), Arc::new(Factories::new()), steps, logged, "route".into())
                .with_resolver("elec.route", crate::route_plan)
        });
        finish(&mut driver, &mut cx);
        let (handle, inbox) = route_channel(|| {});
        driver.router = Some(Router::new(handle));
        let by = By::Ai { turn: 1, call: 2 };
        let (reply, receiver) = std::sync::mpsc::channel();
        driver.invoke("elec.route".into(), vec![], false, by, reply);
        finish(&mut driver, &mut cx);
        let work = take_work(&inbox);
        let [Work::Request(RouteRequest::Base(base)), Work::Request(RouteRequest::Route(job))] = work.as_slice() else {
            panic!("a route start forwards its base before its job");
        };
        assert!(base.exact, "the worker receives exact committed fills before its job");
        assert_eq!(base.hash, base.board.content_hash(), "the worker base names the committed board content");
        assert_eq!(job.job, JobId(1), "the worker receives the strand's numbered job");
        assert!(receiver.try_recv().is_err(), "starting a job leaves its caller pending");
        let tag = job.job.tag();
        let net = NetId(0);
        let name = base.board.nets[net.0 as usize].name.clone();
        let status = JobStatus {
            job: job.job, unit: job.unit, phase: JobPhase::Nets, current: Some(net), routed: 1, failed: 0, total: 3,
            attempt: 0, retry: None, blocked_by: None,
        };
        inbox.publish(RouteEvent::Status(status));
        driver.drain(&mut cx, &mut |_, _, _| false);
        let state = driver.snap().expect("the job has a shown snapshot").job.as_ref().expect("the status is overlaid").clone();
        assert_eq!(state.status, status, "the shown job carries the worker's current net and counts");
        assert_eq!(state.current_name.as_deref(), Some(name.as_str()), "the shown job resolves the committed board's net name");
        assert_eq!(state.end, None, "a status update keeps the job running");
        let shown = driver.snap().expect("the status snapshot remains").clone();
        assert!(!driver.drain(&mut cx, &mut |_, _, _| false), "an idle job drain changes no snapshot");
        assert!(Arc::ptr_eq(&shown, driver.snap().expect("the idle snapshot remains")), "unchanged job state avoids a snapshot clone");

        let via = base.board.vias[0].id;
        let mut partial = route_answer(ProposalSource::Job { job: job.job, step: 1 }, base.hash,
            vec![BoardOp::Move { item: via, by: P::mm(0.2, 0.0) }]);
        partial.unit = job.unit;
        partial.base_generation = base.board.generation;
        inbox.publish(RouteEvent::Proposal(partial.clone()));
        driver.drain(&mut cx, &mut |_, _, _| false);
        driver.kick();
        finish(&mut driver, &mut cx);
        let snapshot = driver.snap().expect("the partial publishes a strand snapshot");
        assert_eq!(snapshot.preview_tag, Some(tag), "the job partial previews under its job tag");
        assert_eq!(snapshot.proposal.as_deref(), Some(&partial), "the preview carries the job's exact partial answer");
        assert_eq!(snapshot.preview_by, Some(by), "the job partial retains its caller's author");
        assert!(snapshot.job.is_none(), "strand snapshots leave job state for the Driver overlay");
        driver.drain(&mut cx, &mut |_, _, _| false);
        assert!(driver.snap().expect("the partial remains shown").job.as_ref().is_some_and(|shown| Arc::ptr_eq(shown, &state)),
            "draining overlays the same job state after a strand snapshot arrives");
        assert!(driver.live_preview.is_none(), "job previews do not become the pane's commit target");
        let _ = driver.act(ViewAction::CommitPreview);
        finish(&mut driver, &mut cx);
        assert_eq!(driver.undo_step(), None, "the pane's Enter does not commit the job preview");
        let mut late = partial.clone();
        late.base_hash = ContentHash([7; 16]);
        late.ops = vec![BoardOp::Move { item: via, by: P::mm(0.6, 0.0) }];
        inbox.publish(RouteEvent::Proposal(late));
        driver.drain(&mut cx, &mut |_, _, _| false);
        driver.kick();
        finish(&mut driver, &mut cx);
        assert_eq!(driver.snap().expect("the partial remains shown").proposal.as_deref(), Some(&partial),
            "an answer with this job's id on another base (a job from before a reset) is not previewed");
        let mut stray = partial.clone();
        stray.source = ProposalSource::Job { job: JobId(job.job.0 + 1), step: 1 };
        inbox.publish(RouteEvent::Proposal(stray));
        driver.drain(&mut cx, &mut |_, _, _| false);
        driver.kick();
        finish(&mut driver, &mut cx);
        assert_eq!(driver.snap().expect("the partial remains shown").proposal.as_deref(), Some(&partial),
            "another job's answer is not previewed while this one runs");

        let mut last = partial.clone();
        last.source = ProposalSource::Job { job: job.job, step: 2 };
        last.ops = vec![BoardOp::Move { item: via, by: P::mm(0.4, 0.0) }];
        last.last = true;
        inbox.publish(RouteEvent::Proposal(last.clone()));
        driver.drain(&mut cx, &mut |_, _, _| false);
        driver.kick();
        finish(&mut driver, &mut cx);
        driver.drain(&mut cx, &mut |_, _, _| false);
        let committed = driver.undo_step();
        assert!(committed.is_some(), "the final proposal creates an undo step");
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Done { seq, label, .. })
            if Some(seq) == committed && label == last.label),
            "finishing answers the caller with the committed step and the job's label");
        assert!(receiver.try_recv().is_err(), "finishing answers the caller exactly once");
        let snapshot = driver.snap().expect("the committed route remains shown");
        assert_eq!(snapshot.preview_tag, None, "committing the final proposal clears its preview tag");
        assert!(snapshot.preview.is_none(), "committing the final proposal clears the preview design");
        assert!(snapshot.proposal.is_none(), "committing the final proposal clears the shown proposal");
        assert_eq!(snapshot.last_commit.as_ref().map(|commit| (commit.tag, commit.by)), Some((Some(tag), by)),
            "the committed route retains the job's tag and author");
        assert_eq!(snapshot.job.as_ref().expect("the finished job keeps its state").end, Some(JobEnd::Finished),
            "the last proposal marks the shown job finished");
        assert_eq!(snapshot.job.as_ref().expect("the final state remains").status, status,
            "finishing preserves the most recent worker status");

        inbox.publish(RouteEvent::Proposal(last.clone()));
        driver.drain(&mut cx, &mut |_, _, _| false);
        driver.kick();
        finish(&mut driver, &mut cx);
        driver.drain(&mut cx, &mut |_, _, _| false);
        assert!(driver.snap().expect("the committed route remains shown").proposal.is_none(),
            "an ended job's later proposal is not previewed");
        assert_eq!(driver.undo_step(), committed, "an ended job's later proposal makes no second entry");
        assert!(receiver.try_recv().is_err(), "an ended job's later proposal makes no second outcome");

        inbox.publish(RouteEvent::Status(JobStatus { current: None, ..status }));
        driver.drain(&mut cx, &mut |_, _, _| false);
        let state = driver.snap().expect("the final status is shown").job.as_ref().expect("the final status keeps job state");
        assert_eq!(state.end, Some(JobEnd::Finished), "a late status preserves the job's finished state");
        assert!(state.current_name.is_none(), "no current net clears the shown net name");
        let mut other = last.clone();
        other.source = ProposalSource::Job { job: JobId(job.job.0 + 1), step: 1 };
        inbox.publish(RouteEvent::Proposal(other));
        inbox.publish(RouteEvent::Status(JobStatus { job: JobId(job.job.0 + 1), ..status }));
        driver.drain(&mut cx, &mut |_, _, _| false);
        driver.kick();
        finish(&mut driver, &mut cx);
        driver.drain(&mut cx, &mut |_, _, _| false);
        let snapshot = driver.snap().expect("an unrelated job preserves the snapshot");
        assert!(snapshot.proposal.is_none(), "another job's proposal is never previewed");
        assert_eq!(snapshot.job.as_ref().expect("the finished job remains shown").status.job, job.job,
            "another job's status cannot replace the shown job");
        assert!(receiver.try_recv().is_err(), "unrelated proposals and statuses make no second caller outcome");
        let old = driver.reset();
        assert!(driver.job_state.is_none(), "reset clears the Driver's job state");
        assert!(driver.router.as_ref().expect("reset retains the router").job.is_none(), "reset forgets the running job identity");
        assert!(driver.snap().is_none(), "reset removes the snapshot carrying the old job overlay");
        drop(old);
    }

    /// Why: keeping a job must still commit its sample when a pane preview replaces the slot
    /// before the strand has taken either sample.
    #[test]
    fn a_kept_routes_sample_survives_a_newer_preview_in_the_slot() {
        use fab_elec_board::{Item, Origin, Track};
        use fab_elec_core::{mm, Path, Sel};
        use fab_elec_route::Reason;

        let mut cx = Cx::new(Box::new(|_, _| {}));
        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        let mut driver = Driver::<()>::new(&mut cx, steps.clone(), logged.clone());
        driver.load(&mut cx, None, move |_| {
            Engine::new(ledctl(), Arc::new(Factories::new()), steps, logged, "route".into())
                .with_resolver("elec.route", crate::route_plan)
        });
        finish(&mut driver, &mut cx);
        let (handle, inbox) = route_channel(|| {});
        driver.router = Some(Router::new(handle));
        let (reply, receiver) = mpsc::channel();
        driver.invoke("elec.route".into(), vec![], false, By::Person, reply);
        finish(&mut driver, &mut cx);
        let work = take_work(&inbox);
        let [Work::Request(RouteRequest::Base(base)), Work::Request(RouteRequest::Route(job))] = work.as_slice() else {
            panic!("the stub worker receives the route's base before its job");
        };
        let via = &base.board.vias[0];
        let (item, before) = (via.id, via.at);
        let mut partial = route_answer(ProposalSource::Job { job: job.job, step: 1 }, base.hash,
            vec![BoardOp::Move { item, by: P::mm(0.2, 0.0) }]);
        partial.unit = job.unit;
        partial.base_generation = base.board.generation;
        assert!(driver.idle(), "no engine step is running when the stub offers its partial");
        assert!(driver.route_event(&mut cx, RouteEvent::Proposal(partial.clone())),
            "the running job accepts the stub's partial into the mailbox");
        let seq = driver.router.as_ref().and_then(|router| router.job.as_ref()).and_then(|job| job.seq)
            .expect("the offered partial has a host sequence");
        // act kicks automatically. This no-op handle prevents an engine step from taking the
        // mailbox until after inspection, even if the no-op itself has already completed.
        driver.job = Some(driver.strand.submit(Class::Interactive, TaskTicket::none(), |_| {}));
        let corner = before.add(P::mm(1.0, 0.0));
        let track = Track {
            id: ItemId(base.board.next_item), net: via.net, layer: LayerId::F_CU, w: mm(0.2), widths: None,
            path: Path::new(before).line(corner).line(corner.add(P::mm(-0.5, 0.5))),
            ends: [Anchor::Free; 2], locked: false, origin: Origin::Authored,
        };
        let notes = vec![(track.net, Reason::SharpCorner { at: corner, layer: track.layer, track: Sel::Item(job.unit, track.id) })];
        assert!(driver.act(ViewAction::Preview {
            tag: 502, notes: notes.clone(),
            ops: vec![DesignOp::Board { unit: job.unit, op: BoardOp::Move { item, by: P::mm(0.1, 0.0) } },
                DesignOp::Board { unit: job.unit, op: BoardOp::Add(Item::Track(track)) }],
            place: vec![], label: "drag".into(),
        }).is_none(), "the pane's preview is handled by the driver");
        driver.mailbox.peek(|inbox| {
            assert!(matches!(inbox.cmds.as_slice(), [(_, Cmd::CommitPreview { seq: kept, tag })]
                if *kept == seq && *tag == job.job.tag()),
                "keeping the job queues a commit for its exact sample before the pane preview is taken");
            assert_eq!(inbox.committing.len(), 1, "the replaced job sample is retained exactly once for its commit");
            assert_eq!(inbox.committing[0].seq, seq, "the retained sample has the job's newest sequence");
            assert_eq!(inbox.committing[0].proposal.as_deref(), Some(&partial),
                "the retained sample carries the job's exact partial proposal");
            assert!(!inbox.dead_samples.iter().any(|sample| sample.seq == seq),
                "the queued job commit's sample is not retired as dead");
            assert_eq!(inbox.preview.as_ref().map(|sample| sample.tag), Some(502),
                "the newest slot belongs to the pane's drag");
            assert!(inbox.preview.as_ref().is_some_and(|s| s.notes == notes && s.proposal.is_none()),
                "the pane preview carries its router notes without echoing a router answer");
        });
        finish(&mut driver, &mut cx);
        driver.kick();
        finish(&mut driver, &mut cx);
        driver.drain(&mut cx, &mut |_, _, _| false);
        assert!(driver.undo_step().is_some(), "the retained job sample commits as an undo step");
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Done { seq, .. }) if Some(seq) == driver.undo_step()),
            "the retained job sample answers its caller with the committed step");
        assert!(receiver.try_recv().is_err(), "keeping the retained sample answers its caller exactly once");
        let snap = driver.snap().expect("the kept route and pane preview have a shown snapshot");
        assert_eq!(snap.preview_tag, Some(502), "the pane's preview is shown after the route commit");
        assert_eq!(snap.job.as_ref().and_then(|state| state.end), Some(JobEnd::Stopped),
            "keeping the route shows its job as stopped");
        let position = |design: &Design| design.unit(job.unit).and_then(|unit| unit.board.as_ref())
            .expect("the route and drag keep the unit's board").vias.iter().find(|via| via.id == item)
            .expect("the route and drag keep the moved via").at;
        assert_eq!(position(&snap.design), before.add(P::mm(0.2, 0.0)),
            "the retained route's move is committed before the pane's move");
        assert_eq!(position(snap.preview.as_deref().expect("the pane's drag has a preview design")),
            before.add(P::mm(0.3, 0.0)), "the pane's preview applies its move on top of the kept route");
    }

    /// Why: invoking a new route stops the old job before any proposal with exactly one
    /// refusal for its caller, with cancellation ordered before the replacement's base and job.
    #[test]
    fn invoking_a_route_during_a_job_first_stops_the_running_one() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        let mut driver = Driver::<()>::new(&mut cx, steps.clone(), logged.clone());
        driver.load(&mut cx, None, move |_| {
            Engine::new(ledctl(), Arc::new(Factories::new()), steps, logged, "route".into())
                .with_resolver("elec.route", crate::route_plan)
        });
        finish(&mut driver, &mut cx);
        let (handle, inbox) = route_channel(|| {});
        driver.router = Some(Router::new(handle));
        let (reply, first) = std::sync::mpsc::channel();
        driver.invoke("elec.route".into(), vec![], false, By::Person, reply);
        finish(&mut driver, &mut cx);
        let _ = take_work(&inbox);
        let old = driver.router.as_ref().and_then(|router| router.job.as_ref()).expect("the first route is running");
        let (id, tag, reserved) = (old.id, old.tag, old.reserved);
        assert!(old.seq.is_none(), "a newly started job has no proposal sequence");
        // Keep invoke's automatic kick from taking the mailbox before its ordering is inspected.
        // This task does no engine work; finish clears its handle before the real kick below.
        driver.job = Some(driver.strand.submit(Class::Interactive, TaskTicket::none(), |_| {}));
        let (reply, second) = std::sync::mpsc::channel();
        driver.invoke("elec.route".into(), vec![], true, By::Person, reply);
        assert!(driver.mailbox.peek(|inbox| matches!(inbox.cmds.as_slice(),
            [(_, Cmd::RefusePreview { seq, tag: refused, reason }), (_, Cmd::Invoke { .. })]
                if *seq == reserved && *refused == tag && reason == "stopped before any net was routed")),
            "invoking a replacement first refuses the original reservation when no proposal has arrived");
        finish(&mut driver, &mut cx);
        driver.kick();
        finish(&mut driver, &mut cx);
        let work = take_work(&inbox);
        // The worker takes a queued base before the other commands, so it reads the new base first.
        assert!(matches!(work.as_slice(), [Work::Request(RouteRequest::Base(_)),
            Work::Request(RouteRequest::Cancel(cancelled)), Work::Request(RouteRequest::Route(job))]
            if *cancelled == id && job.job == JobId(id.0 + 1)),
            "replacement sends the cancellation, the new exact base and the numbered job");
        assert!(matches!(first.try_recv(), Ok(Outcome::Refused(refusal))
            if refusal.text == "stopped before any net was routed" && refusal.sample == Some(tag)),
            "the stopped job's caller receives its tagged refusal before the replacement runs");
        assert!(first.try_recv().is_err(), "the replaced job's caller receives only one outcome");
        assert!(second.try_recv().is_err(), "the replacement job's caller remains pending");
        driver.drain(&mut cx, &mut |_, _, _| false);
        let state = driver.snap().expect("the replacement keeps a snapshot").job.as_ref().expect("the replacement has shown job state");
        assert_eq!(state.status.job, JobId(id.0 + 1), "the overlay follows the replacement job");
        assert_eq!(state.end, None, "the replacement resets the shown job to running");
        let old = driver.reset();
        assert!(matches!(take_work(&inbox).as_slice(), [Work::Request(RouteRequest::Cancel(cancelled))] if *cancelled == JobId(id.0 + 1)),
            "a reset stops the running job on the worker");
        drop(old);
    }

    /// Why: a stop before the first proposal cancels once and refuses the pending caller by job tag.
    #[test]
    fn stopping_a_route_job_before_a_proposal_refuses_its_caller() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        let mut driver = Driver::<()>::new(&mut cx, steps.clone(), logged.clone());
        driver.load(&mut cx, None, move |_| {
            Engine::new(ledctl(), Arc::new(Factories::new()), steps, logged, "route".into())
                .with_resolver("elec.route", crate::route_plan)
        });
        finish(&mut driver, &mut cx);
        let (handle, inbox) = route_channel(|| {});
        driver.router = Some(Router::new(handle));
        let (reply, receiver) = mpsc::channel();
        driver.invoke("elec.route".into(), vec![], false, By::Person, reply);
        finish(&mut driver, &mut cx);
        let _ = take_work(&inbox);
        let job = driver.router.as_ref().and_then(|router| router.job.as_ref()).expect("the route is running before the stop");
        let id = job.id;
        assert!(job.seq.is_none(), "the stopped job has not received any proposal");

        assert!(driver.act(ViewAction::StopRoute(JobId(id.0 + 1))).is_none(), "a stop for another job is not broadcast");
        assert!(driver.act(ViewAction::DiscardRoute(JobId(id.0 + 1))).is_none(), "a discard for another job is not broadcast");
        assert!(take_work(&inbox).is_empty(), "actions for another job send no worker requests");
        assert!(receiver.try_recv().is_err(), "actions for another job leave the running caller pending");

        assert!(driver.act(ViewAction::StopRoute(id)).is_none(), "stopping before a proposal is not a broadcast action");
        assert!(matches!(take_work(&inbox).as_slice(), [Work::Request(RouteRequest::Cancel(cancelled))] if *cancelled == id),
            "stopping before a proposal sends exactly one cancel for the running job");
        finish(&mut driver, &mut cx);
        driver.drain(&mut cx, &mut |_, _, _| false);
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Refused(refusal))
            if refusal.text == "stopped before any net was routed" && refusal.sample == Some(id.tag())),
            "stopping before a proposal refuses the caller with the reason under the job's tag");
        assert_eq!(driver.undo_step(), None, "stopping before a proposal commits nothing");
        assert_eq!(driver.snap().expect("stopping keeps a shown snapshot").job.as_ref().and_then(|state| state.end),
            Some(JobEnd::Stopped), "stopping before a proposal shows the job as stopped");

        assert!(driver.act(ViewAction::StopRoute(id)).is_none(), "stopping an ended job is not broadcast");
        assert!(driver.act(ViewAction::DiscardRoute(id)).is_none(), "discarding an ended job is not broadcast");
        finish(&mut driver, &mut cx);
        driver.drain(&mut cx, &mut |_, _, _| false);
        assert!(take_work(&inbox).is_empty(), "actions for an ended job send no second cancel");
        assert!(receiver.try_recv().is_err(), "actions for an ended job make no second caller outcome");
        assert_eq!(driver.snap().expect("repeated actions keep a shown snapshot").job.as_ref().and_then(|state| state.end),
            Some(JobEnd::Stopped), "actions for an ended job preserve its stopped state");
    }

    /// Why: the person sees a running job's partial routes on the board before the job ends
    /// ("routing visible incrementally"). The real worker holds its job at the first partial
    /// until the shown snapshot carries it, so this holds on any machine load.
    #[test]
    fn a_route_job_shows_its_partials_live() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let Held { mut driver, wakes, held, release, thread, id, receiver, before } = held_route(&mut cx);
        held.recv_timeout(HANG).expect("the real job reaches its first partial");
        let ours = |answer: &RouteProposal| matches!(answer.source, ProposalSource::Job { job, .. } if job == id);
        // Wait until the driver has received the held partial (it gives it a host sequence); showing
        // it then takes one driver round and no worker event, so a partial left unshown fails below.
        loop {
            driver.drain(&mut cx, &mut |_, _, _| false);
            if driver.router.as_ref().and_then(|router| router.job.as_ref()).is_some_and(|job| job.seq.is_some()) {
                break;
            }
            wakes.recv_timeout(HANG).expect("the worker delivers the held partial");
        }
        let mut routed = show(&mut driver, &mut cx, &wakes, |_, _| true);
        let snap = driver.snap().expect("the partial is shown");
        assert!(snap.proposal.as_deref().is_some_and(ours), "the driver shows the job's partial it received");
        let partial = snap.proposal.as_deref().expect("a job answer is shown");
        assert!(!partial.last, "the job's first partial is shown while the job is still held");
        assert!(!partial.ops.is_empty(), "the shown partial carries routed copper");
        assert_eq!(snap.preview_tag, Some(id.tag()), "the partial previews under the job's tag");
        assert_eq!(snap.job.as_ref().map(|state| (state.status.job, state.end)), Some((id, None)),
            "the shown job is this one, still running");

        release.send(()).expect("the held job waits for its release");
        routed.extend(show(&mut driver, &mut cx, &wakes, |driver, snap| {
            snap.job.as_ref().and_then(|state| state.end) == Some(JobEnd::Finished) && driver.undo_step().is_some()
        }));
        let snap = driver.snap().expect("the committed route is shown");
        assert_eq!(snap.job.as_ref().and_then(|state| state.end), Some(JobEnd::Finished), "the last proposal ends the shown job");
        assert_eq!(snap.preview_tag, None, "the final proposal commits and clears its preview tag");
        assert!(snap.proposal.is_none(), "the final proposal is no longer previewed after committing");
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Done { .. })), "the finished route answers its caller with Done");
        assert!(receiver.try_recv().is_err(), "the finished route answers its caller exactly once");
        assert_ne!(board_hash(&snap.design), before, "finishing commits the routed copper to unit 0's board");
        assert!(routed.windows(2).all(|counts| counts[0] <= counts[1]), "shown routed-net counts never fall: {routed:?}");
        driver.undo();
        finish(&mut driver, &mut cx);
        driver.drain(&mut cx, &mut |_, _, _| false);
        assert_eq!(board_hash(&driver.snap().expect("undo keeps a shown snapshot").design), before, "undo restores the board");
        assert_eq!(driver.undo_step(), None, "the whole route was committed as one undo step");
        drop(driver);
        thread.join().expect("the route worker exits when its channel closes");
    }

    /// Why: a person's Route from a board pane must run a real job through the tools a Fab session
    /// registers (`Engine::with_app_tools`, as the app builds its engine): the job routes the nets
    /// the pane named, its partial shows under the job's tag while it runs, and the finished route
    /// commits as one undo step.
    #[test]
    fn a_route_from_the_board_pane_runs_through_the_app_tools() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        let mut driver = Driver::<()>::new(&mut cx, steps.clone(), logged.clone());
        driver.load(&mut cx, None, move |_| {
            Engine::new(ledctl(), Arc::new(Factories::new()), steps, logged, "route".into()).with_app_tools()
        });
        finish(&mut driver, &mut cx);
        let before = board_hash(&driver.snap().expect("the route fixture is loaded").design);
        let (woke, wakes) = mpsc::channel();
        let (handle, inbox) = route_channel(move || {
            let _ = woke.send(());
        });
        let (held_at, held) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let released = std::sync::Mutex::new(Some(released));
        let worker = RouteWorker::new(inbox, Arc::new(OwnThreads)).with_offered(move |_, step| {
            if let Some(released) = released.lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = held_at.send(step);
                let _ = released.recv();
            }
        });
        let thread = std::thread::spawn(move || worker.run());
        driver.router = Some(Router::new(handle));
        let unit = BoardId(0);
        let nets = {
            let snap = driver.snap().expect("the route fixture is loaded");
            let board = snap.design.unit(unit).and_then(|found| found.board.as_ref()).expect("fixture setup: ledctl unit 0 has a board");
            let conn = snap.conn.iter().find(|conn| conn.unit == Some(unit) && conn.of == fab_elec_view::StateOf::Committed)
                .expect("fixture setup: ledctl has committed connectivity");
            scope_nets(board, &conn.value, &RouteScope::All).into_iter().take(2).collect::<Vec<_>>()
        };
        assert_eq!(nets.len(), 2, "fixture setup: ledctl has two open nets");
        let ticket = driver.next_ticket();
        assert!(driver.act(ViewAction::Route { unit, nets: nets.clone(), method: Method::Interactive, reroute: false }).is_none(), "a route is not a broadcast action");
        assert_eq!(driver.next_ticket(), ticket, "only the host submits the Route press");
        let (name, args) = route_action(driver.snap().map(|s| s.design.as_ref()), unit, &nets, Method::Interactive, false).unwrap();
        let (reply, receiver) = mpsc::channel();
        driver.invoke(name.into(), args, false, By::Person, reply);
        finish(&mut driver, &mut cx);
        let id = driver.router.as_ref().and_then(|router| router.job.as_ref()).map(|job| job.id)
            .expect("the pane's route starts a job on ledctl's exact fills");
        held.recv_timeout(HANG).expect("the job reaches its first partial");
        loop {
            driver.drain(&mut cx, &mut |_, _, _| false);
            if driver.router.as_ref().and_then(|router| router.job.as_ref()).is_some_and(|job| job.seq.is_some()) {
                break;
            }
            wakes.recv_timeout(HANG).expect("the worker delivers the held partial");
        }
        show(&mut driver, &mut cx, &wakes, |_, _| true);
        let snap = driver.snap().expect("the partial is shown");
        let partial = snap.proposal.as_deref().expect("the job's partial is shown while it runs");
        assert!(matches!(partial.source, ProposalSource::Job { job, .. } if job == id) && !partial.last && !partial.ops.is_empty(),
            "the shown partial is this job's routed copper, before its last answer");
        assert_eq!((snap.preview_tag, snap.preview_by), (Some(id.tag()), Some(By::Person)), "the partial previews under the job's tag as the person's");
        assert_eq!(snap.job.as_ref().map(|state| (state.status.total, state.end)), Some((2, None)), "the job routes the two nets the pane named");
        let scope = snap.job.as_ref().map(|state| (*state.scope).clone()).expect("the running job has a state");
        let RouteScope::Nets(mut named) = scope else { panic!("the pane's route names its nets: {scope:?}") };
        named.sort();
        assert_eq!(named, nets, "the job's state keeps the nets it was asked to route, for the board's line");

        release.send(()).expect("the held job waits for its release");
        show(&mut driver, &mut cx, &wakes, |driver, snap| {
            snap.job.as_ref().and_then(|state| state.end) == Some(JobEnd::Finished) && driver.undo_step().is_some()
        });
        let snap = driver.snap().expect("the committed route is shown");
        assert!(snap.proposal.is_none(), "the finished route is committed, not previewed");
        assert_ne!(board_hash(&snap.design), before, "the finished route commits its copper");
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Done { by: By::Person, .. })), "the board's Route answers its own ticket");
        drop(driver);
        thread.join().expect("the route worker exits when its channel closes");
    }

    /// Why: the board strip's Update (the person's Place) and the AI's `elec.place` with `@place`
    /// run a real place job through the tools a Fab session registers: the circuit's parts the
    /// board does not place yet go on legal spots inside the outline (a part the circuit fixes to a
    /// spot on that spot), the job shows as placing
    /// while it runs, its final commits as one undo step, the AI's answer counts what it placed,
    /// and with nothing left to place the call is refused with what to do instead.
    #[test]
    fn a_place_job_brings_the_circuit_parts_onto_the_board() {
        use fab_elec_board::placement::{placed_courtyard, PlacementIndex};
        use fab_elec_route::place::Pose;
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        for (cased, part, cancel) in [(true, "D1", Some(false)), (true, "D1", Some(true)),
            (false, "D1", None), (true, "R1", None)] {
            let mut empty = ledctl();
            let board = Arc::make_mut(empty.units[0].board.as_mut().unwrap());
            board.placed.clear();
            board.tracks.clear();
            board.vias.clear();
            board.zones.clear();
            board.reindex();
            let unit = board.unit;
            let generation = board.generation;
            let mut driver = Driver::<()>::new(&mut cx, Arc::default(), Arc::default());
            driver.load(&mut cx, None, move |_| Engine::new(empty, Arc::new(Factories::new()), Arc::default(),
                Arc::default(), "place".into()).with_app_tools());
            finish(&mut driver, &mut cx);
            let (handle, inbox) = route_channel(|| {});
            driver.router = Some(Router::new(handle));
            driver.push(Cmd::CaseFindings { unit, generation, run: 1, cased, findings: vec![] });
            let (reply, receiver) = mpsc::channel();
            driver.invoke("elec.place".into(), vec![("mode".into(), "@place".into()), ("parts".into(), part.into())],
                false, By::Person, reply);
            finish(&mut driver, &mut cx);
            let state = driver.job_state.as_ref().expect("placing has a line immediately");
            let id = state.status.job;
            if let Some(discard) = cancel {
                assert_eq!(state.status.phase, JobPhase::WaitCase);
                assert!(take_work(&inbox).is_empty());
                assert!(driver.snap().unwrap().design.units[0].board.as_ref().unwrap().placed.len() == 1);
                driver.act(if discard { ViewAction::DiscardRoute(id) } else { ViewAction::StopRoute(id) });
                driver.push(Cmd::CaseOutcome { unit, stamp: driver.snap().unwrap().design.generation, reason: None });
                driver.kick();
                finish(&mut driver, &mut cx);
                assert!(take_work(&inbox).is_empty(), "the late case cannot restart a cancelled wait");
                assert!(matches!(receiver.try_recv(), Ok(Outcome::Refused(_))));
                assert_eq!(driver.snap().unwrap().design.units[0].board.as_ref().unwrap().placed.len(), usize::from(!discard));
                assert_eq!(driver.undo_step().is_none(), discard);
                assert!(driver.redo_step().is_none());
            } else {
                assert!(matches!(state.status.phase, JobPhase::Seed { .. }));
                assert!(take_work(&inbox).iter().any(|work| matches!(work, Work::Request(RouteRequest::Place(job)) if job.add.len() == 1)));
                assert!(driver.undo_step().is_none());
            }
        }
        let taken = ["lamps[0].r", "lamps[1].r", "lamps[2].r", "btn.sw"];
        let mut design = ledctl();
        // The button is fixed to a spot: where it stood, turned a half. Brought, it lands there
        // instead of in the row under the board.
        let spot = design.units[0].board.as_deref().and_then(|b| b.placed.iter().find(|p| p.path == taken[3]))
            .map(|p| fab_elec_board::Spot { comp: p.comp, at: p.at, rot: Some(p.rot.add(fab_elec_core::Angle::HALF)) })
            .expect("fixture setup: btn.sw is placed");
        Arc::make_mut(&mut design.units[0].circuit).intents.push(fab_elec_circuit::Intent::Fixed {
            t: fab_elec_circuit::Target::Comp(spot.comp), edge: None, at: Some(spot.at), rot: spot.rot,
        });
        let board = Arc::make_mut(design.units[0].board.as_mut().expect("fixture setup: ledctl unit 0 has a board"));
        board.spots = vec![spot];
        board.placed.retain(|p| !taken.contains(&p.path.as_str()));
        board.tracks.clear();
        board.vias.clear();
        // Without pours no fill job stands between a commit and the next job's start, so every
        // wait below is on a worker event (`show`).
        board.zones.clear();
        // A hand-drawn track from lamps[3].d to its resistor, the LED moved a quarter of the way
        // to the board's middle, off where its nets pull it: placing the LED again takes the
        // track off, counted against the LED, and one undo brings it back.
        let led = board.placed.iter().position(|p| p.path == "lamps[3].d").expect("fixture setup: lamps[3].d is placed");
        let middle = board.outline_bbox().center();
        let at = board.placed[led].at;
        board.placed[led].at = at.add(P::new((middle.x - at.x) / 4, (middle.y - at.y) / 4));
        let (led_id, led_ref) = (board.placed[led].id, board.placed[led].refdes.clone());
        let resistor = board.placed.iter().find(|p| p.path == "lamps[3].r").expect("fixture setup: lamps[3].r is placed");
        let (pad, net, far) = board.placed[led].pad_nets.iter().enumerate().find_map(|(k, net)| {
            let net = (*net)?;
            let j = resistor.pad_nets.iter().position(|n| *n == Some(net))?;
            Some((PadIx(k as _), net, (resistor.id, PadIx(j as _), resistor.pad_at(PadIx(j as _))?)))
        }).expect("fixture setup: the LED and its resistor share a net");
        let from = board.placed[led].pad_at(pad).expect("fixture setup: the LED's pad");
        let id = board.alloc_id();
        board.tracks.push(Track {
            id, net, layer: LayerId::F_CU, w: mm(0.25), widths: None, path: Path::new(from).line(far.2),
            ends: [Anchor::Pad { placed: led_id, pad }, Anchor::Pad { placed: far.0, pad: far.1 }], locked: false, origin: Origin::Authored,
        });
        board.reindex();
        let mut driver = Driver::<()>::new(&mut cx, steps.clone(), logged.clone());
        driver.load(&mut cx, None, move |_| {
            Engine::new(design, Arc::new(Factories::new()), steps, logged, "place".into()).with_app_tools()
        });
        finish(&mut driver, &mut cx);
        let (woke, wakes) = mpsc::channel();
        let (handle, inbox) = route_channel(move || {
            let _ = woke.send(());
        });
        let thread = std::thread::spawn(move || RouteWorker::new(inbox, Arc::new(OwnThreads)).run());
        driver.router = Some(Router::new(handle));
        let unit = BoardId(0);
        let placed_inside = |snap: &DesignSnapshot| {
            let board = snap.design.unit(unit).and_then(|u| u.board.as_deref()).expect("the board stays");
            let outside: Vec<_> = PlacementIndex::new(board, None).outside_outline().into_iter().map(|(p, _)| p.path.clone()).collect();
            taken.iter().all(|path| board.placed.iter().any(|p| p.path == *path)) && outside.is_empty()
        };
        let on_spot = |snap: &DesignSnapshot| {
            let board = snap.design.unit(unit).and_then(|u| u.board.as_deref()).expect("the board stays");
            board.placed_of(spot.comp).is_some_and(|p| spot.holds(p))
        };

        let ticket = driver.next_ticket();
        assert!(driver.act(ViewAction::Place { unit, parts: vec![], all: false, free_panel: false }).is_none(), "a place is not a broadcast action");
        assert_eq!(driver.next_ticket(), ticket, "only the host submits the Place press");
        let (name, args) = place_action(driver.snap().map(|s| s.design.as_ref()), unit, &[], false, false).unwrap();
        let (reply, receiver) = mpsc::channel();
        driver.invoke(name.into(), args, false, By::Person, reply);
        finish(&mut driver, &mut cx);
        assert!(driver.router.as_ref().and_then(|router| router.job.as_ref()).is_some(), "the person's Place starts a job");
        show(&mut driver, &mut cx, &wakes, |driver, snap| {
            snap.job.as_ref().and_then(|state| state.end) == Some(JobEnd::Finished) && driver.undo_step().is_some()
        });
        let snap = driver.snap().expect("the placed board is shown");
        let state = snap.job.as_ref().expect("the finished job keeps its state");
        assert!(matches!(state.status.phase, JobPhase::Seed { .. } | JobPhase::Match { .. }) && (state.status.total, state.status.routed) == (4, 4),
            "the job shows as placing the four parts: {:?}", state.status);
        assert!(placed_inside(snap), "the four parts are committed on legal spots inside the outline");
        assert!(on_spot(snap), "the button fixed to a spot is brought onto it, turned as the spot says");
        assert!(snap.proposal.is_none(), "the finished place is committed, not previewed");
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Done { by: By::Person, .. })), "the board's Place answers its own ticket");

        driver.undo();
        settle(&mut driver, &mut cx);
        assert!(!placed_inside(driver.snap().expect("the undone board is shown")), "one undo takes the whole placement back");
        let (reply, receiver) = mpsc::channel();
        driver.invoke("elec.place".into(), vec![("mode".into(), "@place".into())], false, By::Ai { turn: 1, call: 1 }, reply);
        finish(&mut driver, &mut cx);
        let mut answer = None;
        show(&mut driver, &mut cx, &wakes, |_, _| {
            answer = answer.take().or_else(|| receiver.try_recv().ok());
            answer.is_some()
        });
        let Some(Outcome::Done { notes, .. }) = answer else { panic!("the AI's place answers Done") };
        assert!(notes.iter().any(|(key, value)| key == "placed" && value == "4") && notes.iter().any(|(key, _)| key == "wirelength_mm"),
            "the answer counts the parts placed and their wirelength: {notes:?}");
        assert!(notes.iter().all(|(key, _)| key != "panel_held"), "bringing parts holds none of the four: {notes:?}");
        assert!(placed_inside(driver.snap().expect("the AI's placement is shown")), "the AI's place lands the parts as the person's did");
        assert!(on_spot(driver.snap().expect("the AI's placement is shown")), "the AI's place brings the button onto its spot too");

        let (reply, receiver) = mpsc::channel();
        driver.invoke("elec.place".into(), vec![("mode".into(), "@place".into())], false, By::Ai { turn: 1, call: 2 }, reply);
        finish(&mut driver, &mut cx);
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Refused(why)) if why.text.contains("all: true")),
            "with every part placed the call is refused and says how to place again");

        // Every part now stands inside the outline. A whole-board place keeps the panel poses.
        let snap = driver.snap().expect("the AI's placement is shown");
        let whole = board_hash(&snap.design);
        let placed_unit = snap.design.unit(unit).unwrap();
        let board = placed_unit.board.as_deref().unwrap();
        let panel: Vec<_> = placed_unit.circuit.comps.iter().filter(|c| c.part.body.as_ref().is_some_and(|b| !b.ports.is_empty()))
            .map(|c| (c.id, Pose::from(board.placed_of(c.id).unwrap()))).collect();
        let committed = |driver: &Driver<()>| driver.snap().and_then(|snap| snap.design.unit(unit)?.board.clone()).expect("the board stays");
        let (reply, receiver) = mpsc::channel();
        driver.invoke("elec.place".into(), vec![("mode".into(), "@place".into()), ("all".into(), "true".into())], false, By::Ai { turn: 1, call: 3 }, reply);
        finish(&mut driver, &mut cx);
        let mut answer = None;
        show(&mut driver, &mut cx, &wakes, |_, _| {
            answer = answer.take().or_else(|| receiver.try_recv().ok());
            answer.is_some()
        });
        let Some(Outcome::Done { notes, .. }) = answer else { panic!("placing the whole board answers Done") };
        let lamps: Vec<_> = (0..8).map(|i| format!("\"lamps[{i}]\"")).collect();
        let lamps = format!("{{copies: [{}] parts_each: 2 ", lamps.join(" "));
        assert!(notes.iter().any(|(key, rows)| key == "conformed" && rows.contains(&lamps)),
            "the answer includes the eight lamps as one group: {notes:?}");
        let reason = "the panel parts the case is built round stay where they stand; panel: @free places them too";
        let expected = format!("{{count: {} why: \"{reason}\"}}", panel.len());
        assert!(notes.iter().any(|(key, value)| key == "panel_held" && value == &expected),
            "the answer explains how many standing panel parts were held: {notes:?}");
        let placed = committed(&driver);
        for (comp, pose) in panel {
            assert_eq!(Pose::from(placed.placed_of(comp).unwrap()), pose, "held panel part {comp:?}");
        }
        let courts = placed.placed.iter().fold(fab_elec_core::BBox::EMPTY, |b, p| b.union(placed_courtyard(p).bbox()));
        let area = |b: fab_elec_core::BBox| b.width() as f64 * b.height() as f64;
        let share = format!("{:.3}", area(courts) / area(placed.outline_bbox()));
        assert!(notes.iter().any(|(key, value)| key == "box_share" && value == &share),
            "the answer gives the placed courtyard box's share of the outline: {notes:?}");
        driver.undo();
        settle(&mut driver, &mut cx);
        assert_eq!(board_hash(&driver.snap().expect("the undone board is shown").design), whole, "one undo takes the whole placement back");

        let before = committed(&driver);
        let hand = before.tracks.iter().find(|t| t.id == id).cloned().expect("the hand-drawn track is on the board");
        let led_at = before.placed_item(led_id).map(|p| (p.at, p.rot, p.side));
        let mut args = vec![("mode".into(), "@place".into()), ("parts".into(), "lamps[3].d".into())];
        let (reply, receiver) = mpsc::channel();
        driver.invoke("elec.place".into(), args.clone(), false, By::Ai { turn: 1, call: 4 }, reply);
        finish(&mut driver, &mut cx);
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Refused(why)) if why.text.contains("held: 1") && why.text.contains(reason)),
            "naming just the LED explains the hold and how to free it");
        args.push(("panel".into(), "@free".into()));
        let (reply, receiver) = mpsc::channel();
        driver.invoke("elec.place".into(), args, false, By::Ai { turn: 1, call: 5 }, reply);
        finish(&mut driver, &mut cx);
        let mut answer = None;
        show(&mut driver, &mut cx, &wakes, |_, _| {
            answer = answer.take().or_else(|| receiver.try_recv().ok());
            answer.is_some()
        });
        let Some(Outcome::Done { notes, .. }) = answer else { panic!("placing the LED again answers Done") };
        let counted = format!("[{{part: \"{led_ref}\" tracks: 1 vias: 0}}]");
        assert!(notes.iter().any(|(key, value)| key == "hand_drawn_removed" && *value == counted),
            "the answer counts the hand-drawn track against the LED: {notes:?}");
        let placed = committed(&driver);
        assert!(placed.placed_item(led_id).map(|p| (p.at, p.rot, p.side)) != led_at && placed.tracks.iter().all(|t| t.id != id),
            "the LED moves and its hand-drawn track goes");
        driver.undo();
        settle(&mut driver, &mut cx);
        let back = committed(&driver);
        assert_eq!(back.tracks.iter().find(|t| t.id == id), Some(&hand), "one undo brings the hand-drawn track back as it was");
        assert_eq!(back.placed_item(led_id).map(|p| (p.at, p.rot, p.side)), led_at, "and the LED where it stood");
        drop(driver);
        thread.join().expect("the route worker exits when its channel closes");
    }

    #[test]
    fn a_place_answer_explains_the_choice_when_the_leds_have_seats() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        let design = ledctl();
        let unit = &design.units[0];
        let parts = unit.board.as_ref().unwrap().placed.iter()
            .filter(|p| unit.circuit.comps[p.comp.ix()].part.kind != fab_elec_parts::PartKind::Led)
            .map(|p| p.path.as_str()).collect::<Vec<_>>().join(" ");
        let mut driver = Driver::<()>::new(&mut cx, steps.clone(), logged.clone());
        driver.load(&mut cx, None, move |_| Engine::new(design, Arc::new(Factories::new()), steps, logged, "place".into()).with_app_tools());
        finish(&mut driver, &mut cx);
        let (woke, wakes) = mpsc::channel();
        let (handle, inbox) = route_channel(move || { let _ = woke.send(()); });
        let thread = std::thread::spawn(move || RouteWorker::new(inbox, Arc::new(OwnThreads)).run());
        driver.router = Some(Router::new(handle));
        let (reply, receiver) = mpsc::channel();
        let args = vec![("mode".into(), "@place".into()), ("panel".into(), "@free".into()), ("parts".into(), parts)];
        driver.invoke("elec.place".into(), args, false, By::Ai { turn: 1, call: 1 }, reply);
        finish(&mut driver, &mut cx);
        let mut answer = None;
        show(&mut driver, &mut cx, &wakes, |_, _| {
            answer = receiver.try_recv().ok();
            answer.is_some()
        });
        let Some(Outcome::Done { notes, .. }) = answer else { panic!("placing with standing LEDs answers Done") };
        let line = &notes.iter().find(|(key, _)| key == "seat_guard").expect("the answer explains the seat guard").1;
        assert!((line.contains("seats kept:") || line.contains("plain kept:")) && line.contains(" mm against "), "{line}");
        drop(driver);
        thread.join().unwrap();
    }

    /// Why: stopping a job commits exactly its shown partial while the worker is still held.
    #[test]
    fn stopping_a_route_job_keeps_what_it_has_routed() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let Held { mut driver, wakes, held, release, thread, id, receiver, before } = held_route(&mut cx);
        held.recv_timeout(HANG).expect("the real job reaches its first partial");
        show(&mut driver, &mut cx, &wakes, |_, snap| {
            snap.proposal.as_deref().is_some_and(|answer| matches!(answer.source, ProposalSource::Job { job, .. } if job == id))
        });
        let snap = driver.snap().expect("the held partial is shown before stopping");
        let preview = board_hash(snap.preview.as_deref().expect("the held partial has a preview design"));
        assert_ne!(preview, before, "the held partial has routed copper to keep");

        assert!(driver.act(ViewAction::StopRoute(id)).is_none(), "stopping a route is not a broadcast action");
        settle(&mut driver, &mut cx);
        let snap = driver.snap().expect("the stopped route keeps a shown snapshot");
        assert_eq!(board_hash(&snap.design), preview, "stopping commits exactly the shown partial's board");
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Done { .. })), "stopping after a partial answers the caller with Done");
        assert!(receiver.try_recv().is_err(), "stopping after a partial answers the caller exactly once");
        assert_eq!(snap.job.as_ref().and_then(|state| state.end), Some(JobEnd::Stopped), "stopping is shown in the job status");
        assert!(snap.proposal.is_none(), "stopping clears the committed proposal from the preview");
        assert_eq!(snap.preview_tag, None, "stopping clears the committed preview's tag");
        assert!(snap.preview.is_none(), "stopping clears the committed preview design");
        assert!(driver.router.as_ref().and_then(|router| router.job.as_ref()).is_some_and(|job| job.id == id && job.ended),
            "stopping marks the running job ended before the worker is released");
        drop(release);
        drop(driver);
        thread.join().expect("the route worker exits when its channel closes");
    }

    /// Why: an edit keeps the real worker's partial as one undo step, then previews and commits
    /// on top of it as a separate undo step.
    #[test]
    fn an_edit_during_a_route_job_lands_on_the_kept_route() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let Held { mut driver, wakes, held, release, thread, id, receiver, before } = held_route(&mut cx);
        held.recv_timeout(HANG).expect("the real job reaches its first partial");
        show(&mut driver, &mut cx, &wakes, |_, snap| {
            snap.proposal.as_deref().is_some_and(|answer| matches!(answer.source, ProposalSource::Job { job, .. } if job == id))
        });
        let snap = driver.snap().expect("the held partial is shown before the edit");
        let preview = snap.preview.as_deref().expect("the held partial has a preview design");
        let kept = board_hash(preview);
        let item = preview.unit(BoardId(0)).and_then(|unit| unit.board.as_ref())
            .expect("the shown partial keeps unit 0's board").vias[0].id;
        assert_ne!(kept, before, "the held partial has routed copper for the edit to keep");

        assert!(driver.act(ViewAction::Preview {
            notes: vec![], tag: 501,
            ops: vec![DesignOp::Board { unit: BoardId(0), op: BoardOp::Move { item, by: P::mm(0.1, 0.0) } }],
            place: vec![], label: "drag".into(),
        }).is_none(), "the drag preview is handled by the driver");
        settle(&mut driver, &mut cx);
        let route_step = driver.undo_step();
        let snap = driver.snap().expect("the edit shows its preview over the kept route");
        assert_eq!(board_hash(&snap.design), kept, "the edit first commits exactly the shown route partial");
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Done { seq, .. }) if Some(seq) == route_step),
            "keeping the route for an edit answers its caller with the route's committed step");
        assert!(receiver.try_recv().is_err(), "keeping the route for an edit answers its caller exactly once");
        assert_eq!(snap.job.as_ref().and_then(|state| state.end), Some(JobEnd::Stopped),
            "an edit marks the running route job stopped");
        assert_eq!(snap.preview_tag, Some(501), "the drag's own tag is shown over the kept route");
        let dragged = board_hash(snap.preview.as_deref().expect("the drag is shown as a preview over the kept route"));
        assert_ne!(dragged, kept, "the drag preview moves the via on the kept route");

        assert!(driver.act(ViewAction::CommitPreview).is_none(), "the drag's commit is handled by the driver");
        settle(&mut driver, &mut cx);
        let drag_step = driver.undo_step();
        assert_ne!(drag_step, route_step, "committing the drag makes a separate undo step after the route");
        assert_eq!(board_hash(&driver.snap().expect("the committed drag is shown").design), dragged,
            "committing the drag keeps exactly its shown preview board");
        driver.undo();
        settle(&mut driver, &mut cx);
        assert_eq!(board_hash(&driver.snap().expect("undoing the drag keeps a shown snapshot").design), kept,
            "undoing once removes only the drag and restores the kept route's board");
        assert_eq!(driver.undo_step(), route_step, "the kept route remains undoable after undoing the drag");
        assert_eq!(driver.redo_step(), drag_step, "the undone drag is the redo step");
        drop(release);
        drop(driver);
        thread.join().expect("the route worker exits when its channel closes");
    }

    /// Why: undo during a job must first log its partial and answer the caller, then undo that
    /// route so redo can restore exactly the copper the person saw.
    #[test]
    fn undo_during_a_route_job_keeps_then_undoes_the_route() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let Held { mut driver, wakes, held, release, thread, id, receiver, before } = held_route(&mut cx);
        held.recv_timeout(HANG).expect("the real job reaches its first partial");
        show(&mut driver, &mut cx, &wakes, |_, snap| {
            snap.proposal.as_deref().is_some_and(|answer| matches!(answer.source, ProposalSource::Job { job, .. } if job == id))
        });
        let snap = driver.snap().expect("the held partial is shown before undo");
        let kept = board_hash(snap.preview.as_deref().expect("the held partial has a preview design"));
        assert_ne!(kept, before, "the held partial has routed copper to keep before undo");

        driver.undo();
        settle(&mut driver, &mut cx);
        let route_step = driver.redo_step();
        let snap = driver.snap().expect("undoing the kept route leaves a shown snapshot");
        assert_eq!(board_hash(&snap.design), before, "undo restores the board from before the route job");
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Done { seq, .. }) if Some(seq) == route_step),
            "undo first commits the route and answers its caller with the now redoable step");
        assert!(receiver.try_recv().is_err(), "undoing a running route answers its caller exactly once");
        assert_eq!(driver.undo_step(), None, "the kept route was the only undo step and has been undone");
        assert_eq!(snap.job.as_ref().and_then(|state| state.end), Some(JobEnd::Stopped),
            "undo marks the running route job stopped");
        driver.redo();
        settle(&mut driver, &mut cx);
        assert_eq!(board_hash(&driver.snap().expect("redoing the route keeps a shown snapshot").design), kept,
            "redo restores exactly the route partial shown before undo");
        assert_eq!(driver.undo_step(), route_step, "redo restores the kept route's original undo step");
        assert_eq!(driver.redo_step(), None, "redo consumes the undone route step");
        drop(release);
        drop(driver);
        thread.join().expect("the route worker exits when its channel closes");
    }

    /// Why: discarding a partial refuses its caller and clears the preview without changing the board.
    #[test]
    fn discarding_a_route_job_commits_nothing() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let Held { mut driver, wakes, held, release, thread, id, receiver, before } = held_route(&mut cx);
        held.recv_timeout(HANG).expect("the real job reaches its first partial");
        show(&mut driver, &mut cx, &wakes, |_, snap| {
            snap.proposal.as_deref().is_some_and(|answer| matches!(answer.source, ProposalSource::Job { job, .. } if job == id))
        });

        assert!(driver.act(ViewAction::DiscardRoute(id)).is_none(), "discarding a route is not a broadcast action");
        settle(&mut driver, &mut cx);
        let snap = driver.snap().expect("discarding keeps the committed snapshot");
        assert_eq!(driver.undo_step(), None, "discarding a route creates no undo step");
        assert!(matches!(receiver.try_recv(), Ok(Outcome::Refused(refusal))
            if refusal.text == "the route was discarded" && refusal.sample == Some(id.tag())),
            "discarding answers the caller with the reason under the job's tag");
        assert!(receiver.try_recv().is_err(), "discarding answers the caller exactly once");
        assert!(snap.job.is_none(), "discarding removes the shown job state");
        assert_eq!(snap.preview_tag, None, "discarding removes the job's preview tag");
        assert!(snap.preview.is_none(), "discarding removes the job's preview design");
        assert_eq!(board_hash(&snap.design), before, "discarding leaves the committed board unchanged");
        assert!(driver.router.as_ref().expect("discarding retains the router").job.is_none(),
            "discarding forgets the job so later worker events are ignored");
        drop(release);
        drop(driver);
        thread.join().expect("the route worker exits when its channel closes");
    }

    /// Why: the production Driver must start its worker lazily and deliver the committed base
    /// before the first gesture, whether the worker answers with a refusal or a proposal.
    #[test]
    fn a_route_gesture_reaches_the_real_worker_after_its_base() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        let mut driver = Driver::<()>::new(&mut cx, steps.clone(), logged.clone());
        driver.load(&mut cx, None, move |_| {
            Engine::new(ledctl(), Arc::new(Factories::new()), steps, logged, "demo".into())
        });
        finish(&mut driver, &mut cx);
        driver.show(&mut cx);
        assert!(driver.router.is_none(), "a person who has not routed has no route worker");
        let unit = BoardId(0);
        let generation = driver.snap().expect("the real worker's fixture is loaded").design.unit(unit)
            .expect("the real worker's fixture has unit 0").board.as_ref().expect("unit 0 has a board").generation;
        driver.act(ViewAction::Gesture(GestureSample {
            id: GestureId(41), seq: 1,
            gesture: Gesture::new(GestureKind::Route, unit, generation, Anchor::Free, P::ZERO, LayerId::F_CU),
        }));
        let deadline = Instant::now() + HANG;
        loop {
            let _ = driver.event(&mut cx, &mut |_, _, _| false);
            let snapshot = driver.snap().expect("the worker's events preserve the loaded snapshot");
            if let Some(refusal) = snapshot.last_refusal.as_ref().filter(|refusal| refusal.sample == Some(41)) {
                assert_ne!(refusal.text, "no board for this unit yet", "the real worker receives the base before its first sample");
                break;
            }
            if snapshot.preview_tag == Some(41) {
                break;
            }
            assert!(Instant::now() < deadline, "the real worker answers the gesture through the shared host loop");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

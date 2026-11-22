//! The tool host core: what runs a product's AI control API tools in the
//! window, for the chat, the in-process agents and the remote clients.
//!
//! A product implements [`ToolProduct`]: it registers its handlers (each
//! product its own tools on the shared catalogue, `makepad_studio_api`),
//! starts its workers, and keeps what its waits need in its
//! [`ToolProduct::State`]. The core keeps the rest: the registry, the ids,
//! the calls still waiting, the results not taken yet and the change log.
//!
//! - A call is checked against the catalogue, then its handler runs on the
//!   UI thread: it answers at once ([`Outcome::Done`]) or names what it
//!   waits for ([`Outcome::Wait`]); the product's pump finishes the waits.
//! - [`Worker`]: one long-lived thread fed by a bounded channel, for the
//!   product's slow work (parsing, the library, app runs); it wakes the UI
//!   after each reply. Its jobs whose replies the UI has not taken are the
//!   app's loads in flight (`Cx::pending_async_loads`, "tool job"), so a
//!   wait on a call reads the app as at work while a worker, or a process
//!   it started, works for it.
//!
//! Main-loop cost: a call is a table lookup and its handler; the pump is
//! the product's (bounded channels, a handful of waits).

use makepad_asset_data::tree::TreeDraft;
use makepad_asset_data::AssetRevisionRef;
use makepad_studio_api::document::Edit;
use makepad_studio_api::{CallResult, ChangeLog, Registry, ToolError, ToolSpec, TOOLS};
use makepad_strict_json::Value as Json;
use makepad_widgets::makepad_platform::thread::SignalToUI;
use makepad_widgets::*;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

/// Jobs each worker queues before the host holds them back.
const QUEUE: usize = 4;
/// Replies a product reads per worker per pump.
pub const REPLIES_PER_PUMP: usize = 4;

/// Jobs sent to this process's workers whose replies the UI has not taken.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// The workers' jobs in flight, as the app's loads ("tool job").
fn tool_jobs(_: &Cx) -> usize {
    IN_FLIGHT.load(Ordering::Relaxed)
}

/// One long-lived thread fed by a bounded channel; it wakes the UI after
/// each reply.
pub struct Worker<J, D> {
    jobs: Option<SyncSender<J>>,
    done: Receiver<D>,
    held: VecDeque<J>,
    /// Its jobs sent (held ones too) whose replies are not taken yet.
    in_flight: usize,
}

impl<J: Send + 'static, D: Send + 'static> Worker<J, D> {
    pub fn new(cx: &mut Cx, name: &'static str, mut run: impl FnMut(J) -> D + Send + 'static) -> Result<Self, String> {
        let (job_tx, job_rx) = sync_channel::<J>(QUEUE);
        let (done_tx, done) = sync_channel::<D>(QUEUE);
        let options = ThreadOptions { name: Some(Arc::from(name)), stack_size: Some(8 * 1024 * 1024), priority: CxThreadPriority::UserInitiated };
        let handle = cx.thread_spawner().spawn_worker(options, move || {
            while let Ok(job) = job_rx.recv() {
                // The UI takes a few replies per pump; waiting here is this
                // thread's own time.
                if done_tx.send(run(job)).is_err() {
                    break;
                }
                SignalToUI::set_ui_signal();
            }
        }).map_err(|error| format!("{name}: {error:?}"))?;
        handle.detach();
        cx.register_async_load_probe("tool job", tool_jobs);
        Ok(Self { jobs: Some(job_tx), done, held: VecDeque::new(), in_flight: 0 })
    }

    pub fn send(&mut self, job: J) {
        self.in_flight += 1;
        IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
        self.held.push_back(job);
        self.flush();
    }

    pub fn flush(&mut self) {
        let Some(jobs) = &self.jobs else { return };
        while let Some(job) = self.held.pop_front() {
            match jobs.try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(job)) | Err(TrySendError::Disconnected(job)) => {
                    self.held.push_front(job);
                    break;
                }
            }
        }
    }

    pub fn try_recv(&mut self) -> Option<D> {
        let done = self.done.try_recv().ok()?;
        self.in_flight -= 1;
        IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
        Some(done)
    }
}

impl<J, D> Drop for Worker<J, D> {
    /// A worker dropped with jobs in flight takes them out of the loads: their replies never come.
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(self.in_flight, Ordering::Relaxed);
    }
}

/// What a handler did with a call.
pub enum Outcome<W> {
    Done(CallResult),
    Wait(W),
}

/// A call still waiting, and for what.
pub struct Waiting<W> {
    /// The call's id (0: the window's own change, no one waits for it).
    pub reply: u64,
    pub tool: &'static str,
    /// Who called (`chat`, `generate`, `page`), for the change log.
    pub origin: String,
    pub wait: W,
}

/// A tool's implementation.
pub type Handler<P> = fn(&mut Ctx<P>, &Json) -> Outcome<<P as ToolProduct>::Wait>;

/// What a handler works on: the product's window, the platform and the host.
pub struct Ctx<'a, P: ToolProduct> {
    pub app: &'a mut P::App,
    pub cx: &'a mut Cx,
    pub host: &'a mut ToolHost<P>,
    /// Who called (`chat`, `generate`), for the page's history.
    pub origin: &'a str,
}

/// A product's tools (a marker type the product keeps private, so its
/// waits and state stay private too).
pub trait ToolProduct: Sized + 'static {
    /// The product's window, which the handlers work on.
    type App;
    /// What a call can wait for.
    type Wait;
    /// What the product's waits need (its workers, what it has seen).
    type State: Default;
    /// The tools this window implements.
    fn register(registry: &mut Registry<Handler<Self>>);
    /// Start the product's workers (once, on the first call or pump).
    fn start(host: &mut ToolHost<Self>, cx: &mut Cx);
    /// Run a checked call. A product may take a call over first (open the
    /// document it names, say); the default runs its handler.
    fn run(ctx: &mut Ctx<Self>, tool: &'static str, handler: Handler<Self>, args: Json) -> Outcome<Self::Wait> {
        let _ = tool;
        handler(ctx, &args)
    }
}

/// The host: the registry, the calls waiting, the results and the log, and
/// the product's own state.
pub struct ToolHost<P: ToolProduct> {
    pub started: bool,
    pub registry: Registry<Handler<P>>,
    /// Why a worker did not start.
    pub error: Option<String>,
    pub waiting: Vec<Waiting<P::Wait>>,
    /// The last id handed out ([`Self::next`]; a wait that holds a borrow
    /// of `waiting` bumps it in place).
    pub ticket: u64,
    results: HashMap<u64, CallResult>,
    pub log: ChangeLog,
    pub state: P::State,
}

impl<P: ToolProduct> Default for ToolHost<P> {
    fn default() -> Self {
        Self { started: false, registry: Registry::new(), error: None, waiting: Vec::new(), ticket: 0, results: HashMap::new(), log: ChangeLog::default(), state: P::State::default() }
    }
}

impl<P: ToolProduct> ToolHost<P> {
    /// A fresh id (a call's, a worker job's).
    pub fn next(&mut self) -> u64 {
        self.ticket += 1;
        self.ticket
    }

    /// Register the product's tools and start its workers, once.
    pub fn ensure_started(&mut self, cx: &mut Cx) {
        if self.started {
            return;
        }
        self.started = true;
        P::register(&mut self.registry);
        P::start(self, cx);
    }

    /// Keep a call's result until its caller takes it.
    pub fn finish(&mut self, reply: u64, result: CallResult) {
        if reply != 0 {
            self.results.insert(reply, result);
        }
    }

    /// The result of call `id`, once (none while it runs).
    pub fn take_result(&mut self, id: u64) -> Option<CallResult> {
        self.results.remove(&id)
    }

    /// Call tool `name`: its id; the result arrives in [`Self::take_result`]
    /// (at once for most tools, later for the ones that wait).
    pub fn call(&mut self, app: &mut P::App, cx: &mut Cx, name: &str, args: Json, origin: &str) -> u64 {
        self.ensure_started(cx);
        let id = self.next();
        self.dispatch(app, cx, name, args, origin, id);
        id
    }

    /// Run call `reply` of tool `name`.
    pub fn dispatch(&mut self, app: &mut P::App, cx: &mut Cx, name: &str, args: Json, origin: &str, reply: u64) {
        let checked = self.registry.check(name, args).map(|checked| (*checked.handler, checked.spec.name, checked.args));
        let (handler, tool, args): (Handler<P>, &'static str, Json) = match checked {
            Ok(checked) => checked,
            Err(error) => return self.finish(reply, Err(error)),
        };
        let outcome = P::run(&mut Ctx { app, cx, host: self, origin }, tool, handler, args);
        self.settle(reply, tool, origin, outcome);
    }

    /// File a handler's outcome: its result, or the call among the waiting.
    pub fn settle(&mut self, reply: u64, tool: &'static str, origin: &str, outcome: Outcome<P::Wait>) {
        match outcome {
            Outcome::Done(result) => self.finish(reply, result),
            Outcome::Wait(wait) => self.waiting.push(Waiting { reply, tool, origin: origin.to_string(), wait }),
        }
    }

    /// The catalogue's tools with whether this window implements each (for
    /// a model's tool list).
    pub fn catalogue() -> Vec<(&'static ToolSpec, bool)> {
        let mut registry: Registry<Handler<P>> = Registry::new();
        P::register(&mut registry);
        TOOLS.iter().map(|spec| (spec, registry.available(spec.name))).collect()
    }
}

/// A call that is refused: what it got wrong.
pub fn fail<W>(error: ToolError) -> Outcome<W> {
    Outcome::Done(Err(error))
}

// ------------------------------------------------------------ documents

/// A change to the open document's working tree (or to its one file when
/// it has no tree): what the document tools and the pages' own file panes
/// ask for, applied by the product.
#[derive(Clone, Debug)]
pub enum Change {
    /// A library asset added to the tree at `path`.
    Import { reference: AssetRevisionRef, path: String },
    /// A file's whole text; `expected`: the text it must still have.
    Write { path: String, text: String, expected: Option<String> },
    Patch { path: String, edits: Vec<Edit> },
    /// A new file, a copy of `from` or with `text`.
    Create { path: String, from: Option<String>, text: Option<String> },
    Rename { from: String, to: String },
    Delete { path: String },
}

/// The open document's tree: the file the tools act on, the tree's
/// revision in the window and its library head.
pub struct OpenTree<'a> {
    pub tree: &'a Arc<TreeDraft>,
    pub path: &'a str,
    pub revision: u64,
    pub head: Option<AssetRevisionRef>,
}

/// The open document's working history, for `doc_history`.
pub enum History {
    /// Each undo step's root source and tree, oldest first, then the
    /// working state.
    Steps(Vec<(String, Option<Arc<TreeDraft>>)>),
    /// The product's own answer (a document without a tree).
    Answer(Json),
}

/// The documents a product's window opens, as the document tools see
/// them: the open tree (or one root file), its state, its changes and its
/// undo. The product applies the changes (on its own worker, installed
/// when the page is idle) and answers through its own waits.
pub trait Documents: ToolProduct {
    /// The open document's tree, when it has one.
    fn open_tree(app: &Self::App) -> Option<OpenTree<'_>>;
    /// The open document's file name when it has no tree (`cut.splash`).
    fn root_path(app: &Self::App) -> String;
    /// The open document's source and revision (the root file's, when it
    /// has no tree), or what to open first.
    fn doc_state(app: &Self::App) -> Result<(String, u64), ToolError>;
    /// What `doc_read` adds about the document (a score's summary).
    fn summary(app: &Self::App) -> Option<Json> {
        let _ = app;
        None
    }
    /// For `tree_list`: the slides that show each file, by path.
    fn file_uses(app: &Self::App) -> HashMap<String, Vec<Json>> {
        let _ = app;
        HashMap::new()
    }
    /// For `tree_list`: what is selected in the page.
    fn selection(app: &Self::App) -> Json {
        let _ = app;
        Json::Null
    }
    /// A file with an unapplied source draft (not moved or deleted).
    fn has_file_draft(app: &Self::App, path: &str) -> bool {
        let _ = (app, path);
        false
    }
    /// The history of the document, or of its file `path`.
    fn history(app: &Self::App, path: Option<&str>) -> Result<History, ToolError>;
    /// Apply `change` to the open document: one undo step, logged, answered
    /// with the change.
    fn change(ctx: &mut Ctx<Self>, args: &Json, change: Change) -> Outcome<Self::Wait>;
    fn undo(ctx: &mut Ctx<Self>, redo: bool) -> Outcome<Self::Wait>;
}

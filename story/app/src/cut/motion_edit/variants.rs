//! Variants: "give me 5 colour options for the axes" on the open graphic.
//!
//! One ask, scoped to what is selected (a code row, a picked value) or to
//! what the ask names, goes to a fast model with only the code it is about
//! (the selected region, the lines that mention the ask's words, and the
//! constants those lines use). The model answers with N small patches,
//! each a few `-` lines of the current code and the `+` lines that replace
//! them; they stream in, and each is checked against the code and loaded
//! as its own Motion document on this module's workers the moment it is
//! complete, then evaluated at the playhead there. The strip under the
//! viewer shows each as its frame (`variant_strip.rs`); the pointer on one
//! shows it in the viewer (its document drawn in place of the graphic's,
//! nothing changed); a click applies it to the code as one undo step, and
//! its document, already loaded, is shown at once. Several variants with
//! patches that do not overlap apply together (a mix). "More like this"
//! asks again from one.
//!
//! A variant is plain code: applying one is an ordinary edit of the
//! document (and of the modules it imports), seen in the Source view's
//! history like any other.
//!
//! The same is the control API's `variants`, `apply_variant`,
//! `hover_variant` and `variants_status` (and `graphic_open`), so an agent
//! can ask, look and apply headless.
//!
//! Main-loop cost: per pump, a poll of each writer (draining its stream),
//! parsing of only a block that just closed (linear in the block), a drain
//! of the workers' answers; a variant's patch is located with a scan of
//! its file. Loading and evaluating documents runs on the workers; the UI
//! thread evaluates a variant only while it is hovered (as it does the
//! graphic itself).

use makepad_edit_values::ValueKind;
use super::super::VideoCut;
use super::checker::DocKind;
use super::session::MotionSession;
use super::variant_strip::{CardInfo, MotionVariantThumbs, VariantAction};
use makepad_motion::{patch, MotionDoc, MotionFrame};
use makepad_story_video_agent::{Backend, Brain, BrainEvent};
use makepad_strict_json::{self as json, Value as Json};
use makepad_widgets::*;
use std::collections::{BTreeSet, VecDeque};
use std::ops::Range;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    let StripTool = StageButton{width: Fit height: 22 margin: 0 padding: Inset{left: 8 right: 8} spacing: 6
        draw_text +: {text_style: theme.font_regular{font_size: 9}}}

    // The variant bar: the variants the chat asked for, the graphic as it
    // is first; a click previews one, Choose applies it, Cancel goes back.
    mod.widgets.MotionVariantStrip = View{width: Fill height: Fit flow: Right visible: false spacing: 10
        padding: Inset{left: 12 right: 12 top: 8 bottom: 8} align: Align{y: 0.5}
        v_thumbs := MotionVariantThumbs{}
        View{width: Fit height: Fit flow: Down spacing: 6
            Tip{text: "Apply the variant shown (one undo step) and close" v_choose := StripTool{text: "Choose"}}
            Tip{text: "Back to the graphic as it was, and close" v_cancel := StripTool{text: "Cancel"}}
            v_status := Label{width: 90 text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 7.5}}}
        }
    }
}

/// The model variants are asked of unless told otherwise
/// (`STAGE_VARIANTS_MODEL`, or the API's `model`): `claude-cli:<id>`,
/// `claude-api:<id>`, `hub` or `hub:<model>`.
pub const DEFAULT_MODEL: &str = "claude-cli:claude-sonnet-5-5";
/// How many variants an ask that names no number makes.
const DEFAULT_N: usize = 4;
/// Workers loading and evaluating variants.
const WORKERS: usize = 3;
/// How hard the model thinks over an ask (the CLI's `--effort`).
const EFFORT: &str = "low";
/// Threads running the model calls (one call each at a time).
const WRITERS: usize = 3;
/// Lines of code an ask by name sends at most (the best matches first);
/// more are left out and the prompt says so.
const CONTEXT_LINES: usize = 240;
/// Lines around each line that matches the ask.
const AROUND: usize = 3;

const SYSTEM: &str = "You write variants of one aspect of a motion graphic. The graphic is a document in Splash, a small \
scripting language: objects `Type{field: value}`, `let` bindings, functions `fn(t) {...}`, lists `[...]`, colours `#rrggbb`, \
names `@name`, and imported modules used as `name.member`. The person asks for N alternatives of something (colours, timing, \
text, sizes, motion), or for one change made now. You answer with exactly the blocks asked for (one for a change), each a \
small patch against the code shown, and nothing else.

Format of each block, exactly:
## <number> <Label>: <what it is, at most 8 words>
@@ <file name as shown>
-<an existing line, copied exactly>
+<its replacement>

Rules:
- A block changes only what the ask is about; everything else stays as it is.
- `-` lines are consecutive lines copied character for character from the code shown (with their indentation), enough of \
them to be unique in that file; the `+` lines replace them. A block may have several such hunks, each under its own `@@` line.
- Touch the fewest lines possible; never rewrite a whole function.
- The variants differ clearly from each other and from the current code; each is a finished, tasteful choice a designer \
would show. Keep the code valid Splash.
- Colours as #rrggbb literals; times and sizes in the units the code already uses.
- Labels are one or two evocative words.
- No prose before, between or after the blocks, and no code fences.
";

// ------------------------------------------------------------------ workers

enum Job {
    /// A variant's texts loaded, and its frame at `t`.
    Load { id: u64, source: String, modules: Vec<(String, String, String)>, t: f64 },
    /// A loaded variant's frame at `t`.
    Eval { id: u64, doc: Arc<MotionDoc>, t: f64 },
}

struct Loaded {
    id: u64,
    t: f64,
    doc: Option<Arc<MotionDoc>>,
    frame: Result<MotionFrame, String>,
    ms: f64,
}

/// A few long-lived threads sharing one bounded queue.
struct Loader {
    jobs: SyncSender<Job>,
    done: Receiver<Loaded>,
    held: VecDeque<Job>,
}

impl Loader {
    fn new(cx: &mut Cx) -> Result<Self, String> {
        let (job_tx, job_rx) = sync_channel::<Job>(WORKERS * 2);
        let (done_tx, done_rx) = sync_channel::<Loaded>(64);
        let job_rx = Arc::new(Mutex::new(job_rx));
        for k in 0..WORKERS {
            let (jobs, done) = (job_rx.clone(), done_tx.clone());
            let options = ThreadOptions { name: Some(Arc::from(format!("stage-variants-{k}").as_str())), stack_size: Some(8 * 1024 * 1024), priority: CxThreadPriority::UserInitiated };
            let handle = cx.thread_spawner().spawn_worker(options, move || loop {
                let job = {
                    let Ok(jobs) = jobs.lock() else { return };
                    match jobs.recv() {
                        Ok(job) => job,
                        Err(_) => return,
                    }
                };
                if done.send(run(job)).is_err() {
                    return;
                }
            });
            match handle {
                Ok(handle) => handle.detach(),
                Err(error) => return Err(format!("variant workers unavailable: {error:?}")),
            }
        }
        Ok(Self { jobs: job_tx, done: done_rx, held: VecDeque::new() })
    }

    fn send(&mut self, job: Job) {
        self.held.push_back(job);
        self.flush();
    }

    fn flush(&mut self) {
        while let Some(job) = self.held.pop_front() {
            match self.jobs.try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(job)) | Err(TrySendError::Disconnected(job)) => {
                    self.held.push_front(job);
                    break;
                }
            }
        }
    }
}

fn run(job: Job) -> Loaded {
    let started = std::time::Instant::now();
    match job {
        Job::Load { id, source, modules, t } => {
            let (problems, doc, _) = super::checker::check(DocKind::Motion, &source, modules, None);
            let frame = match (&doc, problems.first()) {
                (Some(doc), _) => Ok(doc.eval(t)),
                (None, Some(problem)) => Err(format!("line {}: {}", problem.line, problem.message)),
                (None, None) => Err("it does not load".into()),
            };
            Loaded { id, t, doc, frame, ms: started.elapsed().as_secs_f64() * 1000.0 }
        }
        Job::Eval { id, doc, t } => {
            let frame = doc.eval(t);
            Loaded { id, t, doc: None, frame: Ok(frame), ms: started.elapsed().as_secs_f64() * 1000.0 }
        }
    }
}

// ------------------------------------------------------------------ writers

/// How a conversation's model is made: which, and its system text (the
/// patch rules and the guide sections the scene needs).
#[derive(Clone)]
pub(crate) struct Make {
    pub(crate) backend: Backend,
    pub(crate) system: Arc<str>,
}

/// Work for a writer thread. Each thread keeps the model conversations
/// (brains) of the sessions placed on it, so a session's conversation, and
/// with the Claude CLI its one process, lives across turns; starting,
/// polling and ending them never holds the UI thread.
enum WriteJob {
    /// One turn of a session's conversation (`fresh`: a new conversation).
    Turn { session: u64, fresh: bool, make: Make, text: String, key: u64, done: SyncSender<Wrote>, cancel: Arc<AtomicBool>, busy: Arc<AtomicBool> },
    /// The session is over: its conversation ends.
    End { session: u64 },
}

enum Wrote {
    Text(u64, String),
    Done(u64),
    Failed(u64, String),
}

type Brains = std::collections::HashMap<u64, Box<dyn Brain>>;

/// Long-lived threads, one queue each (a session stays on its thread).
struct Writers {
    jobs: Vec<SyncSender<WriteJob>>,
    done: Receiver<Wrote>,
    done_tx: SyncSender<Wrote>,
    held: VecDeque<(usize, WriteJob)>,
}

impl Writers {
    fn new(cx: &mut Cx) -> Result<Self, String> {
        let (done_tx, done_rx) = sync_channel::<Wrote>(256);
        let mut jobs = Vec::new();
        for k in 0..WRITERS {
            let (job_tx, job_rx) = sync_channel::<WriteJob>(8);
            let options = ThreadOptions { name: Some(Arc::from(format!("stage-variant-writer-{k}").as_str())), stack_size: None, priority: CxThreadPriority::UserInitiated };
            let handle = cx.thread_spawner().spawn_worker(options, move || {
                let mut brains = Brains::new();
                for job in job_rx {
                    write(job, &mut brains);
                }
            });
            match handle {
                Ok(handle) => handle.detach(),
                Err(error) => return Err(format!("variant writers unavailable: {error:?}")),
            }
            jobs.push(job_tx);
        }
        Ok(Self { jobs, done: done_rx, done_tx, held: VecDeque::new() })
    }

    fn send(&mut self, slot: usize, job: WriteJob) {
        self.held.push_back((slot % self.jobs.len(), job));
        self.flush();
    }

    fn flush(&mut self) {
        while let Some((slot, job)) = self.held.pop_front() {
            match self.jobs[slot].try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(job)) | Err(TrySendError::Disconnected(job)) => {
                    self.held.push_front((slot, job));
                    break;
                }
            }
        }
    }
}

/// A conversation's model, made as `make` says.
pub(crate) fn start_brain(make: &Make) -> Result<Box<dyn Brain>, String> {
    if let Some(reason) = make.backend.unavailable() {
        return Err(reason);
    }
    match &make.backend {
        // Through the CLI: one live process per conversation, thinking
        // little (small answers wanted fast).
        Backend::ClaudeCode { model } => {
            let provider = makepad_ai_hub::providers::claude::ClaudeCodeChatProvider::new(Some(model.clone())).live().effort(EFFORT);
            Ok(Box::new(makepad_story_video_agent::brain::TextBrain::new(provider, make.backend.label(), make.backend.vision(), &make.system, &[])))
        }
        _ => make.backend.start(&make.system, &[]),
    }
}

/// One job on a writer thread.
fn write(job: WriteJob, brains: &mut Brains) {
    let (session, fresh, make, text, key, done, cancel, busy) = match job {
        WriteJob::End { session } => {
            brains.remove(&session);
            return;
        }
        WriteJob::Turn { session, fresh, make, text, key, done, cancel, busy } => (session, fresh, make, text, key, done, cancel, busy),
    };
    let sink = |event: BrainEvent| match event {
        BrainEvent::Delta(text) => done.send(Wrote::Text(key, text)).is_ok(),
        BrainEvent::TurnDone => done.send(Wrote::Done(key)).is_ok(),
        BrainEvent::Failed(error) => done.send(Wrote::Failed(key, error)).is_ok(),
        _ => true,
    };
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    if fresh || !brains.contains_key(&session) {
        match start_brain(&make) {
            Ok(brain) => {
                brains.insert(session, brain);
            }
            Err(error) => {
                sink(BrainEvent::Failed(error));
                return;
            }
        }
    }
    let Some(brain) = brains.get_mut(&session) else { return };
    busy.store(true, Ordering::Relaxed);
    brain.send_user(text, Vec::new());
    'turn: loop {
        if cancel.load(Ordering::Relaxed) {
            brain.cancel();
            break;
        }
        for event in brain.poll() {
            let end = matches!(event, BrainEvent::TurnDone | BrainEvent::Failed(_));
            if !sink(event) {
                brain.cancel();
                break 'turn;
            }
            if end {
                break 'turn;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    busy.store(false, Ordering::Relaxed);
}

// ------------------------------------------------------------------ sessions

/// What one conversation about a graphic has seen of its code: each file's
/// text as of its last ask, and the lines of that text it was shown.
struct Seen {
    text: String,
    shown: BTreeSet<usize>,
}

/// A conversation with the model about one graphic, kept across asks so
/// its code stays in the model's context: a later ask sends only the ask,
/// what changed in the code since (hand edits, applied variants) and the
/// lines it has not seen yet. Session 0 of a graphic is its agent; the
/// others are the parallel writers seeded the same way.
struct Session {
    id: u64,
    scene: String,
    slot: usize,
    seen: Vec<Seen>,
    /// Characters sent and received so far (a fresh conversation past
    /// [`SESSION_CHARS`]).
    chars: usize,
    /// It has an ask in flight (a variant ask), and its thread is running a
    /// turn of it (any: a Generate turn on a scene agent too).
    busy: bool,
    running: Arc<AtomicBool>,
    /// The scene's agent (the guide, the tools; Generate asks it too), not
    /// a light fork for variants.
    agent: bool,
    /// When it was last asked, and when its graphic was last open.
    used: f64,
    open_at: f64,
}

/// A conversation this long starts over (with only what the next ask needs).
const SESSION_CHARS: usize = 400_000;
/// A changed passage this long (or a file replaced) starts the conversation
/// over rather than being sent as a change.
const CHANGE_LINES: usize = 120;
/// A graphic's conversations end this long after it was last asked about,
/// or after it was closed.
const SESSION_IDLE: f64 = 20.0 * 60.0;
const SESSION_CLOSED: f64 = 5.0 * 60.0;

/// The one changed passage between `old` and `new` (by lines): the first
/// line that differs, how many old lines it replaces and how many new ones
/// replace them; `None` when equal.
fn changed_lines(old: &[&str], new: &[&str]) -> Option<(usize, usize, usize)> {
    let start = old.iter().zip(new.iter()).take_while(|(a, b)| a == b).count();
    if start == old.len() && start == new.len() {
        return None;
    }
    let tail = old[start..].iter().rev().zip(new[start..].iter().rev()).take_while(|(a, b)| a == b).count();
    Some((start, old.len() - start - tail, new.len() - start - tail))
}

/// The message of an ask to `session`, given the lines the ask is about
/// (`wanted`, in the current texts), and what the session has then seen;
/// `None` when the session should start over.
fn session_message(session: &mut Session, files: &[(usize, String, String)], wanted: &[BTreeSet<usize>], head: &str) -> Option<String> {
    let first = session.seen.is_empty();
    if !first && (session.seen.len() != files.len() || session.chars > SESSION_CHARS) {
        return None;
    }
    let mut changes: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); files.len()];
    let mut removed: Vec<String> = Vec::new();
    let mut unseen: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); files.len()];
    let mut next: Vec<Seen> = Vec::new();
    for (k, (_, name, text)) in files.iter().enumerate() {
        let new: Vec<&str> = text.split('\n').collect();
        let mut shown: BTreeSet<usize> = BTreeSet::new();
        if !first {
            let seen = &session.seen[k];
            let old: Vec<&str> = seen.text.split('\n').collect();
            match changed_lines(&old, &new) {
                None => shown = seen.shown.clone(),
                Some((at, gone, came)) => {
                    if gone.max(came) > CHANGE_LINES {
                        return None;
                    }
                    // Lines it saw keep their place, moved past the change.
                    for &i in &seen.shown {
                        if i < at {
                            shown.insert(i);
                        } else if i >= at + gone {
                            shown.insert(i + came - gone);
                        }
                    }
                    if came > 0 {
                        for i in at..at + came {
                            changes[k].insert(i);
                            shown.insert(i);
                        }
                    } else {
                        removed.push(format!("{name}: lines {}-{} were removed", at + 1, at + gone));
                    }
                }
            }
        }
        for &i in &wanted[k] {
            if shown.insert(i) && !changes[k].contains(&i) {
                unseen[k].insert(i);
            }
        }
        next.push(Seen { text: text.clone(), shown });
    }
    let mut text = String::from(head);
    if first {
        text.push_str("\nThe code (each part under `@@ <file>` with its line numbers; write `@@ <file>` alone in your answer):\n");
        text.push_str(&excerpt(files, &unseen));
    } else {
        let changed = excerpt(files, &changes);
        if !changed.is_empty() || !removed.is_empty() {
            text.push_str("\nThe code changed since your last answer (by hand, or a variant applied); these parts as they are now:\n");
            text.push_str(&changed);
            for line in &removed {
                text.push_str(line);
                text.push('\n');
            }
        }
        let more = excerpt(files, &unseen);
        if !more.is_empty() {
            text.push_str("\nMore of the code for this ask:\n");
            text.push_str(&more);
        }
        text.push_str("\nPatch the code as it is now; write `@@ <file>` alone in your answer.\n");
    }
    session.seen = next;
    Some(text)
}

// ------------------------------------------------------------------ state

/// One replaced passage: `old` lines of a file (as the model wrote them)
/// and the `new` lines in their place.
#[derive(Clone, Debug)]
struct Hunk {
    /// 0: the document, k: its k-th module.
    doc: usize,
    file: String,
    old: Vec<String>,
    new: Vec<String>,
}

/// A model writing some of a request's variants.
struct Writer {
    key: u64,
    /// The session it asks in, and the characters it sent.
    session: u64,
    sent: usize,
    /// How its conversation is made and runs (to ask again in it), and
    /// how many guide parts it asked for.
    slot: usize,
    make: Make,
    running: Arc<AtomicBool>,
    guides: u8,
    cancel: Arc<AtomicBool>,
    text: String,
    /// Blocks of `text` taken as variants.
    taken: usize,
    done: bool,
    /// Done, and every block taken.
    closed: bool,
    /// The variant numbers it writes.
    numbers: Range<usize>,
}

struct Request {
    serial: u64,
    ask: String,
    n: usize,
    model: String,
    scope: String,
    started: f64,
    writers: Vec<Writer>,
    /// When the first variant's picture painted, when every variant had
    /// loaded (or failed), and when every one that loaded had painted (or
    /// a while after they loaded, when the strip is not drawn): then the
    /// request is complete.
    first: Option<f64>,
    loaded: Option<f64>,
    all: Option<f64>,
    /// The writers' failures.
    failures: Vec<String>,
    /// Characters of code sent.
    context_chars: usize,
    /// A change made at once, not variants to choose from.
    revise: Option<Revise>,
    /// Palettes (colour maps), not patches.
    palette: bool,
    /// An editor panel made in parts: each writer's modules, put together.
    panel: bool,
    /// Its answers are read by the host itself (the deep make's audit).
    raw: bool,
    /// The deep make's own work (its exposures): never shown in the strip.
    internal: bool,
}

impl Request {
    fn writing(&self) -> bool {
        self.writers.iter().any(|w| !w.done)
    }
}

struct Variant {
    id: u64,
    request: u64,
    number: usize,
    label: String,
    note: String,
    hunks: Vec<Hunk>,
    /// Why it cannot be used (its patch does not fit, it does not load).
    error: Option<String>,
    doc: Option<Arc<MotionDoc>>,
    /// Its frame at the strip's time, and whether the strip has it.
    frame: Option<(f64, MotionFrame)>,
    given: Option<u64>,
    /// When it was written, loaded and first painted (since its request).
    arrived: f64,
    loaded: Option<f64>,
    painted: Option<f64>,
    load_ms: f64,
    swatches: Vec<[f32; 4]>,
    loading: bool,
    /// A palette variant: the colours it maps (old as written, new).
    map: Option<Vec<(String, [f64; 4])>>,
}

#[derive(Default)]
pub struct Variants {
    loader: Option<Loader>,
    writers: Option<Writers>,
    next_writer: u64,
    /// The model conversations about graphics, kept across asks.
    sessions: Vec<Session>,
    next_session: u64,
    /// The scene's agent is making a change (the strip's "Do").
    agent_working: bool,
    /// Control API calls the strip makes (the library's palettes): queued
    /// for the app to run, and running (tag, call id).
    calls: Vec<(String, &'static str, Json)>,
    running_calls: Vec<(String, u64)>,
    requests: Vec<Request>,
    list: Vec<Variant>,
    next_id: u64,
    next_request: u64,
    /// The variant the viewer shows instead of the graphic (a card
    /// clicked; not applied until Choose).
    pub previewed: Option<u64>,
    /// The strip's first card: the graphic as it is (clicked to compare).
    original: Option<u64>,
    /// The strip is open.
    pub open: bool,
    status: String,
    cards_dirty: bool,
    /// The time the strip's frames are of.
    strip_t: Option<f64>,
    /// The last application: the variants, the session revision after it
    /// and its undo label (applying another undoes it first).
    applied: Option<(Vec<u64>, u64, String)>,
    /// The graphic the variants are of (its node or title).
    of: Option<String>,
    /// The graphics whose panel was made on opening (once each).
    panel_asked: std::collections::HashSet<String>,
    /// Panels the self-check sent back: (the panel's request, the fix's
    /// request, how many issues).
    panel_fixes: Vec<(u64, u64, usize)>,
    /// Made panels being measured: their static findings, waiting.
    panel_waiting: std::collections::HashMap<u64, Vec<String>>,
    /// Each panel request's graphic (it is written only into that one).
    panel_scene: std::collections::HashMap<u64, String>,
}

impl Variants {
    /// The index of the scene's agent (`agent`), or of its `k`-th fork,
    /// that is idle; a new one when there is none.
    fn session_for(&mut self, scene: &str, agent: bool, k: usize) -> usize {
        let slot = if agent { 0 } else { 1 + k % (WRITERS - 1) };
        let idle = |v: &Session| !v.busy && !v.running.load(Ordering::Relaxed);
        if let Some(at) = self.sessions.iter().position(|v| v.scene == scene && v.agent == agent && (agent || v.slot == slot) && (agent || idle(v))) {
            return at;
        }
        self.next_session += 1;
        self.sessions.push(Session { id: self.next_session, scene: scene.to_string(), slot, seen: Vec::new(), chars: 0, busy: false, running: Arc::new(AtomicBool::new(false)), agent, used: Cx::time_now(), open_at: Cx::time_now() });
        self.sessions.len() - 1
    }
}

/// The open graphic: its Graphic clip, title and length.
pub fn open_graphic(page: &VideoCut) -> Option<(Option<String>, String, f64)> {
    let session = page.motion.desk.session.as_ref()?;
    Some((session.node.clone(), session.title.clone(), session.duration()))
}

/// What the API waits for.
pub enum Pending {
    Request(u64),
    /// A graphic being opened (its node), until it has loaded.
    Open(String),
    /// A change by the scene's agent (its first request), until it was
    /// applied, refused or handed on (and then until Generate finished).
    Revision(u64),
}

pub enum Answer {
    Done(Result<Json, String>),
    Later(Pending),
}

// ------------------------------------------------------------------ the ask

/// The number an ask names ("5 colour options", "five timings"), if any.
fn number_in(ask: &str) -> Option<usize> {
    const WORDS: [&str; 12] = ["one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve"];
    ask.split(|c: char| !c.is_alphanumeric()).find_map(|word| {
        let lower = word.to_lowercase();
        word.parse::<usize>().ok().filter(|n| *n > 0).or_else(|| WORDS.iter().position(|w| *w == lower).map(|i| i + 1))
    })
}

/// The ask's words worth finding in the code, each with its stems
/// (`axes` → `axe`, `axis`).
fn search_words(ask: &str) -> Vec<String> {
    const SKIP: &[&str] = &[
        "give", "show", "make", "try", "options", "option", "variants", "variant", "versions", "version", "alternatives", "alternative", "ideas", "idea",
        "the", "for", "some", "different", "more", "less", "please", "with", "and", "this", "that", "its", "like", "into", "from", "what", "how",
        "colour", "colours", "color", "colors", "timing", "timings", "time", "times", "text", "words", "wording", "size", "sizes", "style", "styles",
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "new", "other", "others", "can", "you",
    ];
    let mut out: Vec<String> = Vec::new();
    for word in ask.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let word = word.to_lowercase();
        if word.len() < 3 || word.chars().all(|c| c.is_ascii_digit()) || SKIP.contains(&word.as_str()) {
            continue;
        }
        let mut stems = vec![word.clone()];
        if let Some(base) = word.strip_suffix("es") {
            stems.push(base.to_string());
            stems.push(format!("{base}is"));
        } else if let Some(base) = word.strip_suffix('s') {
            stems.push(base.to_string());
        }
        for stem in stems {
            if stem.len() >= 3 && !out.contains(&stem) {
                out.push(stem);
            }
        }
    }
    out
}

/// Whether `line` (lowercased) has `stem` at the start of a word.
fn has_word(line: &str, stem: &str) -> bool {
    let bytes = line.as_bytes();
    line.match_indices(stem).any(|(at, _)| at == 0 || !(bytes[at - 1].is_ascii_alphanumeric()))
}

/// The files the model may change: (index, name, text).
fn files(session: &MotionSession) -> Vec<(usize, String, String)> {
    std::iter::once((0, "doc".to_string(), session.source.clone()))
        .chain(session.modules.iter().enumerate().map(|(k, m)| (k + 1, m.name.clone(), m.source.clone())))
        .collect()
}

/// What an ask is about.
#[derive(Clone, Debug)]
enum Target {
    /// The code selected (or picked) in the desk; by name when none is.
    Selection,
    /// Lines `from..=to` (1-based) of a file.
    Lines { file: String, from: usize, to: usize },
    /// What the ask names.
    Named,
}

fn target_of(text: &str) -> Target {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("lines:") {
        let mut parts = rest.rsplitn(2, ':');
        let range = parts.next().unwrap_or("");
        let file = parts.next().unwrap_or("doc").to_string();
        let (a, b) = range.split_once('-').unwrap_or((range, range));
        if let (Ok(from), Ok(to)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
            return Target::Lines { file, from: from.max(1), to: to.max(from) };
        }
    }
    match text {
        "" | "selection" => Target::Selection,
        _ => Target::Named,
    }
}

/// The code an ask is about, as the model reads it, and a short phrase of
/// what it is scoped to.
fn context(session: &MotionSession, selection: Option<(usize, usize)>, focus: Option<usize>, target: &Target, ask: &str) -> (Vec<BTreeSet<usize>>, String, usize) {
    let files = files(session);
    let lines: Vec<Vec<&str>> = files.iter().map(|(_, _, text)| text.split('\n').collect()).collect();
    let mut wanted: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); files.len()];
    let mut scope = String::new();
    let line_of = |text: &str, byte: usize| text[..byte.min(text.len())].matches('\n').count();
    match target {
        Target::Lines { file, from, to } => {
            if let Some(k) = files.iter().position(|(_, name, _)| name == file) {
                for line in (*from - 1)..(*to).min(lines[k].len()) {
                    wanted[k].insert(line);
                }
                scope = format!("{file} lines {from}–{to}");
            }
        }
        Target::Selection => {
            if let Some((doc, region)) = selection {
                let code = if doc == 0 { Some(&session.code) } else { session.modules.get(doc - 1).map(|m| &m.code) };
                if let (Some(code), Some(text)) = (code, files.get(doc).map(|f| &f.2)) {
                    if let Some(region) = code.regions.get(region) {
                        let (a, b) = (line_of(text, region.range.start), line_of(text, region.range.end));
                        for line in a..=b {
                            wanted[doc].insert(line);
                        }
                        let what = focus.and_then(|byte| code.literals.iter().find(|l| l.range.start == byte)).map(|l| format!(" · {}", l.label)).unwrap_or_default();
                        // What fed it: the call and the `let`s its arguments
                        // read (look records, knobs, anchors), whole.
                        for g in makepad_motion::schema::code::gather(code, text, focus.unwrap_or(region.range.start), 12) {
                            for line in line_of(text, g.range.start)..=line_of(text, g.range.end) {
                                wanted[doc].insert(line);
                            }
                        }
                        scope = format!("{} in {}{what}", region.name, files[doc].1);
                    }
                }
            }
        }
        Target::Named => {}
    }
    // The lines that mention what the ask names.
    let words = search_words(ask);
    let mut hits: Vec<(usize, usize, usize)> = Vec::new();
    if !words.is_empty() {
        for (k, file_lines) in lines.iter().enumerate() {
            for (i, line) in file_lines.iter().enumerate() {
                let lower = line.to_lowercase();
                let score = words.iter().filter(|w| has_word(&lower, w)).count();
                if score > 0 {
                    hits.push((score, k, i));
                }
            }
        }
    }
    // Best first, then in order; each with the lines around it.
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut left_out = 0;
    let budget = |wanted: &Vec<BTreeSet<usize>>| wanted.iter().map(BTreeSet::len).sum::<usize>();
    for (_, k, i) in &hits {
        if budget(&wanted) >= CONTEXT_LINES {
            left_out += 1;
            continue;
        }
        for line in i.saturating_sub(AROUND)..(i + AROUND + 1).min(lines[*k].len()) {
            wanted[*k].insert(line);
        }
    }
    if scope.is_empty() {
        scope = if hits.is_empty() { "the document".into() } else { format!("named: {}", words.first().cloned().unwrap_or_default()) };
    }
    // Knobs are made of the scene's named values: the map of them (each
    // top-level `let`'s intent lines and first line), and where `Motion{`
    // starts.
    let lower_ask = ask.to_lowercase();
    if lower_ask.contains("editor") || lower_ask.contains("panel") {
        // The panel as written, whole, and where `Motion{` is.
        for (k, file_lines) in lines.iter().enumerate() {
            let mut inside = false;
            for (i, line) in file_lines.iter().enumerate() {
                let t = line.trim_start();
                if t.starts_with("let editor_panel") {
                    inside = true;
                } else if inside && t.starts_with("let ") || (k == 0 && t.starts_with("Motion{")) {
                    inside = false;
                }
                if inside || (k == 0 && t.starts_with("Motion{")) {
                    wanted[k].insert(i);
                }
            }
        }
    }
    if lower_ask.contains("knob") {
        for (k, file_lines) in lines.iter().enumerate() {
            for (i, line) in file_lines.iter().enumerate() {
                let t = line.trim_start();
                let named = t.strip_prefix("let ").is_some_and(|r| r.chars().next().is_some_and(|c| c.is_ascii_uppercase()));
                if named || (k == 0 && t.starts_with("Motion{")) {
                    wanted[k].insert(i);
                    let mut a = i;
                    while a > 0 && file_lines[a - 1].trim_start().starts_with("///") {
                        a -= 1;
                        wanted[k].insert(a);
                    }
                }
            }
        }
    }
    // Nothing found: the document itself.
    if wanted.iter().all(BTreeSet::is_empty) {
        for line in 0..lines[0].len() {
            wanted[0].insert(line);
        }
    }
    // The constants the lines use, one level: `look.ASH` (a module's) and
    // `ASH` (the file's own).
    let mut constants: Vec<(usize, usize)> = Vec::new();
    for (k, set) in wanted.iter().enumerate() {
        for &i in set {
            let line = lines[k][i];
            let mut rest = line;
            while let Some(at) = rest.find(|c: char| c.is_ascii_uppercase()) {
                let before = &rest[..at];
                let name: String = rest[at..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                rest = &rest[at + name.len().max(1)..];
                if name.len() < 2 || !name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
                    continue;
                }
                let owner = before.strip_suffix('.').map(|b| b.rsplit(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or(""));
                let file = match owner {
                    Some(module) if !module.is_empty() => files.iter().position(|(_, n, _)| n == module),
                    Some(_) => None,
                    None => Some(k),
                };
                let Some(f) = file else { continue };
                let defines = |l: &str| l.trim_start().strip_prefix("let ").is_some_and(|r| r.trim_start().strip_prefix(name.as_str()).is_some_and(|r| r.trim_start().starts_with('=')));
                if let Some(j) = lines[f].iter().position(|l| defines(l)) {
                    // Its intent line(s) above, and the whole record.
                    let mut a = j;
                    while a > 0 && lines[f][a - 1].trim_start().starts_with("///") {
                        a -= 1;
                    }
                    let mut depth = 0i32;
                    let mut b = j;
                    loop {
                        for c in lines[f][b].chars() {
                            match c {
                                '{' | '[' | '(' => depth += 1,
                                '}' | ']' | ')' => depth -= 1,
                                _ => {}
                            }
                        }
                        if depth <= 0 || b + 1 >= lines[f].len() {
                            break;
                        }
                        b += 1;
                    }
                    constants.extend((a..=b).map(|i| (f, i)));
                }
            }
        }
    }
    for (f, j) in constants {
        wanted[f].insert(j);
    }
    (wanted, scope, left_out)
}

/// The lines `sets` names of each file, as runs under `@@ <file>   (lines
/// a-b)` headers.
fn excerpt(files: &[(usize, String, String)], sets: &[BTreeSet<usize>]) -> String {
    let mut out = String::new();
    for (k, set) in sets.iter().enumerate() {
        let lines: Vec<&str> = files[k].2.split('\n').collect();
        let mut run: Vec<usize> = Vec::new();
        let flush = |run: &mut Vec<usize>, out: &mut String| {
            if let (Some(a), Some(b)) = (run.first(), run.last()) {
                out.push_str(&format!("@@ {}   (lines {}-{})\n", files[k].1, a + 1, b + 1));
                for &i in run.iter() {
                    out.push_str(lines[i]);
                    out.push('\n');
                }
                out.push('\n');
            }
            run.clear();
        };
        for &i in set.iter().filter(|&&i| i < lines.len()) {
            if run.last().is_some_and(|last| i != last + 1) {
                flush(&mut run, &mut out);
            }
            run.push(i);
        }
        flush(&mut run, &mut out);
    }
    out
}

/// The backend a model spec names.
pub(crate) fn backend_named(spec: &str) -> Backend {
    match spec.split_once(':') {
        Some(("claude-cli", id)) => Backend::ClaudeCode { model: id.to_string() },
        Some(("claude-api", id)) => Backend::ClaudeApi { model: id.to_string() },
        Some(("hub", id)) => Backend::Hub { model: Some(id.to_string()).filter(|id| !id.is_empty()) },
        _ if spec == "hub" => Backend::Hub { model: None },
        _ => Backend::ClaudeCode { model: spec.to_string() },
    }
}

pub(crate) fn model_spec(asked: Option<&str>) -> String {
    asked.map(str::to_string).filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("STAGE_VARIANTS_MODEL").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

/// A variant's patch written back in the model's own form (a seed).
fn hunks_text(hunks: &[Hunk]) -> String {
    let mut out = String::new();
    for hunk in hunks {
        out.push_str(&format!("@@ {}\n", hunk.file));
        for line in &hunk.old {
            out.push_str(&format!("-{line}\n"));
        }
        for line in &hunk.new {
            out.push_str(&format!("+{line}\n"));
        }
    }
    out
}

// ------------------------------------------------------------------ patches

/// The complete blocks of a writer's text (a block is complete when the
/// next starts, or the writer is done).
fn blocks(text: &str, done: bool) -> Vec<&str> {
    let mut starts: Vec<usize> = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("## ") {
            starts.push(at);
        }
        at += line.len();
    }
    let mut out = Vec::new();
    for (i, &start) in starts.iter().enumerate() {
        match starts.get(i + 1) {
            Some(&end) => out.push(&text[start..end]),
            None if done => out.push(&text[start..]),
            None => {}
        }
    }
    out
}

/// A block's label, note and hunks (files by name, not yet found).
fn parse_block(block: &str) -> (String, String, Vec<(String, Vec<String>, Vec<String>)>) {
    let mut lines = block.lines();
    let head = lines.next().unwrap_or("").trim_start().trim_start_matches('#').trim();
    let head = head.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches(['.', ')', ':']).trim();
    let (label, note) = match head.split_once(':') {
        Some((label, note)) => (label.trim().to_string(), note.trim().to_string()),
        None => match head.split_once(" — ").or_else(|| head.split_once(" - ")) {
            Some((label, note)) => (label.trim().to_string(), note.trim().to_string()),
            None => (head.to_string(), String::new()),
        },
    };
    let mut hunks: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
    let mut file = String::from("doc");
    let mut old: Vec<String> = Vec::new();
    let mut new: Vec<String> = Vec::new();
    let close = |file: &str, old: &mut Vec<String>, new: &mut Vec<String>, hunks: &mut Vec<(String, Vec<String>, Vec<String>)>| {
        if !old.is_empty() {
            hunks.push((file.to_string(), std::mem::take(old), std::mem::take(new)));
        }
        old.clear();
        new.clear();
    };
    for line in lines {
        if let Some(rest) = line.strip_prefix("@@") {
            close(&file, &mut old, &mut new, &mut hunks);
            file = rest.split_whitespace().next().unwrap_or("doc").to_string();
        } else if let Some(rest) = line.strip_prefix('-') {
            if !new.is_empty() {
                close(&file, &mut old, &mut new, &mut hunks);
            }
            old.push(rest.to_string());
        } else if let Some(rest) = line.strip_prefix('+') {
            new.push(rest.to_string());
        } else if !old.is_empty() {
            close(&file, &mut old, &mut new, &mut hunks);
        }
    }
    close(&file, &mut old, &mut new, &mut hunks);
    (label, note, hunks)
}

fn indent(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// Where a hunk goes in `text`, and what replaces it there: its `old`
/// lines exactly, else line by line ignoring the indentation (the `new`
/// lines then take the file's).
fn place(text: &str, hunk: &Hunk) -> Result<(Range<usize>, String), String> {
    let joined = hunk.old.join("\n");
    let exact: Vec<usize> = text.match_indices(joined.as_str()).map(|(at, _)| at).filter(|&at| at == 0 || text.as_bytes()[at - 1] == b'\n').collect();
    if exact.len() == 1 {
        return Ok((exact[0]..exact[0] + joined.len(), hunk.new.join("\n")));
    }
    if exact.len() > 1 {
        return Err(format!("its lines occur {} times in {}", exact.len(), hunk.file));
    }
    let mut starts = Vec::new();
    let mut at = 0;
    for line in text.split('\n') {
        starts.push((at, line));
        at += line.len() + 1;
    }
    let want: Vec<&str> = hunk.old.iter().map(|l| l.trim()).collect();
    let found: Vec<usize> = (0..starts.len().saturating_sub(want.len() - 1)).filter(|&i| want.iter().enumerate().all(|(j, w)| starts[i + j].1.trim() == *w)).collect();
    match found.len() {
        1 => {
            let i = found[0];
            let (start, first) = starts[i];
            let (last_at, last) = starts[i + want.len() - 1];
            let (theirs, ours) = (indent(&hunk.old[0]), indent(first));
            let new: Vec<String> = hunk.new.iter().map(|line| match line.strip_prefix(theirs) {
                Some(rest) => format!("{ours}{rest}"),
                None => line.clone(),
            }).collect();
            Ok((start..last_at + last.len(), new.join("\n")))
        }
        0 => Err(format!("its lines are not in {}", hunk.file)),
        n => Err(format!("its lines occur {n} times in {}", hunk.file)),
    }
}

/// `text` with `hunks` applied (all of one file); refused when one does
/// not fit or two overlap.
fn apply_hunks(text: &str, hunks: &[&Hunk]) -> Result<String, String> {
    let mut placed: Vec<(Range<usize>, String)> = hunks.iter().map(|hunk| place(text, hunk)).collect::<Result<_, _>>()?;
    placed.sort_by_key(|(range, _)| range.start);
    for pair in placed.windows(2) {
        if pair[0].0.end > pair[1].0.start {
            return Err("two of them change the same lines".into());
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (range, new) in placed {
        out.push_str(&text[at..range.start]);
        out.push_str(&keep_indent(&text[range.clone()], &new));
        at = range.end;
    }
    out.push_str(&text[at..]);
    makepad_motion::edit::balanced(&out).map_err(|why| format!("its brackets do not match: {why}"))?;
    Ok(out)
}

/// A hunk's new lines that are old lines again (moved, kept) keep the
/// indentation they had: a writer re-indenting a block it only wraps or
/// touches changes nothing but whitespace, and that is noise in the code.
fn keep_indent(old: &str, new: &str) -> String {
    let indent_of = |l: &str| l[..l.len() - l.trim_start().len()].to_string();
    let olds: Vec<(&str, String)> = old.lines().map(|l| (l.trim(), indent_of(l))).filter(|(t, _)| !t.is_empty()).collect();
    let mut out: Vec<String> = Vec::new();
    for line in new.split('\n') {
        let t = line.trim();
        match olds.iter().find(|(o, _)| *o == t && !t.is_empty()) {
            Some((_, indent)) => out.push(format!("{indent}{t}")),
            None => out.push(line.to_string()),
        }
    }
    out.join("\n")
}

/// The colours a variant writes.
fn swatches(hunks: &[Hunk]) -> Vec<[f32; 4]> {
    let mut out: Vec<[f32; 4]> = Vec::new();
    for line in hunks.iter().flat_map(|h| h.new.iter()) {
        let mut rest = line.as_str();
        while let Some(at) = rest.find('#') {
            let hex: String = rest[at + 1..].chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            rest = &rest[at + 1..];
            let value = match hex.len() {
                6 => u32::from_str_radix(&hex, 16).ok().map(|v| (v << 8) | 0xff),
                8 => u32::from_str_radix(&hex, 16).ok(),
                _ => None,
            };
            if let Some(v) = value {
                let c = Vec4f::from_u32(v);
                let rgba = [c.x, c.y, c.z, c.w];
                if !out.contains(&rgba) {
                    out.push(rgba);
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------ requests

/// Ask for variants of the open graphic. `seed`: a variant to go on from.
/// What an ask is for.
enum Mode {
    /// `n` variants (`split` calls share them), maybe going on from one.
    Variants { n: Option<usize>, split: Option<usize>, seed: Option<u64> },
    /// One change made at once (the strip's "Do"); `retry`: the request it
    /// tries again after, and why that failed.
    Revise { retry: Option<(u64, String)> },
    /// `n` palettes: colour maps over the scene's palette.
    Palette { n: Option<usize>, split: Option<usize> },
}

/// A revision's progress (a request in [`Mode::Revise`]).
#[derive(Clone, Debug, Default)]
struct Revise {
    /// The first request of the change (a retry has its own).
    origin: u64,
    retried: bool,
    /// Applied (as what), refused (why), or handed to Generate (why).
    applied: Option<String>,
    failed: Option<String>,
    escalated: Option<String>,
    /// A question answered (no change).
    answered: Option<String>,
    /// From the ask to applied, or to the end.
    at: Option<f64>,
}

/// Generators before content: the rule every scene and panel ask gets.
const GENERATORS: &str = "# Generators first\n\
Procedural beats written-out content: it runs at once and stays tunable. For a pattern, a palette, a rhythm, a \
shower, a camera move, a layout or a field, first use a generator of the library below (or of the scene's shared \
module) with its knobs. When none fits, write a new generator, not one-off content: a seeded, parameterised \
function (`fn(seed, count, spread, …)`) with a `///` line saying what it makes and its knobs' ranges, in the scene's \
shared module beside the others when it has one, so every plate and every panel can use it; then call it. A panel \
binds a generator's knobs (its call's arguments, tagged `/**name*/` when they are not a `let`'s fields).";

/// How the scene's editor panel is written (the scene's agent's system
/// text has it; asks about the panel get the scene's paths with it).
const PANEL_GUIDE: &str = r##"
# The scene's editor panel
A scene may have an editor panel: `let editor_panel = fn() { View{ ... } }`, written just before `Motion{`. Motion never
calls it. It is a small Splash UI program that Stage runs in a sandbox in a full-height column beside the picture; it is
the person's main way to change the scene, so make it an instrument: rack modules for what a person thinks of (Camera,
Pen, Sparks, Lyrics, Look, Timing), labelled in their words, the most telling controls first, a macro or two that move
the whole feel (Energy, Calm), presets that differ. Paths are the code's own names (`WORD_SPARKS.count`,
`LYRIC_LOOK.font`, `KEYS[3].zoom`, `shapes.unicorn_crude`, a `/**tag*/` expression); use only paths from the list you
are given, never guess one. A value control changes the picture in the frame it moves: bind it to a path the list does
not mark RELOAD (that value is read while the scene loads; bind the literal it is computed from instead, or leave it
out). Swaps and generators (`PanelFn`, `scene.swap`, `scene.replace`) reload the scene; that is expected. Ranges sit around the value written, wide enough to explore and narrow enough that the
whole range looks good. Beyond its widgets' own paths a panel reaches the scene through `scene`:
- `scene.read(path)`: the value written at a path: a number, a bool, or the text as written (`#rrggbb`, `@name`, a font).
- `scene.set(path, value)`: writes a value (a number keeps its unit; a colour is `"#rrggbb"`; a choice `"@name"`).
- `scene.swap(path, fn_name)`: a function slot names another function of the same parameters.
- `scene.replace(path, code)`: new code for the expression at a path (a generator: built from the controls).
Asked to make the panel anew, answer with one block that replaces `let editor_panel` (the whole of it). Asked for one
more control or tool, change only that: a block whose hunks add it, the rest of the panel as it is. A new module goes
first: its hunk's old line is the panel's `flow: Down width: Fill spacing: 10` line, its new lines that line and the
module. Give every new module a `when:` unless it acts throughout ("beats 9-16", "the lyric 'AGI'", "2.4s-5s").

## Three asks and how they are answered
1. "give me a mood pad for the sparks": one module, its controls on the sparks' generator knobs, a pad between its
   four slots: `PanelModule{title: "Sparks mood" when: "2.8s-4.2s" PanelKnob{path: "WORD_SPARKS.count" min: 4 max: 48}
   PanelSlider{path: "WORD_SPARKS.lift"} PanelSeed{path: "WORD_SPARKS.seed"} PanelMood{}}`, added first.
2. "a slider for the sheet grid density": no one value says that (the grid's spacing is computed from `SHEET` and a
   hard-coded `1.0`). Expose it: a hunk that introduces `let GRID_DENSITY = 1.0` beside the sheet's other values and
   one that uses it where the spacing is computed (`spacing / GRID_DENSITY`), so the picture is identical at 1.0; and a
   hunk that adds `PanelModule{title: "Grid" PanelSlider{path: "GRID_DENSITY" min: 0.25 max: 4}}` to the panel. All
   in the one block: one undo step. A top-level number `let` read where the picture is drawn moves in the frame.
3. "a camera path editor for this scene": the camera's keys as points: `PanelModule{title: "Camera path" when:
   "0s-9.3s" PanelPoints{path: "CAMERA_KEYS" joined: true} PanelEase{path: "CAMERA_RIG.ease"}}`; when the keys are
   not one list of points, expose them as one (as in 2) first.
An instrument for a shape, a lyric timing editor, a custom-drawn view: the same, a module of the catalogue's widgets
on paths that exist or that the block exposes.
"##;

/// The guide's sections (`## ` headings of the skill file): title, text.
fn guide_sections() -> &'static Vec<(String, String)> {
    static SECTIONS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    SECTIONS.get_or_init(|| {
        let mut out = Vec::new();
        let skill = makepad_story_video_agent::guide::SKILL;
        let mut at = skill.find("\n## ");
        while let Some(start) = at {
            let body = &skill[start + 1..];
            let end = body[3..].find("\n## ").map(|e| e + 3);
            let text = &body[..end.unwrap_or(body.len())];
            let title = text.lines().next().unwrap_or("").trim_start_matches('#').trim().to_string();
            out.push((title, text.trim_end().to_string()));
            at = end.map(|e| start + 1 + e);
        }
        out.push(("Painting with Draw2D".to_string(), makepad_story_video_agent::guide::CANVAS.to_string()));
        out.push(("3D scenes (Scene3D)".to_string(), makepad_story_video_agent::guide::MOTION3D.to_string()));
        out
    })
}

/// The system text of a scene's conversations: how to answer, and the
/// parts of the guide the scene uses (by what its code has in it); the
/// others are named, sent when asked for.
fn scene_system(session: &MotionSession) -> String {
    // Only the core of the guide; the rest is asked for by title (a
    // scene's agent mostly patches what is there).
    let wanted = |title: &str| title.starts_with("2. ");
    let mut out = format!("{SYSTEM}\n\n{GENERATORS}\n\n# The guide: its core\n");
    let mut others = Vec::new();
    for (title, text) in guide_sections() {
        if wanted(title) {
            out.push_str("\n");
            out.push_str(text);
            out.push('\n');
        } else {
            others.push(format!("`{title}`"));
        }
    }
    out.push_str(&format!("\nOther parts of the guide, sent when you answer only `GUIDE: <title>`: {}.\n", others.join(", ")));
    out.push_str(&format!("\n# The scene's palette (when it was opened)\nEach colour, its name, and where it is used:\n{}", super::palette::listing(&super::palette::palette(session))));
    // The functions of the modules the scene imports (a shared module of
    // shapes and styles the whole edit uses): what a slot can name, and
    // where a new one goes so every plate can pick it.
    let shared: Vec<String> = session.modules.iter().filter(|m| m.id != "builtin").flat_map(|m| m.code.fns.iter().map(move |(name, params)| format!("{}.{name}({})", m.name, params.join(", ")))).collect();
    if !shared.is_empty() {
        out.push_str(&format!("\n# Functions of the modules the scene imports\nA new function of a kind the edit shares (a shape, a style) goes into the shared module beside its alternatives, with the same parameters, and the scene names it there.\n{}\n", shared.join("\n")));
    }
    out.push_str(&format!("\n# The generator library (`use mod.motion.modules.<module>.*`)\n{}\n", makepad_motion::modules::INDEX));
    out.push_str(PANEL_GUIDE);
    out.push_str(makepad_value_pickers::panel::CATALOGUE);
    let slots = super::swaps::slots(session);
    if !slots.is_empty() {
        out.push_str(&format!("\n# The scene's function slots\nPlaces that name a function as a value; any function with the same parameters fits. A change of what a slot does (\"make the sparks feel electric\", \"a dragon instead\") is one changed name: an alternative listed, or a new function of the same parameters written beside the others and named there.\n{}", super::swaps::listing(&slots)));
    }
    out
}

/// Ask the open graphic's conversations: for variants (`Mode::Variants`),
/// or for one change (`Mode::Revise`).
fn ask(page: &mut VideoCut, cx: &mut Cx, ask: &str, mode: Mode, target: Target, model: Option<&str>) -> Result<u64, String> {
    let ask = ask.trim().to_string();
    if ask.is_empty() {
        return Err("say what to make variants of, e.g. 5 colour options for the axes".into());
    }
    let desk = &page.motion.desk;
    let session = desk.session.as_ref().ok_or("no graphic is open")?;
    let (n, split, seed, revise) = match &mode {
        Mode::Variants { n, split, seed } => {
            let n = n.or_else(|| number_in(&ask)).unwrap_or(DEFAULT_N).max(1);
            // Two calls share four or more (measured on p_open: the first
            // variant ~0.5 s sooner and all ~2 s sooner than one call;
            // more calls repeat each other's ideas).
            (n, split.unwrap_or(if n >= 4 { 2 } else { 1 }).clamp(1, n), *seed, None)
        }
        Mode::Revise { retry } => (1, 1, None, Some(retry.clone())),
        Mode::Palette { n, split } => {
            let n = n.or_else(|| number_in(&ask)).unwrap_or(DEFAULT_N).max(1);
            (n, split.unwrap_or(if n >= 4 { 2 } else { 1 }).clamp(1, n), None, None)
        }
    };
    let palette_ask = matches!(mode, Mode::Palette { .. });
    let (wanted, scope, left_out) = if palette_ask {
        // A palette is made over the colours, not the code.
        (vec![BTreeSet::new(); 1 + session.modules.len()], "the palette".to_string(), 0)
    } else {
        context(session, desk.code_selection, desk.code_focus, &target, &ask)
    };
    let palette_text = palette_ask.then(|| super::palette::listing(&super::palette::palette(session)));
    // An ask about the editor panel: every path it can bind, with kind,
    // value and label (so no path is guessed).
    let paths_text = (ask.to_lowercase().contains("editor") || ask.to_lowercase().contains("panel"))
        .then(|| format!("\nThe scene's paths (path (kind) = value  // label):\n{}", super::editor_panel::paths_listing(session)));
    let files = files(session);
    let scene = session.node.clone().unwrap_or_else(|| session.title.clone());
    let model = model_spec(model);
    let backend = backend_named(&model);
    let make = Make { backend: backend.clone(), system: Arc::from(scene_system(session).as_str()) };
    let variants = &mut page.motion.variants;
    if variants.loader.is_none() {
        variants.loader = Some(Loader::new(cx)?);
    }
    if variants.writers.is_none() {
        variants.writers = Some(Writers::new(cx)?);
    }
    if let Some(reason) = backend.unavailable() {
        return Err(reason);
    }
    let seed_text = seed.and_then(|id| variants.list.iter().find(|v| v.id == id)).map(|v| {
        format!("The person likes this variant (\"{}\": {}) and wants more in its direction: new ones, not repeats of it.\n{}\n", v.label, v.note, hunks_text(&v.hunks))
    });
    let serial = variants.next_request + 1;
    let first_number = variants.list.iter().map(|v| v.number).max().unwrap_or(0) + 1;
    let more = if left_out > 0 { format!("({left_out} more lines mention the ask and are not shown.)\n") } else { String::new() };
    let mut writers = Vec::new();
    for k in 0..split {
        let (a, b) = (k * n / split, (k + 1) * n / split);
        if a == b {
            continue;
        }
        let numbers = first_number + a..first_number + b;
        let head = match &revise {
            Some(Some((_, why))) => format!("Your block for that change did not work: {why}\nSend the corrected block (number it {}), same format, for the code as it is now.\n", numbers.start),
            Some(None) => format!(
                "Change: {ask}\nMake this change now: answer with one block in the variant format, numbered {}, whose label and note say what you changed. If you need lines of the code you were not shown, answer only `CODE: <the names or words they mention>` and they are sent. If it cannot be a patch of this scene (it needs new scenes or documents), answer only `ESCALATE: <why>`; never for lines you were not shown. If the person asks a question rather than for a change, answer only `ANSWER: <your answer, a few plain sentences>`.\nWhat is selected: {scope}\n{more}{}",
                numbers.start, paths_text.clone().unwrap_or_default(),
            ),
            None if palette_ask => {
                let lean = match (split, k) {
                    (1, _) => "Order them from closest to the current palette to boldest.",
                    (_, 0) => "Other writers make the others; yours stay closest to the current palette.",
                    _ => "Other writers make the others; yours are bold departures.",
                };
                format!(
                    "Ask: {ask}\nMake {} palette{} for this scene, numbered {} to {}. {lean} Each is a block: `## <number> <Label>: <what it is>`, then one line per colour you change, `<colour as listed> -> <new colour>` (`#rrggbb`, or `color(r, g, b)` for an HDR one); colours you do not list stay. Keep the roles: what the scene uses as its accent stays the accent, its paper and ink keep their contrast.\nThe scene's palette (each colour, its name, and where it is used):\n{}",
                    numbers.len(), if numbers.len() == 1 { "" } else { "s" }, numbers.start, numbers.end - 1, palette_text.clone().unwrap_or_default(),
                )
            }
            None => {
                let lean = match (split, k) {
                    (1, _) => "Order them from closest to the current look to boldest.",
                    (_, 0) => "Other writers make the other variants of this ask; yours stay closest to the current look (refinements).",
                    (s, k) if k + 1 == s => "Other writers make the other variants of this ask; yours are the boldest departures.",
                    _ => "Other writers make the other variants of this ask; yours are clear departures, between refinement and bold.",
                };
                format!(
                    "Ask: {ask}\nWrite {} variant{} numbered {} to {}. {lean}\n{}What is selected: {scope}\n{more}",
                    numbers.len(), if numbers.len() == 1 { "" } else { "s" }, numbers.start, numbers.end - 1, seed_text.clone().unwrap_or_default(),
                )
            }
        };
        // The first part (and a change) goes to the scene's agent, unless
        // it is busy; the others to forks. Each keeps what it has seen.
        let agent_free = variants.sessions.iter().find(|v| v.scene == scene && v.agent).is_none_or(|v| !v.busy && !v.running.load(Ordering::Relaxed));
        let agent = k == 0 && agent_free;
        if revise.is_some() && !agent {
            return Err("the scene's agent is still answering".into());
        }
        let at = variants.session_for(&scene, agent, k);
        let conversation = &mut variants.sessions[at];
        let (text, fresh) = match session_message(conversation, &files, &wanted, &head) {
            Some(text) => (text, conversation.chars == 0),
            None => {
                // Too long, or the code was replaced: a new conversation.
                conversation.seen.clear();
                conversation.chars = 0;
                (session_message(conversation, &files, &wanted, &head).unwrap_or_default(), true)
            }
        };
        conversation.busy = true;
        conversation.used = Cx::time_now();
        conversation.chars += text.len();
        let (session_id, slot, running) = (conversation.id, conversation.slot, conversation.running.clone());
        let sent = text.len();
        variants.next_writer += 1;
        let (key, cancel) = (variants.next_writer, Arc::new(AtomicBool::new(false)));
        if let Some(pool) = &mut variants.writers {
            let done = pool.done_tx.clone();
            pool.send(slot, WriteJob::Turn { session: session_id, fresh, make: make.clone(), text, key, done, cancel: cancel.clone(), busy: running.clone() });
        }
        log!("variants: part {k} to {} {session_id}{}", if agent { "the scene agent" } else { "fork" }, if fresh { " (new)" } else { "" });
        writers.push(Writer { key, session: session_id, slot, make: make.clone(), running, guides: 0, sent, cancel, text: String::new(), taken: 0, done: false, closed: false, numbers });
    }
    let (started, revise) = match &revise {
        // A retry counts from the change's first ask.
        Some(Some((origin, _))) => {
            let first = variants.requests.iter().find(|r| r.serial == *origin);
            (first.map_or(Cx::time_now(), |r| r.started), Some(Revise { origin: *origin, retried: true, ..Default::default() }))
        }
        Some(None) => (Cx::time_now(), Some(Revise { origin: serial, ..Default::default() })),
        None => (Cx::time_now(), None),
    };
    variants.next_request = serial;
    // A change is made in place; variants open the bar.
    if revise.is_none() {
        variants.open = true;
    }
    variants.requests.push(Request { serial, ask: ask.clone(), n, model: model.clone(), scope: scope.clone(), started, writers, first: None, loaded: None, all: None, failures: Vec::new(), context_chars: 0, revise, palette: matches!(mode, Mode::Palette { .. }), panel: false, raw: false, internal: false });
    variants.of = session.node.clone().or_else(|| Some(session.title.clone()));
    variants.status = if matches!(mode, Mode::Revise { .. }) { "The scene's agent is on it…".to_string() } else { format!("Asking {} for {n}…", backend.label()) };
    variants.cards_dirty = true;
    let request = variants.requests.last_mut().expect("pushed above");
    request.context_chars = request.writers.iter().map(|w| w.sent).sum();
    log!("variants: request {serial} '{ask}' n={n} split={split} model={model} scope='{scope}' sent {:?} chars (system {} chars)", request.writers.iter().map(|w| w.sent).collect::<Vec<_>>(), make.system.len());
    Ok(serial)
}

/// The palette's perceptual moves as variants, at once (no model): each
/// a map over the scene's colours, drawn and previewed like any variant.
/// `only`: one move; `amount` scales them (1: the default).
fn palette_moves(page: &mut VideoCut, cx: &mut Cx, only: Option<&str>, amount: f64) -> Result<u64, String> {
    let session = page.motion.desk.session.as_ref().ok_or("no graphic is open")?;
    let palette = super::palette::palette(session);
    if palette.is_empty() {
        return Err("the scene writes no colours".into());
    }
    let moves: Vec<&(&str, &str, &str)> = super::palette::MOVES.iter().filter(|m| only.is_none_or(|o| o == m.0)).collect();
    if moves.is_empty() {
        return Err(format!("no move `{}`", only.unwrap_or("")));
    }
    let made: Vec<(String, String, Vec<(String, [f64; 4])>)> = moves.iter().filter_map(|(name, label, note)| {
        let map = super::palette::move_map(&palette, name, amount).ok()?;
        (!map.is_empty()).then(|| (label.to_string(), note.to_string(), map))
    }).collect();
    let variants = &mut page.motion.variants;
    if variants.loader.is_none() {
        variants.loader = Some(Loader::new(cx)?);
    }
    let serial = variants.next_request + 1;
    variants.next_request = serial;
    variants.open = true;
    let first = variants.list.iter().map(|v| v.number).max().unwrap_or(0) + 1;
    variants.requests.push(Request { serial, ask: "palette moves".into(), n: made.len(), model: "local".into(), scope: "the palette".into(), started: Cx::time_now(), writers: Vec::new(), first: None, loaded: None, all: None, failures: Vec::new(), context_chars: 0, revise: None, palette: true, panel: false, raw: false, internal: false });
    for (k, (label, note, map)) in made.into_iter().enumerate() {
        let texts = page.motion.desk.session.as_ref().ok_or("no graphic is open".to_string()).and_then(|session| super::palette::apply_map(session, &map));
        add_variant(page, serial, first + k, label, note, Vec::new(), Some(map), texts);
    }
    Ok(serial)
}

/// What the "Make editor" button asks.
const MAKE_EDITOR: &str = "make me an editor panel for this scene";

/// How the panel's parts are written: each writer gets some of the
/// scene's paths (never its code) and answers with modules for them.
const PANEL_SYSTEM: &str = "You write part of the editor panel of a motion graphic made in Splash. You are given some \
of the scene's paths (the code's own names for its values), each with its kind, its value and what it is for; other \
writers make the modules for the scene's other paths at the same time. Answer only with `PanelModule{...}` records \
(Splash, as in the catalogue below) for your paths, as the message asks: no `let`, no outer View, no prose, no code fences, no comments. \
Where a path is a generator's knob (its seed, count, spread, rate), give it a PanelSeed or knob in the module of \
what it makes. Two or three modules, each for what a person thinks of (Pen, Sparks, Lyrics, Look, Timing) with four to seven \
controls: the most telling ones, titled and labelled in their words; leave the rest out (a panel is an instrument, \
not a list of every value). Write each widget on one line, only the fields it needs. Value controls \
should follow the gesture in the frame: prefer paths not marked RELOAD. A RELOAD value is read while the scene loads, so \
it shows after a short reload; bind one only where no live path does that job. If you have nothing worth a module, \
answer with nothing.";

/// The panel writers' system text.
fn panel_system() -> &'static str {
    static TEXT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TEXT.get_or_init(|| format!("{PANEL_SYSTEM}\n\n{}", makepad_value_pickers::panel::CATALOGUE))
}

/// Writers a panel is made by, at once.
const PANEL_PARTS: usize = 3;

/// Makes the open scene's editor panel: its paths split by the `let` they
/// are in, each part written by its own conversation at once, the modules
/// put together into `let editor_panel` (one undo step). The request
/// answers like a change (applied, failed).
pub fn make_panel(page: &mut VideoCut, cx: &mut Cx, model: Option<&str>, brief: Option<&str>) -> Result<u64, String> {
    // One at a time: a make asked while one runs waits for that one.
    if let Some(running) = page.motion.variants.requests.iter().rev().find(|r| r.panel && r.revise.as_ref().is_some_and(|s| s.applied.is_none() && s.failed.is_none())) {
        return Ok(running.serial);
    }
    let session = page.motion.desk.session.as_ref().ok_or("no graphic is open")?;
    let listing = super::editor_panel::paths_listing(session);
    let lines: Vec<&str> = listing.lines().filter(|l| !l.contains(" RELOAD)")).collect();
    // Live paths only when there are enough of them for a panel.
    let lines: Vec<&str> = if lines.len() < 40 { listing.lines().collect() } else { lines };
    if lines.is_empty() {
        return Err("the scene writes no values a panel could hold".into());
    }
    // A path's group: its `let` (a module's: the module and its `let`).
    let root = |line: &str| -> String {
        let path = line.split(' ').next().unwrap_or("");
        let mut parts = path.split(['.', '[']);
        let first = parts.next().unwrap_or("");
        if first.starts_with(|c: char| c.is_lowercase()) { format!("{first}.{}", parts.next().unwrap_or("")) } else { first.to_string() }
    };
    let parts = makepad_editor_panel::make::split(&lines, &root, PANEL_PARTS);
    let scene = session.node.clone().unwrap_or_else(|| session.title.clone());
    let title = session.title.clone();
    // A remake's brief (every part writes to it), and what is selected in
    // the picture when there is something.
    let (_, things) = super::desk::selection(page);
    let selected: Vec<String> = things.iter().take(6).map(|s| if s.paths.is_empty() { s.label.clone() } else { format!("{} (paths: {})", s.label, s.paths.join(", ")) }).collect();
    let brief = brief.map(str::trim).filter(|b| !b.is_empty());
    let steer = match (brief, selected.is_empty()) {
        (Some(b), true) => format!("\nThe person asks for this panel: {b}. Every part follows it."),
        (Some(b), false) => format!("\nThe person asks for this panel: {b}. Every part follows it. Selected in the picture: {}.", selected.join("; ")),
        (None, _) => String::new(),
    };
    let count = parts.len();
    let texts: Vec<String> = parts.into_iter().enumerate().map(|(k, part)| {
        let feel = if k == 0 { " Start with the overview: a `PanelModule{title: \"Overview\" open: true}` of four to eight controls or `PanelMacro`s that move the scene's feel (energy, calm) through paths of yours." } else { "" };
        format!("The scene: {title}. Write the panel's modules for these paths (part {} of {count}; path (kind) = value  // what it is for).{feel}{steer}\n{part}", k + 1)
    }).collect();
    let _ = scene;
    make_panel_parts(page, cx, model, texts)
}

/// A panel made from these parts' messages (each its own writer, at once),
/// written in as each part arrives (the modules in part order, one undo
/// step for the whole make).
pub(super) fn make_panel_parts(page: &mut VideoCut, cx: &mut Cx, model: Option<&str>, texts: Vec<String>) -> Result<u64, String> {
    let session = page.motion.desk.session.as_ref().ok_or("no graphic is open")?;
    let scene = session.node.clone().unwrap_or_else(|| session.title.clone());
    let model = model_spec(model);
    let backend = backend_named(&model);
    let make = Make { backend: backend.clone(), system: Arc::from(panel_system()) };
    let variants = &mut page.motion.variants;
    if variants.writers.is_none() {
        variants.writers = Some(Writers::new(cx)?);
    }
    if let Some(reason) = backend.unavailable() {
        return Err(reason);
    }
    let serial = variants.next_request + 1;
    let count = texts.len();
    let mut writers = Vec::new();
    for (k, text) in texts.into_iter().enumerate() {
        let at = variants.session_for(&format!("{scene}#panel"), false, k);
        let conversation = &mut variants.sessions[at];
        conversation.chars = 0;
        let fresh = true;
        conversation.busy = true;
        conversation.used = Cx::time_now();
        conversation.chars += text.len();
        let (session_id, slot, running) = (conversation.id, conversation.slot, conversation.running.clone());
        let sent = text.len();
        variants.next_writer += 1;
        let (key, cancel) = (variants.next_writer, Arc::new(AtomicBool::new(false)));
        if let Some(pool) = &mut variants.writers {
            let done = pool.done_tx.clone();
            pool.send(slot, WriteJob::Turn { session: session_id, fresh, make: make.clone(), text, key, done, cancel: cancel.clone(), busy: running.clone() });
        }
        writers.push(Writer { key, session: session_id, slot, make: make.clone(), running, guides: 0, sent, cancel, text: String::new(), taken: 0, done: false, closed: false, numbers: 0..0 });
    }
    variants.next_request = serial;
    let context_chars = writers.iter().map(|w| w.sent).sum();
    log!("variants: panel request {serial}: {count} parts, sent {:?} chars (system {} chars)", writers.iter().map(|w| w.sent).collect::<Vec<_>>(), make.system.len());
    variants.requests.push(Request { serial, ask: MAKE_EDITOR.into(), n: 1, model, scope: "the editor panel".into(), started: Cx::time_now(), writers, first: None, loaded: None, all: None, failures: Vec::new(), context_chars, revise: Some(Revise { origin: serial, ..Default::default() }), palette: false, panel: true, raw: false, internal: false });
    variants.panel_asked.insert(scene.clone());
    variants.panel_scene.insert(serial, scene);
    Ok(serial)
}

/// A scene opened without a panel gets one made (once per scene).
fn auto_panel(page: &mut VideoCut, cx: &mut Cx) {
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    if session.doc_of != session.revision || super::editor_panel::body_of(&session.source, &session.code).is_some() {
        return;
    }
    let scene = session.node.clone().unwrap_or_else(|| session.title.clone());
    if page.motion.variants.panel_asked.contains(&scene) {
        return;
    }
    page.motion.variants.panel_asked.insert(scene);
    if let Err(error) = super::deep_make::start(page, cx, "") {
        log!("variants: the panel was not made: {error}");
    }
}

/// The parts of a panel, once every writer has answered: each checked on
/// its own (one that does not run is left out), put together, written.
fn panels_written(page: &mut VideoCut, cx: &mut Cx) {
    let now = Cx::time_now();
    // Parts arrived since: each checked on its own (one that does not run
    // is left out), kept in the writer's text, marked taken.
    let mut fresh: Vec<u64> = Vec::new();
    for request in page.motion.variants.requests.iter_mut().filter(|r| r.panel) {
        let Some(state) = request.revise.as_ref() else { continue };
        if state.applied.is_some() || state.failed.is_some() {
            continue;
        }
        for writer in request.writers.iter_mut().filter(|w| w.done && w.taken == 0) {
            writer.taken = 1;
            // Each record checked on its own: one that does not run is left
            // out; words instead of modules (a part with nothing worth a
            // control) leave nothing.
            let (kept, dropped) = makepad_editor_panel::make::modules_of(cx, &writer.text, "Panel");
            if let Some(why) = dropped.first() {
                request.failures.push(format!("a module left out: {why}"));
            }
            if kept.is_empty() {
                writer.text.clear();
                writer.taken = 2;
            } else {
                writer.text = kept.join("\n");
            }
            if !fresh.contains(&request.serial) {
                fresh.push(request.serial);
            }
        }
    }
    let mut checks = Vec::new();
    for serial in fresh {
        let Some(request) = page.motion.variants.requests.iter().find(|r| r.serial == serial) else { continue };
        let modules: Vec<String> = request.writers.iter().filter(|w| w.taken == 1).map(|w| w.text.clone()).collect();
        let complete = !request.writing();
        let failure = request.failures.first().cloned();
        let started = request.started;
        let open = page.motion.desk.session.as_ref().map(|s| s.node.clone().unwrap_or_else(|| s.title.clone()));
        let made_for = page.motion.variants.panel_scene.get(&serial).cloned();
        let result = if made_for.is_some() && made_for != open {
            Err("another graphic was opened while it was made".to_string())
        } else if modules.is_empty() {
            if complete { Err(failure.unwrap_or_else(|| "no part of the panel was written".into())) } else { continue }
        } else {
            let code = makepad_editor_panel::make::assemble("editor_panel", &["PanelHeader{}"], &modules);
            // The parts so far, in their order: one undo step as it grows.
            let key = format!("panel:{serial}");
            let written = write_panel_as(page, &code, Some(&key));
            if complete {
                if let Some(session) = page.motion.desk.session.as_mut() {
                    session.end_burst();
                }
            }
            written.map(|_| match &failure {
                None => "editor panel".to_string(),
                Some(f) => format!("editor panel ({f})"),
            })
        };
        if !complete && result.is_ok() {
            log!("variants: panel {serial}: {} parts in at {:.0} ms", modules.len(), (now - started) * 1000.0);
            continue;
        }
        page.motion.variants.panel_scene.remove(&serial);
        let Some(request) = page.motion.variants.requests.iter_mut().find(|r| r.serial == serial) else { continue };
        let Some(state) = request.revise.as_mut() else { continue };
        state.at = Some(now - started);
        match result {
            Ok(what) => {
                log!("variants: panel {serial} written at {:.0} ms: {what}", (now - started) * 1000.0);
                state.applied = Some(what);
                checks.push(serial);
            }
            Err(why) => state.failed = Some(why),
        }
    }
    for serial in checks {
        panel_self_check(page, cx, serial);
    }
}

/// A panel's self-check: each control's path read against the scene as it
/// is (does it resolve, does it follow a gesture, does its range hold the
/// value written), and each macro's targets. Empty: nothing to fix.
fn panel_check(session: &MotionSession, body: &str) -> Vec<String> {
    use makepad_motion::paths;
    let files: Vec<paths::File> = std::iter::once(paths::File { name: "", text: &session.source, code: &session.code })
        .chain(session.modules.iter().filter(|m| m.id != "builtin").map(|m| paths::File { name: &m.name, text: &m.source, code: &m.code }))
        .collect();
    // A RELOAD binding is a defect only where the scene has live values
    // enough for a panel (as `make_panel` chooses its paths); a scene read
    // at load has nothing faster to offer.
    let listing = super::editor_panel::paths_listing(session);
    let live = listing.lines().filter(|l| !l.contains(" RELOAD)")).count();
    let reload: std::collections::HashSet<String> = if live < 40 { Default::default() } else {
        listing.lines().filter(|l| l.contains(" RELOAD)")).filter_map(|l| l.split(' ').next().map(str::to_string)).collect()
    };
    let mut issues = Vec::new();
    let mut live_controls = 0;
    for b in makepad_editor_panel::make::bindings(body) {
        let widget = &b.widget;
        let path = &b.path;
        if b.target {
            if let Err(why) = paths::resolve(&files, path) {
                issues.push(format!("{widget} target `{path}`: {why}"));
            }
            continue;
        }
        let site = match paths::resolve(&files, path) {
            Ok(site) => site,
            Err(why) => {
                issues.push(format!("{widget} `{path}`: {why}"));
                continue;
            }
        };
        if reload.contains(path) {
            issues.push(format!("{widget} `{path}` reloads the scene on each step (RELOAD): bind the live value it comes from, or leave it out if there is none"));
        } else {
            live_controls += 1;
        }
        if let (Some(value), Some(min), Some(max)) = (makepad_editor_panel::make::number(&site.text), b.min, b.max) {
            if value < min || value > max {
                issues.push(format!("{widget} `{path}`: its range {min}..{max} does not hold the value written, {value}"));
            }
        }
    }
    issues.extend(super::panel_time::when_disagreements());
    if live_controls == 0 && issues.is_empty() && live >= 40 {
        issues.push("no control follows a gesture live".into());
    }
    issues
}

/// After a panel is written: checked, and when something is wrong, its
/// list sent back to the scene's agent once to fix (one more undo step).
fn panel_self_check(page: &mut VideoCut, cx: &mut Cx, serial: u64) {
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let Some(body) = super::editor_panel::body_of(&session.source, &session.code) else { return };
    let issues = panel_check(session, &body);
    log!("variants: panel {serial} self-check: {} issue{}", issues.len(), if issues.len() == 1 { "" } else { "s" });
    for issue in &issues {
        log!("variants:   {issue}");
    }
    // Then measured on the picture; the fix waits for both.
    match super::panel_sweep::start(page, serial) {
        Ok(_) => {
            page.motion.variants.panel_waiting.insert(serial, issues);
        }
        Err(why) => {
            log!("variants: panel {serial} not measured: {why}");
            ask_panel_fix(page, cx, serial, issues);
        }
    }
}

/// The self-check's findings sent back to the scene's agent, once.
fn ask_panel_fix(page: &mut VideoCut, cx: &mut Cx, serial: u64, issues: Vec<String>) {
    if issues.is_empty() || page.motion.variants.panel_fixes.iter().any(|(s, _, _)| *s == serial) {
        return;
    }
    let asked = format!("Fix the editor panel's problems the check found (change `let editor_panel`; keep everything else):\n- {}", issues.join("\n- "));
    match ask(page, cx, &asked, Mode::Revise { retry: None }, Target::Named, None) {
        Ok(fix) => page.motion.variants.panel_fixes.push((serial, fix, issues.len())),
        Err(error) => log!("variants: the panel's fix was not asked: {error}"),
    }
}

/// The picture measure of a made panel, when it ends: controls whose whole
/// range changes almost nothing (and `when`s that miss), sent back with the
/// static findings it waited for.
fn panel_measured(page: &mut VideoCut, cx: &mut Cx) {
    let Some((serial, measured, failures)) = super::panel_sweep::take_done() else { return };
    let mut issues = page.motion.variants.panel_waiting.remove(&serial).unwrap_or_default();
    for m in measured.iter().filter(|m| m.changed < super::panel_sweep::INVISIBLE) {
        issues.push(format!("{} barely changes the picture: its whole range moves {:.1}% of the pixels at {:.1} s. Widen its range to where the change is plain to see, bind the value that does show, or leave it out", m.control, m.changed * 100.0, m.t));
    }
    issues.extend(super::panel_time::when_disagreements());
    for f in failures.iter().take(3) {
        log!("variants: panel sweep: {f}");
    }
    log!("variants: panel {serial} measured: {} controls, {} issues", measured.len(), issues.len());
    // A deep make's panel (its fix is not asked): the controls that change
    // nothing visible at their own moments taken out, mechanically.
    if page.motion.variants.panel_fixes.iter().any(|(s, f, _)| *s == serial && *f == 0) {
        let invisible: Vec<String> = measured.iter().filter(|m| m.changed < super::panel_sweep::INVISIBLE && !m.control.starts_with("macro ")).map(|m| m.control.clone()).collect();
        prune_controls(page, &invisible);
        return;
    }
    ask_panel_fix(page, cx, serial, issues);
}

/// These paths' controls taken out of the panel (one undo step): a line
/// that is one control on one of them.
fn prune_controls(page: &mut VideoCut, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let Some(range) = makepad_editor_panel::panel_region(&session.source, &session.code) else { return };
    let panel = &session.source[range];
    let mut dropped = 0;
    let kept: Vec<&str> = panel.lines().filter(|line| {
        let t = line.trim();
        let gone = t.starts_with("Panel") && !t.starts_with("PanelModule") && !t.starts_with("PanelGroup") && t.ends_with('}') && t.matches('{').count() == 1
            && paths.iter().any(|p| t.contains(&format!("path: \"{p}\"")));
        if gone {
            dropped += 1;
        }
        !gone
    }).collect();
    if dropped == 0 {
        return;
    }
    let code = kept.join("\n");
    match write_panel(page, &code) {
        Ok(_) => log!("variants: {dropped} controls with no visible effect taken out of the panel"),
        Err(why) => log!("variants: the panel was not pruned: {why}"),
    }
}

/// What the panel slot shows while a panel is made, or why it was not:
/// (text, still making).
pub fn panel_progress(page: &VideoCut) -> Option<(String, bool)> {
    let request = page.motion.variants.requests.iter().rev().find(|r| r.panel)?;
    let state = request.revise.as_ref()?;
    if let Some(why) = &state.failed {
        return Some((format!("The editor could not be made: {why}"), false));
    }
    if state.applied.is_some() {
        return None;
    }
    let mut titles = Vec::new();
    for w in &request.writers {
        let mut rest = w.text.as_str();
        while let Some(at) = rest.find("title: \"") {
            rest = &rest[at + 8..];
            if let Some(end) = rest.find('"') {
                titles.push(rest[..end].to_string());
            }
        }
    }
    let written: usize = request.writers.iter().map(|w| w.text.len()).sum();
    let secs = Cx::time_now() - request.started;
    let what = if titles.is_empty() { if written == 0 { "reading the scene".to_string() } else { "writing".to_string() } } else { titles.join(", ") };
    Some((format!("Making this scene's editor{} {:.0} s\n{what}", ".".repeat(1 + (secs as usize) % 3), secs), true))
}

// ------------------------------------------------------------------ the deep make's requests

/// Writers answering in words the host reads (the deep make's audits): one
/// per text, each in its own fresh conversation under `system`.
pub(super) fn raw_request(page: &mut VideoCut, cx: &mut Cx, label: &str, system: &str, texts: Vec<String>) -> Result<u64, String> {
    let session = page.motion.desk.session.as_ref().ok_or("no graphic is open")?;
    let scene = format!("{}#{label}", session.node.clone().unwrap_or_else(|| session.title.clone()));
    let model = model_spec(None);
    let backend = backend_named(&model);
    let make = Make { backend: backend.clone(), system: Arc::from(system) };
    let variants = &mut page.motion.variants;
    if variants.writers.is_none() {
        variants.writers = Some(Writers::new(cx)?);
    }
    if let Some(reason) = backend.unavailable() {
        return Err(reason);
    }
    let serial = variants.next_request + 1;
    let mut writers = Vec::new();
    for (k, text) in texts.into_iter().enumerate() {
        let at = variants.session_for(&scene, false, k);
        let conversation = &mut variants.sessions[at];
        conversation.chars = text.len();
        conversation.busy = true;
        conversation.used = Cx::time_now();
        let (session_id, slot, running) = (conversation.id, conversation.slot, conversation.running.clone());
        let sent = text.len();
        variants.next_writer += 1;
        let (key, cancel) = (variants.next_writer, Arc::new(AtomicBool::new(false)));
        if let Some(pool) = &mut variants.writers {
            let done = pool.done_tx.clone();
            pool.send(slot, WriteJob::Turn { session: session_id, fresh: true, make: make.clone(), text, key, done, cancel: cancel.clone(), busy: running.clone() });
        }
        writers.push(Writer { key, session: session_id, slot, make: make.clone(), running, guides: 0, sent, cancel, text: String::new(), taken: 0, done: false, closed: false, numbers: 0..0 });
    }
    variants.next_request = serial;
    let context_chars = writers.iter().map(|w| w.sent).sum();
    log!("variants: {label} request {serial}: sent {:?} chars (system {} chars)", writers.iter().map(|w| w.sent).collect::<Vec<_>>(), make.system.len());
    variants.requests.push(Request { serial, ask: label.into(), n: 1, model, scope: label.into(), started: Cx::time_now(), writers, first: None, loaded: None, all: None, failures: Vec::new(), context_chars, revise: None, palette: false, panel: false, raw: true, internal: true });
    Ok(serial)
}

/// A raw request's answers once every writer is done (or why it failed).
pub(super) fn raw_answers(page: &VideoCut, serial: u64) -> Option<Result<Vec<String>, String>> {
    let request = page.motion.variants.requests.iter().find(|r| r.serial == serial)?;
    if request.writing() {
        return None;
    }
    Some(match request.failures.first() {
        Some(error) if request.writers.iter().all(|w| w.text.trim().is_empty()) => Err(error.clone()),
        _ => Ok(request.writers.iter().map(|w| w.text.clone()).collect()),
    })
}

/// Variants written by the scene's agent (forks), one per head, each shown
/// the code lines it is about: the deep make's exposures. Each answers one
/// block, drawn in the strip like any variant.
pub(super) fn variant_request(page: &mut VideoCut, cx: &mut Cx, label: &str, heads: Vec<(String, Vec<BTreeSet<usize>>)>) -> Result<u64, String> {
    let session = page.motion.desk.session.as_ref().ok_or("no graphic is open")?;
    let files = files(session);
    let scene = session.node.clone().unwrap_or_else(|| session.title.clone());
    let model = model_spec(None);
    let backend = backend_named(&model);
    let make = Make { backend: backend.clone(), system: Arc::from(scene_system(session).as_str()) };
    let variants = &mut page.motion.variants;
    if variants.loader.is_none() {
        variants.loader = Some(Loader::new(cx)?);
    }
    if variants.writers.is_none() {
        variants.writers = Some(Writers::new(cx)?);
    }
    if let Some(reason) = backend.unavailable() {
        return Err(reason);
    }
    let serial = variants.next_request + 1;
    let first_number = variants.list.iter().map(|v| v.number).max().unwrap_or(0) + 1;
    let n = heads.len();
    let mut writers = Vec::new();
    for (k, (head, wanted)) in heads.into_iter().enumerate() {
        let numbers = first_number + k..first_number + k + 1;
        let head = head.replace("{number}", &numbers.start.to_string());
        let at = variants.session_for(&format!("{scene}#expose"), false, k);
        let conversation = &mut variants.sessions[at];
        conversation.seen.clear();
        conversation.chars = 0;
        let text = session_message(conversation, &files, &wanted, &head).unwrap_or_default();
        conversation.busy = true;
        conversation.used = Cx::time_now();
        conversation.chars += text.len();
        let (session_id, slot, running) = (conversation.id, conversation.slot, conversation.running.clone());
        let sent = text.len();
        variants.next_writer += 1;
        let (key, cancel) = (variants.next_writer, Arc::new(AtomicBool::new(false)));
        if let Some(pool) = &mut variants.writers {
            let done = pool.done_tx.clone();
            pool.send(slot, WriteJob::Turn { session: session_id, fresh: true, make: make.clone(), text, key, done, cancel: cancel.clone(), busy: running.clone() });
        }
        writers.push(Writer { key, session: session_id, slot, make: make.clone(), running, guides: 0, sent, cancel, text: String::new(), taken: 0, done: false, closed: false, numbers });
    }
    variants.next_request = serial;
    let context_chars = writers.iter().map(|w| w.sent).sum();
    log!("variants: {label} request {serial}: {n} writers, sent {:?} chars", writers.iter().map(|w| w.sent).collect::<Vec<_>>());
    variants.requests.push(Request { serial, ask: label.into(), n, model, scope: label.into(), started: Cx::time_now(), writers, first: None, loaded: None, all: None, failures: Vec::new(), context_chars, revise: None, palette: false, panel: false, raw: false, internal: true });
    Ok(serial)
}

/// A variant request's variants once written and loaded: (id, writer
/// index, label, its texts (session file index, text) when it loaded, else
/// why not).
pub(super) fn request_variants(page: &VideoCut, serial: u64) -> Option<Vec<(u64, usize, String, Result<Vec<(usize, String)>, String>)>> {
    let request = page.motion.variants.requests.iter().find(|r| r.serial == serial)?;
    let mine: Vec<&Variant> = page.motion.variants.list.iter().filter(|v| v.request == serial).collect();
    if request.writing() || mine.iter().any(|v| v.loading) {
        return None;
    }
    let session = page.motion.desk.session.as_ref()?;
    let first = request.writers.first().map_or(0, |w| w.numbers.start);
    Some(mine.iter().map(|v| {
        let texts = match (&v.error, &v.doc) {
            (Some(e), _) => Err(e.clone()),
            (None, Some(_)) => patched(session, &v.hunks.iter().collect::<Vec<_>>()),
            (None, None) => Err("it did not load".into()),
        };
        (v.id, v.number.saturating_sub(first), v.label.clone(), texts)
    }).collect())
}

/// These variants applied together, one undo step.
pub(super) fn apply_variants(page: &mut VideoCut, ids: &[u64]) -> Result<String, String> {
    apply(page, ids)
}

/// These variants' patches together: the files they change.
pub(super) fn combined_texts(page: &VideoCut, ids: &[u64]) -> Result<Vec<(usize, String)>, String> {
    let session = page.motion.desk.session.as_ref().ok_or("no graphic is open")?;
    let hunks: Vec<&Hunk> = ids.iter().filter_map(|id| page.motion.variants.list.iter().find(|v| v.id == *id)).flat_map(|v| v.hunks.iter()).collect();
    patched(session, &hunks)
}

/// The most of these variants (in order) whose patches apply together:
/// one that touches lines another already changes is left out.
pub(super) fn compatible(page: &VideoCut, ids: &[u64]) -> Vec<u64> {
    let Some(session) = page.motion.desk.session.as_ref() else { return Vec::new() };
    let mut chosen: Vec<&Variant> = Vec::new();
    for id in ids {
        let Some(v) = page.motion.variants.list.iter().find(|v| v.id == *id) else { continue };
        let hunks: Vec<&Hunk> = chosen.iter().flat_map(|c| c.hunks.iter()).chain(v.hunks.iter()).collect();
        if patched(session, &hunks).is_ok() {
            chosen.push(v);
        }
    }
    chosen.iter().map(|v| v.id).collect()
}

/// The code lines round these (session file, line) places, for a writer.
pub(super) fn lines_around(page: &VideoCut, places: &[(usize, usize)], margin: usize) -> Vec<BTreeSet<usize>> {
    let Some(session) = page.motion.desk.session.as_ref() else { return Vec::new() };
    let files = files(session);
    let mut sets = vec![BTreeSet::new(); files.len()];
    for &(file, line) in places {
        if let Some((_, _, text)) = files.get(file) {
            let n = text.split('\n').count();
            for i in line.saturating_sub(margin)..(line + margin + 1).min(n) {
                sets[file].insert(i);
            }
        }
    }
    sets
}

/// The scene's files as the deep make names them (session index, name).
pub(super) fn file_names(page: &VideoCut) -> Vec<(usize, String)> {
    page.motion.desk.session.as_ref().map(|s| files(s).into_iter().map(|(i, n, _)| (i, n)).collect()).unwrap_or_default()
}

/// The panel writers' system text (the deep make's parts too).
pub(super) fn panel_system_text() -> &'static str {
    panel_system()
}

/// A raw request's answers so far: (writer index, text) of each done, and
/// whether all are.
pub(super) fn raw_parts(page: &VideoCut, serial: u64) -> Option<(Vec<(usize, String)>, bool)> {
    let request = page.motion.variants.requests.iter().find(|r| r.serial == serial)?;
    Some((request.writers.iter().enumerate().filter(|(_, w)| w.done).map(|(k, w)| (k, w.text.clone())).collect(), !request.writing()))
}

/// Measured like a made panel (the deep make's end); its findings are
/// reported, not sent back as a whole-panel fix (that would rewrite the
/// elements' modules it did not make).
pub(super) fn self_check(page: &mut VideoCut, cx: &mut Cx, serial: u64) {
    page.motion.variants.panel_fixes.push((serial, 0, 0));
    panel_self_check(page, cx, serial);
}

/// One module put into the scene's panel: the module titled `title`
/// replaced, or added (the overview first, others last); the panel made
/// when there is none. Joins the burst `key` (one undo step).
pub(super) fn panel_put_module(page: &mut VideoCut, title: &str, module: &str, key: &str, fresh: bool) -> Result<u64, String> {
    let session = page.motion.desk.session.as_ref().ok_or("no graphic is open")?;
    let region = makepad_editor_panel::panel_region(&session.source, &session.code);
    let indented: String = module.lines().map(|l| format!("        {l}")).collect::<Vec<_>>().join("\n");
    if region.is_none() && !fresh && session.source.contains("\nlet editor_panel") {
        return Err("the panel is written but its code does not read now".into());
    }
    let code = match region.filter(|_| !fresh) {
        None => format!("let editor_panel = fn() {{\n    View{{\n        flow: Down width: Fill spacing: 10\n        PanelHeader{{}}\n{indented}\n    }}\n}}"),
        Some(range) => {
            let panel = session.source[range].to_string();
            let b = panel.as_bytes();
            // Its module of that title: from `PanelModule{title: "…"` to
            // its closing brace.
            let start = panel.find(&format!("PanelModule{{title: \"{title}\""));
            let end_of = |from: usize| -> Option<usize> {
                let mut depth = 0i32;
                let mut in_str = false;
                for (k, &c) in b.iter().enumerate().skip(from) {
                    match c {
                        b'"' if k == 0 || b[k - 1] != b'\\' => in_str = !in_str,
                        b'{' if !in_str => depth += 1,
                        b'}' if !in_str => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(k + 1);
                            }
                        }
                        _ => {}
                    }
                }
                None
            };
            match start.and_then(|a| Some((a, end_of(a)?))) {
                Some((a, e)) => format!("{}{}{}", &panel[..a], module.lines().enumerate().map(|(i, l)| if i == 0 { l.to_string() } else { format!("        {l}") }).collect::<Vec<_>>().join("\n"), &panel[e..]),
                None if title == "Overview" => {
                    // After the header (or the layout line).
                    let at = panel.find("PanelHeader{}").map(|i| i + "PanelHeader{}".len()).or_else(|| panel.find("spacing: 10").map(|i| i + "spacing: 10".len())).ok_or("the panel's layout line is not where it is written")?;
                    format!("{}\n{indented}{}", &panel[..at], &panel[at..])
                }
                None => {
                    // Before the View's and the fn's closing braces.
                    let last = panel.rfind('}').ok_or("the panel has no end")?;
                    let view_end = panel[..last].rfind('}').ok_or("the panel has no end")?;
                    let line_start = panel[..view_end].rfind('\n').map_or(view_end, |n| n + 1);
                    format!("{}{indented}\n{}", &panel[..line_start], &panel[line_start..])
                }
            }
        }
    };
    write_panel_as(page, &code, Some(key))
}

/// How some paths were at a history step, put onto the graphic now as a
/// variant in the bar (beside the graphic as it is): Choose applies it as
/// one undo step. What the chat says.
pub(super) fn merge_from_history(page: &mut VideoCut, cx: &mut Cx, step: &str, paths: &[String]) -> Result<String, String> {
    let session = page.motion.desk.session.as_ref().ok_or("no graphic is open")?;
    let paths: Vec<String> = if paths.is_empty() || paths.iter().any(|p| p == "selection") {
        let (_, things) = super::desk::selection(page);
        things.iter().flat_map(|s| s.paths.iter().cloned()).collect()
    } else {
        paths.to_vec()
    };
    if paths.is_empty() {
        return Err("say what to take (paths, or select it in the picture)".into());
    }
    let (label, changes, skipped) = super::history::merge_hunks(session, step, &paths)?;
    if changes.is_empty() {
        return Err(format!("nothing to take: {}", if skipped.is_empty() { "it is the same then and now".into() } else { skipped.join("; ") }));
    }
    let files = files(session);
    let hunks: Vec<Hunk> = changes.into_iter().map(|(doc, now, then)| Hunk { doc, file: files[doc].1.clone(), old: now.split('\n').map(str::to_string).collect(), new: then.split('\n').map(str::to_string).collect() }).collect();
    let texts = patched(session, &hunks.iter().collect::<Vec<_>>());
    let variants = &mut page.motion.variants;
    if variants.loader.is_none() {
        variants.loader = Some(Loader::new(cx)?);
    }
    let serial = variants.next_request + 1;
    variants.next_request = serial;
    variants.open = true;
    let first = variants.list.iter().map(|v| v.number).max().unwrap_or(0) + 1;
    let what = paths.join(", ");
    variants.requests.push(Request { serial, ask: format!("from {label}: {what}"), n: 1, model: "history".into(), scope: what.clone(), started: Cx::time_now(), writers: Vec::new(), first: None, loaded: None, all: None, failures: Vec::new(), context_chars: 0, revise: None, palette: false, panel: false, raw: false, internal: false });
    add_variant(page, serial, first, format!("From {label}"), format!("{what} as then, the rest as now"), hunks, None, texts);
    log!("variants: a merge from {label} of {what}{}", if skipped.is_empty() { String::new() } else { format!(" ({})", skipped.join("; ")) });
    Ok(format!("{what} as at {label}, the rest as now: in the variant bar beside the original; Choose to keep it.{}", if skipped.is_empty() { String::new() } else { format!(" ({})", skipped.join("; ")) }))
}

/// A panel control asking the scene's agent (`scene.ask`): `replace` and
/// `extend` are a change made now (one undo step), `variants` cards in the
/// strip; `target` the paths it is about (or the selection's).
pub(super) fn panel_ask(page: &mut VideoCut, cx: &mut Cx, prompt: &str, target: &[String], mode: &str) -> Result<u64, String> {
    // A module's "dig deeper": the deep make of its element.
    if prompt.to_lowercase().contains("dig deeper") || prompt.to_lowercase().contains("more controls") {
        super::deep_make::start_element(page, cx, "", target, "")?;
        return Ok(page.motion.variants.next_request);
    }
    let about = if target.is_empty() {
        let (_, things) = super::desk::selection(page);
        things.iter().flat_map(|s| s.paths.iter().cloned()).collect::<Vec<_>>()
    } else {
        target.to_vec()
    };
    let about = if about.is_empty() { String::new() } else { format!("\nIt is about these paths: {}", about.join(", ")) };
    match mode {
        "variants" => ask(page, cx, &format!("{prompt}{about}"), Mode::Variants { n: None, split: None, seed: None }, Target::Named, None),
        "extend" => ask(page, cx, &format!("{prompt} (add to what is there; keep it){about}"), Mode::Revise { retry: None }, Target::Named, None),
        _ => ask(page, cx, &format!("{prompt}{about}"), Mode::Revise { retry: None }, Target::Named, None),
    }
}

thread_local! {
    /// Asks a panel's controls made (id, prompt, targets, mode), for the
    /// desk's next pump; and each ask's state as the panel reads it.
    static PANEL_ASKS: std::cell::RefCell<Vec<(u64, String, Vec<String>, String)>> = const { std::cell::RefCell::new(Vec::new()) };
    static PANEL_ASK_STATE: std::cell::RefCell<Vec<(u64, Option<u64>, String)>> = const { std::cell::RefCell::new(Vec::new()) };
    static PANEL_ASK_NEXT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The panel host's `ask`: queued for the desk; its id.
pub fn queue_panel_ask(prompt: &str, targets: &[String], mode: &str) -> Option<u64> {
    let id = PANEL_ASK_NEXT.with(|n| { n.set(n.get() + 1); n.get() });
    PANEL_ASKS.with(|q| q.borrow_mut().push((id, prompt.to_string(), targets.to_vec(), mode.to_string())));
    PANEL_ASK_STATE.with(|s| s.borrow_mut().push((id, None, "working".into())));
    Some(id)
}

/// The panel host's `ask_status`.
pub fn panel_ask_status(id: u64) -> String {
    PANEL_ASK_STATE.with(|s| s.borrow().iter().find(|(i, _, _)| *i == id).map_or_else(|| "failed: no such ask".to_string(), |(_, _, t)| t.clone()))
}

/// The queued asks started, and the running ones' states read.
fn pump_panel_asks(page: &mut VideoCut, cx: &mut Cx) {
    let queued: Vec<(u64, String, Vec<String>, String)> = PANEL_ASKS.with(|q| std::mem::take(&mut *q.borrow_mut()));
    for (id, prompt, targets, mode) in queued {
        let started = panel_ask(page, cx, &prompt, &targets, &mode);
        PANEL_ASK_STATE.with(|s| {
            if let Some(entry) = s.borrow_mut().iter_mut().find(|(i, _, _)| *i == id) {
                match started {
                    Ok(serial) => entry.1 = Some(serial),
                    Err(error) => entry.2 = format!("failed: {error}"),
                }
            }
        });
    }
    let running: Vec<(u64, u64)> = PANEL_ASK_STATE.with(|s| s.borrow().iter().filter(|(_, r, t)| r.is_some() && t == "working").map(|(i, r, _)| (*i, r.unwrap_or(0))).collect());
    for (id, serial) in running {
        if let Some(outcome) = panel_ask_outcome(page, serial) {
            let text = if outcome.starts_with("Could not") { format!("failed: {outcome}") } else if outcome.starts_with("done") || outcome.starts_with("failed") { outcome } else { format!("done: {outcome}") };
            PANEL_ASK_STATE.with(|s| {
                if let Some(entry) = s.borrow_mut().iter_mut().find(|(i, _, _)| *i == id) {
                    entry.2 = text;
                }
            });
        }
    }
}

/// How a panel's ask went: None while working.
pub(super) fn panel_ask_outcome(page: &VideoCut, serial: u64) -> Option<String> {
    let request = page.motion.variants.requests.iter().find(|r| r.serial == serial)?;
    if request.revise.is_some() {
        return change_outcome(page, serial);
    }
    if request.writing() {
        return None;
    }
    let made = page.motion.variants.list.iter().filter(|v| v.request == serial && v.error.is_none()).count();
    Some(if made > 0 { format!("done: {made} variants in the strip") } else { format!("failed: {}", request.failures.first().cloned().unwrap_or_else(|| "no variant loaded".into())) })
}

/// How a change asked in the chat went: None while the agent works.
pub(super) fn change_outcome(page: &VideoCut, serial: u64) -> Option<String> {
    // A change tried again is answered by its last try.
    let request = page.motion.variants.requests.iter().rev().find(|r| r.revise.as_ref().is_some_and(|s| s.origin == serial) || r.serial == serial)?;
    let state = request.revise.as_ref()?;
    let secs = state.at.map(|t| format!(" ({t:.1} s)")).unwrap_or_default();
    if let Some(what) = &state.applied {
        return Some(format!("Done: {what}{secs}. Undo: cmd-Z."));
    }
    if let Some(why) = &state.escalated {
        return Some(format!("A bigger change ({why}): Generate is on it."));
    }
    if let Some(answer) = &state.answered {
        return Some(format!("{answer}{secs}"));
    }
    match &state.failed {
        Some(why) if !why.starts_with("tried again") => Some(format!("Could not: {why}{secs}")),
        _ => None,
    }
}

/// A panel's ranges put on the scale of the values they move: a control's
/// `min`/`max` and each macro target's two ends hold the value written now
/// and stay within sixteen times it either way (zero is a fair end) (a macro mapping a 64px sheet
/// to 0..1 shrank the sheet to nothing); one that does not becomes a
/// quarter to four times the value (16px..256px for 64px). A range that
/// already fits is kept.
pub(super) fn fit_ranges(session: &MotionSession, code: &str) -> String {
    let files: Vec<makepad_motion::paths::File> = std::iter::once(makepad_motion::paths::File { name: "", text: &session.source, code: &session.code })
        .chain(session.modules.iter().filter(|m| !m.builtin()).map(|m| makepad_motion::paths::File { name: &m.name, text: &m.source, code: &m.code }))
        .collect();
    fit_ranges_in(&files, code)
}

/// `fit_ranges` over these files.
fn fit_ranges_in(files: &[makepad_motion::paths::File], code: &str) -> String {
    use makepad_motion::paths;
    let number = |t: &str| -> Option<f64> {
        let t = t.trim();
        let digits = t.trim_start_matches('-');
        let end = digits.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(digits.len());
        digits[..end].parse::<f64>().ok().map(|v| if t.starts_with('-') { -v } else { v })
    };
    let value_at = |path: &str| paths::resolve(files, path).ok().filter(|site| matches!(site.kind, ValueKind::Number | ValueKind::Time | ValueKind::Angle | ValueKind::Length)).and_then(|site| number(&site.text));
    // The range a value may move over, and whether (a, b) sits in it.
    // A range holds the value and reaches no further than sixteen times
    // it either way (crossing zero is fair: a tilt, an offset; a share
    // of one, 0..1, is fair for any small value).
    let fits = |v: f64, a: f64, b: f64| { let (x, y) = (a.min(b), a.max(b)); let reach = if v == 0.0 { 16.0 } else { v.abs() * 16.0 }; x <= v + 1e-9 && y >= v - 1e-9 && (x.abs().max(y.abs()) <= reach + 1e-9 || (x >= -1.0 && y <= 1.0)) };
    let fitted = |v: f64, a: f64, b: f64| -> (f64, f64) {
        let (x, y) = if v == 0.0 { (-1.0, 1.0) } else if v > 0.0 { (v / 4.0, v * 4.0) } else { (v * 4.0, v / 4.0) };
        if a <= b { (x, y) } else { (y, x) }
    };
    let fmt = |v: f64| makepad_motion::patch::fmt_num((v * 1000.0).round() / 1000.0);
    let field = |line: &str, name: &str| -> Option<(std::ops::Range<usize>, String)> {
        let key = format!("{name}: ");
        let at = line.find(&key)? + key.len();
        let rest = &line[at..];
        let (start, end) = match rest.strip_prefix('"') { Some(q) => (at + 1, at + 1 + q.find('"')?), None => (at, at + rest.find([' ', '}']).unwrap_or(rest.len())) };
        Some((start..end, line[start..end].to_string()))
    };
    let mut out = Vec::new();
    for line in code.split('\n') {
        let mut line = line.to_string();
        if let Some((range, targets)) = field(&line, "targets") {
            let fixed: Vec<String> = targets.split(',').map(|t| {
                let w: Vec<&str> = t.split_whitespace().collect();
                // Ends are plain numbers (the value keeps its unit when
                // written): `48px` read as 48, written 48.
                match (w.first(), w.get(1).and_then(|a| number(a)), w.get(2).and_then(|b| number(b))) {
                    (Some(p), Some(a), Some(b)) => {
                        let (a, b) = match value_at(p) {
                            Some(v) if !fits(v, a, b) => fitted(v, a, b),
                            _ => (a, b),
                        };
                        format!("{p} {} {}", fmt(a), fmt(b))
                    }
                    _ => t.trim().to_string(),
                }
            }).collect();
            line.replace_range(range, &fixed.join(", "));
        } else if let Some((_, path)) = field(&line, "path") {
            if let (Some(v), Some((ra, a)), Some((rb, b))) = (value_at(&path), field(&line, "min"), field(&line, "max")) {
                if let (Some(a), Some(b)) = (a.parse::<f64>().ok(), b.parse::<f64>().ok()) {
                    if !fits(v, a, b) {
                        let (lo, hi) = fitted(v, a, b);
                        // (The later field first, so the earlier's bytes hold.)
                        let (first, second) = if ra.start < rb.start { ((ra, lo), (rb, hi)) } else { ((rb, hi), (ra, lo)) };
                        line.replace_range(second.0, &fmt(second.1));
                        line.replace_range(first.0, &fmt(first.1));
                    }
                }
            }
        }
        out.push(line);
    }
    out.join("\n")
}

/// Write (or replace) the scene's `let editor_panel` with `code`, as one
/// undo step: what an agent installs headless.
fn write_panel(page: &mut VideoCut, code: &str) -> Result<u64, String> {
    write_panel_as(page, code, None)
}

/// `write_panel`, joining the burst `key` (one undo step for a panel
/// written in as its parts arrive).
fn write_panel_as(page: &mut VideoCut, code: &str, key: Option<&str>) -> Result<u64, String> {
    // Every range on the scale of the value it moves.
    let fitted = page.motion.desk.session.as_ref().map(|s| fit_ranges(s, code));
    let code = fitted.as_deref().unwrap_or(code).trim();
    if !code.starts_with("let editor_panel") {
        return Err("the code is the whole `let editor_panel = fn() { View{…} }`".into());
    }
    let session = page.motion.desk.session.as_mut().ok_or("no graphic is open")?;
    let text = session.source.clone();
    let next = makepad_editor_panel::with_panel(&text, code, "Motion{")?;
    let changed = match key {
        Some(key) => session.replace_burst(next, "editor panel", key),
        None => session.replace(next, "editor panel"),
    };
    if changed {
        let revision = session.revision;
        super::desk::changed(page);
        page.motion.desk.repaint = true;
        return Ok(revision);
    }
    Ok(session.revision)
}

/// Every function slot's alternatives as variants, at once (no model):
/// the slot's one name changed (`creature: dragon_builder`). `only`: the
/// slots written under that name.
fn swap_variants(page: &mut VideoCut, cx: &mut Cx, only: Option<&str>) -> Result<u64, String> {
    let session = page.motion.desk.session.as_ref().ok_or("no graphic is open")?;
    let slots: Vec<super::swaps::Slot> = super::swaps::slots(session).into_iter().filter(|s| only.is_none_or(|o| s.what == o)).collect();
    if slots.is_empty() {
        return Err("the scene plugs in no function that has an alternative of the same shape".into());
    }
    let files = files(session);
    let mut made: Vec<(String, String, Vec<Hunk>)> = Vec::new();
    for slot in &slots {
        let text = &files[slot.doc].2;
        let (a, b) = (text[..slot.range.start].rfind('\n').map_or(0, |n| n + 1), text[slot.range.end..].find('\n').map_or(text.len(), |n| slot.range.end + n));
        let line = &text[a..b];
        for alt in &slot.alternatives {
            let new = format!("{}{alt}{}", &text[a..slot.range.start], &text[slot.range.end..b]);
            made.push((format!("{} · {alt}", slot.what), format!("{} instead of {}", alt, slot.current), vec![Hunk { doc: slot.doc, file: files[slot.doc].1.clone(), old: vec![line.to_string()], new: vec![new] }]));
        }
    }
    let variants = &mut page.motion.variants;
    if variants.loader.is_none() {
        variants.loader = Some(Loader::new(cx)?);
    }
    let serial = variants.next_request + 1;
    variants.next_request = serial;
    variants.open = true;
    let first = variants.list.iter().map(|v| v.number).max().unwrap_or(0) + 1;
    variants.requests.push(Request { serial, ask: "function swaps".into(), n: made.len(), model: "local".into(), scope: "the function slots".into(), started: Cx::time_now(), writers: Vec::new(), first: None, loaded: None, all: None, failures: Vec::new(), context_chars: 0, revise: None, palette: false, panel: false, raw: false, internal: false });
    for (k, (label, note, hunks)) in made.into_iter().enumerate() {
        let texts = page.motion.desk.session.as_ref().ok_or("no graphic is open".to_string()).and_then(|session| patched(session, &hunks.iter().collect::<Vec<_>>()));
        add_variant(page, serial, first + k, label, note, hunks, None, texts);
    }
    Ok(serial)
}

/// A saved palette over the scene's, as a variant to preview and apply.
fn palette_saved(page: &mut VideoCut, cx: &mut Cx, label: &str, saved: &[(String, [f64; 4])]) -> Result<u64, String> {
    let session = page.motion.desk.session.as_ref().ok_or("no graphic is open")?;
    let map = super::palette::map_saved(&super::palette::palette(session), saved);
    let texts = super::palette::apply_map(session, &map);
    let variants = &mut page.motion.variants;
    if variants.loader.is_none() {
        variants.loader = Some(Loader::new(cx)?);
    }
    let serial = variants.next_request + 1;
    variants.next_request = serial;
    variants.open = true;
    let number = variants.list.iter().map(|v| v.number).max().unwrap_or(0) + 1;
    variants.requests.push(Request { serial, ask: format!("palette {label}"), n: 1, model: "saved".into(), scope: "the palette".into(), started: Cx::time_now(), writers: Vec::new(), first: None, loaded: None, all: None, failures: Vec::new(), context_chars: 0, revise: None, palette: true, panel: false, raw: false, internal: false });
    add_variant(page, serial, number, label.to_string(), "a saved palette, by name and by role".into(), Vec::new(), Some(map), texts);
    Ok(serial)
}

/// How a saved palette's title starts in the library.
const PALETTE_TITLE: &str = "Palette · ";

/// The control API calls the strip wants made (the app runs them).
pub fn take_calls(page: &mut VideoCut) -> Vec<(String, &'static str, Json)> {
    std::mem::take(&mut page.motion.variants.calls)
}

/// A call the app started for the strip (its tag and id).
pub fn track_call(page: &mut VideoCut, tag: String, id: u64) {
    page.motion.variants.running_calls.push((tag, id));
}

/// The calls the strip waits for.
pub fn running_calls(page: &VideoCut) -> Vec<(String, u64)> {
    page.motion.variants.running_calls.clone()
}

/// A strip call's answer: a palette saved, the saved palettes found, one
/// read (made a variant over this scene).
pub fn call_done(page: &mut VideoCut, cx: &mut Cx, id: u64, result: Result<Json, String>) {
    let Some(at) = page.motion.variants.running_calls.iter().position(|(_, i)| *i == id) else { return };
    let (tag, _) = page.motion.variants.running_calls.remove(at);
    let status = match (tag.as_str(), result) {
        (_, Err(error)) => Some(format!("Library: {error}")),
        ("save", Ok(_)) => Some("Palette saved to the library".to_string()),
        ("search", Ok(found)) => {
            let items: Vec<(String, String)> = found.get("items").and_then(Json::as_arr).unwrap_or(&[]).iter().filter_map(|item| {
                let title = item.get("title").and_then(Json::as_str)?.strip_prefix(PALETTE_TITLE)?;
                let (id, rev) = (item.get("asset_id").and_then(Json::as_str)?, item.get("revision").and_then(Json::as_str)?);
                Some((title.to_string(), format!("asset:{id}:{rev}")))
            }).collect();
            for (title, name) in &items {
                page.motion.variants.calls.push((format!("read:{title}"), "read_motion", json::obj(vec![("name", json::s(name.clone()))])));
            }
            Some(if items.is_empty() { "No palettes saved in the library yet".to_string() } else { format!("{} saved palette{}…", items.len(), if items.len() == 1 { "" } else { "s" }) })
        }
        (tag, Ok(read)) => match tag.strip_prefix("read:") {
            Some(title) => {
                let saved = super::palette::parse_saved(read.get("source").and_then(Json::as_str).unwrap_or(""));
                match palette_saved(page, cx, title, &saved) {
                    Ok(_) => None,
                    Err(error) => Some(error),
                }
            }
            None => None,
        },
    };
    if let Some(status) = status {
        page.motion.variants.status = status;
        page.motion.variants.cards_dirty = true;
    }
}


/// The open graphic's palette as a library module's source (`title`).
pub fn palette_module(page: &VideoCut, title: &str) -> Result<String, String> {
    let session = page.motion.desk.session.as_ref().ok_or("no graphic is open")?;
    Ok(super::palette::module_source(&super::palette::palette(session), title, &session.title))
}

/// The texts of the open graphic's files with a variant's (or several
/// variants') patches: (file index, text) of each file they change.
fn patched(session: &MotionSession, hunks: &[&Hunk]) -> Result<Vec<(usize, String)>, String> {
    let files = files(session);
    let mut out = Vec::new();
    for (index, _, text) in &files {
        let mine: Vec<&Hunk> = hunks.iter().copied().filter(|h| h.doc == *index).collect();
        if !mine.is_empty() {
            out.push((*index, apply_hunks(text, &mine)?));
        }
    }
    Ok(out)
}

/// A block that just closed becomes a variant: placed in the code and
/// sent to load.
fn take_block(page: &mut VideoCut, request: u64, number: usize, block: &str) {
    let (label, note, raw) = parse_block(block);
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let palette_ask = page.motion.variants.requests.iter().find(|r| r.serial == request).is_some_and(|r| r.palette);
    let mut hunks = Vec::new();
    let mut map = None;
    let texts = if palette_ask {
        // A palette: `old -> new` lines.
        let parsed = super::palette::parse_map(block);
        let result = if parsed.is_empty() { Err("it maps no colour".to_string()) } else { super::palette::apply_map(session, &parsed) };
        map = Some(parsed);
        result
    } else {
        let files = files(session);
        let mut error = None;
        for (file, old, new) in raw {
            match files.iter().find(|(_, name, _)| *name == file) {
                Some((index, name, _)) => hunks.push(Hunk { doc: *index, file: name.clone(), old, new }),
                None => error = Some(format!("it names a file that is not shown: {file}")),
            }
        }
        match error {
            Some(error) => Err(error),
            None if hunks.is_empty() => Err("it changes nothing".into()),
            None => patched(session, &hunks.iter().collect::<Vec<_>>()),
        }
    };
    add_variant(page, request, number, label, note, hunks, map, texts);
}

/// A variant made (from a block, or a palette move): placed and sent to
/// load, or kept with why it cannot be.
fn add_variant(page: &mut VideoCut, request: u64, number: usize, label: String, note: String, hunks: Vec<Hunk>, map: Option<Vec<(String, [f64; 4])>>, texts: Result<Vec<(usize, String)>, String>) {
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let (texts, error) = match texts {
        Ok(texts) => (Some(texts), None),
        Err(why) => (None, Some(why)),
    };
    let job = texts.map(|texts| {
        let source = texts.iter().find(|(i, _)| *i == 0).map(|(_, t)| t.clone()).unwrap_or_else(|| session.source.clone());
        let mut modules = session.edited_modules();
        for (index, text) in texts.iter().filter(|(i, _)| *i > 0) {
            let m = &session.modules[index - 1];
            modules.retain(|(id, rev, _)| !(*id == m.id && *rev == m.rev));
            modules.push((m.id.clone(), m.rev.clone(), text.clone()));
        }
        (source, modules)
    });
    let t = session.playhead;
    let scene = session.node.clone().or_else(|| Some(session.title.clone()));
    let variants = &mut page.motion.variants;
    // The variants are this graphic's (another one opened closes them).
    variants.of = scene;
    let started = variants.requests.iter().find(|r| r.serial == request).map_or(Cx::time_now(), |r| r.started);
    variants.next_id += 1;
    let id = variants.next_id;
    log!("variants: #{number} '{label}' arrived at {:.0} ms{}", (Cx::time_now() - started) * 1000.0, error.as_ref().map(|e| format!(" — refused: {e}")).unwrap_or_default());
    let loading = job.is_some();
    let swatches = match &map {
        Some(map) => map.iter().map(|(_, c)| [c[0] as f32, c[1] as f32, c[2] as f32, 1.0]).collect(),
        None => swatches(&hunks),
    };
    variants.list.push(Variant {
        id, request, number, label, note, hunks, map, error, doc: None, frame: None, given: None,
        arrived: Cx::time_now() - started, loaded: None, painted: None, load_ms: 0.0, swatches, loading,
    });
    if let (Some((source, modules)), Some(loader)) = (job, variants.loader.as_mut()) {
        loader.send(Job::Load { id, source, modules, t });
        variants.strip_t = Some(t);
    }
    variants.cards_dirty = true;
}

/// Writers whose model asked for a part of the guide get it and are asked
/// again; a change is applied once its block has loaded, tried once more
/// with the reason when it does not fit or load, and handed to Generate
/// when the agent says it is structural.
/// The code lines that mention any of these words (or names), with a few
/// lines round each, in the excerpt form (at most `CODE_LINES`).
fn code_lines(session: &MotionSession, words: &str) -> String {
    const CODE_LINES: usize = 200;
    let files = files(session);
    let wanted: Vec<String> = words.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| w.len() >= 3).map(|w| w.to_lowercase()).collect();
    let mut sets: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); files.len()];
    let mut total = 0;
    for (k, (_, _, text)) in files.iter().enumerate() {
        let lines: Vec<&str> = text.split('\n').collect();
        for (i, line) in lines.iter().enumerate() {
            let lower = line.to_lowercase();
            if wanted.iter().any(|w| lower.contains(w.as_str())) {
                for j in i.saturating_sub(3)..(i + 4).min(lines.len()) {
                    if total < CODE_LINES && sets[k].insert(j) {
                        total += 1;
                    }
                }
            }
        }
    }
    if total == 0 {
        return String::new();
    }
    excerpt(&files, &sets)
}

fn revisions(page: &mut VideoCut, cx: &mut Cx) {
    // Parts of the guide, and lines of the code, asked for.
    let session = page.motion.desk.session.as_ref();
    let variants = &mut page.motion.variants;
    let mut again = Vec::new();
    for request in &mut variants.requests {
        for writer in request.writers.iter_mut().filter(|w| w.done && w.taken == 0) {
            // The history, read (a merge is answered below, as the change).
            if let Some(read) = session.and_then(|s| super::history::answer(s, &writer.text)) {
                if writer.guides >= 4 {
                    continue;
                }
                writer.guides += 1;
                log!("variants: writer {} read the history", writer.key);
                writer.text.clear();
                writer.done = false;
                writer.closed = false;
                again.push((writer.slot, writer.session, writer.make.clone(), format!("{read}\nNow answer the ask above."), writer.key, writer.cancel.clone(), writer.running.clone()));
                continue;
            }
            if let Some(words) = writer.text.trim_start().strip_prefix("CODE:").map(|t| t.trim().to_string()) {
                if writer.guides >= 3 {
                    continue;
                }
                writer.guides += 1;
                let lines = session.map(|s| code_lines(s, &words)).unwrap_or_default();
                let text = if lines.is_empty() {
                    format!("No line of the code mentions {words}; answer the ask above with what you have.")
                } else {
                    format!("The code's lines that mention {words} (as written; copy them exactly):\n{lines}\nNow answer the ask above.")
                };
                log!("variants: writer {} asked for the code of '{words}'", writer.key);
                writer.text.clear();
                writer.done = false;
                writer.closed = false;
                again.push((writer.slot, writer.session, writer.make.clone(), text, writer.key, writer.cancel.clone(), writer.running.clone()));
                continue;
            }
            let Some(title) = writer.text.trim_start().strip_prefix("GUIDE:").map(|t| t.trim().trim_matches('`').to_string()) else { continue };
            if writer.guides >= 2 {
                continue;
            }
            writer.guides += 1;
            let part = guide_sections().iter().find(|(t, _)| t.to_lowercase().contains(&title.to_lowercase()) || title.to_lowercase().contains(&t.to_lowercase()));
            let text = match part {
                Some((t, body)) => format!("The guide's `{t}`:\n{body}\n\nNow answer the ask above."),
                None => "There is no such part of the guide; answer the ask above.".to_string(),
            };
            log!("variants: writer {} asked for the guide's '{title}'", writer.key);
            writer.text.clear();
            writer.done = false;
            writer.closed = false;
            again.push((writer.slot, writer.session, writer.make.clone(), text, writer.key, writer.cancel.clone(), writer.running.clone()));
        }
    }
    if let Some(pool) = &mut variants.writers {
        for (slot, session, make, text, key, cancel, running) in again {
            if let Some(conversation) = variants.sessions.iter_mut().find(|c| c.id == session) {
                conversation.busy = true;
                conversation.chars += text.len();
            }
            let done = pool.done_tx.clone();
            pool.send(slot, WriteJob::Turn { session, fresh: false, make, text, key, done, cancel, busy: running });
        }
    }
    panels_written(page, cx);
    let variants = &mut page.motion.variants;
    // Changes.
    let now = Cx::time_now();
    let mut apply_now = Vec::new();
    let mut retry = Vec::new();
    let mut escalate = Vec::new();
    let mut merges: Vec<(u64, String)> = Vec::new();
    for request in variants.requests.iter_mut().filter(|r| !r.panel && !r.raw) {
        let writing = request.writing();
        let Some(state) = request.revise.as_mut() else { continue };
        if state.applied.is_some() || state.failed.is_some() || state.escalated.is_some() || state.answered.is_some() {
            continue;
        }
        let writer = &request.writers[0];
        if writer.done && writer.taken == 0 && writer.guides < 4 && ["GUIDE:", "CODE:", "HISTORY:", "HISTORY_DIFF:"].iter().any(|p| writer.text.trim_start().starts_with(p)) {
            continue;
        }
        if let Some(merge) = writer.text.trim_start().strip_prefix("MERGE:") {
            if writer.done {
                merges.push((request.serial, merge.trim().to_string()));
            }
            continue;
        }
        if let Some(answer) = writer.text.trim_start().strip_prefix("ANSWER:") {
            if writer.done {
                state.answered = Some(answer.trim().to_string());
                state.at = Some(now - request.started);
                log!("variants: a question answered: {}", answer.trim());
            }
            continue;
        }
        if let Some(why) = writer.text.trim_start().strip_prefix("ESCALATE:") {
            if writer.done {
                state.escalated = Some(why.trim().to_string());
                state.at = Some(now - request.started);
                escalate.push((request.ask.clone(), why.trim().to_string()));
            }
            continue;
        }
        if let Some(error) = request.failures.first() {
            state.failed = Some(error.clone());
            state.at = Some(now - request.started);
            continue;
        }
        let mine: Vec<&Variant> = variants.list.iter().filter(|v| v.request == request.serial).collect();
        match mine.first() {
            Some(v) if v.error.is_none() && v.doc.is_some() => {
                apply_now.push((request.serial, v.id));
            }
            Some(v) if v.error.is_some() => {
                let why = v.error.clone().unwrap_or_default();
                if state.retried {
                    state.failed = Some(why);
                    state.at = Some(now - request.started);
                } else {
                    // Asked again once, below; this one ends here.
                    state.failed = Some(format!("tried again: {why}"));
                    retry.push((request.ask.clone(), state.origin, why));
                }
            }
            None if writer.done && !writing => {
                if state.retried {
                    state.failed = Some("the answer had no block".into());
                    state.at = Some(now - request.started);
                } else {
                    state.failed = Some("tried again: no block".into());
                    retry.push((request.ask.clone(), state.origin, "the answer had no block in the variant format".into()));
                }
            }
            _ => {}
        }
    }
    let mut checks = Vec::new();
    for (serial, id) in apply_now {
        let result = apply(page, &[id]);
        let variants = &mut page.motion.variants;
        if let Some(state) = variants.requests.iter_mut().find(|r| r.serial == serial).and_then(|r| { let started = r.started; r.revise.as_mut().map(|s| (s, started)) }) {
            let (state, started) = state;
            state.at = Some(now - started);
            match result {
                Ok(what) => {
                    log!("variants: change applied at {:.0} ms: {what}", (now - started) * 1000.0);
                    variants.status = format!("Done: {what} ({:.1} s; undo: cmd-Z)", now - started);
                    state.applied = Some(what);
                    // A change of the panel (not the self-check's own fix):
                    // its controls checked and measured like a made panel's.
                    let ask = variants.requests.iter().find(|r| r.serial == serial).map(|r| r.ask.clone()).unwrap_or_default();
                    if ask.contains("editor_panel") && !ask.starts_with("Fix the editor panel") {
                        checks.push(serial);
                    }
                }
                Err(error) => state.failed = Some(error),
            }
        }
        page.motion.variants.cards_dirty = true;
    }
    for serial in checks {
        panel_self_check(page, cx, serial);
    }
    for (asked, origin, why) in retry {
        log!("variants: change tried again: {why}");
        if let Err(error) = ask(page, cx, &asked, Mode::Revise { retry: Some((origin, why)) }, Target::Selection, None) {
            page.motion.variants.status = error;
        }
    }
    // A merge from the history: a variant to choose.
    for (serial, merge) in merges {
        let mut words = merge.split_whitespace().map(str::to_string).collect::<Vec<_>>();
        // ("N min ago" is one step.)
        let step = match words.iter().position(|w| w == "ago") {
            Some(at) if at <= 2 => words.drain(..=at).collect::<Vec<_>>().join(" "),
            _ if !words.is_empty() => words.remove(0),
            _ => String::new(),
        };
        let said = match merge_from_history(page, cx, &step, &words) {
            Ok(said) => said,
            Err(error) => format!("The merge did not work: {error}"),
        };
        if let Some(state) = page.motion.variants.requests.iter_mut().find(|r| r.serial == serial).and_then(|r| { let started = r.started; r.revise.as_mut().map(|s| (s, started)) }) {
            state.0.answered = Some(said);
            state.0.at = Some(Cx::time_now() - state.1);
        }
    }
    for (asked, why) in escalate {
        log!("variants: change handed to Generate: {why}");
        let status = match super::super::generate::ask_scene(page, &asked) {
            Ok(()) => format!("A bigger change ({why}): Generate is on it…"),
            Err(error) => error,
        };
        page.motion.variants.status = status;
        page.motion.variants.cards_dirty = true;
    }
}

/// End the conversations of graphics not asked about for a long while, or
/// closed a while ago (a brief switch keeps them).
fn expire_sessions(page: &mut VideoCut) {
    let now = Cx::time_now();
    let open = page.motion.desk.session.as_ref().map(|s| s.node.clone().unwrap_or_else(|| s.title.clone()));
    let variants = &mut page.motion.variants;
    for conversation in variants.sessions.iter_mut().filter(|c| open.as_ref() == Some(&c.scene)) {
        conversation.open_at = now;
    }
    let (keep, gone): (Vec<Session>, Vec<Session>) = std::mem::take(&mut variants.sessions).into_iter().partition(|c| {
        let open = open.as_ref() == Some(&c.scene);
        c.busy || c.running.load(Ordering::Relaxed) || if open { now - c.used < SESSION_IDLE } else { now - c.used.max(c.open_at) < SESSION_CLOSED && now - c.used < SESSION_IDLE }
    });
    variants.sessions = keep;
    for conversation in gone {
        log!("variants: conversation {} about {} ended", conversation.id, conversation.scene);
        if let Some(pool) = &mut variants.writers {
            pool.send(conversation.slot, WriteJob::End { session: conversation.id });
        }
    }
}

/// Close the strip: writers stop, variants go (what was applied stays).
fn close(page: &mut VideoCut) {
    let variants = &mut page.motion.variants;
    for request in &mut variants.requests {
        for writer in &mut request.writers {
            writer.cancel.store(true, Ordering::Relaxed);
        }
    }
    variants.requests.clear();
    variants.list.clear();
    variants.previewed = None;
    variants.original = None;
    variants.applied = None;
    variants.open = false;
    variants.status.clear();
    variants.cards_dirty = true;
    page.motion.desk.repaint = true;
}

/// Apply variants (one, or several as a mix) to the open graphic as one
/// undo step. Applying again right after replaces the last application.
fn apply(page: &mut VideoCut, ids: &[u64]) -> Result<String, String> {
    if ids.is_empty() {
        return Err("no variant named".into());
    }
    let chosen: Vec<&Variant> = ids.iter().map(|id| page.motion.variants.list.iter().find(|v| v.id == *id).ok_or(format!("no variant {id}"))).collect::<Result<_, _>>()?;
    if let Some(bad) = chosen.iter().find(|v| v.error.is_some()) {
        return Err(format!("variant {} cannot be applied: {}", bad.number, bad.error.clone().unwrap_or_default()));
    }
    let what = if chosen.len() == 1 {
        format!("variant {} · {}", chosen[0].number, chosen[0].label)
    } else {
        format!("mix of variants {}", chosen.iter().map(|v| v.number.to_string()).collect::<Vec<_>>().join(" + "))
    };
    let hunks: Vec<Hunk> = chosen.iter().flat_map(|v| v.hunks.iter().cloned()).collect();
    let chosen_maps: Vec<Option<Vec<(String, [f64; 4])>>> = chosen.iter().map(|v| v.map.clone()).collect();
    let doc = (chosen.len() == 1).then(|| chosen[0].doc.clone()).flatten();
    let previous = page.motion.variants.applied.clone();
    // The last application, if nothing changed since: undone first, so
    // trying one variant after another swaps them.
    let mut undone = false;
    // (Only within one ask: a later ask was written against the code with
    // the last application in it.)
    let same_ask = previous.as_ref().is_some_and(|(applied, _, _)| {
        let request_of = |id: &u64| page.motion.variants.list.iter().find(|v| v.id == *id).map(|v| v.request);
        let asks: Vec<Option<u64>> = applied.iter().chain(ids.iter()).map(request_of).collect();
        asks.windows(2).all(|w| w[0] == w[1])
    });
    let session = page.motion.desk.session.as_mut().ok_or("no graphic is open")?;
    if let (Some((_, revision, label)), true) = (&previous, same_ask) {
        if session.revision == *revision && session.undo.last().is_some_and(|(_, what)| what == label) {
            undone = session.undo(false).is_some();
        }
    }
    let maps: Vec<&Vec<(String, [f64; 4])>> = chosen_maps.iter().flatten().collect();
    let texts = match if maps.is_empty() { patched(session, &hunks.iter().collect::<Vec<_>>()) } else { super::palette::apply_map(session, &maps.iter().flat_map(|m| m.iter().cloned()).collect::<Vec<_>>()) } {
        Ok(texts) => texts,
        Err(error) => {
            // Refused: the last application stays as it was.
            if undone {
                session.undo(true);
            }
            return Err(error);
        }
    };
    let burst = format!("variants:{}", Cx::time_now().to_bits());
    let mut changed = false;
    for (index, text) in &texts {
        if *index == 0 {
            changed |= session.replace_burst(text.clone(), &what, &burst);
        } else {
            let key = session.modules[index - 1].key.clone();
            changed |= session.replace_module(&key, text.clone(), &what, Some(&burst));
        }
    }
    session.end_burst();
    // The variant's document is loaded already: shown at once (the
    // checker loads the code again behind it for the timeline).
    if let Some(doc) = doc {
        session.doc = Some(doc);
    }
    let revision = session.revision;
    let variants = &mut page.motion.variants;
    variants.applied = Some((ids.to_vec(), revision, what.clone()));
    variants.previewed = None;
    variants.cards_dirty = true;
    variants.status = format!("Applied {what} (undo: cmd-Z)");
    page.motion.desk.author = Some(format!("AI · {what}"));
    if changed {
        super::desk::changed(page);
    }
    page.motion.desk.repaint = true;
    log!("variants: applied {what}");
    Ok(what)
}

/// The document the viewer shows instead of the graphic's: the hovered
/// variant's, once it has loaded.
pub fn shown_doc(page: &VideoCut) -> Option<(u64, Arc<MotionDoc>)> {
    let id = page.motion.variants.previewed.filter(|id| Some(*id) != page.motion.variants.original)?;
    let variant = page.motion.variants.list.iter().find(|v| v.id == id)?;
    Some((id, variant.doc.clone()?))
}

// ------------------------------------------------------------------ pump

/// Every pump while a graphic is open: the writers' streams, the workers'
/// answers, the strip.
pub fn pump(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    expire_sessions(page);
    if page.motion.desk.session.is_none() {
        if page.motion.variants.open || !page.motion.variants.list.is_empty() {
            close(page);
        }
        return;
    }
    // A different graphic opened: its own variants.
    let of = page.motion.desk.session.as_ref().map(|s| s.node.clone().unwrap_or_else(|| s.title.clone()));
    if page.motion.variants.of.is_some() && page.motion.variants.of != of {
        close(page);
        page.motion.variants.of = None;
    }
    auto_panel(page, cx);
    pump_panel_asks(page, cx);
    panel_measured(page, cx);
    // The writers.
    let mut wrote = Vec::new();
    if let Some(pool) = &mut page.motion.variants.writers {
        pool.flush();
        while let Ok(event) = pool.done.try_recv() {
            wrote.push(event);
        }
    }
    for event in wrote {
        let key = match &event {
            Wrote::Text(key, _) | Wrote::Done(key) | Wrote::Failed(key, _) => *key,
        };
        let Some(request) = page.motion.variants.requests.iter_mut().find(|r| r.writers.iter().any(|w| w.key == key)) else { continue };
        let serial = request.serial;
        let Some(writer) = request.writers.iter_mut().find(|w| w.key == key) else { continue };
        let ended = !matches!(event, Wrote::Text(..));
        match event {
            Wrote::Text(_, text) => writer.text.push_str(&text),
            Wrote::Done(_) => writer.done = true,
            Wrote::Failed(_, error) => {
                writer.done = true;
                log!("variants: request {serial} writer failed: {error}");
                request.failures.push(error);
            }
        }
        if ended {
            let (id, received) = (writer.session, writer.text.len());
            if let Some(conversation) = page.motion.variants.sessions.iter_mut().find(|c| c.id == id) {
                conversation.busy = false;
                conversation.chars += received;
            }
        }
    }
    let mut closed: Vec<(u64, usize, String)> = Vec::new();
    for request in page.motion.variants.requests.iter_mut().filter(|r| !r.panel && !r.raw) {
        for writer in request.writers.iter_mut().filter(|w| !w.closed) {
            let complete = blocks(&writer.text, writer.done);
            while writer.taken < complete.len() {
                let number = writer.numbers.start + writer.taken;
                closed.push((request.serial, number, complete[writer.taken].to_string()));
                writer.taken += 1;
            }
            writer.closed = writer.done;
        }
    }
    for (request, number, block) in closed {
        take_block(page, request, number, &block);
    }
    // The workers' answers.
    let now = Cx::time_now();
    let mut answers = Vec::new();
    if let Some(loader) = &mut page.motion.variants.loader {
        loader.flush();
        while let Ok(loaded) = loader.done.try_recv() {
            answers.push(loaded);
        }
    }
    for loaded in answers {
        let started = |request: u64| page.motion.variants.requests.iter().find(|r| r.serial == request).map_or(now, |r| r.started);
        let Some(request) = page.motion.variants.list.iter().find(|v| v.id == loaded.id).map(|v| v.request) else { continue };
        let since = now - started(request);
        let Some(variant) = page.motion.variants.list.iter_mut().find(|v| v.id == loaded.id) else { continue };
        if variant.loading {
            variant.loading = false;
            variant.loaded = Some(since);
            variant.load_ms = loaded.ms;
            log!("variants: #{} loaded in {:.0} ms (at {:.0} ms){}", variant.number, loaded.ms, since * 1000.0, loaded.frame.as_ref().err().map(|e| format!(" — {e}")).unwrap_or_default());
        }
        if let Some(doc) = loaded.doc {
            variant.doc = Some(doc);
        }
        match loaded.frame {
            Ok(frame) => variant.frame = Some((loaded.t, frame)),
            Err(error) => variant.error = Some(error),
        }
        page.motion.variants.cards_dirty = true;
    }
    revisions(page, cx);
    ensure_original(page);
    // The strip's time follows the paused playhead.
    if let Some(session) = &page.motion.desk.session {
        let t = session.playhead;
        let variants = &mut page.motion.variants;
        if session.playing.is_none() && variants.strip_t != Some(t) && variants.list.iter().any(|v| v.doc.is_some()) {
            variants.strip_t = Some(t);
            if let Some(loader) = &mut variants.loader {
                for variant in variants.list.iter().filter(|v| v.doc.is_some() && v.frame.as_ref().is_none_or(|(ft, _)| *ft != t)) {
                    loader.send(Job::Eval { id: variant.id, doc: variant.doc.clone().unwrap(), t });
                }
            }
        }
    }
    // Requests that are complete.
    let variants = &mut page.motion.variants;
    for request in &mut variants.requests {
        let mine: Vec<&Variant> = variants.list.iter().filter(|v| v.request == request.serial).collect();
        if request.loaded.is_none() && !request.writing() && mine.iter().all(|v| !v.loading) {
            request.loaded = Some(now - request.started);
        }
        let painted = mine.iter().filter(|v| v.error.is_none()).map(|v| v.painted).try_fold(0.0f64, |at, p| p.map(|p| at.max(p)));
        let settled = request.loaded.is_some_and(|loaded| now - request.started - loaded > 2.0);
        if let (None, Some(loaded), true) = (request.all, request.loaded, painted.is_some() || settled) {
            request.all = Some(painted.unwrap_or(loaded).max(loaded));
            let ok = mine.iter().filter(|v| v.error.is_none()).count();
            log!("variants: request {} done: {ok}/{} usable of {} asked, first painted {}, all loaded at {:.0} ms, all painted {}", request.serial, mine.len(), request.n,
                request.first.map(|t| format!("{:.0} ms", t * 1000.0)).unwrap_or("-".into()), loaded * 1000.0, painted.map(|t| format!("{:.0} ms", t * 1000.0)).unwrap_or("-".into()));
            variants.status = if request.failures.is_empty() {
                format!("{ok} of {} · {:.1} s", request.n, request.all.unwrap_or(0.0))
            } else {
                format!("{ok} of {} · {}", request.n, request.failures[0])
            };
            variants.cards_dirty = true;
        }
    }
    let (working, summary) = super::super::generate::scene_state(page);
    let variants = &mut page.motion.variants;
    if working != variants.agent_working {
        variants.agent_working = working;
        variants.status = if working { "The scene's agent is working…".into() } else { summary.unwrap_or_else(|| "The scene's agent stopped".into()) };
        variants.cards_dirty = true;
    }
    if variants.requests.iter().any(Request::writing) {
        let made = variants.list.len();
        let asked: usize = variants.requests.iter().filter(|r| r.all.is_none()).map(|r| r.n).sum();
        let since = variants.requests.iter().filter(|r| r.all.is_none()).map(|r| now - r.started).fold(0.0, f64::max);
        variants.status = format!("Writing… {made} so far of {asked} · {since:.1} s");
    }
    paint(page, cx, ui);
}

fn paint(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let strip = ui.widget(cx, ids!(cut_motion)).widget(cx, ids!(m_variants));
    // Shown while it has variants (or is writing them).
    let open = page.motion.variants.open && (page.motion.variants.original.is_some() || page.motion.variants.requests.iter().any(|r| r.revise.is_none() && !r.panel && !r.internal && r.writing()));
    if strip.visible() != open {
        strip.set_visible(cx, open);
        page.motion.desk.repaint = true;
    }
    if !open {
        return;
    }
    let now = Cx::time_now();
    let thumbs = strip.widget(cx, ids!(v_thumbs));
    let mut labels: Option<String> = None;
    {
        let Some(mut thumbs) = thumbs.borrow_mut::<MotionVariantThumbs>() else { return };
        let variants = &mut page.motion.variants;
        // Pictures that painted: the request's first.
        for variant in variants.list.iter_mut().filter(|v| v.painted.is_none() && v.given.is_some()) {
            if thumbs.painted(variant.id) {
                if let Some(request) = variants.requests.iter_mut().find(|r| r.serial == variant.request) {
                    let since = now - request.started;
                    variant.painted = Some(since);
                    if request.first.is_none() {
                        request.first = Some(since);
                        log!("variants: request {} first variant painted at {:.0} ms", request.serial, since * 1000.0);
                    }
                }
            }
        }
        if std::mem::take(&mut variants.cards_dirty) {
            let size = page.motion.desk.session.as_ref().and_then(|s| s.doc.as_ref().map(|d| d.size)).unwrap_or([1920.0, 1080.0]);
            // The newest ask first, each in its numbers' order.
            let internal: Vec<u64> = variants.requests.iter().filter(|r| r.internal || r.revise.is_some() || r.panel).map(|r| r.serial).collect();
            let mut order: Vec<&Variant> = variants.list.iter().filter(|v| !internal.contains(&v.request)).collect();
            order.sort_by(|a, b| b.request.cmp(&a.request).then(a.number.cmp(&b.number)));
            let cards = order.into_iter().map(|v| CardInfo {
                id: v.id,
                index: v.number,
                label: v.label.clone(),
                note: v.note.clone(),
                state: match (&v.error, v.loading) {
                    (Some(error), _) => error.clone(),
                    (None, true) => "loading…".into(),
                    _ => String::new(),
                },
                failed: v.error.is_some(),
                selected: variants.previewed.unwrap_or(variants.original.unwrap_or(0)) == v.id,
                swatches: v.swatches.clone(),
            }).collect();
            thumbs.set_cards(cx, size, cards);
            labels = Some(variants.status.clone());
        } else if variants.requests.iter().any(Request::writing) {
            labels = Some(variants.status.clone());
        }
        for variant in variants.list.iter_mut() {
            let Some((t, frame)) = &variant.frame else { continue };
            let key = t.to_bits() ^ variant.id.rotate_left(17);
            if variant.given != Some(key) {
                variant.given = Some(key);
                thumbs.set_frame(cx, variant.id, frame.clone(), key);
            }
        }
    }
    // (The labels are found once the cards are let go: a search of the
    // strip passes through them.)
    if let Some(status) = labels {
        strip.widget(cx, ids!(v_status)).set_text(cx, &status);
    }
}

/// Desk actions for the variant bar: a card clicked previews it, Choose
/// applies it, Cancel goes back to the graphic as it was.
pub fn handle_actions(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
    let strip = ui.widget(cx, ids!(cut_motion)).widget(cx, ids!(m_variants));
    if strip.widget(cx, ids!(v_cancel)).as_button().clicked(actions) {
        close(page);
        return;
    }
    if strip.widget(cx, ids!(v_choose)).as_button().clicked(actions) {
        let chosen = page.motion.variants.previewed.filter(|id| Some(*id) != page.motion.variants.original);
        if let Some(id) = chosen {
            if let Err(error) = apply(page, &[id]) {
                page.motion.variants.status = error;
                page.motion.variants.cards_dirty = true;
                return;
            }
        }
        close(page);
        return;
    }
    let thumbs = strip.widget(cx, ids!(v_thumbs));
    for action in actions.filter_widget_actions_cast::<VariantAction>(thumbs.widget_uid()) {
        if let VariantAction::Select(id) = action {
            let variants = &mut page.motion.variants;
            variants.previewed = (Some(id) != variants.original).then_some(id);
            variants.cards_dirty = true;
            page.motion.desk.repaint = true;
        }
    }
}

/// The graphic as it is, the bar's first card (to compare), once it has
/// variants.
fn ensure_original(page: &mut VideoCut) {
    let Some(doc) = page.motion.desk.session.as_ref().and_then(|s| s.doc.clone()) else { return };
    let variants = &mut page.motion.variants;
 let internal: Vec<u64> = variants.requests.iter().filter(|r| r.internal || r.revise.is_some() || r.panel).map(|r| r.serial).collect();
    let any = variants.list.iter().any(|v| Some(v.id) != variants.original && !internal.contains(&v.request));
    if !any || variants.original.is_some() {
        return;
    }
    variants.next_id += 1;
    let id = variants.next_id;
    variants.original = Some(id);
    variants.list.push(Variant { id, request: u64::MAX, number: 0, label: "Original".into(), note: "the graphic as it is".into(), hunks: Vec::new(), error: None, doc: Some(doc), frame: None, given: None, arrived: 0.0, loaded: Some(0.0), painted: None, load_ms: 0.0, swatches: Vec::new(), loading: false, map: None });
    // Its frame at the bar's time (the others' come the same way).
    variants.strip_t = None;
    variants.cards_dirty = true;
}

// ------------------------------------------------------------------ control API

fn text<'a>(args: &'a Json, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Json::as_str)
}

fn number(args: &Json, key: &str) -> Option<f64> {
    match args.get(key)? {
        Json::Int(n) => Some(*n as f64),
        Json::F64(n) => Some(*n),
        _ => None,
    }
}

fn ms(seconds: Option<f64>) -> Json {
    seconds.map_or(Json::Null, |s| Json::Int((s * 1000.0).round() as i64))
}

fn variant_json(session: Option<&MotionSession>, v: &Variant) -> Json {
    let files = session.map(files).unwrap_or_default();
    let patches: Vec<Json> = v.hunks.iter().map(|hunk| {
        let at = files.get(hunk.doc).and_then(|(_, _, text)| place(text, hunk).ok()).map(|(range, _)| {
            let text = &files[hunk.doc].2;
            text[..range.start].matches('\n').count() as i64 + 1
        });
        json::obj(vec![
            ("file", json::s(hunk.file.clone())),
            ("line", at.map_or(Json::Null, Json::Int)),
            ("before", json::s(hunk.old.join("\n"))),
            ("after", json::s(hunk.new.join("\n"))),
        ])
    }).collect();
    json::obj(vec![
        ("id", Json::Int(v.id as i64)),
        ("number", Json::Int(v.number as i64)),
        ("label", json::s(v.label.clone())),
        ("note", json::s(v.note.clone())),
        ("state", json::s(match (&v.error, v.loading) {
            (Some(_), _) => "failed",
            (None, true) => "loading",
            _ => "ready",
        })),
        ("error", v.error.clone().map_or(Json::Null, json::s)),
        ("arrived_ms", ms(Some(v.arrived))),
        ("loaded_ms", ms(v.loaded)),
        ("painted_ms", ms(v.painted)),
        ("load_ms", Json::Int(v.load_ms.round() as i64)),
        ("patches", Json::Arr(patches)),
    ])
}

fn request_json(page: &VideoCut, serial: u64) -> Json {
    let variants = &page.motion.variants;
    let session = page.motion.desk.session.as_ref();
    let Some(request) = variants.requests.iter().find(|r| r.serial == serial) else { return json::obj(vec![("request", Json::Int(serial as i64)), ("error", json::s("gone (the strip was closed)"))]) };
    let list: Vec<Json> = variants.list.iter().filter(|v| v.request == serial).map(|v| variant_json(session, v)).collect();
    json::obj(vec![
        ("request", Json::Int(serial as i64)),
        ("ask", json::s(request.ask.clone())),
        ("n", Json::Int(request.n as i64)),
        ("model", json::s(request.model.clone())),
        ("writers", Json::Int(request.writers.len() as i64)),
        ("scope", json::s(request.scope.clone())),
        ("sent_chars", Json::Arr(request.writers.iter().map(|w| Json::Int(w.sent as i64)).collect())),
        // Still answering, or a change not yet applied, refused or handed on.
        ("writing", Json::Bool(request.writing() || request.revise.as_ref().is_some_and(|s| s.applied.is_none() && s.failed.is_none() && s.escalated.is_none() && s.answered.is_none()))),
        ("answers", Json::Arr(request.writers.iter().map(|w| json::s(w.text.clone())).collect())),
        ("first_painted_ms", ms(request.first)),
        ("all_loaded_ms", ms(request.loaded)),
        ("all_painted_ms", ms(request.all)),
        ("failures", Json::Arr(request.failures.iter().cloned().map(json::s).collect())),
        ("variants", Json::Arr(list)),
    ])
}

/// The control API's variant tools on the CUT page's open graphic.
pub fn tool(page: &mut VideoCut, cx: &mut Cx, name: &str, args: &Json) -> Answer {
    match name {
        "graphic_open" => {
            let Some(node) = text(args, "node") else { return Answer::Done(Err("`node` names the Graphic clip to open".into())) };
            if page.motion.desk.session.as_ref().is_some_and(|s| s.node.as_deref() == Some(node)) {
                return Answer::Later(Pending::Open(node.to_string()));
            }
            if !super::open(page, node) {
                return Answer::Done(Err(format!("`{node}` is not a Graphic clip of the open edit")));
            }
            Answer::Later(Pending::Open(node.to_string()))
        }
        "variants" => {
            let Some(asked) = text(args, "ask") else { return Answer::Done(Err("`ask` says what to make variants of".into())) };
            let n = number(args, "n").map(|n| n.max(1.0) as usize);
            let target = target_of(text(args, "target").unwrap_or(""));
            let split = number(args, "split").map(|n| n.max(1.0) as usize);
            // A moment to look at (the desk's playhead, paused there).
            if let (Some(at), Some(session)) = (number(args, "at"), page.motion.desk.session.as_mut()) {
                let fps = session.fps();
                session.playhead = (at.clamp(0.0, session.duration()) * fps).round() / fps;
                session.playing = None;
                page.motion.desk.repaint = true;
            }
            let mode = if asked.to_lowercase().contains("palette") { Mode::Palette { n, split } } else { Mode::Variants { n, split, seed: number(args, "more_like").map(|id| id as u64) } };
            match ask(page, cx, asked, mode, target, text(args, "model")) {
                Ok(serial) => {
                    if args.get("wait").and_then(Json::as_bool) == Some(false) {
                        Answer::Done(Ok(request_json(page, serial)))
                    } else {
                        Answer::Later(Pending::Request(serial))
                    }
                }
                Err(error) => Answer::Done(Err(error)),
            }
        }
        "apply_variant" => {
            let ids: Vec<u64> = match args.get("ids").and_then(Json::as_arr) {
                Some(ids) => ids.iter().filter_map(Json::as_u64).collect(),
                None => number(args, "id").map(|id| vec![id as u64]).unwrap_or_default(),
            };
            Answer::Done(apply(page, &ids).map(|what| {
                let session = page.motion.desk.session.as_ref();
                json::obj(vec![
                    ("applied", json::s(what)),
                    ("revision", session.map_or(Json::Null, |s| Json::Int(s.revision as i64))),
                    ("undo_depth", session.map_or(Json::Null, |s| Json::Int(s.undo.len() as i64))),
                ])
            }))
        }
        "hover_variant" => {
            let id = number(args, "id").map(|id| id as u64);
            if id.is_some_and(|id| !page.motion.variants.list.iter().any(|v| v.id == id)) {
                return Answer::Done(Err("no such variant".into()));
            }
            page.motion.variants.previewed = id;
            page.motion.desk.repaint = true;
            Answer::Done(Ok(json::obj(vec![("previewed", id.map_or(Json::Null, |id| Json::Int(id as i64)))])))
        }
        "history_list" => match page.motion.desk.session.as_ref() {
            Some(session) => Answer::Done(Ok(json::obj(vec![("steps", json::s(super::history::listing(session, 200)))]))),
            None => Answer::Done(Err("no graphic is open".into())),
        },
        "history_read" => match page.motion.desk.session.as_ref() {
            Some(session) => {
                let paths: Vec<String> = text(args, "paths").unwrap_or("").split([',', ' ']).filter(|p| !p.is_empty()).map(str::to_string).collect();
                Answer::Done(super::history::read(session, text(args, "step").unwrap_or("now"), &paths).map(|t| json::obj(vec![("text", json::s(t))])))
            }
            None => Answer::Done(Err("no graphic is open".into())),
        },
        "history_diff" => match page.motion.desk.session.as_ref() {
            Some(session) => Answer::Done(super::history::diff(session, text(args, "a").unwrap_or("picked"), text(args, "b").unwrap_or("now")).map(|t| json::obj(vec![("diff", json::s(t))]))),
            None => Answer::Done(Err("no graphic is open".into())),
        },
        "history_pick" => match page.motion.desk.session.as_ref() {
            Some(session) => Answer::Done(super::history::pick_step(session, text(args, "step")).map(|t| json::obj(vec![("picked", json::s(t))]))),
            None => Answer::Done(Err("no graphic is open".into())),
        },
        "merge_from_history" => {
            let paths: Vec<String> = text(args, "scope").unwrap_or("selection").split([',', ' ']).filter(|p| !p.is_empty()).map(str::to_string).collect();
            Answer::Done(merge_from_history(page, cx, text(args, "step").unwrap_or("picked"), &paths).map(|t| json::obj(vec![("said", json::s(t))])))
        }
        "scene_ask" => {
            let Some(asked) = text(args, "ask") else { return Answer::Done(Err("`ask` says what to change".into())) };
            match ask(page, cx, asked, Mode::Revise { retry: None }, target_of(text(args, "target").unwrap_or("")), text(args, "model")) {
                Ok(serial) => Answer::Later(Pending::Revision(serial)),
                Err(error) => Answer::Done(Err(error)),
            }
        }
        "palette" => {
            let Some(session) = page.motion.desk.session.as_ref() else { return Answer::Done(Err("no graphic is open".into())) };
            let swatches: Vec<Json> = super::palette::palette(session).iter().map(|s| json::obj(vec![
                ("colour", json::s(s.key())),
                ("name", s.name.clone().map_or(Json::Null, json::s)),
                ("hdr", Json::Bool(s.hdr)),
                ("uses", Json::Arr(s.uses.iter().map(|u| json::obj(vec![("what", json::s(u.what.clone())), ("intent", u.intent.clone().map_or(Json::Null, json::s))])).collect())),
            ])).collect();
            Answer::Done(Ok(json::obj(vec![("palette", Json::Arr(swatches)), ("moves", Json::Arr(super::palette::MOVES.iter().map(|m| json::s(m.0)).collect()))])))
        }
        "palette_apply" => {
            // A saved palette (as read_motion gives it) over this scene's.
            let saved: Vec<(String, [f64; 4])> = args.get("colours").and_then(Json::as_arr).unwrap_or(&[]).iter().filter_map(|c| {
                let color = c.get("color").and_then(Json::as_str)?;
                Some((c.get("name").and_then(Json::as_str).unwrap_or("").to_string(), patch::literal_color(color)?))
            }).collect();
            if saved.is_empty() {
                return Answer::Done(Err("`colours`: [{name, color: \"#rrggbb\"}], the saved palette's".into()));
            }
            match palette_saved(page, cx, text(args, "label").unwrap_or("Saved palette"), &saved) {
                Ok(serial) => Answer::Later(Pending::Request(serial)),
                Err(error) => Answer::Done(Err(error)),
            }
        }
        "editor_make" => {
            if let Some(element) = text(args, "element") {
                return Answer::Done(super::deep_make::start_element(page, cx, element, &[], text(args, "brief").unwrap_or("")).map(|_| json::obj(vec![("started", Json::Bool(true)), ("element", json::s(element))])));
            }
            let Some(asked) = text(args, "ask") else {
                // Quick: the panel over the values the code names; deep (the
                // default): audited, exposed, checked, then made.
                if args.get("quick").and_then(|v| v.as_bool()) == Some(true) {
                    return match make_panel(page, cx, text(args, "model"), text(args, "brief")) {
                        Ok(serial) => Answer::Later(Pending::Revision(serial)),
                        Err(error) => Answer::Done(Err(error)),
                    };
                }
                let started = super::deep_make::start(page, cx, text(args, "brief").unwrap_or(""));
                super::deep_make::set_doc_only(page, args.get("doc_only").and_then(|v| v.as_bool()) == Some(true));
                return Answer::Done(started.map(|_| json::obj(vec![("started", Json::Bool(true)), ("deep", Json::Bool(true))])));
            };
            let asked = if asked.to_lowercase().contains("editor") || asked.to_lowercase().contains("panel") { asked.to_string() } else { format!("{asked} (the editor panel)") };
            match ask(page, cx, &asked, Mode::Revise { retry: None }, Target::Named, text(args, "model")) {
                Ok(serial) => Answer::Later(Pending::Revision(serial)),
                Err(error) => Answer::Done(Err(error)),
            }
        }
        "editor_write" => {
            let Some(code) = text(args, "code") else { return Answer::Done(Err("`code`: the whole `let editor_panel = fn() { View{…} }`".into())) };
            Answer::Done(write_panel(page, code).map(|revision| json::obj(vec![("revision", Json::Int(revision as i64))])))
        }
        "editor_drive" => {
            // Started with a path: a gesture from `from` to `to`; without,
            // the last one's steps (each live or reloading, and how long it
            // took to show).
            if let Some(path) = text(args, "path") {
                let num = |k: &str| number(args, k);
                let (Some(from), Some(to)) = (num("from"), num("to")) else { return Answer::Done(Err("`from` and `to`: the gesture's first and last values".into())) };
                let n = num("steps").unwrap_or(8.0).clamp(1.0, 60.0) as usize;
                let values = (0..=n).map(|k| from + (to - from) * k as f64 / n as f64).collect();
                page.motion.panel.drive = Some(super::editor_panel::Drive { path: path.to_string(), values, ..Default::default() });
                cx.new_next_frame();
                return Answer::Done(Ok(json::obj(vec![("started", Json::Bool(true))])));
            }
            let Some(drive) = page.motion.panel.drive.as_ref() else { return Answer::Done(Err("no gesture was driven".into())) };
            let steps: Vec<Json> = drive.steps.iter().map(|s| json::obj(vec![
                ("value", Json::F64(s.value)),
                ("reach", json::s(if s.reload { "reload" } else { "live" })),
                ("shown_ms", s.shown_ms.map_or(Json::Null, Json::F64)),
            ])).collect();
            Answer::Done(Ok(json::obj(vec![("path", json::s(drive.path.clone())), ("done", Json::Bool(drive.done)), ("steps", Json::Arr(steps))])))
        }
        "editor_animate" => {
            // A control's key button, as the panel presses it.
            let Some(path) = text(args, "path") else { return Answer::Done(Err("`path`: a path from editor_paths".into())) };
            let on = args.get("on").and_then(|v| v.as_bool()) != Some(false);
            // Off: the animation taken away whole (the value at the playhead stays).
            if on {
                super::panel_feed::animate(path, true);
            } else {
                super::panel_feed::animate(&format!("!{path}"), false);
            }
            cx.new_next_frame();
            Answer::Done(Ok(json::obj(vec![("asked", Json::Bool(true))])))
        }
        "editor_fit" => {
            // The panel's ranges put on the scale of their values (written
            // as one undo step).
            let Some(session) = page.motion.desk.session.as_ref() else { return Answer::Done(Err("no graphic is open".into())) };
            let Some(range) = makepad_editor_panel::panel_region(&session.source, &session.code) else { return Answer::Done(Err("the scene has no panel".into())) };
            let panel = session.source[range].to_string();
            let fitted = fit_ranges(session, &panel);
            let changed = panel.lines().zip(fitted.lines()).filter(|(a, b)| a != b).count();
            if changed > 0 {
                if let Err(why) = write_panel(page, &fitted) {
                    return Answer::Done(Err(why));
                }
            }
            Answer::Done(Ok(json::obj(vec![("lines_fitted", Json::Int(changed as i64))])))
        }
        "graphic_source" => {
            // The open graphic's texts as the desk has them (its document and
            // each module: name, asset id, text).
            let Some(session) = page.motion.desk.session.as_ref() else { return Answer::Done(Err("no graphic is open".into())) };
            let modules: Vec<Json> = session.modules.iter().filter(|m| !m.builtin()).map(|m| json::obj(vec![("name", json::s(m.name.clone())), ("id", json::s(m.id.clone())), ("source", json::s(m.source.clone()))])).collect();
            Answer::Done(Ok(json::obj(vec![("source", json::s(session.source.clone())), ("revision", Json::Int(session.revision as i64)), ("modules", Json::Arr(modules))])))
        }
        "editor_compare" => {
            // The document as given drawn beside the desk's, at five moments:
            // how many pixels differ (read with editor_sweep).
            let Some(source) = text(args, "source") else { return Answer::Done(Err("`source`: the document to compare".into())) };
            let duration = page.motion.desk.session.as_ref().map_or(1.0, |s| s.duration());
            let times: Vec<f64> = [0.1, 0.3, 0.5, 0.7, 0.9].iter().map(|f| f * duration).collect();
            let serial = page.motion.variants.next_request;
            Answer::Done(super::panel_sweep::start_compare(page, serial, vec![("given".into(), vec![(0, source.to_string())])], &times).map(|_| json::obj(vec![("started", Json::Bool(true))])))
        }
        "editor_deep" => {
            let Some((done, r)) = super::deep_make::report(page) else { return Answer::Done(Err("no deep make has run".into())) };
            let strs = |v: &Vec<String>| Json::Arr(v.iter().cloned().map(json::s).collect());
            let per: Vec<Json> = r.per_element.iter().map(|(n, f, e, b)| json::obj(vec![("element", json::s(n.clone())), ("found", Json::Int(*f as i64)), ("exposed", Json::Int(*e as i64)), ("bound", Json::Int(*b as i64))])).collect();
            Answer::Done(Ok(json::obj(vec![
                ("done", Json::Bool(done)),
                ("elements", strs(&r.elements)),
                ("found", Json::Int(r.found as i64)),
                ("bind", Json::Int(r.bind as i64)),
                ("expose_asked", Json::Int(r.expose_asked as i64)),
                ("exposed", strs(&r.exposed)),
                ("refused", strs(&r.refused)),
                ("bound_in_panel", Json::Int(r.bound as i64)),
                ("per_element", Json::Arr(per)),
                ("frame_checks", strs(&r.checks)),
                ("stages", strs(&r.stages)),
                ("seconds", Json::F64(r.seconds)),
                ("failed", r.failed.clone().map_or(Json::Null, json::s)),
            ])))
        }
        "editor_sweep" => {
            // The made panel's controls measured on the picture: start one
            // with `start`, read the last otherwise.
            if args.get("start").and_then(|v| v.as_bool()) == Some(true) {
                let serial = page.motion.variants.next_request;
                return Answer::Done(super::panel_sweep::start(page, serial).map(|n| json::obj(vec![("controls", Json::Int(n as i64))])));
            }
            let rows: Vec<Json> = super::panel_sweep::last().into_iter().map(|m| json::obj(vec![
                ("control", json::s(m.control)),
                ("changed", Json::F64(m.changed)),
                ("at", Json::F64(m.t)),
            ])).collect();
            Answer::Done(Ok(json::obj(vec![("busy", Json::Bool(super::panel_sweep::busy())), ("controls", Json::Arr(rows))])))
        }
        "editor_when" => {
            // Where a control acts in time (its spans) and whether it does
            // now; with `focus`, shown on the scrubber and in the picture.
            let paths: Vec<String> = args.get("paths").and_then(|v| v.as_arr()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
            if args.get("focus").and_then(|v| v.as_bool()) == Some(true) {
                super::panel_time::focus(Some(&paths));
            } else if args.get("focus").and_then(|v| v.as_bool()) == Some(false) {
                super::panel_time::focus(None);
            }
            let rows: Vec<Json> = paths.iter().map(|p| json::obj(vec![
                ("path", json::s(p.clone())),
                ("active", super::panel_time::active(p).map_or(Json::Null, Json::Bool)),
                ("spans", super::panel_time::spans(p).map_or(Json::Null, |s| Json::Arr(s.into_iter().map(|(a, b)| Json::Arr(vec![Json::F64(a), Json::F64(b)])).collect()))),
            ])).collect();
            cx.new_next_frame();
            Answer::Done(Ok(json::obj(vec![("paths", Json::Arr(rows))])))
        }
        "editor_paths" => {
            let Some(session) = page.motion.desk.session.as_ref() else { return Answer::Done(Err("no graphic is open".into())) };
            Answer::Done(Ok(json::obj(vec![("paths", json::s(super::editor_panel::paths_listing(session)))])))
        }
        "swaps" => {
            let Some(session) = page.motion.desk.session.as_ref() else { return Answer::Done(Err("no graphic is open".into())) };
            let slots: Vec<Json> = super::swaps::slots(session).iter().map(|s| json::obj(vec![
                ("what", json::s(s.what.clone())),
                ("current", json::s(s.current.clone())),
                ("alternatives", Json::Arr(s.alternatives.iter().cloned().map(json::s).collect())),
            ])).collect();
            Answer::Done(Ok(json::obj(vec![("slots", Json::Arr(slots))])))
        }
        "swap_variants" => match swap_variants(page, cx, text(args, "what")) {
            Ok(serial) => Answer::Later(Pending::Request(serial)),
            Err(error) => Answer::Done(Err(error)),
        },
        "palette_moves" => match palette_moves(page, cx, text(args, "move"), number(args, "amount").unwrap_or(1.0)) {
            Ok(serial) => Answer::Later(Pending::Request(serial)),
            Err(error) => Answer::Done(Err(error)),
        },
        "variants_status" => {
            let requests: Vec<Json> = page.motion.variants.requests.iter().map(|r| request_json(page, r.serial)).collect();
            Answer::Done(Ok(json::obj(vec![
                ("open", Json::Bool(page.motion.variants.open)),
                ("previewed", page.motion.variants.previewed.map_or(Json::Null, |id| Json::Int(id as i64))),
                ("requests", Json::Arr(requests)),
            ])))
        }
        _ => Answer::Done(Err(format!("no variant tool `{name}`"))),
    }
}

/// A waiting call's answer once it is in.
pub fn tool_reply(page: &VideoCut, pending: &Pending) -> Option<Result<Json, String>> {
    match pending {
        Pending::Open(node) => {
            let session = page.motion.desk.session.as_ref().filter(|s| s.node.as_deref() == Some(node.as_str()))?;
            if session.doc.is_none() {
                // It does not load: why, at once.
                let problem = session.problems.first()?;
                return Some(Err(format!("the graphic does not load: line {}:{} {}", problem.line, problem.col, problem.message)));
            }
            Some(Ok(json::obj(vec![
                ("node", session.node.clone().map_or(Json::Null, json::s)),
                ("title", json::s(session.title.clone())),
                ("duration", Json::F64(session.duration())),
                ("files", Json::Arr(files(session).into_iter().map(|(_, name, text)| json::obj(vec![("name", json::s(name)), ("lines", Json::Int(text.lines().count() as i64))])).collect())),
            ])))
        }
        Pending::Revision(origin) => {
            let variants = &page.motion.variants;
            let chain: Vec<&Request> = variants.requests.iter().filter(|r| r.revise.as_ref().is_some_and(|v| v.origin == *origin)).collect();
            let last = chain.last()?;
            let state = last.revise.clone().unwrap_or_default();
            let ended = state.applied.is_some() || state.failed.is_some() || state.escalated.is_some() || state.answered.is_some();
            if !ended {
                return None;
            }
            let (working, summary) = super::super::generate::scene_state(page);
            if state.escalated.is_some() && working {
                return None;
            }
            let session = page.motion.desk.session.as_ref();
            let tries: Vec<Json> = chain.iter().map(|r| request_json(page, r.serial)).collect();
            Some(Ok(json::obj(vec![
                ("applied", state.applied.clone().map_or(Json::Null, json::s)),
                ("failed", state.failed.clone().map_or(Json::Null, json::s)),
                ("escalated", state.escalated.clone().map_or(Json::Null, json::s)),
                ("answered", state.answered.clone().map_or(Json::Null, json::s)),
                ("generate_summary", if state.escalated.is_some() { summary.map_or(Json::Null, json::s) } else { Json::Null }),
                ("applied_ms", ms(state.at)),
                ("tries", Json::Arr(tries)),
                ("revision", session.map_or(Json::Null, |s| Json::Int(s.revision as i64))),
            ])))
        }
        Pending::Request(serial) => {
            let request = page.motion.variants.requests.iter().find(|r| r.serial == *serial);
            match request {
                Some(request) if request.all.is_none() => None,
                _ => Some(Ok(request_json(page, *serial))),
            }
        }
    }
}

#[cfg(test)]
mod panel_range_tests {
    use super::super::panel_effects::controls_have_an_effect;

    fn film_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/panel_film")
    }

    /// Fitting a prefabbed panel preserves each control's range and each
    /// macro target's ends when they fit the value written in the plate.
    #[test]
    fn prefab_panel_ranges_hold_their_values() {
        makepad_editor_panel::install_doc(makepad_stage_motion_pickers::motion_doc());
        let dir = film_dir();
        let plates: std::collections::BTreeMap<String, String> = match makepad_strict_json::parse_depth(std::fs::read(dir.join("plates.json")).unwrap().as_slice(), 4).unwrap() {
            makepad_strict_json::Value::Obj(pairs) => pairs.iter().filter_map(|(k, v)| Some((k.to_string(), v.as_str()?.to_string()))).collect(),
            _ => panic!("plates.json is an object"),
        };
        let mut checked = 0;
        for file in plates.values().collect::<std::collections::BTreeSet<_>>() {
            let text = std::fs::read_to_string(dir.join(file)).unwrap();
            let code = makepad_motion::schema::code::outline(&text);
            let Some(region) = makepad_editor_panel::panel_region(&text, &code) else { continue };
            let panel = &text[region];
            let files = [makepad_motion::paths::File { name: "", text: &text, code: &code }];
            let fitted = super::fit_ranges_in(&files, panel);
            for (a, b) in panel.lines().zip(fitted.lines()) {
                assert_eq!(a, b, "{file}: a range off the scale of its value");
            }
            checked += 1;
        }
        assert!(checked > 0, "no plate has a panel");
    }

    /// The waterline moves only in the rising tide placement. The camera,
    /// post, ink and font controls affect both placements.
    #[test]
    fn a_plates_panel_controls_have_an_effect() {
        assert!(controls_have_an_effect(&film_dir(), "film.cut.splash", Some("plates/tide.splash")));
    }

    /// Each control's path and each macro target must resolve in the plate
    /// or one of its imported modules for the panel to show the control.
    #[test]
    fn prefab_panel_paths_resolve() {
        makepad_editor_panel::install_doc(makepad_stage_motion_pickers::motion_doc());
        let dir = film_dir();
        // Imported modules resolve against the same tree snapshot.
        let plates: std::collections::BTreeMap<String, String> = match makepad_strict_json::parse_depth(std::fs::read(dir.join("plates.json")).unwrap().as_slice(), 4).unwrap() {
            makepad_strict_json::Value::Obj(pairs) => pairs.iter().filter_map(|(k, v)| Some((k.to_string(), v.as_str()?.to_string()))).collect(),
            _ => panic!("plates.json is an object"),
        };
        let snapshot = makepad_motion::tree::read_folder(&dir, "film.cut.splash", makepad_asset_data::tree::TreeRole::Cut).unwrap();
        let mut gone = Vec::new();
        let mut checked = 0;
        for file in plates.values().collect::<std::collections::BTreeSet<_>>() {
            let text = std::fs::read_to_string(dir.join(file)).unwrap();
            let code = makepad_motion::schema::code::outline(&text);
            let Some(region) = makepad_editor_panel::panel_region(&text, &code) else { continue };
            // (name, text) of each module the plate imports.
            let imported: Vec<(String, String)> = text.lines().filter_map(|line| {
                let rest = line.trim_start().strip_prefix("let ")?;
                let (name, value) = rest.split_once('=')?;
                use makepad_motion::tree::TreeResolver;
                let path = value.trim().strip_prefix("import(\"")?.split('"').next()?;
                Some((name.trim().to_string(), snapshot.resolve(file, path).unwrap().text().unwrap().to_string()))
            }).collect();
            let outlines: Vec<makepad_motion::schema::code::CodeOutline> = imported.iter().map(|(_, t)| makepad_motion::schema::code::outline(t)).collect();
            let files: Vec<makepad_motion::paths::File> = std::iter::once(makepad_motion::paths::File { name: "", text: &text, code: &code })
                .chain(imported.iter().zip(&outlines).map(|((name, t), c)| makepad_motion::paths::File { name, text: t, code: c }))
                .collect();
            let panel = &text[region.clone()];
            let mut paths: Vec<String> = panel.split("path: \"").skip(1).filter_map(|p| p.split('"').next().map(str::to_string)).collect();
            for targets in panel.split("targets: \"").skip(1).filter_map(|p| p.split('"').next()) {
                paths.extend(targets.split(',').filter_map(|t| t.split_whitespace().next().map(str::to_string)));
            }
            for path in paths {
                if let Err(why) = makepad_motion::paths::resolve_exact(&files, &path) {
                    gone.push(format!("{file}: {why}"));
                }
            }
            checked += 1;
        }
        assert!(checked > 0, "no plate has a panel");
        assert!(gone.is_empty(), "panel paths that are not there:\n{}", gone.join("\n"));
    }
}

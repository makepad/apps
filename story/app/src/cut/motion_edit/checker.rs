//! The Source view's checker: a typed draft is loaded here, off the UI
//! thread, and comes back either proven (it loads; the page applies it as
//! one undoable change) or with its diagnostics and their locations, which
//! the view marks inline. One long-lived thread fed by a bounded channel;
//! a draft that finds the queue full is kept and sent on a later pump.

use makepad_widgets::*;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

/// What a draft is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocKind {
    Cut,
    Motion,
    /// A deck (`*.deck.splash`).
    Deck,
}

/// One problem with a draft, 1-based.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub line: u32,
    pub col: u32,
    pub message: String,
}

pub struct Check {
    pub tree: Option<(Arc<makepad_asset_data::tree::TreeDraft>, String)>,
    pub serial: u64,
    pub kind: DocKind,
    pub source: String,
    /// Library modules loaded in place of their revisions (being edited):
    /// `(id, rev, text)`.
    pub modules: Vec<(String, String, String)>,
    /// After the document is handed over: when its code draws over the
    /// film, sampled here (off the UI thread) and sent as a second answer.
    pub activity: bool,
    /// A graphic placed in the open edit: loaded as the edit loads it (its
    /// clock, the songs and voices it hears), the edit's own document.
    pub film: Option<makepad_story_edit_render::graphic::FilmLoad>,
    /// Only where a text that did not load goes wrong ([`diagnose`]): the
    /// file alone in its tree, never the whole tree.
    pub diagnose: bool,
}

/// One moment of a document's film: which code drew how much (call sites,
/// resolved), and which layers drew.
#[derive(Clone, Debug, Default)]
pub struct ActivitySample {
    pub sites: Vec<(makepad_motion::schema::CodeAt, f32)>,
    pub layers: Vec<String>,
    /// What the code's `let`s held (numbers): where, and the value.
    pub values: Vec<(makepad_motion::schema::CodeAt, f64)>,
    /// What they held that is a vector or a colour: where, the components
    /// (unused ones 0), and how many (2..4; 5 a colour).
    pub vectors: Vec<(makepad_motion::schema::CodeAt, [f32; 4], u8)>,
}

/// Samples of the film for the code timeline's ribbons.
pub const ACTIVITY_SAMPLES: usize = 96;
/// The first, quick look at the film (no pauses between frames).
pub const COARSE_SAMPLES: usize = 24;

/// The library, with some revisions' texts as they are being edited.
pub(super) struct Edited {
    pub(super) edits: Vec<(String, String, String)>,
    pub(super) library: makepad_motion::modules::Modules,
}

impl makepad_motion::modules::ModuleResolver for Edited {
    fn named(&self, name: &str) -> Option<makepad_motion::modules::ModuleSource> {
        self.library.0.named(name)
    }
    fn asset(&self, id: &str, rev: &str) -> Option<makepad_motion::modules::ModuleSource> {
        match self.edits.iter().find(|(i, r, _)| i == id && r == rev) {
            Some((_, _, text)) => Some(makepad_motion::modules::ModuleSource::new(id, rev, text.as_str())),
            None => self.library.0.asset(id, rev),
        }
    }
}

pub struct Checked {
    pub serial: u64,
    pub source: String,
    /// Errors; empty: the draft loads.
    pub problems: Vec<Problem>,
    /// A Motion document that loads, and its warnings.
    pub motion: Option<Arc<makepad_motion::MotionDoc>>,
    pub modules: Vec<makepad_motion::modules::LoadedModule>,
    pub warnings: Vec<Problem>,
    /// The second answer of a check that asked for it: the film sampled.
    pub activity: Option<Vec<ActivitySample>>,
    /// A first, coarse sampling (a fine one follows): a value's curve
    /// follows a scrubbed parameter at once.
    pub coarse: bool,
}

pub struct Checker {
    jobs: Option<SyncSender<Check>>,
    done: Receiver<Checked>,
    pub error: Option<String>,
}

impl Checker {
    pub fn new(cx: &mut Cx) -> Self {
        let (job_tx, job_rx) = sync_channel::<Check>(2);
        let (done_tx, done_rx) = sync_channel::<Checked>(2);
        let options = ThreadOptions { name: Some(Arc::from("stage-source-check")), stack_size: Some(8 * 1024 * 1024), priority: CxThreadPriority::UserInitiated };
        let mut checker = Self { jobs: None, done: done_rx, error: None };
        match cx.thread_spawner().spawn_worker(options, move || run(job_rx, done_tx)) {
            Ok(handle) => {
                handle.detach();
                checker.jobs = Some(job_tx);
            }
            Err(error) => checker.error = Some(format!("source checker unavailable: {error:?}")),
        }
        checker
    }

    pub fn try_send(&self, check: Check) -> Result<(), Check> {
        let Some(jobs) = &self.jobs else { return Err(check) };
        jobs.try_send(check).map_err(|error| match error {
            TrySendError::Full(check) | TrySendError::Disconnected(check) => check,
        })
    }

    pub fn try_recv(&self) -> Option<Checked> {
        self.done.try_recv().ok()
    }

    pub fn stop(&mut self) {
        self.jobs.take();
    }
}

fn motion_problem(diag: makepad_motion::Diagnostic) -> Problem {
    let mut message = if diag.path.is_empty() { diag.message } else { format!("{}: {}", diag.path, diag.message) };
    if let Some(hint) = diag.hint {
        message.push_str(&format!(" (hint: {hint})"));
    }
    Problem { line: diag.loc.line, col: diag.loc.col, message }
}

/// Load `source` as its kind: its errors, and a Motion document that
/// loads with its warnings.
/// `film`: the graphic's place in its edit ([`Check::film`]); `None`, the
/// document on its own.
pub fn check(kind: DocKind, source: &str, modules: Vec<(String, String, String)>, film: Option<&makepad_story_edit_render::graphic::FilmLoad>) -> (Vec<Problem>, Option<Arc<makepad_motion::MotionDoc>>, Vec<Problem>) {
    match kind {
        DocKind::Cut => match makepad_story_cut::CutDoc::load(source) {
            Ok(_) => (Vec::new(), None, Vec::new()),
            Err(diags) => (diags.into_iter().map(|diag| Problem { line: diag.line, col: diag.col, message: diag.message }).collect(), None, Vec::new()),
        },
        DocKind::Deck => match makepad_story_deck::DeckDoc::load(source) {
            Ok(_) => (Vec::new(), None, Vec::new()),
            Err(diags) => (diags.into_iter().map(|diag| Problem { line: diag.line, col: diag.col, message: diag.message }).collect(), None, Vec::new()),
        },
        DocKind::Motion => match if let Some(film) = film {
            let modules = makepad_motion::modules::Modules(Arc::new(Edited { edits: modules, library: Default::default() }));
            makepad_motion::editable(|| film.load(source, modules))
        } else if modules.is_empty() {
            // An editor's document: its numbers can be set while it runs.
            makepad_motion::editable(|| makepad_motion::MotionDoc::load(source))
        } else {
            let context = makepad_motion::MotionContext { modules: makepad_motion::modules::Modules(Arc::new(Edited { edits: modules, library: Default::default() })), ..Default::default() };
            makepad_motion::editable(|| makepad_motion::MotionDoc::load_with(source, &[], context))
        } {
            Ok(doc) => {
                let warnings = doc.warnings.iter().cloned().map(motion_problem).collect();
                (Vec::new(), Some(Arc::new(doc)), warnings)
            }
            Err(diags) => (diags.into_iter().filter(|diag| diag.is_error()).map(motion_problem).collect(), None, Vec::new()),
        },
    }
}

/// The film sampled as the checker samples it (a test's).
#[cfg(test)]
pub(super) fn sample_for_test(doc: &makepad_motion::MotionDoc, samples: usize) -> Vec<ActivitySample> {
    let (_keep, jobs) = sync_channel::<Check>(1);
    sample_activity(doc, samples, false, &jobs, &mut None).unwrap()
}

/// The film sampled: at each of `ACTIVITY_SAMPLES` moments, which code
/// drew how much. `None` when a newer job came in meanwhile (it is kept in
/// `pending`). Each sample lets the document go between them, so the
/// viewer (which draws the same document) waits at most one sample.
fn sample_activity(doc: &makepad_motion::MotionDoc, samples: usize, pause: bool, jobs: &Receiver<Check>, pending: &mut Option<Check>) -> Option<Vec<ActivitySample>> {
    use std::collections::{BTreeMap, HashMap};
    let mut cache: BTreeMap<makepad_widgets::ScriptIp, Option<makepad_motion::schema::CodeAt>> = BTreeMap::new();
    let mut out = Vec::with_capacity(samples);
    for k in 0..samples {
        if pending.is_none() {
            if let Ok(next) = jobs.try_recv() {
                *pending = Some(next);
            }
        }
        if pending.is_some() {
            return None;
        }
        let t = (k as f64 + 0.5) / samples as f64 * doc.duration;
        let (frame, lets) = doc.eval_watched(t);
        let mut sites: HashMap<makepad_motion::schema::CodeAt, f32> = HashMap::new();
        let mut layers = Vec::new();
        for (_, item) in frame.walk() {
            match &item.kind {
                makepad_motion::ItemKind::Draw2d { picture, .. } => {
                    let list = &picture.list;
                    for (i, mark) in list.marks.iter().enumerate() {
                        let next = list.marks.get(i + 1);
                        let amount = (next.map_or(list.ops.len() as u32, |n| n.op).saturating_sub(mark.op) + next.map_or(list.segs.len() as u32, |n| n.seg).saturating_sub(mark.seg)) as f32;
                        if amount <= 0.0 {
                            continue;
                        }
                        let missing: Vec<_> = mark.sites.iter().copied().filter(|ip| !cache.contains_key(ip)).collect();
                        if !missing.is_empty() {
                            for (ip, at) in missing.iter().zip(doc.code_at(&missing)) {
                                cache.insert(*ip, at);
                            }
                        }
                        let mut seen: Vec<&makepad_motion::schema::CodeAt> = Vec::new();
                        for at in mark.sites.iter().filter_map(|ip| cache.get(ip).and_then(Option::as_ref)) {
                            if !seen.contains(&at) {
                                *sites.entry(at.clone()).or_insert(0.0) += amount;
                                seen.push(at);
                            }
                        }
                    }
                }
                _ => layers.push(item.path.clone()),
            }
        }
        let mut numbers = Vec::new();
        let mut vectors = Vec::new();
        for (ip, _, value) in lets {
            if let Some(n) = value.number {
                numbers.push((ip, n));
            } else if let Some(c) = value.color {
                vectors.push((ip, c, 5u8));
            } else if let Some(v) = &value.vector {
                let mut a = [0.0f32; 4];
                a[..v.len().min(4)].copy_from_slice(&v[..v.len().min(4)]);
                vectors.push((ip, a, v.len().min(4) as u8));
            }
        }
        let missing: Vec<_> = numbers.iter().map(|(ip, _)| *ip).chain(vectors.iter().map(|(ip, _, _)| *ip)).filter(|ip| !cache.contains_key(ip)).collect();
        if !missing.is_empty() {
            for (ip, at) in missing.iter().zip(doc.code_at(&missing)) {
                cache.insert(*ip, at);
            }
        }
        let values = numbers.into_iter().filter_map(|(ip, v)| Some((cache.get(&ip)?.clone()?, v))).collect();
        let vectors = vectors.into_iter().filter_map(|(ip, v, k)| Some((cache.get(&ip)?.clone()?, v, k))).collect();
        out.push(ActivitySample { sites: sites.into_iter().collect(), layers, values, vectors });
        // The viewer's turn with the document (the fine pass only).
        if pause {
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
    }
    Some(out)
}

/// Where `source`, written at `path` of `tree`, goes wrong, on its own lines:
/// its syntax, then the file loaded in its tree as what it is (a Motion
/// document, a cut, the deck file with its first slide). Empty: the file
/// itself loads (what failed is elsewhere).
pub fn diagnose(tree: &Arc<makepad_asset_data::tree::TreeDraft>, path: &str, source: &str) -> Vec<Problem> {
    use makepad_asset_data::AssetKind;
    if let Err(error) = makepad_widgets::makepad_script::syntax::parse(source) {
        let before = &source[..error.at.min(source.len())];
        let line = before.matches('\n').count() as u32 + 1;
        let col = before.rsplit('\n').next().map_or(0, |text| text.chars().count()) as u32 + 1;
        return vec![Problem { line, col, message: error.message }];
    }
    let written = match crate::document_tree::apply(tree, &makepad_app_shell::tool_host::Change::Write { path: path.to_string(), text: source.to_string(), expected: None }) {
        Ok(tree) => tree,
        Err(message) => return vec![Problem { line: 1, col: 1, message }],
    };
    let Some(file) = written.files.get(path) else { return Vec::new() };
    let context = written.context(path);
    let plain = |diags: Vec<makepad_story_cut::Diagnostic>| diags.into_iter().map(|diag| Problem { line: diag.line, col: diag.col, message: diag.message }).collect();
    match file.kind {
        AssetKind::Cut => makepad_story_cut::CutDoc::load_tree(source, context).err().map_or_else(Vec::new, plain),
        AssetKind::Deck => makepad_story_deck::DeckDoc::load_tree_first(source, context).err().map_or_else(Vec::new, |diags| diags.into_iter().map(|diag| Problem { line: diag.line, col: diag.col, message: diag.message }).collect()),
        AssetKind::Motion if makepad_motion::modules::is_module(source) => match makepad_motion::modules::check(source, makepad_motion::modules::Modules::from_tree(context)) {
            Ok(_) => Vec::new(),
            Err(messages) => messages.into_iter().map(|message| Problem { line: 1, col: 1, message }).collect(),
        },
        AssetKind::Motion => {
            let modules = makepad_motion::modules::Modules::from_tree(context);
            match makepad_motion::MotionDoc::load_with(source, &[], makepad_motion::MotionContext { modules, ..Default::default() }) {
                Ok(_) => Vec::new(),
                // A problem in a file it imports is named on the first line.
                Err(diags) => diags.into_iter().filter(|diag| diag.is_error()).map(|diag| match diag.file.clone().filter(|file| &**file != path) {
                    Some(file) => Problem { line: 1, col: 1, message: format!("{file}: {}", motion_problem(diag).message) },
                    None => motion_problem(diag),
                }).collect(),
            }
        }
        _ => Vec::new(),
    }
}

fn run(jobs: Receiver<Check>, done: SyncSender<Checked>) {
    let mut pending: Option<Check> = None;
    loop {
        let job = match pending.take() {
            Some(job) => job,
            None => match jobs.recv() {
                Ok(job) => job,
                Err(_) => break,
            },
        };
        let Check { serial, kind, source, modules, activity, film, tree, diagnose: only } = job;
        let mut loaded_modules = Vec::new();
        let (problems, motion, warnings) = if let (true, Some((tree, path))) = (only, &tree) {
            (diagnose(tree, path, &source), None, Vec::new())
        } else if let Some((tree, path)) = tree {
            let mut changes: Vec<_> = modules.into_iter().map(|(path, _, text)| makepad_app_shell::tool_host::Change::Write { path, text, expected: None }).collect();
            changes.push(makepad_app_shell::tool_host::Change::Write { path: path.clone(), text: source.clone(), expected: None });
            let result = crate::document_tree::apply_all(&tree, &changes).and_then(|tree| {
                crate::document_tree::load(&tree, &tree.root.1)?;
                Ok(tree)
            });
            match result {
                Err(message) => (vec![Problem { line: 1, col: 1, message }], None, Vec::new()),
                Ok(tree) if kind == DocKind::Motion && !makepad_motion::modules::is_module(&source) => {
                    let modules = makepad_motion::modules::Modules::from_tree(tree.context(&path));
                    loaded_modules = makepad_motion::modules::resolve(&source, modules.0.as_ref()).unwrap_or_default();
                    let result = makepad_motion::editable(|| match &film {
                        Some(film) => film.load(&source, modules),
                        None => makepad_motion::MotionDoc::load_with(&source, &[], makepad_motion::MotionContext { modules, ..Default::default() }),
                    });
                    match result {
                        Ok(doc) => { let warnings = doc.warnings.iter().cloned().map(motion_problem).collect(); (Vec::new(), Some(Arc::new(doc)), warnings) }
                        Err(diags) => (diags.into_iter().filter(|diag| diag.is_error()).map(motion_problem).collect(), None, Vec::new()),
                    }
                }
                Ok(_) => (Vec::new(), None, Vec::new()),
            }
        } else {
            if kind == DocKind::Motion {
                let resolver = Edited { edits: modules.clone(), library: Default::default() };
                loaded_modules = makepad_motion::modules::resolve(&source, &resolver).unwrap_or_default();
            }
            check(kind, &source, modules, film.as_ref())
        };
        let sample = activity.then(|| motion.clone()).flatten();
        if done.send(Checked { serial, source: source.clone(), problems, motion, modules: loaded_modules, warnings, activity: None, coarse: false }).is_err() {
            break;
        }
        if let Some(doc) = sample {
            // Coarse at once, then fine (each stops for a newer check).
            for (samples, pause, coarse) in [(COARSE_SAMPLES, false, true), (ACTIVITY_SAMPLES, true, false)] {
                let Some(samples) = sample_activity(&doc, samples, pause, &jobs, &mut pending) else { break };
                if done.send(Checked { serial, source: source.clone(), problems: Vec::new(), motion: None, modules: Vec::new(), warnings: Vec::new(), activity: Some(samples), coarse }).is_err() {
                    return;
                }
            }
        }
    }
}

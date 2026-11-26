//! The Fab model document.
//!
//! A model is a **project** ([`project`]): object scripts (`<name>.cad.splash`, Splash
//! documents in the model language, [`program`]) placed on the plate, each with its own
//! parameter values shared by all its linked clones, and where and how every object prints.
//! Saved rulers live in the project in plate coordinates and never reach slicing.
//! The project and scripts' source texts are the document; there is no second format.
//!
//! Every change is a transaction on one undo log: [`FabModel::apply`] takes the head it
//! expects (optimistic concurrency), a request id (a retried request returns its first
//! reply instead of applying twice) and a batch of [`Change`]s: structured splices of a
//! script's source ([`Edit`]: a value site, a field, a statement, typed text) and edits of
//! the project (a placement, a print setting, a parameter value, a script placed or taken
//! away, a linked clone, an independent script copy, or a saved ruler). The batch commits
//! whole or not at all, and only when the project it leaves builds, so the document
//! never holds one that does not build.
//!
//! Each script reference compiles once and is cached by its source and parameters.
//! Linked placements share the same compiled `Arc<Script>`; copies fork the source file.
//! A body's build history ([`steps`]) comes from its compiled solid inputs.

pub mod align;
pub mod colour_sets;
pub mod eval;
pub mod field_volume;
pub mod grown;
pub mod generated;
pub mod mech;
pub mod paint;
pub mod params;
pub mod picture;
pub mod program;
pub mod project;
pub mod steps;
mod test_prints;
pub mod study;
pub mod expect;
pub mod thicken;
pub mod volumes;
pub mod veins;
pub mod weave;

pub use makepad_model_script::lang::edit::{Anchor, Edit};
pub use makepad_model_script::lang::{self, Diag};

use eval::{Evaluated, Evaluator, Failure};
use makepad_core_util::sha256::Sha256;
use program::{link_name, split_link, Compiled, Instance, Script, ScriptEnv};
use project::{Placement, Project, ScriptRef};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

/// A document revision: its generation and the content's hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Head {
    pub generation: u64,
    pub content: [u8; 32],
}

impl Head {
    /// `generation:hash` (what the tools hand out and take back).
    pub fn text(&self) -> String {
        format!("{}:{}", self.generation, makepad_core_util::sha256::to_hex(&self.content[..8]))
    }

    pub fn parse(text: &str) -> Option<(u64, String)> {
        let (g, h) = text.split_once(':')?;
        Some((g.trim().parse().ok()?, h.trim().to_string()))
    }

    /// The head a tool's `generation:hash` names.
    pub fn matches(&self, text: &str) -> bool {
        Head::parse(text).is_some_and(|(g, h)| g == self.generation && makepad_core_util::sha256::to_hex(&self.content[..8]) == h)
    }
}

const MAX_RECEIPTS: usize = 128;
const MAX_HISTORY: usize = 4096;
const MAX_BATCH: usize = 256;
/// The largest script a document holds (bytes).
const MAX_SOURCE: usize = 4 << 20;
/// The most compiled scripts asked between two changes of the document ([`Scripts`]).
const MAX_CACHE: usize = 256;

/// The strokes files of a document (file -> log): binary, beside the scripts that read them.
pub type StrokeFiles = Arc<BTreeMap<String, Arc<fab_paint_field::Strokes>>>;

/// The document: the project, the scripts' sources (file -> text) and the strokes files they
/// read (`m.strokes`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub project: Project,
    pub scripts: BTreeMap<String, String>,
    pub strokes: StrokeFiles,
}

#[derive(Clone, Debug)]
struct Step {
    state: State,
    label: Arc<str>,
    /// Its number from the window's step counter, taken at its commit.
    seq: u64,
}

#[derive(Clone, Debug)]
struct Receipt {
    request_id: String,
    fingerprint: [u8; 32],
    reply: Applied,
}

/// What a committed (or replayed) transaction did.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    pub head: Head,
    pub replayed: bool,
    /// Plate parts (`instance/solid`) the batch added, in order.
    pub added: Vec<String>,
    /// Parts, objects and instances it named.
    pub changed: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ApplyError {
    /// `expected` is not the current head: inspect and retry against `current`.
    StaleHead { current: Head },
    /// The request id was used before with a different batch.
    RequestIdReused,
    /// Change `index` of the batch cannot be made.
    Invalid { index: usize, message: String },
    /// The project would not build: nothing was committed.
    Build(Failure),
    /// Nothing to undo or redo.
    NoHistory,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::StaleHead { current } => write!(f, "stale head: the document is at {}", current.text()),
            ApplyError::RequestIdReused => write!(f, "request_id was already used for a different batch"),
            ApplyError::Invalid { index, message } => write!(f, "change {index}: {message}"),
            ApplyError::Build(failure) => write!(f, "the project would not build, nothing committed: {failure}"),
            ApplyError::NoHistory => write!(f, "nothing to undo or redo"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryAction {
    Undo,
    Redo,
}

/// One argument of an object's placement.
#[derive(Clone, Debug, PartialEq)]
pub enum PlaceArg {
    /// Where its model origin sits on the bed (mm); none: arranged with the free objects.
    At(Option<[f64; 2]>),
    /// Tool edit targets in bed mm, resolved to `At` against the built footprint before applying.
    Center([f64; 2]),
    Corner([f64; 2]),
    /// Degrees about the printer's X, Y, Z.
    Rotate([f64; 3]),
    /// Factors along the printer's X, Y, Z of the unturned object (all three equal: a uniform
    /// scale), before its turn.
    Scale([f64; 3]),
    Drop(bool),
    Name(Option<String>),
    /// The print colour it prints as (none: the project's first).
    Color(Option<String>),
    /// Its paint materials (sRGB 0xRRGGBBAA) and the print colours they print as.
    Paint(Vec<(u32, String)>),
    Print(bool),
    /// On the plate or not.
    Show(bool),
    /// Out of the views, kept on the plate (`hidden: true`).
    Hidden(bool),
    Settings(Vec<(String, String)>),
    /// One of its overrides, the others kept (none: it follows the plate again). Rapid edits
    /// of different keys each land, whatever the view had seen when it sent them.
    Setting { key: String, value: Option<String> },
    /// What the user marked on its supports ([`project::SupportMark`]), all of it.
    Supports(Vec<project::SupportMark>),
    /// One more tree the user deleted, by its tips: the marks made before are kept, so rapid
    /// deletes each land, whatever the view had seen when it sent them.
    NoTree(Vec<[f64; 3]>),
}

/// One change of a transaction.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// An edit of a script's source; its sites and solids are the script's own names.
    Script { file: String, edit: Edit },
    /// How an emitted object prints (`object`: its name in the script); `instance` may
    /// name a linked clone (`bracket~2`).
    Place { instance: String, object: String, arg: PlaceArg },
    /// The plate's global print settings: these set, those back to the profile.
    PlateSettings { set: Vec<(String, String)>, reset: Vec<String> },
    /// Set the gap between shelf slots in mm, or remove the project's override.
    ArrangeGap(Option<f64>),
    /// The print colour named `name`: set to `set` (added when new; a new name renames it and
    /// what names it), or removed (`None`: parts and paint naming it go back to the first).
    Color { name: String, set: Option<project::ColorDef> },
    /// Print colour `from` named `to`, and everything that names it with it: the project's
    /// `color(..)`, the placements' `color:` and `paint:` pairs and the object scripts'
    /// `Material{name}`s (fills and strokes). Refused when a colour is named `to` already.
    RenameColor { from: String, to: String },
    /// A declared parameter shared by the base and all linked clones (metres, radians);
    /// a clone id resolves to its script instance. None: the script's own value.
    Param { instance: String, name: String, value: Option<f64> },
    /// Place a script (again): a new instance named from `name` (default: the file's stem).
    /// `source` adds the file to the document; without it the file must be in it. Its
    /// objects are free, except that `at` puts its first one there.
    AddInstance { file: String, source: Option<String>, name: Option<String>, at: Option<[f64; 2]> },
    /// Take one placement off the plate. Removing the original promotes the lowest link;
    /// without links its ScriptRef is removed (the script file stays on disk).
    RemoveInstance { instance: String },
    /// Remove a ScriptRef and all its linked placements. Drop unreferenced source from state.
    RemoveScript { instance: String },
    /// Another placement of the same script and parameters. Copy the source link's overrides;
    /// `at` anchors its first object, preserving stored offsets; none makes all objects free.
    Clone { instance: String, at: Option<[f64; 2]> },
    /// Fork the source file and parameters into an independent ScriptRef, copying the source
    /// link's placements and positioning them as Clone does.
    Copy { instance: String, at: Option<[f64; 2]> },
    /// Append a saved ruler in plate coordinates (mm).
    MeasureAdd { measure: project::Measure },
    /// Edit a ruler by its index; `name: Some(None)` clears its display name.
    MeasureSet { index: usize, points: Option<Vec<[f64; 3]>>, name: Option<Option<String>> },
    /// Remove a saved ruler; later indices shift down by one.
    MeasureRemove { index: usize },
    /// The test print named `name`: set to `set` (added when new; a new name renames it), or
    /// removed (`None`).
    TestPrint { name: String, set: Option<project::TestPrint> },
    /// One hand-drawn stroke appended to a strokes file (made when new), points in the model
    /// frame (mm); a paint stroke's colour is `rgb` (linear; none: erase). The print colour it
    /// prints as is its placement's paint pair for that colour.
    Stroke { file: String, op: fab_paint_field::StrokeOp, rgb: Option<[f32; 3]>, radius: f64, strength: f64, points: Vec<[f64; 3]> },
}

impl Change {
    /// A placement change of the object a plate part (`instance/solid`) or object id belongs to.
    pub fn place(c: &Compiled, id: &str, arg: PlaceArg) -> Option<Change> {
        let (i, o) = c.object(id)?;
        Some(Change::Place { instance: i.name.clone(), object: o.name.clone(), arg })
    }
}

/// A plate's electronics documents (file -> text), opaque to the model ([`FabModel::set_docs`]).
pub type Docs = BTreeMap<String, Arc<str>>;

/// A plate's binary electronics files, opaque to the model ([`FabModel::set_elec_assets`]).
pub type ElecAssets = BTreeMap<String, Arc<[u8]>>;

/// A plate's device memories: each co-simulated chip's non-volatile bytes (an MCU's EEPROM) as
/// its runs left them, by file (`<board>.<part>.eeprom`), opaque to the model
/// ([`FabModel::set_elec_memory`]).
pub type ElecMemory = BTreeMap<String, Arc<[u8]>>;

/// Project-relative files captured by the reader that put this document on the bed.
#[derive(Debug, Default)]
pub struct LoadedProject {
    pub manifest: String,
    pub files: BTreeMap<String, Arc<[u8]>>,
}

/// A committed transition on the model worker. The live model holds the new state
/// and can render its manifest with the same units used by the project writer.
pub enum JournalEvent<'a> {
    Commit { seq: u64, label: &'a str },
    History { undo: bool, before: Option<u64>, after: Option<u64> },
}

pub trait JournalSink: Send + Sync {
    fn receive(&self, event: JournalEvent<'_>, old: &State, model: &FabModel);
}

pub struct FabModel {
    journal: Option<Arc<dyn JournalSink>>,
    pub opened_files: Option<LoadedProject>,
    pub name: String,
    /// The meshes scripts can import (`m.import("cat")`).
    pub assets: Arc<program::Assets>,
    /// The imported meshes' painted leaves, in their asset frames.
    pub facets: Arc<program::FacetAssets>,
    /// The pictures picture parts print (`m.picture({image})`), by file beside the scripts.
    pub pictures: Arc<picture::Pictures>,
    /// The printer inputs scripts compile with (the weave builders' sizes); the app sets them.
    context: program::CompileContext,
    /// The libraries and named inputs scripts compile with.
    env: ScriptEnv,
    state: State,
    head: Head,
    undo: VecDeque<Step>,
    redo: Vec<Step>,
    /// Numbers come from this shared counter; a reservation takes one early.
    steps: Arc<AtomicU64>,
    /// The newest entry logged by either history.
    logged: Arc<AtomicU64>,
    /// Redo stays valid while no entry was logged since the undo.
    redo_mark: u64,
    receipts: VecDeque<Receipt>,
    cache: Mutex<Scripts>,
    /// The fields solved for graded and grown parts, by script file: the last solved, which every
    /// build of the script reads while the app solves what its program's requests ask next, so an
    /// edit never drops a part back to its ungraded web or its ungrown space.
    solved: HashMap<String, program::Solved>,
    /// What each graded part's last solve was asked for, by file and part.
    solved_asked: HashMap<(String, String), u64>,
    /// The shapes grown parts last settled at, by file and part, which a save writes beside their
    /// scripts.
    stored: HashMap<(String, String), Arc<grown::Stored>>,
    /// The plate's electronics documents: outside the undoable `State` and in no `Step`, like the
    /// meshes; set only by [`Self::set_docs`]. The model never reads them.
    docs: Arc<Docs>,
    /// Documents removed by Fab, with their last text: save bookkeeping, outside content and head.
    docs_removed: BTreeMap<String, Arc<str>>,
    /// Each document's text hash with the text it hashed, by file ([`Self::content`]).
    docs_hashes: BTreeMap<String, (Arc<str>, [u8; 32])>,
    /// Electronics assets stay outside object history, alongside the documents that name them.
    elec_assets: Arc<ElecAssets>,
    /// Each asset's content hash with the bytes it hashed, by file ([`Self::content`]).
    elec_assets_hashes: BTreeMap<String, (Arc<[u8]>, [u8; 32])>,
    /// Assets Fab removed, with their content hash and no bytes: save bookkeeping like
    /// `docs_removed`, outside content and head.
    elec_assets_removed: BTreeMap<String, [u8; 32]>,
    /// Device memories stay outside the undoable `State` and in no `Step`. They are not content
    /// and are set only by [`Self::set_elec_memory`].
    elec_memory: Arc<ElecMemory>,
}

fn content_hash(state: &State) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(format!("{:?}", state.project).as_bytes());
    for (file, source) in &state.scripts {
        h.update(&[0]);
        h.update(file.as_bytes());
        h.update(&[0]);
        h.update(source.as_bytes());
    }
    for (file, log) in state.strokes.iter() {
        h.update(&[2]);
        h.update(file.as_bytes());
        h.update(&[0]);
        h.update(&log.key().to_le_bytes());
    }
    h.finish()
}

/// The first problem of a script that does not evaluate.
fn first(file: &str, error: impl Into<lang::EvalError>) -> Failure {
    let (diags, imports) = match error.into() {
        lang::EvalError::Imports(d) => (d, true),
        lang::EvalError::Document(d) => (d, false),
    };
    let mut f = diags.first().map(Failure::program).unwrap_or(Failure { lost: None, file: String::new(), line: 1, col: 1, id: String::new(), op: String::new(), message: "the script does not evaluate".into() });
    if imports {
        // The project editor's text has a one-line header before its ordered imports.
        f.line += 1;
        f.message = format!("project {}", f.message);
    } else {
        f.file = file.to_string();
    }
    f
}

fn label_of(changes: &[Change]) -> String {
    let edit = |e: &Edit| match e {
        Edit::Set { site, .. } => format!("set {site}"),
        Edit::Field { solid, field, .. } | Edit::Unset { solid, field } => format!("edit {solid}.{field}"),
        Edit::Add { code, .. } => format!("add {}", code.trim_start_matches("let ").split([' ', '=']).next().unwrap_or("")),
        Edit::Replace { solid, .. } => format!("replace {solid}"),
        Edit::Operand { solid, code } => format!("{solid} takes {code}"),
        Edit::Remove { solid } => format!("remove {solid}"),
        Edit::Rename { solid, to } => format!("rename {solid} to {to}"),
        Edit::Truncate { solid, to } => format!("roll back {solid} to {to}"),
        Edit::After { solid, .. } => format!("build on {solid}"),
        Edit::Move { solid, to } => format!("move {solid} to {to}"),
        Edit::RootArg { solid, name, .. } => format!("{solid} {name}"),
        Edit::Root { kind, solid, add } => format!("{}{kind} {solid}", if *add { "" } else { "un" }),
        Edit::Splice { .. } => "edit code".into(),
        Edit::Program { .. } => "new script".into(),
    };
    let one = |c: &Change| match c {
        Change::Script { file, edit: e } => format!("{}: {}", project::stem(file), edit(e)),
        Change::Place { object, arg, .. } => {
            let what = match arg {
                PlaceArg::At(_) | PlaceArg::Center(_) | PlaceArg::Corner(_) => "move",
                PlaceArg::Rotate(_) => "turn",
                PlaceArg::Scale(_) => "scale",
                PlaceArg::Drop(_) => "drop",
                PlaceArg::Name(_) => "rename",
                PlaceArg::Color(_) | PlaceArg::Paint(_) => "colour",
                PlaceArg::Print(_) => "print",
                PlaceArg::Show(_) => "show",
                PlaceArg::Hidden(_) => "hide",
                PlaceArg::Settings(_) | PlaceArg::Setting { .. } => "settings",
                PlaceArg::Supports(_) | PlaceArg::NoTree(_) => "supports",
            };
            format!("{what} {object}")
        }
        Change::PlateSettings { .. } => "plate settings".to_string(),
        Change::ArrangeGap(_) => "arrange gap".to_string(),
        Change::Color { name, set: Some(_) } => format!("colour {name}"),
        Change::Color { name, set: None } => format!("remove colour {name}"),
        Change::RenameColor { from, to } => format!("rename colour {from} to {to}"),
        Change::Param { instance, name, .. } => format!("set {instance}/{name}"),
        Change::AddInstance { file, .. } => format!("place {}", project::stem(file)),
        Change::RemoveInstance { instance } => format!("remove {instance}"),
        Change::RemoveScript { instance } => format!("remove script {instance}"),
        Change::Clone { instance, .. } => format!("clone {instance}"),
        Change::Copy { instance, .. } => format!("copy {instance}"),
        Change::MeasureAdd { .. } => "add measure".into(),
        Change::MeasureSet { index, .. } => format!("edit measure {index}"),
        Change::MeasureRemove { index } => format!("remove measure {index}"),
        Change::TestPrint { name, set: Some(_) } => format!("test print {name}"),
        Change::TestPrint { name, set: None } => format!("remove test print {name}"),
        Change::Stroke { file, op, .. } => format!("{}: {}", project::stem(file), if op.is_paint() { "paint stroke" } else { "sculpt stroke" }),
    };
    match changes {
        [c] => one(c),
        [a, rest @ ..] => format!("{} (+{})", one(a), rest.len()),
        [] => String::new(),
    }
}

/// A script file name for a new object script from `name` (`lid` -> `lid.cad.splash`).
pub fn script_file(name: &str) -> String {
    format!("{name}.{}", program::SCRIPT_EXTENSION)
}

/// The compiled scripts kept, by what they are compiled from. A compiled script fills in its
/// shapes' slicing data as it is used, so what goes with it is dear to keep: it stays while the
/// document's state or the one before asks it. An undo further back compiles it again from the
/// text its step holds, and fills it in as an edit does.
#[derive(Default)]
struct Scripts {
    /// Asked since the document last changed.
    asked: HashMap<[u8; 32], Arc<Script>>,
    /// Asked before that, and not since.
    before: HashMap<[u8; 32], Arc<Script>>,
}

impl Scripts {
    fn get(&mut self, key: &[u8; 32]) -> Option<Arc<Script>> {
        if let Some(s) = self.asked.get(key) {
            return Some(s.clone());
        }
        let s = self.before.remove(key)?;
        self.asked.insert(*key, s.clone());
        Some(s)
    }

    fn put(&mut self, key: [u8; 32], script: Arc<Script>) {
        if self.asked.len() >= MAX_CACHE {
            self.turn();
        }
        self.asked.insert(key, script);
    }

    /// The document changed: what was not asked since it last did goes.
    fn turn(&mut self) {
        self.before = std::mem::take(&mut self.asked);
    }
}

/// Everything a script compile reads, including only the host inputs its calls can name.
fn script_key(file: &str, source: &str, assets: &Arc<program::Assets>, facets: &Arc<program::FacetAssets>, pictures: &Arc<picture::Pictures>, context: &program::CompileContext, env: &ScriptEnv, overrides: &[((u32, u32), f64)], state: &State, solved: &program::Solved, skip: &[String]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(&(state.project.imports.len() as u64).to_le_bytes());
    for import in state.project.imports.iter() {
        h.update(&(import.len() as u64).to_le_bytes());
        h.update(import.as_bytes());
    }
    // The solids a draft builds as their input.
    for id in skip {
        h.update(&[2]);
        h.update(id.as_bytes());
    }
    // The meshes it can import are part of what it builds.
    h.update(&(Arc::as_ptr(assets) as usize).to_le_bytes());
    h.update(&(Arc::as_ptr(facets) as usize).to_le_bytes());
    h.update(&(Arc::as_ptr(pictures) as usize).to_le_bytes());
    // So are the printer's weave sizes.
    let w = &context.weave;
    for v in [w.bar, w.clearance, w.clearance_z] {
        h.update(&v.to_bits().to_le_bytes());
    }
    h.update(file.as_bytes());
    h.update(&[0]);
    h.update(source.as_bytes());
    // The paint files it can read.
    for (f, t) in state.scripts.iter().filter(|(f, _)| program::is_paint_file(f)) {
        h.update(&[1]);
        h.update(f.as_bytes());
        h.update(&[0]);
        h.update(t.as_bytes());
    }
    for ((l, c), v) in overrides {
        h.update(&l.to_le_bytes());
        h.update(&c.to_le_bytes());
        h.update(&v.to_bits().to_le_bytes());
    }
    // The fields its graded and grown parts read.
    let mut graded: Vec<(&String, u64)> = solved.iter().map(|(part, g)| (part, g.hash())).collect();
    graded.sort();
    for (part, hash) in graded {
        h.update(&[3]);
        h.update(part.as_bytes());
        h.update(&hash.to_le_bytes());
    }
    for (kind, generator) in &env.generators {
        h.update(&[9]);
        h.update(&(kind.len() as u64).to_le_bytes());
        h.update(kind.as_bytes());
        h.update(&(Arc::as_ptr(generator) as *const () as usize).to_le_bytes());
    }
    for lib in env.libraries.iter() {
        h.update(&[4]);
        h.update(&(lib.file.len() as u64).to_le_bytes());
        h.update(lib.file.as_bytes());
        h.update(&(lib.source.len() as u64).to_le_bytes());
        h.update(lib.source.as_bytes());
    }
    if let Some(lazy) = &env.inputs.lazy {
        h.update(&[10]);
        h.update(&lazy.id.to_le_bytes());
    }
    for (kind, name) in input_reads(source, env) {
        h.update(&[6]);
        h.update(&(kind.len() as u64).to_le_bytes());
        h.update(kind.as_bytes());
        h.update(&(name.len() as u64).to_le_bytes());
        h.update(name.as_bytes());
        match env.inputs.stamp(&(kind, name)) {
            Some(stamp) => {
                h.update(&[7]);
                h.update(&stamp);
            }
            None => h.update(&[8]),
        }
    }
    h.finish()
}

/// The host inputs a script can read, found by syntax in it and in every library (a call's literal
/// first argument; a call with a computed name reads every input of its kind), sorted. What an
/// evaluation reads (`Program::inputs_read`) is a subset: [`FabModel::script`] logs any read
/// outside it and never serves a cached compile whose reads moved. Only the kinds the host gave
/// values of: a kind its lazy provider alone serves keys on the provider ([`script_key`]), whose
/// values are fixed, so the source's names already say what it reads.
fn input_reads(source: &str, env: &ScriptEnv) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for kind in env.inputs.kinds.iter().filter(|k| env.gives(k)) {
        let (mut names, mut dynamic) = program::calls_of_with_dynamic(source, kind);
        for (k, lib_names, lib_dynamic) in env.library_reads() {
            if k == kind {
                names.extend(lib_names.iter().cloned());
                dynamic |= *lib_dynamic;
            }
        }
        if dynamic {
            names.extend(env.inputs.values.keys().filter(|(k, _)| k == kind).map(|(_, n)| n.clone()));
        }
        names.sort();
        names.dedup();
        out.extend(names.into_iter().map(|n| (kind.clone(), n)));
    }
    out
}

impl FabModel {
    /// An empty project.
    pub fn new(name: &str) -> FabModel {
        Self::open(name, State::default())
    }

    /// A document holding `state` (not evaluated yet).
    pub fn open(name: &str, mut state: State) -> FabModel {
        paint::migrate_strokes(&mut state);
        let head = Head { generation: 0, content: content_hash(&state) };
        FabModel { journal: None, opened_files: None, name: name.into(), assets: Default::default(), facets: Default::default(), pictures: Default::default(), context: Default::default(), env: Default::default(), state, head, undo: VecDeque::new(), redo: Vec::new(), steps: Arc::new(AtomicU64::new(0)), logged: Arc::new(AtomicU64::new(0)), redo_mark: 0, receipts: VecDeque::new(), cache: Mutex::default(), solved: HashMap::new(), solved_asked: HashMap::new(), stored: HashMap::new(), docs: Arc::default(), docs_removed: BTreeMap::new(), docs_hashes: BTreeMap::new(), elec_assets: Arc::default(), elec_assets_hashes: BTreeMap::new(), elec_assets_removed: BTreeMap::new(), elec_memory: Arc::default() }
    }

    pub fn set_journal(&mut self, journal: Option<Arc<dyn JournalSink>>) {
        self.journal = journal;
    }

    /// Set the window's number and logged-entry counters, shared with the electronics engine.
    pub fn set_steps(&mut self, steps: Arc<AtomicU64>, logged: Arc<AtomicU64>) {
        self.steps = steps;
        self.logged = logged;
    }

    /// Numbers come from this shared counter; a reservation takes one early.
    pub fn steps(&self) -> &Arc<AtomicU64> {
        &self.steps
    }

    /// The newest entry logged by either history.
    pub fn logged(&self) -> &Arc<AtomicU64> {
        &self.logged
    }

    /// The plate's electronics documents ([`Self::set_docs`]).
    pub fn docs(&self) -> &Arc<Docs> {
        &self.docs
    }

    /// Documents Fab removed and their last text; a returning name clears its record.
    pub fn docs_removed(&self) -> &BTreeMap<String, Arc<str>> {
        &self.docs_removed
    }

    /// Put the plate's electronics documents in. Moves neither the head nor the undo and redo
    /// lists, and turns no script cache: an object undo never reverts them. O(documents); a
    /// document's text is hashed only when its `Arc` is not the one hashed last. Removal tracking
    /// visits the old and new document names and retains Arc texts for saves; nothing per frame.
    pub fn set_docs(&mut self, docs: Arc<Docs>) {
        for (file, text) in self.docs.iter() {
            if !docs.contains_key(file) {
                self.docs_removed.insert(file.clone(), text.clone());
            }
        }
        let old = std::mem::take(&mut self.docs_hashes);
        self.docs_hashes = docs.iter().map(|(file, text)| {
            self.docs_removed.remove(file);
            let hash = match old.get(file) {
                Some((hashed, hash)) if Arc::ptr_eq(hashed, text) => *hash,
                _ => {
                    let mut h = Sha256::new();
                    h.update(text.as_bytes());
                    h.finish()
                }
            };
            (file.clone(), (text.clone(), hash))
        }).collect();
        self.docs = docs;
    }

    /// The plate's binary electronics files ([`Self::set_elec_assets`]).
    pub fn elec_assets(&self) -> &Arc<ElecAssets> {
        &self.elec_assets
    }

    /// Assets Fab removed and their content hash; a returning name clears its record.
    pub fn elec_assets_removed(&self) -> &BTreeMap<String, [u8; 32]> {
        &self.elec_assets_removed
    }

    /// Put the plate's electronics assets in without moving its head, undo or redo lists, or
    /// script caches. O(assets); bytes are hashed only when their `Arc` changed since last time.
    /// A name that goes keeps its hash as a removal record, so a save removes its file.
    pub fn set_elec_assets(&mut self, assets: Arc<ElecAssets>) {
        let old = std::mem::take(&mut self.elec_assets_hashes);
        for (file, (_, hash)) in &old {
            if !assets.contains_key(file) {
                self.elec_assets_removed.insert(file.clone(), *hash);
            }
        }
        self.elec_assets_hashes = assets.iter().map(|(file, bytes)| {
            self.elec_assets_removed.remove(file);
            let hash = match old.get(file) {
                Some((hashed, hash)) if Arc::ptr_eq(hashed, bytes) => *hash,
                _ => {
                    let mut h = Sha256::new();
                    h.update(bytes);
                    h.finish()
                }
            };
            (file.clone(), (bytes.clone(), hash))
        }).collect();
        self.elec_assets = assets;
    }

    /// The plate's device memories ([`Self::set_elec_memory`]).
    pub fn elec_memory(&self) -> &Arc<ElecMemory> {
        &self.elec_memory
    }

    /// Store the device memories in O(1), without moving the head, undo or redo, or any hash.
    /// A run writing its chip's memory is no edit: it makes no undo step and does not change
    /// [`Self::content`], so running a demo never turns it into a plate.
    pub fn set_elec_memory(&mut self, memory: Arc<ElecMemory>) {
        self.elec_memory = memory;
    }

    /// The plate's content: the object document's (`head.content`) with each electronics
    /// document's name and hash, then each asset's, folded in in name order. With neither it is
    /// `head.content`, so the saved hashes of plates without them still match. What `saved`,
    /// `modified` and a demo's fork compare; the head stays the object document's (the tools'
    /// stale-head token).
    pub fn content(&self) -> [u8; 32] {
        if self.docs.is_empty() && self.elec_assets.is_empty() {
            return self.head.content;
        }
        let mut h = Sha256::new();
        h.update(&self.head.content);
        for (file, (_, hash)) in &self.docs_hashes {
            h.update(&[3]);
            h.update(file.as_bytes());
            h.update(&[0]);
            h.update(hash);
        }
        for (file, (_, hash)) in &self.elec_assets_hashes {
            h.update(&[4]);
            h.update(file.as_bytes());
            h.update(&[0]);
            h.update(hash);
        }
        h.finish()
    }

    /// No object script, electronics document or asset: what an empty plate's working copy is.
    pub fn is_empty(&self) -> bool {
        self.state.project.scripts.is_empty() && self.docs.is_empty() && self.elec_assets.is_empty()
    }

    /// A document of one script placed once (a fixture or a single object).
    pub fn with_script(name: &str, file: &str, source: &str) -> FabModel {
        Self::with_script_files(name, file, source, BTreeMap::new(), BTreeMap::new())
    }

    /// [`Self::with_script`] with the other files it reads (its paint files: name -> text; its
    /// strokes files: name -> log).
    pub fn with_script_files(name: &str, file: &str, source: &str, files: BTreeMap<String, String>, strokes: BTreeMap<String, Arc<fab_paint_field::Strokes>>) -> FabModel {
        let mut state = State { strokes: Arc::new(strokes), ..Default::default() };
        state.scripts.extend(files);
        state.scripts.insert(file.to_string(), source.to_string());
        state.project.scripts.push(ScriptRef { name: project::stem(file), file: file.to_string(), params: Vec::new() });
        Self::open(name, state)
    }

    /// A document from a project's text and its scripts' sources (file -> text). A
    /// placement that leaves out `object` takes its instance's only object.
    pub fn load(name: &str, project_text: &str, scripts: BTreeMap<String, String>) -> Result<FabModel, Failure> {
        Self::load_with_assets(name, project_text, scripts, BTreeMap::new(), Default::default(), Default::default(), Default::default(), ScriptEnv::default())
    }

    /// [`Self::load`] with the project's meshes and painted leaves: resolving a placement's object compiles its
    /// script, and a script that imports a mesh needs it then.
    /// `env`: the host inputs the model builds under; resolving a placement compiles its script under them.
    pub fn load_with_assets(name: &str, project_text: &str, scripts: BTreeMap<String, String>, strokes: BTreeMap<String, Arc<fab_paint_field::Strokes>>, assets: Arc<program::Assets>, facets: Arc<program::FacetAssets>, pictures: Arc<picture::Pictures>, env: ScriptEnv) -> Result<FabModel, Failure> {
        let project = project::parse(project_text).map_err(|d| first("", d))?;
        let mut model = Self::open(name, State { project, scripts, strokes: Arc::new(strokes) });
        model.env = env;
        model.assets = assets;
        model.facets = facets;
        model.pictures = pictures;
        let mut state = model.state.clone();
        for i in 0..state.project.places.len() {
            if !state.project.places[i].object.is_empty() {
                continue;
            }
            let inst = state.project.places[i].instance.clone();
            let Some(sref) = state.project.script(&inst).cloned() else { continue };
            let s = model.compile_script(&sref.file)?;
            if s.object(&inst).is_some() {
                state.project.places[i].object = inst.clone();
                continue;
            }
            match s.objects.as_slice() {
                [one] => state.project.places[i].object = one.name.clone(),
                many => {
                    let names: Vec<&str> = many.iter().map(|o| o.name.as_str()).collect();
                    return Err(Failure { lost: None, file: String::new(), line: 1, col: 1, id: String::new(), op: String::new(), message: format!("place(\"{inst}\") needs object: {} emits {}", sref.file, names.join(", ")) });
                }
            }
        }
        model.head = Head { generation: 0, content: content_hash(&state) };
        model.state = state;
        Ok(model)
    }

    pub fn head(&self) -> Head {
        self.head
    }
    pub fn state(&self) -> &State {
        &self.state
    }
    pub fn project(&self) -> &Project {
        &self.state.project
    }
    /// A script's source by its file name.
    pub fn source(&self, file: &str) -> Option<&str> {
        self.state.scripts.get(file).map(|s| s.as_str())
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        self.redo_top().is_some()
    }
    /// The newest undo step's number, for the shared undo order.
    pub fn undo_top(&self) -> Option<u64> {
        self.undo.back().map(|step| step.seq)
    }
    /// The next redo step's number, for the shared undo order; None after a commit in either history.
    pub fn redo_top(&self) -> Option<u64> {
        if self.logged.load(SeqCst) != self.redo_mark {
            return None;
        }
        self.redo.last().map(|step| step.seq)
    }
    /// Labels of the undo steps, oldest first.
    pub fn undo_labels(&self) -> impl Iterator<Item = &str> {
        self.undo.iter().map(|s| &*s.label)
    }
    /// The undo steps, oldest first: each one's number and label.
    pub fn undo_steps(&self) -> impl DoubleEndedIterator<Item = (u64, &Arc<str>)> + ExactSizeIterator {
        self.undo.iter().map(|s| (s.seq, &s.label))
    }

    /// One script with these parameter values, compiled once.
    /// The project's imports and the paint files a script reads both come from `state`.
    /// `skip`: the solids it builds as their input ([`Self::preview`]).
    fn script(&self, file: &str, source: &str, names: Option<Arc<lang::names::Names>>, overrides: &[((u32, u32), f64)], state: &State, skip: &[String]) -> Result<Arc<Script>, Failure> {
        let solved = self.solved.get(file).cloned().unwrap_or_default();
        let key = script_key(file, source, &self.assets, &self.facets, &self.pictures, &self.context, &self.env, overrides, state, &solved, skip);
        // The key holds the inputs the syntax names; a read it could not see (a library fn reading
        // one, an aliased call) is caught here, so a cached compile never outlives its inputs.
        let same = |s: &Script| s.program.inputs_read.iter().all(|k| self.env.inputs.stamp(k) == s.env.inputs.stamp(k));
        if let Some(s) = self.cache.lock().unwrap().get(&key).filter(|s| same(s)) {
            return Ok(s);
        }
        let mut env = self.env.clone();
        env.imports = state.project.imports.clone();
        let s = Arc::new(program::compile_document(file, source, names, &self.assets, &self.facets, &self.pictures, &self.context, env, overrides.iter().copied().collect(), &solved, skip, &HashMap::new(), &|f| state.scripts.get(f).cloned()).map_err(|d| first(file, d))?);
        if !s.program.inputs_read.is_empty() {
            let seen = input_reads(source, &self.env);
            for read in s.program.inputs_read.iter().filter(|r| self.env.gives(&r.0) && !seen.contains(r)) {
                eprintln!("{file}: reads {}(\"{}\") where no syntax shows it (an aliased call?): its cached compile is checked against that input on each use", read.0, read.1);
            }
        }
        self.cache.lock().unwrap().put(key, s.clone());
        Ok(s)
    }

    /// One script reference with its shared parameter overrides.
    fn compile_ref(&self, state: &State, sref: &ScriptRef, skip: &[String]) -> Result<Arc<Script>, Failure> {
        let fail = |file: &str, message: String| Failure { lost: None, file: file.to_string(), line: 1, col: 1, id: String::new(), op: String::new(), message };
        let Some(source) = state.scripts.get(&sref.file) else { return Err(fail(&sref.file, format!("'{}' is not open in the project", sref.file))) };
        let base = self.script(&sref.file, source, None, &[], state, skip)?;
        let mut overrides = Vec::with_capacity(sref.params.len());
        for (name, v) in &sref.params {
            let Some((_, loc)) = base.param(name) else {
                let names: Vec<&str> = base.program.params.iter().filter_map(|q| q.name.as_deref()).collect();
                return Err(fail(&sref.file, format!("'{}' sets '{name}', which {} does not declare (params: {})", sref.name, sref.file, names.join(", "))));
            };
            overrides.push((loc, *v));
        }
        overrides.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(if overrides.is_empty() { base } else { self.script(&sref.file, source, Some(base.program.names.clone()), &overrides, state, skip)? })
    }

    /// A state compiled: every instance's script with its values, and its placements.
    fn compile_state(&self, state: &State) -> Result<Compiled, Failure> {
        self.compile_skipping(state, &BTreeMap::new())
    }

    /// [`Self::compile_state`], the solids `skip` names (by script) built as their input.
    fn compile_skipping(&self, state: &State, skip: &BTreeMap<String, Vec<String>>) -> Result<Compiled, Failure> {
        let mut instances = Vec::with_capacity(state.project.scripts.len());
        let fail = |file: &str, message: String| Failure { lost: None, file: file.to_string(), line: 1, col: 1, id: String::new(), op: String::new(), message };
        for sref in &state.project.scripts {
            let script = self.compile_ref(state, sref, skip.get(&sref.file).map_or(&[], |s| s.as_slice()))?;
            let mut links = BTreeSet::from([0]);
            links.extend(state.project.places.iter().filter(|p| p.instance == sref.name).map(|p| p.link));
            for link in links {
                let mut placed = Vec::new();
                let placements: std::collections::HashMap<&str, Placement> = script.objects.iter().map(|o| {
                    let p = state.project.place_link(&sref.name, &o.name, link).cloned().unwrap_or_else(|| {
                        let mut p = Placement::new(&sref.name, &o.name);
                        p.link = link;
                        p
                    });
                    (o.name.as_str(), p)
                }).collect();
                let hosts: std::collections::HashSet<&str> = script.objects.iter()
                    .filter(|o| placements[o.name.as_str()].show)
                    .filter_map(|o| o.insert.as_ref().map(|i| i.into.as_str())).collect();
                for o in &script.objects {
                    let mut p = placements[o.name.as_str()].clone();
                    if !p.show && !hosts.contains(o.name.as_str()) {
                        continue;
                    }
                    // A board's stand-in (`board_solid`, linked `board:<name>`) is the board itself,
                    // drawn in its place: never sliced or printed, whatever its place says.
                    if o.link.as_deref().is_some_and(|l| l.starts_with("board:")) {
                        p.print = false;
                    }
                    if let Some(insert) = &o.insert {
                        let host = &placements[insert.into.as_str()];
                        p.at = host.at;
                        p.rotate = host.rotate;
                        p.scale = host.scale;
                        p.drop = host.drop;
                        p.print = false;
                    }
                    p.print &= p.show;
                    placed.extend(o.bodies.iter().map(|b| p.placed(*b)));
                }
                instances.push(Instance { name: link_name(&sref.name, link), script_instance: sref.name.clone(), link, script: script.clone(), placed });
            }
        }
        // Every placement names an emitted object.
        for p in &state.project.places {
            let Some(i) = instances.iter().find(|i| i.name == p.instance) else { return Err(fail("", format!("'{}' is placed but no script is named so", p.instance))) };
            if i.script.object(&p.object).is_none() {
                let names: Vec<&str> = i.script.objects.iter().map(|o| o.name.as_str()).collect();
                return Err(fail(&i.script.file, format!("'{}' places '{}', which {} does not emit (objects: {})", p.instance, p.object, i.script.file, names.join(", "))));
            }
        }
        Ok(Compiled { instances, settings: state.project.settings.clone(), colors: crate::eval::print_colors(&state.project), strokes: state.strokes.clone() })
    }

    /// The current project, compiled (no meshes).
    pub fn compile(&self) -> Result<Compiled, Failure> {
        self.compile_state(&self.state)
    }

    /// One script compiled at its own values (for code views and edits).
    /// The printer inputs scripts compile with.
    pub fn context(&self) -> &program::CompileContext {
        &self.context
    }

    /// Set the libraries and host inputs; unchanged script dependencies keep their cached compile.
    pub fn set_env(&mut self, env: ScriptEnv) {
        self.env = env;
    }

    /// The libraries and host inputs scripts compile with (a host carries them to the next model).
    pub fn env(&self) -> &ScriptEnv {
        &self.env
    }

    /// Set the printer inputs scripts compile with (the next compile rebuilds what they change).
    pub fn set_context(&mut self, context: program::CompileContext) {
        self.context = context;
    }

    /// What a graded part's stress now depends on (`None` when nothing poses it). True when that
    /// changed: the app then solves, or with `None` drops the part's stress.
    pub fn ask_solved(&mut self, file: &str, part: &str, asked: Option<u64>) -> bool {
        let key = (file.to_string(), part.to_string());
        let was = match asked {
            Some(a) => self.solved_asked.insert(key, a),
            None => self.solved_asked.remove(&key),
        };
        was != asked
    }

    /// The answer to what [`Self::ask_solved`] was told: a graded part's stress, or none (nothing
    /// to grade by). An answer an edit has outrun changes nothing. True when it changed (the
    /// project then needs a rebuild).
    pub fn set_solved(&mut self, file: &str, part: &str, asked: Option<u64>, grid: Option<Arc<makepad_model_script::lang::csg_sdf::SdfGrid>>) -> bool {
        if self.solved_asked.get(&(file.to_string(), part.to_string())).copied() != asked {
            return false;
        }
        self.put_solved(file, part, grid)
    }

    /// A grown part's shape, as it is now, or none (nothing poses it). A grow publishes as it goes,
    /// so every shape is news and nothing outruns it; a stale one comes down when the app stops the
    /// run. True when it changed (the project then needs a rebuild).
    pub fn set_grown(&mut self, file: &str, part: &str, grid: Option<Arc<makepad_model_script::lang::csg_sdf::SdfGrid>>) -> bool {
        self.put_solved(file, part, grid)
    }

    /// The shape a grown part settled at, to keep, or none to keep.
    pub fn set_stored(&mut self, file: &str, part: &str, stored: Option<Arc<grown::Stored>>) {
        let key = (file.to_string(), part.to_string());
        match stored {
            Some(s) => {
                self.stored.insert(key, s);
            }
            None => {
                self.stored.remove(&key);
            }
        }
    }

    /// The shape kept for a grown part, and the surface the part has now.
    pub fn stored(&self, file: &str, part: &str) -> Option<(Arc<grown::Stored>, Arc<makepad_model_script::lang::csg_sdf::SdfGrid>)> {
        let stored = self.stored.get(&(file.to_string(), part.to_string()))?;
        Some((stored.clone(), self.solved.get(file)?.get(part)?.clone()))
    }

    /// Every shape kept: file, part and the shape.
    pub fn all_stored(&self) -> impl Iterator<Item = (&str, &str, &Arc<grown::Stored>)> {
        self.stored.iter().map(|((file, part), s)| (file.as_str(), part.as_str(), s))
    }

    fn put_solved(&mut self, file: &str, part: &str, grid: Option<Arc<makepad_model_script::lang::csg_sdf::SdfGrid>>) -> bool {
        let parts = self.solved.entry(file.to_string()).or_default();
        match grid {
            Some(g) if parts.get(part).is_some_and(|had| had.hash() == g.hash()) => false,
            Some(g) => {
                parts.insert(part.to_string(), g);
                true
            }
            None => parts.remove(part).is_some(),
        }
    }

    pub fn compile_script(&self, file: &str) -> Result<Arc<Script>, Failure> {
        let source = self.state.scripts.get(file).ok_or_else(|| Failure { lost: None, file: file.into(), line: 1, col: 1, id: String::new(), op: String::new(), message: format!("no script '{file}'") })?;
        self.script(file, source, None, &[], &self.state, &[])
    }

    /// Evaluate the current project.
    pub fn evaluate(&self, eval: &mut Evaluator, cancelled: Option<&dyn Fn() -> bool>) -> Result<(Compiled, Evaluated), Failure> {
        let c = self.compile()?;
        eval.evaluate_compiled(c, self.state.project.arrange_gap, cancelled)
    }

    /// The project's text (`.fab.splash`), parameter values written in their scripts' units.
    pub fn project_text(&self) -> String {
        let unit = |instance: &str, param: &str| -> project::Unit {
            let Some(sref) = self.state.project.script(instance) else { return project::Unit::Plain };
            let Ok(s) = self.compile_script(&sref.file) else { return project::Unit::Plain };
            let Some(q) = s.program.params.iter().find(|q| q.name.as_deref() == Some(param)) else { return project::Unit::Plain };
            let written = q.value_span.as_ref().and_then(|r| s.source().get(r.clone())).unwrap_or("").trim();
            let kind = q.call.kind.as_deref().unwrap_or("");
            if matches!(kind, "length" | "radius" | "diameter") || written.ends_with('m') {
                project::Unit::Mm
            } else if kind == "angle" || written.ends_with('°') || written.ends_with("deg") {
                project::Unit::Degrees
            } else {
                project::Unit::Plain
            }
        };
        project::write(&self.state.project, &unit)
    }

    /// The state after `changes` and the project it compiles to, without committing.
    pub fn edited(&self, changes: &[Change]) -> Result<(State, Compiled), ApplyError> {
        if changes.is_empty() {
            return Err(ApplyError::Invalid { index: 0, message: "empty batch".into() });
        }
        // A selection or a file of placed objects is one undo step, however many parts it has.
        let deletion = changes.iter().all(|c| matches!(c, Change::RemoveInstance { .. } | Change::RemoveScript { .. } | Change::Place { arg: PlaceArg::Show(false), .. }));
        let placement = changes.iter().all(|c| matches!(c, Change::AddInstance { .. } | Change::Place { .. } | Change::PlateSettings { .. } | Change::ArrangeGap(_)));
        if changes.len() > MAX_BATCH && !deletion && !placement {
            return Err(ApplyError::Invalid { index: MAX_BATCH, message: format!("at most {MAX_BATCH} changes per batch") });
        }
        let mut state = self.state.clone();
        let mut placed_at: Vec<(String, [f64; 2])> = Vec::new();
        // Per script, the names this batch's added statements define: each stays before the
        // first statement that reads it, however the batch's later edits use it.
        let mut added: HashMap<String, Vec<String>> = HashMap::new();
        for (index, change) in changes.iter().enumerate() {
            let invalid = |message: String| ApplyError::Invalid { index, message };
            match change {
                Change::Script { file, edit } => {
                    project::script_file_name(file).map_err(invalid)?;
                    // A paint file is made by writing it whole.
                    let source = match state.scripts.get(file) {
                        Some(s) => s.clone(),
                        None if program::is_paint_file(file) && matches!(edit, Edit::Program { .. }) => String::new(),
                        None => return Err(invalid(format!("no script '{file}' in the project"))),
                    };
                    let new = if let Edit::Add { code, at } = edit {
                        added.entry(file.clone()).or_default().extend(lang::edit::defines(code));
                        let roots: Vec<&str> = program::ROOTS.iter().map(|r| r.name).collect();
                        lang::edit::add(&source, code, at, &roots).map_err(invalid)?
                    } else if edit.names_solids() {
                        let s = self.script(file, &source, None, &[], &state, &[]).map_err(|f| if index == 0 { ApplyError::Build(f) } else { invalid(format!("follows changes that leave {file} not evaluating: {f}")) })?;
                        lang::edit::apply(&s.program, edit).map_err(invalid)?
                    } else {
                        match edit {
                            Edit::Program { source } => source.clone(),
                            Edit::Splice { range, text } => {
                                if range.end > source.len() || !source.is_char_boundary(range.start) || !source.is_char_boundary(range.end) || range.start > range.end {
                                    return Err(invalid("the splice is outside the script".into()));
                                }
                                format!("{}{text}{}", &source[..range.start], &source[range.end..])
                            }
                            other => {
                                let s = self.script(file, &source, None, &[], &state, &[]).map_err(ApplyError::Build)?;
                                lang::edit::apply(&s.program, other).map_err(invalid)?
                            }
                        }
                    };
                    let new = match added.get(file) {
                        Some(names) => lang::edit::place_defs(&new, names).map_err(invalid)?,
                        None => new,
                    };
                    if new.len() > MAX_SOURCE {
                        return Err(invalid(format!("a script holds at most {MAX_SOURCE} bytes")));
                    }
                    state.scripts.insert(file.clone(), new);
                }
                Change::Place { instance, object, arg } => {
                    let (base, link) = split_link(instance);
                    let Some(sref) = state.project.script(base).cloned() else { return Err(invalid(format!("no instance '{instance}'"))) };
                    // No object named: the instance's only one.
                    let object = if object.is_empty() {
                        let s = self.compile_ref(&state, &sref, &[]).map_err(ApplyError::Build)?;
                        match s.objects.as_slice() {
                            _ if s.object(base).is_some() => base.to_string(),
                            [one] => one.name.clone(),
                            many => return Err(invalid(format!("'{instance}' emits {}: name the object", many.iter().map(|o| o.name.as_str()).collect::<Vec<_>>().join(", ")))),
                        }
                    } else {
                        object.clone()
                    };
                    // A setting the same as the plate's is no override: never stored.
                    let plate = state.project.settings.clone();
                    let inherits = |k: &str, v: &str| plate.iter().any(|(pk, pv)| pk == k && project::same_setting(pv, v));
                    let p = state.project.place_link_mut(base, &object, link);
                    match arg.clone() {
                        PlaceArg::At(v) => p.at = v,
                        PlaceArg::Center(_) | PlaceArg::Corner(_) => return Err(invalid("place the part with fab.place to resolve its footprint to at".into())),
                        PlaceArg::Rotate(v) => p.rotate = v,
                        PlaceArg::Scale(v) if v.iter().all(|&k| k > 0.0 && k <= 1000.0) => p.scale = v,
                        PlaceArg::Scale(v) => return Err(invalid(format!("scale is a positive factor along each axis, not {v:?}"))),
                        PlaceArg::Drop(v) => p.drop = v,
                        PlaceArg::Name(v) => p.name = v,
                        PlaceArg::Color(v) => p.color = v,
                        PlaceArg::Paint(v) => p.paint = v,
                        PlaceArg::Print(v) => p.print = v,
                        PlaceArg::Show(v) => p.show = v,
                        PlaceArg::Hidden(v) => p.hidden = v,
                        PlaceArg::Supports(v) => p.supports = v,
                        PlaceArg::NoTree(tips) => {
                            let mark = project::SupportMark::NoTree { tips };
                            if !p.supports.contains(&mark) {
                                p.supports.push(mark);
                            }
                        }
                        PlaceArg::Setting { key, value } => {
                            p.settings.retain(|(k, _)| *k != key);
                            if let Some(value) = value {
                                if let Some(fab_plate::SettingScope::Plate(why)) = fab_plate::print_setting(&key).map(|s| s.scope) {
                                    return Err(invalid(format!("'{key}' is the plate's, not an object's: {why}")));
                                }
                                let value = fab_plate::checked_value(&key, &value).map_err(invalid)?;
                                if !inherits(&key, &value) {
                                    p.settings.push((key, value));
                                    p.settings.sort_by(|a, b| a.0.cmp(&b.0));
                                }
                            }
                        }
                        PlaceArg::Settings(mut v) => {
                            for (k, value) in &mut v {
                                if let Some(fab_plate::SettingScope::Plate(why)) = fab_plate::print_setting(k).map(|s| s.scope) {
                                    return Err(invalid(format!("'{k}' is the plate's, not an object's: {why}")));
                                }
                                *value = fab_plate::checked_value(k, value).map_err(invalid)?;
                            }
                            v.retain(|(k, value)| !inherits(k, value));
                            v.sort_by(|a, b| a.0.cmp(&b.0));
                            p.settings = v;
                        }
                    }
                }
                Change::Color { name, set } => {
                    let colors = &mut state.project.colors;
                    let at = colors.iter().position(|c| c.name == *name);
                    match (set, at) {
                        (Some(c), _) if c.name.trim().is_empty() => return Err(invalid("a colour needs a name".into())),
                        (Some(c), _) if c.name != *name && colors.iter().any(|o| o.name == c.name) => return Err(invalid(format!("there is a colour named '{}' already", c.name))),
                        (Some(c), Some(i)) => colors[i] = c.clone(),
                        (Some(c), None) => colors.push(c.clone()),
                        (None, Some(i)) => {
                            colors.remove(i);
                        }
                        (None, None) => {}
                    }
                    let to = set.as_ref().map(|c| c.name.clone());
                    if to.as_deref() != Some(name.as_str()) {
                        for p in &mut state.project.places {
                            if p.color.as_deref() == Some(name.as_str()) {
                                p.color = to.clone();
                            }
                            match &to {
                                Some(to) => p.paint.iter_mut().filter(|(_, n)| n == name).for_each(|(_, n)| *n = to.clone()),
                                None => p.paint.retain(|(_, n)| n != name),
                            }
                        }
                    }
                }
                Change::RenameColor { from, to } => {
                    let to = to.trim();
                    if to.is_empty() {
                        return Err(invalid("a colour needs a name".into()));
                    }
                    if to == from {
                        return Err(invalid(format!("the colour is named '{to}' already")));
                    }
                    // The object scripts and the paint files they read.
                    let scripts: Vec<String> = state.scripts.keys().filter(|f| f.ends_with(&format!(".{}", program::SCRIPT_EXTENSION)) || program::is_paint_file(f)).cloned().collect();
                    let named = |state: &State, name: &str| {
                        state.project.colors.iter().any(|c| c.name == name)
                            || state.project.places.iter().any(|p| p.color.as_deref() == Some(name) || p.paint.iter().any(|(_, n)| n == name))
                            || scripts.iter().any(|f| program::material_names(&state.scripts[f]).iter().any(|n| n == name))
                    };
                    if !named(&state, from) {
                        return Err(invalid(format!("no colour is named '{from}'")));
                    }
                    if named(&state, to) {
                        return Err(invalid(format!("there is a colour named '{to}' already: give it another name, or rename that one first")));
                    }
                    if let Some(c) = state.project.colors.iter_mut().find(|c| c.name == *from) {
                        c.name = to.to_string();
                    }
                    for p in &mut state.project.places {
                        if p.color.as_deref() == Some(from.as_str()) {
                            p.color = Some(to.to_string());
                        }
                        p.paint.iter_mut().filter(|(_, n)| n == from).for_each(|(_, n)| *n = to.to_string());
                    }
                    for file in &scripts {
                        if let Some(new) = program::rename_material_names(&state.scripts[file], from, to).map_err(|e| invalid(format!("renaming in {file}: {e}")))? {
                            state.scripts.insert(file.clone(), new);
                        }
                    }
                }
                Change::ArrangeGap(gap) => state.project.arrange_gap = gap.map(project::checked_arrange_gap).transpose().map_err(invalid)?,
                Change::PlateSettings { set, reset } => {
                    state.project.settings.retain(|(k, _)| !reset.contains(k) && !set.iter().any(|(s, _)| s == k));
                    for (k, v) in set {
                        state.project.settings.push((k.clone(), project::checked_plate_value(k, v).map_err(invalid)?));
                    }
                    state.project.settings.sort_by(|a, b| a.0.cmp(&b.0));
                }
                Change::Param { instance, name, value } => {
                    let (base, _) = split_link(instance);
                    let sref = state.project.scripts.iter_mut().find(|s| s.name == base).ok_or_else(|| invalid(format!("no instance '{instance}'")))?;
                    sref.params.retain(|(k, _)| k != name);
                    if let Some(v) = value {
                        if !v.is_finite() {
                            return Err(invalid(format!("'{name}' is not a number")));
                        }
                        sref.params.push((name.clone(), *v));
                    }
                }
                Change::AddInstance { file, source, name, at } => {
                    project::script_file_name(file).map_err(invalid)?;
                    match (state.scripts.get(file), source) {
                        (Some(have), Some(src)) if have != src => return Err(invalid(format!("'{file}' is already in the project with other text"))),
                        (None, None) => return Err(invalid(format!("no script '{file}' in the project: give its source"))),
                        (_, Some(src)) => {
                            state.scripts.insert(file.clone(), src.clone());
                        }
                        _ => {}
                    }
                    let instance = match name {
                        Some(n) if n.is_empty() || n.contains('/') => return Err(invalid(format!("'{n}' is not an instance name"))),
                        Some(n) if state.project.script(n).is_some() => return Err(invalid(format!("an instance is named '{n}' already"))),
                        Some(n) => n.clone(),
                        None => state.project.fresh_name(&project::stem(file)),
                    };
                    state.project.scripts.push(ScriptRef { name: instance.clone(), file: file.clone(), params: Vec::new() });
                    if let Some(at) = at {
                        placed_at.push((instance, *at));
                    }
                }
                Change::Clone { instance, at } | Change::Copy { instance, at } => {
                    let (base, link) = split_link(instance);
                    let Some(sref) = state.project.script(base).cloned() else { return Err(invalid(format!("no instance '{instance}'"))) };
                    if link > 0 && !state.project.places.iter().any(|p| p.instance == base && p.link == link) {
                        return Err(invalid(format!("no instance '{instance}'")));
                    }
                    if at.is_some_and(|p| p.iter().any(|n| !n.is_finite())) {
                        return Err(invalid("'at' must be two finite coordinates in mm".into()));
                    }
                    let script = self.compile_ref(&state, &sref, &[]).map_err(ApplyError::Build)?;
                    if script.objects.is_empty() && matches!(change, Change::Clone { .. }) {
                        return Err(invalid(format!("'{instance}' emits no objects to clone")));
                    }
                    let mut places: Vec<Placement> = script.objects.iter().map(|o| {
                        state.project.place_link(base, &o.name, link).cloned().unwrap_or_else(|| Placement::new(base, &o.name))
                    }).collect();
                    // Stored centres define the offsets. With a free first object there is no
                    // stored anchor, so other stored centres stay put; free objects remain free.
                    let anchor = places.first().and_then(|p| p.at).or(*at);
                    for (i, p) in places.iter_mut().enumerate() {
                        p.at = match (at, p.at, anchor) {
                            (Some(target), _, _) if i == 0 => Some(*target),
                            (Some(target), Some(old), Some(origin)) => Some([old[0] + target[0] - origin[0], old[1] + target[1] - origin[1]]),
                            _ => None,
                        };
                    }
                    match change {
                        Change::Clone { .. } => {
                            let link = state.project.places.iter().filter(|p| p.instance == base).map(|p| p.link).max().unwrap_or(0).checked_add(1)
                                .ok_or_else(|| invalid("linked placement numbers are exhausted".into()))?;
                            for p in &mut places {
                                p.link = link;
                                p.name = Some(format!("{} {}", p.name.as_deref().unwrap_or(&p.object), u64::from(link) + 1));
                            }
                        }
                        Change::Copy { .. } => {
                            let compiled = self.compile_state(&state).map_err(ApplyError::Build)?;
                            let mut names: BTreeSet<String> = compiled.instances.iter().flat_map(|i| i.placed.iter()).map(|p| p.name.as_ref().unwrap_or(&p.object).clone()).collect();
                            names.extend(state.project.places.iter().map(|p| p.name.as_ref().unwrap_or(&p.object).clone()));
                            let stem = project::stem(&sref.file);
                            let prefix = sref.file.rfind(['/', '\\']).map_or("", |i| &sref.file[..=i]);
                            let file = (2u64..).map(|n| format!("{prefix}{stem}_{n}.{}", program::SCRIPT_EXTENSION))
                                .find(|file| !state.scripts.contains_key(file) && state.project.script(&project::stem(file)).is_none()).unwrap();
                            project::script_file_name(&file).map_err(invalid)?;
                            let name = state.project.fresh_name(&project::stem(&file));
                            let source = state.scripts.get(&sref.file).unwrap().clone();
                            state.scripts.insert(file.clone(), source);
                            state.project.scripts.push(ScriptRef { name: name.clone(), file, params: sref.params.clone() });
                            for p in &mut places {
                                let display = format!("{} copy", p.name.as_deref().unwrap_or(&p.object));
                                let display = if names.contains(&display) {
                                    (2u64..).map(|n| format!("{display} {n}")).find(|n| !names.contains(n)).unwrap()
                                } else { display };
                                names.insert(display.clone());
                                p.name = Some(display);
                                p.instance = name.clone();
                                p.link = 0;
                            }
                        }
                        _ => unreachable!(),
                    }
                    state.project.places.extend(places);
                }
                Change::RemoveInstance { instance } | Change::RemoveScript { instance } => {
                    let (base, link) = split_link(instance);
                    let Some(i) = state.project.scripts.iter().position(|s| s.name == base) else { return Err(invalid(format!("no instance '{instance}'"))) };
                    if matches!(change, Change::RemoveInstance { .. }) {
                        if link > 0 {
                            if !state.project.places.iter().any(|p| p.instance == base && p.link == link) {
                                return Err(invalid(format!("no instance '{instance}'")));
                            }
                            state.project.places.retain(|p| p.instance != base || p.link != link);
                            state.project.test_prints.retain(|t| t.instance != base || t.link != link);
                            continue;
                        }
                        if let Some(promote) = state.project.places.iter().filter(|p| p.instance == base && p.link > 0).map(|p| p.link).min() {
                            state.project.places.retain(|p| p.instance != base || p.link != 0);
                            for p in &mut state.project.places {
                                if p.instance == base && p.link == promote {
                                    p.link = 0;
                                }
                            }
                            state.project.test_prints.retain(|t| t.instance != base || t.link != 0);
                            for t in &mut state.project.test_prints {
                                if t.instance == base && t.link == promote {
                                    t.link = 0;
                                }
                            }
                            continue;
                        }
                    }
                    let file = state.project.scripts.remove(i).file;
                    state.project.places.retain(|p| p.instance != base);
                    state.project.test_prints.retain(|t| t.instance != base);
                    if !state.project.scripts.iter().any(|s| s.file == file) {
                        state.scripts.remove(&file);
                    }
                }
                Change::MeasureAdd { measure } => {
                    project::validate_measure(measure).map_err(invalid)?;
                    state.project.measures.push(measure.clone());
                }
                Change::MeasureSet { index: measure_index, points, name } => {
                    let measure = state.project.measures.get_mut(*measure_index).ok_or_else(|| invalid(format!("no measure {measure_index}")))?;
                    if let Some(points) = points {
                        measure.points = points.clone();
                    }
                    if let Some(name) = name {
                        measure.name = name.clone();
                    }
                    project::validate_measure(measure).map_err(invalid)?;
                }
                Change::TestPrint { name: was, set } => {
                    let at = state.project.test_prints.iter().position(|t| t.name == *was);
                    match set {
                        Some(t) => {
                            project::validate_test_print(t).map_err(invalid)?;
                            if t.name != *was && state.project.test_print(&t.name).is_some() {
                                return Err(invalid(format!("a test print is named '{}' already", t.name)));
                            }
                            if state.project.script(&t.instance).is_none() {
                                return Err(invalid(format!("no instance '{}'", t.instance)));
                            }
                            match at {
                                Some(i) => state.project.test_prints[i] = t.clone(),
                                None => state.project.test_prints.push(t.clone()),
                            }
                        }
                        None => {
                            let i = at.ok_or_else(|| invalid(format!("no test print '{was}'")))?;
                            state.project.test_prints.remove(i);
                        }
                    }
                }
                Change::Stroke { file, op, rgb, radius, strength, points } => {
                    project::script_file_name(file).map_err(invalid)?;
                    if !program::is_strokes_file(file) {
                        return Err(invalid(format!("'{file}' is no strokes file (.{})", program::STROKES_EXTENSION)));
                    }
                    if points.is_empty() || !radius.is_finite() || *radius <= 0.0 || !points.iter().flatten().all(|v| v.is_finite()) {
                        return Err(invalid("a stroke needs points and a radius over 0".into()));
                    }
                    let files = Arc::make_mut(&mut state.strokes);
                    let log = Arc::make_mut(files.entry(file.clone()).or_default());
                    let op = match rgb {
                        Some(rgb) => fab_paint_field::StrokeOp::Paint { slot: log.slot_for(fab_paint_field::StrokeSwatch { rgb: *rgb, filament: None }) },
                        None => *op,
                    };
                    log.push(op, *radius, *strength, points);
                }
                Change::MeasureRemove { index: measure_index } => {
                    if *measure_index >= state.project.measures.len() {
                        return Err(invalid(format!("no measure {measure_index}")));
                    }
                    state.project.measures.remove(*measure_index);
                }
            }
        }
        // A script placed at a point: its first object goes there.
        for (instance, at) in placed_at {
            let Some(sref) = state.project.script(&instance).cloned() else { continue };
            let Some(first) = self.compile_ref(&state, &sref, &[]).map_err(ApplyError::Build)?.objects.first().map(|o| o.name.clone()) else { continue };
            state.project.place_mut(&instance, &first).at = Some(at);
        }
        // The naming moment (`program::named_objects`): an object the project knows by its
        // builder and line (`box@6`) gets that name written into its code when a tool writes its
        // script, or when the script is written or its objects placed while the project places
        // one by it (a project saved before names were written gets them on its next write).
        let files: BTreeSet<&str> = state.project.scripts.iter().map(|r| r.file.as_str()).filter(|f| !program::is_paint_file(f)).collect();
        let mut named: Vec<(String, String)> = Vec::new();
        for file in files {
            let Some(source) = state.scripts.get(file) else { continue };
            let was = self.state.scripts.get(file);
            let instances: BTreeSet<&str> = state.project.scripts.iter().filter(|r| r.file == file).map(|r| r.name.as_str()).collect();
            let places = |p: &Project| p.places.iter().filter(|p| instances.contains(p.instance.as_str())).cloned().collect::<Vec<_>>();
            if was == Some(source) && places(&self.state.project) == places(&state.project) {
                continue;
            }
            // A script that does not build says so when the state compiles.
            let Ok(after) = self.script(file, source, None, &[], &state, &[]) else { continue };
            let before = match was {
                Some(was) if was != source => self.script(file, was, None, &[], &self.state, &[]).ok(),
                _ => None,
            };
            let tool = changes.iter().any(|c| matches!(c, Change::Script { file: f, edit } if f == file && !matches!(edit, Edit::Program { .. })));
            let placed = |name: &str| state.project.places.iter().any(|p| p.object == name && instances.contains(p.instance.as_str()));
            let base = before.as_ref().map_or(&after.program, |b| &b.program);
            match program::named_objects(base, &after.program, &|name| tool || placed(name)) {
                Ok(Some(text)) => named.push((file.to_string(), text)),
                Ok(None) => {}
                Err(message) => return Err(ApplyError::Invalid { index: changes.len() - 1, message: format!("naming the objects of {file}: {message}") }),
            }
        }
        state.scripts.extend(named);
        // A script edited so it no longer emits an object: that object's placement goes with it.
        let edited: Vec<&String> = changes.iter().filter_map(|c| match c { Change::Script { file, .. } => Some(file), _ => None }).collect();
        if !edited.is_empty() {
            let mut emitted: Vec<(String, Vec<String>)> = Vec::new();
            for sref in state.project.scripts.iter().filter(|r| edited.contains(&&r.file)) {
                let Some(source) = state.scripts.get(&sref.file) else { continue };
                if let Ok(s) = self.script(&sref.file, source, None, &[], &state, &[]) {
                    emitted.push((sref.name.clone(), s.objects.iter().map(|o| o.name.clone()).collect()));
                }
            }
            state.project.places.retain(|p| emitted.iter().find(|(i, _)| *i == p.instance).is_none_or(|(_, objects)| objects.contains(&p.object)));
            state.project.test_prints.retain(|t| emitted.iter().find(|(i, _)| *i == t.instance).is_none_or(|(_, objects)| objects.contains(&t.object)));
        }
        let c = self.compile_state(&state).map_err(ApplyError::Build)?;
        Ok((state, c))
    }

    /// The project after `changes`, evaluated but not committed: a live drag's or a typed
    /// text's preview. The document, its head and its history are untouched.
    ///
    /// A draft whose statement lost its selector (a B-rep step's edge or face names nothing on
    /// its body any more) builds that statement as its input, as if it were not there, and says
    /// so in [`Evaluated::lost`]: the view shows what is left to pick the edge or face on. Each
    /// statement is skipped once; a commit of the draft fails at it.
    pub fn preview(&self, eval: &mut Evaluator, changes: &[Change], cancelled: Option<&dyn Fn() -> bool>) -> Result<(State, Compiled, Evaluated), ApplyError> {
        let (state, mut c) = self.edited(changes)?;
        let mut skip: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut lost = Vec::new();
        loop {
            match eval.evaluate_compiled(c, state.project.arrange_gap, cancelled) {
                Ok((c, mut ev)) => {
                    ev.lost = lost;
                    return Ok((state, c, ev));
                }
                Err(f) => {
                    let skipped = skip.get(&f.file).is_some_and(|ids| ids.contains(&f.id));
                    let Some(index) = f.lost.filter(|_| !skipped && !f.id.is_empty()) else { return Err(ApplyError::Build(f)) };
                    let (_, kind, verb) = eval::lost_words(&f.op);
                    lost.push(eval::Lost { file: f.file.clone(), id: f.id.clone(), index, line: f.line, kind, hint: format!("{}: pick the {kind} it should {verb}", eval::lost_what(&f.id, &f.op)), message: f.message.clone() });
                    skip.entry(f.file.clone()).or_default().push(f.id.clone());
                    c = self.compile_skipping(&state, &skip).map_err(ApplyError::Build)?;
                }
            }
        }
    }

    /// Whether the project after `changes` builds no exact body, `head` being the project's build
    /// as it is: the engine holds every body its evaluation reads ([`eval::exact_keys`]), as for a
    /// value dragged back to a shape built before. No when it does not compile.
    pub fn bodies_built(&self, changes: &[Change], head: &Compiled) -> bool {
        let Ok((_, c)) = self.edited(changes) else { return false };
        eval::exact_keys(&c, head).is_some_and(|keys| makepad_csg_brep_model::engine().built(&keys))
    }

    fn fingerprint(expected: Head, changes: &[Change]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(&expected.generation.to_le_bytes());
        h.update(&expected.content);
        for c in changes {
            h.update(format!("{c:?}").as_bytes());
            h.update(&[0]);
        }
        h.finish()
    }

    /// Apply a batch: all of it or nothing. On success returns what it did and the
    /// evaluation of the new head (for the live view).
    pub fn apply(
        &mut self,
        eval: &mut Evaluator,
        request_id: &str,
        expected: Head,
        changes: &[Change],
        cancelled: Option<&dyn Fn() -> bool>,
    ) -> Result<(Applied, Option<(Compiled, Evaluated)>), ApplyError> {
        self.apply_gated(eval, request_id, expected, changes, cancelled, &|_| None)
    }

    /// [`Self::apply`], refused when `refuse` finds fault with the batch's build: an AI's edit that
    /// breaks a number the person gave (`m.expect`, [`expect::newly_failing`]). The refusal is a
    /// build error (`refuse`'s, at its line); nothing commits and the inspection tools keep the
    /// build before it.
    pub fn apply_gated(
        &mut self,
        eval: &mut Evaluator,
        request_id: &str,
        expected: Head,
        changes: &[Change],
        cancelled: Option<&dyn Fn() -> bool>,
        refuse: &dyn Fn(&Evaluated) -> Option<eval::Failure>,
    ) -> Result<(Applied, Option<(Compiled, Evaluated)>), ApplyError> {
        self.apply_gated_labeled(eval, request_id, expected, changes, cancelled, refuse, None)
    }

    /// Apply the same gated batch with a caller-supplied undo label.
    pub fn apply_gated_labeled(
        &mut self, eval: &mut Evaluator, request_id: &str, expected: Head, changes: &[Change],
        cancelled: Option<&dyn Fn() -> bool>, refuse: &dyn Fn(&Evaluated) -> Option<eval::Failure>, label: Option<String>,
    ) -> Result<(Applied, Option<(Compiled, Evaluated)>), ApplyError> {
        let fingerprint = Self::fingerprint(expected, changes);
        if let Some(r) = self.receipts.iter().find(|r| r.request_id == request_id) {
            if r.fingerprint != fingerprint {
                return Err(ApplyError::RequestIdReused);
            }
            let mut reply = r.reply.clone();
            reply.replayed = true;
            return Ok((reply, None));
        }
        if expected != self.head {
            return Err(ApplyError::StaleHead { current: self.head });
        }
        let parts = |c: &Compiled| -> Vec<String> { c.instances.iter().flat_map(|i| i.script.program.solids.iter().filter(|s| s.named).map(|s| program::part_id(&i.name, &s.id))).collect() };
        let before_compiled = self.compile().ok();
        let before: Vec<String> = before_compiled.as_ref().map(|c| parts(c)).unwrap_or_default();
        let (state, c) = self.edited(changes)?;
        let inspected = eval.inspected();
        let (c, ev) = eval.evaluate_compiled(c, state.project.arrange_gap, cancelled).map_err(ApplyError::Build)?;
        if let Some(f) = refuse(&ev) {
            eval.put_back(inspected);
            return Err(ApplyError::Build(f));
        }
        let added: Vec<String> = parts(&c).into_iter().filter(|s| !before.contains(s)).collect();
        let mut changed: Vec<String> = Vec::new();
        let mut note = |id: String| {
            if !changed.contains(&id) {
                changed.push(id);
            }
        };
        for ch in changes {
            match ch {
                Change::Script { file, edit } => {
                    let solid = match edit {
                        Edit::Field { solid, .. } | Edit::Unset { solid, .. } | Edit::Replace { solid, .. } | Edit::Operand { solid, .. } | Edit::Remove { solid } | Edit::RootArg { solid, .. } | Edit::Root { solid, .. } | Edit::After { solid, .. } => Some(solid.clone()),
                        Edit::Rename { to, .. } => Some(to.clone()),
                        Edit::Set { site, .. } => Some(site.split('.').next().unwrap_or(site).to_string()),
                        _ => None,
                    };
                    for i in c.instances.iter().filter(|i| i.script.file == *file) {
                        match &solid {
                            Some(s) => note(program::part_id(&i.name, s)),
                            None => note(i.name.clone()),
                        }
                    }
                }
                Change::Place { instance, object, .. } => note(program::part_id(instance, object)),
                Change::Param { instance, .. } => {
                    let (base, _) = split_link(instance);
                    for i in c.instances.iter().filter(|i| i.script_instance == base) {
                        note(i.name.clone());
                    }
                }
                Change::RemoveInstance { instance } => {
                    note(instance.clone());
                    let (base, _) = split_link(instance);
                    if let Some(before) = &before_compiled {
                        for i in before.instances.iter().filter(|i| i.script_instance == base && c.instance(&i.name).is_none()) {
                            note(i.name.clone());
                        }
                    }
                }
                Change::RemoveScript { instance } => {
                    let (base, _) = split_link(instance);
                    note(base.to_string());
                    if let Some(before) = &before_compiled {
                        for i in before.instances.iter().filter(|i| i.script_instance == base) {
                            note(i.name.clone());
                        }
                    }
                }
                Change::Clone { .. } | Change::Copy { .. } => {
                    for i in &c.instances {
                        if before_compiled.as_ref().is_none_or(|before| before.instance(&i.name).is_none()) {
                            note(i.name.clone());
                        }
                    }
                }
                Change::MeasureAdd { .. } => note("measures".into()),
                Change::MeasureSet { index, .. } | Change::MeasureRemove { index } => note(format!("measure/{index}")),
                Change::TestPrint { name, .. } => note(format!("test_print/{name}")),
                Change::Stroke { file, .. } => {
                    for i in c.instances.iter().filter(|i| program::strokes_files(i.script.source()).contains(file)) {
                        note(i.name.clone());
                    }
                }
                Change::RenameColor { .. } => {
                    for i in c.instances.iter().filter(|i| self.state.scripts.get(&i.script.file) != state.scripts.get(&i.script.file)) {
                        note(i.name.clone());
                    }
                }
                Change::AddInstance { .. } | Change::PlateSettings { .. } | Change::ArrangeGap(_) | Change::Color { .. } => {}
            }
        }
        // An edit that leaves the project as it was is no undo step.
        if state != self.state {
            self.commit(state, label.unwrap_or_else(|| label_of(changes)));
        }
        let reply = Applied { head: self.head, replayed: false, added, changed };
        self.receipts.push_back(Receipt { request_id: request_id.into(), fingerprint, reply: reply.clone() });
        while self.receipts.len() > MAX_RECEIPTS {
            self.receipts.pop_front();
        }
        Ok((reply, Some((c, ev))))
    }

    fn commit(&mut self, state: State, label: String) {
        let old = std::mem::replace(&mut self.state, state);
        let seq = self.steps.fetch_add(1, SeqCst) + 1;
        self.undo.push_back(Step { state: old, label: label.into(), seq });
        self.logged.fetch_max(seq, SeqCst);
        if self.undo.len() > MAX_HISTORY {
            self.undo.pop_front();
        }
        self.redo.clear();
        self.advance();
        if let (Some(sink), Some(step)) = (&self.journal, self.undo.back()) {
            sink.receive(JournalEvent::Commit { seq, label: &step.label }, &step.state, self);
        }
    }

    fn advance(&mut self) {
        self.head = Head { generation: self.head.generation + 1, content: content_hash(&self.state) };
        self.cache.get_mut().unwrap().turn();
    }

    /// Undo or redo one transaction (itself a new head).
    pub fn history(&mut self, eval: &mut Evaluator, expected: Head, action: HistoryAction, cancelled: Option<&dyn Fn() -> bool>) -> Result<(Head, Compiled, Evaluated), ApplyError> {
        if expected != self.head {
            return Err(ApplyError::StaleHead { current: self.head });
        }
        if self.logged.load(SeqCst) != self.redo_mark {
            // A later undo must not revive redo killed by a commit in the other history.
            self.redo.clear();
            if matches!(action, HistoryAction::Redo) {
                return Err(ApplyError::NoHistory);
            }
        }
        // What the state being left asks stays too, as an edit's does: going back and forth
        // compiles nothing.
        let _ = self.compile();
        let before = self.undo_top();
        let step = match action {
            HistoryAction::Undo => self.undo.pop_back(),
            HistoryAction::Redo => self.redo.pop(),
        }
        .ok_or(ApplyError::NoHistory)?;
        let built = self.compile_state(&step.state).and_then(|c| eval.evaluate_compiled(c, step.state.project.arrange_gap, cancelled));
        let (c, ev) = match built {
            Ok(x) => x,
            Err(f) => {
                match action {
                    HistoryAction::Undo => self.undo.push_back(step),
                    HistoryAction::Redo => self.redo.push(step),
                }
                return Err(ApplyError::Build(f));
            }
        };
        let label = step.label.clone();
        let seq = step.seq;
        let old = std::mem::replace(&mut self.state, step.state);
        match action {
            HistoryAction::Undo => {
                self.redo.push(Step { state: old, label, seq });
                self.redo_mark = self.logged.load(SeqCst);
            }
            HistoryAction::Redo => self.undo.push_back(Step { state: old, label, seq }),
        }
        self.advance();
        if let Some(sink) = &self.journal {
            let undo = matches!(action, HistoryAction::Undo);
            let previous = if undo { self.redo.last() } else { self.undo.back() };
            if let Some(step) = previous {
                sink.receive(JournalEvent::History { undo, before, after: self.undo_top() }, &step.state, self);
            }
        }
        Ok((self.head, c, ev))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod assembly_drop_tests;

mod assembly_rest;

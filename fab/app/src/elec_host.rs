//! Fab's electronics host: mechanics, the plate, the bed, item states and the live bench.
//! The shared engine loop is [`Driver`].
//! The mechanics bridge runs on the pool per committed design change (never the preview):
//! O(parts + drills + keepouts + port vertices), latest wins. The UI makes O(boards) hash compares
//! and one new script env when a board input moved, including additions and removals.
//! Item states: nothing per frame when nothing changed; per change O(texels lit before + after
//! + the items the findings and selection name), plus O(board items) while a net is emphasised.
//! One partial texture upload on the next draw.
//! Schematic states cost the same, with net scans over circuit items, plus O(units) pointer
//! compares per snapshot and O(circuit items) per committed circuit change on the strand.
//! Case checks: on the pool per plate rebuild, O(case triangles + bodies + port rays) per board;
//! on the UI, one message and one mailbox push per board.
//! Each shown snapshot hands every live board and its fills to the shared board drawing
//! (`BoardDraws`), which the board panes, the plate's board faces and the 3D view read: O(units)
//! `Arc` compares per change; an unchanged board costs nothing more.
//! Before this host starts, the model's scripts read the same boards through [`script_env`]:
//! registered when the model starts (the kinds' names), each board built at a script's first read.
//! The engine is the bed's: it loads the bed plate's electronics documents (an empty design, no
//! board, for a plate without any), a step that changed their text hands the plate the electronics
//! documents and the versions they may replace (Arc clones, then one queue push). The plate
//! keeps its other files. A bed status compares O(files x versions), bytes only when the Arcs
//! differ; electronics changed outside the engine restart it once its strand is idle. Another
//! document on the bed also drops the engine and loads that one's (O(units) releases on the UI,
//! the load on the strand). While the bed is changing, edits are not taken.
//! The live bench (SimHost) runs only while something wants a run (the transport or a shown scope; later a Bench
//! tab, the Traces dock, a check that needs a run or an AI ask). Before that the host has no
//! bench: no lowering, no solver thread, nothing per frame beyond one scan of each action batch
//! for wants (O(actions)). While wanted: one `frame` per draw (its rows' upload; solving runs on
//! its own thread, lowering on the pool, so the first frames show no values yet, never a wait).
//! When the last want goes, the bench pauses and the views' live rows clear.
//! A made run follows the chosen bench. A change of bench or its evaluation gives the host
//! its new scope, recording length and stimulus for the next lowering.

use fab::api::NoticeView;
use fab_elec_board::{Board, BoardOp, CarriedKeepout, Item, ItemKind, PortRoom, ZoneFill};
use fab_elec_core::{BoardId, CompRef, FeatureRef, ItemId, Sel, Xform3, P};
use fab_elec_design::{By, Design, DesignOp, FocusKind};
use fab_elec_route::GestureKind;
use fab_elec_host::{CircuitChange, Cmd, Docs, DocSet, Drained, Driver, Engine, Inbox, Outcome};
use fab_elec_lang::BenchView;
use fab_elec_mech::bridge::{board_inputs, CaseInputs, LazyBoards};
use fab_elec_mech::scene::{moved_poses, scene_of, ElecScene};
use fab_elec_mech::Pose;
use fab_elec_mech::{board_mech_with, case_checks, case_pipes, case_solids, CheckOpts};
use fab_elec_mech::derived::case_keepout;
use fab_elec_circuit::Violation;
use fab_elec_bench::SimCmd;
use fab_elec_simhost::SimHost;
use fab_elec_view::live::sim::SimAction;
use fab_elec_view::instruments::bench_setup::{BenchCheck, CheckBench};
use fab_elec_view::live::views::{changed_lines, EditOrigin, ElecEditor, ElecOutsideEdit};
use fab_elec_view::{board_rows, board_texels, part_rows, sheet_rows, sheet_texels, with_flash, DesignSnapshot, ElecLive, ItemStates, SheetIndex, StatesOf, ViewAction, ViewState};
use fab_elec_view::board::BoardDraws;
use fab_elec_view::board::tools::Tidying;
use fab_elec_view::board::view::{BoardEditDone, BoardEditPress, FillWaitNotice, StitchPress};
use fab_elec_view::req_list::{AddRefused, ReqAction};
use fab_model::eval::Evaluated;
use fab_model::program::ScriptEnv;
use makepad_widgets::makepad_platform::thread::{to_ui_oneshot, Class, Strand, Ticket as TaskTicket, TaskHandle, ToUIOneshotReceiver, ToUIReceiver};
use std::sync::mpsc::TryRecvError;
use makepad_widgets::{log, Actions, Cx, Event, WidgetActionCast};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};

/// Scope ids start at one; zero belongs only to the workspace transport.
const TRANSPORT_WANT: u64 = 0;

#[path = "elec_scope.rs"]
mod scope;
pub use scope::SCOPE_ACTIONS;

/// The demo design whose boards a script reads before the project's electronics documents load
/// (ELANG, [`script_env`]). The host never loads it: a plate without documents has no board.
pub fn design() -> Design {
    fab_elec_fixtures::ledctl()
}

/// The model's script env from its start: the enclosure library and the boards of [`design`],
/// each built when a script first reads it (on the model's worker). The host replaces it with
/// the boards of the design it edits once it has built them.
pub fn script_env() -> ScriptEnv {
    LazyBoards::env(Arc::new(LazyBoards::new(design)))
}

/// Each plate's script env, from its electronics documents (`fab_ai::Session::env_for`): the
/// boards its case scripts read are its own. One env per documents and assets (the same two
/// `Arc`s give the same env, so a plate switched back to shows its build again); a plate without documents
/// takes the shared one, [`script_env`]'s stock boards. While the electronics host edits the bed
/// plate, the model reads that plate's boards through the env the host hands over instead
/// (`ModelHost::set_env`). Making an env evaluates nothing: a script's first board read
/// evaluates the documents, on the model's worker.
type DocEnv = (Weak<fab_model::Docs>, Weak<fab_model::ElecAssets>, ScriptEnv);

fn doc_envs() -> &'static (ScriptEnv, std::sync::Mutex<Vec<DocEnv>>) {
    static ENVS: std::sync::OnceLock<(ScriptEnv, std::sync::Mutex<Vec<DocEnv>>)> = std::sync::OnceLock::new();
    ENVS.get_or_init(|| (script_env(), Default::default()))
}

/// The env of a plate without electronics documents ([`doc_env`]): one env for the process.
pub fn shared_env() -> ScriptEnv {
    doc_envs().0.clone()
}

/// The env a plate with `docs` and `assets` builds with: O(plates kept) for the pointer lookup,
/// plus O(documents + assets) map copies for a new env (text and bytes stay shared). The boards
/// build when a script first reads them. Documents that do not evaluate serve no board, none of
/// the stock catalogue's either: each read is refused, naming the document and why.
pub fn doc_env(docs: &Arc<fab_model::Docs>, assets: &Arc<fab_model::ElecAssets>) -> ScriptEnv {
    if docs.is_empty() {
        return shared_env();
    }
    let mut made = doc_envs().1.lock().unwrap_or_else(|e| e.into_inner());
    made.retain(|(of, data, _)| of.strong_count() > 0 && data.strong_count() > 0);
    if let Some((_, _, env)) = made.iter().find(|(of, data, _)| {
        std::ptr::eq(of.as_ptr(), Arc::as_ptr(docs)) && std::ptr::eq(data.as_ptr(), Arc::as_ptr(assets))
    }) {
        return env.clone();
    }
    let texts: fab_model::Docs = (**docs).clone();
    let bytes: fab_model::ElecAssets = (**assets).clone();
    let env = LazyBoards::env(Arc::new(LazyBoards::fallible(move || docs_design(&texts, &bytes, crate::part_models::library().as_deref()))));
    made.push((Arc::downgrade(docs), Arc::downgrade(assets), env.clone()));
    env
}

/// A plate's documents as a project, with the one system document the language's
/// `Sources::ensure_system` keeps: an empty one is reused (or renamed to `SYSTEM_DOC`), else one
/// is added; while it is empty it is no file (`Docs::texts`). The names that are no electronics
/// document's are the error, with why.
fn plate_sources(texts: &fab_model::Docs) -> (fab_elec_lang::Sources, Vec<(String, String)>) {
    let mut sources = fab_elec_lang::Sources::new();
    let mut refused = Vec::new();
    for (name, text) in texts {
        if let Err(diag) = sources.add(name.clone(), text.clone()) {
            refused.push((name.clone(), diag.message));
        }
    }
    sources.ensure_system().expect("a project without a system document takes SYSTEM_DOC");
    (sources, refused)
}

/// Registers the plate's asset names with `sources`, so a part whose body mesh names one
/// evaluates. O(assets) name clones; no bytes are read.
fn add_asset_names(sources: &mut fab_elec_lang::Sources, assets: &fab_model::ElecAssets) {
    for name in assets.keys() {
        sources.add_asset(name.clone());
    }
}

/// The process mesh cache's key of each of the plate's assets, by name ([`Design::resolve_meshes`]):
/// every asset hashed (O(bytes), about 2 ms per 0.7 MB) and put into the cache, where each
/// distinct asset parses once. Invalid meshes log once per key and keep the plan box.
fn asset_keys(assets: &fab_model::ElecAssets) -> BTreeMap<String, String> {
    let mut keys = BTreeMap::new();
    for (name, bytes) in assets {
        let hash = makepad_core_util::sha256::sha256_hex(bytes);
        let key = format!("asset:{hash}/{name}");
        // A failed put keeps its key too: the cache holds the failure, so the readers fall back
        // to the plan box and a project-relative name never reaches the working directory.
        keys.insert(name.clone(), key.clone());
        if let Err(issue) = fab_elec_import::cache::MeshCache::shared().put(&key, bytes) {
            static WARNED: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> = std::sync::OnceLock::new();
            let first = WARNED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).insert(key);
            if first {
                log!("fab: the plate's electronics asset {name} is not a body mesh: {issue}; using its plan box");
            }
        }
    }
    keys
}

/// The models the person's part library holds that a plate's documents may name without the
/// plate holding them: each by its project path (`parts/lcsc/C2040.glb`) with the key its bodies
/// resolve to, the library file's path, which the process mesh cache reads when a body is first
/// drawn or checked. The plate's own files come first, so their names are left out.
/// Cost: [`fab_elec_import::library::Library::meshes`], one directory read and a stat per entry, on
/// the caller's worker; no model is read here.
fn library_meshes(part_library: Option<&std::path::Path>, assets: &fab_model::ElecAssets) -> BTreeMap<String, String> {
    use fab_elec_import::library::Source;
    let Some(root) = part_library else { return BTreeMap::new() };
    // EasyEDA off: its models are not used, though the library keeps them (3D model sources).
    if !crate::part_models::source_on(fab::model_sources::ModelSource::EasyEda) {
        return BTreeMap::new();
    }
    let source = fab_elec_import::easyeda::Lcsc;
    let meshes: BTreeMap<String, String> = fab_elec_import::library::Library::new(root).meshes(&source).into_iter().filter_map(|(number, mesh)| {
        let name = format!("parts/{}/{number}.glb", source.folder());
        if assets.contains_key(&name) {
            return None;
        }
        Some((name, mesh.to_str()?.to_string()))
    }).collect();
    // Where each came from, for the part cards (one lock; notes already kept change nothing).
    fab_elec_parts::model_board::note_all(meshes.iter()
        .map(|(name, key)| (crate::part_models::easyeda_note(name), Some(key.clone()))).collect());
    meshes
}

/// The design a plate's electronics documents and assets hold, or the first document problem. A
/// body's mesh is the plate's file, else the part library's model ([`library_meshes`]), else
/// the placeholder. All assets are registered and hashed on this worker, once per env evaluation
/// ([`asset_keys`]).
pub(crate) fn docs_design(texts: &fab_model::Docs, assets: &fab_model::ElecAssets, part_library: Option<&std::path::Path>) -> Result<Design, String> {
    let (mut sources, refused) = plate_sources(texts);
    if let Some((name, message)) = refused.first() {
        let why = format!("the plate's electronics document {name} is not one: {message}");
        log!("fab: {why}");
        return Err(why);
    }
    add_asset_names(&mut sources, assets);
    let library = library_meshes(part_library, assets);
    sources.set_library(library.keys().cloned().collect());
    fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).map(|mut evaluated| {
        if !assets.is_empty() {
            evaluated.design.resolve_meshes(&asset_keys(assets));
        }
        evaluated.design.resolve_meshes(&library);
        evaluated.design
    }).map_err(|diags| {
        let first = diags.first();
        let at = first.and_then(|d| d.sel.iter().find_map(|s| match s {
            fab_elec_core::Sel::Source { doc, start, .. } => {
                let name = &sources.doc(*doc)?.name;
                Some(match sources.line_col(*doc, *start) {
                    Some((line, col)) => format!("{name}:{line}:{col}"),
                    None => name.clone(),
                })
            }
            _ => None,
        }));
        let message = first.map_or("no design", |d| d.message.as_str());
        let more = if diags.len() > 1 { format!(" ({} problems)", diags.len()) } else { String::new() };
        let why = match at {
            Some(at) => format!("the plate's electronics document {at} does not evaluate: {message}{more}"),
            None => format!("the plate's electronics documents do not evaluate: {message}{more}"),
        };
        log!("fab: {why}");
        why
    })
}

/// Answer the AI from the same plate documents and assets its case scripts read.
/// Each call builds the design from the documents, then costs O(parts + ports + holes + outline
/// points) for the board summary. The app runs this on the pool; there is no work per frame.
pub fn board_answer(docs: &fab_model::Docs, assets: &fab_model::ElecAssets, board: Option<&str>, only: Option<&str>) -> Result<String, String> {
    if docs.is_empty() {
        return Err("This plate has no board: it has no electronics documents.".into());
    }
    let design = docs_design(docs, assets, crate::part_models::library().as_deref())?;
    fab_elec_mech::read::board_read(&design, board, only)
}

/// `fab.align`: the plate's board fitted onto a printed part's bosses, hole by hole, with the
/// edits that write its mount ([`fab_elec_mech::align_answer`]). Read only.
pub fn align_answer(docs: &fab_model::Docs, assets: &fab_model::ElecAssets, ev: &fab_model::eval::Evaluated,
    script_of: &dyn Fn(&str) -> Option<(String, String)>, board: Option<&str>, part: Option<&str>) -> Result<String, String> {
    if docs.is_empty() {
        return Err("This plate has no board: it has no electronics documents.".into());
    }
    let design = docs_design(docs, assets, crate::part_models::library().as_deref())?;
    fab_elec_mech::align_answer::align_answer(&design, ev, script_of, board, part)
}

/// The bed's electronics documents (names `DocKind::of_name` knows), the part the engine owns.
/// Cost: O(files), Arc clones.
pub(crate) fn electronics(docs: &fab_model::Docs) -> fab_model::Docs {
    docs.iter().filter(|(name, _)| fab_elec_lang::DocKind::of_name(name).is_some())
        .map(|(name, text)| (name.clone(), text.clone())).collect()
}

/// The editors whose documents differ between `before` and `now` (a name in either), each once.
/// Cost: O(files) compares; equal `Arc`s compare by pointer.
fn edited_editors(before: &fab_model::Docs, now: &fab_model::Docs) -> Vec<ElecEditor> {
    let mut editors = Vec::new();
    for name in now.keys().chain(before.keys()) {
        if now.get(name) != before.get(name) {
            if let Some(editor) = ElecEditor::of_doc(name).filter(|e| !editors.contains(e)) {
                editors.push(editor);
            }
        }
    }
    editors
}

/// An edit's label when its design shows no change (text no view draws, a comment): the editors.
fn edited_words(editors: &[ElecEditor]) -> String {
    let names: Vec<&str> = editors.iter().map(|e| match e {
        ElecEditor::Schematic => "the schematic's text",
        ElecEditor::Board => "the board's text",
        ElecEditor::Firmware => "the firmware",
        ElecEditor::Bench => "the bench",
    }).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Each firmware document of `now` whose text differs from `before`'s, with its changed lines.
/// Cost: O(lines) of the changed firmware documents.
fn firmware_lines(before: &fab_model::Docs, now: &fab_model::Docs) -> Vec<(String, std::ops::Range<usize>)> {
    now.iter().filter(|(name, _)| fab_elec_lang::DocKind::of_name(name) == Some(fab_elec_lang::DocKind::Firmware))
        .filter(|(name, text)| before.get(*name) != Some(*text))
        .filter_map(|(name, text)| Some((name.clone(), changed_lines(before.get(name).map_or("", |t| t), text)?)))
        .collect()
}

/// The same files with the same text: equal names, and per name the same `Arc` or equal bytes.
/// Cost: O(files) pointer compares; bytes only where the pointers differ.
pub(crate) fn same_files(a: &fab_model::Docs, b: &fab_model::Docs) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|((a_name, a_text), (b_name, b_text))| {
        a_name == b_name && (Arc::ptr_eq(a_text, b_text) || a_text == b_text)
    })
}

/// The bed's files after the engine's write. Documents: the bed's own non-electronics files kept,
/// its electronics documents replaced by `docs`. Assets, three ways against the version the write
/// lands over: each asset the write lists (the bed's bytes, else the write's), each asset another
/// writer added since that version (the engine registers it later), and not one the version
/// listed and the write does not (an undone import's mesh). None when the bed holds none of
/// `bases` (its electronics documents changed, or an asset a version listed went, after the engine
/// last saw them): the write must not land. The assets are None when their names do not change.
/// Cost: O(files x bases + assets x bases), a handful of each; byte Arcs are shared, never copied.
pub(crate) fn merge_files(
    current: &fab_model::Docs, assets: &fab_model::ElecAssets, bases: &[Version], docs: &fab_model::Docs,
    written: &BTreeMap<String, Option<Arc<[u8]>>>,
) -> Option<(fab_model::Docs, Option<fab_model::ElecAssets>)> {
    let now = electronics(current);
    let base = bases.iter().find(|base| same_files(&now, &base.docs) && base.assets.iter().all(|name| assets.contains_key(name)))?;
    let mut all: fab_model::Docs = current.iter().filter(|(name, _)| fab_elec_lang::DocKind::of_name(name).is_none())
        .map(|(name, text)| (name.clone(), text.clone())).collect();
    all.extend(docs.iter().map(|(name, text)| (name.clone(), text.clone())));
    let mut next = fab_model::ElecAssets::new();
    for (name, bytes) in written {
        match assets.get(name).or(bytes.as_ref()) {
            Some(bytes) => {
                next.insert(name.clone(), bytes.clone());
            }
            None => log!("electronics: the engine lists {name}, but neither the plate nor the engine holds its bytes"),
        }
    }
    for (name, bytes) in assets {
        if !written.contains_key(name) && base.assets.binary_search(name).is_err() {
            next.insert(name.clone(), bytes.clone());
        }
    }
    let changed = !next.keys().eq(assets.keys());
    Some((all, changed.then_some(next)))
}

/// A version of the bed's electronics files as the engine loaded or wrote them: its documents and
/// its asset names, in name order.
#[derive(Clone, Debug)]
pub struct Version {
    pub docs: Arc<fab_model::Docs>,
    pub assets: Arc<[String]>,
}

/// The electronics write and the versions it may land over; cloned Arcs, no text or byte copies.
pub struct DocsWrite {
    pub docs: Arc<fab_model::Docs>,
    /// Every asset the engine lists, with the bytes of those its history added (an import, its
    /// redo): the history's own allocations.
    pub assets: Arc<BTreeMap<String, Option<Arc<[u8]>>>>,
    pub bases: Vec<Version>,
}

/// The engine's design and documents for a bed's electronics documents: an empty design and no
/// documents when there are none, so a plate without electronics shows no board (user, 11-19:
/// it showed the stock controller's), else the plate's own, as they read. Documents that do not
/// evaluate load as an empty design with their notes, never as the stock ones, so a first edit
/// cannot write stock text over the person's board. A file whose name is no electronics document is not
/// loaded; the bed keeps its other files because the model host's write replaces only electronics
/// documents.
/// The plate's `assets` are registered with the documents, and the design's body meshes read
/// them from the process mesh cache, as the plate's env does ([`docs_design`]); a mesh the plate
/// has no file of is the part library's model when `part_library` holds it ([`library_meshes`]),
/// on the first design, with no step to undo. The returned keys (the plate's, then the library's)
/// let the engine resolve re-evaluated units without hashing the assets again.
/// Cost: one evaluation of the documents, one hash of each asset ([`asset_keys`]) and one listing
/// of the part library, on the strand.
pub(super) fn plate_project(docs: &fab_model::Docs, assets: &fab_model::ElecAssets, part_library: Option<&std::path::Path>)
    -> (Design, Option<Docs>, BTreeMap<String, String>, BTreeMap<String, String>)
{
    if docs.is_empty() {
        return (Design::default(), None, BTreeMap::new(), BTreeMap::new());
    }
    let (mut sources, refused) = plate_sources(docs);
    for (name, message) in refused {
        log!("electronics: the plate's file {name} is kept as it is and not loaded: {message}");
    }
    add_asset_names(&mut sources, assets);
    let library = library_meshes(part_library, assets);
    sources.set_library(library.keys().cloned().collect());
    let (mut design, docs) = Docs::load(sources, fab_elec_lang::std_libs());
    let keys = asset_keys(assets);
    design.resolve_meshes(&keys);
    design.resolve_meshes(&library);
    (design, Some(docs), keys, library)
}

/// Fab's own messages on the Driver's channel, ordered with the engine's.
enum FromEngine {
    /// The loaded design's boards, sent before the engines run, so the board pane draws them
    /// at once (without fills); the first snapshot follows with the engines' results.
    Loaded { boards: Vec<Arc<Board>> },
    Case(CaseUpdate),
    FillWait(FillWaitNotice),
    /// The plate scene of the live design asked for as `seq` (the loaded design's is 0).
    /// `committed`: built from a design with no preview in it.
    Scene { seq: u64, scene: Arc<ElecScene>, committed: bool },
}

/// Per board unit of `design`: whether the board has case parts in `ev` (the requirements'
/// `case(u).fits` says "no case" from it, not "fits"), and the case checks of that board against
/// them as Checks rows. A unit with no case gets an empty list, which clears its old findings.
fn case_results(ev: &Evaluated, design: &Design) -> Vec<(BoardId, u64, bool, Vec<Violation>)> {
    design.units.iter().filter_map(|unit| {
        let board = unit.board.as_deref()?;
        let solids = case_solids(ev, &unit.name);
        let findings = if solids.is_empty() {
            Vec::new()
        } else {
            let opts = CheckOpts { pipes: case_pipes(ev, &unit.name), ..CheckOpts::default() };
            case_checks(&board_mech_with(board, &unit.circuit, &board.drills()), &solids, &opts)
                .iter().map(|finding| finding.violation(board)).collect()
        };
        Some((unit.id, board.generation, !solids.is_empty(), findings))
    }).collect()
}

struct CaseKeepouts {
    unit: BoardId,
    generation: u64,
    sets: Vec<(String, Vec<Item>)>,
    rooms: Vec<PortRoom>,
}

/// Fixed records share a set by name. A resolved port key has no slash. Escape leading
/// @ in fixed names so even a fixed name spelling a port's whole key remains distinct.
fn case_set(name: &str, port: Option<&str>) -> String {
    match port {
        Some(key) => format!("@{key}/{name}"),
        None if name.starts_with('@') => format!("@{name}"),
        None => name.into(),
    }
}

/// Build a complete publication per board across printed and excluded parts.
fn case_keepouts(ev: &Evaluated, design: &Design) -> Vec<CaseKeepouts> {
    let records: Vec<&fab_plate::Keepout> = ev.plate.parts.iter().chain(ev.plate.excluded.iter().map(|x| &x.part))
        .flat_map(|part| &part.keepouts).collect();
    design.units.iter().filter_map(|unit| {
        let board = unit.board.as_ref()?;
        Some(board_keepouts(&unit.name, board, records.iter().copied()))
    }).collect()
}

fn board_keepouts<'a>(name: &str, board: &Board, records: impl IntoIterator<Item = &'a fab_plate::Keepout>) -> CaseKeepouts {
    let placed: HashMap<_, _> = board.placed.iter().map(|p| (p.comp, p)).collect();
    let mut sets: BTreeMap<String, Vec<Item>> = BTreeMap::new();
    let mut ports = BTreeMap::new();
    let mut missing = HashSet::new();
    for record in records.into_iter().filter(|record| record.board == name) {
        if record.room && record.feature.is_none() { continue }
        let side = match record.side {
            fab_plate::KeepoutSide::Top => fab_elec_core::Side::Top,
            fab_plate::KeepoutSide::Bottom => fab_elec_core::Side::Bottom,
        };
        let item = case_keepout(&record.name, &record.area, side, record.z);
        let port = record.feature.as_deref().and_then(|key| {
            let port = match fab_elec_mech::keys::feature_of_key(key) {
                Some(FeatureRef::Port(c, index)) if c.board == board.unit => placed.get(&c.comp).map(|&part| (part, index, key)),
                _ => None,
            };
            if port.is_none() && missing.insert((&record.name, key)) {
                // The case was built round a part the board no longer places there: no room
                // to carry, but the solid it printed still stands in the board's space.
                log!("electronics: case record '{}' on '{}' names no placed port '{}': no room is carried for it", record.name, name, key);
            }
            port
        });
        let key = port.map(|(_, _, key)| key);
        let port = port.map(|(part, index, _)| &mut ports.entry((part.comp, index))
            .or_insert_with(|| (part, PortRoom { comp: part.comp, room: None, keepout: None })).1);
        let Item::Keepout(k) = &item else { unreachable!() };
        if record.room {
            if let Some(port) = port { port.room = Some(k.area.clone()); }
            continue;
        }
        let name = case_set(&record.name, key);
        if let Some(port) = port {
            let from = FeatureRef::Panel(format!("{}{name}", fab_elec_host::CASE_KEEPOUT));
            port.keepout = k.z.map(|band| CarriedKeepout { from, area: k.area.clone(), band, side });
        }
        let set = sets.entry(name).or_default();
        if !set.contains(&item) { set.push(item); }
    }
    let rooms = ports.into_values().map(|(p, port)| port.relative_to(p.at)).collect();
    CaseKeepouts { unit: board.unit, generation: board.generation, sets: sets.into_iter().collect(), rooms }
}

/// A case library's own rule, asked about a finished arrangement of a board: the library
/// function a case left as a [`fab_model::eval::LibraryCall`] (the generator's light-pipe plan),
/// loaded into a [`ModuleIsolate`] and called with the arguments the case called it with and the
/// ports of the arrangement. Built on the thread that asks (never the UI thread), once for a job,
/// and dropped with it. Nothing in the route crates knows of it: they get rooms in and give poses out.
pub(crate) struct CasePlan {
    isolate: makepad_model_script::lang::ModuleIsolate,
    call: fab_model::eval::LibraryCall,
}

/// What the rule says of an arrangement: the case's own records for it, in the plate's form, and
/// the groups it would refuse, in the generator's words.
pub(crate) struct PlanAnswer {
    /// Every room and block record, as the case would publish them (kernel units, the board's frame).
    pub keepouts: Vec<fab_plate::Keepout>,
    pub refused: Vec<String>,
    refused_pipes: u32,
    made_pipes: u32,
}

impl CasePlan {
    /// Load the libraries `call` was made from. O(library size), once per job.
    pub(crate) fn open(libraries: &[makepad_model_script::lang::Library], call: &fab_model::eval::LibraryCall) -> Result<Self, Vec<makepad_model_script::lang::Diag>> {
        Ok(CasePlan { isolate: makepad_model_script::lang::ModuleIsolate::load(libraries)?, call: call.clone() })
    }

    /// The ports of a board as the case reads them, from the text `board("name")` gives it
    /// ([`fab_elec_mech::BoardMech::splash`]), the one place ports turn into script values.
    pub(crate) fn ports_of(&mut self, board_text: &str) -> Result<makepad_model_script::lang::V, Vec<makepad_model_script::lang::Diag>> {
        use makepad_model_script::lang::{Diag, V};
        match self.isolate.value("board", board_text)? {
            V::Obj(o) => o.get("ports").cloned().ok_or_else(|| vec![Diag { line: 1, col: 1, message: "the board has no ports".into() }]),
            _ => Err(vec![Diag { line: 1, col: 1, message: "the board is a record".into() }]),
        }
    }

    /// The rule's answer for `ports`. O(ports^2) in the script's merge loop, no solids.
    pub(crate) fn plan(&mut self, ports: makepad_model_script::lang::V) -> Result<PlanAnswer, Vec<makepad_model_script::lang::Diag>> {
        use makepad_model_script::lang::{read_keepouts, Diag, V};
        let mut args = self.call.args.clone();
        args.retain(|(k, _)| k != "ports");
        args.push(("ports".into(), ports));
        let record = V::Obj(Arc::new(makepad_model_script::lang::Obj { ident: 0, part: None, proto: None, fields: args, items: Vec::new(), children: Vec::new(), loc: None }));
        let V::Obj(plan) = self.isolate.call_named(&self.call.function, &record)? else {
            return Err(vec![Diag { line: 1, col: 1, message: format!("{} gave no record", self.call.function) }]);
        };
        let keepouts = match plan.get("keepouts") {
            Some(list) => read_keepouts(list, fab_model::program::UNIT).map_err(|d| vec![d])?,
            None => return Err(vec![Diag { line: 1, col: 1, message: format!("{} returned no keepouts", self.call.function) }]),
        };
        let bad = |field: &str| {
            let (line, col) = plan.loc.unwrap_or((1, 1));
            vec![Diag { line, col, message: format!("{} returned invalid {field}", self.call.function) }]
        };
        let Some(V::Arr(reasons)) = plan.get("refused") else { return Err(bad("refused")) };
        let refused = reasons.iter().map(|v| match v { V::Str(s) => Ok(s.clone()), _ => Err(bad("refused")) }).collect::<Result<_, _>>()?;
        let Some(V::Arr(groups)) = plan.get("groups") else { return Err(bad("groups")) };
        let (mut made_pipes, mut refused_pipes) = (0, 0);
        for g in groups {
            let g = g.obj().ok_or_else(|| bad("group"))?;
            let (Some(V::Arr(chans)), Some(V::Bool(refused))) = (g.get("chans"), g.get("refused")) else { return Err(bad("group")) };
            let count = if *refused { &mut refused_pipes } else { &mut made_pipes };
            *count += chans.len() as u32;
        }
        Ok(PlanAnswer { keepouts: fab_model::eval::plate_keepouts(&keepouts), refused, refused_pipes, made_pipes })
    }
}

struct PublishedPlan {
    libraries: Arc<Vec<makepad_model_script::lang::Library>>,
    call: fab_model::eval::LibraryCall,
}

/// Host calls carry plain values. Named fields compare by name and list items
/// by position, independently of the VM allocations and source locations.
fn same_args(a: &[(String, makepad_model_script::lang::V)], b: &[(String, makepad_model_script::lang::V)]) -> bool {
    use makepad_model_script::lang::V;
    fn same(a: &V, b: &V) -> bool {
        match (a, b) {
            (V::Nil, V::Nil) => true,
            (V::Bool(a), V::Bool(b)) => a == b,
            (V::Num(a), V::Num(b)) => a == b,
            (V::Str(a), V::Str(b)) | (V::Id(a), V::Id(b)) => a == b,
            (V::Color(a), V::Color(b)) => a == b,
            (V::Arr(a), V::Arr(b)) => list(a, b),
            (V::Obj(a), V::Obj(b)) => a.proto == b.proto && same_args(&a.fields, &b.fields) && list(&a.items, &b.items),
            _ => false,
        }
    }
    fn list(a: &[V], b: &[V]) -> bool { a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b)) }
    a.len() == b.len() && a.iter().all(|(k, v)| b.iter().find(|(key, _)| key == k).is_some_and(|(_, w)| same(v, w)))
}

impl PublishedPlan {
    fn for_board(name: &str, calls: &[fab_model::eval::LibraryCall], libraries: &Arc<Vec<makepad_model_script::lang::Library>>)
        -> Result<Option<Arc<dyn fab_elec_route::PlacePlan>>, String>
    {
        let mut calls = calls.iter().filter(|c| c.board == name);
        let Some(first) = calls.next() else { return Ok(None) };
        if !calls.all(|c| c.function == first.function && same_args(&c.args, &first.args)) {
            return Err(format!("{name}: the case published different placement plans for this board"));
        }
        Ok(Some(Arc::new(Self { call: first.clone(), libraries: libraries.clone() })))
    }
}

fn plan_error(diags: Vec<makepad_model_script::lang::Diag>) -> String {
    diags.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
}

impl fab_elec_route::PlacePlan for PublishedPlan {
    fn open(&self) -> Result<Box<dyn fab_elec_route::PlacePlanner>, String> {
        Ok(Box::new(CasePlan::open(&self.libraries, &self.call).map_err(plan_error)?))
    }
}

impl fab_elec_route::PlacePlanner for CasePlan {
    fn plan(&mut self, board: &Board, circuit: &fab_elec_circuit::Circuit) -> Result<fab_elec_route::PlanVerdict, String> {
        let text = board_mech_with(board, circuit, &board.drills()).splash();
        let ports = self.ports_of(&text).map_err(plan_error)?;
        let answer = self.plan(ports).map_err(plan_error)?;
        let rooms = board_keepouts(&self.call.board, board, &answer.keepouts).rooms;
        Ok(fab_elec_route::PlanVerdict { rooms, refused: answer.refused, refused_pipes: answer.refused_pipes, made_pipes: answer.made_pipes })
    }
}

/// One frozen model environment can cover several design generations. The strand
/// owns both the evaluated plate and the latest design with those board inputs.
#[derive(Default)]
pub(super) struct CaseBridge {
    inputs: CaseInputs,
    names: HashSet<String>,
    read: Option<CaseRead>,
    run: u64,
}

type CaseReply = Result<Arc<Evaluated>, String>;
type InputId = Weak<makepad_model_script::lang::Inputs>;

struct CaseRead {
    libraries: Arc<Vec<makepad_model_script::lang::Library>>,
    sent: u64,
    inputs: InputId,
    design: Arc<Design>,
    reply: Option<CaseReply>,
}

pub(super) enum CaseUpdate {
    Set { generation: u64, env: ScriptEnv },
    /// `failed`: the evaluator's reason when this publication is of a failed evaluation.
    Checked { run: u64, commands: Vec<Cmd>, failed: Option<String> },
}

impl CaseBridge {
    pub(super) fn cover(&mut self, design: Arc<Design>) -> Option<CaseUpdate> {
        let mut changed = false;
        let mut names = HashSet::new();
        for unit in &design.units {
            if let Some(board) = &unit.board {
                names.insert(unit.name.clone());
                changed |= self.inputs.update(board_inputs(&unit.name, board, &unit.circuit, &board.drills()));
            }
        }
        for gone in self.names.difference(&names) {
            changed |= self.inputs.remove(gone);
        }
        self.names = names;
        if let Some(read) = self.read.as_mut().filter(|_| !changed) {
            read.design = design;
            return self.publish();
        }
        let env = self.inputs.env();
        let generation = design.generation;
        self.read = Some(CaseRead { libraries: env.libraries.clone(), sent: generation, inputs: Arc::downgrade(&env.inputs), design, reply: None });
        Some(CaseUpdate::Set { generation, env })
    }

    pub(super) fn reply(&mut self, inputs: &InputId, sent: Option<u64>, reply: CaseReply) -> Option<CaseUpdate> {
        let read = self.read.as_mut()?;
        if !inputs.ptr_eq(&read.inputs) || sent.is_some_and(|stamp| stamp != read.sent) { return None }
        read.reply = Some(reply);
        self.publish()
    }

    fn publish(&mut self) -> Option<CaseUpdate> {
        let read = self.read.as_ref()?;
        let reply = read.reply.as_ref()?;
        let design = &read.design;
        let stamp = design.generation;
        self.run += 1;
        let run = self.run;
        let mut commands = Vec::new();
        let failed = reply.as_ref().err().cloned();
        match reply {
            Ok(ev) => {
                for CaseKeepouts { unit, generation, sets, rooms } in case_keepouts(ev, design) {
                    let name = &design.unit(unit).expect("a case publication names a board").name;
                    let plan = PublishedPlan::for_board(name, &ev.calls, &read.libraries);
                    commands.push(Cmd::CaseKeepouts { unit, generation, sets, rooms, plan });
                }
                for (unit, generation, cased, findings) in case_results(ev, design) {
                    commands.push(Cmd::CaseFindings { unit, generation, run, cased, findings });
                    commands.push(Cmd::CaseOutcome { unit, stamp, reason: (!cased).then(|| "the plate has no case solids for this board".into()) });
                }
            }
            Err(reason) => {
                for unit in design.units.iter().filter(|unit| unit.board.is_some()) {
                    commands.push(Cmd::CaseOutcome { unit: unit.id, stamp, reason: Some(reason.clone()) });
                }
            }
        }
        Some(CaseUpdate::Checked { run, commands, failed })
    }
}

/// The size (mm) of the footprint in the plate's XY of the boards `loose` keeps (those no case
/// part on the plate holds): the union of their outlines under their poses, at the view's
/// meshing tolerance, so it matches the drawn boards. O(outline vertices). None without such a
/// board.
pub fn scene_footprint(scene: &ElecScene, loose: impl Fn(&str) -> bool) -> Option<[f64; 2]> {
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for board in scene.boards.iter().filter(|board| loose(&board.name)) {
        for p in board.outline.iter().flat_map(|c| c.to_ring(2_000)) {
            let q = board.pose.apply_f([p.x as f64, p.y as f64, 0.0]);
            for k in 0..2 {
                lo[k] = lo[k].min(q[k] * 1e-6);
                hi[k] = hi[k].max(q[k] * 1e-6);
            }
        }
    }
    (lo[0] <= hi[0]).then(|| [hi[0] - lo[0], hi[1] - lo[1]])
}

use fab_elec_bom::{import::Draft as PriceCsv, prices::Library as PriceBook};
use fab_elec_host::bom_actions::{self as price_actions, PriceProposal, PriceRequest, PriceWrite};

enum LibraryJob {
    Load,
    Save(PriceWrite),
    Preview(String),
    Import(Arc<PriceCsv>, Vec<String>),
    Settings(fab_elec_bom::prices::Settings),
}
enum LibraryOutput {
    Snapshot(Arc<PriceBook>),
    Preview(Arc<PriceCsv>),
}

fn price_date() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    fab_elec_import::kicad3d::date(now.as_secs())
}

pub struct ElecHost {
    journal: Option<crate::journal::Electronics>,
    journal_calls: HashMap<u64, Arc<crate::journal::Binding>>,
    next_journal_call: u64,
    driver: Driver<FromEngine>,
    /// When the host started (`Cx::monotonic_now`), for the first snapshot's and scene's timing.
    started: f64,
    /// The first scene for the plate was logged.
    scene_timed: bool,
    /// The live bench of the committed design (its threads, lowering and row textures), made
    /// when something first wants a run.
    sim: Option<SimHost>,
    /// Compiles committed firmware on the pool and swaps only the newest good image.
    firmware: crate::elec_fw::FwRunner,
    /// The last bench list and choice handed to the run.
    sim_benches: Option<(Arc<Vec<BenchView>>, Option<Arc<str>>)>,
    /// While the chosen bench has failed, whether to play when it builds or another is picked.
    resume_after_failure: Option<bool>,
    /// A design without a usable bench is reported once until the host restarts.
    sim_no_bench_logged: bool,
    /// What wants a run now, by id (shown scopes, heard nets and TRANSPORT_WANT). Empty: stopped
    /// or not made.
    sim_wants: HashSet<u64>,
    /// The probes each view asked (`SimAction::Probes`), by id: a bench made later gets them too.
    sim_probes: HashMap<u64, Vec<usize>>,
    /// Whether the first want plays the bench: a new bench or another plate's, not one Stop ended.
    sim_resume: bool,
    view: ViewState,
    /// The last selection in this action batch was a full view state. Earlier single-focus
    /// actions reach `act` later in the same event, after `event` has published the group.
    group_selected_in_batch: bool,
    states: HashMap<BoardId, ItemStates>,
    states_from: HashMap<BoardId, (Arc<Board>, Arc<Vec<Violation>>)>,
    sheet_states: HashMap<BoardId, ItemStates>,
    sheet_states_from: HashMap<BoardId, (Arc<SheetIndex>, Arc<Vec<Violation>>)>,
    view_moved: bool,
    case: Strand<CaseBridge>,
    case_seen: u64,
    case_task: Option<TaskHandle<()>>,
    bom_task: Option<TaskHandle<()>>,
    bom_rx: ToUIReceiver<Arc<fab_elec_view::bom::Shown>>,
    bom_design: Option<Arc<Design>>,
    bom_group: bool,
    bom_quantity: i64,
    price_book: Option<Result<Arc<PriceBook>, String>>,
    price_jobs: VecDeque<(Delivery, LibraryJob)>,
    price_running: Option<(Delivery, TaskHandle<()>)>,
    price_rx: ToUIReceiver<Result<LibraryOutput, String>>,
    price_proposals: VecDeque<(Recipient, PriceProposal)>,
    price_review_changed: bool,
    rates: Option<Result<Arc<fab_elec_bom::Rates>, String>>,
    rates_task: Option<TaskHandle<()>>,
    rates_rx: ToUIReceiver<Result<Arc<fab_elec_bom::Rates>, String>>,
    /// The requirements' judging on the pool (`judge_requirements`): latest wins.
    req_task: Option<TaskHandle<()>>,
    /// Every document starts at the load's initial revision. Adding its presence advances it
    /// once, even if an undo has already taken it back out by the time the caller reads its reply.
    doc_load_rev: u64,
    /// Replaced with each judging, so an older report never lands after a newer one.
    req_rx: ToUIReceiver<ReqJudged>,
    /// The newest judging that came back: what the editor and the chip show, and what an AI
    /// edit's answer compares its commit against (`req_before`, `finish_reqs`).
    req_judged: Option<ReqJudged>,
    /// What only the app knows for the requirements: the newest case run asked for and the
    /// whole plate's slice figures (`set_print`); the benches come from `req_benches`.
    req_app: fab_elec_host::req_actions::AppFacts,
    /// Per bench by name, its last counted check as facts, with the lowering generation it ran on
    /// (current while the running lowering is still of that one).
    req_benches: Vec<(String, u64, fab_elec_req::BenchRun)>,
    /// The lowering generation the requirements were last judged at, to judge again when it moves.
    req_lowering: Option<u64>,
    /// New asset names not yet handed to the engine; replacement tasks include all of them.
    pending_assets: fab_model::ElecAssets,
    asset_task: Option<TaskHandle<()>>,
    /// Replaced with each hashing task, so only the newest result can register assets.
    asset_rx: ToUIReceiver<Vec<DocSet>>,
    env: Option<(u64, ScriptEnv)>,
    /// The one scene job on the pool and the seq it builds.
    scene_task: Option<(u64, TaskHandle<()>)>,
    /// The newest plate scene asked for: 0 is the loaded design's (`start`), each live design
    /// after it the next.
    scene_seq: u64,
    /// The newest scene handed to the view. A result older than it never reaches the view; one
    /// newer does even when a later one was asked for since, so a drag faster than `scene_of`
    /// still shows its samples' scenes as they finish, not only the last at the drop.
    scene_shown: Option<u64>,
    /// The live design (the preview while one is live) that scene was asked for. Weak keeps no
    /// design alive on the UI thread, and holding it keeps another design from that address, so
    /// the identity test is exact.
    scene_input: Option<Weak<Design>>,
    scene_out: Option<Arc<ElecScene>>,
    /// `scene_out` (or the scene taken last) has no preview in it.
    scene_committed: bool,
    /// The seq of the newest scene job started (0: the loaded design's, `load`).
    scene_started: u64,
    /// The newest drag poses (its moved bodies'; empty: none any more), until the app takes them
    /// (`take_poses`).
    poses_out: Option<Arc<[(Sel, Xform3)]>>,
    /// The placements the live drag moves (`act`), until a snapshot without a preview.
    gesture: Vec<(BoardId, ItemId)>,
    /// The board units handed to `BoardDraws`.
    drawn: HashSet<BoardId>,
    /// The fills of a board that has none yet: one `Arc`, so handing it in again is a no-op.
    no_fills: Arc<Vec<ZoneFill>>,
    /// The bed the engine's documents are of (`fab_ai::Session::switched` when it loaded), and
    /// that bed's plate id when it had one.
    bed: u64,
    plate: Option<String>,
    /// The bed whose device memories the firmware runner holds (`set_memory`).
    memory_bed: Option<u64>,
    /// The electronics files the bed can hold as far as the engine knows: the front is the
    /// version confirmed (loaded, or last seen back in a status), then each write handed out and
    /// not yet seen back. A bed holding none of them was changed by another writer.
    versions: VecDeque<Version>,
    /// Asset names the engine's own writes took off the bed (an undone import's mesh): a reload
    /// that finds one gone confirms the write instead of restarting. A write listing it again
    /// takes it back out of here.
    written_away: BTreeSet<String>,
    /// The bed's documents after another writer changed its electronics documents, until the
    /// engine restarts from them (once its strand is idle).
    outside: Option<Arc<fab_model::Docs>>,
    /// What an outside edit changed, sent once the engine restarted from it shows its first
    /// snapshot: the views then show the edit's text, so its flashes start with it. With the
    /// design shown before the edit, which that snapshot's design is compared against.
    edit_pending: Option<(ElecOutsideEdit, Option<Arc<fab_elec_design::Design>>)>,
    /// Outside edits' sites flashing on the sheet and board, and when each flash began: each
    /// fades over [`fab_elec_view::firmware::FLASH_SECONDS`] (V9.3).
    flashes: Vec<(fab_elec_core::Sel, f64)>,
    /// The bed's newest assets, compared by name and byte Arc before each same-bed reload.
    assets: Arc<fab_model::ElecAssets>,
    /// The model host's bed is this host's and no job that can change it waits: edits are taken.
    settled: bool,
    /// An edit was not taken while the bed was changing (logged once per change).
    edit_dropped: bool,
    /// Typing that came while edits were not taken, for the engine once they are.
    typing: Vec<ViewAction>,
    /// Catalog action calls (`run_action`) not started yet, in arrival order: an edit waits while
    /// edits are not taken or a person's gesture is live, and everything waits for a snapshot.
    held_calls: VecDeque<ActionCall>,
    /// Edits in the engine, each with the caller to answer and the receiver of its one outcome.
    engine_calls: Vec<EngineCall>,
    /// A text change on its check before it goes to the engine (`release_calls`): the calls
    /// behind it wait for it.
    /// Steps the AI committed for a turn of the chat, as (turn, step), until the app takes them.
    turn_steps: Vec<(u64, u64)>,
    text_check: Option<TextCheck>,
    /// Reads on the pool, each with the call to answer.
    read_calls: Vec<(Recipient, ToUIOneshotReceiver<Result<String, String>>)>,
    /// The AI edits that wait for the checks to run on them (`finish_checks`).
    check_waits: Vec<CheckWait>,
    /// The counted check, running or the last one done.
    check: Option<CheckRun>,
    /// Finished calls (`take_answers`): the call, whether it did what it was asked, its text.
    answers: Vec<(u64, bool, String)>,
    /// Each design generation the engine published and the newest commit in it
    /// (`last_commit.seq`, 0 before any), oldest first: the run reflects a step once its source
    /// generation reaches the first one holding it. Generations the run has passed go; the
    /// newest of them stays.
    gens: VecDeque<(u64, u64)>,
    /// The newest step a call committed (an answer can come before its snapshot).
    done_seq: u64,
    /// Transport calls sent as the transport's own action (`SimAction::Cmd`), answered once the
    /// host has handled that action batch.
    run_calls: Vec<(Recipient, &'static str)>,
    /// `elec.sim.status` calls with `wait: seq` (`check_waits`).
    status_waits: Vec<StatusWait>,
    /// Step calls waiting for the run to stand again (`check_steps`).
    step_waits: Vec<StepWait>,
    /// The saved project's file, whose folder exports go beside (None: never saved, or a demo).
    project: Option<std::path::PathBuf>,
    /// Exports and their checks on the pool, each with the call to answer (None: a person's).
    export_calls: Vec<(Delivery, ToUIOneshotReceiver<Result<crate::elec_export::Exported, String>>)>,
    /// The newest export, for `elec.export.validate` without a folder.
    last_export: Option<crate::elec_export::LastExport>,
    /// Pending changes to view notices, in the order they were made.
    notices: Vec<(NoticeView, Option<String>)>,
    /// The Scope's tools for the AI and their calls (`elec_scope.rs`).
    scope: scope::ScopeDesk,
}

/// An `elec.sim.status` call waiting for the run to have a step: once the run reflects it, the
/// commands sent to the run by then (`sent`), until a publication the run stepped with them
/// (`SimHost::stepped_past`): never a count of publications, which a loaded box makes before the
/// run takes the commands or steps with them.
struct StatusWait {
    call: Recipient,
    seq: u64,
    sent: Option<fab_elec_simhost::host::SentMark>,
}

/// The bench actions the app runs on the live bench (`ElecHost::run_action`), as the transport,
/// its status row, the firmware's step buttons, the controls' knobs and the views' voltage tags
/// do, and the counted check beside it as EXPECTATIONS' Check bench does;
/// `chat::register_elec_actions` puts these in the catalog.
pub static SIM_ACTIONS: &[fab_elec_design::action::ActionDef] = &[
    fab_elec_simhost::actions::STATUS,
    fab_elec_simhost::actions::LEDS,
    fab_elec_simhost::actions::MIC,
    fab_elec_simhost::actions::NET,
    fab_elec_simhost::actions::PROBE,
    fab_elec_simhost::actions::PLAY,
    fab_elec_simhost::actions::PAUSE,
    fab_elec_simhost::actions::RATE,
    fab_elec_simhost::actions::STEP,
    fab_elec_simhost::actions::STEP_FW,
    fab_elec_simhost::actions::SET,
    fab_elec_simhost::actions::BENCH_RUN,
];

/// The member number a counted check's firmware process is made as: no ensemble member's, so it
/// plays the network's fakes and reports no events or screens, as a member does.
const CHECK_MEMBER: u32 = u32::MAX;

/// The bench's counted check (`elec.bench.run`, EXPECTATIONS' Check bench): a run of its own on
/// the pool from the running lowering's operating point to the check's stop, every point judged
/// (`fab_elec_bench::bench_run`), and the calls waiting for its report. One at a time: a new one
/// stops the one before. Kept once done, so the shelf can say when the design moved on.
struct CheckRun {
    calls: Vec<Recipient>,
    /// What the shelf shows of it (`ElecLive::check`).
    shown: Arc<BenchCheck>,
    stop: Arc<AtomicBool>,
    /// The source generation of the lowering it runs (`SimHost::generation`), once it started
    /// (it waits while the bench's lowering is in flight).
    generation: Option<u64>,
    /// Its report, while it runs.
    rx: Option<ToUIOneshotReceiver<fab_elec_bench::BenchReport>>,
}

impl Drop for CheckRun {
    /// A check let go of (its host gone, a newer one in its place) stops its run.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// An `elec.sim.step` or `elec.sim.step_fw` call, answered once the run stands again after it
/// (`ElecHost::check_steps`).
struct StepWait {
    call: Recipient,
    /// The simulated time a timed step reaches; None for a firmware step, which stops where the
    /// code does.
    to: Option<f64>,
    /// The values when the step was sent: the answer needs a newer publication.
    sent: Option<Arc<fab_elec_sim::live::LiveValues>>,
}

/// The counted check's answer: the bench, its stop and how far the run got (its points, a
/// failure, a stop, the steps only the gmin ladder got through), and per expectation its outcome
/// (`pass`, `fail`, `not judged`), value and when judged, first failure and worst excursion.
/// O(expectations).
fn check_text(check: &BenchCheck, report: &fab_elec_bench::BenchReport) -> String {
    use fab_ai::out;
    use fab_elec_bench::expect::Outcome;
    use fab_elec_circuit::{QtyUnit, Quantity};
    // a quantity as the bench writes one (`"312.5uA"`, `"8ms"`), a count or percentage as a
    // plain number, unrounded
    let qty = |si: f64, unit: &str| {
        let unit = match unit { "V" => QtyUnit::Volt, "A" => QtyUnit::Ampere, "W" => QtyUnit::Watt, "Hz" => QtyUnit::Hertz, "s" => QtyUnit::Second, _ => QtyUnit::Count };
        match si.is_finite() {
            false => out::S::Nil,
            true if unit == QtyUnit::Count => out::S::Num(si),
            true => out::s(fab_elec_lang::units::literal_text(&Quantity { si, unit })),
        }
    };
    let state = if report.stopped { "stopped" } else if report.error.is_some() { "failed" } else { "done" };
    let items = check.expects.iter().zip(&report.states).map(|(expect, s)| {
        let unit = expect.unit();
        let mut fields = vec![
            ("name", out::s(&expect.name)),
            ("outcome", out::s(match s.outcome() { Outcome::Pass => "pass", Outcome::Fail => "fail", Outcome::NotJudged => "not judged" })),
        ];
        if s.value.is_finite() {
            fields.extend([("value", qty(s.value, unit)), ("at", qty(s.at, "s"))]);
        }
        if let Some((t, value)) = s.failed {
            fields.push(("first_failure", out::obj(vec![("t", qty(t, "s")), ("value", qty(value, unit))])));
        }
        fields.push(("worst", qty(s.worst, unit)));
        out::obj(fields)
    });
    let mut fields = vec![
        ("bench", out::s(&check.bench)),
        ("state", out::s(state)),
        ("tstop", qty(check.tstop, "s")),
        ("t", qty(report.t, "s")),
        ("points", out::int(report.points)),
    ];
    if let Some(error) = &report.error {
        fields.push(("error", out::s(error)));
    }
    if !report.rescues.is_empty() {
        fields.push(("rescued_steps", out::int(report.rescues.len())));
    }
    if !report.diags.is_empty() {
        fields.push(("unresolved", out::arr(report.diags.iter().map(|d| out::s(&d.message)))));
    }
    fields.push(("items", out::arr(items)));
    out::text(&out::obj(fields))
}

/// A rate field's text: simulated seconds per real second, or `inf` for as fast as it goes.
fn parse_rate(text: &str) -> Result<f64, String> {
    let text = text.trim();
    let rate = if text == "inf" { f64::INFINITY } else { text.parse::<f64>().unwrap_or(f64::NAN) };
    if rate > 0.0 { Ok(rate) } else { Err(format!("rate: expected a positive rate or inf, got {text}")) }
}

/// A rate in an answer: `inf` as a name, any other as a number.
fn rate_out(rate: f64) -> fab_ai::out::S {
    if rate.is_infinite() { fab_ai::out::name("inf") } else { fab_ai::out::n(rate) }
}

fn mic_file(file: &str) -> Result<std::path::PathBuf, String> {
    let path = std::path::Path::new(file);
    if file.is_empty() { return Err("file: expected a WAV path relative to the project".into()); }
    let drive_root = matches!(file.as_bytes(), [b'A'..=b'Z' | b'a'..=b'z', b':', b'/' | b'\\', ..]);
    if path.has_root() || matches!(path.components().next(), Some(std::path::Component::Prefix(_))) || file.starts_with('\\') || drive_root {
        return Err("file: use a path relative to the project, without an absolute path or drive prefix".into());
    }
    if file.split(['/', '\\']).any(|part| part == "..") {
        return Err("file: a project WAV path cannot contain ..".into());
    }
    Ok(path.to_owned())
}

fn net_arrival(ms: f64, arrival: &fab_elec_mcu_cosim::net::Arrival) -> fab_ai::out::S {
    use fab_ai::out;
    use fab_elec_mcu_cosim::net::{Arrival, code_text};
    let (c, gen) = arrival.attempt();
    let mut fields = vec![("ms", out::S::Num(ms)), ("c", out::int(c)), ("gen", out::int(gen))];
    match arrival {
        Arrival::Opened { .. } => fields.push(("kind", out::name("opened"))),
        Arrival::Ended { code, detail, .. } => fields.extend([
            ("kind", out::name("ended")), ("code", out::s(code_text(*code))), ("detail", out::int(*detail)),
        ]),
        Arrival::Taken { bytes, .. } => fields.extend([("kind", out::name("taken")), ("bytes", out::int(*bytes))]),
        Arrival::Message { binary, bytes, .. } => {
            fields.push(("kind", out::name("message")));
            if *binary {
                fields.push(("length", out::int(bytes.len())));
                fields.push(("hex", out::s(bytes.iter().take(8).map(|b| format!("{b:02x}")).collect::<String>())));
            } else {
                // Four UTF-8 bytes per character at most. The Splash writer escapes CR and LF.
                let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(120 * 4)]);
                fields.push(("text", out::s(prefix.chars().take(120).collect::<String>())));
            }
        }
    }
    out::obj(fields)
}

fn net_page(status: &fab_elec_mcu_cosim::NetStatus, log: &fab_elec_mcu_cosim::SharedLog, from: usize) -> Result<String, String> {
    use fab_ai::out;
    use fab_elec_mcu_cosim::net;
    use makepad_script::syntax::write::write;
    let connections = out::arr(status.conns.iter().map(|conn| {
        let state = match conn.state { net::READY => "ready", net::CONNECTING => "connecting", _ => "offline" };
        out::obj(vec![("c", out::int(conn.c)), ("state", out::name(state)), ("code", out::int(conn.code)), ("detail", out::int(conn.detail))])
    }));
    let page = |next, cuts, arrivals| write(&out::obj(vec![
        ("next", out::int(next)), ("cuts", out::int(cuts)), ("connections", connections.clone()),
        ("up", out::int(status.up)), ("down", out::int(status.down)),
        ("bytes_up", out::int(status.bytes_up)), ("bytes_down", out::int(status.bytes_down)),
        ("arrivals", out::arr(arrivals)),
    ]));
    // Leave room for both cursor fields at their widest. Only whole rows enter a tool reply.
    let header = page(usize::MAX, u64::MAX, Vec::new())?;
    let mut room = makepad_ai_services::wire::MAX_RESULT_BYTES.saturating_sub(header.len());
    let mut rows = Vec::new();
    let log = log.lock().unwrap_or_else(|e| e.into_inner());
    let cuts = log.cuts;
    let arrivals = log.arrivals().get(from..).ok_or_else(|| format!("from: the log has {} arrivals; read from 0 after a reset or cut", log.arrivals().len()))?;
    for (ns, arrival) in arrivals {
        let row = write(&net_arrival(*ns as f64 / 1_000_000.0, arrival))?;
        let Some(rest) = room.checked_sub(row.len() + 2) else { break; };
        rows.push(out::S::Verbatim(row));
        room = rest;
    }
    drop(log);
    page(from + rows.len(), cuts, rows)
}

/// One catalog action called through `ElecHost::run_action`: its arguments as Splash text, its
/// author, the chat call that waits for it, and the bed it was made for.
struct ActionCall {
    name: &'static str,
    args: Vec<(String, String)>,
    by: By,
    call: Delivery,
    bed: u64,
    /// Where to deliver the engine's outcome once the call leaves the queue.
    caller: Option<EngineCaller>,
    /// A `fab.elec_text` call's change (its name is [`ELEC_TEXT`]); None for a catalog action.
    text: Option<TextAsk>,
}

/// A person's pending Add form is identified by the requirement name it is waiting to see.
fn add_form<'a>(action: &str, by: By, args: &'a [(String, String)]) -> Option<&'a str> {
    if by != By::Person || action != "elec.req.add" { return None; }
    args.iter().find(|(k, _)| k == "name").map(|(_, v)| v.trim())
}

/// `fab.elec_text`: a change of a document's text, held in order with the catalog calls
/// (`ElecHost::run_text`).
pub const ELEC_TEXT: &str = "fab.elec_text";

/// The new findings an AI edit's answer lists at most, a check list page's worth; the rest are
/// counted, and `elec.check.list` lists them.
const EDIT_FINDINGS: usize = 40;

/// What a `fab.elec_text` call asks: the document by name, the revision whose text it read, and
/// the change of that text.
pub struct TextAsk {
    pub doc: String,
    pub rev: u64,
    pub change: fab_elec_host::doc_actions::TextChange,
}

/// Independent destinations: only a real chat reply changes person-facing behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Recipient {
    reply: Option<u64>,
    journal: Option<u64>,
    outer: bool,
}
type Delivery = Option<Recipient>;
fn reply(call: Delivery) -> Option<u64> { call.and_then(|r| r.reply) }

#[derive(Clone, Copy)]
enum EngineCaller {
    Chat(u64),
    BoardEdit(u64),
    Tidy { bed: u64, unit: BoardId },
}

/// An edit in the engine: the caller to answer (None: nobody waits), its action's name, the
/// receiver of its one outcome, and for a text change what its answer says beside the commit.
struct EngineCall {
    dest: Delivery,
    binding: Option<(crate::journal::Electronics, fab_elec_host::Ticket)>,
    call: Option<EngineCaller>,
    name: &'static str,
    rx: std::sync::mpsc::Receiver<Outcome>,
    text: Option<TextDone>,
    /// A Scope tool's probe edit, which hears where its statement is (`elec_scope.rs`).
    scope: Option<scope::ScopeEdit>,
    /// An AI edit of what the checks read: the findings shown when it was sent, by stable id, sorted. Its answer
    /// waits for the checks to run on it and says what it changed for them (`CheckWait`).
    before: Option<Vec<u64>>,
    /// An AI edit of a project with requirements, or one that writes them: the rows shown when it
    /// was sent (`req_before`). Its answer waits for the requirements judged on its commit.
    reqs: Option<Vec<fab_elec_host::req_actions::ReqMark>>,
}

/// An AI edit's answer, held until every check has run on its commit `seq` (with the findings
/// that are new since `before` and how many the edit cleared) and its requirements are judged on
/// it (with what it changed for them, against `reqs`).
struct CheckWait {
    call: Delivery,
    seq: u64,
    fields: Vec<(String, fab_ai::out::S)>,
    /// The findings shown when it was sent; None once its `checks` are in, or when it has none.
    before: Option<Vec<u64>>,
    /// The requirement rows shown when it was sent; None once its `requirements` are in, or when
    /// the project has none.
    reqs: Option<Vec<fab_elec_host::req_actions::ReqMark>>,
    /// It answers with `checks`: its requirements are judged on a snapshot its checks ran on.
    checks: bool,
}

/// One judging of the requirements: the commit and whether every check had run on the snapshot
/// it judged, and what it found (None: the project has no requirements).
#[derive(Clone)]
struct ReqJudged {
    seq: u64,
    checks: bool,
    shown: Option<Arc<fab_elec_view::ReqShown>>,
}

/// The commit a snapshot shows; 0 before the first.
fn commit_seq(snap: &DesignSnapshot) -> u64 {
    snap.last_commit.as_ref().map_or(0, |commit| commit.seq)
}

/// Whether the project has a requirements document. O(documents).
fn has_requirements(snap: &DesignSnapshot) -> bool {
    use fab_elec_lang::DocKind;
    snap.docs.as_ref().is_some_and(|docs| docs.texts.iter().any(|text| DocKind::of_name(&text.name) == Some(DocKind::Requirements)))
}

/// The factory document's reader errors, shown with the same source lines as requirement errors.
pub(super) fn profile_mistakes(snap: &DesignSnapshot) -> impl Iterator<Item = String> + '_ {
    snap.profile_diags.iter().filter_map(|diag| {
        let (name, line) = fab_elec_host::req_actions::located(snap.docs.as_ref()?, &diag.sel);
        Some(format!("{name}:{line}: {}", diag.message))
    })
}

/// The findings the Checks pane shows (not those waiting for a pour's refill), by stable id,
/// sorted. O(findings log findings).
fn shown_findings(snap: &fab_elec_view::DesignSnapshot) -> Vec<u64> {
    let mut ids: Vec<u64> = snap.violations.value.iter().map(|v| v.id).filter(|id| snap.awaiting.binary_search(id).is_err()).collect();
    ids.sort_unstable();
    ids
}

/// A text change the engine commits: its document, the revision it was made for, each edit's
/// lines in the new text (0-based) and whether it compiled (Err: why it was not compiled).
struct TextDone {
    form_name: Option<String>,
    doc: String,
    rev: u64,
    lines: Vec<std::ops::Range<usize>>,
    compiled: Result<(), String>,
    /// The existing document's id, where a refusal's mistake may be. A new document has no prior id.
    id: Option<fab_elec_core::DocId>,
    /// A circuit, blocks, bench or part document's change, committed only once it evaluates
    /// (`Driver::checked_text`): the text it makes, where its mistake is when it is refused.
    /// None for firmware, checked by its build.
    evaluated: Option<Arc<str>>,
    /// What an AI's requirements text does to the person's requirements, one line each, for it
    /// to say (`requirements_changed` in its answer).
    said: Vec<String>,
}

/// A text change on its check: what goes to the engine once it passes, and the check's result
/// (Err: the call's whole answer, nothing committed).
struct TextCheck {
    form_name: Option<String>,
    call: Delivery,
    by: By,
    /// Whether the AI's answer waits for the findings on this edit's commit.
    checks: bool,
    bed: u64,
    doc: fab_elec_core::DocId,
    name: String,
    rev: u64,
    rx: ToUIOneshotReceiver<Result<TextChecked, String>>,
    result: Option<Result<TextChecked, String>>,
}

/// A text change that may be committed: the engine's replacement, each edit's lines in the new
/// text, and whether a build of it compiled (Err: why it was not compiled).
struct TextChecked {
    edit: fab_elec_design::TextEdit,
    lines: Vec<std::ops::Range<usize>>,
    compiled: Result<(), String>,
    /// The new text of a change the engine evaluates before committing it (`TextDone::evaluated`).
    evaluated: Option<Arc<str>>,
    /// `TextDone::said`.
    said: Vec<String>,
}

/// `elec.flash.ask`: the Firmware pane's Flash to row brought forward for the person, and what
/// it shows. The person chooses the board and presses Flash; the call writes nothing to a board.
pub const FLASH_ASK: fab_elec_design::action::ActionDef = fab_elec_design::action::ActionDef {
    name: "elec.flash.ask",
    label: "Ask to flash",
    group: "Firmware",
    help: "Ask the person to flash the running firmware to a board: the Firmware pane comes forward with its Flash to row, and the answer says what the row shows. The person chooses the board and presses Flash; this call writes nothing to a board.",
    returns: "doc: text  the firmware the running image was built from
part: text  the part that runs it
rev: int  the document revision of the running image
boards: [{..}]  the boards Flash to lists
  port: text
  name: text
  may: bool  false: its row is greyed
  why: text  why it may not be flashed
chosen: text  the board chosen now, by its port
hint: text  why the list is empty
state: text  how a flash, read or back-up under way stands",
    fields: &[],
    inputs: &[], risk: fab_elec_design::action::Risk::Run, keys: "",
};

/// The firmware actions the app runs; `chat::register_elec_actions` puts them in the catalog.
pub static FW_ACTIONS: &[fab_elec_design::action::ActionDef] = &[FLASH_ASK];

/// A firmware build's diagnostics as a text call's answer: nothing was committed, and where each
/// one is in the changed text (line and column from 1). O(text x diagnostics).
fn compile_refusal(doc: &str, rev: u64, text: &str, diags: &[fab_elec_mcu::Diagnostic]) -> String {
    use fab_ai::out;
    let rows = diags.iter().map(|diag| {
        let mut fields = Vec::new();
        if let Some(span) = &diag.span {
            let (line, col) = line_col(text, span.start);
            fields.push(("line", out::int(line)));
            fields.push(("col", out::int(col)));
        }
        if let Some(param) = &diag.param {
            fields.push(("param", out::s(param.clone())));
        }
        fields.push(("message", out::s(diag.message.clone())));
        out::line(out::obj(fields))
    });
    out::text(&out::obj(vec![
        ("error", out::s(format!("{doc} does not compile with the change, so nothing was committed; it stays at rev {rev}"))),
        ("diagnostics", out::arr(rows)),
    ]))
}

/// Byte `at` of `text` as a line and column, both from 1 (a column counts characters). O(at).
fn line_col(text: &str, at: usize) -> (usize, usize) {
    let at = at.min(text.len());
    let at = (0..=at).rev().find(|&at| text.is_char_boundary(at)).unwrap_or(0);
    let head = &text[..at];
    (head.matches('\n').count() + 1, head.rsplit('\n').next().map_or(0, |last| last.chars().count()) + 1)
}

/// The board's fabrication files and their check (`ElecHost::export_call`).
const EXPORT: &str = "elec.export";
const EXPORT_VALIDATE: &str = "elec.export.validate";

/// The reads `run_action` answers from the committed design, by action name.
fn read_of(name: &str) -> Option<fn(&Design, &[(String, String)]) -> Result<String, fab_elec_core::Diag>> {
    match name {
        "elec.bom.read" => Some(fab_elec_host::bom_actions::bom_read),
        "elec.design.read" => Some(fab_elec_host::edit_actions::design_read),
        _ => None,
    }
}

/// The editors that show `sites`, each once, in the order the sites name them.
fn site_editors(sites: &[Sel]) -> Vec<ElecEditor> {
    let mut editors = Vec::new();
    for sel in sites {
        let editor = match sel {
            Sel::Comp(_) | Sel::Pin(_) | Sel::Net(_) | Sel::Block(..) => ElecEditor::Schematic,
            Sel::Item(..) => ElecEditor::Board,
            _ => continue,
        };
        if !editors.contains(&editor) {
            editors.push(editor);
        }
    }
    editors
}

impl ElecHost {
    /// Start the host on the bed's electronics documents (`docs` of bed `bed`, plate `plate`, with
    /// the files beside them in `assets`; an empty design when there are none): the strand's first job loads them, hands the
    /// plate's scene of the design to a pool job beside the engines (the boards reach the plate
    /// without waiting for the first snapshot), builds the engine (full results, the one inline
    /// fill) and publishes its first snapshot. Nothing of it runs on the UI thread.
    /// The loaded boards are drawn first, without fills.
    /// `steps` supplies numbers, including early reservations; `logged` tracks logged entries.
    #[cfg(test)]
    pub fn start(cx: &mut Cx, bed: u64, plate: Option<String>, docs: Arc<fab_model::Docs>, assets: Arc<fab_model::ElecAssets>, steps: Arc<AtomicU64>, logged: Arc<AtomicU64>) -> ElecHost {
        Self::open(cx, None, bed, plate, docs, assets, steps, logged)
    }

    pub(crate) fn open(cx: &mut Cx, journal: Option<crate::journal::Plate>, bed: u64, plate: Option<String>, docs: Arc<fab_model::Docs>, assets: Arc<fab_model::ElecAssets>, steps: Arc<AtomicU64>, logged: Arc<AtomicU64>) -> Self {
        // A demo can get its journal through plate_open after this host has started.
        cx.global::<fab_elec_view::snapshot::CapturePlannerGestures>();
        if let Some(plate) = &journal { plate.attach(&cx.thread_spawner()); }
        cx.global::<Tidying>().boards.clear();
        let mut host = ElecHost {
            journal: journal.map(crate::journal::Electronics::new), journal_calls: HashMap::new(), next_journal_call: 0,
            started: Cx::monotonic_now(),
            scene_timed: false,
            driver: Driver::new(cx, steps, logged),
            sim: None,
            firmware: crate::elec_fw::FwRunner::default(),
            sim_benches: None,
            resume_after_failure: None,
            sim_no_bench_logged: false,
            sim_wants: HashSet::new(),
            sim_probes: HashMap::new(),
            sim_resume: true,
            view: ViewState::default(),
            group_selected_in_batch: false,
            states: HashMap::new(),
            states_from: HashMap::new(),
            sheet_states: HashMap::new(),
            sheet_states_from: HashMap::new(),
            view_moved: false,
            case: Strand::with_state(&cx.task_pool(), "electronics case publications", CaseBridge::default()),
            case_seen: 0,
            case_task: None,
            bom_task: None, bom_rx: Default::default(), bom_design: None, bom_group: true, bom_quantity: 10,
            price_book: None, price_jobs: [(None, LibraryJob::Load)].into(), price_running: None,
            price_rx: Default::default(), price_proposals: Default::default(), price_review_changed: false,
            rates: None, rates_task: None, rates_rx: Default::default(),
            req_task: None,
            doc_load_rev: 0,
            req_rx: ToUIReceiver::default(),
            req_judged: None,
            req_app: Default::default(),
            req_benches: Vec::new(),
            req_lowering: None,
            pending_assets: fab_model::ElecAssets::new(),
            asset_task: None,
            asset_rx: ToUIReceiver::default(),
            env: None,
            scene_task: None,
            scene_seq: 0,
            scene_shown: None,
            scene_input: None,
            scene_out: None,
            scene_committed: true,
            scene_started: 0,
            poses_out: None,
            gesture: Vec::new(),
            drawn: HashSet::new(),
            no_fills: Arc::new(Vec::new()),
            bed,
            memory_bed: None,
            plate,
            versions: VecDeque::new(),
            written_away: BTreeSet::new(),
            outside: None,
            edit_pending: None,
            flashes: Vec::new(),
            assets: assets.clone(),
            settled: true,
            edit_dropped: false,
            typing: Vec::new(),
            held_calls: VecDeque::new(),
            engine_calls: Vec::new(),
            turn_steps: Vec::new(),
            text_check: None,
            read_calls: Vec::new(),
            check_waits: Vec::new(),
            check: None,
            answers: Vec::new(),
            gens: VecDeque::new(),
            done_seq: 0,
            run_calls: Vec::new(),
            status_waits: Vec::new(),
            step_waits: Vec::new(),
            project: None,
            export_calls: Vec::new(),
            last_export: None,
            notices: Vec::new(),
            scope: Default::default(),
        };
        host.read_rates(cx);
        host.load(cx, docs, assets, None);
        host
    }

    /// The strand job that (re)loads the engine from `docs`; the engine it replaces, with its log
    /// and preview, and `old` (the commands the old engine did not take and the snapshots retired
    /// for it) are dropped there, off the UI thread.
    fn load(&mut self, cx: &mut Cx, docs: Arc<fab_model::Docs>, assets: Arc<fab_model::ElecAssets>, old: Option<Inbox>) {
        self.versions = VecDeque::from([Version { docs: Arc::new(electronics(&docs)), assets: assets.keys().cloned().collect() }]);
        self.written_away.clear();
        self.outside = None;
        let steps = self.driver.steps().clone();
        let logged = self.driver.logged().clone();
        let scene_tx = self.driver.sender();
        let pool = cx.task_pool();
        let journal = self.journal.clone();
        self.driver.load(cx, old, move |tx| {
            if let Some(journal) = &journal { journal.loaded(&docs, &assets); }
            let (design, docs, keys, library) = plate_project(&docs, &assets, crate::part_models::library().as_deref());
            let boards = design.units.iter().filter_map(|u| u.board.clone()).collect();
            tx.send(FromEngine::Loaded { boards });
            let scene_design = design.clone();
            let pool_tx = scene_tx.clone();
            match pool.submit_named(Class::Visible, "elec scene for the plate", move || {
                let scene = Arc::new(scene_of(&scene_design, &Pose::default()));
                pool_tx.send(FromEngine::Scene { seq: 0, scene, committed: true });
            }) {
                Ok(task) => task.detach(),
                Err(error) => {
                    // The pool refuses work: the strand builds the plate's scene before the engines.
                    log!("electronics scene for the plate: {error}; built before the engines");
                    let scene = Arc::new(scene_of(&design, &Pose::default()));
                    scene_tx.send(FromEngine::Scene { seq: 0, scene, committed: true });
                }
            }
            let fill_tx = tx.clone();
            let engine = Engine::new_held(design, Arc::default(), steps, logged, "demo".into())
                .with_app_tools()
                .with_fill_wait_notice(move |notice| fill_tx.send(FromEngine::FillWait(notice)))
                .with_mesh_keys(keys)
                .with_library_keys(library);
            let engine = match docs { Some(docs) => engine.with_docs(docs), None => engine };
            engine.with_journal(journal.map(|journal| Arc::new(journal) as Arc<dyn fab_elec_host::JournalSink>))
        });
    }

    pub(crate) fn plate_open(&mut self, cx: &mut Cx, journal: Option<crate::journal::Plate>, bed: u64, plate: Option<String>, docs: Arc<fab_model::Docs>, assets: Arc<fab_model::ElecAssets>) {
        if bed != self.bed || self.journal.is_none() {
            if bed != self.bed && plate.is_some() && plate == self.plate {
                if let Some(old) = self.journal.clone() {
                    let (docs, assets) = (docs.clone(), assets.clone());
                    let next = journal.clone();
                    self.driver.on_strand(move || {
                        if old.loaded(&docs, &assets) {
                            if let Some(plate) = next { plate.checkpoint(); }
                        }
                    });
                }
            }
            if let Some(journal) = &journal { journal.attach(&cx.thread_spawner()); }
            self.journal = journal.map(crate::journal::Electronics::new);
            if bed == self.bed {
                if let Some(journal) = &self.journal { self.driver.attach_journal(Arc::new(journal.clone())); }
            }
        }
        self.reload(cx, bed, plate, docs, assets);
    }

    /// The bed's documents and their `assets`: confirm a known electronics version, or restart from
    /// another writer's text or changed/removed assets once the strand is idle. New asset names
    /// are registered on the pool without restarting. A different bed restarts at once. Cost:
    /// O(files x versions) text compares and O(assets) name/Arc compares, no asset hashing on the
    /// UI; a restart releases O(units) on the UI and loads on the strand.
    pub fn reload(&mut self, cx: &mut Cx, bed: u64, plate: Option<String>, docs: Arc<fab_model::Docs>, assets: Arc<fab_model::ElecAssets>) {
        if bed == self.bed {
            // A demo saved is a plate from then on, on the same bed.
            self.plate = plate;
            // Both maps are sorted: one merge pass, comparing byte identities without reading
            // the bytes. A replacement Arc reloads even when its contents happen to be equal.
            let mut previous = self.assets.iter().peekable();
            let mut added = Vec::new();
            let mut removed = Vec::new();
            let mut changed_assets = Vec::new();
            for (name, bytes) in assets.iter() {
                while previous.peek().is_some_and(|(old, _)| *old < name) {
                    removed.push(previous.next().expect("the old name precedes this one").0.clone());
                }
                if previous.peek().is_some_and(|(old, _)| *old == name) {
                    let (_, old) = previous.next().expect("the asset name was present");
                    if !Arc::ptr_eq(old, bytes) {
                        changed_assets.push(name.clone());
                    }
                } else {
                    added.push((name.clone(), bytes.clone()));
                }
            }
            removed.extend(previous.map(|(name, _)| name.clone()));
            self.assets = assets;
            let now = electronics(&docs);
            // A version holds when its documents are the bed's and every asset it lists is still
            // there. An asset gone from the bed is the engine's own removal (an undone import's
            // mesh) only when one of its writes took it off.
            let known = self.versions.iter().position(|version| same_files(&version.docs, &now)
                && version.assets.iter().all(|name| self.assets.contains_key(name)));
            let ours = removed.iter().all(|name| self.written_away.contains(name));
            if let Some(i) = known.filter(|_| changed_assets.is_empty() && ours && self.outside.is_none()) {
                drop(self.versions.drain(..i));
                // Assets the engine listed itself (an import's mesh, its redo) have their keys there.
                let listed = self.versions[0].assets.clone();
                added.retain(|(name, _)| listed.binary_search(name).is_err());
                if !added.is_empty() {
                    self.pending_assets.extend(added);
                    self.start_assets(cx);
                }
                return;
            }
            // The editors this outside edit changed, against the documents the engine has or is
            // about to load (writes in quick succession each name only their own): Follow AI's cue.
            self.outside_edit(cx, EditOrigin::Elsewhere, &now);
            if self.outside.is_none() {
                let newest = &self.versions.back().expect("the engine has loaded documents").docs;
                let mut names: Vec<_> = now.keys().chain(newest.keys())
                    .filter(|name| now.get(*name) != newest.get(*name)).cloned().collect();
                names.extend(changed_assets);
                names.extend(removed);
                names.sort_unstable();
                names.dedup();
                log!("electronics: {} changed outside the electronics engine; it loads the bed's documents again and its undo history starts over", names.join(", "));
            }
            // The forthcoming load owns all bytes; no older asset result reaches this engine.
            if let Some(task) = self.asset_task.take() {
                task.cancel();
            }
            self.asset_rx = ToUIReceiver::default();
            self.pending_assets.clear();
            self.outside = Some(docs);
            if self.driver.idle() {
                let docs = self.outside.take().expect("the bed's outside documents");
                self.restart(cx, docs);
            }
            return;
        }
        // The plate on the bed read again (reloaded, reopened): what its files bring that the engine
        // does not have was changed on disk, outside the app. Another plate's load shows no edit.
        self.edit_pending = None;
        self.flashes.clear();
        if plate.is_some() && plate == self.plate {
            self.outside_edit(cx, EditOrigin::Disk, &electronics(&docs));
        }
        self.assets = assets;
        log!("electronics: another document is on the bed; the engine loads its {} electronics documents{}", docs.len(), if docs.is_empty() { " (none: no board)" } else { "" });
        self.bed = bed;
        cx.global::<Tidying>().boards.clear();
        self.plate = plate;
        // Another plate's bench is a run of its own: a Stop of the one before does not hold it,
        // so the first want (a shown scope, Play) plays it.
        self.sim_resume = true;
        self.restart(cx, docs);
    }

    /// Send what `now` changes against the documents the engine has or is about to load, once the
    /// engine shows it: the editors of the changed documents, and those the design diff finds
    /// (a block's definition changed shows in the parts it builds). O(documents) compares, O(lines)
    /// of changed firmware.
    fn outside_edit(&mut self, cx: &mut Cx, origin: EditOrigin, now: &fab_model::Docs) {
        let Some(before) = self.outside.as_deref().or(self.versions.back().map(|version| &*version.docs)) else { return };
        if same_files(before, now) {
            return;
        }
        let editors = edited_editors(before, now);
        let lines = firmware_lines(before, now);
        // writes in quick succession are one edit: each names its own change against the last
        match &mut self.edit_pending {
            Some((pending, _)) => {
                for editor in editors {
                    if !pending.editors.contains(&editor) {
                        pending.editors.push(editor);
                    }
                }
                for (doc, range) in lines {
                    match pending.lines.iter_mut().find(|(d, _)| *d == doc) {
                        Some((_, was)) => *was = was.start.min(range.start)..was.end.max(range.end),
                        None => pending.lines.push((doc, range)),
                    }
                }
            }
            None => {
                let shown = self.driver.snap().map(|snap| snap.design.clone());
                self.edit_pending = Some((ElecOutsideEdit { origin, label: String::new(), sites: Vec::new(), editors, lines }, shown));
            }
        }
        let _ = cx;
    }

    /// Hash pending additions and register their bytes on the pool. A newer task includes all
    /// names not yet sent, so replacing its receiver cannot lose an earlier import. A refused
    /// submission keeps the pending names for a later event. Cost on the UI: O(pending assets)
    /// name/Arc clones; the worker hashes O(asset bytes) and parses each distinct mesh once.
    fn start_assets(&mut self, cx: &mut Cx) {
        let assets = self.pending_assets.clone();
        let rx = ToUIReceiver::default();
        let tx = rx.sender();
        match cx.task_pool().submit_named(Class::Background, "elec assets added", move || {
            let set = asset_keys(&assets).into_iter().map(|(name, key)| DocSet::Asset { name, key }).collect();
            if makepad_widgets::makepad_platform::thread::checkpoint().is_ok() {
                let _ = tx.send(set);
            }
        }) {
            Ok(task) => {
                self.asset_rx = rx;
                if let Some(old) = self.asset_task.replace(task) {
                    old.cancel();
                }
            }
            Err(error) => log!("electronics assets added: {error}; the pending assets will retry"),
        }
    }

    /// Drop the engine's history, preview, commands, fills, case inputs, scene, bench, selection
    /// and drawn boards, then load `docs`. Old results reach receivers that are gone. Cost:
    /// O(units) releases on the UI; the old engine and inbox are dropped on the strand.
    fn restart(&mut self, cx: &mut Cx, docs: Arc<fab_model::Docs>) {
        // a bench call waiting on the old documents' run is answered once, as an engine call is
        let waits = std::mem::take(&mut self.status_waits).into_iter().map(|wait| wait.call)
            .chain(std::mem::take(&mut self.step_waits).into_iter().map(|wait| wait.call));
        for call in std::mem::take(&mut self.run_calls).into_iter().map(|(call, _)| call).chain(waits) {
            self.answer(Some(call), Err("the plate's electronics loaded again: the run started over".into()));
        }
        // a placement committed before the reload is answered without its checks
        for CheckWait { call, mut fields, before, reqs, .. } in std::mem::take(&mut self.check_waits) {
            use fab_ai::out;
            if before.is_some() {
                fields.push(("checks".into(), out::obj(vec![("current", out::flag(false)), ("why", out::s("the plate's electronics loaded again before the checks ran"))])));
            }
            if reqs.is_some() {
                fields.push(("requirements".into(), out::obj(vec![("current", out::flag(false)), ("why", out::s("the plate's electronics loaded again before the requirements were judged"))])));
            }
            self.answer(call, Ok(out::text(&out::S::Object(None, fields))));
        }
        self.gens.clear();
        self.done_seq = 0;
        for (_, mut states) in self.states.drain().chain(self.sheet_states.drain()) {
            states.release(cx);
        }
        self.states_from.clear();
        self.sheet_states_from.clear();
        for unit in self.drawn.drain() {
            BoardDraws::remove(cx, unit);
        }
        for task in self.case_task.take().into_iter().chain(self.req_task.take()).chain(self.bom_task.take()).chain(self.scene_task.take().map(|(_, task)| task)).chain(self.asset_task.take()) {
            task.cancel();
        }
        if let Some(mut sim) = self.sim.take() {
            sim.command(&SimCmd::Pause);
            if cx.has_global::<ElecLive>() {
                let live = &mut cx.get_global::<ElecLive>().sim;
                (live.nets, live.segs, live.parts) = (None, None, None);
            }
        }
        self.sim_benches = None;
        self.resume_after_failure = None;
        self.firmware.restart();
        if cx.has_global::<ElecLive>() {
            let live = &mut cx.get_global::<ElecLive>().sim;
            live.firmware = None;
            if !live.screens.is_empty() {
                live.screens = Arc::default();
                live.screens_key += 1;
            }
        }
        self.sim_no_bench_logged = false;
        let old = self.driver.reset();
        let retiring: Vec<_> = self.engine_calls.iter_mut().filter_map(|call| call.binding.take()).collect();
        if !retiring.is_empty() {
            self.driver.on_strand(move || {
                for (journal, ticket) in retiring { journal.unbind(ticket); }
            });
        }
        self.req_rx = ToUIReceiver::default();
        self.req_judged = None;
        self.bom_rx = Default::default();
        self.bom_design = None;
        self.req_benches.clear();
        if cx.get_global_ref::<ElecLive>().is_some() {
            cx.get_global::<ElecLive>().bom = None;
            cx.get_global::<ElecLive>().requirements = None;
        }
        self.asset_rx = ToUIReceiver::default();
        self.pending_assets.clear();
        self.case.submit(Class::Background, TaskTicket::none(), |state| *state = CaseBridge::default()).detach();
        self.case_seen = 0;
        self.env = None;
        // The new engine's first scene is seq 0 again (`load`); the old receiver is gone.
        self.scene_seq = 0;
        self.scene_started = 0;
        self.scene_shown = None;
        self.scene_input = None;
        self.scene_out = None;
        self.scene_timed = false;
        self.started = Cx::monotonic_now();
        self.gesture.clear();
        self.view = ViewState::default();
        self.view_moved = true;
        let assets = self.assets.clone();
        self.load(cx, docs, assets, Some(old));
    }

    /// The bed the engine's documents are of.
    pub fn bed(&self) -> u64 {
        self.bed
    }

    /// The bed's co-simulated chips' kept EEPROMs, as its status says them: taken once per bed
    /// (the runner's own writes come back in later statuses, and an older one must not undo a
    /// newer write).
    pub fn set_memory(&mut self, bed: u64, memory: Arc<fab_model::ElecMemory>) {
        if self.memory_bed != Some(bed) {
            self.memory_bed = Some(bed);
            self.firmware.set_memory(memory);
        }
    }

    /// The device memories the bed's runs changed, for the model host, with the bed they are of.
    pub fn take_memory(&mut self) -> Option<(u64, Arc<fab_model::ElecMemory>)> {
        self.firmware.take_memory().map(|memory| (self.bed, memory))
    }

    /// The runs' changed device memories go now, unsent ones included (the app quits, or a plate
    /// switch waits for them).
    pub fn flush_memory(&mut self) {
        self.firmware.flush_memory();
    }

    /// The runs' device memories have writes waiting to go to the model host.
    pub fn memory_pending(&self) -> bool {
        self.firmware.memory_pending()
    }

    /// This host's newest documents and its chips' memories go to the bed plate, then the model
    /// host holds a job that can replace the bed document while this host is busy or its chips'
    /// writes wait (the once-a-second send), and this host takes edits only while the bed is
    /// settled. In that order: such a job queues behind the documents and the memories (a held
    /// one makes the waiting writes go at once), so no edit or write for one plate reaches another
    /// or is lost to a switch. Called around every electronics input and once per event. Cost:
    /// two atomic loads and one short lock; a write also clones O(unconfirmed versions) Arcs.
    pub fn sync_model(&mut self, host: &mut crate::model_host::ModelHost) {
        let bed = self.bed();
        if let Some(write) = self.take_docs() {
            host.set_docs(bed, self.plate(), write);
        }
        if host.document_waits() {
            self.flush_memory();
        }
        if let Some((bed, memory)) = self.take_memory() {
            host.set_memory(bed, memory);
        }
        host.hold_document_changes(self.busy() || self.memory_pending());
        self.set_settled(host.document_settled(bed));
    }

    /// The running chip's EEPROM as the nominal run last published it.
    #[cfg(test)]
    pub(crate) fn published_eeprom(&self) -> Option<Arc<[u8]>> {
        let values = self.sim.as_ref()?.values()?;
        values.co.as_ref()?.get::<fab_elec_mcu_cosim::FwState>()?.eeprom.clone()
    }

    /// That bed's plate id, when it had one.
    pub fn plate(&self) -> Option<String> {
        self.plate.clone()
    }

    /// A command waits or a strand job runs: a job that can replace the bed document waits.
    pub fn busy(&self) -> bool {
        self.driver.busy()
    }

    /// The views are owed a scene of the bed's electronics: the bed is changing (or documents
    /// wait to replace the engine's), a command is in flight (its design may be a new one), the
    /// scene of the loaded design or of a newer one has not come, or a case change is on its way
    /// to the plate's scripts. O(1).
    pub fn scene_coming(&self) -> bool {
        !self.settled || self.outside.is_some() || self.busy()
            || self.scene_shown.is_none_or(|shown| shown < self.scene_seq)
            || self.case_task.as_ref().is_some_and(|task| !task.is_finished()) || self.env.is_some()
    }

    /// Whether edits are taken: false while the model's bed is changing (a job that can replace
    /// the bed document waits or runs, or the worker's bed is not yet this host's), so no edit
    /// meant for one plate's board reaches another.
    pub fn set_settled(&mut self, settled: bool) {
        if settled && !self.settled {
            self.edit_dropped = false;
        }
        self.settled = settled;
    }

    /// The newest changed documents and every version they may replace, once: O(versions) Arc
    /// clones. An outside change discards the pending write; restarting replaces that state.
    pub fn take_docs(&mut self) -> Option<DocsWrite> {
        let written = self.driver.take_docs()?;
        if self.outside.is_some() {
            return None;
        }
        let bases = self.versions.iter().cloned().collect();
        let newest = self.versions.back().expect("the engine has loaded documents");
        for name in newest.assets.iter().filter(|name| !written.assets.contains_key(*name)) {
            self.written_away.insert(name.clone());
        }
        self.written_away.retain(|name| !written.assets.contains_key(name));
        self.versions.push_back(Version { docs: written.docs.clone(), assets: written.assets.keys().cloned().collect() });
        Some(DocsWrite { docs: written.docs, assets: written.assets, bases })
    }

    /// The model worker took write `docs` (`HostAction::DocsLanded`, ordered after the statuses of
    /// earlier jobs): the versions written before it can no longer be on the bed, so they go. This
    /// keeps `versions` to the writes still in flight. Cost: O(versions) pointer compares.
    pub fn docs_landed(&mut self, bed: u64, docs: &Arc<fab_model::Docs>) {
        if bed != self.bed {
            return;
        }
        if let Some(i) = self.versions.iter().position(|version| Arc::ptr_eq(&version.docs, docs)) {
            drop(self.versions.drain(..i));
        }
    }

    /// Log once per bed change that an edit was not taken.
    fn drop_edit(&mut self, what: &str) {
        if !self.edit_dropped {
            self.edit_dropped = true;
            log!("electronics: {what} while another document goes on the bed is not taken");
        }
    }

    /// Scope and stimulus invalidate one lowering for the next frame, as for a bench edit.
    fn sync_bench(&mut self, cx: &mut Cx) {
        let (Some(sim), Some(snap)) = (self.sim.as_mut(), self.driver.snap()) else { return };
        let choice = cx.get_global_ref::<ElecLive>().and_then(|live| live.chosen_bench.as_ref());
        if self.sim_benches.as_ref().is_some_and(|(seen, name)| Arc::ptr_eq(seen, &snap.benches) && name.as_ref() == choice) {
            return;
        }
        self.sim_benches = Some((snap.benches.clone(), choice.cloned()));
        let name = choice.map(|s| s.as_ref());
        if let Some(bench) = fab_elec_view::snapshot::run_bench(&snap.benches, name) {
            sim.set_scope(Some(bench.out.scope.clone()));
            sim.set_take_len(bench.out.stop);
            sim.set_take_live(bench.out.live);
            sim.set_bench(bench.out.spec.clone());
            // the bench's expectations, judged on its runs (the Bench editor and elec.expect.*)
            sim.set_expects(bench.out.expects.clone());
            // Firmware follows the completed lowering after sim.frame below.
        } else {
            let failed = name.is_some_and(|name| snap.benches.iter().any(|b| b.out.name == name));
            if failed && self.resume_after_failure.is_none() {
                self.resume_after_failure = Some(sim.playing());
                sim.command(&SimCmd::Stop);
            }
            sim.set_scope(None);
            sim.set_take_len(None);
            sim.set_take_live(false);
            sim.set_bench(Default::default());
            sim.set_expects(Vec::new());
            if failed {
                cx.get_global::<ElecLive>().sim.stop(sim.status().into());
                return;
            }
            if !self.sim_no_bench_logged {
                log!("electronics: the design has no bench to run; using the default scope and stimulus");
                self.sim_no_bench_logged = true;
            }
        }
        if self.resume_after_failure.take() == Some(true) {
            sim.command(&SimCmd::Play);
        }
    }

    fn ensure_sim(&mut self) {
        if self.sim.is_none() {
            let mut sim = SimHost::new();
            // the firmware glue reads every lowering this host takes in: the host knows it from the
            // start, so a first lowering taken in by the step that asked for it (before the glue's
            // first frame) still waits for its firmware's first build
            sim.cosim_building(None, false);
            for (&id, rows) in &self.sim_probes {
                sim.act(&SimAction::Probes { id, rows: rows.clone() });
            }
            self.sim = Some(sim);
        }
    }

    /// An action batch's wants of the bench (the transport and shown scopes), then every SimAction and
    /// the shared time cursor (ViewAction::Cursor) to the bench, if it is made. The first want
    /// makes it (it lowers on the pool and plays), and the run it plays is the transport's from then
    /// on: a scope going off screen (another layout, another workspace) never pauses it, and the
    /// device in a 3D view plays on. Only Pause and Stop end it. A Stop ends the run and takes its
    /// state off every view (`SimLive::stop`); no later want plays it again until Play.
    fn bench_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let was = !self.sim_wants.is_empty();
        let mut stopped = false;
        for action in actions {
            match action.downcast_ref::<SimAction>() {
                Some(SimAction::ShowScope { id, .. }) => {
                    self.sim_wants.insert(*id);
                }
                Some(SimAction::HideScope { id }) => {
                    self.sim_wants.remove(id);
                }
                // a net being heard keeps the bench running, as a visible scope does
                Some(SimAction::Listen { id, .. }) => {
                    self.sim_wants.insert(*id);
                }
                Some(SimAction::Unlisten { id }) => {
                    self.sim_wants.remove(id);
                }
                Some(SimAction::Cmd(SimCmd::Play | SimCmd::Step(_) | SimCmd::StepFw(_) | SimCmd::SetRate(_))) => {
                    self.sim_wants.insert(TRANSPORT_WANT);
                }
                Some(SimAction::Cmd(SimCmd::Stop)) => {
                    self.sim_wants.remove(&TRANSPORT_WANT);
                    stopped = true;
                }
                Some(SimAction::Probes { id, rows }) => {
                    if rows.is_empty() {
                        self.sim_probes.remove(id);
                    } else {
                        self.sim_probes.insert(*id, rows.clone());
                    }
                }
                _ => {}
            }
        }
        let wanted = !self.sim_wants.is_empty();
        if wanted { self.ensure_sim(); }
        self.sync_bench(cx);
        let Some(sim) = self.sim.as_mut() else { return };
        if let Some(resume) = &mut self.resume_after_failure {
            // Keep scope sinks and their subscriptions across the failure, ready for the next bench.
            if !was && wanted && self.sim_resume {
                *resume = true;
                self.sim_wants.insert(TRANSPORT_WANT);
            }
            for action in actions.iter().filter_map(|a| a.downcast_ref::<SimAction>()) {
                match action {
                    SimAction::Cmd(SimCmd::Play) => *resume = true,
                    SimAction::Cmd(SimCmd::Stop | SimCmd::Pause | SimCmd::Hold) => *resume = false,
                    SimAction::Cmd(_) => {}
                    _ => sim.act(action),
                }
            }
            if stopped { self.sim_resume = false; }
            return;
        }
        let mut forwarded = actions.iter().any(|action| action.downcast_ref::<SimAction>().is_some()
            || matches!(action.downcast_ref::<ViewAction>(), Some(ViewAction::Cursor(_))));
        // Play first, so an explicit Pause or Step in this batch remains the last command.
        if !was && wanted && self.sim_resume {
            sim.command(&SimCmd::Play);
            self.sim_wants.insert(TRANSPORT_WANT);
            forwarded = true;
        }
        sim.handle_actions(actions);
        if stopped {
            self.sim_resume = false;
            if cx.has_global::<ElecLive>() {
                cx.get_global::<ElecLive>().sim.stop(sim.status().into());
            }
            forwarded = true;
        }
        if forwarded {
            cx.new_next_frame();
        }
    }

    /// Once per event, before widgets. True when the shown snapshot or item states changed.
    pub fn event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        if let Some(journal) = &self.journal { journal.plate().retry_worker(); }
        self.firmware.flash_event(cx, event);
        self.firmware.radio_event(cx, event);
        self.group_selected_in_batch = false;
        if let Event::Actions(actions) = event {
            for answer in actions.iter().filter_map(|action| action.downcast_ref::<crate::elec_mic::MicAnswer>()) {
                self.answer(Some(answer.call), answer.result.clone().map(|()| {
                    use fab_ai::out;
                    out::text(&out::obj(vec![("playing", out::flag(true))]))
                }));
            }
            for request in actions.iter().filter_map(|a| a.downcast_ref::<fab_elec_view::bom::Request>()) {
                use fab_elec_view::bom::Request;
                match request {
                    Request::Run(name, args) => self.run_action(cx, name, args.clone(), By::Person, None),
                    Request::Reload => {
                        self.read_rates(cx);
                        self.price_jobs.push_back((None, LibraryJob::Load));
                    }
                    Request::PreviewCsv(path) => {
                        cx.global::<fab_elec_view::bom::PricePanels>().draft = None;
                        self.price_jobs.push_back((None, LibraryJob::Preview(path.clone())));
                        cx.redraw_all();
                    }
                    Request::Import(draft, columns) => {
                        if cx.global::<fab_elec_view::bom::PricePanels>().draft.as_ref().is_some_and(|d| Arc::ptr_eq(d, draft)) {
                            cx.global::<fab_elec_view::bom::PricePanels>().draft = None;
                            self.price_jobs.push_back((None, LibraryJob::Import(draft.clone(), columns.clone())));
                        }
                    }
                    Request::Settings(settings) => self.price_jobs.push_back((None, LibraryJob::Settings(settings.clone()))),
                    Request::Review(id, approved) => {
                        if self.price_proposals.front().is_some_and(|(call, _)| call.reply == Some(*id)) {
                            let (call, proposal) = self.price_proposals.pop_front().unwrap();
                            if *approved { self.price_jobs.push_back((Some(call), LibraryJob::Save(proposal.accept()))); }
                            else { self.answer(Some(call), Err("Price declined".into())); }
                            self.show_price_review(cx);
                        }
                    }
                    Request::Group(group) => { self.bom_group = *group; self.bom_design = None; }
                    Request::Quantity(q) => { self.bom_quantity = *q; self.bom_design = None; }
                }
            }
            for press in actions.iter().filter_map(|a| a.downcast_ref::<StitchPress>()) {
                self.queue_action(cx, StitchPress::ACTION, press.args.clone(), By::Person, Some(EngineCaller::BoardEdit(press.tag)));
            }
            for press in actions.iter().filter_map(|a| a.downcast_ref::<BoardEditPress>()) {
                self.queue_action(cx, press.action, press.args.clone(), By::Person, Some(EngineCaller::BoardEdit(press.tag)));
            }
            // Board strip commands run as the person on the pane's board.
            for action in actions.iter().filter_map(|action| action.downcast_ref::<fab_elec_view::board::tools::BoardStripAction>()) {
                use fab_elec_view::board::tools::BoardStripAction;
                match action {
                    BoardStripAction::Tidy { unit, parts } => {
                        let Some(unit) = self.driver.snap().and_then(|snap| snap.design.unit(*unit)) else { continue };
                        let mut args = vec![("unit".into(), unit.name.clone())];
                        if !parts.is_empty() {
                            let Some(names) = parts.iter().map(|id| unit.circuit.comps.get(id.ix()).map(|c| c.refdes.as_str()))
                                .collect::<Option<Vec<_>>>() else { continue };
                            args.push(("parts".into(), names.join(",")));
                        }
                        self.queue_action(cx, "elec.place", args, By::Person, Some(EngineCaller::Tidy { bed: self.bed, unit: unit.id }));
                    }
                    BoardStripAction::Export(unit) => {
                        let name = self.driver.snap().and_then(|snap| snap.design.unit(*unit)).map(|unit| unit.name.clone());
                        self.run_action(cx, "elec.export", name.map(|name| ("board".to_string(), name)).into_iter().collect(), By::Person, None);
                    }
                    _ => {}
                }
            }
            for action in actions.iter().filter_map(|a| a.as_widget_action()?.action.downcast_ref::<ReqAction>()) {
                match action {
                    ReqAction::Slicer => cx.action(fab::ui::shell::ShowPanel(fab::ui::editors::Editor::Slicer)),
                    ReqAction::Run { action, args, bench } => {
                        if let Some(name) = bench {
                            let Some(live) = cx.get_global_ref::<ElecLive>() else { continue };
                            if !fab_elec_view::snapshot::run_bench(&live.snap.benches, Some(name)).is_some_and(|b| b.out.name == *name) { continue; }
                            let name: Arc<str> = Arc::from(name.as_str());
                            let project = live.title.clone();
                            cx.get_global::<ElecLive>().chosen_bench = Some(name.clone());
                            cx.action(fab_elec_view::live::transport::BenchChosen { project, name });
                            self.ensure_sim();
                            self.sync_bench(cx);
                            // Mark the chosen lowering wanted before the counted check can read the old one.
                            if let (Some(sim), Some(design)) = (self.sim.as_mut(), self.driver.sim_design()) {
                                sim.commit(design);
                            }
                            cx.new_next_frame();
                        }
                        self.run_action(cx, action, args.clone(), By::Person, None);
                    }
                }
            }
            // EXPECTATIONS' Check bench: the person's `elec.bench.run`; Stop check stops it
            for trace in actions.iter().filter_map(|action| action.downcast_ref::<fab_elec_view::live::transport::TraceAction>()) {
                use fab_elec_view::live::transport::TraceAction;
                match trace {
                    TraceAction::Save => self.run_action(cx, "elec.capture.save", Vec::new(), By::Person, None),
                    TraceAction::Open(path) => self.run_action(cx, "elec.capture.open", vec![("path".into(), path.clone())], By::Person, None),
                    TraceAction::Close => self.run_action(cx, "elec.capture.close", Vec::new(), By::Person, None),
                    TraceAction::List => self.list_traces(cx),
                }
            }
            for press in actions.iter().filter_map(|action| action.downcast_ref::<CheckBench>()) {
                match press {
                    CheckBench::Start => self.run_action(cx, fab_elec_simhost::actions::BENCH_RUN.name, Vec::new(), By::Person, None),
                    CheckBench::Stop => self.stop_check(cx),
                }
            }
            // an outside edit's sites flash, whoever sent it (a reload's diff, an AI commit)
            for edit in actions.iter().filter_map(|action| action.downcast_ref::<ElecOutsideEdit>()) {
                let now = Cx::monotonic_now();
                self.flashes.extend(edit.sites.iter().map(|site| (site.clone(), now)));
                self.view_moved |= !edit.sites.is_empty();
            }
            for action in actions.iter().rev() {
                let widget = action.as_widget_action();
                if let Some(group) = widget.and_then(|w| w.action.downcast_ref::<ViewState>()) {
                    self.view.clone_from(group);
                    self.view_moved = true;
                    self.group_selected_in_batch = true;
                    break;
                }
                let focus = widget.and_then(|w| w.action.downcast_ref::<ViewAction>()).or_else(|| action.downcast_ref::<ViewAction>());
                if matches!(focus, Some(ViewAction::Focus(_) | ViewAction::Unfocus)) { break; }
            }
            self.bench_actions(cx, actions);
            for erase in actions.iter().filter_map(|action| action.downcast_ref::<fab_elec_view::firmware::FirmwareErase>()) {
                if let Some(sim) = self.sim.as_mut() {
                    self.firmware.erase(cx, sim, &erase.0);
                }
            }
            for choose in actions.iter().filter_map(|action| action.downcast_ref::<fab_elec_view::firmware::FirmwareFlashChoose>()) {
                self.firmware.flash_choose(cx, &choose.0);
            }
            if actions.iter().any(|action| action.downcast_ref::<fab_elec_view::firmware::FirmwareFlash>().is_some()) {
                self.firmware.flash(cx);
            }
            if actions.iter().any(|action| action.downcast_ref::<fab_elec_view::firmware::FirmwareFlashRead>().is_some()) {
                self.firmware.flash_read(cx);
            }
            if actions.iter().any(|action| action.downcast_ref::<fab_elec_view::firmware::FirmwareBackup>().is_some()) {
                self.firmware.backup(cx);
            }
            if actions.iter().any(|action| action.downcast_ref::<fab_elec_view::firmware::FirmwareMonitor>().is_some()) {
                self.firmware.monitor(cx);
            }
            if actions.iter().any(|action| action.downcast_ref::<fab_elec_view::firmware::FirmwareRadioGet>().is_some()) {
                self.firmware.radio_get(cx);
            }
            if actions.iter().any(|action| action.downcast_ref::<fab_elec_view::firmware::FirmwareRadioCancel>().is_some()) {
                self.firmware.radio_cancel();
            }
        }
        if !self.typing.is_empty() && self.settled && self.outside.is_none() {
            for text in std::mem::take(&mut self.typing) {
                let _ = self.driver.act(text);
            }
        }
        if !self.held_calls.is_empty() || self.text_check.is_some() {
            self.release_calls(cx);
        }
        let mut publications = Vec::new();
        let mut changed = self.driver.drain(cx, &mut |cx, tx, message| {
            match message {
                Drained::Snapshot { snap, first } => {
                    if first {
                        self.doc_load_rev = snap.docs.as_ref().and_then(|d| d.texts.first()).map_or(0, |d| d.rev);
                        log!("electronics: the engine's first snapshot {:.0} ms after the host started", (Cx::monotonic_now() - self.started) * 1e3);
                        if let Some((mut edit, before)) = self.edit_pending.take() {
                            let changes = before.map(|before| fab_elec_view::live::edit_diff::design_changes(&before, &snap.design, &edit.lines)).unwrap_or_default();
                            for editor in changes.editors {
                                if !edit.editors.contains(&editor) {
                                    edit.editors.push(editor);
                                }
                            }
                            edit.label = if changes.label.is_empty() { edited_words(&edit.editors) } else { changes.label };
                            edit.sites = changes.sites;
                            // a change nothing shows (a comment in a block's definition) is no edit
                            if !edit.label.is_empty() {
                                log!("electronics: an outside edit ({:?}): {}, {} sites", edit.origin, edit.label, edit.sites.len());
                                cx.action(edit);
                            }
                        }
                    }
                    // The first committed generation can be zero too.
                    if first || snap.design.generation != self.case_seen {
                        let design = snap.design.clone();
                        let generation = design.generation;
                        self.case_seen = generation;
                        let tx = tx.clone();
                        let task = self.case.submit(Class::Background, TaskTicket::none(), move |bridge| {
                            if let Some(update) = bridge.cover(design) { tx.send(FromEngine::Case(update)); }
                        });
                        self.case_task = Some(task);
                    }
                    // The plate scene is the live design's, as the board drawing is
                    // (`publish_boards`): a dragged part's body moves with the preview, not at the
                    // drop. A drag's moved bodies are posed straight from its gesture (`act`); the
                    // scene with meshes, outline and holes comes from one scene_of on the pool
                    // at a time. A running one is never cancelled for a newer sample: it finishes
                    // and shows, and the newest live design asked for since starts the next
                    // after the drain (`start_scene`), so a drag faster than scene_of still lands
                    // a scene every scene_of.
                    let live = snap.live();
                    if self.scene_input.is_none() {
                        // The first snapshot's design is the loaded one: `start` builds its scene.
                        // No earlier plate's drag poses outlive it (an empty set clears them).
                        self.scene_input = Some(Arc::downgrade(live));
                        self.poses_out = Some(Arc::from([]));
                    } else if !self.scene_input.as_ref().is_some_and(|input| std::ptr::eq(input.as_ptr(), Arc::as_ptr(live))) {
                        self.scene_input = Some(Arc::downgrade(live));
                        self.scene_seq += 1;
                    }
                    // Release changed input mirrors while the old snapshot still owns them,
                    // so retiring it cannot leave the last board reference on the UI.
                    self.states_from.retain(|unit, (board, violations)| {
                        Arc::ptr_eq(violations, &snap.violations.value) && snap.live().units.iter().any(|u| {
                            u.id == *unit && u.board.as_ref().is_some_and(|b| Arc::ptr_eq(board, b))
                        })
                    });
                    self.sheet_states_from.retain(|unit, (index, violations)| {
                        Arc::ptr_eq(violations, &snap.violations.value) && snap.sheets.iter().any(|s| {
                            s.unit == Some(*unit) && Arc::ptr_eq(index, &s.value)
                        })
                    });
                }
                Drained::Host { msg, shown } => match msg {
                    FromEngine::FillWait(wait) => cx.action(wait),
                    FromEngine::Loaded { boards } => {
                        if !shown {
                            for board in boards {
                                self.drawn.insert(board.unit);
                                BoardDraws::set_board(cx, board, self.no_fills.clone());
                            }
                            log!("electronics: the loaded boards handed to the drawing {:.0} ms after the host started", (Cx::monotonic_now() - self.started) * 1e3);
                            return true;
                        }
                    }
                    FromEngine::Case(CaseUpdate::Set { generation, env }) => self.env = Some((generation, env)),
                    FromEngine::Case(CaseUpdate::Checked { run, commands, failed }) => publications.push((run, commands, failed)),
                    FromEngine::Scene { seq, scene, committed } => {
                        // One scene job at a time: when this was it, a newer live design asked for
                        // while it ran starts the next after the drain. (The loaded design's
                        // scene, seq 0, is no job of this host's.)
                        if self.scene_task.as_ref().is_some_and(|(running, _)| *running == seq) {
                            self.scene_task = None;
                        }
                        if self.scene_shown.is_some_and(|shown| seq <= shown) {
                            return false;
                        }
                        self.scene_shown = Some(seq);
                        if !self.scene_timed {
                            self.scene_timed = true;
                            log!("electronics: the plate's first scene {:.0} ms after the host started", (Cx::monotonic_now() - self.started) * 1e3);
                        }
                        self.scene_out = Some(scene);
                        self.scene_committed = committed;
                    }
                },
            }
            false
        });
        // A call's answer comes with the step that committed it; a first snapshot or a gesture's
        // end lets held calls go. O(calls waiting), nothing when none waits.
        self.note_generation();
        if !self.engine_calls.is_empty() || !self.read_calls.is_empty() || !self.export_calls.is_empty() {
            self.finish_calls(cx);
        }
        if !self.check_waits.is_empty() {
            self.finish_checks();
        }
        if self.check.is_some() {
            self.pump_check(cx);
        }
        if self.scope.started() {
            self.scope_pump(cx);
        }
        if !self.held_calls.is_empty() || self.text_check.is_some() {
            self.release_calls(cx);
        }
        while let Ok(set) = self.asset_rx.try_recv() {
            let mut added = fab_model::ElecAssets::new();
            if let Some(task) = self.asset_task.take() {
                task.detach();
            }
            for change in &set {
                if let DocSet::Asset { name, .. } = change {
                    if let Some(bytes) = self.pending_assets.remove(name) {
                        if self.journal.is_some() { added.insert(name.clone(), bytes); }
                    }
                }
            }
            if let Some(journal) = self.journal.clone().filter(|_| !added.is_empty()) {
                self.driver.on_strand(move || journal.assets_added(added));
            }
            self.driver.push(Cmd::Docs { set, label: "assets added".into(), by: By::default(), ticket: None });
        }
        if self.asset_task.is_none() && !self.pending_assets.is_empty() && self.outside.is_none() {
            self.start_assets(cx);
        }
        // The one scene job, for the newest live design asked for since the last one started.
        if self.scene_task.is_none() && self.scene_started < self.scene_seq {
            if let Some(snap) = self.driver.snap().cloned() {
                self.start_scene(cx, snap.live().clone(), snap.preview.is_none());
            }
        }
        // The drag is over once the engine's design has its edit (or is back without it): its
        // moved bodies take that design's poses at once (a refused commit puts them back too),
        // until that design's scene replaces them.
        if !self.gesture.is_empty() && !self.driver.previewing() {
            if let Some(snap) = self.driver.snap() {
                let still: Vec<_> = self.gesture.drain(..).map(|(unit, item)| (unit, item, P::new(0, 0))).collect();
                self.poses_out = Some(moved_poses(snap.live(), &still).into());
            }
        }
        if std::mem::take(&mut self.price_review_changed) { self.show_price_review(cx); }
        self.pump_prices(cx);
        if let Ok(rates) = self.rates_rx.try_recv() {
            self.rates = Some(rates);
            self.rates_task = None;
            self.bom_design = None;
        }
        let mut judged = false;
        while let Ok(newer) = self.req_rx.try_recv() {
            if cx.get_global_ref::<ElecLive>().is_some() {
                cx.get_global::<ElecLive>().requirements = newer.shown.clone();
                makepad_widgets::SignalToUI::set_ui_signal();
            }
            self.req_judged = Some(newer);
            judged = true;
        }
        if judged {
            self.finish_reqs();
        }
        for (run, commands, failed) in publications {
            self.req_app.case_run = Some(run);
            // A failed evaluation leaves the case rows stale, saying why, until one succeeds.
            self.req_app.case_failed = failed;
            for cmd in commands { self.driver.push(cmd); }
            self.rejudge(cx);
        }
        if let Some(words) = self.driver.take_place_notice() {
            self.notices.push((NoticeView::Board, Some(words)));
        }
        // A counted check is current while the lowering it ran on runs: judge again when it moves.
        let lowering = self.sim.as_ref().map(SimHost::generation);
        if lowering != self.req_lowering {
            self.req_lowering = lowering;
            if !self.req_benches.is_empty() {
                self.rejudge(cx);
            }
        }
        if self.outside.is_some() && self.driver.idle() {
            let docs = self.outside.take().expect("the bed's outside documents");
            self.restart(cx, docs);
            changed = true;
        }
        self.driver.kick();
        changed |= self.driver.mark_backlog();
        if changed {
            if let Some(snap) = self.driver.show(cx) {
                self.publish_boards(cx, &snap);
                self.judge_requirements(cx, &snap);
                #[cfg(debug_assertions)]
                self.check_row_owners(cx);
            }
        }
        // The committed design, as elec.bom.read reads: a drag's previews would rebuild the sheet
        // and the board facts on every step.
        if let Some(design) = self.driver.snap().map(|s| s.design.clone()) {
            if self.rates.is_some() && self.price_book.is_some() && self.bom_design.as_ref().is_none_or(|old| !Arc::ptr_eq(old, &design)) {
                self.build_bom(cx, design);
            }
        }
        if let Ok(shown) = self.bom_rx.try_recv() {
            if cx.has_global::<ElecLive>() {
                cx.get_global::<ElecLive>().bom = Some(shown);
                cx.redraw_all();
            }
        }
        // a flash fades a step each frame: every unit's states are written again until it ends
        let now = Cx::monotonic_now();
        self.flashes.retain(|(_, at)| now - at < fab_elec_view::firmware::FLASH_SECONDS);
        if !self.flashes.is_empty() {
            self.view_moved |= matches!(event, Event::NextFrame(_));
            cx.new_next_frame();
        }
        let flash_level = |at: f64| (1.0 - (now - at) / fab_elec_view::firmware::FLASH_SECONDS).clamp(0.0, 1.0) as f32;
        if changed || self.view_moved {
            if let Some(snap) = self.driver.snap() {
                let mut owners = HashSet::new();
                for unit in &snap.live().units {
                    let Some(board) = &unit.board else { continue };
                    owners.insert(unit.id);
                    let moved = self.states_from.get(&unit.id).is_none_or(|(was_board, was_violations)| {
                        !Arc::ptr_eq(was_board, board) || !Arc::ptr_eq(was_violations, &snap.violations.value)
                    });
                    if self.view_moved || moved {
                        let states = self.states.entry(unit.id).or_insert_with(ItemStates::new);
                        let mut flashed = Vec::new();
                        if !self.flashes.is_empty() {
                            let parts = part_rows(board);
                            let mut rows = Vec::new();
                            for (site, at) in &self.flashes {
                                board_rows(unit.id, board, &parts, site, &mut rows);
                                flashed.extend(rows.drain(..).map(|row| (row, flash_level(*at))));
                            }
                        }
                        states.write(board.next_item as usize, with_flash(board_texels(unit.id, board, &snap.violations.value, &self.view), &flashed));
                        changed |= states.is_dirty();
                        self.states_from.insert(unit.id, (board.clone(), snap.violations.value.clone()));
                    }
                }
                // A departed unit's row texture is released here: Drop has no Cx.
                self.states.retain(|unit, states| owners.contains(unit) || { states.release(cx); false });
                self.states_from.retain(|unit, _| owners.contains(unit));
                // Each unit once, with the rows of the state it shows: a circuit preview's own index
                // and circuit together (Fable H1), else the committed pair.
                let mut sheet_owners = HashSet::new();
                for unit in &snap.live().units {
                    let Some(sheet) = snap.of(&snap.sheets, unit.id) else { continue };
                    sheet_owners.insert(unit.id);
                    let index = &sheet.value;
                    let moved = self.sheet_states_from.get(&unit.id).is_none_or(|(was_index, was_violations)| {
                        !Arc::ptr_eq(was_index, index) || !Arc::ptr_eq(was_violations, &snap.violations.value)
                    });
                    if self.view_moved || moved {
                        let states = self.sheet_states.entry(unit.id).or_insert_with(ItemStates::new);
                        let mut flashed = Vec::new();
                        let mut rows = Vec::new();
                        for (site, at) in &self.flashes {
                            sheet_rows(unit.id, &unit.circuit, unit.board.as_deref(), &index.rows, site, &mut rows);
                            flashed.extend(rows.drain(..).map(|row| (row, flash_level(*at))));
                        }
                        states.write(index.count as usize, with_flash(sheet_texels(unit.id, &unit.circuit, unit.board.as_deref(), &index.rows, &snap.violations.value, &self.view), &flashed));
                        changed |= states.is_dirty();
                        self.sheet_states_from.insert(unit.id, (index.clone(), snap.violations.value.clone()));
                    }
                }
                self.sheet_states.retain(|unit, states| sheet_owners.contains(unit) || { states.release(cx); false });
                self.sheet_states_from.retain(|unit, _| sheet_owners.contains(unit));
                if cx.has_global::<ElecLive>() {
                    cx.get_global::<ElecLive>().states.retain(|(of, _)| match of {
                        StatesOf::Board(unit) => owners.contains(unit),
                        StatesOf::Schematic(unit) => sheet_owners.contains(unit),
                    });
                }
            }
            self.view_moved = false;
        }
        if matches!(event, Event::Draw(_) | Event::Signal | Event::NextFrame(_)) {
            self.sync_bench(cx);
            if let (Some(snap), Some(design), Some(sim)) = (self.driver.snap(), self.driver.sim_design(), self.sim.as_mut()) {
                // On signals and frames while a run is wanted, after ElecLive.snap is set: the bench notes a new
                // committed generation (or a value-only preview's: a value dragged is heard while it
                // moves), steps and uploads its rows into ElecLive.sim.
                let can_run = self.resume_after_failure.is_none();
                let waiting_for_bench = self.check.as_ref().is_some_and(|c| c.generation.is_none());
                if can_run && (!self.sim_wants.is_empty() || waiting_for_bench) && sim.frame(cx, design, matches!(event, Event::NextFrame(_))) {
                    cx.new_next_frame();
                }
                changed |= self.firmware.frame(cx, sim, snap, &self.assets, can_run);
                if !can_run {
                    // With no simulation frame, the output still needs to hush the stopped take.
                    fab_elec_view::live::listen::frame(cx);
                }
            }
        }
        // a transport call's batch went through `bench_actions` above; a wait is checked as the
        // run's status moves. Nothing when no call waits.
        if !self.run_calls.is_empty() && matches!(event, Event::Actions(actions)
            if actions.iter().any(|action| matches!(action.downcast_ref::<SimAction>(), Some(SimAction::Cmd(_)))))
        {
            self.finish_run_calls();
        }
        if !self.status_waits.is_empty() {
            self.note_generation();
            self.check_waits();
        }
        if !self.step_waits.is_empty() {
            self.check_steps();
        }
        if matches!(event, Event::Draw(_)) && cx.has_global::<ElecLive>() {
            let owners = self.states.iter_mut().map(|(&unit, states)| (StatesOf::Board(unit), states))
                .chain(self.sheet_states.iter_mut().map(|(&unit, states)| (StatesOf::Schematic(unit), states)));
            for (of, states) in owners {
                let missing = cx.get_global::<ElecLive>().item_states(of).is_none();
                if states.take_dirty() || missing {
                    let texture = states.texture(cx);
                    let live = cx.get_global::<ElecLive>();
                    if let Some((_, old)) = live.states.iter_mut().find(|(owner, _)| *owner == of) {
                        *old = texture;
                    } else {
                        live.states.push((of, texture));
                    }
                }
            }
        }
        changed
    }

    /// Hands every board of the live design (the preview while one is live) and its newest fills
    /// to the shared drawing; units gone from the design leave it. Views only read
    /// `BoardDraws::current`. The loaded boards are drawn first, without fills.
    fn publish_boards(&mut self, cx: &mut Cx, snap: &DesignSnapshot) {
        let mut shown = HashSet::with_capacity(self.drawn.len());
        for unit in &snap.live().units {
            let Some(board) = &unit.board else { continue };
            let fills = snap.of(&snap.fills, unit.id).map_or_else(|| self.no_fills.clone(), |t| t.value.clone());
            BoardDraws::set_board(cx, board.clone(), fills);
            shown.insert(unit.id);
        }
        for gone in self.drawn.difference(&shown) {
            BoardDraws::remove(cx, *gone);
        }
        self.drawn = shown;
    }

    /// Debug builds: every owner of live rows must be a pane or drawing this host holds. A board's
    /// copper (`Copper(unit)`) belongs to the shared drawing while the unit is drawn, a sheet's
    /// (`Sheet(unit, _)`, the Circuit tab's explorer) while the shown snapshot has the unit; any
    /// other is a stray: a pane dropped without its
    /// `release(cx)` (Drop has no Cx). A pane that registers rows adds its owners here when it
    /// comes to Fab. Cost O(owners) per shown snapshot in debug builds; nothing in release builds.
    #[cfg(debug_assertions)]
    fn check_row_owners(&self, cx: &mut Cx) {
        if !cx.has_global::<ElecLive>() {
            return;
        }
        let sim = &cx.get_global::<ElecLive>().sim;
        let strays: Vec<_> = sim.trees.owners().filter(|owner| match owner {
            fab_elec_view::live::segs::TreeOwner::Copper(unit) => !self.drawn.contains(unit),
            fab_elec_view::live::segs::TreeOwner::Sheet(unit, _) => !self.driver.snap().is_some_and(|snap| snap.design.unit(*unit).is_some()),
        }).collect();
        for owner in &strays {
            log!("electronics: live rows owned by {owner:?}, which this host no longer holds");
        }
        debug_assert!(strays.is_empty(), "live rows owned by {strays:?}, which this host no longer holds: a pane dropped without release(cx)?");
    }

    /// The model's new host inputs, once per moved board input.
    pub fn take_env(&mut self) -> Option<(u64, ScriptEnv)> {
        self.env.take()
    }

    /// True from a view's preview action until the engine's snapshot shows no preview after its
    /// commit or cancel: an edit is moving what the views draw.
    pub fn previewing(&self) -> bool {
        self.driver.previewing()
    }

    /// The live design's new plate scene (the preview's while one is live), once per completed
    /// scene task.
    pub fn take_scene(&mut self) -> Option<Arc<ElecScene>> {
        self.scene_out.take()
    }

    /// The parts the views have selected (`ViewState::selected`): a part, one of its pins, pads,
    /// body or ports, or its placement on a board. What the 3D views ring. O(selection).
    pub fn selected_parts(&self) -> Vec<CompRef> {
        let board = |unit: BoardId| self.driver.snap()
            .and_then(|snap| snap.live().units.iter().find(|u| u.id == unit))
            .and_then(|u| u.board.as_deref());
        self.view.selected.iter().filter_map(|sel| match sel {
            Sel::Comp(c) | Sel::Feature(FeatureRef::Part(c) | FeatureRef::Pad(c, _) | FeatureRef::Body(c) | FeatureRef::Port(c, _)) => Some(*c),
            Sel::Pin(p) => Some(CompRef { board: p.board, comp: p.comp }),
            Sel::Item(unit, item) => {
                let board = board(*unit)?;
                match board.locate(*item)? {
                    (ItemKind::Placed, i) => board.placed.get(i).map(|p| CompRef { board: *unit, comp: p.comp }),
                    _ => None,
                }
            }
            _ => None,
        }).collect()
    }

    /// The scene `take_scene` gave last was built from the committed design (no preview in it):
    /// once the 3D views have it prepared, it replaces a finished drag's poses.
    pub fn scene_committed(&self) -> bool {
        self.scene_committed
    }

    /// A drag's newest body poses (its moved parts', from the gesture; at its end, the engine's),
    /// once: what the 3D views move the bodies by before the design's scene lands. Empty: a load,
    /// no drag poses any more.
    pub fn take_poses(&mut self) -> Option<Arc<[(Sel, Xform3)]>> {
        self.poses_out.take()
    }

    /// The plate scene of `design`, the newest live design (`committed`: it has no preview in
    /// it), on the pool: the one scene job.
    fn start_scene(&mut self, cx: &mut Cx, design: Arc<Design>, committed: bool) {
        let seq = self.scene_seq;
        self.scene_started = seq;
        let tx = self.driver.sender();
        match cx.task_pool().submit_named(Class::Background, "elec scene for the plate", move || {
            let scene = Arc::new(scene_of(&design, &Pose::default()));
            drop(design);
            tx.send(FromEngine::Scene { seq, scene, committed });
        }) {
            Ok(task) => self.scene_task = Some((seq, task)),
            Err(error) => log!("electronics scene for the plate: {error}"),
        }
    }

    fn show_price_review(&self, cx: &mut Cx) {
        // Only proposals with a chat reply are queued (`release_calls`); one without is never shown.
        cx.global::<fab_elec_view::bom::PricePanels>().review = self.price_proposals.front().and_then(|(id, p)| {
            let entry = p.entry();
            Some((id.reply?, Arc::new(format!("Save {} for {}? {} · {} · {} · minimum {}, multiple {} · {}",
                entry.source.name, entry.key.label(), entry.break_label(), entry.currency, entry.source.date, entry.moq, entry.multiple,
                entry.source.url.as_deref().unwrap_or_default()))))
        });
        cx.redraw_all();
    }

    fn price_result(&mut self, cx: &mut Cx, call: Delivery, result: Result<LibraryOutput,String>) {
        match result {
            Ok(LibraryOutput::Snapshot(book)) => {
                self.price_book = Some(Ok(book));
                self.bom_design = None;
                self.answer(call, Ok("{ok: true}".into()));
            }
            Ok(LibraryOutput::Preview(draft)) => {
                cx.global::<fab_elec_view::bom::PricePanels>().draft = Some(draft);
                cx.redraw_all();
            }
            Err(why) => {
                if self.price_book.is_none() { self.price_book = Some(Err(why.clone())); self.bom_design = None; }
                cx.action(fab_elec_view::bom::EditResult(Err(why.clone())));
                self.answer(call, Err(why));
            }
        }
    }

    fn pump_prices(&mut self, cx: &mut Cx) {
        if let Ok(result) = self.price_rx.try_recv() {
            if let Some((call, _)) = self.price_running.take() { self.price_result(cx, call, result); }
        }
        if self.price_running.is_some() { return; }
        let Some((call, job)) = self.price_jobs.pop_front() else { return };
        let tx = self.price_rx.sender();
        if matches!(job, LibraryJob::Load) { self.price_book = None; }
        let current = self.price_book.clone();
        match cx.task_pool().submit_named(Class::Interactive, "BOM price library", move || {
            let dir = makepad_fab_settings::home::user_home().join(".makepad/fab/prices");
            let run = || {
                let snapshot = match job {
                    LibraryJob::Load => price_actions::read_library(&dir)?,
                    LibraryJob::Save(write) => price_actions::write_price(&dir, write)?,
                    LibraryJob::Settings(settings) => price_actions::write_settings(&dir, settings, &price_date())?,
                    LibraryJob::Preview(path) => {
                        let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
                        let book = current.ok_or("price library is loading")??;
                        let draft = PriceCsv::from_text(&text, &path, &price_date(), &book)?;
                        return Ok(LibraryOutput::Preview(Arc::new(draft)));
                    }
                    LibraryJob::Import(draft, columns) => {
                        let mut import = draft.confirm(&columns)?;
                        let date = price_date();
                        for entry in &mut import.entries { entry.source.date.clone_from(&date); }
                        price_actions::write_import(&dir, import)?
                    }
                };
                Ok(LibraryOutput::Snapshot(Arc::new(snapshot)))
            };
            let _ = tx.send(run());
        }) {
            Ok(task) => self.price_running = Some((call, task)),
            Err(e) => self.price_result(cx, call, Err(e.to_string())),
        }
    }

    fn read_rates(&mut self, cx: &mut Cx) {
        let rx = ToUIReceiver::default();
        let tx = rx.sender();
        self.rates = None;
        self.rates_rx = rx;
        if let Some(task) = self.rates_task.take() { task.cancel(); }
        match cx.task_pool().submit_named(Class::Interactive, "BOM rates", move || {
            use fab_elec_bom::Rates;
            let path = makepad_fab_settings::home::user_home().join(".makepad/fab/rates.json");
            let rates = match std::fs::read_to_string(path) {
                Ok(text) => Rates::from_json(&text).map_err(|e| format!("rates.json: {e}")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Rates::example()),
                Err(e) => Err(format!("rates.json: {e}")),
            };
            let _ = tx.send(rates.map(Arc::new));
        }) {
            Ok(task) => self.rates_task = Some(task),
            Err(e) => { self.rates = Some(Err(e.to_string())); self.bom_design = None; }
        }
    }

    /// Reuse sheets and measured geometry when just the quantity or the profile changed.
    /// Replacing the receiver prevents a superseded worker from publishing an older result.
    fn build_bom(&mut self, cx: &mut Cx, design: Arc<Design>) {
        use fab_elec_view::bom::{cost_text, Shown, UnitShown};
        let (Some(rates), Some(book)) = (self.rates.clone(), self.price_book.clone()) else { return };
        let previous = cx.get_global_ref::<ElecLive>().and_then(|live| live.bom.clone()).filter(|s| Arc::ptr_eq(&s.design, &design));
        let (grouped, quantity) = (self.bom_group, self.bom_quantity);
        let rx = ToUIReceiver::default();
        let tx = rx.sender();
        let requested = design.clone();
        let source_map = self.driver.snap().and_then(|snap| snap.source_map.clone());
        match cx.task_pool().submit_named(Class::Interactive, "board bill of materials", move || {
            let mut view = fab_elec_bom::SheetView::default();
            view.cam.group_symbols = grouped;
            let calls = std::cell::OnceCell::new();
            let mut units = Vec::new();
            for unit in &design.units {
                let Some(board) = &unit.board else { continue };
                if makepad_widgets::makepad_platform::thread::checkpoint().is_err() { return; }
                let cached = previous.as_ref().and_then(|s| s.units.iter().find(|u| u.unit == unit.id));
                let facts = cached.map(|u| u.facts.clone()).unwrap_or_else(|| Arc::new(fab_elec_bom::BoardFacts::from_board(board)));
                let shared = || source_map.as_ref().map(|map| calls.get_or_init(|| fab_elec_lang::splice::SharedCalls::new(&design, map)));
                let grouped_sheet = cached.map(|u| u.grouped_sheet.clone()).unwrap_or_else(|| {
                    let mut sheet = fab_elec_bom::sheet(&design, unit.id, &Default::default()).expect("the board unit being visited");
                    if let Some(shared) = shared() {
                        let lines = fab_elec_host::bom_actions::GroupedLines::new(shared, unit.id, &sheet);
                        lines.annotate(&design, unit.id, shared, &mut sheet, true);
                    }
                    Arc::new(sheet)
                });
                let sheet = if grouped {
                    grouped_sheet.clone()
                } else if let Some(u) = cached.filter(|_| previous.as_ref().is_some_and(|s| !s.grouped)) {
                    u.sheet.clone()
                } else {
                    let mut sheet = fab_elec_bom::sheet(&design, unit.id, &view).expect("the board unit being visited");
                    if let Some(shared) = shared() {
                        let lines = fab_elec_host::bom_actions::GroupedLines::new(shared, unit.id, &grouped_sheet);
                        lines.annotate(&design, unit.id, shared, &mut sheet, false);
                    }
                    Arc::new(sheet)
                };
                let placements = cached.map(|u| u.placements.clone()).unwrap_or_else(|| Arc::new(price_actions::assembly_placements(board,&unit.circuit)));
                let mut settings = Default::default();
                let mut prepared = None;
                let estimate = (|| {
                    let rates = rates.as_ref().map_err(String::clone)?;
                    let book = book.as_ref().map_err(String::clone)?;
                    settings = book.settings.clone();
                    let options = settings.options()?;
                    let previous = cached.filter(|u| u.book.as_ref().is_some_and(|b| Arc::ptr_eq(b,book))
                        && u.rates.as_ref().is_some_and(|r| Arc::ptr_eq(r,rates))).and_then(|u| u.prepared.clone());
                    let plan = previous.unwrap_or_else(|| Arc::new(fab_elec_bom::cost::Prepared::new(&grouped_sheet, &placements, &book.prices, rates, &options)));
                    let estimate = plan.estimate(&facts,rates,quantity,&options);
                    prepared = Some(plan);
                    Ok(estimate)
                })();
                let (cost, details, prices) = cost_text(estimate, &grouped_sheet, &sheet, &price_date());
                units.push(UnitShown { unit: unit.id, name: unit.name.clone(), sheet, grouped_sheet, facts, placements,
                    cost, details, prepared, rates: rates.as_ref().ok().cloned(), prices: Arc::new(prices), book: book.as_ref().ok().cloned(), settings });
            }
            let _ = tx.send(Arc::new(Shown { design, units, grouped, quantity }));
        }) {
            Ok(task) => {
                if let Some(old) = self.bom_task.replace(task) { old.cancel(); }
                self.bom_design = Some(requested);
                self.bom_rx = rx;
            }
            Err(e) => log!("electronics BOM: {e}"),
        }
    }

    /// Judge the project's requirements against a newly shown snapshot on the pool, latest wins
    /// (`ElecLive::requirements`); a project without a requirements document clears them at once.
    /// At most one job per shown snapshot. O(documents) here; the facts and the judging,
    /// O(requirements text + units x parts + findings), on the pool.
    fn judge_requirements(&mut self, cx: &mut Cx, snap: &Arc<DesignSnapshot>) {
        let (seq, checks) = (commit_seq(snap), fab_elec_host::edit_actions::checks_current(snap));
        if !has_requirements(snap) && snap.profile_diags.is_empty() {
            if let Some(task) = self.req_task.take() {
                task.cancel();
            }
            self.req_rx = ToUIReceiver::default();
            if cx.get_global_ref::<ElecLive>().is_some_and(|live| live.requirements.is_some()) {
                cx.get_global::<ElecLive>().requirements = None;
                makepad_widgets::SignalToUI::set_ui_signal();
            }
            self.req_judged = Some(ReqJudged { seq, checks, shown: None });
            self.finish_reqs();
            return;
        }
        let snap = snap.clone();
        let app = self.req_facts();
        let rx = ToUIReceiver::default();
        let tx = rx.sender();
        match cx.task_pool().submit_named(Class::Interactive, "elec requirements", move || {
            use fab_elec_host::req_actions::{app_facts, shown};
            let docs = snap.docs.clone().unwrap_or_default();
            let mut judged = shown(&docs, &app_facts(&snap, &app));
            if !snap.profile_diags.is_empty() {
                judged.get_or_insert_with(Default::default).mistakes.extend(profile_mistakes(&snap));
            }
            if makepad_widgets::makepad_platform::thread::checkpoint().is_ok() {
                let _ = tx.send(ReqJudged { seq, checks, shown: judged.map(Arc::new) });
            }
        }) {
            Ok(task) => {
                self.req_rx = rx;
                if let Some(old) = self.req_task.replace(task) {
                    old.cancel();
                }
            }
            Err(error) => log!("electronics requirements: {error}"),
        }
    }

    /// Judge the requirements again against the shown snapshot, after one of the app's own facts
    /// moved (a case run asked for, a counted check, the lowering, the slice figures).
    fn rejudge(&mut self, cx: &mut Cx) {
        if let Some(snap) = self.driver.snap().cloned() {
            self.judge_requirements(cx, &snap);
        }
    }

    /// The app's facts for the requirements as of now: a counted check is current while its
    /// lowering runs. O(benches).
    fn req_facts(&self) -> fab_elec_host::req_actions::AppFacts {
        let lowering = self.sim.as_ref().map(SimHost::generation);
        let benches = self.req_benches.iter().map(|(name, ran_on, run)| {
            (name.clone(), fab_elec_req::BenchRun { current: run.current && lowering == Some(*ran_on), ..run.clone() })
        }).collect();
        fab_elec_host::req_actions::AppFacts { benches, ..self.req_app.clone() }
    }

    /// The whole plate's slice figures (seconds, grams) as the print state holds them, None while
    /// the plate has none of its own: on every event, judging again only when they change (the
    /// last figures stay, stale, once the plate moves on). O(1).
    pub fn set_print(&mut self, cx: &mut Cx, figures: Option<(f64, f64)>) {
        let print = match (figures, &self.req_app.print) {
            (Some((seconds, grams)), Some(held)) if held.current && held.seconds == seconds && held.grams == grams => return,
            (Some((seconds, grams)), _) => fab_elec_req::PrintFacts { current: true, at: Cx::time_now(), seconds, grams },
            (None, Some(held)) if held.current => fab_elec_req::PrintFacts { current: false, ..held.clone() },
            (None, _) => return,
        };
        self.req_app.print = Some(print);
        self.rejudge(cx);
    }

    /// A model reply, including a CAD edit under the same frozen board inputs. The
    /// strand matches it to the design those inputs cover, including across reloads.
    pub fn case_reply(&mut self, inputs: &InputId, stamp: Option<u64>, reply: CaseReply) {
        let tx = self.driver.sender();
        let inputs = inputs.clone();
        self.case_task = Some(self.case.submit(Class::Background, TaskTicket::none(), move |bridge| {
            if let Some(update) = bridge.reply(&inputs, stamp, reply) { tx.send(FromEngine::Case(update)); }
        }));
    }

    /// A drag's moved bodies at the committed design's poses again: O(moved parts).
    fn pose_back(&mut self) {
        if let Some(snap) = self.driver.snap().filter(|_| !self.gesture.is_empty()) {
            let still: Vec<_> = self.gesture.iter().map(|(unit, item)| (*unit, *item, P::new(0, 0))).collect();
            self.poses_out = Some(moved_poses(&snap.design, &still).into());
        }
    }

    /// A drag's moved bodies at their dragged poses now: O(moved parts).
    fn pose_moves(&mut self, moves: &[(BoardId, ItemId, P)]) {
        let Some(snap) = self.driver.snap().filter(|_| !moves.is_empty()) else { return };
        self.poses_out = Some(moved_poses(&snap.design, moves).into());
        for (unit, item, _) in moves {
            if !self.gesture.contains(&(*unit, *item)) {
                self.gesture.push((*unit, *item));
            }
        }
    }

    /// A view's action: one mailbox push (or UI-only state).
    pub fn act(&mut self, cx: &mut Cx, action: ViewAction) {
        let taken = self.settled && self.outside.is_none();
        match &action {
            ViewAction::Planned { call, action: inner } => {
                let why = match inner.as_ref() {
                    ViewAction::PlanRefused(why) => Some(why.as_str()),
                    _ if !taken => Some("another document is going on the bed: the edit was not applied"),
                    _ => None,
                };
                if let Some(why) = why {
                    let dest = self.begin_call(None, By::Person, call.name, || crate::journal::arguments(&call.args));
                    self.answer(dest, Err(why.to_owned()));
                    if !taken {
                        if let ViewAction::Land(tag) = inner.as_ref() { self.driver.refuse_landing(*tag, why); }
                        self.drop_edit("an edit");
                    }
                    return
                }
            }
            ViewAction::Route { unit, nets, method, reroute } => {
                let job = fab_elec_host::route_action(self.design().map(Arc::as_ref), *unit, nets, *method, *reroute);
                return self.queue_board_job(cx, job);
            }
            ViewAction::Place { unit, parts, all, free_panel } => {
                let job = fab_elec_host::place_action(self.design().map(Arc::as_ref), *unit, parts, *all, *free_panel);
                return self.queue_board_job(cx, job);
            }
            ViewAction::Ops { .. } | ViewAction::Preview { .. } if !taken => {
                self.drop_edit("an edit");
                return;
            }
            // Typing waits until edits are taken again, then goes to the engine, which answers it:
            // it applies, or it is refused (a later load refuses an earlier one's revisions).
            ViewAction::Text { .. } if !taken => {
                self.typing.push(action);
                return;
            }
            // A preview started before the bed began changing is refused under its tag, so the
            // view gets its one answer; its dragged bodies go back at once.
            ViewAction::CommitPreview if !taken => {
                self.drop_edit("an edit");
                self.pose_back();
                self.driver.refuse_preview("another document is going on the bed: the edit was not applied");
                return;
            }
            // A drag (moves only, over the committed design) poses its moved bodies now, from
            // the gesture: the 3D bodies follow every step, ahead of the engine's sample.
            // O(moves), through the board's item index.
            ViewAction::Preview { ops, .. } => {
                let moves: Vec<_> = ops.iter().filter_map(|op| match op {
                    DesignOp::Board { unit, op: BoardOp::Move { item, by } } => Some((*unit, *item, *by)),
                    _ => None,
                }).collect();
                if moves.len() == ops.len() {
                    self.pose_moves(&moves);
                }
            }
            // A part or via drag the route worker stretches poses its moved bodies from each
            // sample, as a plain move's preview does. O(moved).
            ViewAction::Gesture(sample) if matches!(sample.gesture.kind, GestureKind::DragParts | GestureKind::DragVia(_)) => {
                let unit = sample.gesture.unit;
                let moves: Vec<_> = sample.gesture.moved.iter().map(|(item, x)| (unit, *item, x.by)).collect();
                self.pose_moves(&moves);
            }
            // A cancelled drag's bodies go back at once.
            ViewAction::CancelPreview => self.pose_back(),
            _ => {}
        }
        match self.driver.act(action) {
            Some(ViewAction::Focus(f)) if !self.group_selected_in_batch => {
                self.view = if f.kind == FocusKind::Net {
                    ViewState { selected: vec![], emphasis: vec![(f.sel, 1.0)] }
                } else {
                    ViewState { selected: vec![f.sel], emphasis: vec![] }
                };
                self.view_moved = true;
            }
            Some(ViewAction::Unfocus) if !self.group_selected_in_batch => {
                self.view = ViewState::default();
                self.view_moved = true;
            }
            _ => {}
        }
        // Hover stays in pane uniforms; the time cursor reaches the bench through `event`
        // (SimHost::handle_actions), which sees every action.
    }

    fn queue_board_job(&mut self, cx: &mut Cx, job: Option<(&str, Vec<(String, String)>)>) {
        if let Some((name, args)) = job {
            // Keep the previous job's partial now, even when the queue is waiting on its preview.
            self.driver.keep_job();
            self.queue_action(cx, name, args, By::Person, None);
        }
    }

    /// Run a catalog action (`fab_elec_design::action`) as `by`: the one path the AI's tools and
    /// a person's command share, not an AI adapter. A read is answered from the committed design
    /// on the pool; an edit goes to the engine (`Driver::invoke`), which plans it with the
    /// action's plan function and commits it as one undo step, exactly as the matching pane edit.
    /// An edit waits while edits are not taken (the bed changing, an outside change waiting) and
    /// while a person's gesture is live, so it never ends their drag; a call made for another
    /// bed is refused when it would go. `call` is the chat call to answer (None: nobody waits);
    /// every call is answered once (`take_answers`). A tool commit sends its caller's cue.
    /// Cost on the UI: O(args) for the check and one mailbox push or pool submit; nothing per
    /// frame.
    pub fn run_action(&mut self, cx: &mut Cx, name: &str, args: Vec<(String, String)>, by: By, call: Option<u64>) {
        self.queue_action(cx, name, args, by, call.map(EngineCaller::Chat));
    }

    fn queue_action(&mut self, cx: &mut Cx, name: &str, args: Vec<(String, String)>, by: By, caller: Option<EngineCaller>) {
        let reply = match caller { Some(EngineCaller::Chat(id)) => Some(id), _ => None };
        let call = self.begin_call(reply, by, name, || crate::journal::arguments(&args));
        self.dispatch_action(cx, name, args, by, caller, call);
    }

    pub(crate) fn tool_action(&mut self, cx: &mut Cx, name: &str, args: Vec<(String, String)>, id: u64, work: &crate::journal::Work) {
        let call = self.tool_delivery(id, work);
        self.dispatch_action(cx, name, args, work.by, Some(EngineCaller::Chat(id)), call);
    }

    fn dispatch_action(&mut self, cx: &mut Cx, name: &str, args: Vec<(String, String)>, by: By, caller: Option<EngineCaller>, call: Delivery) {
        let Some(def) = fab_elec_design::action::find(name) else {
            let why = format!("no electronics action {name}");
            if let Some(EngineCaller::BoardEdit(tag)) = caller { cx.action(BoardEditDone { tag, refusal: Some(why.clone()) }); }
            return self.answer(call, Err(why));
        };
        if let Err(diag) = def.check_args(&args) {
            if let Some(EngineCaller::BoardEdit(tag)) = caller { cx.action(BoardEditDone { tag, refusal: Some(diag.message.clone()) }); }
            return self.refuse_form(cx, call, add_form(name, by, &args), diag.message);
        }
        if let Some(EngineCaller::Tidy { unit, .. }) = caller {
            if !cx.global::<Tidying>().boards.insert(unit) { return self.answer(call, Err("this board is already being tidied".into())); }
            cx.redraw_all();
        }
        self.held_calls.push_back(ActionCall { name: def.name, args, by, call, bed: self.bed, caller, text: None });
        self.release_calls(cx);
    }

    /// Change a document's text as `by` (`fab.elec_text`), as typing in its code pane does: the
    /// change is found in the text of the revision it names, a firmware document's changed text
    /// is compiled with the running bench's inputs first, and one engine text step commits it,
    /// one undo step. It waits behind the calls before it, and the calls after it wait for it.
    /// Cost on the UI: O(1) to hold; the search and the compile run on the pool.
    #[cfg(test)]
    pub fn run_text(&mut self, cx: &mut Cx, ask: TextAsk, by: By, call: Option<u64>) {
        let caller = call.map(EngineCaller::Chat);
        let call = self.begin_call(call, by, ELEC_TEXT, || crate::journal::text_args(&ask));
        self.queue_text(cx, ask, by, caller, call);
    }

    pub(crate) fn tool_text(&mut self, cx: &mut Cx, ask: TextAsk, id: u64, work: &crate::journal::Work) {
        let call = self.tool_delivery(id, work);
        self.queue_text(cx, ask, work.by, Some(EngineCaller::Chat(id)), call);
    }

    fn queue_text(&mut self, cx: &mut Cx, ask: TextAsk, by: By, caller: Option<EngineCaller>, call: Delivery) {
        self.held_calls.push_back(ActionCall { name: ELEC_TEXT, args: Vec::new(), by, call, bed: self.bed, caller, text: Some(ask) });
        self.release_calls(cx);
    }

    /// A held call cancelled by its caller is answered so and dropped; one already in the engine
    /// answers with what happened. True when it was held.
    pub fn cancel_call(&mut self, call: u64) -> bool {
        if let Some(at) = self.price_proposals.iter().position(|(id, _)| id.reply == Some(call)) {
            let (dest, _) = self.price_proposals.remove(at).unwrap();
            self.cancelled(Some(dest), "Price proposal cancelled");
            self.price_review_changed = true;
            return true;
        }
        let Some(at) = self.held_calls.iter().position(|held| reply(held.call) == Some(call)) else { return false };
        let dest = self.held_calls.remove(at).unwrap().call;
        self.cancelled(dest, "cancelled before it was applied");
        true
    }

    /// The steps the AI's calls committed since the last take, with the chat turn each belongs to.
    pub fn take_turn_steps(&mut self) -> Vec<(u64, u64)> {
        std::mem::take(&mut self.turn_steps)
    }

    /// The calls finished since the last take: the call, whether it did what it was asked, and
    /// its answer's text.
    pub fn take_answers(&mut self) -> Vec<(u64, bool, String)> {
        std::mem::take(&mut self.answers)
    }

    fn refuse_form(&mut self, cx: &mut Cx, call: Delivery, form: Option<&str>, why: String) {
        if let Some(name) = form {
            cx.action(AddRefused { name: name.into(), why: why.clone() });
        }
        self.answer(call, Err(why));
    }

    fn begin_call(&mut self, reply: Option<u64>, by: By, name: &str, args: impl FnOnce() -> makepad_strict_json::Value) -> Delivery {
        let call = self.journal.as_ref().map(|journal| crate::journal::Call::begin(journal.plate(), by, name, args()));
        self.delivery(reply, call.map(crate::journal::Binding::new), false)
    }

    fn tool_delivery(&mut self, id: u64, work: &crate::journal::Work) -> Delivery {
        self.delivery(Some(id), work.journal.clone(), true)
    }

    fn delivery(&mut self, reply: Option<u64>, call: Option<Arc<crate::journal::Binding>>, outer: bool) -> Delivery {
        let journal = call.map(|call| {
            self.next_journal_call += 1;
            self.journal_calls.insert(self.next_journal_call, call);
            self.next_journal_call
        });
        (reply.is_some() || journal.is_some()).then_some(Recipient { reply, journal, outer })
    }

    fn bind_call(&self, call: Delivery) -> Option<(crate::journal::Electronics, fab_elec_host::Ticket)> {
        let journal = self.journal.as_ref()?;
        let call = self.journal_calls.get(&call?.journal?)?;
        let ticket = self.driver.next_ticket();
        journal.bind(ticket, &call.0);
        Some((journal.clone(), ticket))
    }

    fn finish_answer(&mut self, dest: Recipient, outcome: &'static str, text: String) {
        if let Some(call) = dest.journal.and_then(|id| self.journal_calls.remove(&id)) {
            if !dest.outer { call.0.done(outcome, text.clone()); }
        }
        if let Some(id) = dest.reply { self.answers.push((id, outcome == "ok", text)); }
    }

    fn answer(&mut self, call: Delivery, result: Result<String, String>) {
        if let Some(dest) = call {
            match result {
                Ok(text) => self.finish_answer(dest, "ok", text),
                Err(error) => self.finish_answer(dest, "refused", format!("{{error: {}}}", fab_ai::out::quote(&error))),
            }
        }
    }

    fn cancelled(&mut self, call: Delivery, reason: &str) {
        if let Some(dest) = call { self.finish_answer(dest, "cancelled", format!("{{error: {}}}", fab_ai::out::quote(reason))); }
    }

    /// Whether an AI edit that answers with `checks` may go to the engine now: every check has run on
    /// the latest commit and no other such edit waits for its checks, so the findings it diffs
    /// against are the checks' own and its diff is its own (`answer_edit`). O(engine calls).
    fn checks_quiet(&self, snap: &DesignSnapshot) -> bool {
        fab_elec_host::edit_actions::checks_current(snap) && self.check_waits.is_empty()
            && !self.engine_calls.iter().any(|call| call.before.is_some())
    }

    /// Whether an AI edit may go to the engine as far as the requirements go: the project has
    /// none, or those shown were judged on the latest commit and no other edit waits for its own,
    /// so the rows its answer compares against are the ones its commit starts from
    /// (`req_before`). O(engine calls + waits).
    fn reqs_quiet(&self, snap: &DesignSnapshot) -> bool {
        !has_requirements(snap) || (self.req_judged.as_ref().is_some_and(|judged| judged.seq == commit_seq(snap))
            && self.check_waits.iter().all(|wait| wait.reqs.is_none()) && !self.engine_calls.iter().any(|call| call.reqs.is_some()))
    }

    /// The requirement rows an AI edit's answer compares its commit against: those shown now.
    /// None when nobody hears the answer, or when the project has no requirements and the edit
    /// writes none. O(requirements).
    fn req_before(&self, heard: bool, writes: bool) -> Option<Vec<fab_elec_host::req_actions::ReqMark>> {
        if !heard {
            return None;
        }
        match self.req_judged.as_ref().and_then(|judged| judged.shown.as_ref()) {
            Some(shown) => Some(fab_elec_host::req_actions::marks(&shown.report)),
            None => writes.then(Vec::new),
        }
    }

    /// An edit's answer, catalog action or text change alike: sent now, or held. With `before` (an
    /// AI edit of what the checks read, sent when they were quiet) until every check has run on its
    /// commit `seq`, when `finish_checks` adds `checks`; with `reqs` (an AI edit of a project with
    /// requirements, sent when they were quiet) until they are judged on it, when `finish_reqs`
    /// adds `requirements`. O(answer fields).
    fn answer_edit(&mut self, call: Delivery, seq: u64, fields: Vec<(&str, fab_ai::out::S)>, notes: Vec<(String, String)>, before: Option<Vec<u64>>,
        reqs: Option<Vec<fab_elec_host::req_actions::ReqMark>>) {
        use fab_ai::out;
        let mut fields: Vec<_> = fields.into_iter().map(|(key, value)| (key.to_owned(), value)).collect();
        fields.extend(notes.into_iter().map(|(key, value)| {
            let field = if matches!(key.as_str(), "case" | "undo" | "case_plan") { out::s(value) } else { out::S::Verbatim(value) };
            (key, field)
        }));
        if before.is_none() && reqs.is_none() {
            self.answer(call, Ok(out::text(&out::S::Object(None, fields))));
            return;
        }
        let checks = before.is_some();
        self.check_waits.push(CheckWait { call, seq, fields, before, reqs, checks });
        // the requirements judged on this commit may be in already
        self.finish_reqs();
    }

    /// Start the held calls that can go now, in arrival order: one that must wait keeps the ones
    /// behind it waiting, so a read after an edit reads what the edit left.
    fn release_calls(&mut self, cx: &mut Cx) {
        loop {
            // A text change on its check goes to the engine before anything behind it.
            if self.text_check.is_some() && !self.send_text(cx) {
                return;
            }
            // A Scope tool's call on its worker: what comes after it waits for its answer.
            if self.scope.busy() {
                return;
            }
            let Some(next) = self.held_calls.front() else { return };
            // A call queued behind the first Add must see the newly committed document.
            if self.engine_calls.iter().any(|c| c.name == fab_elec_host::req_actions::REQ_ADD.name) { return; }
            if next.bed != self.bed {
                let held = self.held_calls.pop_front().expect("a front call");
                if let Some(EngineCaller::BoardEdit(tag)) = held.caller { cx.action(BoardEditDone { tag, refusal: Some("the plate changed before the board edit".into()) }); }
                self.refuse_form(cx, held.call, add_form(held.name, held.by, &held.args), "another plate went on the bed while the call waited: nothing was applied".into());
                continue;
            }
            // The take's save (`elec.capture.save` without a capture) is the host's: the record
            // and the lanes are here, the writer the tools' own.
            if next.name == "elec.capture.save" && !next.args.iter().any(|(key, _)| key == "capture") {
                let held = self.held_calls.pop_front().expect("a front call");
                self.save_take(cx, held);
                continue;
            }
            // and so is the way back from a saved trace (its opening is the tools worker's)
            if next.name == "elec.capture.close" {
                let held = self.held_calls.pop_front().expect("a front call");
                self.close_trace(cx, held);
                continue;
            }
            if scope::is_tool(next.name) {
                let held = self.held_calls.pop_front().expect("a front call");
                self.scope_call(cx, held);
                continue;
            }
            let Some(snap) = self.driver.snap() else { return };
            if SIM_ACTIONS.iter().any(|def| def.name == next.name) {
                let held = self.held_calls.pop_front().expect("a front call");
                self.sim_call(cx, held);
                continue;
            }
            // A part's model as its card reads it (with where its part's e.body is written), and
            // this computer's choice for it as the card sets it: one part's lookups, answered at
            // once; the app keeps a choice and asks for the model again.
            use fab_elec_host::model_actions::{model_from, model_read, MODEL_FROM, MODEL_READ};
            if next.name == MODEL_FROM.name || next.name == MODEL_READ.name {
                let held = self.held_calls.pop_front().expect("a front call");
                let answer = if held.name == MODEL_READ.name {
                    model_read(&snap.design, snap.source_map.as_deref(), &held.args)
                } else {
                    model_from(&snap.design, &held.args)
                };
                self.answer(held.call, answer.map_err(|diag| diag.message));
                continue;
            }
            if let Some(read) = read_of(next.name) {
                let held = self.held_calls.pop_front().expect("a front call");
                let design = snap.design.clone();
                self.pool_read(cx, held.call, move || read(&design, &held.args).map_err(|diag| diag.message));
                continue;
            }
            // The board's files, from the committed board once its fills are exact; and their check.
            if next.name == EXPORT || next.name == EXPORT_VALIDATE {
                if snap.stale.has(fab_elec_view::StageFlags::EVAL) || snap.stale.has(fab_elec_view::StageFlags::ENGINE)
                    || snap.stale.has(fab_elec_view::StageFlags::FILLS) {
                    return;
                }
                let held = self.held_calls.pop_front().expect("a front call");
                let snap = snap.clone();
                self.export_call(cx, held, snap);
                continue;
            }
            // What the Checks pane shows: the snapshot's findings.
            if next.name == fab_elec_host::edit_actions::CHECK_LIST.name || next.name == "elec.net.length" {
                let held = self.held_calls.pop_front().expect("a front call");
                let snap = snap.clone();
                let read = if held.name == "elec.net.length" { fab_elec_host::Engine::net_length } else { fab_elec_host::edit_actions::check_list };
                self.pool_read(cx, held.call, move || read(&snap, &held.args).map_err(|diag| diag.message));
                continue;
            }
            if next.name == price_actions::PRICE_SET.name {
                let held = self.held_calls.pop_front().unwrap();
                match price_actions::price_request(&held.args, held.by, &price_date()) {
                    Ok(PriceRequest::Person(write)) => self.price_jobs.push_back((held.call, LibraryJob::Save(write))),
                    Ok(PriceRequest::Review(proposal)) => {
                        if let Some(id) = held.call.filter(|id| id.reply.is_some()) {
                            self.price_proposals.push_back((id, proposal));
                            self.show_price_review(cx);
                        } else {
                            // Only a chat call can be approved in the review; any other shape is
                            // refused where the person sees it, never assumed away.
                            let why = "a price proposal came without a chat call to answer, so it was not saved".to_string();
                            log!("elec: {why}");
                            cx.action(fab_elec_view::bom::EditResult(Err(why.clone())));
                            self.answer(held.call, Err(why));
                        }
                    }
                    Err(why) => self.price_result(cx, held.call, Err(why)),
                }
                continue;
            }
            if next.name == fab_elec_host::bom_actions::COST_READ.name {
                if self.price_running.is_some() || !self.price_jobs.is_empty() { return; }
                let Some(book) = self.price_book.clone() else { return };
                let Some(rates) = self.rates.clone() else { return };
                let held = self.held_calls.pop_front().expect("a front call");
                let design = snap.design.clone();
                self.pool_read(cx, held.call, move || {
                    let book = book?;
                    price_actions::cost_read(&design, rates?.as_ref(), &book, &book.settings.options()?, &held.args).map_err(|d| d.message)
                });
                continue;
            }
            // The requirements, held against what the design and its checks measure.
            if next.name == fab_elec_host::req_actions::REQ_LIST.name {
                let held = self.held_calls.pop_front().expect("a front call");
                let (snap, app) = (snap.clone(), self.req_facts());
                self.pool_read(cx, held.call, move || {
                    use fab_elec_host::req_actions::{app_facts, req_list};
                    let docs = snap.docs.clone().unwrap_or_default();
                    req_list(&docs, &app_facts(&snap, &app), &held.args).map_err(|diag| diag.message)
                });
                continue;
            }
            // The Library panel's entries.
            if next.name == fab_elec_host::sheet_actions::LIBRARY_LIST.name {
                let held = self.held_calls.pop_front().expect("a front call");
                let catalog = snap.catalog.clone();
                self.pool_read(cx, held.call, move || fab_elec_host::sheet_actions::library_list(catalog.as_deref(), &held.args).map_err(|diag| diag.message));
                continue;
            }
            // The documents as the code panes hold them.
            use fab_elec_host::doc_actions::{doc_list, doc_read, DOC_LIST, DOC_READ};
            if next.name == DOC_LIST.name || next.name == DOC_READ.name {
                let held = self.held_calls.pop_front().expect("a front call");
                let docs = snap.docs.clone().unwrap_or_default();
                let read = if held.name == DOC_LIST.name { doc_list } else { doc_read };
                self.pool_read(cx, held.call, move || read(&docs, &held.args).map_err(|diag| diag.message));
                continue;
            }
            if next.name == FLASH_ASK.name {
                let held = self.held_calls.pop_front().expect("a front call");
                let answer = self.flash_ask(cx, held.by);
                self.answer(held.call, answer);
                continue;
            }
            if !self.settled || self.outside.is_some() || self.driver.previewing() {
                return;
            }
            // A tool edit's answer says what it did to the requirements: it starts from rows judged
            // on the latest commit, and from no other edit's.
            if reply(next.call).is_some() && next.by.is_tool() && !self.reqs_quiet(snap) {
                return;
            }
            if next.name == ELEC_TEXT {
                let held = self.held_calls.pop_front().expect("a front call");
                let snap = snap.clone();
                self.text_call(cx, held, &snap);
                continue;
            }
            // A requirement's add, remove, waive or unwaive: its document's new text, sent on as
            // that document's text step (one undo step; refused when it adds a mistake).
            // O(requirements text) here.
            if fab_elec_host::req_actions::is_req_edit(next.name) {
                let mut held = self.held_calls.pop_front().expect("a front call");
                let docs = snap.docs.clone().unwrap_or_default();
                let person = !held.by.is_tool();
                match fab_elec_host::req_actions::req_edit(&docs, held.name, &held.args, person) {
                    Ok(fab_elec_host::req_actions::ReqEdit::Text { doc, rev, text }) => {
                        held.text = Some(TextAsk { doc, rev, change: fab_elec_host::doc_actions::TextChange::Whole(text) });
                        let snap = snap.clone();
                        self.text_call(cx, held, &snap);
                    }
                    Ok(fab_elec_host::req_actions::ReqEdit::New { doc, text }) => {
                        let lines = vec![0..text.lines().count()];
                        let said = if person { Vec::new() } else { fab_elec_host::req_actions::requirements_changed("", &text) };
                        let reqs = self.req_before(reply(held.call).is_some() && !person, true);
                        let (tx, rx) = std::sync::mpsc::channel();
                        let binding = self.bind_call(held.call);
                        self.driver.docs(vec![DocSet::Add { name: doc.clone(), text: text.into() }], format!("Add {doc}"), held.by, tx);
                        let text = TextDone { form_name: add_form(held.name, held.by, &held.args).map(str::to_owned),
                            doc, rev: self.doc_load_rev, lines, compiled: Ok(()), id: None, evaluated: None, said };
                        self.engine_calls.push(EngineCall { dest: held.call, binding, call: held.caller, name: held.name, rx, text: Some(text), scope: None, before: None, reqs });
                    }
                    Err(why) => self.refuse_form(cx, held.call, add_form(held.name, held.by, &held.args), why),
                }
                continue;
            }
            let checks = reply(next.call).is_some() && next.by.is_tool()
                && fab_elec_design::action::find(next.name).is_some_and(|def| def.answers_checks());
            if checks && !self.checks_quiet(snap) {
                return;
            }
            let held = self.held_calls.pop_front().expect("a front call");
            let before = checks.then(|| shown_findings(snap));
            let reqs = self.req_before(reply(held.call).is_some() && held.by.is_tool(), false);
            let (tx, rx) = std::sync::mpsc::channel();
            let binding = self.bind_call(held.call);
            self.driver.invoke(held.name.to_string(), held.args, false, held.by, tx);
            let call = held.caller;
            self.engine_calls.push(EngineCall { dest: held.call, binding, call, name: held.name, rx, text: None, scope: None, before, reqs });
        }
    }

    /// Start a text call's check on the pool (`fab.elec_text`): its document by name, at the
    /// revision it read: firmware, circuit, blocks, bench, part or requirements. The change is found
    /// in that revision's text. A firmware document's new text is compiled as the running bench
    /// builds it; with nothing here to build it, it is committed unchecked and the answer says so.
    /// A requirements document's is read, refused when it adds a mistake, and never evaluated. Any
    /// other goes to the engine to be evaluated first, and only text that adds no mistake is committed.
    /// `release_calls` sends it on. O(documents) on the UI thread.
    fn text_call(&mut self, cx: &mut Cx, held: ActionCall, snap: &DesignSnapshot) {
        use fab_elec_host::doc_actions::{find_doc, find_edits, kind_word};
        let form_name = add_form(held.name, held.by, &held.args).map(str::to_owned);
        let refused = |host: &mut Self, cx: &mut Cx, why| host.refuse_form(cx, held.call, form_name.as_deref(), why);
        let ask = held.text.expect("a text call holds its change");
        let Some(docs) = snap.docs.as_deref() else {
            return refused(self, cx, "the plate's electronics documents are not loaded".into());
        };
        let doc = match find_doc(docs, &ask.doc) {
            Ok(doc) => doc.clone(),
            Err(why) => return refused(self, cx, why),
        };
        use fab_elec_lang::DocKind;
        let kind = DocKind::of_name(&doc.name);
        let evaluate = match kind {
            Some(DocKind::Firmware | DocKind::Requirements) => false,
            Some(DocKind::Circuit | DocKind::Blocks | DocKind::Bench | DocKind::Part) => true,
            _ => return refused(self, cx, format!("{} is a {} document: fab.elec_text changes firmware, circuit, blocks, bench, part and requirements documents; fab.elec_edit's actions change the rest",
                doc.name, kind_word(&doc.name))),
        };
        if doc.rev != ask.rev {
            return refused(self, cx, format!("{} changed since rev {}: it is at rev {} now; read it again with elec.doc.read", doc.name, ask.rev, doc.rev));
        }
        let checks = reply(held.call).is_some() && held.by.is_tool()
            && matches!(kind, Some(DocKind::Circuit | DocKind::Blocks | DocKind::Part));
        let check = (kind == Some(DocKind::Firmware)).then(|| self.firmware.check_input(&doc.name));
        let requirements = kind == Some(DocKind::Requirements);
        // The person approves a waiver. Tools can propose one.
        let tool = held.by.is_tool();
        let (name, rev, base) = (doc.name.clone(), doc.rev, doc.text.clone());
        let for_form = form_name.is_some();
        let job = move || -> Result<TextChecked, String> {
            let refuse = |why: String| if for_form { why } else { format!("{{error: {}}}", fab_ai::out::quote(&why)) };
            let found = find_edits(&base, &ask.change).map_err(refuse)?;
            if requirements {
                fab_elec_host::req_actions::check_text(doc.doc, &name, &base, &found.text, tool).map_err(refuse)?;
            }
            let said = if requirements && tool { fab_elec_host::req_actions::requirements_changed(&base, &found.text) } else { Vec::new() };
            let compiled = match check {
                None => Ok(()),
                Some(Ok(mut input)) => {
                    input.text = Arc::from(found.text.as_str());
                    input.rev = rev + 1;
                    let started = std::time::Instant::now();
                    let built = fab_elec_fw::compile(&input);
                    log!("electronics text: the check's build of {name} took {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
                    if let Err(diags) = built {
                        return Err(compile_refusal(&name, rev, &found.text, &diags));
                    }
                    Ok(())
                }
                Some(Err(why)) => Err(why),
            };
            // One replacement, as the pane's typing sends: one revision on, one undo step.
            let edit = fab_elec_design::TextEdit::trimmed(doc.doc, &base, &found.text);
            let evaluated = evaluate.then(|| Arc::from(found.text.as_str()));
            Ok(TextChecked { edit, lines: found.lines, compiled, evaluated, said })
        };
        let (tx, rx) = to_ui_oneshot();
        match cx.task_pool().submit_named(Class::Interactive, "elec text check", move || { let _ = tx.send(job()); }) {
            Ok(task) => {
                task.detach();
                self.text_check = Some(TextCheck { form_name, call: held.call, by: held.by, checks, bed: held.bed, doc: doc.doc, name: doc.name, rev, rx, result: None });
            }
            Err(error) => refused(self, cx, format!("the check could not start: {error}; nothing was committed")),
        }
    }

    /// The text change on its check, once its result is here: refused (its answer), or sent to
    /// the engine as one text step when edits are taken. True when it is done with and the calls
    /// behind it may go. O(edits).
    fn send_text(&mut self, cx: &mut Cx) -> bool {
        let taken = self.settled && self.outside.is_none() && !self.driver.previewing();
        let snap = self.driver.snap();
        let quiet = snap.is_some_and(|snap| self.checks_quiet(snap));
        let bed = self.bed;
        let Some(check) = self.text_check.as_mut() else { return true };
        if check.result.is_none() {
            check.result = Some(match check.rx.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) if check.form_name.is_some() => Err("The check stopped before it answered.".into()),
                Err(TryRecvError::Disconnected) => Err("{error: \"the check stopped before it answered: nothing was committed\"}".into()),
            });
        }
        if check.bed == bed && (!taken || (check.checks && !quiet)) && matches!(check.result, Some(Ok(_))) {
            return false;
        }
        let check = self.text_check.take().expect("a text check");
        if check.bed != bed {
            self.refuse_form(cx, check.call, check.form_name.as_deref(), "another plate went on the bed while the call waited: nothing was applied".into());
            return true;
        }
        match check.result.expect("a checked text change") {
            Err(why) if check.form_name.is_some() => self.refuse_form(cx, check.call, check.form_name.as_deref(), why),
            Err(text) => {
                if let Some(call) = check.call {
                    self.finish_answer(call, "refused", text);
                }
            }
            Ok(checked) => {
                let before = check.checks.then(|| shown_findings(snap.expect("quiet checks have a snapshot")));
                let writes = fab_elec_lang::DocKind::of_name(&check.name) == Some(fab_elec_lang::DocKind::Requirements);
                let reqs = self.req_before(reply(check.call).is_some() && check.by.is_tool(), writes);
                let (tx, rx) = std::sync::mpsc::channel();
                let label = format!("Edit {}", check.name);
                let binding = self.bind_call(check.call);
                if checked.evaluated.is_some() {
                    self.driver.checked_text(check.doc, check.rev, vec![checked.edit], label, check.by, tx);
                } else {
                    self.driver.text(check.doc, check.rev, vec![checked.edit], label, check.by, tx);
                }
                let text = TextDone {
                    form_name: check.form_name, doc: check.name, rev: check.rev, lines: checked.lines, compiled: checked.compiled, id: Some(check.doc), evaluated: checked.evaluated,
                    said: checked.said,
                };
                self.engine_calls.push(EngineCall { dest: check.call, binding, call: reply(check.call).map(EngineCaller::Chat), name: ELEC_TEXT, rx, text: Some(text), scope: None, before, reqs });
            }
        }
        true
    }

    /// `elec.flash.ask`: what the Flash to row shows, with the Firmware pane brought forward for
    /// the person (Follow AI and the area's outline, as an AI edit's cue). Refused when the
    /// running image cannot go to a board. O(boards).
    fn flash_ask(&mut self, cx: &mut Cx, by: By) -> Result<String, String> {
        use fab_ai::out;
        use fab_elec_view::live::sim::FlashState;
        let fw = cx.get_global_ref::<ElecLive>().and_then(|live| live.sim.firmware.clone());
        let Some(fw) = fw else {
            return Err("no firmware runs on the bench: Flash to sends the running image, so play a bench that runs firmware first".into());
        };
        let Some(flash) = fw.flash.as_ref() else {
            return Err(match fw.running_rev {
                None => format!("{} has no running image yet: Flash to sends it once it is built", fw.doc),
                Some(_) => format!("{}'s image cannot be flashed from Fab: Flash to sends an ESP32-S3's image", fw.doc),
            });
        };
        let boards = flash.devices.iter().map(|device| {
            let mut fields = vec![("port", out::s(device.path.clone())), ("name", out::s(device.label.clone())), ("may", out::flag(device.off.is_none()))];
            if let Some(why) = &device.off {
                fields.push(("why", out::s(why.clone())));
            }
            out::line(out::obj(fields))
        });
        let state = match &flash.state {
            FlashState::Idle => "idle".to_string(),
            FlashState::Flashing(step) => format!("flashing: {step}"),
            FlashState::Done(text) => text.clone(),
            FlashState::Failed(why) => format!("flash failed: {why}"),
            FlashState::Reading(step) => format!("reading the board: {step}"),
            FlashState::ReadFailed(why) => format!("read failed: {why}"),
            FlashState::BackingUp(step) => format!("backing up the board: {step}"),
            FlashState::BackupFailed(why) => format!("back-up failed: {why}"),
        };
        cx.action(ElecOutsideEdit {
            origin: EditOrigin::tool(by).unwrap_or(EditOrigin::Elsewhere),
            label: "asks you to flash: choose the board under Flash to".into(),
            sites: vec![], editors: vec![ElecEditor::Firmware], lines: vec![],
        });
        Ok(out::text(&out::obj(vec![
            ("doc", out::s(fw.doc.clone())),
            ("part", out::s(fw.part.clone())),
            ("rev", out::opt(fw.running_rev.map(out::int))),
            ("boards", out::arr(boards)),
            ("chosen", out::opt(flash.chosen.clone().map(out::s))),
            ("hint", out::opt(flash.hint.clone().map(out::s))),
            ("state", out::s(state)),
            ("asked", out::s("The Firmware pane shows Flash to: the person chooses the board and presses Flash.")),
        ])))
    }

    /// The saved project's file (None: never saved, or a demo), whose folder exports go beside.
    /// O(path) when it changes, a compare otherwise.
    pub fn set_project(&mut self, file: Option<&std::path::Path>) {
        if self.project.as_deref() != file {
            self.project = file.map(std::path::Path::to_path_buf);
            // the traces' writer reads it when it writes
            *self.scope.project.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = self.project.clone();
            self.scope.left_project = true;
        }
    }

    /// Drain the views' notices. A missing message clears the notice in its view.
    pub fn take_notices(&mut self) -> Vec<(NoticeView, Option<String>)> {
        std::mem::take(&mut self.notices)
    }

    /// `elec.export` or `elec.export.validate`, on the pool from the committed snapshot: the
    /// board, its profile (the call's, the board's own, else the standard one) and its fills.
    fn export_call(&mut self, cx: &mut Cx, held: ActionCall, snap: Arc<DesignSnapshot>) {
        use crate::elec_export;
        let arg = |name: &str| held.args.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str());
        let root = elec_export::root(self.project.as_deref());
        let design = snap.design.clone();
        let job: Box<dyn FnOnce() -> Result<elec_export::Exported, String> + Send> = if held.name == EXPORT {
            let unit = match fab_elec_host::edit_actions::board_unit(&design, "board", arg("board")) {
                Ok(unit) => unit,
                Err(diag) => return self.export_failed(held.call, diag.message),
            };
            let folder = elec_export::Folder::Project(elec_export::folder_of(&root, &unit.name));
            let job = elec_export::ExportJob::from_snapshot(&snap, unit, arg("profile"), folder);
            Box::new(move || elec_export::export(job))
        } else {
            let last = match arg("folder") {
                Some(folder) => {
                    let folder = match elec_export::relative_folder(&root, folder) {
                        Ok(folder) => folder,
                        Err(why) => return self.export_failed(held.call, why),
                    };
                    let unit = match fab_elec_host::edit_actions::board_unit(&design, "board", None) {
                        Ok(unit) => unit,
                        Err(diag) => return self.export_failed(held.call, diag.message),
                    };
                    let profile = unit.factory.clone().unwrap_or_else(|| fab_elec_cam::PROFILES[0].to_string());
                    elec_export::LastExport { unit: unit.id, profile, folder }
                }
                None => match &self.last_export {
                    Some(last) => last.clone(),
                    None => return self.export_failed(held.call, "folder: nothing was exported since the plate opened; name the folder".into()),
                },
            };
            Box::new(move || elec_export::validate(&snap, &last))
        };
        let (tx, rx) = to_ui_oneshot();
        match cx.task_pool().submit_named(Class::Interactive, "elec export", move || { let _ = tx.send(job()); }) {
            Ok(task) => {
                task.detach();
                self.export_calls.push((held.call, rx));
            }
            Err(error) => self.export_failed(held.call, format!("the export could not start: {error}")),
        }
    }

    /// An export that did not happen: its caller's answer, and the board notice.
    fn export_failed(&mut self, call: Delivery, why: String) {
        self.notices.push((NoticeView::Board, Some(format!("Board files not written: {why}"))));
        self.answer(call, Err(why));
    }

    /// Run a read on the pool and answer `call` with it once it comes back (`finish_calls`).
    fn pool_read(&mut self, cx: &mut Cx, call: Delivery, read: impl FnOnce() -> Result<String, String> + Send + 'static) {
        let (tx, rx) = to_ui_oneshot();
        match cx.task_pool().submit_named(Class::Interactive, "elec action read", move || { let _ = tx.send(read()); }) {
            Ok(task) => {
                task.detach();
                if let Some(call) = call {
                    self.read_calls.push((call, rx));
                }
            }
            Err(error) => self.answer(call, Err(format!("the read could not start: {error}"))),
        }
    }

    /// Answer the calls whose outcome or read came back, and send the cue of each AI commit:
    /// its label and what it changed, from the commit itself (`Outcome::Done`), never a diff.
    fn finish_calls(&mut self, cx: &mut Cx) {
        let mut at = 0;
        while at < self.engine_calls.len() {
            let outcome = match self.engine_calls[at].rx.try_recv() {
                Ok(outcome) => Ok(outcome),
                Err(TryRecvError::Empty) => {
                    at += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => Err(()),
            };
            let EngineCall { dest, binding, call, name, text, scope, before, reqs, .. } = self.engine_calls.remove(at);
            if let Some((journal, ticket)) = binding { journal.unbind(ticket); }
            let call = match call {
                Some(EngineCaller::Chat(_)) => dest,
                Some(EngineCaller::BoardEdit(tag)) => {
                    let refusal = match &outcome {
                        Ok(Outcome::Done { seq, .. }) => {
                            self.done_seq = self.done_seq.max(*seq);
                            None
                        }
                        Ok(Outcome::Refused(r)) => Some(r.text.clone()),
                        _ => Some("the board closed before the edit finished".into()),
                    };
                    self.answer(dest, refusal.clone().map_or_else(|| Ok("{ok: true}".into()), Err));
                    cx.action(BoardEditDone { tag, refusal });
                    continue;
                }
                Some(EngineCaller::Tidy { bed, unit }) => {
                    if bed == self.bed && cx.global::<Tidying>().boards.remove(&unit) { cx.redraw_all(); }
                    dest
                }
                None => dest,
            };
            if name.starts_with("elec.bom.") && reply(call).is_none() {
                let result = match &outcome {
                    Ok(Outcome::Done { label, .. }) => Ok(label.clone()),
                    Ok(Outcome::Refused(refusal)) => Err(refusal.text.clone()),
                    _ => Err("BOM edit did not complete".into()),
                };
                cx.action(fab_elec_view::bom::EditResult(result));
            }
            if let Some(name) = text.as_ref().and_then(|t| t.form_name.as_ref()) {
                let why = match &outcome {
                    Ok(Outcome::Refused(r)) => Some(r.text.as_str()),
                    Ok(Outcome::Previewed { .. }) => Some("The Add became a preview."),
                    Err(()) => Some("The plate changed before the requirement was added."),
                    Ok(Outcome::Done { .. }) => None,
                };
                if let Some(why) = why { cx.action(AddRefused { name: name.clone(), why: why.into() }); }
            }
            if let Some(edit) = scope {
                match &outcome {
                    Ok(Outcome::Done { seq, sites, .. }) => self.scope_done(*seq, sites.clone(), edit),
                    Ok(Outcome::Refused(refusal)) => edit.refuse(refusal.text.clone()),
                    Ok(Outcome::Previewed { .. }) => edit.refuse("the edit became a preview".into()),
                    Err(()) => edit.refuse("the plate's electronics loaded again: nothing was applied".into()),
                }
            }
            match outcome {
                Ok(Outcome::Done { seq, label, sites, created, by, circuit, notes, .. }) if text.is_some() => {
                    self.done_seq = self.done_seq.max(seq);
                    if let By::Ai { turn, .. } = by {
                        self.turn_steps.push((turn, seq));
                    }
                    let text = text.expect("a text change");
                    let made = self.made(seq, &created);
                    if let Some(origin) = EditOrigin::tool(by) {
                        // The pane washes one range: from the first edit's lines to the last's.
                        let lines = text.lines.first().zip(text.lines.last()).map(|(first, last)| (text.doc.clone(), first.start..last.end));
                        let mut sites = sites;
                        sites.extend(created.into_iter().filter(|sel| !matches!(sel, Sel::Source { .. })));
                        cx.action(ElecOutsideEdit {
                            origin, label: fab_elec_host::edit_actions::cue_words(&label), sites,
                            editors: ElecEditor::of_doc(&text.doc).into_iter().collect(), lines: lines.into_iter().collect(),
                        });
                    }
                    use fab_ai::out;
                    let lines = text.lines.iter().map(|lines| out::line(out::arr([out::int(lines.start + 1), out::int(lines.end)])));
                    let mut fields = vec![
                        ("seq", out::int(seq)), ("label", out::s(label)), ("doc", out::s(text.doc)), ("rev", out::int(text.rev + 1)),
                        ("lines", out::arr(lines)),
                    ];
                    if !text.said.is_empty() {
                        fields.push(("requirements_changed", out::arr(text.said.into_iter().map(out::s))));
                    }
                    if text.id.is_none() {
                        self.answer_edit(call, seq, fields, notes, before, reqs);
                        continue;
                    }
                    // A build of the new text replaces the running image, which starts the run over;
                    // an evaluated document's change moves the run as typing it does.
                    let firmware_built = text.evaluated.is_none() && text.compiled.is_ok();
                    let (state, effect) = self.take_effect(if firmware_built { CircuitChange::Structure } else { circuit });
                    let take = out::obj(vec![("state", out::name(state)), ("effect", out::name(effect))]);
                    if text.evaluated.is_some() {
                        fields.push(("evaluated", out::flag(true)));
                        if !made.is_empty() {
                            fields.push(("made", out::arr(made.into_iter().map(out::s))));
                        }
                    } else {
                        fields.push(("compiled", out::flag(text.compiled.is_ok())));
                        if let Err(why) = text.compiled {
                            fields.push(("why", out::s(format!("not compiled, not checked: {why}"))));
                        }
                    }
                    fields.push(("take", take));
                    self.answer_edit(call, seq, fields, notes, before, reqs);
                }
                Ok(Outcome::Done { seq, label, created, by, targets, circuit, notes, .. }) => {
                    self.done_seq = self.done_seq.max(seq);
                    if let By::Ai { turn, .. } = by {
                        self.turn_steps.push((turn, seq));
                    }
                    let made = self.made(seq, &created);
                    if let Some(origin) = EditOrigin::tool(by) {
                        let mut sites = targets;
                        for sel in created {
                            if !sites.contains(&sel) {
                                sites.push(sel);
                            }
                        }
                        let editors = site_editors(&sites);
                        cx.action(ElecOutsideEdit { origin, label: fab_elec_host::edit_actions::cue_words(&label), sites, editors, lines: vec![] });
                    }
                    use fab_ai::out;
                    let (state, effect) = self.take_effect(circuit);
                    let take = out::obj(vec![("state", out::name(state)), ("effect", out::name(effect))]);
                    let mut fields = vec![("seq", out::int(seq)), ("label", out::s(label)), ("take", take)];
                    if !made.is_empty() {
                        fields.push(("made", out::arr(made.into_iter().map(out::s))));
                    }
                    if name == fab_elec_host::edit_actions::ROUTE.name {
                        fields.push(("route", self.route_summary()));
                    }
                    self.answer_edit(call, seq, fields, notes, before, reqs);
                }
                // A text change the engine refused (the document moved on, another load) says
                // what to do next in the AI's words.
                Ok(Outcome::Refused(refusal)) => match &text {
                    Some(text) if text.evaluated.is_some() && refusal.text.starts_with(fab_elec_host::MISTAKE) => {
                        let answer = self.mistake_refusal(text, &refusal);
                        if let Some(call) = call {
                            self.finish_answer(call, "refused", answer);
                        }
                    }
                    Some(text) => self.answer(call, Err(format!("{}: nothing was committed; read {} again with elec.doc.read", refusal.text, text.doc))),
                    None => self.answer(call, Err(refusal.text.clone())),
                },
                Ok(Outcome::Previewed { .. }) => self.answer(call, Err("the edit became a preview, which this path never asks for".into())),
                Err(()) => self.answer(call, Err("the plate's electronics loaded again: nothing was applied".into())),
            }
        }
        let mut at = 0;
        while at < self.export_calls.len() {
            let result = match self.export_calls[at].1.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => {
                    at += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => Err("the export stopped before it answered".into()),
            };
            let (call, _) = self.export_calls.remove(at);
            match result {
                Ok(done) => {
                    if done.last.is_some() {
                        self.last_export = done.last;
                    }
                    if let Some(notice) = done.notice {
                        self.notices.push((NoticeView::Board, Some(notice)));
                    }
                    self.answer(call, Ok(done.answer));
                }
                Err(why) => self.export_failed(call, why),
            }
        }
        let mut at = 0;
        while at < self.read_calls.len() {
            match self.read_calls[at].1.try_recv() {
                Ok(result) => {
                    let (call, _) = self.read_calls.remove(at);
                    self.answer(Some(call), result);
                }
                Err(TryRecvError::Empty) => at += 1,
                Err(TryRecvError::Disconnected) => {
                    let (call, _) = self.read_calls.remove(at);
                    self.answer(Some(call), Err("the read stopped before it answered".into()));
                }
            }
        }
    }

    /// A checked text change the engine refused for a mistake it adds: nothing was committed, and
    /// where the first mistake is (line and column from 1, in the changed text when it is in the
    /// changed document), with the others' words. O(text).
    fn mistake_refusal(&self, text: &TextDone, refusal: &fab_elec_view::Refusal) -> String {
        use fab_ai::out;
        let message = refusal.text.strip_prefix(fab_elec_host::MISTAKE).unwrap_or(&refusal.text);
        let mut row = Vec::new();
        if let Some(&Sel::Source { doc, start, .. }) = refusal.site.as_ref() {
            let other = (Some(doc) != text.id).then(|| self.driver.snap().and_then(|snap| snap.docs.as_ref()
                .and_then(|docs| docs.texts.iter().find(|held| held.doc == doc).map(|held| (held.name.clone(), held.text.clone()))))).flatten();
            let located = match other {
                Some((name, held)) => Some((name, line_col(&held, start as usize))),
                None if Some(doc) == text.id => text.evaluated.as_ref().map(|new| (text.doc.clone(), line_col(new, start as usize))),
                None => None,
            };
            if let Some((name, (line, col))) = located {
                row = vec![("doc", out::s(name)), ("line", out::int(line)), ("col", out::int(col))];
            }
        }
        row.push(("message", out::s(message.to_string())));
        out::text(&out::obj(vec![
            ("error", out::s(format!("{} does not evaluate with the change, so nothing was committed; it stays at rev {}", text.doc, text.rev))),
            ("diagnostics", out::arr([out::line(out::obj(row))])),
        ]))
    }

    /// Answer the AI edits whose commit every check has run on: the answer gains `checks`,
    /// what the edit changed for them, in `elec.check.list`'s words. A wait ends at the first
    /// snapshot at or after its commit with no check pending. O(findings) per answer; nothing
    /// while the checks run.
    fn finish_checks(&mut self) {
        use fab_ai::out;
        let Some(snap) = self.driver.snap().cloned() else { return };
        let at = snap.last_commit.as_ref().map_or(0, |commit| commit.seq);
        if !fab_elec_host::edit_actions::checks_current(&snap) || !self.check_waits.iter().any(|wait| wait.before.is_some() && wait.seq <= at) {
            return;
        }
        let shown = shown_findings(&snap);
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.check_waits).into_iter().partition(|wait| wait.before.is_some() && wait.seq <= at);
        self.check_waits = waiting;
        for mut wait in ready {
            let before = wait.before.take().expect("a wait for its checks");
            let mut new: Vec<&fab_elec_circuit::Violation> = snap.violations.value.iter()
                .filter(|v| shown.binary_search(&v.id).is_ok() && before.binary_search(&v.id).is_err()).collect();
            new.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.id.cmp(&b.id)));
            let gone = before.iter().filter(|id| shown.binary_search(id).is_err()).count();
            let listed = new.len().min(EDIT_FINDINGS);
            let mut checks = vec![
                ("new", out::arr(new[..listed].iter().map(|v| out::S::Verbatim(fab_elec_host::edit_actions::finding_text(&snap.design, v))))),
                ("gone", out::int(gone)),
            ];
            if new.len() > listed {
                checks.push(("more", out::s(format!("{} more new findings: elec.check.list lists them", new.len() - listed))));
            }
            wait.fields.push(("checks".into(), out::obj(checks)));
            if wait.reqs.is_some() {
                self.check_waits.push(wait);
            } else {
                self.answer(wait.call, Ok(out::text(&out::S::Object(None, wait.fields))));
            }
        }
        self.finish_reqs();
    }

    /// Answer the AI edits whose checks are in and whose requirements are judged on their commit
    /// (on a snapshot their checks ran on, for one that answers `checks`): the answer gains
    /// `requirements`, what the edit changed for them against the rows shown when it was sent
    /// (`req_actions::req_delta`). O(requirements) per answer; nothing while none is ready.
    fn finish_reqs(&mut self) {
        use fab_ai::out;
        let Some(ReqJudged { seq, checks, shown }) = self.req_judged.clone() else { return };
        let ready = |wait: &CheckWait| wait.before.is_none() && wait.reqs.is_some() && wait.seq <= seq && (checks || !wait.checks);
        if !self.check_waits.iter().any(ready) {
            return;
        }
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.check_waits).into_iter().partition(ready);
        self.check_waits = waiting;
        for CheckWait { call, mut fields, reqs, .. } in ready {
            let reqs = reqs.expect("a wait for its requirements");
            if let Some(delta) = fab_elec_host::req_actions::req_delta(&reqs, shown.as_ref().map(|shown| &shown.report)) {
                fields.push(("requirements".into(), out::S::Verbatim(delta)));
            }
            self.answer(call, Ok(out::text(&out::S::Object(None, fields))));
        }
    }

    /// What commit `seq` made, in the reads' words: each new part by its reference, each new net
    /// by its name, each new link as link N. Names come from that commit's snapshot; none while
    /// the shown snapshot is another commit's. O(created).
    fn made(&self, seq: u64, created: &[Sel]) -> Vec<String> {
        let Some(snap) = self.driver.snap().filter(|snap| snap.last_commit.as_ref().is_some_and(|commit| commit.seq == seq)) else {
            return Vec::new();
        };
        let mut made: Vec<String> = Vec::new();
        for sel in created {
            let name = match sel {
                Sel::Comp(at) => snap.design.unit(at.board).and_then(|unit| unit.circuit.comps.get(at.comp.ix())).map(|comp| comp.refdes.clone()),
                Sel::Net(at) => snap.design.unit(at.board).and_then(|unit| unit.circuit.nets.get(at.net.ix())).map(|net| net.name.clone()),
                Sel::Link(id) => Some(format!("link {}", id.0)),
                _ => None,
            };
            if let Some(name) = name.filter(|name| !made.contains(name)) {
                made.push(name);
            }
        }
        made
    }

    /// The ended route job as `elec.route` answers it: its counts and each net it left open with
    /// why, in the words the board's route line uses. O(open nets).
    fn route_summary(&self) -> fab_ai::out::S {
        use fab_ai::out;
        let Some(snap) = self.driver.snap() else { return out::opt(None) };
        let Some(job) = snap.job.as_deref() else { return out::opt(None) };
        let board = snap.design.unit(job.status.unit).and_then(|unit| unit.board.as_ref());
        let open = job.open.iter().map(|(net, reason)| {
            let name = board.and_then(|board| board.nets.get(net.ix())).map_or_else(|| format!("net {}", net.0), |net| net.name.clone());
            let why = board.map_or_else(String::new, |board| fab_elec_view::board::routeline::why_open(board, reason));
            out::obj(vec![("net", out::s(name)), ("why", out::s(why))])
        });
        // A finished job's counts are its last proposal's; a stopped one's, its last status.
        let (end, routed, failed) = match (job.end, self.driver.job_metrics()) {
            (Some(fab_elec_view::JobEnd::Finished), Some(metrics)) => ("finished", metrics.nets_routed, metrics.nets_open),
            _ => ("stopped", job.status.routed, job.status.failed),
        };
        out::obj(vec![
            ("end", out::name(end)),
            ("routed", out::int(routed)),
            ("failed", out::int(failed)),
            ("open", out::arr(open)),
        ])
    }

    /// What a commit does to the take, as the edit's answer says it: the run's state (playing,
    /// held, or none: no run, a stopped bench, nothing wanting it) and what the circuit change
    /// does to it (a value is retuned where it is heard, a structural change starts the run
    /// over, a board edit leaves it). `elec.sim.status` says when the run has it.
    fn take_effect(&self, circuit: CircuitChange) -> (&'static str, &'static str) {
        let state = match &self.sim {
            Some(sim) if !self.sim_wants.is_empty() && !sim.stopped() => if sim.status().playing { "playing" } else { "held" },
            _ => return ("none", "none"),
        };
        (state, match circuit {
            CircuitChange::None => "none",
            CircuitChange::Values => "retune",
            CircuitChange::Structure => "restart",
        })
    }

    /// The newest committed step the run was made from (0 with no run, or before any).
    fn reflects(&self) -> u64 {
        let Some(sim) = &self.sim else { return 0 };
        let source = sim.generation();
        self.gens.iter().rev().find(|&&(generation, _)| generation <= source).map_or(0, |&(_, seq)| seq)
    }

    /// Note the committed generation the engine published, with the newest commit in it, and drop
    /// the generations the run has passed but the newest of them. O(1) amortised per event.
    fn note_generation(&mut self) {
        let Some(snap) = self.driver.snap() else { return };
        let generation = snap.design.generation;
        if self.gens.back().is_none_or(|&(g, _)| g != generation) {
            self.gens.push_back((generation, snap.last_commit.as_ref().map_or(0, |c| c.seq)));
        }
        let passed = self.sim.as_ref().map_or(u64::MAX, |sim| sim.generation());
        while self.gens.len() > 1 && self.gens[1].0 <= passed {
            self.gens.pop_front();
        }
    }

    /// Start the counted check of the bench the run plays (`board`, when given, must be its board)
    /// to `tstop` (else the bench's stop, else 20 ms), answering `call` with its report: a run
    /// of its own on the pool, from the running lowering with the run's firmware, judged at every
    /// point; the live run and its verdicts go on untouched. A check already running stops and
    /// answers what it got. It starts once the bench's lowering is in (`pump_check`). O(1) on
    /// the UI thread here.
    fn check_bench(&mut self, cx: &mut Cx, call: Delivery, board: Option<String>, tstop: Option<f64>) {
        let Some(snap) = self.driver.snap().cloned() else { return self.answer(call, Err("the design is still loading".into())) };
        let choice = cx.get_global_ref::<ElecLive>().and_then(|live| live.chosen_bench.clone());
        let Some(bench) = fab_elec_view::snapshot::run_bench(&snap.benches, choice.as_deref()) else {
            return self.answer(call, Err("the design has no bench to check: a bench document states one with bench(..)".into()));
        };
        if let Some(name) = board {
            let unit = match fab_elec_host::edit_actions::board_unit(&snap.design, "board", Some(&name)) {
                Ok(unit) => unit,
                Err(diag) => return self.answer(call, Err(diag.message)),
            };
            if !matches!(bench.out.scope, fab_elec_lang::BenchScope::Design(id) if id == unit.id) {
                return self.answer(call, Err(format!("board: the bench that runs, {}, is not {}'s: choose one of its benches in the recording bar first", bench.out.name, unit.name)));
            }
        }
        if self.sim.is_none() {
            return self.answer(call, Err("no bench runs here".into()));
        }
        if let Some(mut old) = self.check.take().filter(|old| old.rx.is_some() || old.generation.is_none()) {
            old.stop.store(true, Ordering::Relaxed);
            for call in std::mem::take(&mut old.calls) {
                self.answer(Some(call), Err("stopped: a newer check of the bench took its place".into()));
            }
        }
        let shown = Arc::new(BenchCheck { bench: bench.out.name.clone(), expects: bench.out.expects.clone(), tstop: tstop.or(bench.out.stop).unwrap_or(20e-3), report: None, earlier: false, reached: Default::default() });
        self.check = Some(CheckRun { calls: call.into_iter().collect(), shown: shown.clone(), stop: Arc::default(), generation: None, rx: None });
        Self::show_check(cx, Some(shown));
        self.pump_check(cx);
    }

    /// Stop check: the running check ends within a slice of wall time and answers what it got;
    /// one still waiting for its lowering goes, its calls answered so.
    fn stop_check(&mut self, cx: &mut Cx) {
        let Some(check) = self.check.as_mut() else { return };
        if check.rx.is_some() {
            check.stop.store(true, Ordering::Relaxed);
        } else if check.generation.is_none() {
            let calls = std::mem::take(&mut check.calls);
            self.check = None;
            for call in calls {
                self.answer(Some(call), Err("stopped before its run started".into()));
            }
            Self::show_check(cx, None);
        }
    }

    /// The counted check's next step, on every event while there is one: start it once the
    /// bench's lowering is in (no lowering in flight, its numbering the run's, its firmware's
    /// maker made), take its report, or mark the shown one as of an earlier design once the run's
    /// source moves on. O(1) unless it starts or ends; the run is on the pool (`Class::Visible`).
    fn pump_check(&mut self, cx: &mut Cx) {
        let (Some(check), Some(sim)) = (self.check.as_mut(), self.sim.as_ref()) else { return };
        match (check.generation, &check.rx) {
            (None, _) => {
                let Some(lowered) = sim.lowered().cloned() else {
                    if sim.lowering() {
                        return;
                    }
                    let calls = std::mem::take(&mut check.calls);
                    self.check = None;
                    for call in calls {
                        self.answer(Some(call), Err("the bench does not run yet: play it (or open a view of it) first".into()));
                    }
                    return;
                };
                let index = sim.index().clone();
                let maker = sim.cosim_maker().cloned();
                if sim.lowering() || index.generation != lowered.index.generation || (!lowered.firmware.is_empty() && maker.is_none()) {
                    return;
                }
                let (tx, rx) = to_ui_oneshot();
                let (expects, tstop, stop, reached) = (check.shown.expects.clone(), check.shown.tstop, check.stop.clone(), check.shown.reached.clone());
                let job = move || {
                    let mut lowered = fab_elec_bench::Lowered::clone(&lowered);
                    lowered.index = fab_elec_bench::ValueIndex::clone(&index);
                    let co = maker.as_ref().and_then(|make| make(&lowered, CHECK_MEMBER));
                    // how far it got, for the shelf: a store and a UI signal a slice (50 ms)
                    let progress = |t: f64| {
                        reached.set(t);
                        makepad_widgets::SignalToUI::set_ui_signal();
                    };
                    let _ = tx.send(fab_elec_bench::bench_run(lowered, expects, tstop, co, &stop, &progress));
                };
                match cx.task_pool().submit_named(Class::Visible, "elec bench check", job) {
                    Ok(task) => {
                        task.detach();
                        check.generation = Some(sim.generation());
                        check.rx = Some(rx);
                    }
                    Err(why) => {
                        let calls = std::mem::take(&mut check.calls);
                        self.check = None;
                        for call in calls {
                            self.answer(Some(call), Err(format!("the check could not start: {why}")));
                        }
                    }
                }
            }
            (Some(_), Some(rx)) => {
                let report = match rx.try_recv() {
                    Ok(report) => report,
                    Err(TryRecvError::Empty) => return,
                    Err(TryRecvError::Disconnected) => fab_elec_bench::BenchReport {
                        states: Vec::new(), diags: Vec::new(), points: 0, t: 0.0, error: Some("the check's run ended without a report".into()), rescues: Vec::new(), stopped: false,
                    },
                };
                check.rx = None;
                let calls = std::mem::take(&mut check.calls);
                let text = check_text(&check.shown, &report);
                let shown = Arc::new(BenchCheck { report: Some(report), ..BenchCheck::clone(&check.shown) });
                check.shown = shown.clone();
                let ran_on = check.generation;
                for call in calls {
                    self.answer(Some(call), Ok(text.clone()));
                }
                // The bench's last counted check, for its requirements.
                if let (Some(ran_on), Some(run)) = (ran_on, fab_elec_host::req_actions::bench_run(&shown, true, Cx::time_now())) {
                    self.req_benches.retain(|(name, ..)| *name != shown.bench);
                    self.req_benches.push((shown.bench.clone(), ran_on, run));
                    self.rejudge(cx);
                }
                Self::show_check(cx, Some(shown));
            }
            (Some(generation), None) => {
                if !check.shown.earlier && sim.generation() != generation {
                    let shown = Arc::new(BenchCheck { earlier: true, ..BenchCheck::clone(&check.shown) });
                    check.shown = shown.clone();
                    Self::show_check(cx, Some(shown));
                }
            }
        }
    }

    /// Put the check up for the EXPECTATIONS shelf (`ElecLive::check`), which follows on the next
    /// signal.
    fn show_check(cx: &mut Cx, shown: Option<Arc<BenchCheck>>) {
        if cx.get_global_ref::<ElecLive>().is_some() {
            cx.get_global::<ElecLive>().check = shown;
        }
        makepad_widgets::SignalToUI::set_ui_signal();
    }

    /// A bench action. Status can wait for the run to reflect a step. Play, pause and rate go
    /// through the transport, and a microphone pick goes through the Firmware pane's player.
    /// The network read copies the published connection status and formats one log page on the pool.
    fn sim_call(&mut self, cx: &mut Cx, held: ActionCall) {
        let arg = |name: &str| held.args.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone());
        let cmds = match held.name {
            "elec.sim.status" => {
                let Some(wait) = arg("wait") else {
                    let text = self.status_text();
                    return self.answer(held.call, Ok(text));
                };
                let Ok(seq) = wait.replace('_', "").parse::<u64>() else {
                    return self.answer(held.call, Err(format!("wait: expected a step, got {wait}")));
                };
                let newest = self.gens.back().map_or(0, |&(_, seq)| seq).max(self.done_seq).max(self.driver.undo_step().unwrap_or(0));
                if seq > newest {
                    return self.answer(held.call, Err(format!("wait: no step {seq} yet; the newest is {newest}")));
                }
                if let Some(call) = held.call {
                    self.status_waits.push(StatusWait { call, seq, sent: None });
                    self.check_waits();
                }
                return;
            }
            "elec.sim.leds" => {
                let text = self.leds_text();
                return self.answer(held.call, Ok(text));
            }
            "elec.sim.mic" => {
                let file = match mic_file(&arg("file").expect("a required field, checked")) {
                    Ok(file) => file,
                    Err(why) => return self.answer(held.call, Err(why)),
                };
                cx.action(crate::elec_mic::PlayFile { bed: held.bed, file, call: held.call });
                return;
            }
            "elec.sim.net" => {
                let from = arg("from").unwrap_or_else(|| "0".into());
                let Ok(from) = from.trim().replace('_', "").parse::<usize>() else {
                    return self.answer(held.call, Err("from: expected an arrival index".into()));
                };
                let board = self.sim_mut().filter(|s| s.live_run().is_some()).and_then(|s| s.values())
                    .and_then(|v| v.co.as_ref()).and_then(|co| co.get::<fab_elec_mcu_cosim::FwState>());
                let Some(board) = board else { return self.answer(held.call, Err("No firmware board is running.".into())); };
                let Some((status, log)) = board.net.clone().zip(board.net_log.clone()) else {
                    return self.answer(held.call, Err("The running board has no network connections.".into()));
                };
                let (tx, rx) = to_ui_oneshot();
                match cx.task_pool().submit_named(Class::Interactive, "read board network", move || {
                    let _ = tx.send(net_page(&status, &log, from));
                }) {
                    Ok(task) => {
                        task.detach();
                        if let Some(call) = held.call { self.read_calls.push((call, rx)); }
                    }
                    Err(why) => self.answer(held.call, Err(format!("the network read could not start: {why}"))),
                }
                return;
            }
            "elec.sim.play" => match arg("rate").map(|rate| parse_rate(&rate)).transpose() {
                Ok(rate) => rate.map(SimCmd::SetRate).into_iter().chain([SimCmd::Play]).collect::<Vec<_>>(),
                Err(error) => return self.answer(held.call, Err(error)),
            },
            "elec.sim.pause" => vec![SimCmd::Pause],
            "elec.sim.step" => {
                let dt = match fab_elec_view::instruments::tools::call::parse_field(&fab_elec_simhost::actions::STEP.fields[0], &arg("dt").expect("a required field, checked")) {
                    Ok(fab_elec_view::instruments::tools::call::Value::Num(dt)) if dt > 0.0 => dt,
                    Ok(_) => return self.answer(held.call, Err("dt: expected a positive time, such as 1ms".into())),
                    Err(why) => return self.answer(held.call, Err(why)),
                };
                // A stopped run starts again from 0, as the transport's step does.
                let from = self.sim.as_ref().filter(|sim| !sim.stopped()).and_then(|sim| sim.values()).map_or(0.0, |values| values.t);
                return self.step_call(cx, held.call, SimCmd::Step(dt), Some(from + dt));
            }
            "elec.sim.step_fw" => {
                use fab_elec_sim::cosim::StepTo;
                let to = match arg("to").as_deref().map(|to| to.trim().trim_start_matches('@')) {
                    Some("handler_entry") => StepTo::HandlerEntry,
                    Some("handler_exit") => StepTo::HandlerExit,
                    _ => StepTo::Statement,
                };
                // The firmware's step buttons' own rule: a bound firmware, and a step the device takes.
                let offered = cx.get_global_ref::<ElecLive>()
                    .map(|live| fab_elec_view::live::fw_steps::steps(live.sim.firmware.is_some(), live.sim.co.as_ref()));
                let why = match offered {
                    None => Some("No firmware board is running."),
                    Some(steps) if !steps.shown => Some("No firmware board is running."),
                    Some(steps) if to == StepTo::HandlerExit && !steps.out => Some("to: no handler runs to step out of; step to a statement or a handler entry"),
                    Some(steps) if !steps.step => Some("The firmware cannot step from here: it has halted, or its build has no step map yet."),
                    Some(_) => None,
                };
                if let Some(why) = why {
                    return self.answer(held.call, Err(why.into()));
                }
                return self.step_call(cx, held.call, SimCmd::StepFw(to), None);
            }
            "elec.sim.set" => {
                let Some(snap) = self.driver.snap().cloned() else { return self.answer(held.call, Err("the design is still loading".into())) };
                let (comp, part) = match fab_elec_host::edit_actions::find_comp(&snap.design, "comp", &arg("comp").expect("a required field, checked")) {
                    Ok(found) => found,
                    Err(diag) => return self.answer(held.call, Err(diag.message)),
                };
                let text = arg("value").expect("a required field, checked");
                // A plain number is the value itself (a knob's place, 0 to 1); a quantity reads in the part's unit.
                let value = match text.trim().replace('_', "").parse::<f64>() {
                    Ok(value) if value.is_finite() => value,
                    _ => match part.value.as_ref().map(|current| fab_elec_lang::units::parse_value(&text, current, &part.part.kind)) {
                        Some(Ok(value)) => match value.qty {
                            Some(qty) => qty.si,
                            None => return self.answer(held.call, Err(format!("value: {} takes a number", part.refdes))),
                        },
                        Some(Err(why)) => return self.answer(held.call, Err(format!("value: {why}"))),
                        None => return self.answer(held.call, Err(format!("value: not a number: {text}"))),
                    },
                };
                let Some(sim) = self.sim.as_ref() else {
                    return self.answer(held.call, Err("The bench is not running: elec.sim.play or elec.sim.step starts it.".into()));
                };
                let cmd = SimCmd::SetParam { comp, value };
                let effect = !sim.index().command(&cmd).is_empty();
                cx.action(SimAction::Cmd(cmd));
                use fab_ai::out;
                return self.answer(held.call, Ok(out::text(&out::obj(vec![("effect", out::flag(effect))]))));
            }
            "elec.sim.probe" => {
                let Some(snap) = self.driver.snap().cloned() else { return self.answer(held.call, Err("the design is still loading".into())) };
                let target = match fab_elec_host::edit_actions::find_target(&snap.design, "target", &arg("target").expect("a required field, checked")) {
                    Ok(target) => target,
                    Err(diag) => return self.answer(held.call, Err(diag.message)),
                };
                let measure = arg("measure").map_or_else(|| "now".to_string(), |m| m.trim().trim_start_matches('@').to_string());
                let text = self.probe_text(&target, &measure);
                return self.answer(held.call, text);
            }
            "elec.bench.run" => {
                use fab_elec_view::instruments::tools::call::{parse_field, Value};
                let def = &fab_elec_simhost::actions::BENCH_RUN;
                let tstop = match arg("tstop").map(|text| parse_field(&def.fields[1], &text)) {
                    None => None,
                    Some(Ok(Value::Num(t))) if t > 0.0 && t.is_finite() => Some(t),
                    Some(Ok(_)) => return self.answer(held.call, Err("tstop: expected a positive time, such as 20ms".into())),
                    Some(Err(why)) => return self.answer(held.call, Err(why)),
                };
                return self.check_bench(cx, held.call, arg("board"), tstop);
            }
            "elec.sim.rate" => match parse_rate(&arg("rate").expect("a required field, checked")) {
                Ok(rate) => vec![SimCmd::SetRate(rate)],
                Err(error) => return self.answer(held.call, Err(error)),
            },
            other => return self.answer(held.call, Err(format!("{other} has no executor in this app"))),
        };
        for cmd in cmds {
            cx.action(SimAction::Cmd(cmd));
        }
        if let Some(call) = held.call {
            self.run_calls.push((call, held.name));
        }
    }

    /// Send a step as the transport's own action and answer `call` once the run stands after it.
    fn step_call(&mut self, cx: &mut Cx, call: Delivery, cmd: SimCmd, to: Option<f64>) {
        let sent = self.sim.as_ref().and_then(|sim| sim.values()).cloned();
        cx.action(SimAction::Cmd(cmd));
        if let Some(call) = call {
            self.step_waits.push(StepWait { call, to, sent });
        }
    }

    /// Answer the step calls the run has finished: a newer publication that stands (not
    /// running, no firmware step armed), at or past a timed step's end. A run that ended answers
    /// with its error. O(waits), only while one waits.
    fn check_steps(&mut self) {
        use fab_ai::out;
        let values = self.sim.as_ref().and_then(|sim| sim.values()).cloned();
        let error = self.sim.as_ref().and_then(|sim| sim.error()).map(str::to_string);
        let mut at = 0;
        while at < self.step_waits.len() {
            let wait = &self.step_waits[at];
            let stands = values.as_ref()
                .filter(|values| wait.sent.as_ref().is_none_or(|sent| !Arc::ptr_eq(sent, values)))
                .filter(|values| !values.running && !values.stepping && wait.to.is_none_or(|to| values.t >= to - 1e-9));
            let answer = match (stands, &error) {
                (Some(values), _) => Ok(out::text(&out::obj(vec![("time", out::n(values.t))]))),
                (None, Some(error)) => Err(format!("the run ended: {error}")),
                (None, None) => {
                    at += 1;
                    continue;
                }
            };
            let call = wait.call;
            self.step_waits.remove(at);
            self.answer(Some(call), answer);
        }
    }

    /// `elec.sim.probe`: a net's voltage, a pin's current or a part's dissipation from the newest
    /// values of the running lowering; a net's interval mean, min or max. O(1).
    fn probe_text(&self, target: &Sel, measure: &str) -> Result<String, String> {
        use fab_ai::out;
        let sim = self.sim.as_ref().ok_or("The bench is not running: elec.sim.play or elec.sim.step starts it.")?;
        let index = sim.index();
        let values = sim.values().filter(|values| values.generation == index.generation)
            .ok_or("The bench has no values of the current circuit yet: step or play it first.")?;
        let missing = || "target: not in the running circuit".to_string();
        let now_only = |what: &str| if measure == "now" { Ok(()) } else { Err(format!("measure: a {what} reads now only; mean, min and max are a net's")) };
        let (value, unit) = match *target {
            Sel::Net(net) => {
                let row = index.net(net).ok_or_else(missing)?;
                let list = match measure {
                    "mean" => &values.net_mean,
                    "min" => &values.net_min,
                    "max" => &values.net_max,
                    _ => &values.net_v,
                };
                (list.get(row).copied(), "V")
            }
            Sel::Pin(pin) => {
                now_only("pin")?;
                (index.pin(pin).and_then(|row| values.branch_i.get(row).copied()), "A")
            }
            Sel::Comp(comp) => {
                now_only("part")?;
                (index.comp(comp).and_then(|row| values.part_p.get(row).copied()), "W")
            }
            _ => return Err("target: a net, a pin or a part".into()),
        };
        let value = value.ok_or_else(missing)?;
        let mut fields = vec![("value", out::n(f64::from(value))), ("unit", out::s(unit)), ("t", out::n(values.t))];
        if measure != "now" {
            fields.push(("since", out::n(values.since)));
        }
        Ok(out::text(&out::obj(fields)))
    }

    /// Answer the status waits the run now satisfies. A playing run has the step once it
    /// reflects it, has published values stepped with the commands sent by then, past the time
    /// the newest heard edit goes in at (`SimHost::heard_edit`: the run went back there and
    /// renders on, the old circuit's values until it passes it), and renders no re-render of the
    /// take; a held (paused) run once it reflects the step, as its values move only when it
    /// plays; a bench nothing runs (no run, stopped, nothing wanting it) at once. O(waits), only
    /// while one waits.
    fn check_waits(&mut self) {
        let reflects = self.reflects();
        let idle = self.sim_wants.is_empty() || self.sim.as_ref().is_none_or(|sim| sim.stopped());
        let status = self.sim.as_ref().map(|sim| sim.status());
        let playing = status.as_ref().is_some_and(|status| status.playing);
        let rerendering = status.as_ref().is_some_and(|status| status.rerender.is_some());
        let past_heard = status.as_ref().is_some_and(|status| self.sim.as_ref().and_then(|sim| sim.heard_edit()).is_none_or(|(_, from)| status.t >= from));
        let mut at = 0;
        while at < self.status_waits.len() {
            let wait = &mut self.status_waits[at];
            if reflects >= wait.seq && wait.sent.is_none() {
                wait.sent = self.sim.as_ref().map(|sim| sim.sent_mark());
            }
            let stepped = wait.sent.is_some_and(|mark| self.sim.as_ref().is_some_and(|sim| sim.stepped_past(mark)));
            let has = reflects >= wait.seq && (!playing || (stepped && past_heard && !rerendering));
            if idle || has {
                let call = wait.call;
                self.status_waits.remove(at);
                let text = self.status_text();
                self.answer(Some(call), Ok(text));
            } else {
                at += 1;
            }
        }
    }

    /// The transport calls whose action batch the host has handled, answered with what the
    /// bench shows now.
    fn finish_run_calls(&mut self) {
        use fab_ai::out;
        for (call, name) in std::mem::take(&mut self.run_calls) {
            let status = self.sim.as_ref().map(|sim| sim.status());
            let row = match name {
                "elec.sim.rate" => out::obj(vec![
                    ("rate_asked", status.as_ref().map_or(out::opt(None), |st| rate_out(st.rate_asked))),
                    ("rate_achieved", out::opt(status.as_ref().and_then(|st| st.rate_achieved).map(rate_out))),
                ]),
                _ => out::obj(vec![("time", out::n(status.as_ref().map_or(0.0, |st| st.t)))]),
            };
            self.answer(Some(call), Ok(out::text(&row)));
        }
    }

    /// `elec.sim.status`: what the transport row shows, the step the run reflects and where the
    /// take renders again from.
    fn status_text(&self) -> String {
        use fab_ai::out;
        let status = self.sim.as_ref().map(|sim| sim.status());
        let st = status.as_ref();
        out::text(&out::obj(vec![
            ("t", out::n(st.map_or(0.0, |st| st.t))),
            ("playing", out::flag(st.is_some_and(|st| st.playing))),
            ("stepping", out::flag(st.is_some_and(|st| st.stepping))),
            ("rate_asked", st.map_or(out::opt(None), |st| rate_out(st.rate_asked))),
            ("rate_achieved", out::opt(st.and_then(|st| st.rate_achieved).map(rate_out))),
            ("limited_by", out::s(st.and_then(|st| st.limited_by).map_or(String::new(), |limit| format!("{limit:?}").to_lowercase()))),
            ("steady", out::flag(st.is_some_and(|st| st.steady))),
            ("reflects", out::int(self.reflects())),
            ("rerender", out::opt(st.and_then(|st| st.rerender).map(out::n))),
        ]))
    }

    /// `elec.sim.leds`: each LED of the live bench, by its component path.
    fn leds_text(&self) -> String {
        use fab_ai::out;
        let (Some(sim), Some(snap)) = (&self.sim, self.driver.snap()) else { return out::text(&out::obj(vec![("leds", out::arr([]))])) };
        let design = &snap.design;
        let leds = sim.leds(design).into_iter().map(|led| {
            let path = design.unit(led.part.board).and_then(|unit| unit.circuit.comps.get(led.part.comp.ix())).map_or(String::new(), |comp| comp.path.clone());
            out::obj(vec![
                ("part", out::s(path)),
                ("i", out::n(led.i as f64)),
                ("i_mean", out::n(led.i_mean as f64)),
                ("brightness", out::n(led.brightness as f64)),
                ("rgb", out::arr(led.rgb.into_iter().flatten().map(|c| out::n(c as f64)))),
            ])
        });
        out::text(&out::obj(vec![("leds", out::arr(leds))]))
    }

    pub fn undo(&mut self, _cx: &mut Cx) {
        if !self.settled || self.outside.is_some() {
            return self.drop_edit("an undo");
        }
        self.driver.undo();
    }

    pub fn redo(&mut self, _cx: &mut Cx) {
        if !self.settled || self.outside.is_some() {
            return self.drop_edit("a redo");
        }
        self.driver.redo();
    }

    /// The newest undoable electronics step (for the shared undo order).
    pub fn undo_step(&self) -> Option<u64> {
        self.driver.undo_step()
    }

    /// The newest redoable electronics step (for the shared undo order).
    pub fn redo_step(&self) -> Option<u64> {
        self.driver.redo_step()
    }

    /// The committed design, None before the engine's first snapshot.
    pub fn design(&self) -> Option<&Arc<Design>> {
        self.driver.snap().map(|snap| &snap.design)
    }

    /// The live bench, while something wants a run (None before and after).
    /// This computer's firmware network choice for the project on the bed (`elec_net`).
    pub fn set_network(&mut self, cx: &mut Cx, ctx: crate::elec_net::NetContext) {
        self.firmware.set_network(cx, self.sim.as_mut(), ctx);
    }

    /// When the twin last opened a real connection, once per new time.
    pub fn take_net_used(&mut self) -> Option<i64> {
        self.firmware.take_net_used()
    }

    pub(crate) fn sim_mut(&mut self) -> Option<&mut SimHost> {
        self.sim.as_mut().filter(|_| self.resume_after_failure.is_none())
    }

    /// Make newly available library models visible to the open design.
    pub fn library_models(&mut self, models: Vec<(String, String)>) {
        if !models.is_empty() {
            self.driver.push(Cmd::LibraryModels(models));
            self.driver.kick();
        }
    }

    /// Stop using the library models under `prefix` (`parts/lcsc/`): their source was turned off,
    /// and the parts naming them keep their own bodies again. One re-evaluation when any was used.
    pub fn drop_library_models(&mut self, prefix: &str) {
        self.driver.push(Cmd::LibraryDrop(prefix.to_string()));
        self.driver.kick();
    }
}

/// For a test's wait on a playing run: panics at once when the run can no longer get there, because
/// it ended with an error or stands (its take ended, or it paused) at the time it says. A wait that
/// calls it each turn it is not done never spins on a run that stopped, which would hold the tests'
/// shared Cx lock and stall every test that takes it. O(1).
#[cfg(test)]
impl ElecHost {
    pub(crate) fn assert_plays_on(&self, what: &str) {
        let Some(sim) = self.sim.as_ref() else { return };
        if let Some(error) = sim.error() {
            panic!("{what}: the run ended: {error}");
        }
        if let Some(values) = sim.values().filter(|values| !values.running && values.t > 0.0) {
            panic!("{what}: the run stands at {} s (its take ended, or it paused) while the wait needs it playing", values.t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_board::Factories;
    use fab_elec_fixtures::ledctl;
    use std::time::{Duration, Instant};

    /// How long a test waits on the host before it calls the host hung. Every wait ends as soon
    /// as the host gets there; this bound only stops one that never does, so it is minutes, not
    /// seconds: a slow box is no failed test. On a loaded box (four test jobs beside these, 10 GB
    /// in use) a plate's first snapshot took over 60 s, where it takes under 5 s on a quiet one.
    const HANG_GUARD: Duration = Duration::from_secs(300);

    /// The AI's text change through the host as the app sends it (`fab.elec_text`), pumped until
    /// it is answered: whether it did what it asked, its answer, and the cues sent meanwhile.
    fn ai_text(host: &mut ElecHost, cx: &mut Cx, doc: &str, rev: u64, change: fab_elec_host::doc_actions::TextChange, id: u64) -> (bool, String, Vec<ElecOutsideEdit>) {
        let ask = TextAsk { doc: doc.into(), rev, change };
        let mut sent = cx.capture_actions(|cx| host.run_text(cx, ask, By::Ai { turn: 0, call: id }, Some(id)));
        let deadline = Instant::now() + HANG_GUARD;
        let mut cues = Vec::new();
        loop {
            cues.extend(sent.iter().filter_map(|a| a.downcast_ref::<ElecOutsideEdit>()).cloned());
            if let Some((_, ok, text)) = host.take_answers().into_iter().find(|answer| answer.0 == id) {
                return (ok, text, cues);
            }
            assert!(Instant::now() < deadline, "the text call {id} is answered");
            std::thread::sleep(Duration::from_millis(1));
            sent = cx.capture_actions(|cx| { host.event(cx, &Event::Signal); });
        }
    }

    /// The stock controller as a plate holds it: its files, the documents `ledctl()` is evaluated
    /// from (the stock's empty system document is no file, `Doc::is_file`).
    /// The controller's bench line with its 20 ms take, and the same bench with none: an open run
    /// plays until the test pauses or stops it, so no wait races a take's end on a loaded box.
    const OPEN_TAKE: (&str, &str) = (r#"bench("bringup", {circuit: "ledctl" run: {stop: 20ms}}"#, r#"bench("bringup", {circuit: "ledctl"}"#);

    fn ledctl_plate() -> Arc<fab_model::Docs> {
        let stock = fab_elec_fixtures::docs("ledctl").expect("the controller has stock documents");
        Arc::new(stock.docs().iter().filter(|d| d.is_file()).map(|d| (d.name.clone(), d.text.clone())).collect())
    }

    /// A value dragged through its series is heard while it moves (user, 11-18: "if we change a
    /// resistor i expect to hear it"): the playing run takes the value-only preview as an edit,
    /// with no restart, and goes back to the committed value when the drag is cancelled. A move's
    /// preview never reaches the run: the run keeps the committed design.
    #[test]
    fn a_value_preview_reaches_the_playing_run_and_a_move_preview_does_not() {
        use fab_elec_circuit::CircuitOp;
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let sources = fab_elec_fixtures::docs("ledctl").expect("the controller has fixture documents");
        // The bench's 20 ms take made open: the run plays on through every wait below until the
        // test ends, however long a loaded box takes over them (a 10 s take could end first).
        let docs: fab_model::Docs = sources.docs().into_iter().map(|doc| (doc.name.clone(), Arc::from(doc.text.replace(OPEN_TAKE.0, OPEN_TAKE.1)))).collect();
        assert!(docs.values().any(|text| text.contains(OPEN_TAKE.1)), "the controller's bench names no take");
        let mut host = ElecHost::start(&mut cx, 0, None, Arc::new(docs), Arc::default(),
            Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        // Each wait ends on the host's own progress; a run that ended or stands fails it at once.
        let until = |host: &mut ElecHost, cx: &mut Cx, what: &str, done: &dyn Fn(&ElecHost) -> bool| loop {
            host.event(cx, &Event::Signal);
            if done(host) { break; }
            host.assert_plays_on(what);
            std::thread::sleep(Duration::from_millis(1));
        };
        let t = |host: &ElecHost| host.sim.as_ref().and_then(|sim| sim.values()).map_or(0.0, |values| values.t);
        host.event(&mut cx, &Event::Actions(vec![Box::new(SimAction::Cmd(fab_elec_bench::SimCmd::Play))]));
        until(&mut host, &mut cx, "running values", &|host| host.sim.as_ref().and_then(|sim| sim.values()).is_some_and(|v| v.running && v.t >= 2e-3));
        let snap = host.driver.snap().expect("the snapshot arrives before running values").clone();
        let committed = snap.design.generation;
        assert_eq!(host.sim.as_ref().map(|sim| sim.generation()), Some(committed), "the run runs the committed design");
        let unit = snap.design.unit_by_name("ledctl").expect("the plate holds the controller");
        let boot = unit.circuit.comp("mcu.boot_r").expect("the fixture has its boot resistor");
        let value = boot.value.clone().expect("the boot resistor has a value");
        let qty = value.qty.expect("its value is a quantity");
        let tagged = |tag: u64| move |host: &ElecHost| host.driver.snap().is_some_and(|snap| snap.preview_tag == Some(tag));
        // A fresh lowering numbers the run anew; values taken as commands keep its number.
        let numbering = |host: &ElecHost| host.sim.as_ref().map(|sim| sim.index().generation);
        let run = numbering(&host);

        // a value dragged: its preview reaches the playing run before any release
        let tag = fab_elec_view::snapshot::next_tag();
        let dragged = fab_elec_circuit::Value { text: "22k".into(), qty: Some(fab_elec_circuit::Quantity { si: 22e3, ..qty }), tol: value.tol };
        host.act(&mut cx, ViewAction::Preview { tag, place: vec![], notes: vec![], label: "Set value".into(),
            ops: vec![DesignOp::Circuit { unit: unit.id, op: CircuitOp::SetValue { comp: boot.id, value: dragged } }] });
        until(&mut host, &mut cx, "the value's preview shown", &tagged(tag));
        let preview = host.driver.snap().and_then(|snap| snap.preview.clone()).expect("the value's preview");
        assert!(host.driver.sim_design().is_some_and(|design| Arc::ptr_eq(design, &preview)), "the run is handed the value's preview");
        until(&mut host, &mut cx, "the run on the dragged value", &|host| host.sim.as_ref().is_some_and(|sim| sim.generation() == preview.generation));
        assert_eq!(numbering(&host), run, "the dragged value is taken by the playing run as an edit, not a new run");
        host.act(&mut cx, ViewAction::CancelPreview);
        until(&mut host, &mut cx, "the run back on the committed value", &|host| host.sim.as_ref().is_some_and(|sim| sim.generation() == committed));
        assert_eq!(numbering(&host), run, "a cancelled drag goes back by the same edit");

        // a part moved on the board: its preview never reaches the run
        let item = unit.board.as_ref().expect("the fixture has a board").placed[0].id;
        let tag = fab_elec_view::snapshot::next_tag();
        host.act(&mut cx, ViewAction::Preview { tag, place: vec![], notes: vec![], label: "drag".into(),
            ops: vec![DesignOp::Board { unit: unit.id, op: BoardOp::Move { item, by: P::mm(2.0, 0.0) } }] });
        until(&mut host, &mut cx, "the move's preview shown", &tagged(tag));
        let shown = host.driver.snap().expect("the move's snapshot").clone();
        assert!(shown.preview.is_some() && host.driver.sim_design().is_some_and(|design| Arc::ptr_eq(design, &shown.design)),
            "the run is handed the committed design while a move previews");
        // 50 ms of the run's own time with the move previewed: the frames that would hand it on
        let from = t(&host);
        until(&mut host, &mut cx, "the run plays on through the move's preview", &|host| t(host) > from + 50e-3);
        assert_eq!(host.sim.as_ref().map(|sim| sim.generation()), Some(committed), "a move's preview never reaches the run");
        assert!(!host.sim.as_ref().is_some_and(|sim| sim.lowering()), "nor starts a lowering of it");
        assert_eq!(numbering(&host), run, "the run goes on through a move's preview");
        host.act(&mut cx, ViewAction::CancelPreview);
    }

    #[test]
    fn transport_owns_a_run_without_a_scope_and_keeps_the_stopped_leds() {
        use fab_elec_view::live::sim::{LiveStatus, SinkSlot};

        struct ScopeSink;
        impl fab_elec_sim::analysis::Sink for ScopeSink {}

        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let sources = fab_elec_fixtures::docs("ledctl").expect("the controller has fixture documents");
        // The bench's 20 ms take made open: a run that ended at 20 ms between two publications
        // left the waits below nothing to see (it hung) or replaced the stopped LED snapshot
        // between them, and a 10 s one could end under a loaded box's waits. The open run plays
        // on through every wait until Pause.
        let docs: fab_model::Docs = sources.docs().into_iter().map(|doc| (doc.name.clone(), Arc::from(doc.text.replace(OPEN_TAKE.0, OPEN_TAKE.1)))).collect();
        assert!(docs.values().any(|text| text.contains(OPEN_TAKE.1)), "the controller's bench names no take");
        let docs = Arc::new(docs);
        let mut host = ElecHost::start(&mut cx, 0, None, docs, Arc::default(),
            Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let send = |host: &mut ElecHost, cx: &mut Cx, action: SimAction| {
            host.event(cx, &Event::Actions(vec![Box::new(action)]));
        };
        // Each wait ends on the run's own progress, never on a clock: the playing run publishes,
        // and only a run that ended stops publishing.
        let until = |host: &mut ElecHost, cx: &mut Cx, what: &str, done: &dyn Fn(&ElecHost, &Cx) -> bool| loop {
            host.event(cx, &Event::Signal);
            if done(host, cx) { break; }
            if let Some(error) = host.sim.as_ref().and_then(|sim| sim.error()) {
                panic!("{what}: the run ended: {error}");
            }
            std::thread::sleep(Duration::from_millis(1));
        };

        assert!(host.sim.is_none(), "no scope or transport means no bench");
        send(&mut host, &mut cx, SimAction::Cmd(SimCmd::Play));
        assert!(host.sim.as_ref().is_some_and(|sim| sim.status().playing),
            "Play without a scope makes the bench and reaches it in the same action batch");
        assert_eq!(host.sim_wants.len(), 1, "only the transport wants this run");
        let still_running = |host: &ElecHost, what: &str| {
            if let Some(v) = host.sim.as_ref().and_then(|sim| sim.values()).filter(|v| !v.running && v.t > 0.0) {
                panic!("{what}: the run stopped at {} s while it should play on", v.t);
            }
        };
        // The document's PWM keeps publications distinct; its always-low PA0 keeps LED0 lit.
        until(&mut host, &mut cx, "running values", &|host, _| {
            still_running(host, "running values");
            host.sim.as_ref().and_then(|sim| sim.values()).is_some_and(|v| v.running && v.t >= 5e-3)
        });
        let snap = host.driver.snap().expect("the controller snapshot arrives before running values");
        let unit = snap.design.unit_by_name("ledctl").expect("the plate holds the controller");
        let lit_part = fab_elec_core::CompRef { board: unit.id,
            comp: unit.circuit.comp("lamps[0].d").expect("the fixture has its always-on LED").id };
        let leds = host.sim.as_ref().expect("Play keeps the bench").leds(&snap.design);
        assert!(leds.iter().any(|led| led.part == lit_part && led.i > 1e-3),
            "an LED row goes non-zero with no scope shown: the document's stimulus drives over 1 mA");
        let running_leds = cx.get_global::<ElecLive>().sim.leds.clone();
        let running_glow = cx.get_global::<ElecLive>().sim.glow.clone();
        let running_values = host.sim.as_ref().and_then(|sim| sim.values()).expect("running values arrived").clone();
        until(&mut host, &mut cx, "another running publication", &|host, _| {
            still_running(host, "another running publication");
            host.sim.as_ref().and_then(|sim| sim.values()).is_some_and(|v| !Arc::ptr_eq(v, &running_values))
        });
        assert!(Arc::ptr_eq(&running_leds, &cx.get_global::<ElecLive>().sim.leds),
            "running publications do not replace the stopped LED snapshot");
        // The LED row a Bench reads follows every publication while the run plays.
        let live = &cx.get_global::<ElecLive>().sim;
        assert!(!Arc::ptr_eq(&running_glow, &live.glow), "a running publication refreshes the LEDs' light output");
        let at = live.glow_parts.iter().position(|part| *part == lit_part).expect("the glow row names the always-on LED");
        assert!(live.glow.len() == live.glow_parts.len() && live.glow[at] > 0.0,
            "the always-on LED glows while the run plays: {:?} of {:?}", live.glow, live.glow_parts);

        send(&mut host, &mut cx, SimAction::Cmd(SimCmd::Pause));
        assert!(host.sim.is_some() && host.sim_wants.contains(&TRANSPORT_WANT),
            "Pause keeps the transport's bench and want");
        until(&mut host, &mut cx, "paused LEDs", &|host, cx| {
            host.sim.as_ref().and_then(|sim| sim.values()).is_some_and(|v| !v.running)
                && cx.get_global_ref::<ElecLive>().is_some_and(|live| !Arc::ptr_eq(&live.sim.leds, &running_leds))
        });
        let paused_leds = cx.get_global::<ElecLive>().sim.leds.clone();
        assert!(!paused_leds.is_empty(), "the stopped snapshot contains the controller's LEDs");
        assert!(paused_leds.iter().any(|led| led.part == lit_part && led.i > 1e-3 && led.brightness > 0.0),
            "Pause writes ElecLive.sim.leds at rest and names the same LED as lit with no scope shown");
        let values = host.sim.as_ref().and_then(|sim| sim.values()).expect("paused values arrived").clone();
        for _ in 0..4 { host.event(&mut cx, &Event::Signal); }
        assert!(host.sim.as_ref().and_then(|sim| sim.values()).is_some_and(|v| Arc::ptr_eq(v, &values)),
            "idle frames consume no new values");
        assert!(Arc::ptr_eq(&paused_leds, &cx.get_global::<ElecLive>().sim.leds),
            "the one stopped LED Vec is retained on every unchanged idle frame");
        assert_eq!(cx.get_global::<ElecLive>().sim.status,
            LiveStatus::from(host.sim.as_ref().expect("Pause keeps the bench").status()),
            "views receive the complete host status");

        send(&mut host, &mut cx, SimAction::Cmd(SimCmd::Step(1e-5)));
        until(&mut host, &mut cx, "stepped LEDs", &|host, cx| {
            host.sim.as_ref().and_then(|sim| sim.values()).is_some_and(|v| !v.running && v.t > values.t)
                && cx.get_global_ref::<ElecLive>().is_some_and(|live| !Arc::ptr_eq(&live.sim.leds, &paused_leds))
        });
        assert!(host.sim_wants.contains(&TRANSPORT_WANT), "Step retains the transport want");

        send(&mut host, &mut cx, SimAction::Cmd(SimCmd::SetRate(0.1)));
        assert!(host.sim_wants.contains(&TRANSPORT_WANT), "SetRate owns a run without a scope");
        assert!(cx.get_global::<ElecLive>().sim.parts.is_some(), "the paused instant has live rows before Stop");
        let (lights, stopped_at) = (cx.get_global::<ElecLive>().sim.lights,
            host.sim.as_ref().and_then(|sim| sim.values()).expect("the paused run has values").t);
        send(&mut host, &mut cx, SimAction::Cmd(SimCmd::Stop));
        assert!(host.sim_wants.is_empty(), "Stop without a scope removes the final want");
        assert!(!host.sim.as_ref().expect("the paused bench is retained").status().playing,
            "Stop leaves the bench not playing");
        let live = &cx.get_global::<ElecLive>().sim;
        assert!(live.nets.is_none() && live.segs.is_none() && live.parts.is_none() && !live.playing && !live.status.playing,
            "Stop clears the live rows and play state");
        // Stop takes the whole run off the views (the user's Stop: no glow, stripes, heat or lit
        // LEDs anywhere), moves the lights once so a view drawing LED light draws it off, and the
        // run's own state goes with it.
        assert!(!live.runs() && live.leds.is_empty() && live.co.is_none() && live.t == 0.0 && live.record == (0.0, 0.0),
            "Stop leaves no live state: LEDs {}, device state {}, t {}, record {:?}", live.leds.len(), live.co.is_some(), live.t, live.record);
        assert_eq!(live.lights, lights + 1, "Stop moves the lights counter once");
        assert!(host.sim.as_ref().is_some_and(|sim| sim.values().is_none()), "the stopped bench holds no values");
        for _ in 0..3 {
            host.event(&mut cx, &Event::Signal);
        }
        assert!(!cx.get_global::<ElecLive>().sim.runs() && host.sim.as_ref().is_some_and(|sim| !sim.lowering()),
            "frames after Stop publish nothing and lower nothing: no run until Play");
        // Play after Stop runs the design again from t = 0, slowly enough that its first
        // publication comes long before the stopped instant.
        send(&mut host, &mut cx, SimAction::Cmd(SimCmd::SetRate(1e-3)));
        send(&mut host, &mut cx, SimAction::Cmd(SimCmd::Play));
        until(&mut host, &mut cx, "the run after Stop", &|host, _| host.sim.as_ref().and_then(|sim| sim.values()).is_some());
        let again = host.sim.as_ref().and_then(|sim| sim.values()).expect("the new run publishes").t;
        assert!(again < stopped_at, "Play after Stop starts from 0: the new run is at {again} s, the stopped one was at {stopped_at} s");
        send(&mut host, &mut cx, SimAction::Cmd(SimCmd::Stop));

        // A shown scope remains a want after the transport stops.
        send(&mut host, &mut cx, SimAction::ShowScope { id: 1, sink: SinkSlot::new(Box::new(ScopeSink)), column: 0.0 });
        send(&mut host, &mut cx, SimAction::Cmd(SimCmd::Play));
        send(&mut host, &mut cx, SimAction::Cmd(SimCmd::Stop));
        assert!(host.sim.is_some() && host.sim_wants.contains(&1), "a scope keeps the bench after Stop");
        assert!(!host.sim_wants.contains(&TRANSPORT_WANT), "Stop removes only the transport want");
        send(&mut host, &mut cx, SimAction::HideScope { id: 1 });

        // A run a shown scope started plays on once no scope is on screen (another layout or
        // workspace): the transport holds it, so the device in a 3D view keeps running.
        host.sim_resume = true;
        send(&mut host, &mut cx, SimAction::ShowScope { id: 2, sink: SinkSlot::new(Box::new(ScopeSink)), column: 0.0 });
        assert!(host.sim_wants.contains(&TRANSPORT_WANT) && host.sim.as_ref().is_some_and(|sim| sim.status().playing),
            "a scope's first want plays the run and the transport holds it");
        send(&mut host, &mut cx, SimAction::HideScope { id: 2 });
        assert!(host.sim.as_ref().is_some_and(|sim| sim.status().playing) && host.sim_wants == HashSet::from([TRANSPORT_WANT]),
            "the run plays on with no scope on screen");
        send(&mut host, &mut cx, SimAction::Cmd(SimCmd::Stop));
    }

    #[test]
    fn firmware_compiles_on_commit_newest_wins_and_a_bad_image_never_attaches() {
        use fab_elec_mcu_cosim::FwState;

        const ON: &str = "let led = output(PD6)\nfn setup() { write(led, low) }\n";
        const OFF: &str = "let led = output(PD6)\nfn setup() { write(led, high) }\n";
        const BAD: &str = "let led = output(PD6)\nfn setup( { write(led, low) }\n";
        const BENCH: &str = r#"
bench("firmware", {circuit: "ledavr"}, fn(b, c) {
    b.firmware(c.mcu.u, "led.fw.splash", {})
})
"#;
        // The bench names no take: the run is open and plays until the test stops it, so no wait
        // depends on how far the run gets while the compiles are slow.
        // The fixture's USB power input lowers to its own 5 V source on VBUS.
        let docs = Arc::new(fab_model::Docs::from([
            ("parts/avr/ATMEGA328P_TQFP32.part.splash".into(),
                Arc::from(include_str!("../../elec/fixtures/parts/avr/ATMEGA328P_TQFP32.part.splash"))),
            ("ledavr.ckt.splash".into(), Arc::from(include_str!("../../elec/fixtures/ledavr.ckt.splash"))),
            ("led.bench.splash".into(), Arc::from(BENCH)),
            ("led.fw.splash".into(), Arc::from(ON)),
        ]));
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut host = ElecHost::start(&mut cx, 0, None, docs, Arc::default(),
            Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        // The pool runs every compile except the off text's: the test keeps that job (no pool
        // worker waits on it) and runs it itself later, so its result is superseded on any box.
        let held: Arc<std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>> = Arc::default();
        host.firmware = crate::elec_fw::FwRunner::with_spawn({
            let held = held.clone();
            Arc::new(move |cx: &mut Cx, source: &str, job: crate::elec_fw::FwJob| {
                if source != OFF {
                    return crate::elec_fw::pool_spawn(cx, source, job);
                }
                *held.lock().expect("only the test touches the held job") = Some(job);
                Ok(None)
            })
        });
        let status = |cx: &Cx| cx.get_global_ref::<ElecLive>().and_then(|live| live.sim.firmware.clone());
        let rev = |host: &ElecHost| {
            host.driver.snap().and_then(|snap| snap.docs.as_ref())
                .and_then(|docs| docs.texts.iter().find(|doc| doc.name == "led.fw.splash"))
                .expect("the committed firmware document is present").rev
        };
        let lit = |host: &ElecHost| {
            let (Some(sim), Some(snap)) = (&host.sim, host.driver.snap()) else { return false };
            let Some(unit) = snap.design.unit_by_name("ledavr") else { return false };
            let Some(led) = unit.circuit.comp("lamps[0].d") else { return false };
            let part = fab_elec_core::CompRef { board: unit.id, comp: led.id };
            sim.leds(&snap.design).iter().any(|led| led.part == part && led.i > 1e-3)
        };
        let device_time = |host: &ElecHost| {
            host.sim.as_ref().and_then(|sim| sim.values()).and_then(|values| values.co.as_ref())
                .and_then(|co| co.get::<FwState>()).map(|state| (state.t_reset, state.t))
        };
        // Each wait ends on the run's own progress, never on a clock: a compile result posts to
        // the runner and the playing run publishes. The open run plays on through every wait; a
        // run that ended or stands fails the wait at once.
        let until = |host: &mut ElecHost, cx: &mut Cx, property: &str, done: &dyn Fn(&ElecHost, &Cx) -> bool| loop {
            host.event(cx, &Event::Signal);
            if done(host, cx) { break; }
            if let Some(error) = host.sim.as_ref().and_then(|sim| sim.error()) {
                panic!("{property}: the run ended: {error}; firmware={:?}", status(cx));
            }
            host.assert_plays_on(property);
            std::thread::sleep(Duration::from_millis(1));
        };
        let commit = |host: &mut ElecHost, base_rev, text: &'static str| {
            host.driver.push(Cmd::Docs {
                set: vec![DocSet::Replace { name: "led.fw.splash".into(), base_rev, text: Arc::from(text) }],
                label: "firmware edit".into(), by: By::default(), ticket: None,
            });
        };

        host.event(&mut cx, &Event::Actions(vec![Box::new(SimAction::Cmd(SimCmd::Play))]));
        assert_eq!(host.sim_wants, HashSet::from([TRANSPORT_WANT]),
            "Play before the first image owns a run with no scope");
        until(&mut host, &mut cx, "the initial firmware drives LED0 above 1 mA", &|host, cx| {
            status(cx).is_some_and(|live| live.running_rev == Some(rev(host)))
                && lit(host) && device_time(host).is_some()
        });
        let first_rev = rev(&host);
        let (first_reset, first_t) = device_time(&host).expect("the nominal run publishes firmware state");
        assert!(lit(&host), "PD6 low sinks more than 1 mA through LED0 without a scope");
        let first_live = status(&cx).expect("the running image has firmware status");
        assert_eq!(first_live.running_rev, Some(first_rev), "the initial image names the committed document revision");
        host.event(&mut cx, &Event::Signal);
        assert!(Arc::ptr_eq(&first_live, &status(&cx).expect("an idle firmware frame keeps status")),
            "unchanged frames retain the firmware status Arc");

        commit(&mut host, first_rev, BAD);
        let bad_rev = first_rev + 1;
        until(&mut host, &mut cx, "a bad commit diagnoses its own revision while the old image runs", &|host, cx| {
            status(cx).is_some_and(|live| live.failed.as_ref().is_some_and(|(rev, _)| *rev == bad_rev))
                && device_time(host).is_some_and(|(_, t)| t > first_t)
        });
        let failed = status(&cx).expect("a rejected image publishes firmware status");
        let (failed_rev, diags) = failed.failed.as_ref().expect("the syntax error produces diagnostics");
        assert_eq!(*failed_rev, rev(&host), "diagnostics name the committed bad revision");
        assert!(!diags.is_empty() && diags.iter().all(|diag| diag.span.as_ref()
            .is_some_and(|span| span.start < span.end && span.end <= BAD.len())),
            "every syntax diagnostic selects a byte span inside the bad firmware text");
        assert_eq!(failed.running_rev, Some(first_rev), "a bad image never replaces the running revision");
        assert_eq!(device_time(&host).map(|(reset, _)| reset), Some(first_reset),
            "a bad image never resets or replaces the attached device");
        assert!(lit(&host), "the old image still lights LED0 after the bad commit");

        // Newest wins, deterministically: the off commit's compile is held while the on commit
        // compiles and attaches, then released; its late result must never attach.
        let off_rev = bad_rev + 1;
        let last_rev = off_rev + 1;
        commit(&mut host, bad_rev, OFF);
        until(&mut host, &mut cx, "the off commit's compile is submitted and kept by the test", &|_, cx| {
            status(cx).is_some_and(|live| live.compiling_rev == Some(off_rev))
        });
        assert!(held.lock().expect("the held job").is_some(), "the off commit's compile job is in the test's hands");
        commit(&mut host, off_rev, ON);
        until(&mut host, &mut cx, "the newer on commit compiles and attaches while off is held", &|host, cx| {
            let Some(live) = status(cx) else { return false };
            live.running_rev == Some(last_rev) && lit(host)
                && device_time(host).is_some_and(|(reset, t)| reset > first_reset && t > reset)
        });
        let (on_reset, _) = device_time(&host).expect("the on image's device publishes its state");
        let superseded = status(&cx).expect("the on image publishes firmware status").superseded;
        // The off compile runs now, on the test thread, and posts its result to the runner.
        let off_job = held.lock().expect("the held job").take().expect("the off compile is still held");
        off_job();
        until(&mut host, &mut cx, "the released off compile comes back and is dropped", &|_, cx| {
            status(cx).is_some_and(|live| live.superseded > superseded)
        });
        let latest = status(&cx).expect("the last image publishes firmware status");
        assert_eq!(rev(&host), last_rev, "both edits commit through the host's revision check");
        assert_eq!(latest.running_rev, Some(last_rev), "a superseded compile never attaches: on keeps running");
        assert_eq!(device_time(&host).map(|(reset, _)| reset), Some(on_reset),
            "the late off result neither resets nor replaces the attached device");
        assert!(latest.failed.is_none() && latest.compiling_rev.is_none(),
            "a good result clears the failure and finishes its compile");
        assert!(lit(&host), "the newest image drives PD6 low and keeps LED0 above 1 mA");

        // The AI's change of the running firmware is built first with the run's own inputs:
        // text that does not compile commits nothing and says where; text that does goes in.
        use fab_elec_host::doc_actions::TextChange;
        let replace = |old: &str, new: &str| TextChange::Replace(vec![(old.to_string(), new.to_string())]);
        let (ok, answer, cues) = ai_text(&mut host, &mut cx, "led.fw.splash", last_rev, replace("fn setup()", "fn setup("), 60);
        assert!(!ok && answer.contains("does not compile") && answer.contains("nothing was committed") && answer.contains("line: 2, col: "),
            "a change that does not compile is refused with where: {answer}");
        assert!(cues.is_empty() && rev(&host) == last_rev, "nothing was committed and nothing cued");
        let (ok, answer, cues) = ai_text(&mut host, &mut cx, "led.fw.splash", last_rev, replace("output(PD6)", "output(PD5)"), 61);
        assert!(ok && answer.contains("compiled: true") && answer.contains(&format!("rev: {}", last_rev + 1)) && answer.contains("effect: @restart"),
            "a change that compiles is committed and the run starts over with it: {answer}");
        assert!(matches!(cues.as_slice(), [cue] if cue.lines == vec![("led.fw.splash".to_string(), 0..1)]), "its cue washes its line: {cues:?}");
        assert_eq!(rev(&host), last_rev + 1, "committed through the host's revision check");
        host.event(&mut cx, &Event::Actions(vec![Box::new(SimAction::Cmd(SimCmd::Stop))]));
    }

    #[test]
    fn firmware_screen_frames_reach_the_live_view_and_clear_on_removal() {
        const CIRCUIT: &str = r#"
use mod.elec.*
use mod.std.*
let e = mod.elec
let gnd = e.net({name: "GND" kind: @ground})
let vdd = e.net({name: "3V3" kind: @power volts: 3.3V})
let sda = e.net({name: "SDA"})
let scl = e.net({name: "SCL"})
let panel_cs = e.net({name: "PANEL_CS"})
let panel_clock = e.net({name: "PANEL_CLOCK"})
let panel_data = e.bus("PANEL_DATA", 4)
let panel_reset = e.net({name: "PANEL_RESET"})
let expander_int = e.net({name: "EXPANDER_INT"})
let spare = e.bus("SPARE", 7)
let xtal_p = e.net({name: "XTAL_P"})
let xtal_n = e.net({name: "XTAL_N"})
let enable = e.net({name: "ENABLE"})
let boot = e.net({name: "BOOT"})
let mcu = e.part("mcu", {pins: {
    VDD3P3: vdd VDD3P3_RTC: vdd VDD3P3_CPU: vdd VDD_SPI: vdd VDDA: vdd GND: gnd
    XTAL_P: xtal_p XTAL_N: xtal_n CHIP_PU: enable GPIO0: boot
    GPIO15: sda GPIO14: scl GPIO12: panel_cs GPIO11: panel_clock
    GPIO4: panel_data[0] GPIO5: panel_data[1] GPIO6: panel_data[2] GPIO7: panel_data[3]
} nc: [@LNA_IN @GPIO1 @GPIO2 @GPIO3 @GPIO8 @GPIO9 @GPIO10 @GPIO13 @GPIO16 @GPIO17
       @GPIO18 @GPIO19 @GPIO20 @GPIO21 @GPIO26 @GPIO27 @GPIO28 @GPIO29 @GPIO30 @GPIO31 @GPIO32
       @GPIO33 @GPIO34 @GPIO35 @GPIO36 @GPIO37 @GPIO38 @GPIO39 @GPIO40 @GPIO41 @GPIO42
       @GPIO43 @GPIO44 @GPIO45 @GPIO46 @GPIO47 @GPIO48]})
let panel = e.part("panel", {pins: {
    VDD: vdd GND: gnd CSX: panel_cs SCLK: panel_clock RESX: panel_reset
    SIO0: panel_data[0] SIO1: panel_data[1] SIO2: panel_data[2] SIO3: panel_data[3]
}})
let expander = e.part("expander", {pins: {
    VDD: vdd GND: gnd SDA: sda SCL: scl INT: expander_int
    P0: panel_reset P1: spare[0] P2: spare[1] P3: spare[2] P4: spare[3]
    P5: spare[4] P6: spare[5] P7: spare[6]
}})
let pull_sda = e.r(2.2kohm, sda, vdd, {package: "0402"})
let pull_scl = e.r(2.2kohm, scl, vdd, {package: "0402"})
let pull_expander_int = e.r(10kohm, expander_int, vdd, {package: "0402"})
let pull_panel_cs = e.r(10kohm, panel_cs, vdd, {package: "0402"})
let pull_enable = e.r(10kohm, enable, vdd, {package: "0402"})
let pull_boot = e.r(10kohm, boot, vdd, {package: "0402"})
let crystal = e.part(crystal_part("3225"), {value: 40MHz pins: [xtal_p xtal_n gnd]})
let load_p = e.c(12pF, xtal_p, gnd, {package: "0402"})
let load_n = e.c(12pF, xtal_n, gnd, {package: "0402"})
let decouple = e.c(100nF, vdd, gnd, {package: "0402"})
"#;
        const BENCH: &str = r#"
bench("screen", {circuit: "screen" run: {stop: 2s}}, fn(b, c) {
    b.supply(c.net("3V3"), {v: 3.3V})
    b.firmware(c.mcu, "screen.fw.splash", {})
    b.net(c.mcu, 0, {endpoint: "svc" url: "wss://svc.example/v1"})
})
"#;
        const SOURCE: &str = r#"
let screen = display()
let change = after(ms(100), fn() {
    fill_rect(screen, 0, 0, display_w, display_h, rgb(0, 0, 255))
})
fn setup() {
    fill_rect(screen, 0, 0, display_w, display_h, rgb(255, 0, 0))
    start(change)
}
"#;
        let docs = Arc::new(fab_model::Docs::from([
            ("parts/s3/ESP32S3_QFN56.part.splash".into(),
                Arc::from(include_str!("../../elec/fixtures/parts/s3/ESP32S3_QFN56.part.splash"))),
            ("handheld.models.part.splash".into(), Arc::from(include_str!("../../elec/fixtures/handheld.models.part.splash"))),
            ("screen.ckt.splash".into(), Arc::from(CIRCUIT)),
            ("screen.bench.splash".into(), Arc::from(BENCH)),
            ("screen.fw.splash".into(), Arc::from(SOURCE)),
        ]));
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut host = ElecHost::start(&mut cx, 0, None, docs, Arc::default(),
            Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let t = |host: &ElecHost| host.sim.as_ref().and_then(|sim| sim.values()).map_or(0.0, |values| values.t);
        // Each wait ends on the run's own progress (its publications), never on a clock.
        let until = |host: &mut ElecHost, cx: &mut Cx, property: &str, done: &dyn Fn(&ElecHost, &Cx) -> bool| loop {
            host.event(cx, &Event::Signal);
            if let Some(error) = host.sim.as_ref().and_then(|sim| sim.error()) {
                panic!("{property}: the run ended: {error}");
            }
            if done(host, cx) { break; }
            std::thread::sleep(Duration::from_millis(1));
        };
        let send = |host: &mut ElecHost, cx: &mut Cx, cmd: SimCmd| host.event(cx, &Event::Actions(vec![Box::new(SimAction::Cmd(cmd))]));
        let step = |host: &mut ElecHost, cx: &mut Cx, property: &str, dt: f64| {
            let to = t(host) + dt;
            send(host, cx, SimCmd::Step(dt));
            until(host, cx, property, &|host, _| {
                host.sim.as_ref().and_then(|sim| sim.values()).is_some_and(|v| !v.running && v.t >= to - 1e-9)
            });
        };
        // Wait for the whole glass to fill: a partly written red frame is still changing.
        let colour = |cx: &Cx, channel: usize| cx.get_global_ref::<ElecLive>().is_some_and(|live| {
            live.sim.screens.first().is_some_and(|screen| screen.mean[channel] > 1.0 - 1e-6
                && screen.mean.iter().enumerate().all(|(i, value)| i == channel || *value < 0.01))
        });

        // The first step waits for the firmware's first build. The run then advances only in
        // simulated steps, so it stands long before the panel's first frame however slowly this
        // thread runs. A played run could reach its 2 s stop before a slow (cold) thread paused
        // it, and a busy host must not miss the red interval by reading only a later blue frame.
        let fw_runs = |cx: &Cx| cx.get_global_ref::<ElecLive>()
            .and_then(|live| live.sim.firmware.clone()).is_some_and(|fw| fw.running_rev.is_some());
        while !fw_runs(&cx) || t(&host) <= 5e-3 {
            assert!(t(&host) < 0.2, "the panel's firmware runs by 0.2 simulated seconds");
            step(&mut host, &mut cx, "the panel's firmware runs", 5e-3);
        }
        assert!(!colour(&cx, 0) && !colour(&cx, 2), "the run stands before the panel's first red frame: {} s", t(&host));
        // Initialization takes about 0.41 simulated seconds; none of these waits times the CPU.
        while !colour(&cx, 0) {
            assert!(t(&host) < 1.5, "the first red frame reaches ElecLive.sim.screens by 1.5 simulated seconds");
            step(&mut host, &mut cx, "the first red frame reaches ElecLive.sim.screens", 5e-3);
        }
        let snap = host.driver.snap().expect("a published panel frame has a committed design");
        let unit = snap.design.unit_by_name("screen").expect("the design contains the screen board");
        let panel = fab_elec_core::CompRef { board: unit.id,
            comp: unit.circuit.comp("panel").expect("the board contains its panel").id };
        let live = &cx.get_global::<ElecLive>().sim;
        assert_eq!(live.screens.len(), 1, "one panel publishes exactly one LiveScreen");
        let screen = &live.screens[0];
        assert_eq!(screen.comp, panel, "the LiveScreen names the panel's CompRef");
        assert!(screen.on && screen.t > 0.0, "the published panel is on and has a positive simulated frame time");
        assert!(screen.mean[0] > 0.99 && screen.mean[1] < 0.01 && screen.mean[2] < 0.01,
            "setup's published frame is red: {:?}", screen.mean);
        let red_key = live.screens_key;
        let red_t = t(&host);

        step(&mut host, &mut cx, "the unchanged red interval advances", 10e-3);
        assert!(t(&host) > red_t, "the unchanged-frame check advances simulated time");
        assert!(colour(&cx, 0), "the unchanged-frame check stays red before the blue timer fires");
        assert_eq!(cx.get_global::<ElecLive>().sim.screens_key, red_key,
            "an unchanged red frame does not move screens_key while simulated time advances");

        while !colour(&cx, 2) {
            assert!(t(&host) < 1.5, "the changed blue frame reaches ElecLive.sim.screens by 1.5 simulated seconds");
            step(&mut host, &mut cx, "the changed blue frame reaches ElecLive.sim.screens", 5e-3);
        }
        let live = &cx.get_global::<ElecLive>().sim;
        let blue_key = live.screens_key;
        assert!(blue_key > red_key, "the changed blue frame advances screens_key");
        let mean = live.screens[0].mean;
        assert!(mean[2] > 0.99 && mean[0] < 0.01 && mean[1] < 0.01,
            "the timer's published frame is blue: {mean:?}");

        // What the run would reach on the real network is part of its identity: turning it on for
        // the project gives the run a new device from reset, and turning it off another. This run
        // keeps no EEPROM, so `start_with` hands back the image's own maker; the sim host compares
        // makers before identities, and a shared one hid the change (found by a live look). The
        // firmware opens no connection and the server is a reserved `.example` name: nothing dials.
        let reset_at = |host: &ElecHost| host.sim.as_ref().and_then(|sim| sim.values()).and_then(|v| v.co.as_ref())
            .and_then(|co| co.get::<fab_elec_mcu_cosim::FwState>()).map(|state| state.t_reset)
            .expect("the S3 publishes its state");
        let mut net = fab::firmware_net::FirmwareNet::default();
        for on in [true, false] {
            if on { net.turn_on("screen-test", &["svc.example".to_string()], 0) } else { net.turn_off("screen-test") }
            let before = reset_at(&host);
            host.set_network(&mut cx, crate::elec_net::NetContext {
                project: Some("screen-test".into()), net: net.clone(), tokens: Ok(Vec::new()), tokens_rev: 0,
            });
            step(&mut host, &mut cx, "the run after a network switch", 5e-3);
            assert!(reset_at(&host) > before,
                "switching the network {} gives the run a new device from reset: its reset at {} s, before {before} s",
                if on { "on" } else { "off" }, reset_at(&host));
        }

        let base_rev = host.driver.snap().and_then(|snap| snap.docs.as_ref())
            .and_then(|docs| docs.texts.iter().find(|doc| doc.name == "screen.bench.splash"))
            .expect("the running bench has a committed document revision").rev;
        host.driver.push(Cmd::Docs {
            set: vec![DocSet::Replace { name: "screen.bench.splash".into(), base_rev,
                text: Arc::from(BENCH.replace("    b.firmware(c.mcu, \"screen.fw.splash\", {})\n", "")
                    .replace("    b.net(c.mcu, 0, {endpoint: \"svc\" url: \"wss://svc.example/v1\"})\n", "")) }],
            label: "remove panel firmware".into(), by: By::default(), ticket: None,
        });
        until(&mut host, &mut cx, "removing the firmware device clears the live screens", &|_, cx| {
            cx.get_global_ref::<ElecLive>().is_some_and(|live| live.sim.screens.is_empty())
        });
        let live = &cx.get_global::<ElecLive>().sim;
        assert!(live.screens.is_empty(), "removing the firmware device leaves no LiveScreen");
        assert!(live.screens_key > blue_key, "removing the firmware device advances screens_key");
        send(&mut host, &mut cx, SimCmd::Stop);
    }

    /// A chip's EEPROM is kept with the project between runs: a firmware that counts its boots in
    /// it finds the last run's count after a Stop and Play (the run from reset loads the kept
    /// bytes), the bytes go to the project as `<board>.<part>.eeprom` (at once on a Stop), the
    /// Firmware pane says they are kept, and Erase drops them and starts the run again as a new
    /// chip, whose count starts over.
    #[test]
    fn a_chips_eeprom_is_kept_between_runs_and_erase_gives_a_new_chip() {
        use fab_elec_mcu_cosim::FwState;
        use fab_elec_mcu_cosim::FwValue;
        use fab_elec_view::firmware::FirmwareErase;

        // An erased byte reads 255: the first boot counts 0.
        const BOOTS: &str = "let led = output(PD6)\nvar boots: i32 = 0\nfn setup() {\n    boots = (eeprom_read(0) + 1) & 255\n    eeprom_write(0, boots)\n}\n";
        const BENCH: &str = r#"
bench("firmware", {circuit: "ledavr" run: {stop: 1s}}, fn(b, c) {
    b.firmware(c.mcu.u, "boots.fw.splash", {})
})
"#;
        const FILE: &str = "ledavr.mcu.u.eeprom";
        let docs = Arc::new(fab_model::Docs::from([
            ("parts/avr/ATMEGA328P_TQFP32.part.splash".into(),
                Arc::from(include_str!("../../elec/fixtures/parts/avr/ATMEGA328P_TQFP32.part.splash"))),
            ("ledavr.ckt.splash".into(), Arc::from(include_str!("../../elec/fixtures/ledavr.ckt.splash"))),
            ("boots.bench.splash".into(), Arc::from(BENCH)),
            ("boots.fw.splash".into(), Arc::from(BOOTS)),
        ]));
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut host = ElecHost::start(&mut cx, 0, None, docs, Arc::default(),
            Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        host.set_memory(0, Arc::default());
        // The device's boot count and EEPROM byte 0 as the nominal run publishes them, 5 ms after
        // its reset: the count is stored by then (an EEPROM write commits about 3.3 ms after it
        // starts).
        let stored = |host: &ElecHost| -> Option<(i32, u8)> {
            let values = host.sim.as_ref()?.values()?;
            let state = values.co.as_ref()?.get::<FwState>()?;
            if state.t - state.t_reset < 5e-3 {
                return None;
            }
            let at = state.debug.as_ref()?.vars.iter().position(|var| var.name == "boots")?;
            let FwValue::I32(boots) = state.vars.get(at)? else { return None };
            Some((*boots, state.eeprom.as_ref()?[0]))
        };
        let kept = |cx: &Cx| cx.get_global_ref::<ElecLive>().and_then(|live| live.sim.firmware.clone()).and_then(|fw| fw.eeprom);
        // Each wait ends on the run's own progress, never on a clock: the bench stops at 1 simulated
        // second, long after a boot stores its count, so a wait the run reaches its stop in failed.
        let until = |host: &mut ElecHost, cx: &mut Cx, property: &str, done: &mut dyn FnMut(&mut ElecHost, &Cx) -> bool| loop {
            host.event(cx, &Event::Signal);
            if done(host, cx) { break; }
            if let Some(error) = host.sim.as_ref().and_then(|sim| sim.error()) {
                panic!("{property}: the run ended: {error}");
            }
            let t = host.sim.as_ref().and_then(|sim| sim.values()).map_or(0.0, |values| values.t);
            assert!(t < 1.0, "{property}: not before the bench's 1 s stop (the run is at {t} s)");
            std::thread::sleep(Duration::from_millis(1));
        };

        host.event(&mut cx, &Event::Actions(vec![Box::new(SimAction::Cmd(SimCmd::Play))]));
        until(&mut host, &mut cx, "a new chip's first boot stores its count", &mut |host, _| stored(host).is_some()
            || { host.assert_plays_on("a new chip's first boot stores its count"); false });
        assert_eq!(stored(&host), Some((0, 0)), "a new chip's first boot counts 0");
        host.event(&mut cx, &Event::Actions(vec![Box::new(SimAction::Cmd(SimCmd::Stop))]));
        let mut sent = None;
        until(&mut host, &mut cx, "a Stop sends the run's EEPROM to the project", &mut |host, _| {
            sent = sent.take().or_else(|| host.take_memory());
            sent.is_some()
        });
        let (bed, memory) = sent.take().expect("the memories went");
        assert_eq!(bed, 0, "the memories are the bed's");
        assert_eq!(memory.keys().collect::<Vec<_>>(), [FILE], "one file, named by the board and the part");
        assert!(memory[FILE].len() == 1024 && memory[FILE][0] == 0 && memory[FILE][1..].iter().all(|&b| b == 0xFF),
            "the file holds the chip's whole EEPROM as the run left it");
        assert_eq!(kept(&cx), Some(1024), "the Firmware pane says the EEPROM is kept");

        host.event(&mut cx, &Event::Actions(vec![Box::new(SimAction::Cmd(SimCmd::Play))]));
        until(&mut host, &mut cx, "the run from reset loads the kept bytes and counts on", &mut |host, _| stored(host).is_some()
            || { host.assert_plays_on("the run from reset loads the kept bytes and counts on"); false });
        assert_eq!(stored(&host), Some((1, 1)), "the second boot read the first one's count from the kept EEPROM");

        host.event(&mut cx, &Event::Actions(vec![Box::new(FirmwareErase("mcu.u".into()))]));
        assert_eq!(host.take_memory().map(|(_, memory)| memory.contains_key(FILE)), Some(false), "Erase drops the kept bytes from the project");
        assert_eq!(kept(&cx), None, "the Firmware pane no longer says the EEPROM is kept");
        until(&mut host, &mut cx, "the erased chip boots as a new one", &mut |host, _| stored(host).is_some()
            || { host.assert_plays_on("the erased chip boots as a new one"); false });
        assert_eq!(stored(&host), Some((0, 0)), "after Erase the run starts again as a new chip and counts from 0");
        host.event(&mut cx, &Event::Actions(vec![Box::new(SimAction::Cmd(SimCmd::Stop))]));
    }

    /// The nominal device's events reach the host's store: every statement begun, in program
    /// order inside its handler's Enter/Exit, beside the image's own watch frames. A UI that does
    /// not drain fills the feed: the run names events as its limit, the device holds, and one
    /// drain delivers everything with nothing lost (consecutive watch sequence numbers, a handler
    /// run on every period with no gap, the same statements in every run).
    #[test]
    fn firmware_events_reach_the_store_in_order_and_a_full_feed_holds_the_device() {
        use fab_elec_mcu_cosim::{FwFrame, FwState};
        use fab_elec_mcu_cosim::watch;
        use fab_elec_simhost::Limit;

        // The AVR runtime serves timers on its 128 µs Timer0 overflow, so a run on every period
        // needs a longer one: runs then come 384 or 512 µs apart, and a lost run leaves 896 µs.
        const COUNT: &str = "var n: i32 = 0\nvar m: i32 = 0\nlet tick = every(us(500), fn() {\n    n = n + 1\n    m = m ^ n\n})\n";
        const PERIOD: f64 = 500e-6;
        const BENCH: &str = r#"
bench("firmware", {circuit: "ledavr" run: {stop: 5s}}, fn(b, c) {
    b.firmware(c.mcu.u, "led.fw.splash", {})
})
"#;
        let docs = Arc::new(fab_model::Docs::from([
            ("parts/avr/ATMEGA328P_TQFP32.part.splash".into(),
                Arc::from(include_str!("../../elec/fixtures/parts/avr/ATMEGA328P_TQFP32.part.splash"))),
            ("ledavr.ckt.splash".into(), Arc::from(include_str!("../../elec/fixtures/ledavr.ckt.splash"))),
            ("led.bench.splash".into(), Arc::from(BENCH)),
            ("led.fw.splash".into(), Arc::from(COUNT)),
        ]));
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut host = ElecHost::start(&mut cx, 0, None, docs, Arc::default(),
            Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        host.firmware.statements = true;
        let state = |host: &ElecHost| -> Option<FwState> {
            host.sim.as_ref().and_then(|sim| sim.values()).and_then(|values| values.co.as_ref())
                .and_then(|co| co.get::<FwState>()).cloned()
        };
        let failed = |cx: &Cx| cx.get_global_ref::<ElecLive>().and_then(|live| live.sim.firmware.clone())
            .and_then(|live| live.failed.clone());
        let limited_by = |host: &ElecHost| host.sim.as_ref().expect("the bench runs").status().limited_by;
        // A run that ended stops publishing: a wait that sees it fails at once with the worker's error.
        // Every wait below ends on the run's own progress, never on a clock: a compile result, a
        // publication, and the feed's 4096 batches filling while no frame drains. The device
        // commits at least every 4000 ticks (250 µs), so each handler run sends a batch of its
        // own: 4096 runs, 2.05 s of the bench's 5 s, fill the feed at the latest.
        host.event(&mut cx, &Event::Actions(vec![Box::new(SimAction::Cmd(SimCmd::SetRate(f64::INFINITY))),
            Box::new(SimAction::Cmd(SimCmd::Play))]));
        // The reader registers when the image's log appears, before its device attaches, so it
        // reads from the first event. The run publishes before the image attaches, settled
        // (nothing to do yet): wait for the attached device's own state, whose publication is the
        // one the status reads below, since no frame takes newer values from here.
        let mut reader = None;
        let debug = loop {
            host.event(&mut cx, &Event::Signal);
            assert_eq!(failed(&cx), None, "the counting firmware compiles");
            if reader.is_none() {
                reader = host.sim.as_mut().and_then(|sim| sim.events::<FwFrame>()).map(|store| store.register());
            }
            if let Some(debug) = state(&host).and_then(|state| state.debug).filter(|_| reader.is_some()) { break debug; }
            host.assert_plays_on("the image attaches and its device publishes its table");
            std::thread::sleep(Duration::from_millis(1));
        };
        let reader = reader.expect("the image's log exists");
        assert!(host.sim.as_ref().and_then(|sim| sim.values()).is_some_and(|values| values.running && !values.steady),
            "a running device keeps the run from settling");
        // No UI frames from here: the device fills the feed and holds.
        while limited_by(&host) != Some(Limit::Events) {
            host.assert_plays_on("the device fills the feed while no frame drains it");
            std::thread::sleep(Duration::from_millis(1));
        }
        // One frame takes every batch the full feed holds, each a handler run's own (at least one
        // event), without waiting. Whether the device has filled it again by the time the status
        // is read is the box's speed; the host's own test holds the limit's end.
        let next = |host: &mut ElecHost| host.sim.as_mut().and_then(|sim| sim.events::<FwFrame>()).map_or(0, |store| store.next());
        let before = next(&mut host);
        host.event(&mut cx, &Event::Signal);
        let took = next(&mut host).saturating_sub(before);
        assert!(took >= fab_elec_simhost::events::BATCHES as u64, "one frame drains the full feed: {took} events");
        let rev = host.driver.snap().and_then(|snap| snap.docs.as_ref())
            .and_then(|docs| docs.texts.iter().find(|doc| doc.name == "led.fw.splash")).map(|doc| doc.rev);
        assert_eq!(Some(debug.source_rev), rev, "the device's table names the committed firmware revision");

        // A statement mark is an effect boundary, and a handler's state scalars are stored back at
        // its end: statements with no effect of their own begin where the next effect does (here
        // both at the store-back). A statement event names every statement beginning at its
        // address, in table order.
        let lines = |pc: u32| -> Vec<Option<u32>> {
            let at: Vec<_> = debug.stmts.iter().filter(|stmt| stmt.addr == Some(pc)).map(|stmt| Some(stmt.line)).collect();
            if at.is_empty() { vec![None] } else { at }
        };
        let source_line = |text: &str| COUNT.lines().position(|l| l.contains(text)).map(|n| n as u32 + 1);
        let expected = [source_line("n = n + 1"), source_line("m = m ^ n")];
        let store = host.sim.as_mut().and_then(|sim| sim.events::<FwFrame>()).expect("the log holds firmware frames");
        let events: Vec<_> = store.read(reader).cloned().collect();
        assert!(events.windows(2).all(|w| w[0].t <= w[1].t), "events arrive in time order");
        let (mut runs, mut seq, mut entered) = (0usize, None::<u8>, None::<f64>);
        let mut inside: Option<Vec<Option<u32>>> = None;
        // A failed run names its place in the read: its index, time and the events around it.
        let around = |i: usize| format!("event {i} of {}: {:?}", events.len(),
            events[i.saturating_sub(8)..(i + 3).min(events.len())].iter().map(|e| (e.t, &e.frame)).collect::<Vec<_>>());
        for (i, event) in events.iter().enumerate() {
            match &event.frame {
                FwFrame::Image(frame) => {
                    assert_eq!(event.source, fab_elec_simhost::events::EventSource::Image, "watch frames are the image's");
                    if let Some(last) = seq {
                        assert_eq!(frame.seq, last.wrapping_add(1), "no watch frame is lost: {frame:?}");
                    }
                    seq = Some(frame.seq);
                    assert!(!matches!(frame.event, watch::Event::Lost { .. }), "the image loses no watch record");
                }
                FwFrame::Enter { .. } => {
                    assert_eq!(event.source, fab_elec_simhost::events::EventSource::Trace, "entries are the trace's");
                    assert!(inside.is_none(), "handlers do not nest");
                    if let Some(last) = entered {
                        assert!(event.t - last < 1.5 * PERIOD, "a run on every period, no gap: {} s after the last", event.t - last);
                    }
                    entered = Some(event.t);
                    inside = Some(Vec::new());
                }
                FwFrame::Statement { pc } => {
                    if let Some(run) = inside.as_mut() { run.extend(lines(*pc)); }
                }
                FwFrame::Exit { .. } => {
                    if let Some(run) = inside.take() {
                        assert_eq!(run, expected, "every run begins the handler's two statements, in order: run {runs}, {}; statements {:?}", around(i),
                            debug.stmts.iter().map(|s| (s.line, s.addr)).collect::<Vec<_>>());
                        runs += 1;
                    }
                }
                FwFrame::ImageBad(bad) => panic!("the image's stream decodes: {bad:?}"),
                FwFrame::Note(_) => {}
            }
        }
        assert!(runs > 1000, "the read covers many handler runs: {runs}");
        host.event(&mut cx, &Event::Actions(vec![Box::new(SimAction::Cmd(SimCmd::Stop))]));
    }

    /// A press from a view reaches the firmware through the board, with no document edit: the
    /// button's press source closes its switch, the firmware reads PD2 low and sinks LED0, and the
    /// release lets PD2 rise and LED0 go dark again. The board's first run waits for its firmware's
    /// first build, so it runs the firmware from t = 0 and nothing restarts it. The bench names no
    /// take: the open run is there for every step however long a loaded box takes to pause it.
    #[test]
    fn a_live_button_press_reaches_the_firmware_through_the_board() {
        use fab_elec_mcu_cosim::FwState;
        use fab_elec_simhost::host::MIN_PRESS;
        const FOLLOW: &str = "let button = input(PD2, pull_up)\nlet led = output(PD6)\nfn setup() { write(led, high) }\n\
            let follow = every(ms(1), fn() { write(led, if read(button) == low { low } else { high }) })\n";
        const BENCH: &str = r#"
bench("press", {circuit: "ledavr"}, fn(b, c) {
    b.firmware(c.mcu.u, "follow.fw.splash", {})
})
"#;
        let docs = Arc::new(fab_model::Docs::from([
            ("parts/avr/ATMEGA328P_TQFP32.part.splash".into(),
                Arc::from(include_str!("../../elec/fixtures/parts/avr/ATMEGA328P_TQFP32.part.splash"))),
            ("ledavr.ckt.splash".into(), Arc::from(include_str!("../../elec/fixtures/ledavr.ckt.splash"))),
            ("press.bench.splash".into(), Arc::from(BENCH)),
            ("follow.fw.splash".into(), Arc::from(FOLLOW)),
        ]));
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut host = ElecHost::start(&mut cx, 0, None, docs, Arc::default(),
            Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let lit = |host: &ElecHost| {
            let (Some(sim), Some(snap)) = (&host.sim, host.driver.snap()) else { return false };
            let Some(unit) = snap.design.unit_by_name("ledavr") else { return false };
            let Some(led) = unit.circuit.comp("lamps[0].d") else { return false };
            let part = fab_elec_core::CompRef { board: unit.id, comp: led.id };
            sim.leds(&snap.design).iter().any(|led| led.part == part && led.i > 1e-3)
        };
        let t = |host: &ElecHost| host.sim.as_ref().and_then(|sim| sim.values()).map_or(0.0, |values| values.t);
        // Each wait ends on the run's own progress (its publications), never on a clock.
        let until = |host: &mut ElecHost, cx: &mut Cx, property: &str, done: &dyn Fn(&ElecHost, &Cx) -> bool| loop {
            host.event(cx, &Event::Signal);
            if done(host, cx) { break; }
            if let Some(error) = host.sim.as_ref().and_then(|sim| sim.error()) {
                panic!("{property}: the run ended: {error}");
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        let send = |host: &mut ElecHost, cx: &mut Cx, cmd: SimCmd| host.event(cx, &Event::Actions(vec![Box::new(SimAction::Cmd(cmd))]));

        send(&mut host, &mut cx, SimCmd::Play);
        until(&mut host, &mut cx, "the host is made", &|host, _| host.sim.is_some());
        until(&mut host, &mut cx, "the image runs for 5 ms with the button released", &|host, cx| {
            cx.get_global_ref::<ElecLive>().and_then(|live| live.sim.firmware.clone()).is_some_and(|fw| fw.running_rev.is_some())
                && t(host) > 5e-3
                || { host.assert_plays_on("the image runs for 5 ms with the button released"); false }
        });
        assert!(!lit(&host), "released: PD2 reads high, so PD6 stays high and LED0 dark");
        let sim = host.sim.as_ref().expect("the host is made");
        let reset = sim.values().and_then(|v| v.co.as_ref()).and_then(|co| co.get::<FwState>()).map(|state| state.t_reset);
        assert_eq!(reset, Some(0.0), "the first run waited for the first build: the firmware runs from t = 0");
        assert!(sim.restart_pending().is_none() && sim.content().is_some_and(|c| c.edit.is_none()), "nothing restarted the first run: it is named by no edit, and no restart waits");
        let buttons = cx.get_global::<ElecLive>().sim.index.momentary().to_vec();
        let [button] = buttons.as_slice() else { panic!("the running lowering offers ledavr's one button: {buttons:?}") };

        // The run pauses, so the host's `t` is the worker's: each press lands at that instant, and
        // each check reads the run where a `Step` ends, at a simulated time the test names. A
        // playing run is read only through its newest publication, which a busy box can leave a
        // tap's whole hold behind.
        let stands = |host: &ElecHost, at: f64| host.sim.as_ref().and_then(|sim| sim.values()).is_some_and(|v| !v.running && v.t >= at - 1e-9);
        send(&mut host, &mut cx, SimCmd::Pause);
        until(&mut host, &mut cx, "the run stands", &|host, _| stands(host, 0.0));
        let step = |host: &mut ElecHost, cx: &mut Cx, property: &str, dt: f64| {
            let to = t(host) + dt;
            send(host, cx, SimCmd::Step(dt));
            until(host, cx, property, &|host, _| stands(host, to));
        };

        // held: lit within the firmware's millisecond poll; released: dark within the next, once
        // the press has lasted MIN_PRESS
        send(&mut host, &mut cx, SimCmd::press(*button, true));
        step(&mut host, &mut cx, "pressed: 5 ms of simulated time", 5e-3);
        assert!(lit(&host), "pressed: the firmware reads PD2 low within a millisecond poll and sinks LED0");
        send(&mut host, &mut cx, SimCmd::press(*button, false));
        step(&mut host, &mut cx, "released: the press's MIN_PRESS and 5 ms", MIN_PRESS);
        assert!(!lit(&host), "released: PD2 rises on its pull-up and the firmware lets LED0 go dark");

        // A tap (the release in the same frame as the press) still holds the button for
        // `MIN_PRESS` of simulated time, longer than a firmware's poll, then lets it go.
        send(&mut host, &mut cx, SimCmd::press(*button, true));
        send(&mut host, &mut cx, SimCmd::press(*button, false));
        step(&mut host, &mut cx, "tapped: 5 ms into the hold", 5e-3);
        assert!(lit(&host), "tapped: the firmware sees a tap held for MIN_PRESS of simulated time");
        step(&mut host, &mut cx, "tapped: 5 ms after the hold", MIN_PRESS);
        assert!(!lit(&host), "tapped: the tap's release follows once its press has lasted MIN_PRESS");
        send(&mut host, &mut cx, SimCmd::Stop);
    }

    /// The bridge's one gated call reaches the plate (EMECH's bridge): a script reading the
    /// board builds from the host's inputs, a moved board input gives the plate new content, a move
    /// the case does not read sets no env, and switching documents keeps the host's inputs.
    #[test]
    fn a_moved_board_input_gives_the_plate_new_content() {
        let mut design = ledctl();
        let name = design.units[0].name.clone();
        let inputs = |d: &Design| {
            let unit = &d.units[0];
            let board = unit.board.as_deref().expect("the fixture has a board");
            board_inputs(&unit.name, board, &unit.circuit, &board.drills())
        };
        let mut case = CaseInputs::default();
        assert!(case.update(inputs(&design)), "a new board sets the env");
        let script = format!("{}let pcb = board(\"{name}\")\nlet a = m.box({{size: [10mm 10mm pcb.thickness]}})\nobject(a)\n", fab_model::program::EMPTY);
        let project = "use mod.fab.*\nscript(\"t.cad.splash\")\nplace(\"t\", object: \"a\", at: [150mm 150mm], name: \"Part\")\n";
        let load = || fab_model::FabModel::load("t", project, [("t.cad.splash".to_string(), script.clone())].into()).expect("loads");
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), std::env::temp_dir());
        session.model = load();
        session.path = None;
        session.model.set_env(case.env());
        let before = session.rebuild().expect("a script reading the board builds from the host's inputs").content;

        let board = design.units[0].board.as_ref().expect("the fixture has a board");
        let (hole, via) = (board.holes[0].id, board.vias[0].id);
        let unit = design.units[0].id;
        design.apply(&DesignOp::Board { unit, op: BoardOp::Move { item: via, by: P::mm(0.5, 0.0) } }, false).expect("the via moves");
        assert!(!case.update(inputs(&design)), "a via the case does not read sets no env");
        design.apply(&DesignOp::Board { unit, op: BoardOp::Move { item: hole, by: P::mm(-1.0, 0.0) } }, false).expect("the mounting hole moves");
        assert!(case.update(inputs(&design)), "a moved mounting hole moves the board input");
        session.model.set_env(case.env());
        let after = session.rebuild().expect("the case rebuilds").content;
        assert_ne!(before, after, "a moved board input gives the plate new content");

        let env = session.model.env().clone();
        session.replace_model(load(), None).expect("a document switch keeps the host's inputs, so the board still resolves");
        assert!(session.model.env().same(&env), "the incoming document takes the session's env");
    }

    /// The plate scene is the live design's (11-15, a drag on the board): a part dragged in a
    /// preview moves its body in the 3D view with the preview, not at the drop, and a cancelled
    /// preview puts it back where the committed design has it.
    #[test]
    fn the_plate_scene_follows_a_live_preview() {
        use fab_elec_core::{CompRef, Sel};
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let design = ledctl();
        let unit = design.units[0].id;
        let placed = &design.units[0].board.as_ref().expect("the fixture has a board").placed[0];
        let (item, comp) = (placed.id, placed.comp);
        let mut host = ElecHost::start(&mut cx, 0, None, ledctl_plate(), Arc::default(), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let next_scene = |host: &mut ElecHost, cx: &mut Cx, what: &str| {
            let deadline = Instant::now() + HANG_GUARD;
            loop {
                host.event(cx, &Event::Signal);
                if let Some(scene) = host.take_scene() {
                    return scene;
                }
                assert!(Instant::now() < deadline, "{what}: no plate scene came");
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        let body_at = |scene: &ElecScene| {
            let sel = Sel::Comp(CompRef { board: unit, comp });
            scene.boards[0].bodies.iter().find(|body| body.sel == sel).expect("the dragged part has a body").pose.t
        };
        let sel = Sel::Comp(CompRef { board: unit, comp });
        let loaded = next_scene(&mut host, &mut cx, "loaded");
        let deadline = Instant::now() + HANG_GUARD;
        while host.driver.snap().is_none() {
            host.event(&mut cx, &Event::Signal);
            assert!(Instant::now() < deadline, "the loaded design's snapshot never came");
            std::thread::sleep(Duration::from_millis(2));
        }
        host.act(&mut cx, ViewAction::Preview {
            tag: fab_elec_view::snapshot::next_tag(), place: vec![], notes: vec![],
            ops: vec![DesignOp::Board { unit, op: BoardOp::Move { item, by: P::mm(2.0, 0.0) } }],
            label: "drag".into(),
        });
        assert!(host.previewing(), "a view's preview action starts the edit's motion");
        // A drag's body poses come with its gesture, before the engine's sample: the 3D view
        // moves the dragged body on every pointer move (EBOARD 11-16: the body froze mid-drag).
        let live = host.take_poses().expect("a drag's gesture poses its bodies at once");
        assert_eq!(live.len(), 1, "the gesture poses only the dragged part's body");
        let pose_of = |poses: &[(Sel, Xform3)]| poses.iter().find(|(s, _)| *s == sel).map(|(_, pose)| *pose);
        let posed = pose_of(&live).expect("the gesture poses have the dragged part's body").t;
        let moved = [0, 1, 2].map(|axis| posed[axis] - body_at(&loaded)[axis]);
        assert_eq!(moved, [fab_elec_core::mm(2.0), 0, 0], "the preview's live poses have the part's body 2 mm along");
        let dragged = next_scene(&mut host, &mut cx, "preview");
        let moved = [0, 1, 2].map(|axis| body_at(&dragged)[axis] - body_at(&loaded)[axis]);
        assert_eq!(moved, [fab_elec_core::mm(2.0), 0, 0], "the preview's scene has the part's body 2 mm along");
        assert!(live.iter().all(|(s, pose)| dragged.boards.iter().flat_map(|board| &board.bodies).any(|body| body.sel == *s && body.pose == *pose)),
            "the gesture poses are exactly the dragged scene's body poses");
        host.act(&mut cx, ViewAction::CancelPreview);
        let back = host.take_poses().expect("a cancelled drag poses its bodies back at once");
        assert_eq!(pose_of(&back).map(|pose| pose.t), Some(body_at(&loaded)), "a cancelled drag's poses are the committed ones again");
        let cancelled = next_scene(&mut host, &mut cx, "cancel");
        assert!(!host.previewing(), "the cancelled preview's snapshot ends the motion");
        assert_eq!((body_at(&cancelled), cancelled.hash), (body_at(&loaded), loaded.hash), "a cancelled preview's scene is the committed one again");

        // A drop while the bed is changing is refused under its tag, the one answer a pane waits
        // on (Fable H3, EBOARD 11-16: it was cancelled with no answer), and its bodies go back.
        let tag = fab_elec_view::snapshot::next_tag();
        host.act(&mut cx, ViewAction::Preview {
            tag, place: vec![], notes: vec![], ops: vec![DesignOp::Board { unit, op: BoardOp::Move { item, by: P::mm(2.0, 0.0) } }], label: "drag".into(),
        });
        let _ = host.take_poses();
        let undo = host.undo_step();
        host.set_settled(false);
        host.act(&mut cx, ViewAction::CommitPreview);
        let back = host.take_poses().expect("a refused drop poses its bodies back at once");
        assert_eq!(pose_of(&back).map(|pose| pose.t), Some(body_at(&loaded)), "a refused drop's poses are the committed ones again");
        let deadline = Instant::now() + HANG_GUARD;
        while !host.driver.snap().and_then(|snap| snap.last_refusal.as_ref()).is_some_and(|refusal| refusal.sample == Some(tag)) {
            host.event(&mut cx, &Event::Signal);
            assert!(Instant::now() < deadline, "the drop on a changing bed got no refusal under its tag");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!host.previewing(), "the refused drop ends the motion");
        assert_eq!(host.undo_step(), undo, "the refused drop commits nothing");
        host.set_settled(true);
    }

    /// A plate without electronics has no board, after a plate with one too (user, 11-19: "if i
    /// load a non-electronics thing in fab there still is a rando circuit board present please
    /// remove that"). The host loaded the stock controller for a plate without electronics
    /// documents, and the 3D views drew its board on that plate.
    #[test]
    fn a_plate_without_electronics_has_no_board_after_one_with() {
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let until = |host: &mut ElecHost, cx: &mut Cx, what: &str, done: &mut dyn FnMut(&mut ElecHost) -> bool| {
            let deadline = Instant::now() + HANG_GUARD;
            loop {
                host.event(cx, &Event::Signal);
                if done(host) {
                    return;
                }
                assert!(Instant::now() < deadline, "{what}: the host never got there");
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        let next_scene = |host: &mut ElecHost, cx: &mut Cx, what: &str| {
            let mut scene = None;
            until(host, cx, what, &mut |host| { scene = host.take_scene(); scene.is_some() });
            scene.expect("the scene waited for")
        };
        let mut host = ElecHost::start(&mut cx, 1, None, ledctl_plate(), Arc::default(), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        assert_eq!(next_scene(&mut host, &mut cx, "the controller's plate").boards.len(), 1, "precondition: the controller's plate shows its board");
        host.reload(&mut cx, 2, None, Arc::default(), Arc::default());
        let plain = next_scene(&mut host, &mut cx, "the plain plate after the controller's");
        assert!(plain.boards.is_empty(), "the plain plate's scene holds no board, it holds {}", plain.boards.len());
        until(&mut host, &mut cx, "the plain plate's snapshot", &mut |host| host.design().is_some());
        let units = host.design().expect("the plain plate's design").units.len();
        assert_eq!(units, 0, "the plain plate's design has no unit for a board pane or a list to show");

        let mut fresh = ElecHost::start(&mut cx, 3, None, Arc::default(), Arc::default(), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        assert!(next_scene(&mut fresh, &mut cx, "a plain plate first").boards.is_empty(), "a plain plate opened first shows no board either");
    }

    /// Fable H1 (11-16): a circuit preview's sheet texels come from the preview circuit and its own
    /// row index, once per unit. Pairing the preview's rows with the committed circuit left a part
    /// the preview adds unlit on an emphasised net.
    #[test]
    fn a_circuit_preview_lights_its_parts_from_its_own_circuit_and_rows() {
        use fab_elec_circuit::{CircuitOp, End};
        use fab_elec_core::NetRef;
        use fab_elec_design::{Focus, FocusOrigin};
        use fab_elec_view::StateOf;
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut host = ElecHost::start(&mut cx, 0, None, ledctl_plate(), Arc::default(), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let pump = |host: &mut ElecHost, cx: &mut Cx, what: &str, done: &dyn Fn(&ElecHost) -> bool| {
            let deadline = Instant::now() + HANG_GUARD;
            while !done(host) {
                host.event(cx, &Event::Signal);
                // An event that delivers past the deadline still got there: look before giving up.
                if done(host) {
                    break;
                }
                assert!(Instant::now() < deadline, "{what}: the host never got there");
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        pump(&mut host, &mut cx, "load", &|host| host.driver.snap().is_some());
        let loaded = host.driver.snap().expect("the loaded snapshot").clone();
        let unit = loaded.design.units[0].id;
        let net = |name: &str| loaded.design.units[0].circuit.nets.iter().find(|net| net.name == name)
            .unwrap_or_else(|| panic!("the controller has {name}")).id;
        let btn = net("BTN");
        host.act(&mut cx, ViewAction::Focus(Focus {
            kind: FocusKind::Net, sel: Sel::Net(NetRef { board: unit, net: btn }), origin: FocusOrigin::Sheet,
        }));
        let tag = fab_elec_view::snapshot::next_tag();
        host.act(&mut cx, ViewAction::Preview {
            tag, place: vec![], notes: vec![], label: "add resistor".into(),
            ops: vec![DesignOp::Circuit { unit, op: CircuitOp::AddPart {
                stmt: "let r = e.r(10kohm, _n0, _n1, {package: \"0402\"})".into(),
                after: None, ends: vec![End::Net(btn), End::Net(net("3V3"))], hint: None,
            } }],
        });
        pump(&mut host, &mut cx, "preview", &|host| host.driver.snap().is_some_and(|snap| snap.preview_tag == Some(tag)));
        let snap = host.driver.snap().expect("the preview snapshot").clone();
        let sheet = snap.of(&snap.sheets, unit).expect("the previewed unit has rows");
        assert_eq!(sheet.of, StateOf::Preview, "precondition: the circuit preview publishes its own rows");
        let circuit = &snap.live().unit(unit).expect("the preview keeps the unit").circuit;
        let added = snap.created.iter().find_map(|sel| match sel {
            Sel::Comp(comp) => Some(comp.comp),
            _ => None,
        }).expect("the preview reports the added resistor");
        let states = host.sheet_states.get(&unit).expect("the unit's sheet states are written");
        let lit = |comp: u32| sheet.value.rows.comp(comp).map(|row| states.row(row as usize)[3]);
        assert_eq!(lit(added.0), Some(1.0), "the resistor the preview adds on the emphasised net is lit");
        for (comp, _) in circuit.pins_of(btn) {
            assert_eq!(lit(comp.0), Some(1.0), "every part on the emphasised net is lit from the preview's rows");
        }
        assert!(host.sheet_states_from.get(&unit).is_some_and(|(index, _)| Arc::ptr_eq(index, &sheet.value)),
            "the texels were written from the preview's index");
    }

    /// Why: the engine must keep the bed's other files and restart on outside electronics edits;
    /// a write queued before that edit must never replace it, even before a status confirms it.
    #[test]
    fn outside_changes_reach_the_engine_and_nothing_is_written_over() {
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let bed: Arc<fab_model::Docs> = Arc::new([
            ("ledctl.ckt.splash", include_str!("../../elec/fixtures/ledctl.ckt.splash")),
            ("ledctl.pcb.splash", include_str!("../../elec/fixtures/ledctl.pcb.splash")),
            ("ledctl.bench.splash", include_str!("../../elec/fixtures/ledctl.bench.splash")),
            ("notes.txt", "the bed's notes\n"),
        ].into_iter().map(|(file, text)| (file.to_string(), Arc::<str>::from(text))).collect());
        let mut host = ElecHost::start(&mut cx, 1, None, bed.clone(), Arc::default(), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let idle = |host: &mut ElecHost, cx: &mut Cx, what: &str| {
            let deadline = Instant::now() + HANG_GUARD;
            loop {
                host.event(cx, &Event::Signal);
                if !host.busy() && host.driver.snap().is_some() {
                    return;
                }
                assert!(Instant::now() < deadline, "{what}: the engine reaches idle with a snapshot");
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        idle(&mut host, &mut cx, "loaded");
        let design = &host.driver.snap().expect("the loaded engine has a snapshot").design;
        let unit = design.units[0].id;
        let item = design.units[0].board.as_ref().expect("the loaded fixture has a board").placed[0].id;
        let moved = || ViewAction::Ops {
            ops: vec![DesignOp::Board { unit, op: BoardOp::Move { item, by: P::mm(1.0, 0.0) } }],
            place: vec![],
            label: "move".into(),
        };

        host.act(&mut cx, moved());
        idle(&mut host, &mut cx, "first move");
        let first = host.take_docs().expect("a committed board move hands out documents");
        assert!(!first.docs.contains_key("notes.txt") && first.bases.len() == 1
            && same_files(&first.bases[0].docs, &electronics(&bed)),
            "the engine writes only electronics over the version it loaded");
        let none = fab_model::ElecAssets::new();
        let merged = merge_files(&bed, &none, &first.bases, &first.docs, &first.assets).expect("a write over the loaded version lands").0;
        assert!(merged.get("notes.txt") == bed.get("notes.txt")
            && merged.get("ledctl.pcb.splash") == first.docs.get("ledctl.pcb.splash")
            && merged.get("ledctl.pcb.splash") != bed.get("ledctl.pcb.splash"),
            "the merged write keeps notes and carries the moved board text");

        let merged: Arc<fab_model::Docs> = Arc::new(merged.into_iter()
            .map(|(name, text)| (name, Arc::<str>::from(text.as_ref()))).collect());
        // what an outside edit changed, for Follow AI: none of the engine's own writes, nor other files
        let edits = |actions: &Actions| -> Vec<ElecOutsideEdit> { actions.iter().filter_map(|a| a.downcast_ref::<ElecOutsideEdit>()).cloned().collect() };
        let undo = host.undo_step();
        let sent = cx.capture_actions(|cx| host.reload(cx, 1, None, merged.clone(), Arc::default()));
        assert!(undo.is_some() && host.undo_step() == undo,
            "equal text in new Arcs confirms the write without restarting its undo history");
        assert!(edits(&sent).is_empty(), "the engine's own write seen back is no outside edit");

        let mut added = (*merged).clone();
        added.insert("added.txt".into(), Arc::from("a file added outside the engine\n"));
        let added = Arc::new(added);
        let sent = cx.capture_actions(|cx| host.reload(cx, 1, None, added.clone(), Arc::default()));
        assert_eq!(host.undo_step(), undo, "adding another file does not restart the engine");
        assert!(edits(&sent).is_empty(), "a file no editor shows is no editor's edit");
        host.act(&mut cx, moved());
        idle(&mut host, &mut cx, "second move");
        let second = host.take_docs().expect("the next board move hands out documents");
        let next = merge_files(&added, &none, &second.bases, &second.docs, &second.assets).expect("the next write lands over the confirmed version").0;
        assert_eq!(next.get("added.txt"), added.get("added.txt"),
            "the next write keeps a file added after the engine loaded");
        let undo = host.undo_step();
        host.reload(&mut cx, 1, None, added.clone(), Arc::default());
        assert!(undo.is_some() && host.undo_step() == undo,
            "a known bed version before the pending write is seen back does not restart the engine");
        host.docs_landed(1, &second.docs);
        assert!(host.versions.len() == 1 && Arc::ptr_eq(&host.versions[0].docs, &second.docs),
            "a landed write keeps only itself as a known version, so versions stay the writes in flight");

        let mut outside = (*added).clone();
        outside.insert("ledctl.ckt.splash".into(), Arc::from(format!("{}\n// outside\n", outside["ledctl.ckt.splash"])));
        let outside = Arc::new(outside);
        let sent = cx.capture_actions(|cx| {
            host.reload(cx, 1, None, outside.clone(), Arc::default());
            idle(&mut host, cx, "outside change");
        });
        let part = |host: &ElecHost, refdes: &str| {
            let unit = &host.driver.snap().expect("the restarted engine's design").design.units[0];
            let comp = unit.circuit.comps.iter().find(|c| c.refdes == refdes).expect("the part");
            (fab_elec_core::Sel::Comp(fab_elec_core::ids::CompRef { board: unit.id, comp: comp.id }), comp.value.as_ref().map(|v| v.text.clone()))
        };
        assert_eq!(edits(&sent), vec![ElecOutsideEdit { origin: EditOrigin::Elsewhere, label: "J1 moved".into(), sites: vec![part(&host, "J1").0], editors: vec![ElecEditor::Schematic, ElecEditor::Board], lines: vec![] }],
            "an outside edit, once the engine restarted from it, names each editor whose text differs from the engine's, once (the circuit, and the board this bed holds from before the engine's landed move), and what its design changed: that move undone, at the part");
        assert!(host.undo_step().is_none(), "an outside electronics change restarts the engine's undo history");
        assert!(merge_files(&outside, &none, &second.bases, &second.docs, &second.assets).is_none(),
            "a queued write cannot land over an outside electronics change");
        host.act(&mut cx, moved());
        idle(&mut host, &mut cx, "move after the restart");
        let after = host.take_docs().expect("the restarted engine accepts another board move");
        assert!(after.docs["ledctl.ckt.splash"].contains("// outside"),
            "the restarted engine carries the outside circuit text into its next write");

        // The demo saved as plate p on the same bed, then that plate read again from its files: what
        // they changed came from disk. Another plate on the bed is no edit.
        let landed: Arc<fab_model::Docs> = Arc::new(merge_files(&outside, &none, &after.bases, &after.docs, &after.assets).expect("the write lands").0
            .into_iter().map(|(name, text)| (name, Arc::<str>::from(text.as_ref()))).collect());
        let sent = cx.capture_actions(|cx| host.reload(cx, 1, Some("p".into()), landed.clone(), Arc::default()));
        assert!(edits(&sent).is_empty(), "the engine's own write seen back under the saved plate's id is no edit");
        host.docs_landed(1, &after.docs);
        let mut on_disk = (*landed).clone();
        let reset_cap = "e.c(100nF, swd.NRST";
        assert!(on_disk["ledctl.ckt.splash"].contains(reset_cap), "the fixture's reset capacitor");
        on_disk.insert("ledctl.ckt.splash".into(), Arc::from(on_disk["ledctl.ckt.splash"].replace(reset_cap, "e.c(220nF, swd.NRST")));
        let on_disk = Arc::new(on_disk);
        let sent = cx.capture_actions(|cx| {
            host.reload(cx, 2, Some("p".into()), on_disk.clone(), Arc::default());
            idle(&mut host, cx, "plate read again");
        });
        let refdes = host.driver.snap().expect("the reread plate's design").design.units[0].circuit.comps.iter()
            .find(|c| c.value.as_ref().is_some_and(|v| v.text.contains("220"))).expect("the changed capacitor").refdes.clone();
        let (cap_site, value) = part(&host, &refdes);
        assert_eq!(edits(&sent), vec![ElecOutsideEdit { origin: EditOrigin::Disk, label: format!("{refdes} to {}", value.expect("its value")), sites: vec![cap_site], editors: vec![ElecEditor::Schematic, ElecEditor::Board], lines: vec![] }],
            "the plate on the bed read again names what its files changed, as changed on disk: the one part, by its value, in the schematic its text is and on the board it is placed on");
        let sent = cx.capture_actions(|cx| host.reload(cx, 3, Some("q".into()), landed.clone(), Arc::default()));
        assert!(edits(&sent).is_empty(), "another plate on the bed is no edit, whatever its documents");
    }

    fn idle(host: &mut ElecHost, cx: &mut Cx, what: &str) {
        let deadline = Instant::now() + HANG_GUARD;
        loop {
            host.event(cx, &Event::Signal);
            if !host.busy() && host.driver.snap().is_some() && host.engine_calls.is_empty() && host.read_calls.is_empty() && host.held_calls.is_empty()
                && host.export_calls.is_empty() && host.check_waits.is_empty() && host.text_check.is_none() {
                return;
            }
            assert!(Instant::now() < deadline, "{what}: the engine reaches idle with its calls answered");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    include!("journal/session_test.rs");

    #[test]
    fn board_setting_presses_reach_the_catalog_as_the_person() {
        use makepad_widgets::*;
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        cx.init_cx_os();
        let pane = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            fab_elec_view::live::script_mod(vm);
            fab_elec_view::board::script_mod(vm);
            let value = script_eval!(vm, {
                use mod.widgets.*
                BoardPane{
                    tools: View{a_board := Button{text: "Board"}}
                    settings: View{visible: false
                        thickness := View{value := TextInput{} refusal := View{reason := Label{}}}
                        factory := View{picker := DropDown{} refusal := View{reason := Label{}}}
                        track := View{value := TextInput{} limit := Label{} raised := Label{}}
                        drill := View{value := TextInput{empty_text: "Inherited"}}
                    }
                }
            });
            WidgetRef::script_from_value(vm, value)
        });
        crate::chat::register_elec_actions();
        let mut plate = ledctl_plate();
        let pcb = Arc::make_mut(&mut plate).get_mut("ledctl.pcb.splash").unwrap();
        *pcb = format!("{pcb}\nrules({{track: 0.10mm}})\n").into();
        let mut host = ElecHost::start(&mut cx, 1, None, plate, Arc::default(), Arc::default(), Arc::default());
        idle(&mut host, &mut cx, "loaded");
        let unit = &host.design().unwrap().units[0];
        let before = unit.board.as_ref().unwrap().stackup.thickness;
        pane.borrow_mut::<fab_elec_view::board::BoardPane>().unwrap().set_unit(&mut cx, Some(unit.id));
        let button = pane.widget(&cx, ids!(a_board)).widget_uid();
        let open = cx.capture_actions(|cx| cx.widget_action(button, ButtonAction::Clicked(Default::default())));
        pane.handle_event(&mut cx, &Event::Actions(open), &mut Scope::empty());
        let field = pane.widget(&cx, ids!(thickness.value));
        let old_value = field.text();
        assert_eq!(old_value, fab_elec_core::to_mm(before).to_string());
        let thickness = |host: &ElecHost| host.design().unwrap().units[0].board.as_ref().unwrap().stackup.thickness;
        for (value, enter) in [("1.2", true), ("-1", false)] {
            host.set_settled(false);
            let events = cx.capture_actions(|cx| {
                cx.widget_action(field.widget_uid(), TextInputAction::Changed(value.into()));
                if enter { cx.widget_action(field.widget_uid(), TextInputAction::Returned(value.into(), Default::default())); }
                cx.widget_action(field.widget_uid(), TextInputAction::KeyFocusLost);
            });
            let presses = cx.capture_actions(|cx| pane.handle_event(cx, &Event::Actions(events), &mut Scope::empty()));
            let tags: Vec<_> = presses.iter().filter_map(|a| a.downcast_ref::<BoardEditPress>().map(|p| p.tag)).collect();
            assert_eq!(tags.len(), 1, "one field commit");
            host.event(&mut cx, &Event::Actions(presses));
            assert_eq!(host.held_calls.len(), 1);
            let call = host.held_calls.front().unwrap();
            assert_eq!(call.name, "elec.stackup.set");
            assert_eq!(call.by, By::Person);
            host.set_settled(true);
            let answers = cx.capture_actions(|cx| {
                host.release_calls(cx);
                idle(&mut host, cx, "board settings");
            });
            let mut done = answers.iter().filter_map(|a| a.downcast_ref::<BoardEditDone>());
            let answer = done.next().unwrap();
            assert_eq!(answer.tag, tags[0]);
            assert!(done.next().is_none());
            assert_eq!(answer.refusal.is_some(), !enter);
            pane.handle_event(&mut cx, &Event::Actions(answers), &mut Scope::empty());
            pane.handle_event(&mut cx, &Event::Signal, &mut Scope::empty());
            assert_eq!(thickness(&host), 1_200_000);
            assert_eq!(field.text(), "1.2");
            assert_eq!(pane.widget(&cx, ids!(thickness.refusal)).visible(), !enter);
        }
        host.undo(&mut cx);
        idle(&mut host, &mut cx, "undo thickness");
        pane.handle_event(&mut cx, &Event::Signal, &mut Scope::empty());
        assert_eq!(thickness(&host), before);
        assert_eq!(field.text(), old_value);

        let picker = pane.drop_down(&cx, ids!(factory.picker));
        let snap = host.driver.snap().unwrap();
        let house = snap.profiles.iter().position(|p| p.name == "JLCPCB 2-layer").unwrap();
        assert_eq!(picker.selected_label(), "No factory");
        assert_eq!(pane.widget(&cx, ids!(drill.value)).text(), "");
        assert_eq!(host.design().unwrap().units[0].board.as_ref().unwrap().rules.base.drill, None);
        let select = cx.capture_actions(|cx| cx.widget_action(picker.widget_uid(), DropDownAction::Select(house)));
        let actions = cx.capture_actions(|cx| pane.handle_event(cx, &Event::Actions(select), &mut Scope::empty()));
        host.set_settled(false);
        host.event(&mut cx, &Event::Actions(actions));
        assert_eq!(host.held_calls.len(), 1);
        let call = host.held_calls.front().unwrap();
        assert_eq!((call.name, call.by), ("elec.factory.set", By::Person));
        host.set_settled(true);
        let answers = cx.capture_actions(|cx| { host.release_calls(cx); idle(&mut host, cx, "choose factory"); });
        pane.handle_event(&mut cx, &Event::Actions(answers), &mut Scope::empty());
        pane.handle_event(&mut cx, &Event::Signal, &mut Scope::empty());
        assert_eq!(host.design().unwrap().units[0].factory.as_deref(), Some("JLCPCB 2-layer"));
        assert_eq!(picker.selected_label(), "JLCPCB 2-layer (standard)");
        let raised = pane.widget(&cx, ids!(track.raised));
        assert!(raised.visible());
        assert_eq!(raised.text(), "checks use 0.15 mm (factory)");
        assert_eq!(pane.widget(&cx, ids!(track.value)).text(), "0.1");
        host.undo(&mut cx);
        idle(&mut host, &mut cx, "undo factory");
        pane.handle_event(&mut cx, &Event::Signal, &mut Scope::empty());
        assert_eq!(picker.selected_label(), "No factory");
        assert!(!raised.visible());
    }

    #[test]
    fn tidy_press_stays_busy_until_its_commit_or_refusal() {
        use fab_elec_view::board::tools::BoardStripAction;
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        crate::chat::register_elec_actions();
        let mut host = ElecHost::start(&mut cx, 1, None, ledctl_plate(), Arc::default(), Arc::default(), Arc::default());
        idle(&mut host, &mut cx, "loaded");
        let unit = host.design().unwrap().units[0].id;
        let args = [("part", "mcu.boot_r"), ("at", "[25mm 14mm]"), ("legalise", "@off")];
        host.run_action(&mut cx, "elec.part.place", args.map(|(k, v)| (k.into(), v.into())).into(), By::Person, None);
        idle(&mut host, &mut cx, "overlap");
        let press = || Box::new(BoardStripAction::Tidy { unit, parts: vec![] });
        for moves in [true, false] {
            let before = host.undo_step();
            host.set_settled(false);
            host.event(&mut cx, &Event::Actions(vec![press(), press()]));
            assert!(cx.global::<Tidying>().boards.contains(&unit), "the first press shows busy while queued");
            assert_eq!(host.held_calls.len(), 1, "a repeated press queues no second Tidy");
            host.set_settled(true);
            host.release_calls(&mut cx);
            assert!(cx.global::<Tidying>().boards.contains(&unit), "dispatch keeps the button busy");
            idle(&mut host, &mut cx, "Tidy answer");
            assert!(!cx.global::<Tidying>().boards.contains(&unit), "the answer clears busy");
            assert_eq!(host.undo_step() != before, moves, "only the first Tidy commits");
        }
        let snap = host.driver.snap().unwrap();
        assert_eq!(snap.last_refusal.as_ref().unwrap().text, "Every part already sits on a legal spot.");
    }

    /// The AI's edit tool through the host (`run_action`, stage 1 of the AI edit path): an AI
    /// value edit commits as the part card's edit would, answers its call once with the step it
    /// committed, sends one cue from the commit (its label and its part, origin AI) and undoes
    /// in one press. The same edit by a person through the same path sends no cue; a refused
    /// call answers once with the plan's words; a call caught by a reload answers once.
    #[test]
    fn an_ai_action_commits_as_a_persons_edit_answers_once_and_sends_its_cue() {
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        crate::chat::register_elec_actions();
        let bed: Arc<fab_model::Docs> = Arc::new([
            ("ledctl.ckt.splash", include_str!("../../elec/fixtures/ledctl.ckt.splash")),
            ("ledctl.pcb.splash", include_str!("../../elec/fixtures/ledctl.pcb.splash")),
            ("ledctl.bench.splash", include_str!("../../elec/fixtures/ledctl.bench.splash")),
        ].into_iter().map(|(file, text)| (file.to_string(), Arc::<str>::from(text))).collect());
        let mut host = ElecHost::start(&mut cx, 1, None, bed.clone(), Arc::default(), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let cues = |actions: &Actions| -> Vec<ElecOutsideEdit> { actions.iter().filter_map(|a| a.downcast_ref::<ElecOutsideEdit>()).cloned().collect() };
        let resistance = |host: &ElecHost| host.design().expect("a snapshot").units[0].circuit.comp("mcu.boot_r")
            .and_then(|comp| comp.value.as_ref()?.qty).expect("the boot resistor has a resistance").si;
        let set = |value: &str| vec![("comp".to_string(), "mcu.boot_r".to_string()), ("value".to_string(), value.to_string())];
        idle(&mut host, &mut cx, "loaded");
        let loaded = resistance(&host);
        let unit = host.design().expect("a snapshot").units[0].id;
        let boot = host.design().expect("a snapshot").units[0].circuit.comp("mcu.boot_r").expect("the boot resistor").id;
        let refdes = host.design().expect("a snapshot").units[0].circuit.comp("mcu.boot_r").expect("the boot resistor").refdes.clone();

        let before = host.design().expect("a snapshot").clone();
        let sent = cx.capture_actions(|cx| {
            host.run_action(cx, "elec.value.set", set("4.7k"), By::Ai { turn: 0, call: 7 }, Some(7));
            idle(&mut host, cx, "the AI's value edit");
        });
        let diff = fab_elec_view::live::edit_diff::design_changes(&before, host.design().expect("a snapshot"), &[]);
        let answers = host.take_answers();
        assert!(matches!(answers.as_slice(), [(7, true, _)]), "the AI's call is answered once, as done: {answers:?}");
        assert_eq!(resistance(&host), 4700.0, "the AI's value is the design's");
        let label = format!("Set {refdes} to 4.7k");
        assert!(answers[0].2.contains(&label), "the answer names the step as the undo history does: {}", answers[0].2);
        let step = host.undo_step().expect("the AI's edit is an undo step");
        assert!(answers[0].2.contains(&format!("seq: {step}")), "the answer names the step it committed: {}", answers[0].2);
        let site = Sel::Comp(fab_elec_core::CompRef { board: unit, comp: boot });
        assert_eq!(cues(&sent), vec![ElecOutsideEdit {
            origin: EditOrigin::Ai, label: format!("{refdes} to 4.7k"), sites: vec![site.clone()],
            editors: vec![ElecEditor::Schematic], lines: vec![],
        }], "the AI's commit sends one cue: the change's words and its part, origin AI");
        assert_eq!((diff.label.as_str(), diff.sites.as_slice()), (cues(&sent)[0].label.as_str(), [site].as_slice()),
            "the AI's cue names and lights its change as comparing the designs would for an outside edit");

        host.undo(&mut cx);
        idle(&mut host, &mut cx, "undo");
        assert_eq!(resistance(&host), loaded, "one undo takes the AI's edit back");
        host.redo(&mut cx);
        idle(&mut host, &mut cx, "redo");
        assert_eq!((resistance(&host), host.undo_step()), (4700.0, Some(step)), "redo brings it back as the same step");

        let sent = cx.capture_actions(|cx| {
            host.run_action(cx, "elec.value.set", set("22k"), By::Person, None);
            idle(&mut host, cx, "the person's value edit");
        });
        assert_eq!(resistance(&host), 22000.0, "the person's edit takes the same path");
        assert!(cues(&sent).is_empty() && host.take_answers().is_empty(), "a person's edit sends no cue and answers nobody");

        let refused = vec![("comp".to_string(), "R999".to_string()), ("value".to_string(), "1k".to_string())];
        let sent = cx.capture_actions(|cx| {
            host.run_action(cx, "elec.value.set", refused, By::Ai { turn: 0, call: 8 }, Some(8));
            idle(&mut host, cx, "a refused edit");
        });
        let answers = host.take_answers();
        assert!(matches!(answers.as_slice(), [(8, false, text)] if text.contains("no part R999")), "a refused call answers once with the plan's words: {answers:?}");
        assert!(cues(&sent).is_empty(), "a refused call changes nothing and sends no cue");

        // The checks the Checks pane lists, read by the AI: counts, the stages still running and
        // each finding in the words the edit actions take.
        host.run_action(&mut cx, "elec.check.list", vec![("only".into(), "drc".into())], By::Ai { turn: 0, call: 10 }, Some(10));
        idle(&mut host, &mut cx, "the check list");
        let answers = host.take_answers();
        assert!(matches!(answers.as_slice(), [(10, true, text)] if text.contains("findings: [") && text.contains("current: ")),
            "the check list answers once with the findings: {answers:?}");
        assert!(["erc", "system", "custom"].iter().all(|family| !answers[0].2.contains(&format!("family: {family}"))),
            "a drc-only list lists board findings only: {}", answers[0].2);
        // The counts, the total and the list agree, with a unit or without: a reply that counts
        // errors it does not list (the drum machine's `errors: 4 total: 0 findings: []`) fails.
        let name = host.design().expect("a snapshot").units[0].name.clone();
        let with = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
        for (call, args) in [(30, with(&[])), (31, with(&[("unit", &name)])), (32, with(&[("unit", &name), ("severity", "warning")]))] {
            host.run_action(&mut cx, "elec.check.list", args.clone(), By::Ai { turn: 0, call }, Some(call));
            idle(&mut host, &mut cx, "the check list");
            let answers = host.take_answers();
            let [(_, true, text)] = answers.as_slice() else { panic!("{args:?}: the check list answers once: {answers:?}") };
            let num = |key: &str| text.split(&format!(" {key}: ")).nth(1).and_then(|rest| rest.split(' ').next())
                .and_then(|n| n.parse::<usize>().ok()).unwrap_or_else(|| panic!("{args:?}: {key} in {text}"));
            let listed = text.split(" findings: [").nth(1).map_or(0, |rest| rest.matches("{unit: ").count());
            let counted = num("errors") + num("warnings");
            assert!(call != 30 || counted > 0, "ledctl's checks find something, so the agreement below is tested: {text}");
            assert!(num("total") >= counted && listed == num("total").min(40),
                "{args:?}: every error and warning counted is in the total, and the list holds the total: {counted} counted, {listed} listed: {text}");
        }

        // The AI moves a resistor off its track: the answer waits for the checks to run on the
        // move and names what it broke, as the board marks it; one undo takes the move back.
        let before_move = host.undo_step();
        host.run_action(&mut cx, "elec.part.place", with(&[("part", "lamps[0].r"), ("at", "[31mm 22mm]")]), By::Ai { turn: 0, call: 33 }, Some(33));
        idle(&mut host, &mut cx, "the AI's move");
        let answers = host.take_answers();
        assert!(matches!(answers.as_slice(), [(33, true, text)] if text.contains("checks:") && text.contains("new:")
            && (text.contains("check: \"Dangling\"") || text.contains("check: \"Unconnected\""))),
            "the move answers once, after its checks ran, with the track it left short: {answers:?}");
        host.undo(&mut cx);
        idle(&mut host, &mut cx, "undo of the move");
        assert_eq!(host.undo_step(), before_move, "one undo takes the move back");

        // The AI routes one open net as the board pane's Route does: one undo step, answered once
        // when the job ends with what it routed, and one cue from the commit.
        let open = {
            let snap = host.driver.snap().expect("a snapshot");
            let board = snap.design.unit(unit).and_then(|found| found.board.as_ref()).expect("ledctl's unit has a board");
            let conn = snap.conn.iter().find(|conn| conn.unit == Some(unit) && conn.of == fab_elec_view::StateOf::Committed).expect("ledctl's connectivity");
            board.nets[conn.value.unrouted.first().expect("ledctl has an open net").ix()].name.clone()
        };
        let before_route = host.undo_step();
        let sent = cx.capture_actions(|cx| {
            host.run_action(cx, "elec.route", vec![("nets".into(), open.clone())], By::Ai { turn: 0, call: 11 }, Some(11));
            idle(&mut host, cx, "the AI's route");
        });
        let answers = host.take_answers();
        assert!(matches!(answers.as_slice(), [(11, true, text)] if text.contains("route: {end: @finished, routed: 1, failed: 0")),
            "the route answers once when its job ends, with what it routed: {answers:?}");
        let text = &answers[0].2;
        let gone = text.split_once("gone: ").and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next()?.parse::<usize>().ok());
        assert!(text.contains("checks: {new: [") && gone.is_some_and(|n| n >= 1),
            "the route answers after its checks ran, with the open net's Unconnected finding among those it cleared: {answers:?}");
        assert_ne!(host.undo_step(), before_route, "the AI's route is an undo step");
        assert!(matches!(cues(&sent).as_slice(), [cue] if cue.origin == EditOrigin::Ai && cue.editors.contains(&ElecEditor::Board)),
            "the AI's route sends one cue, on the board: {:?}", cues(&sent));
        let unrouted = |host: &ElecHost| host.driver.snap().expect("a snapshot").conn.iter()
            .find(|conn| conn.unit == Some(unit) && conn.of == fab_elec_view::StateOf::Committed).map_or(0, |conn| conn.value.unrouted.len());
        let left = unrouted(&host);
        host.undo(&mut cx);
        idle(&mut host, &mut cx, "undo of the route");
        assert_eq!(unrouted(&host), left + 1, "one undo takes the AI's route back");

        // The AI writes the board's files as the board strip's Export does (the chat asks the
        // person first): into the project's fabrication folder, each file read back and checked
        // against the board, the person told where they went; then it checks that folder again.
        let project = std::env::temp_dir().join(format!("fab-elec-export-test-{}", std::process::id()));
        host.set_project(Some(&project.join("ledctl.fab.splash")));
        host.run_action(&mut cx, "elec.export", vec![("profile".into(), "jlcpcb 2-layer".into())], By::Ai { turn: 0, call: 12 }, Some(12));
        idle(&mut host, &mut cx, "the AI's export");
        let answers = host.take_answers();
        assert!(matches!(answers.as_slice(), [(12, true, text)] if text.contains("profile: \"JLCPCB 2-layer\"") && text.contains("validation: {valid: true")),
            "the export answers once with its files checked against the board: {answers:?}");
        let written = std::fs::read_dir(project.join("fabrication")).into_iter().flatten().flatten()
            .flat_map(|board| std::fs::read_dir(board.path()).into_iter().flatten().flatten()).count();
        assert!(written > 5, "the export wrote the board's files under the project's fabrication folder: {written}");
        assert!(host.take_notices().iter().all(|(_, words)| words.is_none()), "a clean export tells the person nothing more: the answer says the files check");
        host.run_action(&mut cx, "elec.export.validate", vec![], By::Ai { turn: 0, call: 13 }, Some(13));
        idle(&mut host, &mut cx, "the AI's check of the files");
        let answers = host.take_answers();
        assert!(matches!(answers.as_slice(), [(13, true, text)] if text.contains("valid: true")),
            "checking the newest export again finds the files as the board: {answers:?}");
        let _ = std::fs::remove_dir_all(&project);

        // The AI drives the bench as the time strip's step, the knobs and the voltage tags do: a
        // probe before any run is refused; a step starts the bench and answers once the run stands
        // at its end; a probe reads there; a live set says whether the run hears it; a firmware
        // step of a board with no firmware is refused, never left waiting.
        let deliver = |host: &mut ElecHost, cx: &mut Cx, name: &str, args: &[(&str, &str)], id: u64| {
            let args = args.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect();
            let sent = cx.capture_actions(|cx| host.run_action(cx, name, args, By::Ai { turn: 0, call: id }, Some(id)));
            if !sent.is_empty() {
                host.event(cx, &Event::Actions(sent));
            }
            host.take_answers()
        };
        let net = host.design().expect("a snapshot").units[0].circuit.nets.iter().find(|net| !net.name.is_empty()).expect("ledctl has a named net").name.clone();
        let answers = deliver(&mut host, &mut cx, "elec.sim.probe", &[("target", net.as_str())], 14);
        assert!(matches!(answers.as_slice(), [(14, false, text)] if text.contains("not running")), "a probe before any run is refused: {answers:?}");
        let mut stepped = deliver(&mut host, &mut cx, "elec.sim.step", &[("dt", "1ms")], 15);
        let deadline = Instant::now() + HANG_GUARD;
        while stepped.is_empty() {
            let sent = cx.capture_actions(|cx| {
                host.event(cx, &Event::Signal);
            });
            if !sent.is_empty() {
                host.event(&mut cx, &Event::Actions(sent));
            }
            stepped.extend(host.take_answers());
            if let Some(error) = host.sim.as_ref().and_then(|sim| sim.error()) {
                panic!("the stepped run ended: {error}");
            }
            assert!(Instant::now() < deadline, "the step answers");
            std::thread::sleep(Duration::from_millis(1));
        }
        let values = host.sim.as_ref().and_then(|sim| sim.values()).cloned().expect("the stepped run's values");
        assert!(matches!(stepped.as_slice(), [(15, true, text)] if text.contains("time: ")) && !values.running && values.t >= 1e-3 - 1e-9,
            "the step answers once, when the run stands at its end: {stepped:?} at {} s", values.t);
        let answers = deliver(&mut host, &mut cx, "elec.sim.probe", &[("target", net.as_str())], 16);
        assert!(matches!(answers.as_slice(), [(16, true, text)] if text.contains("unit: \"V\"") && text.contains("value: ")),
            "a probe of a net reads its voltage where the run stands: {answers:?}");
        let answers = deliver(&mut host, &mut cx, "elec.sim.probe", &[("target", refdes.as_str()), ("measure", "mean")], 17);
        assert!(matches!(answers.as_slice(), [(17, false, text)] if text.contains("now only")), "a part reads now only: {answers:?}");
        let answers = deliver(&mut host, &mut cx, "elec.sim.set", &[("comp", "mcu.boot_r"), ("value", "22k")], 18);
        assert!(matches!(answers.as_slice(), [(18, true, text)] if text.contains("effect: ")), "a live set answers whether the run hears it: {answers:?}");
        let answers = deliver(&mut host, &mut cx, "elec.sim.step_fw", &[], 19);
        assert!(matches!(answers.as_slice(), [(19, false, text)] if text.contains("No firmware")), "a firmware step with no firmware is refused: {answers:?}");

        host.run_action(&mut cx, "elec.value.set", set("1k"), By::Ai { turn: 0, call: 9 }, Some(9));
        host.reload(&mut cx, 2, None, bed.clone(), Arc::default());
        idle(&mut host, &mut cx, "another bed");
        idle(&mut host, &mut cx, "settled on the other bed");
        let answers = host.take_answers();
        assert!(matches!(answers.as_slice(), [(9, _, _)]), "a call caught by a reload is answered once: {answers:?}");

        let args = vec![("name".into(), "quick print".into()), ("measure".into(), "print.time".into()), ("bound".into(), "{max: 1h}".into())];
        host.settled = true;
        host.run_action(&mut cx, "elec.req.add", args, By::Ai { turn: 0, call: 20 }, Some(20));
        idle(&mut host, &mut cx, "create the first requirements document");
        let name = "ledctl.req.splash";
        let stored = host.driver.snap().unwrap().docs.as_ref().unwrap().texts.iter().find(|d| d.name == name)
            .expect("the first Add puts the requirements document in the engine");
        let (seq, rev) = (host.undo_step().unwrap(), stored.rev);
        let answers = host.take_answers();
        let [(20, true, answer)] = answers.as_slice() else { panic!("the Docs ticket answers once with success: {answers:?}"); };
        let answer = makepad_script::syntax::parse_value(answer).unwrap();
        let field = |key| &answer.field(key).unwrap().value;
        assert_eq!((field("seq").number(), field("rev").number()), (Some(seq as f64), Some(rev as f64)),
            "the reply identifies the undo step and actual revision of the new document");
        assert_eq!((field("label").string(), field("doc").string()), (Some("Add ledctl.req.splash"), Some(name)),
            "the reply names the creation step and its document");
        let [range] = field("lines").items().unwrap() else { panic!("the first statement changes one line range"); };
        assert_eq!(range.items().unwrap().iter().map(|n| n.number()).collect::<Vec<_>>(), [Some(1.), Some(1.)],
            "the first statement starts and ends on line one");
        host.undo(&mut cx);
        idle(&mut host, &mut cx, "undo document creation");
        assert!(host.driver.snap().unwrap().docs.as_ref().unwrap().texts.iter().all(|d| d.name != name),
            "one undo removes the newly created requirements document");

        // Each AI edit's answer says what it did to the person's requirements once they are judged
        // on its commit, and an AI change of a requirement's own text says so in lines to repeat.
        let args = vec![("name".into(), "two layers".into()), ("measure".into(), r#"board("ledctl").layers"#.into()), ("bound".into(), "{max: 2}".into())];
        host.run_action(&mut cx, "elec.req.add", args, By::Ai { turn: 0, call: 21 }, Some(21));
        idle(&mut host, &mut cx, "the AI's first requirement");
        let answers = host.take_answers();
        assert!(matches!(answers.as_slice(), [(21, true, text)] if text.contains("requirements: {met: true changed: [{name: \"two layers\" was: new now: pass")
            && text.contains("requirements_changed: [\"added the requirement `two layers`: board(\\\"ledctl\\\").layers {max: 2}\"]")),
            "the first requirement's answer judges it on its commit and says it was added: {answers:?}");
        let rev = host.driver.snap().unwrap().docs.as_ref().unwrap().texts.iter().find(|d| d.name == name).expect("the requirements document").rev;
        let tighter = fab_elec_host::doc_actions::TextChange::Replace(vec![("{max: 2}".into(), "{max: 1}".into())]);
        let (ok, text, _) = ai_text(&mut host, &mut cx, name, rev, tighter, 22);
        assert!(ok && text.contains("requirements_changed: [\"changed the person's requirement `two layers`: {max: 2} → {max: 1}\"]")
            && text.contains("requirements: {met: false changed: [{name: \"two layers\" was: pass now: fail"),
            "a bound the AI's text moved is said, and the row it fails is in the answer: {text}");
        host.run_action(&mut cx, "elec.part.place", with(&[("part", "lamps[0].r"), ("at", "[31mm 22mm]")]), By::Ai { turn: 0, call: 23 }, Some(23));
        idle(&mut host, &mut cx, "the AI's move with requirements");
        let answers = host.take_answers();
        assert!(matches!(answers.as_slice(), [(23, true, text)] if text.contains("checks: {") && text.contains("requirements: {met: false")
            && !text.contains("two layers") && !text.contains("requirements_changed")),
            "an edit of the board answers with its checks and the requirements judged after them, naming no row it left alone: {answers:?}");

    }

    /// The AI hears what its edit did (stage 2). Its Play is the transport's own action, the
    /// edit's answer says what the commit does to the playing take, and `elec.sim.status` with
    /// `wait: seq` answers once the run has the step: its answer reflects the step, and a
    /// measurement taken after it reads the new circuit. Whether the run already has a retune
    /// when the edit's answer arrives is not promised (the snapshot reaches the run on the same
    /// events that deliver the answer), so nothing here depends on it.
    #[test]
    fn an_ai_wait_answers_once_the_run_has_its_edit_and_a_measurement_after_it_reads_the_new_circuit() {
        /// One event, the actions it sent delivered back as the app delivers them, its answers kept.
        fn pump(host: &mut ElecHost, cx: &mut Cx, event: &Event, answers: &mut Vec<(u64, bool, String)>) {
            let sent = cx.capture_actions(|cx| {
                host.event(cx, event);
            });
            if !sent.is_empty() {
                host.event(cx, &Event::Actions(sent));
            }
            answers.extend(host.take_answers());
        }
        fn until(host: &mut ElecHost, cx: &mut Cx, answers: &mut Vec<(u64, bool, String)>, what: &str, done: &dyn Fn(&ElecHost, &[(u64, bool, String)]) -> bool) {
            let deadline = Instant::now() + HANG_GUARD;
            loop {
                pump(host, cx, &Event::Signal, answers);
                if done(host, answers) {
                    return;
                }
                if let Some(error) = host.sim.as_ref().and_then(|sim| sim.error()) {
                    panic!("{what}: the run ended: {error}");
                }
                assert!(Instant::now() < deadline, "{what}: reached");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        /// An AI call, the actions it sent delivered; true when it sent a transport command.
        fn call(host: &mut ElecHost, cx: &mut Cx, answers: &mut Vec<(u64, bool, String)>, name: &str, args: &[(&str, &str)], id: u64) -> bool {
            let args = args.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect();
            let sent = cx.capture_actions(|cx| host.run_action(cx, name, args, By::Ai { turn: 0, call: id }, Some(id)));
            let transport = sent.iter().any(|action| matches!(action.downcast_ref::<SimAction>(), Some(SimAction::Cmd(_))));
            host.event(cx, &Event::Actions(sent));
            answers.extend(host.take_answers());
            transport
        }
        let answers_of = |answers: &[(u64, bool, String)], id: u64| answers.iter().filter(|a| a.0 == id).cloned().collect::<Vec<_>>();

        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        crate::chat::register_elec_actions();
        // The LED drivers' resistor as one literal they all share, so one value edit sets every
        // LED's current; the always-low PA0 keeps LED0 lit. The take is made open below.
        const DERIVED: &str = "e.r(e.series((p.vdd.volts - d.vf) / o.current - o.sink, @E24), p.vdd, d.A, {package: \"0402\"})";
        let sources = fab_elec_fixtures::docs("ledctl").expect("the controller has fixture documents");
        let docs: fab_model::Docs = sources.docs().into_iter()
            .map(|doc| (doc.name.clone(), Arc::from(doc.text.replace(DERIVED, "e.r(470ohm, p.vdd, d.A, {package: \"0402\"})").replace(OPEN_TAKE.0, OPEN_TAKE.1)))).collect();
        assert!(docs.values().any(|text| text.contains("e.r(470ohm")), "the LED drivers share one resistor literal");
        assert!(docs.values().any(|text| text.contains(OPEN_TAKE.1)), "the bench's 20 ms take, made open, plays on through every wait");
        let mut host = ElecHost::start(&mut cx, 0, None, Arc::new(docs), Arc::default(), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let mut answers = Vec::new();
        let lit = |host: &ElecHost| -> f32 {
            let (Some(sim), Some(design)) = (host.sim.as_ref(), host.design()) else { return 0.0 };
            sim.leds(design).iter().map(|led| led.i).fold(0.0, f32::max)
        };
        until(&mut host, &mut cx, &mut answers, "the first snapshot", &|host, _| host.driver.snap().is_some());

        assert!(call(&mut host, &mut cx, &mut answers, "elec.sim.play", &[], 1), "the AI's Play goes as the transport's own action");
        assert!(matches!(answers_of(&answers, 1).as_slice(), [(1, true, text)] if text.contains("time:")), "Play answers once: {answers:?}");
        assert!(host.sim.as_ref().is_some_and(|sim| sim.status().playing), "and the bench plays");
        until(&mut host, &mut cx, &mut answers, "LED0 lit", &|host, _| lit(host) > 1e-3);
        let before = lit(&host);
        let run = host.sim.as_ref().map(|sim| sim.index().generation);

        let unit = &host.design().expect("a snapshot").units[0];
        let resistor = unit.circuit.comps.iter().find(|comp| comp.path.starts_with("lamps") && comp.refdes.starts_with('R'))
            .expect("an LED driver has its resistor").path.clone();
        call(&mut host, &mut cx, &mut answers, "elec.value.set", &[("comp", &resistor), ("value", "4.7k")], 2);
        until(&mut host, &mut cx, &mut answers, "the edit's answer", &|_, answers| answers.iter().any(|a| a.0 == 2));
        let edit = answers_of(&answers, 2);
        let [(_, true, text)] = edit.as_slice() else { panic!("the edit answers once, done: {edit:?}") };
        assert!(text.contains("state: @playing") && text.contains("effect: @retune"), "the edit says the playing take retunes: {text}");
        let seq: u64 = text.split("seq: ").nth(1).and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next()).and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("the edit's answer names its step: {text}"));

        // A playing run's wait answers once the run has published with the step: on the call when
        // its newest values were already stepped with every command sent and are past the time
        // the edit went in at where the take is heard (a quiet moment of the run, which a loaded
        // box can hit), else on a later publication, never before. A loaded box can publish while
        // the run renders again from its snapshot up to that time, still the old circuit.
        let published = host.reflects() >= seq
            && host.sim.as_ref().is_some_and(|sim| sim.stepped_past(sim.sent_mark()) && sim.status().rerender.is_none()
                && sim.heard_edit().is_none_or(|(_, from)| sim.status().t >= from));
        call(&mut host, &mut cx, &mut answers, "elec.sim.status", &[("wait", &seq.to_string())], 3);
        assert_eq!(!answers_of(&answers, 3).is_empty(), published,
            "the wait answers on the call exactly when the playing run has already published with the step");
        until(&mut host, &mut cx, &mut answers, "the wait's answer", &|host, answers| {
            let answered = answers.iter().any(|a| a.0 == 3);
            assert!(!answered || host.reflects() >= seq, "the wait answers only once the run reflects the step");
            answered
        });
        let waited = answers_of(&answers, 3);
        let [(3, true, text)] = waited.as_slice() else { panic!("the wait answers once, done: {waited:?}") };
        let reflects: u64 = text.split("reflects: ").nth(1).and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next()).and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("the wait's answer names the step the run reflects: {text}"));
        assert!(reflects >= seq, "the wait's answer reflects the edit: reflects {reflects}, the edit's step {seq}");
        let after = lit(&host);
        assert!(after < before * 0.5, "a measurement after the wait reads the new circuit: {after} A, {before} A before");
        assert_eq!(host.sim.as_ref().map(|sim| sim.index().generation), run, "the value was retuned into the playing run, not a new run");
        assert_eq!(host.take_effect(CircuitChange::Structure), ("playing", "restart"), "a structural commit says the playing take starts over");

        call(&mut host, &mut cx, &mut answers, "elec.sim.status", &[("wait", &(seq + 50).to_string())], 4);
        assert!(matches!(answers_of(&answers, 4).as_slice(), [(4, false, text)] if text.contains("no step")), "a wait on a step not made is refused: {answers:?}");

        // The Scope's tools (`elec_scope.rs`): the AI probes the 3V3 rail as + Add probe does (the
        // bench's own step, by the AI), records the playing run from the rail's 3 V on (the step
        // may start the run over) and measures the recording; the ended capture comes off the bench.
        call(&mut host, &mut cx, &mut answers, "elec.probe.add", &[("target", "3V3")], 6);
        until(&mut host, &mut cx, &mut answers, "the probe's answer", &|_, answers| answers.iter().any(|a| a.0 == 6));
        let probed = answers_of(&answers, 6);
        assert!(matches!(probed.as_slice(), [(6, true, text)] if text.contains("site: {file: \"ledctl.bench.splash\" line: ") && !text.contains("line: 0}")),
            "the probe is the bench's step, written in the bench's text: {probed:?}");
        assert!(host.driver.snap().and_then(|snap| snap.benches.first()).is_some_and(|bench| bench.out.probes.iter().any(|probe| probe.of.target == "3V3")),
            "the bench probes the rail");
        call(&mut host, &mut cx, &mut answers, "elec.capture.run", &[("trigger", "level"), ("level", "3V"), ("to", "1ms"), ("timeout", &format!("{}s", HANG_GUARD.as_secs()))], 7);
        until(&mut host, &mut cx, &mut answers, "the capture's answer", &|_, answers| answers.iter().any(|a| a.0 == 7));
        let captured = answers_of(&answers, 7);
        let [(7, true, text)] = captured.as_slice() else { panic!("the capture answers once, done: {captured:?}") };
        assert!(text.contains("state: @done"), "the playing run records the probed lane: {text}");
        let capture = text.split("capture: \"").nth(1).and_then(|rest| rest.split('"').next()).expect("the answer names its capture").to_string();
        assert!(host.scope.sinks.is_empty(), "the ended capture is off the bench");
        call(&mut host, &mut cx, &mut answers, "elec.measure", &[("capture", &capture), ("what", "mean")], 8);
        until(&mut host, &mut cx, &mut answers, "the measure's answer", &|_, answers| answers.iter().any(|a| a.0 == 8));
        let measured = answers_of(&answers, 8);
        let [(8, true, text)] = measured.as_slice() else { panic!("the measure answers once: {measured:?}") };
        let mean: f64 = text.split("value: ").nth(1).and_then(|rest| rest.split(|c: char| !(c.is_ascii_digit() || c == '.')).next())
            .and_then(|n| n.parse().ok()).unwrap_or_else(|| panic!("the measure gives a value: {text}"));
        assert!(text.contains("V") && (2.5..4.0).contains(&mean), "the recording reads the 3.3 V rail: {text}");

        // The bench's expectations, judged on the run as the Bench editor shows them
        // (`fab_elec_bench::judge`): a run of them starts the run over as the transport's Power
        // cycle does and counts each verdict once the run has judged them all: the rail's voltage
        // passes, and every LED's current is judged, LED0's as the run publishes it (the 4.7k above
        // dims every LED below its bounds). All nine rows answer at the default budget: whole rows
        // when they fit (their numbers' lengths vary with the run), else compact ones.
        call(&mut host, &mut cx, &mut answers, "elec.expect.run", &[("repeats", "1"), ("timeout", &format!("{}s", HANG_GUARD.as_secs()))], 9);
        until(&mut host, &mut cx, &mut answers, "the expectations' answer", &|_, answers| answers.iter().any(|a| a.0 == 9));
        let judged = answers_of(&answers, 9);
        let [(9, true, text)] = judged.as_slice() else { panic!("the expectations' run answers once: {judged:?}") };
        assert!(text.contains("state: \"done\""), "the restarted run judges every expectation: {text}");
        assert!(text.len() <= 4096 && text.contains("total: 9") && !text.contains("next: "),
            "the nine-row bench answers whole at the default budget: {text}");
        let row = |name: &str| text.split(&format!("name: \"{name}\"")).nth(1).unwrap_or_else(|| panic!("{name} is listed: {text}")).to_string();
        let counts = |name: &str| row(name).split("repeats: {").nth(1).and_then(|rest| rest.split('}').next()).unwrap_or_default().to_string();
        assert!(counts("3V3 up").contains("pass: 1"), "the rail is up on the restarted run: {text}");
        assert!((0..8).all(|i| counts(&format!("LED{i}")).contains("not_judged: 0")), "every LED's current is judged on the run: {text}");
        let led0: f32 = row("LED0").split("sim: {").nth(1).and_then(|rest| rest.split("value: ").nth(1))
            .and_then(|rest| rest.split(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | 'e' | '-'))).next()).and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("LED0 has its judged value: {text}"));
        assert!((led0 - after).abs() < after * 0.1, "the judge reads LED0's current as the run publishes it: {led0} A judged, {after} A lit");
        assert!(host.sim.as_ref().is_some_and(|sim| sim.judged().lock().unwrap().run == sim.runs()), "the verdicts are of the restarted run");

        // The counted check (`elec.bench.run`, EXPECTATIONS' Check bench): a run of its own of
        // the edited circuit from power-on to its stop, every point judged, beside the live run,
        // which it neither restarts nor judges for: the rail passes, and LED0, dimmed by the 4.7k,
        // fails with when it first did.
        let runs = host.sim.as_ref().map(|sim| sim.runs());
        call(&mut host, &mut cx, &mut answers, "elec.bench.run", &[("tstop", "8ms")], 10);
        until(&mut host, &mut cx, &mut answers, "the check's answer", &|_, answers| answers.iter().any(|a| a.0 == 10));
        let checked = answers_of(&answers, 10);
        let [(10, true, text)] = checked.as_slice() else { panic!("the check answers once: {checked:?}") };
        assert!(text.contains("bench: \"bringup\"") && text.contains("state: \"done\"") && text.contains("t: \"8ms\""), "the check runs the bench to its stop: {text}");
        let item = |name: &str| text.split(&format!("name: \"{name}\"")).nth(1).and_then(|rest| rest.split("name: ").next())
            .unwrap_or_else(|| panic!("{name} is checked: {text}")).to_string();
        assert!(item("3V3 up").contains("outcome: \"pass\""), "the rail passes the check: {text}");
        assert!(item("LED0").contains("outcome: \"fail\"") && item("LED0").contains("first_failure: {t: \""), "LED0 fails the check, with its first failure: {text}");
        assert_eq!(host.sim.as_ref().map(|sim| sim.runs()), runs, "the check started no run of the bench's own");
        assert!(host.check.as_ref().is_some_and(|check| check.rx.is_none() && check.shown.report.as_ref().is_some_and(|r| r.points > 100 && !r.stopped)),
            "the check's report is up for the EXPECTATIONS shelf");

        // The take kept and shown again (the recording bar's Save trace, a saved trace in the
        // Scope menu, Back to live): saved beside the project as the first take, shown in the
        // Scope tab in place of the run with the bench's lanes, then the run again; a path out of
        // the project's captures is refused.
        let project = std::env::temp_dir().join(format!("fab-elec-trace-test-{}", std::process::id()));
        host.set_project(Some(&project.join("ledctl.fab.splash")));
        let answered = |id: u64| move |_: &ElecHost, answers: &[(u64, bool, String)]| answers.iter().any(|a| a.0 == id);
        call(&mut host, &mut cx, &mut answers, "elec.capture.save", &[], 20);
        until(&mut host, &mut cx, &mut answers, "the take's save", &answered(20));
        let saved = answers_of(&answers, 20);
        assert!(matches!(saved.as_slice(), [(_, true, text)] if text.contains("path: \"captures/take-1.trace\"")) && project.join("captures/take-1.trace").is_file(),
            "the take is saved beside the project as its first: {saved:?}");
        call(&mut host, &mut cx, &mut answers, "elec.capture.open", &[("path", "captures/take-1.trace")], 21);
        until(&mut host, &mut cx, &mut answers, "the trace's opening", &answered(21));
        let opened = answers_of(&answers, 21);
        let shown = |cx: &Cx| cx.get_global_ref::<ElecLive>().and_then(|live| live.opened.as_ref().map(|o| (o.path.clone(), o.layout.channels.len(), o.store.len())));
        assert!(matches!(opened.as_slice(), [(_, true, text)] if text.contains("channels: [\"")), "the opening answers once with the trace's lanes: {opened:?}");
        assert!(shown(&cx).is_some_and(|(path, lanes, samples)| path == "captures/take-1.trace" && lanes > 0 && samples > 1), "the Scope shows the saved take: {:?}", shown(&cx));
        // the take is a capture the AI reads as it reads a capture of the run
        let take = opened[0].2.split("capture: \"").nth(1).and_then(|rest| rest.split('"').next()).unwrap_or_else(|| panic!("the opening names its capture: {opened:?}")).to_string();
        call(&mut host, &mut cx, &mut answers, "elec.measure", &[("capture", &take), ("what", "max")], 24);
        until(&mut host, &mut cx, &mut answers, "the take's measure", &answered(24));
        assert!(matches!(answers_of(&answers, 24).as_slice(), [(_, true, text)] if text.contains("value: ")), "the opened take measures: {:?}", answers_of(&answers, 24));
        call(&mut host, &mut cx, &mut answers, "elec.capture.close", &[], 22);
        until(&mut host, &mut cx, &mut answers, "Back to live", &answered(22));
        assert!(matches!(answers_of(&answers, 22).as_slice(), [(_, true, text)] if text.contains("closed: \"captures/take-1.trace\"")) && shown(&cx).is_none(),
            "Back to live shows the run again: {:?}", answers_of(&answers, 22));
        call(&mut host, &mut cx, &mut answers, "elec.capture.open", &[("path", "../take-1.trace")], 23);
        until(&mut host, &mut cx, &mut answers, "a path out of the captures", &answered(23));
        assert!(matches!(answers_of(&answers, 23).as_slice(), [(_, false, _)]) && shown(&cx).is_none(), "a path out of the project's captures is refused");
        let _ = std::fs::remove_dir_all(&project);

        call(&mut host, &mut cx, &mut answers, "elec.sim.pause", &[], 5);
        assert!(matches!(answers_of(&answers, 5).as_slice(), [(5, true, _)]) && host.sim.as_ref().is_some_and(|sim| !sim.status().playing),
            "Pause answers once and holds the bench");
    }

    #[test]
    fn microphone_and_network_actions_use_the_running_board() {
        use crate::elec_mic::MicPlayback;
        use fab_elec_mcu_cosim::{net::{self, Arrival}, FwState};
        use makepad_script::syntax::{parse_value, Node};
        use std::path::Path;

        fn pump(host: &mut ElecHost, mic: &mut MicPlayback, cx: &mut Cx, mut event: Event, project: Option<&Path>) {
            loop {
                let actions = cx.capture_actions(|cx| {
                    host.event(cx, &event);
                    mic.handle_event(cx, &event, host.sim_mut(), 0, project);
                });
                if actions.is_empty() { break; }
                event = Event::Actions(actions);
            }
        }
        fn field<'a>(node: &'a Node, key: &str) -> &'a Node {
            &node.field(key).unwrap_or_else(|| panic!("the answer includes {key}")).value
        }
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        crate::chat::register_elec_actions();
        let mut docs: fab_model::Docs = fab_elec_fixtures::docs("handheld").unwrap().docs().iter()
            .filter(|doc| doc.is_file()).map(|doc| (doc.name.clone(), doc.text.clone())).collect();
        docs.insert("handheld.fw.splash".into(), Arc::from(
            "let rx = mic(16000, 20, fn() {})\nlet conn = net(net_websocket, 2, 32, fn() {})\nfn setup() { net_open(conn) }"));
        docs.insert("handheld.bench.splash".into(), Arc::from(r#"
bench("board", {circuit: "handheld"}, fn(b, c) {
    b.supply(c.net("VBUS"), {v: 5V})
    b.firmware(c.mcu, "handheld.fw.splash", {})
    b.net(c.mcu, 0, {})
})"#));
        let mut host = ElecHost::start(&mut cx, 0, None, Arc::new(docs), Arc::default(), Arc::default(), Arc::default());
        let mut mic = MicPlayback::default();
        pump(&mut host, &mut mic, &mut cx, Event::Actions(vec![Box::new(SimAction::Cmd(SimCmd::Play))]), None);
        let (log, status) = loop {
            pump(&mut host, &mut mic, &mut cx, Event::Signal, None);
            if let Some(fw) = cx.get_global_ref::<ElecLive>().and_then(|l| l.sim.firmware.as_ref()) {
                assert!(fw.failed.is_none() && fw.problem.is_none(), "the board fixture compiles: {:?} {:?}", fw.failed, fw.problem);
            }
            // The board ready on the run that stays: hearing the board's speaker starts the run
            // over at the audio rate, and a board a loaded box saw ready on the run before was gone
            // before its WAV could play.
            let stays = host.sim.as_ref().is_some_and(|sim| sim.restart_pending().is_none() && sim.status().sample_rate == sim.sample_rate());
            if let Some(state) = host.sim.as_ref().filter(|_| stays).and_then(SimHost::values).and_then(|v| v.co.as_ref()).and_then(|co| co.get::<FwState>()) {
                if let Some(status) = state.net.as_ref().filter(|n| n.conns.iter().any(|c| c.state == net::READY)) {
                    break (state.net_log.clone().expect("the ready board publishes its input log"), status.clone());
                }
            }
            host.assert_plays_on("the board reaches its echo endpoint");
            std::thread::sleep(Duration::from_millis(1));
        };

        let mut id = 0;
        let mut call = |name: &str, args: &[(&str, &str)], project: Option<&Path>| {
            id += 1;
            let args = args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            let actions = cx.capture_actions(|cx| host.run_action(cx, name, args, By::Ai { turn: 0, call: id }, Some(id)));
            pump(&mut host, &mut mic, &mut cx, Event::Actions(actions), project);
            loop {
                let answers = host.take_answers();
                if !answers.is_empty() {
                    assert_eq!(answers.len(), 1, "{name} answers once");
                    let (answered, ok, text) = answers.into_iter().next().unwrap();
                    assert_eq!(answered, id, "{name} answers its own call");
                    break (ok, text);
                }
                host.assert_plays_on(name);
                std::thread::sleep(Duration::from_millis(1));
                pump(&mut host, &mut mic, &mut cx, Event::Signal, project);
            }
        };
        let record = |ms: u64, arrival| {
            let ns = log.lock().unwrap().record(ms * 1_000_000 - 1, arrival);
            assert_eq!(ns, ms * 1_000_000, "the scripted arrival has its board time");
        };
        let (c, gen) = (0, 7);
        let prefix = format!("a\r\n{}", "é".repeat(117));
        record(10, Arrival::Opened { c, gen });
        record(11, Arrival::Message { c, gen, binary: false, bytes: format!("{prefix}tail").into_bytes() });
        record(12, Arrival::Message { c, gen, binary: true, bytes: (0xf6..=0xff).collect() });
        let (ok, first) = call("elec.sim.net", &[], None);
        assert!(ok && first.contains("a\\r\\n"), "the text preview escapes line breaks: {first}");
        let page = parse_value(&first).unwrap();
        let next = field(&page, "next").number().unwrap() as usize;
        assert_eq!(next, 3, "the default page starts at zero and consumes the three arrivals");
        let entries = field(&page, "arrivals").items().unwrap();
        assert_eq!(entries.len(), 3, "the first page contains exactly the logged arrivals");
        assert_eq!(field(&entries[1], "text").string(), Some(prefix.as_str()), "the text prefix counts Unicode characters");
        assert_eq!(field(&entries[2], "length").number(), Some(10.0), "binary keeps its full byte count");
        assert_eq!(field(&entries[2], "hex").string(), Some("f6f7f8f9fafbfcfd"), "binary shows only its first eight bytes");
        assert_eq!(field(&page, "up").number(), Some(status.up as f64), "upstream count comes from the host");
        assert_eq!(field(&page, "down").number(), Some(status.down as f64), "downstream count comes from the host");
        assert!(first.contains("connections: [{c: 0, state: @ready, code: 0, detail: 0}]"), "the host reports the ready connection");
        for key in ["bytes_up", "bytes_down"] { assert_eq!(field(&page, key).number(), Some(0.0), "{key} counts host data"); }

        record(13, Arrival::Taken { c, gen, bytes: 440 });
        record(14, Arrival::Ended { c, gen, code: net::code::RESET, detail: 19 });
        let (ok, second) = call("elec.sim.net", &[("from", &next.to_string())], None);
        assert!(ok, "the next cursor reads the later arrivals: {second}");
        let second = parse_value(&second).unwrap();
        let later = field(&second, "arrivals").items().unwrap();
        assert_eq!(later.len(), 2, "the second page omits every previously returned arrival");
        assert_eq!(field(&second, "next").number(), Some(5.0), "the cursor follows the last arrival");
        assert_eq!(field(&later[0], "bytes").number(), Some(440.0), "taken reports its byte count");
        assert_eq!(field(&later[1], "code").string(), Some("reset"), "ended reports its code text");
        assert_eq!(field(&later[1], "detail").number(), Some(19.0), "ended reports its detail");
        for ((index, row), kind) in entries.iter().chain(later).enumerate().zip(["opened", "message", "message", "taken", "ended"]) {
            assert_eq!(field(row, "kind").name(), Some(kind), "arrival {index} keeps log order");
            assert_eq!(field(row, "ms").number(), Some((10 + index) as f64), "arrival {index} has board milliseconds");
            assert_eq!(field(row, "c").number(), Some(f64::from(c)), "arrival {index} identifies the connection");
            assert_eq!(field(row, "gen").number(), Some(f64::from(gen)), "arrival {index} identifies the attempt");
        }

        for ms in 20..60 {
            record(ms, Arrival::Message { c, gen, binary: false, bytes: "🎤".repeat(120).into_bytes() });
        }
        let mut from = 5;
        while from < 45 {
            let (ok, text) = call("elec.sim.net", &[("from", &from.to_string())], None);
            assert!(ok && text.len() <= makepad_ai_services::wire::MAX_RESULT_BYTES, "the log page fits the tool reply");
            let page = parse_value(&text).expect("a full page ends on a whole arrival");
            let rows = field(&page, "arrivals").items().unwrap();
            let next = field(&page, "next").number().unwrap() as usize;
            assert!(!rows.is_empty() && next == from + rows.len(), "every page advances by the arrivals returned");
            if from == 5 { assert!(next < 45, "the backlog needs more than one reply"); }
            for (i, row) in rows.iter().enumerate() {
                assert_eq!(field(row, "ms").number(), Some((15 + from + i) as f64), "paging preserves every arrival in order");
            }
            from = next;
        }
        assert_eq!(from, 45, "the last page reaches the end of the backlog");

        let dir = std::env::temp_dir().join(format!("fab-board-mic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let project = dir.join("board.fab");
        let absolute = dir.join("voice.wav");
        for (file, saved, why) in [(absolute.to_str().unwrap(), Some(project.as_path()), "absolute"),
            ("voice/../other.wav", Some(project.as_path()), ".."), ("voice.wav", None, "Save the project")] {
            let (ok, text) = call("elec.sim.mic", &[("file", file)], saved);
            assert!(!ok && text.contains(why), "the microphone refuses {file} with its reason: {text}");
        }
        let (ok, text) = call("elec.sim.mic", &[("file", "missing.wav")], Some(&project));
        assert!(!ok && text.contains("Cannot load") && text.contains("missing.wav"), "a failed pool read answers the call: {text}");
        std::fs::write(&absolute, makepad_ai_hub::wav::encode_wav_pcm16_mono(&[0.5; 32], 16000)).unwrap();
        let (ok, text) = call("elec.sim.mic", &[("file", "voice.wav")], Some(&project));
        assert!(ok && text.contains("playing: true"), "the converted project WAV reaches the board: {text}");
        std::fs::remove_file(&absolute).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    /// The Firmware pane's typing through the host (E3b), with the pane's own model. The host
    /// answers every edit: one made for a load an outside change replaced is refused, explicitly
    /// and under its tag, and the person's typing goes again on the new load. Edits go one at a
    /// time; changes close together are one undo step; the plate gets the typed text. Another
    /// writer's change elsewhere and the person's typing both end up in the document; where they
    /// meet, the typing is kept in the editor until the person keeps it. Typing while the bed is
    /// changing waits in the host and applies once edits are taken again.
    #[test]
    fn typed_firmware_reaches_its_document_through_the_host() {
        use fab_elec_view::firmware::FirmwareShown;
        const FW: &str = "let led = output(PD6)\nfn setup() { write(led, high) }\n";
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let bed: Arc<fab_model::Docs> = Arc::new([
            ("ledctl.ckt.splash", include_str!("../../elec/fixtures/ledctl.ckt.splash")),
            ("ledctl.pcb.splash", include_str!("../../elec/fixtures/ledctl.pcb.splash")),
            ("blink.fw.splash", FW),
        ].into_iter().map(|(file, text)| (file.to_string(), Arc::<str>::from(text))).collect());
        let mut host = ElecHost::start(&mut cx, 1, None, bed.clone(), Arc::default(), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let idle = |host: &mut ElecHost, cx: &mut Cx, what: &str| {
            let deadline = Instant::now() + HANG_GUARD;
            loop {
                host.event(cx, &Event::Signal);
                if !host.busy() && host.driver.snap().is_some() {
                    return;
                }
                assert!(Instant::now() < deadline, "{what}: the engine reaches idle with a snapshot");
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        // The pane as it follows the host: its update with the shown snapshot's documents.
        let benches = Arc::new(vec![]);
        let fw = Arc::new(fab_elec_view::live::sim::FwLive { doc: "blink.fw.splash".into(), ..Default::default() });
        let look = |shown: &mut FirmwareShown, host: &ElecHost| {
            let snap = host.driver.snap().expect("the host shows a snapshot");
            shown.update(Some(&fw), &benches, snap.docs.as_ref(), snap.last_refusal.as_ref()).expect("the documents or the refusal changed")
        };
        let doc_text = |host: &ElecHost| {
            let snap = host.driver.snap().expect("the host shows a snapshot");
            let text = snap.docs.as_ref().and_then(|docs| docs.texts.iter().find(|text| text.name == "blink.fw.splash"))
                .expect("the firmware document is in the engine's documents");
            (text.doc, text.rev, text.text.to_string())
        };
        let tag_of = |edit: &ViewAction| match edit {
            ViewAction::Text { tag, .. } => *tag,
            other => panic!("typing is a text action: {other:?}"),
        };
        let base_of = |edit: &ViewAction| match edit {
            ViewAction::Text { base_rev, .. } => *base_rev,
            other => panic!("typing is a text action: {other:?}"),
        };
        // Another writer's edit of the firmware, addressed to what it read.
        let writer = |doc, base_rev, from: &str, to: &str| ViewAction::Text {
            doc, base_rev, edits: vec![fab_elec_design::TextEdit::trimmed(doc, from, to)], group: None,
            label: "another writer".into(), tag: fab_elec_view::snapshot::next_tag(),
        };
        idle(&mut host, &mut cx, "loaded");
        let mut shown = FirmwareShown::default();
        let (_, first_rev, _) = doc_text(&host);
        assert_eq!(look(&mut shown, &host).load.map(|(_, text)| text.to_string()), Some(FW.to_string()), "the pane loads the firmware");

        // An outside change restarts the engine while an edit made for its first load is sent.
        let early = FW.replace("high", "low");
        let sent = shown.typed(0.0, || early.clone()).expect("a change goes to the engine");
        let mut outside = (*bed).clone();
        outside.insert("ledctl.ckt.splash".into(), Arc::from(format!("{}\n// outside\n", bed["ledctl.ckt.splash"])));
        host.reload(&mut cx, 1, None, Arc::new(outside), Arc::default());
        let tag = tag_of(&sent);
        host.act(&mut cx, sent);
        idle(&mut host, &mut cx, "outside change");
        let (doc, loaded_rev, text) = doc_text(&host);
        assert_eq!(text, FW, "the edit made for the first load does not apply to the new one");
        assert!(loaded_rev > first_rev, "the new load's revisions start above the first load's");
        let refusal = host.driver.snap().and_then(|snap| snap.last_refusal.clone()).expect("the host answers the edit");
        assert!(refusal.op == "text" && refusal.sample == Some(tag) && refusal.text.contains("earlier load"),
            "the answer is an explicit refusal of that edit, by kind and tag: {refusal:?}");
        let up = look(&mut shown, &host);
        assert_eq!((up.load.map(|(_, text)| text.to_string()), up.kept), (Some(early.clone()), false),
            "the pane reads the new load with the person's typing moved onto it");
        assert!(shown.sends(), "the typing goes again");
        let again = shown.typed(1.0, || early.clone()).expect("the moved typing goes as the next edit");
        assert_eq!(base_of(&again), loaded_rev, "on the new load's revision");
        host.act(&mut cx, again);
        idle(&mut host, &mut cx, "typing on the new load");
        assert_eq!(doc_text(&host).2, early, "typed input is not lost across the restart");
        assert!(look(&mut shown, &host).load.is_none(), "the pane holds the engine's text");

        // One edit on the way at a time; changes close together are one undo step.
        let (_, rev, _) = doc_text(&host);
        let one = early.replace("setup", "start");
        let first = shown.typed(10.0, || one.clone()).expect("a change goes to the engine");
        assert_eq!(base_of(&first), rev, "it addresses the revision the pane holds");
        host.act(&mut cx, first);
        let two = one.replace("low", "high");
        assert!(shown.typed(10.2, || two.clone()).is_none(), "a change while the first is on the way waits");
        idle(&mut host, &mut cx, "first change");
        assert_eq!(doc_text(&host), (doc, rev + 1, one.clone()), "the typed text reaches the document, one revision on");
        let step = host.undo_step();
        assert!(look(&mut shown, &host).load.is_none() && shown.sends(), "the first applied: the waiting change goes");
        let next = shown.typed(10.4, || two.clone()).expect("the waiting change goes");
        assert_eq!(base_of(&next), rev + 1, "it addresses the first change's revision");
        host.act(&mut cx, next);
        idle(&mut host, &mut cx, "second change");
        assert_eq!(doc_text(&host).2, two, "both changes are in the document");
        assert!(step.is_some() && host.undo_step() == step, "changes close together merge into one undo step");
        assert!(look(&mut shown, &host).load.is_none(), "the pane holds the engine's text");
        let write = host.take_docs().expect("typed firmware hands the plate its documents");
        assert_eq!(write.docs.get("blink.fw.splash").map(|text| text.as_ref()), Some(two.as_str()), "the plate gets the typed firmware");

        // Another writer changes another part first: the pane's edit is refused, its typing
        // moves onto the new text and goes again; both changes end up in the document.
        let (_, rev, text) = doc_text(&host);
        let mine = text.replace("start", "begin");
        let theirs = text.replace("PD6", "PD7");
        let pane = shown.typed(20.0, || mine.clone()).expect("a change goes to the engine");
        host.act(&mut cx, writer(doc, rev, &text, &theirs));
        host.act(&mut cx, pane);
        idle(&mut host, &mut cx, "another writer elsewhere");
        assert_eq!(doc_text(&host).2, theirs, "the other writer's change came first; the pane's was refused");
        let both = theirs.replace("start", "begin");
        let up = look(&mut shown, &host);
        assert_eq!((up.load.map(|(_, text)| text.to_string()), up.kept), (Some(both.clone()), false),
            "the pane shows the other writer's change with the person's typing in it");
        let moved = shown.typed(20.1, || both.clone()).expect("the moved typing goes");
        host.act(&mut cx, moved);
        idle(&mut host, &mut cx, "the moved typing");
        assert_eq!(doc_text(&host).2, both, "both changes are in the document");
        assert!(look(&mut shown, &host).load.is_none(), "the pane holds the engine's text");

        // Where the two changes meet, the typing is kept in the editor and the pane says so;
        // it goes over the document's text once the person keeps it.
        let (_, rev, text) = doc_text(&host);
        let mine = text.replace("high", "low");
        let theirs = text.replace("high", "off");
        let pane = shown.typed(30.0, || mine.clone()).expect("a change goes to the engine");
        host.act(&mut cx, writer(doc, rev, &text, &theirs));
        host.act(&mut cx, pane);
        idle(&mut host, &mut cx, "another writer at the same place");
        assert_eq!(doc_text(&host).2, theirs, "the other writer's change is in the document");
        let up = look(&mut shown, &host);
        assert!(up.load.is_none() && up.kept && up.status.contains("Not in the document") && up.status.contains("blink.fw.splash"),
            "the editor keeps the typing and the pane says it is not in the document: {up:?}");
        assert!(!shown.sends() && shown.typed(30.5, || mine.clone()).is_none(), "kept typing waits for the person");
        let kept = shown.keep_mine(31.0).expect("keeping it sends it");
        assert_eq!(base_of(&kept), rev + 1, "over the document's text as it is");
        host.act(&mut cx, kept);
        idle(&mut host, &mut cx, "the kept typing");
        assert_eq!(doc_text(&host).2, mine, "the person's typing is in the document");
        let up = look(&mut shown, &host);
        assert!(up.load.is_none() && !up.kept, "the pane holds the engine's text and keeps nothing aside");

        // While the bed is changing, typing waits in the host; it applies once edits are taken.
        host.set_settled(false);
        let late = mine.replace("PD7", "PD5");
        let pane = shown.typed(40.0, || late.clone()).expect("a change goes to the engine");
        host.act(&mut cx, pane);
        idle(&mut host, &mut cx, "the bed changing");
        assert_eq!(doc_text(&host).2, mine, "no edit is taken while the bed changes");
        host.set_settled(true);
        idle(&mut host, &mut cx, "edits taken again");
        assert_eq!(doc_text(&host).2, late, "the waiting typing applies once edits are taken");

        // The AI reads the documents and changes the firmware's text as the pane's typing does
        // (`fab.elec_text`). No bench runs this firmware, so nothing here builds it: the change
        // goes in unchecked and the answer says so, never as a failed build.
        use fab_elec_host::doc_actions::TextChange;
        crate::chat::register_elec_actions();
        let read = |host: &mut ElecHost, cx: &mut Cx, name: &str, args: &[(&str, &str)], id: u64| {
            host.run_action(cx, name, args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), By::Ai { turn: 0, call: id }, Some(id));
            let deadline = Instant::now() + HANG_GUARD;
            loop {
                if let Some((_, ok, text)) = host.take_answers().into_iter().find(|answer| answer.0 == id) {
                    return (ok, text);
                }
                assert!(Instant::now() < deadline, "{name} is answered");
                std::thread::sleep(Duration::from_millis(1));
                host.event(cx, &Event::Signal);
            }
        };
        let (_, rev, text) = doc_text(&host);
        let (ok, list) = read(&mut host, &mut cx, "elec.doc.list", &[], 50);
        assert!(ok && list.contains(&format!("{{name: \"blink.fw.splash\" kind: firmware rev: {rev} lines: 2}}")), "the list names the firmware with its rev: {list}");
        let (ok, page) = read(&mut host, &mut cx, "elec.doc.read", &[("doc", "blink.fw.splash")], 51);
        assert!(ok && page.contains(&format!("rev: {rev}")) && page.contains("text: \"1\\tlet led = output(PD5)\\n2\\tfn begin()"), "the read numbers the lines: {page}");
        let replace = |pairs: &[(&str, &str)]| TextChange::Replace(pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect());
        let step = host.undo_step();
        let (ok, answer, cues) = ai_text(&mut host, &mut cx, "blink.fw.splash", rev - 1, replace(&[("PD5", "PD4")]), 52);
        assert!(!ok && answer.contains(&format!("it is at rev {rev} now")) && cues.is_empty(), "a stale rev is refused with the new one: {answer}");
        let (ok, answer, _) = ai_text(&mut host, &mut cx, "blink.fw.splash", rev, replace(&[("PD9", "PD4")]), 53);
        assert!(!ok && answer.contains("edit 1") && doc_text(&host).2 == text, "an old not in the text commits nothing: {answer}");
        let (ok, answer, cues) = ai_text(&mut host, &mut cx, "blink.fw.splash", rev, replace(&[("low", "high"), ("PD5", "PD4"), ("begin", "setup")]), 54);
        let three = "let led = output(PD4)\nfn setup() { write(led, high) }\n";
        assert!(ok && answer.contains(&format!("rev: {}", rev + 1)) && answer.contains("lines: [[1, 1], [2, 2], [2, 2]]"), "three replacements commit once: {answer}");
        assert!(answer.contains("compiled: false") && answer.contains("not compiled") && !answer.contains("error"), "an unchecked change says so: {answer}");
        assert_eq!(doc_text(&host), (doc, rev + 1, three.to_string()), "the three replacements are in the document, one revision on");
        assert!(matches!(cues.as_slice(), [cue] if cue.origin == EditOrigin::Ai && cue.editors == vec![ElecEditor::Firmware]
            && cue.lines == vec![("blink.fw.splash".to_string(), 0..2)] && cue.label == "Edit blink.fw.splash"),
            "one AI cue on the Firmware pane over the edited lines: {cues:?}");
        assert!(host.undo_step() != step, "the call is a step of its own");
        host.undo(&mut cx);
        idle(&mut host, &mut cx, "undo of the AI's change");
        assert_eq!(doc_text(&host).2, text, "one undo takes back all three replacements");
        assert_eq!(host.undo_step(), step, "the step before it is the newest again");
        // The circuit's text is the AI's too, evaluated before it is committed: a change that adds
        // a mistake commits nothing and says where it is; a board's text stays the actions'.
        let circuit_rev = |host: &ElecHost| host.driver.snap().and_then(|snap| snap.docs.as_ref()?.texts.iter()
            .find(|text| text.name == "ledctl.ckt.splash").map(|text| text.rev)).expect("the circuit document is in the engine's documents");
        let rev = circuit_rev(&host);
        let (ok, answer, cues) = ai_text(&mut host, &mut cx, "ledctl.ckt.splash", rev, replace(&[("let boot_r = e.r(10kohm", "let boot_r = e.r(ten_kohm")]), 55);
        assert!(!ok && answer.contains("does not evaluate") && answer.contains("doc: \"ledctl.ckt.splash\", line: 92, col: 34") && cues.is_empty(),
            "circuit text with a mistake commits nothing and says where it is: {answer}");
        assert_eq!(circuit_rev(&host), rev, "the refused change leaves the circuit's revision");
        let (ok, answer, cues) = ai_text(&mut host, &mut cx, "ledctl.ckt.splash", rev, replace(&[("let boot_r = e.r(10kohm", "let boot_r = e.r(22kohm")]), 56);
        assert!(ok && answer.contains("evaluated: true") && answer.contains(&format!("rev: {}", rev + 1)) && !answer.contains("compiled"),
            "circuit text that evaluates is committed: {answer}");
        assert!(matches!(cues.as_slice(), [cue] if cue.origin == EditOrigin::Ai && cue.lines == vec![("ledctl.ckt.splash".to_string(), 91..92)]),
            "one AI cue over the edited line: {cues:?}");
        let (ok, answer, _) = ai_text(&mut host, &mut cx, "ledctl.pcb.splash", 0, replace(&[("a", "b")]), 57);
        assert!(!ok && answer.contains("board document"), "a board's text stays the actions': {answer}");
    }

    /// A committed host removal reaches the saved plate, so reloading cannot resurrect the part.
    #[test]
    fn a_removed_part_document_stays_removed_after_save_and_reload() {
        let dir = std::env::temp_dir().join(format!("fab-elec-host-remove-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("plate.fab.splash");
        let part = "parts/local/C1.part.splash";
        let docs: Arc<fab_model::Docs> = Arc::new([
            ("ledctl.ckt.splash", include_str!("../../elec/fixtures/ledctl.ckt.splash")),
            (part, include_str!("../../elec/fixtures/parts/avr/ATMEGA328P_TQFP32.part.splash")),
        ].into_iter().map(|(name, text)| (name.to_string(), Arc::<str>::from(text))).collect());
        let mut model = fab_model::FabModel::new("plate");
        model.set_docs(docs.clone());
        fab_model::project::save(&model, &path).expect("save the plate with its part document");
        let part_path = fab_model::project::elec_dir(&path).join(part);
        assert!(part_path.is_file(), "the part document starts on disk");

        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut host = ElecHost::start(&mut cx, 1, None, docs, Arc::default(), model.steps().clone(), model.logged().clone());
        let idle = |host: &mut ElecHost, cx: &mut Cx| {
            // The wait is for the engine's state; the deadline only stops a hung engine, as long
            // as the host's other engine waits allow.
            let deadline = Instant::now() + HANG_GUARD;
            loop {
                host.event(cx, &Event::Signal);
                if !host.busy() && host.driver.snap().is_some() {
                    return;
                }
                assert!(Instant::now() < deadline, "hang guard, not a timing check: the engine never reached idle with a snapshot (busy {}, snapshot {})",
                    host.busy(), host.driver.snap().is_some());
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        idle(&mut host, &mut cx);
        let snap = host.driver.snap().expect("the loaded engine has a snapshot");
        assert!(snap.design.unit_by_name("ledctl").is_some(), "the fixture documents evaluate");
        let base_rev = snap.docs.as_ref().expect("the engine retains the plate's documents").texts.iter()
            .find(|doc| doc.name == part).expect("the part is present before removal").rev;
        host.driver.push(Cmd::Docs {
            set: vec![DocSet::Remove { name: part.into(), base_rev }],
            label: "remove part document".into(), by: By::default(), ticket: None,
        });
        idle(&mut host, &mut cx);
        assert!(host.undo_step().is_some(), "the host commits the removal as an undoable step");
        let write = host.take_docs().expect("a committed removal hands out documents");
        assert!(!write.docs.contains_key(part), "the host's write omits the removed document");
        let (merged, assets) = merge_files(model.docs(), model.elec_assets(), &write.bases, &write.docs, &write.assets)
            .expect("the removal lands over the loaded plate");
        assert!(assets.is_none(), "removing a document leaves the plate's assets");
        model.set_docs(Arc::new(merged));
        fab_model::project::save(&model, &path).expect("save the host's removal");
        assert!(!part_path.exists(), "saving removes the part document from disk");
        let loaded = fab_model::project::load(&path, model.env()).expect("reload the saved plate");
        assert!(!loaded.docs().contains_key(part), "the host's removed document stays absent after reload");
        assert!(loaded.docs().contains_key("ledctl.ckt.splash"), "the circuit document still loads");
        std::fs::remove_dir_all(&dir).expect("remove the test project");
    }

    /// An import is one undo step on the plate too (4b3b): its write brings the part document and
    /// the importer's mesh bytes (the same allocation), undo takes both off the plate, so the
    /// saved plate keeps no orphan glb, and redo puts the same bytes back from the history. The
    /// engine's own asset writes never restart its history.
    #[test]
    fn an_undone_import_leaves_the_saved_plate_and_redo_brings_its_mesh_back() {
        use fab_elec_circuit::CircuitOp;
        use fab_elec_design::{AssetBytes, FileOp};
        let dir = std::env::temp_dir().join(format!("fab-elec-host-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("plate.fab.splash");
        let (part, mesh) = ("parts/test/T1.part.splash", "parts/test/T1.glb");
        let part_text = r#"let __test_t1_e = mod.elec
let __test_t1_f = mod.elec.f
let test_t1 = fn() {
    let e = __test_t1_e
    let f = __test_t1_f
    let existing = e.lookup("test:T1")
    if existing != nil { return existing }
    return e.define("test:T1", {
        kind: @resistor
        symbol: f.two_terminal([["1", @passive], ["2", @passive]])
        footprint: f.footprint({
            pads: [
                f.smd("1", [-0.5mm, 0mm], [0.5mm, 0.3mm], {shape: @rect rot: 0deg})
                f.smd("2", [0.5mm, 0mm], [0.5mm, 0.3mm], {shape: @rect rot: 0deg})
            ]
            body: [[-1mm, -0.5mm], [1mm, 0.5mm]]
            mount: @smd
        })
        body: {mesh: "T1.glb" offset: [0mm, 0mm, 0mm] rotation: [0deg, 0deg, 0deg] height: 0.5mm}
        pads: [["1", "1"], ["2", "2"]]
        prefix: "R"
        fields: {}
    })
}
test_t1()
"#;
        let docs: Arc<fab_model::Docs> = Arc::new([
            ("ledctl.ckt.splash", include_str!("../../elec/fixtures/ledctl.ckt.splash")),
        ].into_iter().map(|(name, text)| (name.to_string(), Arc::<str>::from(text))).collect());
        let mut model = fab_model::FabModel::new("plate");
        model.set_docs(docs.clone());
        fab_model::project::save(&model, &path).expect("save the plate before the import");
        let elec = fab_model::project::elec_dir(&path);

        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut host = ElecHost::start(&mut cx, 1, None, docs, Arc::default(), model.steps().clone(), model.logged().clone());
        let idle = |host: &mut ElecHost, cx: &mut Cx| {
            // The wait is for the engine's state; the deadline only stops a hung engine, as long
            // as the host's other engine waits allow.
            let deadline = Instant::now() + HANG_GUARD;
            loop {
                host.event(cx, &Event::Signal);
                if !host.busy() && host.driver.snap().is_some() {
                    return;
                }
                assert!(Instant::now() < deadline, "hang guard, not a timing check: the engine never reached idle with a snapshot (busy {}, snapshot {})",
                    host.busy(), host.driver.snap().is_some());
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        // What the model worker does with a write, then the status that follows it, then a save.
        let land = |host: &mut ElecHost, model: &mut fab_model::FabModel, cx: &mut Cx, what: &str| {
            let write = host.take_docs().unwrap_or_else(|| panic!("{what} hands the plate its files"));
            let (merged, assets) = merge_files(model.docs(), model.elec_assets(), &write.bases, &write.docs, &write.assets)
                .unwrap_or_else(|| panic!("{what} lands over the plate"));
            model.set_docs(Arc::new(merged));
            if let Some(assets) = assets {
                model.set_elec_assets(Arc::new(assets));
            }
            host.docs_landed(1, &write.docs);
            host.reload(cx, 1, None, model.docs().clone(), model.elec_assets().clone());
            fab_model::project::save(model, &path).unwrap_or_else(|e| panic!("save after {what}: {e}"));
        };
        idle(&mut host, &mut cx);
        let unit = host.driver.snap().expect("the loaded engine has a snapshot").design.unit_by_name("ledctl")
            .expect("the fixture evaluates").id;
        let bytes: Arc<[u8]> = Arc::from(&b"glTF test mesh bytes"[..]);
        host.driver.push(Cmd::Commit {
            ops: vec![
                DesignOp::File(FileOp::AddDoc { name: part.into(), text: part_text.into() }),
                DesignOp::File(FileOp::AddAsset { name: mesh.into(), key: "asset:00/T1.glb".into(), bytes: AssetBytes(bytes.clone()) }),
                DesignOp::Circuit { unit, op: CircuitOp::AddPart {
                    stmt: "let imp = e.part(\"test:T1\", {})".into(), after: None, ends: vec![], hint: None,
                } },
            ],
            place: vec![], label: "import T1".into(), circuits: vec![], expect: None, by: By::default(), ticket: None,
        });
        idle(&mut host, &mut cx);
        assert!(host.undo_step().is_some(), "the import is one undoable step");
        land(&mut host, &mut model, &mut cx, "the import");
        assert!(Arc::ptr_eq(&model.elec_assets()[mesh], &bytes), "the plate holds the importer's allocation, not a copy");
        assert!(elec.join(part).is_file(), "the saved plate has the part document");
        assert_eq!(std::fs::read(elec.join(mesh)).expect("the saved plate has the mesh"), &bytes[..], "the saved mesh is the importer's bytes");

        host.driver.push(Cmd::Undo);
        idle(&mut host, &mut cx);
        land(&mut host, &mut model, &mut cx, "the undo");
        assert!(!model.elec_assets().contains_key(mesh) && !model.docs().contains_key(part), "undo takes the copy off the plate");
        assert!(!elec.join(mesh).exists() && !elec.join(part).exists(), "the saved plate keeps no orphan mesh or part document");

        host.driver.push(Cmd::Redo);
        idle(&mut host, &mut cx);
        land(&mut host, &mut model, &mut cx, "the redo");
        assert!(Arc::ptr_eq(&model.elec_assets()[mesh], &bytes), "redo puts the history's allocation back, not a copy");
        assert_eq!(std::fs::read(elec.join(mesh)).expect("the redone mesh is saved again"), &bytes[..], "the saved mesh is the same bytes");
        assert!(elec.join(part).is_file(), "the redone part document is saved again");
        std::fs::remove_dir_all(&dir).expect("remove the test project");
    }

    /// Why: the boards a case script reads are its plate's own electronics documents' (11-15):
    /// a restart or a switch with Electronics closed builds the case against the plate's board,
    /// not the stock one, and a plate switched back to shows its build again (no second build).
    #[test]
    fn a_recovered_plate_builds_its_case_against_its_own_board() {
        let root = std::env::temp_dir().join(format!("fab-elec-doc-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let session = || {
            let mut s = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), root.join("Documents"));
            s.store = Some(fab_ai::plates::PlateStore::under(&root.join("fab"), &root.join("Documents")));
            s.model.set_env(shared_env());
            s.env_for = Some(Arc::new(doc_env));
            s
        };
        // the base's long side, mm
        let long_side = |ev: &Evaluated| {
            let base = case_solids(ev, "ledctl").into_iter().find(|c| c.name == "Base").expect("the case has its base");
            let extent = |axis: usize| {
                let (lo, hi) = base.positions.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p[axis]), hi.max(p[axis])));
                hi - lo
            };
            extent(0).max(extent(1))
        };
        let case = "use mod.model.*\nuse mod.fab.*\nlet pcb = board(\"ledctl\")\nlet case = enclosure(pcb, {})\nobject(case.base, name: \"Base\")\n";
        let pcb = include_str!("../../elec/fixtures/ledctl.pcb.splash");
        let longer = pcb.replace("[50mm, 0mm], [50mm, 30mm]", "[60mm, 0mm], [60mm, 30mm]");
        assert_ne!(longer, pcb, "the board's outline is 10 mm longer");
        let docs: fab_model::Docs = [
            ("ledctl.ckt.splash", include_str!("../../elec/fixtures/ledctl.ckt.splash")),
            ("ledctl.pcb.splash", longer.as_str()),
            ("ledctl.bench.splash", include_str!("../../elec/fixtures/ledctl.bench.splash")),
        ].into_iter().map(|(file, text)| (file.to_string(), Arc::<str>::from(text))).collect();

        let mut s = session();
        let stock = s.replace_model(fab_model::FabModel::with_script("cased", "case.cad.splash", case), None).expect("the case builds on the stock board");
        let a = s.plate.clone().expect("a plate");
        s.model.set_docs(Arc::new(docs));
        s.autosave().expect("the plate and its documents are saved");
        drop(s);

        let mut t = session();
        let recovered = t.recover().expect("the bed comes back").expect("the case builds on the plate's board");
        assert_eq!(t.plate.as_deref(), Some(a.as_str()));
        let grown = long_side(&recovered) - long_side(&stock);
        assert!((grown - 10.0).abs() < 0.5, "the case is built on the plate's own board, 10 mm longer: it grew {grown:.2} mm");
        let built = t.compiled.clone().expect("the plate's build");
        t.replace_model(fab_model::FabModel::with_script("b", "b.cad.splash", &format!("{}let block = m.box({{size: [10mm 10mm 10mm]}})\nobject(block)\n", fab_model::program::EMPTY)), None).expect("plate B builds");
        assert!(t.model.env().same(&shared_env()), "a plate without documents reads the shared boards");
        t.switch(&a).expect("back to the cased plate");
        assert!(t.compiled.as_ref().is_some_and(|c| Arc::ptr_eq(c, &built)), "switched back to, its build shows again: no second build");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Why: a plate's board document that does not evaluate is the person's text (11-15): its
    /// case says which document fails and why, never builds on the stock board of that name, and
    /// the engine holds that text as it reads, so a first edit cannot write stock text over it.
    #[test]
    fn a_board_document_that_does_not_evaluate_is_reported_and_kept_as_it_reads() {
        let root = std::env::temp_dir().join(format!("fab-elec-broken-doc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let session = || {
            let mut s = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), root.join("Documents"));
            s.store = Some(fab_ai::plates::PlateStore::under(&root.join("fab"), &root.join("Documents")));
            s.model.set_env(shared_env());
            s.env_for = Some(Arc::new(doc_env));
            s
        };
        let case = "use mod.model.*\nuse mod.fab.*\nlet pcb = board(\"ledctl\")\nlet case = enclosure(pcb, {})\nobject(case.base, name: \"Base\")\n";
        let broken = format!("{}\nplace(\"nothing\", \n", include_str!("../../elec/fixtures/ledctl.pcb.splash"));
        let docs: Arc<fab_model::Docs> = Arc::new([
            ("ledctl.ckt.splash", include_str!("../../elec/fixtures/ledctl.ckt.splash")),
            ("ledctl.pcb.splash", broken.as_str()),
            ("ledctl.bench.splash", include_str!("../../elec/fixtures/ledctl.bench.splash")),
        ].into_iter().map(|(file, text)| (file.to_string(), Arc::<str>::from(text))).collect());

        let mut s = session();
        s.replace_model(fab_model::FabModel::with_script("cased", "case.cad.splash", case), None).expect("the case builds on the stock board");
        let a = s.plate.clone().expect("a plate");
        s.model.set_docs(docs.clone());
        s.autosave().expect("the plate and its documents are saved");
        drop(s);

        let mut t = session();
        let error = t.recover().expect("the bed comes back").expect_err("a case on a board that does not evaluate does not build");
        assert!(error.contains("ledctl.pcb.splash:") && error.contains("does not evaluate"), "the case names the board document, where in it, and that it does not evaluate: {error}");
        let scripts = t.store.as_ref().expect("a store").dir(&a).join("scripts");
        let texts: Vec<String> = std::fs::read_dir(&scripts).expect("the plate's working copy").flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".elec.splash"))
            .map(|e| std::fs::read_to_string(e.path()).expect("a document reads")).collect();
        assert!(texts.iter().any(|text| *text == broken), "the plate keeps its board document as it was written");

        let (design, engine_docs, _, _) = plate_project(&docs, &fab_model::ElecAssets::new(), None);
        assert!(design.units.is_empty(), "documents that do not evaluate load as an empty design, not the stock one");
        assert_eq!(*engine_docs.expect("the engine holds the documents").texts(), *docs, "the engine holds the plate's text as it reads");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn apply_case_keepouts(publications: Vec<CaseKeepouts>, design: &Design) -> Design {
        let mut held = design.clone();
        for CaseKeepouts { unit, sets, .. } in publications {
            for (name, items) in sets {
                let key = name.strip_prefix("@elec:port:").and_then(|s| s.split_once('/'));
                assert!(items.iter().all(|i| matches!(i, Item::Keepout(k) if key.map_or_else(
                    || case_set(&k.reason, None) == name, |(_, reason)| k.reason == reason))),
                    "each keepout keeps its record name as its reason");
                let from = FeatureRef::Panel(format!("{}{name}", fab_elec_host::CASE_KEEPOUT));
                held.set_derived(unit, &from, items).expect("the keepouts apply").expect("the board takes the set");
            }
        }
        held
    }

    fn check_plan_records(ev: &Evaluated, name: &str, mut from_plan: Vec<fab_plate::Keepout>) {
        let mut case: Vec<fab_plate::Keepout> = Vec::new();
        for record in ev.plate.parts.iter().chain(ev.plate.excluded.iter().map(|x| &x.part)).flat_map(|p| &p.keepouts)
            .filter(|k| k.board == name && k.name.starts_with("Light pipe ")) {
            if !case.contains(record) { case.push(record.clone()); }
        }
        assert!(!case.is_empty(), "{name}'s case holds light records");
        let key = |k: &fab_plate::Keepout| (k.name.clone(), k.room);
        from_plan.sort_by_key(key);
        case.sort_by_key(key);
        assert_eq!(from_plan, case, "{name}: the plan's rooms and blocks are the case's records, bit for bit");
    }

    /// The plan through an isolate, given the board's own ports, says what the full case did: the
    /// case's light records for the board and the groups it refused, and with three LEDs set as
    /// the guard's example, the generator's refusal in its own words.
    fn check_plan_equals_case(ev: &Evaluated, libraries: &Arc<Vec<makepad_model_script::lang::Library>>, board_text: &str, name: &str, pipes: usize) {
        use makepad_model_script::lang::V;
        let call = ev.calls.iter().find(|c| c.board == name).unwrap_or_else(|| panic!("the case leaves a plan call for {name}"));
        assert_eq!(call.function, "light_plan", "the call names the library's export");
        assert!(PublishedPlan::for_board(name, &[], libraries).unwrap().is_none());
        let mut repeated = [call.clone(), call.clone()];
        repeated[1].args.reverse();
        assert!(PublishedPlan::for_board(name, &repeated, libraries).unwrap().is_some());
        repeated[1].args.push(("extra".into(), V::Bool(true)));
        assert!(PublishedPlan::for_board(name, &repeated, libraries).unwrap_err().contains(name));
        let mut plan = CasePlan::open(libraries, call).unwrap_or_else(|e| panic!("the isolate loads: {e:?}"));
        let ports = plan.ports_of(board_text).unwrap_or_else(|e| panic!("the board's ports read: {e:?}"));
        let answer = plan.plan(ports.clone()).unwrap_or_else(|e| panic!("the plan is called: {e:?}"));
        check_plan_records(ev, name, answer.keepouts);
        assert_eq!(answer.refused.len(), 0, "{name}: nothing is refused");
        assert_eq!(case_pipes(ev, name).len(), pipes, "{name}: the case made a pipe for every LED the plan did not refuse");
        // Fable's example: three LEDs at (0, 0), (2.2, 0) and (0.3, 4.0) mm turn out one board-Y group the
        // generator refuses, though the X rooms allow them.
        let V::Arr(all) = &ports else { panic!("ports are a list") };
        let lights: Vec<&V> = all.iter().filter(|p| matches!(p, V::Obj(o) if matches!(o.get("kind"), Some(V::Id(k)) if k == "light"))).collect();
        assert!(lights.len() >= 1, "{name} has an LED");
        let moved = |p: &V, label: &str, x: f64, y: f64| {
            let V::Obj(o) = p else { unreachable!() };
            let mut fields = o.fields.clone();
            for (k, v) in &mut fields {
                match k.as_str() {
                    "ref" | "refdes" => *v = V::Str(label.into()),
                    "at" => if let V::Arr(at) = v { at[0] = V::Num(x * 0.001); at[1] = V::Num(y * 0.001); },
                    _ => {}
                }
            }
            V::Obj(Arc::new(makepad_model_script::lang::Obj { ident: 0, part: None, proto: None, fields, items: Vec::new(), children: Vec::new(), loc: None }))
        };
        let ports_at = |sites: &[(&str, f64, f64)]| V::Arr(sites.iter().map(|&(label, x, y)| moved(lights[0], label, x, y)).collect());
        let three = [("A", 0.0, 0.0), ("B", 2.2, 0.0), ("C", 0.3, 4.0)];
        let flip = plan.plan(ports_at(&three)).unwrap_or_else(|e| panic!("the plan is called on the three LEDs: {e:?}"));
        assert_eq!((flip.refused_pipes, flip.made_pipes), (3, 0), "the entire group loses its pipes");
        assert_eq!(flip.refused.len(), 1, "{name}: the three LEDs form one group that is refused: {:?}", flip.refused);
        assert!(["A and B", "B and A"].iter().any(|pair| flip.refused[0].contains(
            &format!("The light pipes of {pair} would leave less than two 0.4mm lines of lid between them"))),
            "{name}: the refusal is the generator's own: {:?}", flip.refused);

        // Compare each named polygon relative to its LED in the board's own units.
        let check_rooms = |answer: &PlanAnswer, sites: &[(&str, f64, f64)], hx: f64, hy: f64| {
            let rooms: Vec<_> = answer.keepouts.iter().filter(|k| k.room).collect();
            assert_eq!(rooms.len(), sites.len());
            for &(led, x, y) in sites {
                let room = rooms.iter().find(|k| k.name == format!("Light pipe room {led}")).expect("each LED has its room");
                let centre = P::mm(x, y);
                let offsets: Vec<_> = room.area.iter().map(|&[x, y]| P::mm(x, y).sub(centre)).collect();
                assert_eq!(offsets, [P::mm(-hx, -hy), P::mm(hx, -hy), P::mm(hx, hy), P::mm(-hx, hy)], "{name}: room of {led}");
            }
        };
        let horizontal = [("A", 10.0, 2.5), ("B", 12.2, 2.5), ("C", 14.4, 2.5)];
        check_rooms(&flip, &three, 1.4, 1.0);
        check_rooms(&plan.plan(ports_at(&horizontal)).unwrap(), &horizontal, 1.0, 1.4);

        for (row, hx, hy) in [("x", 1.0, 1.4), ("y", 1.4, 1.0)] {
            plan.call.args.iter_mut().find(|(key, _)| key == "spec").unwrap().1 = V::record(vec![("row", V::Id(row.into()))]);
            for sites in [&three[..], &horizontal, &three[..1]] {
                let answer = plan.plan(ports_at(sites)).unwrap();
                check_rooms(&answer, sites, hx, hy);
                if row == "x" {
                    assert_eq!(answer.made_pipes as usize, sites.len());
                    assert!(answer.refused.is_empty());
                }
            }
        }
    }

    fn check_led_port_rooms(ev: &Evaluated, design: &Design, publication: &CaseKeepouts) {
        use fab_elec_core::{mm, Angle, PolySet, Side};

        let unit = design.unit(publication.unit).unwrap();
        let board = unit.board.as_ref().unwrap();
        assert_eq!(publication.generation, board.generation);
        assert_eq!(publication.rooms.len(), 8, "one port room per LED on {}", unit.name);
        assert!(publication.rooms.iter().all(|r| r.room.is_some() && r.keepout.is_some()), "each LED carries both records");
        let mut owners = HashSet::new();
        // Republish the same board-axis records with each LED turned a quarter turn.
        let mut turned = design.clone();
        let turned_unit = turned.units.iter_mut().find(|u| u.id == unit.id).unwrap();
        for p in &mut Arc::make_mut(turned_unit.board.as_mut().unwrap()).placed {
            if publication.rooms.iter().any(|port| port.comp == p.comp) { p.rot = p.rot.add(Angle::QUARTER); }
        }
        let turned = case_keepouts(ev, &turned).into_iter().find(|p| p.unit == unit.id).unwrap();
        assert_eq!(turned.rooms, publication.rooms, "a quarter turn preserves each port's board orientation");
        let by = P::mm(3.0, -2.0);
        for record in ev.plate.parts.iter().chain(ev.plate.excluded.iter().map(|x| &x.part)).flat_map(|p| &p.keepouts)
            .filter(|k| k.board == unit.name) {
            let Some(key) = &record.feature else { continue };
            let Some(FeatureRef::Port(c, _)) = fab_elec_mech::keys::feature_of_key(key) else { panic!("{} has invalid port {key}", record.name) };
            assert_eq!(c.board, unit.id);
            let part = board.placed_of(c.comp).unwrap_or_else(|| panic!("{} has no placed part for {key}", record.name));
            owners.insert(part.comp);
            let port = publication.rooms.iter().find(|r| r.comp == part.comp).expect(key);
            let area = PolySet::from_ring_nonzero(record.area.iter().map(|p| P::mm(p[0], p[1])).collect());
            let moved = area.translated(by);
            for (placed, expected) in [
                (port.placed(part.at), area),
                (port.placed(part.at.add(by)), moved),
            ] {
                if record.room {
                    assert_eq!(placed.room, Some(expected), "{} follows its LED", record.name);
                } else {
                    let side = match record.side { fab_plate::KeepoutSide::Top => Side::Top, fab_plate::KeepoutSide::Bottom => Side::Bottom };
                    assert_eq!(placed.keepout, Some(CarriedKeepout {
                        from: FeatureRef::Panel(format!("{}{}", fab_elec_host::CASE_KEEPOUT, case_set(&record.name, Some(key)))),
                        area: expected, band: (mm(record.z[0]), mm(record.z[1])), side,
                    }), "{} follows its LED and identifies its derived set", record.name);
                }
            }
        }
        assert_eq!(owners.len(), 8, "the records resolve to eight distinct placed LEDs");
    }

    /// Run the ordinary worker and its proposal checks, serially for the comparison.
    fn run_place_trial(base: fab_elec_route::Base, job: fab_elec_route::PlaceJob) -> fab_elec_route::RouteProposal {
        use fab_elec_route::{route_channel, RouteEvent, RouteRequest, RouteWorker, Spawn};
        struct Serial;
        impl Spawn for Serial {
            fn spawn(&self, task: Box<dyn FnOnce() + Send>) { task(); }
        }
        let (wake, wakes) = std::sync::mpsc::channel();
        let (handle, inbox) = route_channel(move || { let _ = wake.send(()); });
        let worker = std::thread::spawn(move || RouteWorker::new(inbox, Arc::new(Serial)).run());
        handle.request(RouteRequest::Base(base)).unwrap();
        handle.request(RouteRequest::Place(job)).unwrap();
        let last = loop {
            wakes.recv().unwrap();
            if let Some(RouteEvent::Proposal(p)) = handle.try_recv() {
                if p.last { break p }
            }
        };
        drop(handle);
        worker.join().unwrap();
        last
    }

    /// The checked proposal carries each light port's keepout from the place job, once per source,
    /// with the room's area, height band and side.
    fn check_carried_keepouts(base: &fab_elec_route::Base, job: &fab_elec_route::PlaceJob) {
        let proposal = run_place_trial(base.clone(), job.clone());
        let checked = &proposal.checked.as_ref().expect("the worker checked the placement").board;
        for port in &job.rooms {
            let p = checked.placed_of(port.comp).unwrap();
            let carried = port.placed(p.at).keepout.unwrap();
            let source = fab_elec_board::Origin::Derived { from: carried.from };
            let mut members = checked.keepouts.iter().filter(|k| k.origin == source);
            let member = members.next().expect("the checked board carries the port's keepout");
            assert!(members.next().is_none(), "one checked keepout for this source");
            assert_eq!(member.area, carried.area);
            assert_eq!(member.z, Some(carried.band));
            assert_eq!(member.layers, fab_elec_core::LayerSet::one(fab_elec_core::LayerId::F_CU.on(carried.side)));
        }
    }

    /// Why: a place from empty onto a cased board brings its parts, waits for the case to see
    /// them and places with their rooms, so every LED gets its pipe with no case error and no
    /// keepout conflict. It drives the same frozen inputs, evaluation, publication, worker and
    /// kept proposal as a place from the pane; only delivery between their owners is synchronous.
    fn place_empty_with_case(name: &str, mut model: fab_model::FabModel, evaluator: &mut fab_model::eval::Evaluator) {
        use fab_elec_host::{Mailbox, RouteStart, Step, Ticket};
        use fab_elec_core::Severity;

        fn settle(engine: &mut Engine, mail: &Mailbox) -> Step {
            loop {
                let step = engine.step(mail.take(), &mut |_| {});
                assert!(step.rejected.is_empty(), "{:?}", step.rejected);
                if step.fills.is_empty() && step.checks.is_empty() && step.evals.is_empty() { return step }
                for job in step.fills { mail.result(job.run()); }
                for job in step.checks { mail.result(job.run()); }
                for job in step.evals { mail.result(job.run()); }
                if !step.routes.is_empty() { return Step { fills: vec![], checks: vec![], evals: vec![], ..step } }
            }
        }

        fn publish(bridge: &mut CaseBridge, design: Arc<Design>, model: &mut fab_model::FabModel,
            evaluator: &mut fab_model::eval::Evaluator, mail: &Mailbox) -> Arc<Evaluated> {
            let update = match bridge.cover(design.clone()).unwrap() {
                CaseUpdate::Set { generation, env } => {
                    let inputs = Arc::downgrade(&env.inputs);
                    model.set_env(env);
                    let ev = model.evaluate(evaluator, None).map(|(_, ev)| Arc::new(ev)).map_err(|e| e.to_string());
                    bridge.reply(&inputs, Some(generation), ev).unwrap()
                }
                checked => checked,
            };
            let CaseUpdate::Checked { commands, .. } = update else { panic!("the case replied") };
            for cmd in commands { mail.push(cmd); }
            bridge.read.as_ref().unwrap().reply.as_ref().unwrap().as_ref().expect("the plate builds").clone()
        }

        let mut source = fab_elec_fixtures::docs(name).unwrap();
        let pcb = format!("{name}.pcb.splash");
        if source.find(&pcb).is_none() {
            // The fixture set leaves this board out; the demo carries the same file.
            let &(file, text) = crate::demo_part::demo(name).unwrap().files.iter().find(|(file, _)| *file == pcb).unwrap();
            source.add(file, text).unwrap();
        }
        let doc = source.find(&pcb).unwrap();
        let before = &source.doc(doc).unwrap().text;
        // The plate places some panel parts' own objects ("Pin btn.sw", "Knob pitch") and cannot
        // build without them, so those parts stand; every other part, track and via goes.
        let plate_parts: Vec<String> = crate::demo_part::demo(name).into_iter().flat_map(|demo| demo.files.iter())
            .filter(|(file, _)| file.ends_with(".fab.splash"))
            .flat_map(|(_, text)| text.split("object: \"").skip(1).filter_map(|rest| {
                let object = rest.split('"').next()?;
                object.split_once(' ').map(|(_, part)| format!("place(\"{part}\""))
            })).collect();
        let empty = before.lines().filter(|line| plate_parts.iter().any(|kept| line.starts_with(kept.as_str()))
            || !["place(", "track(", "via("].iter().any(|prefix| line.starts_with(prefix)))
            .collect::<Vec<_>>().join("\n");
        source.apply(&[fab_elec_design::TextEdit::trimmed(doc, before, &empty)]).unwrap();
        let (design, docs) = Docs::load(source, fab_elec_lang::std_libs());
        let unit = design.unit_by_name(name).unwrap().id;
        assert_eq!(design.unit(unit).unwrap().board.as_ref().unwrap().placed.len(), plate_parts.len());
        let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::default(), Arc::default(), "place".into())
            .with_docs(docs).with_app_tools();
        let mail = Mailbox::default();
        let mut bridge = CaseBridge::default();
        publish(&mut bridge, engine.design().clone(), &mut model, evaluator, &mail);
        settle(&mut engine, &mail);
        assert!(engine.case_rooms(unit).unwrap().1.is_empty());
        let invoke = |ticket| Cmd::Invoke { tool: "elec.place".into(),
            args: vec![("mode".into(), "@place".into()), ("all".into(), "true".into()), ("panel".into(), "@free".into())],
            by: By::Person, preview: false, ticket: Ticket(ticket) };
        mail.push(invoke(1));
        let brought = engine.step(mail.take(), &mut |_| {});
        let (_, id, _) = brought.parked.expect("a bring onto a cased board waits for the case");
        assert!(brought.routes.is_empty() && brought.rejected.is_empty());
        for job in brought.fills { mail.result(job.run()); }
        for job in brought.checks { mail.result(job.run()); }
        let tray = publish(&mut bridge, engine.design().clone(), &mut model, evaluator, &mail);
        let tray_call = tray.calls.iter().find(|c| c.board == name).unwrap();
        let libraries = model.env().libraries.clone();
        let started = settle(&mut engine, &mail);
        let [RouteStart { job: fab_elec_host::Job::Place(job), base, .. }] = started.routes.as_slice() else { panic!("the published case releases its place") };
        let lights = |board: &Board, circuit: &fab_elec_circuit::Circuit| board_mech_with(board, circuit, &board.drills()).ports.into_iter()
            .filter(|p| p.kind == fab_elec_parts::BodyPortKind::Light).map(|p| p.comp).collect::<BTreeSet<_>>();
        let owners = lights(&base.board, &job.circuit);
        assert!(job.add.is_empty() && job.rooms.iter().all(|r| r.room.is_some() && r.keepout.is_some()));
        assert_eq!(job.rooms.iter().map(|r| r.comp).collect::<BTreeSet<_>>(), owners);

        // Observe the worker's one real call. Comparing its answer after the rebuild
        // catches a stale call context even when the group still makes all its pipes.
        use fab_elec_route::{PlacePlan, PlacePlanner, PlanVerdict};
        use std::sync::Mutex;
        struct ObservedFactory(Arc<dyn PlacePlan>, Arc<Mutex<Option<PlanVerdict>>>);
        struct ObservedPlanner(Box<dyn PlacePlanner>, Arc<Mutex<Option<PlanVerdict>>>);
        impl PlacePlan for ObservedFactory {
            fn open(&self) -> Result<Box<dyn PlacePlanner>, String> {
                Ok(Box::new(ObservedPlanner(self.0.open()?, self.1.clone())))
            }
        }
        impl PlacePlanner for ObservedPlanner {
            fn plan(&mut self, board: &Board, circuit: &fab_elec_circuit::Circuit) -> Result<PlanVerdict, String> {
                let verdict = self.0.plan(board, circuit)?;
                assert!(self.1.lock().unwrap().replace(verdict.clone()).is_none(), "the first arrangement needed one call");
                Ok(verdict)
            }
        }
        let observed = Arc::new(Mutex::new(None));
        let mut job = job.clone();
        let plan = job.plan.take().expect("the engine handed the job the published case plan");
        job.plan = Some(Arc::new(ObservedFactory(plan, observed.clone())));
        let result = Arc::new(run_place_trial(base.clone(), job));
        let metrics = result.metrics.place.as_ref().unwrap();
        let guard = metrics.case_plan.as_ref().expect("the final offer reports its guard");
        let verdict = observed.lock().unwrap().take().expect("the worker asked the plan");
        assert_eq!((guard.calls, guard.replaces), (1, 0), "{name}: guard the seed's winner once");
        assert!(guard.note.is_none() && verdict.refused.is_empty() && guard.refused_pipes == Some(0),
            "{name}: the final arrangement is accepted quietly: {guard:?}");
        mail.preview(fab_elec_host::job_sample(result.clone(), By::Person, 1).unwrap());
        mail.push(Cmd::CommitPreview { seq: 1, tag: id.tag() });
        let kept = engine.step(mail.take(), &mut |_| {});
        assert!(kept.rejected.is_empty(), "{:?}", kept.rejected);
        assert!(kept.outcomes.iter().any(|(_, o)| matches!(o, Outcome::Done { notes, .. } if notes.iter().any(|(k, v)| k == "case" && v.contains("waited for the case")))));
        for job in kept.fills { mail.result(job.run()); }
        for job in kept.checks { mail.result(job.run()); }
        let ev = publish(&mut bridge, engine.design().clone(), &mut model, evaluator, &mail);
        settle(&mut engine, &mail);
        let design = engine.design();
        let board = design.unit(unit).unwrap().board.as_ref().unwrap();
        let case_ids: BTreeSet<_> = board.keepouts.iter().filter(|k| matches!(&k.origin,
            fab_elec_board::Origin::Derived { from: FeatureRef::Panel(s) } if s.starts_with(fab_elec_host::CASE_KEEPOUT)))
            .map(|k| Sel::Item(unit, k.id)).collect();
        let conflicts = fab_elec_design::engines::full(design).boards.iter().flat_map(|b| b.drc.iter()).filter(|v|
            v.kind == fab_elec_circuit::CheckKind::Keepout && v.items.iter().any(|item| case_ids.contains(item))).count();
        let errors: Vec<_> = case_results(&ev, design).into_iter().flat_map(|(_, _, _, findings)| findings)
            .filter(|v| v.severity == Severity::Error).map(|v| v.rule).collect();
        // The route verdict uses board units. Compare the script's unrounded records
        // as well, using the captured tray call on the finished board.
        let mut reference = CasePlan::open(&libraries, tray_call).unwrap();
        let ports = reference.ports_of(&board_mech_with(board, &design.unit(unit).unwrap().circuit, &board.drills()).splash()).unwrap();
        check_plan_records(&ev, name, reference.plan(ports).unwrap().keepouts);
        assert_eq!(guard.made_pipes, Some(case_pipes(&ev, name).len() as u32), "{name}: the guard predicts the rebuilt pipes");
        assert_eq!(verdict.rooms, engine.case_rooms(unit).unwrap().1, "{name}: the predicted rooms and carried keepouts equal the rebuild");
        assert_eq!(case_pipes(&ev, name).len(), owners.len(), "{name}: every LED brought from empty gets its pipe");
        assert!(errors.is_empty(), "{name}: no case error: {errors:?}");
        assert_eq!(conflicts, 0, "{name}: no part stands in a case keepout");
        assert!(metrics.no_room.is_empty(), "{name}: every part placed: {:?}", metrics.no_room);

        // One press of Route on the kept board, with the person's default effort.
        if name == "tonebox" {
            mail.push(Cmd::Invoke { tool: "elec.route".into(), args: vec![], by: By::Person, preview: false, ticket: Ticket(2) });
            let route = settle(&mut engine, &mail);
            let [RouteStart { job: fab_elec_host::Job::Route(job), base, .. }] = route.routes.as_slice() else {
                panic!("the placed tonebox starts a whole-board route")
            };
            // An empty list of preview steps checks just the final offer.
            let offers = fab_elec_route::replay(base.clone(), job, Vec::new());
            let [routed] = offers.as_slice() else { panic!("one final offer from Route") };
            assert!(routed.verified, "the tonebox route must be safe to keep");
            assert!(routed.open.is_empty(), "one Route press must finish tonebox: {:?}", routed.open);
        }

        // Unchanged board inputs answer at once, against the newer design and with its stamp.
        let mut same = (**engine.design()).clone();
        same.generation += 1;
        let stamp = same.generation;
        let Some(CaseUpdate::Checked { commands, failed: None, .. }) = bridge.cover(Arc::new(same)) else {
            panic!("{name}: unchanged inputs publish from the held evaluation at once")
        };
        assert!(commands.iter().any(|c| matches!(c, Cmd::CaseOutcome { unit: u, stamp: s, reason: None } if *u == unit && *s == stamp)),
            "{name}: the outcome carries the newer stamp");
        assert!(commands.iter().any(|c| matches!(c, Cmd::CaseKeepouts { unit: u, rooms, .. } if *u == unit && !rooms.is_empty())),
            "{name}: with the rooms of the held evaluation");
        // A failed evaluation of these inputs answers every board with the evaluator's words.
        let read = bridge.read.as_ref().unwrap();
        let (inputs, sent) = (read.inputs.clone(), read.sent);
        let Some(CaseUpdate::Checked { commands, failed: Some(why), .. }) = bridge.reply(&inputs, Some(sent), Err("roof: no solid".into())) else {
            panic!("{name}: a failed evaluation is answered")
        };
        assert_eq!(why, "roof: no solid");
        assert!(commands.iter().any(|c| matches!(c, Cmd::CaseOutcome { unit: u, reason: Some(r), .. } if *u == unit && r == "roof: no solid")),
            "{name}: the waiting place hears why");
    }

    /// Why: the guard asks the case's own rule, so on the tone box (one LED) the plan through an
    /// isolate is the case's records too.
    #[test]
    fn the_tone_box_plan_through_an_isolate_is_the_cases_own() {
        let demo = crate::demo_part::demo("tonebox").expect("the tone box demo");
        let mut model = demo.model().unwrap_or_else(|e| panic!("the demo loads: {e}"));
        model.set_env(doc_env(model.docs(), model.elec_assets()));
        let mut evaluator = fab_model::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
        let (_, ev) = model.evaluate(&mut evaluator, None).unwrap_or_else(|f| panic!("the case builds: {f}"));
        let design = docs_design(model.docs(), model.elec_assets(), None).unwrap_or_else(|e| panic!("the demo's documents build: {e}"));
        let unit = design.unit_by_name("tonebox").expect("the board unit");
        let board = unit.board.as_deref().expect("a board");
        let text = board_mech_with(board, &unit.circuit, &board.drills()).splash();
        check_plan_equals_case(&ev, &doc_env(model.docs(), model.elec_assets()).libraries, &text, "tonebox", 1);
    }

    #[test]
    fn the_led_chaser_case_rooms_resolve_on_ledctl() {
        let source = r#"use mod.model.*
use mod.fab.*
let case = enclosure(board("ledctl"), {pipes: {}})
object(case.base, name: "Base")
let lid = [case.lid]
let clear = Material{albedo: [0.9 0.92 0.9] name: "Clear"}
for p in case.pipes {
    mod.model.fill(p.pipe, clear)
    lid.push(p.pipe)
}
object(lid, name: "Lid")
"#;
        let mut model = fab_model::FabModel::with_script("case", "case.cad.splash", source);
        model.set_env(script_env());
        for row in ["@z", r#""x""#, "nil"] {
            let invalid = source.replace("pipes: {}", &format!("pipes: {{row: {row}}}"));
            let mut case = fab_model::FabModel::with_script("case", "case.cad.splash", &invalid);
            case.set_env(model.env().clone());
            let Err(error) = case.compile() else { panic!("the case accepted row: {row}") };
            let mut location = error.message.splitn(4, ':');
            assert_eq!(location.next(), Some("enclosure.splash"), "{error}");
            for _ in 0..2 {
                assert!(location.next().and_then(|n| n.parse::<u32>().ok()).is_some_and(|n| n > 0), "{error}");
            }
            assert_eq!(location.next().map(str::trim), Some("assertion failed: pipes.row must be @x or @y"));
        }
        let mut evaluator = fab_model::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
        let (_, ev) = model.evaluate(&mut evaluator, None).expect("the controller's piped lid builds");
        let design = design();
        let publications = case_keepouts(&ev, &design);
        let [publication] = publications.as_slice() else { panic!("one publication for the controller") };
        check_led_port_rooms(&ev, &design, publication);
        let unit = &design.units[0];
        let board = unit.board.as_ref().unwrap();
        let text = board_mech_with(board, &unit.circuit, &board.drills()).splash();
        check_plan_equals_case(&ev, &script_env().libraries, &text, &unit.name, 8);
        let expected = publication.rooms.clone();
        // A later board can lose a port while its printed block still stands.
        let missing = expected[0].comp;
        let mut later = design.clone();
        let board = Arc::make_mut(later.units[0].board.as_mut().unwrap());
        board.placed.retain(|p| p.comp != missing);
        board.reindex();
        let later = case_keepouts(&ev, &later);
        assert_eq!(later[0].rooms.len(), 7);
        assert!(later[0].rooms.iter().all(|p| p.comp != missing));
        let old = expected[0].keepout.as_ref().unwrap();
        let mut fixed_sets = publication.sets.clone();
        let (key, items) = fixed_sets.iter_mut().find(|(key, _)| old.from == FeatureRef::Panel(format!("{}{key}", fab_elec_host::CASE_KEEPOUT))).unwrap();
        let [Item::Keepout(k)] = items.as_slice() else { panic!("one block per port") };
        *key = case_set(&k.reason, None);
        fixed_sets.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(later[0].sets, fixed_sets, "the missing port's block becomes a fixed set with the same reason and area");

        // Repeat the publication with a common display name for every solid. The fixed
        // records share one set, and the eight port features still have eight other sets.
        let mut same_name = ev;
        for part in same_name.plate.parts.iter_mut().chain(same_name.plate.excluded.iter_mut().map(|x| &mut x.part)) {
            for k in part.keepouts.iter_mut().filter(|k| !k.room) { k.name = "Cover".into(); }
        }
        let published = case_keepouts(&same_name, &design);
        let sets = &published[0].sets;
        assert_eq!(sets.len(), 9);
        assert!(sets.iter().any(|(key, _)| key == "Cover"));
        assert!(sets.iter().flat_map(|(_, items)| items).all(|i| matches!(i, Item::Keepout(k) if k.reason == "Cover")));
        assert_eq!(published[0].rooms.iter().map(|p| &p.keepout.as_ref().unwrap().from).collect::<BTreeSet<_>>().len(), 8);

        // Both trials bring every footprint through the host's ordinary tray path.
        // Use the same published sets on each empty board, including the old LED blocks.
        let mut empty = design.clone();
        let board = Arc::make_mut(empty.units[0].board.as_mut().unwrap());
        board.placed.clear();
        board.tracks.clear();
        board.vias.clear();
        board.reindex();
        let mut engine = Engine::new(empty.clone(), Arc::new(Factories::new()), Arc::default(), Arc::default(), "place".into()).with_app_tools();
        let mailbox = fab_elec_host::Mailbox::default();
        // A job may already be waiting when the publication reaches the strand.
        mailbox.push(Cmd::Invoke {
            tool: "elec.place".into(), args: vec![("mode".into(), "@place".into()), ("all".into(), "true".into()), ("panel".into(), "@free".into())],
            by: By::Person, preview: false, ticket: fab_elec_host::Ticket(1),
        });
        let evaluated = publication.generation;
        for CaseKeepouts { unit, generation, sets, rooms } in publications {
            mailbox.push(Cmd::CaseKeepouts { unit, generation, sets, rooms, plan: Ok(None) });
        }
        loop {
            let step = engine.step(mailbox.take(), &mut |_| {});
            assert!(step.rejected.is_empty(), "the place job starts: {:?}", step.rejected);
            if let [start] = step.routes.as_slice() {
                let fab_elec_host::Job::Place(job) = &start.job else { panic!("a place job") };
                assert_ne!(start.base.board.generation, evaluated, "applying the sets advanced the board");
                assert_eq!(job.rooms, expected, "the job carries all eight rooms with their block geometry and set names");
                assert_eq!(job.rooms.iter().map(|p| &p.keepout.as_ref().unwrap().from).collect::<BTreeSet<_>>().len(), 8);
                assert!(start.base.board.placed.is_empty());
                assert_eq!(job.add.len(), design.units[0].board.as_ref().unwrap().placed.len());
                check_carried_keepouts(&start.base, job);
                break;
            }
            assert!(!step.fills.is_empty() || !step.checks.is_empty(), "the job can only be waiting for its committed base");
            for fill in step.fills { mailbox.result(fill.run()); }
            for check in step.checks { mailbox.result(check.run()); }
        }
        place_empty_with_case("ledctl", model, &mut evaluator);
        for name in ["ledavr", "tonebox"] {
            let model = crate::demo_part::demo(name).unwrap().model().unwrap();
            place_empty_with_case(name, model, &mut evaluator);
        }
    }

    /// Why: the user's end goal for the suite is the LED chaser's board in a printed enclosure
    /// (11-17). The demo's case builds round its own board: its base, lid and the button's press
    /// pin print, the board rides in the base off the print, and the case checks (fit, reach,
    /// depth, hold) find nothing but notes.
    #[test]
    fn the_led_chaser_case_holds_its_board_and_passes_its_checks() {
        let demo = crate::demo_part::demo("ledavr").expect("the LED chaser demo");
        let mut model = demo.model().unwrap_or_else(|e| panic!("the demo loads: {e}"));
        model.set_env(doc_env(model.docs(), model.elec_assets()));
        let mut evaluator = fab_model::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
        let (compiled, ev) = model.evaluate(&mut evaluator, None).unwrap_or_else(|f| panic!("the case builds: {f}"));
        // The lid's object: the lid and a light pipe per LED, glued, each pipe with its optics.
        let pipes: Vec<&fab_plate::Part> = ev.plate.parts.iter().filter(|part| part.optics.is_some()).collect();
        assert_eq!(pipes.len(), 8, "a light pipe per LED");
        let lid_group = pipes[0].body_of.clone();
        assert!(lid_group.is_some() && pipes.iter().all(|pipe| pipe.body_of == lid_group), "the pipes print into the lid");
        let mut names: Vec<&str> = ev.plate.parts.iter().filter(|part| part.optics.is_none()).map(|part| part.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["Base", "Lid", "Pin btn.sw"], "the case prints its base, its lid and the button's press pin");
        assert!(ev.plate.parts.iter().any(|part| part.name == "Lid" && part.body_of == lid_group), "the lid holds the pipes");
        let generated: Vec<&str> = (0..ev.plate.parts.len() + ev.plate.excluded.len()).map(|i| ev.placed_part(i).name.as_str()).filter(|name| name.contains('#')).collect();
        assert!(generated.is_empty(), "nothing in the case shows a generated name: {generated:?}");
        let base = ev.plate.parts.iter().find(|part| part.name == "Base").expect("the base prints");
        let board = ev.plate.excluded.iter().find(|x| x.part.name == "Board").expect("the board is on the plate, off the print");
        assert_eq!(board.insert.as_ref().map(|insert| insert.into.as_str()), Some(base.object.as_str()), "the board rides in the base");
        let design = docs_design(model.docs(), model.elec_assets(), None).unwrap_or_else(|e| panic!("the demo's documents build: {e}"));
        let unit = design.unit_by_name("ledavr").expect("the demo's board unit");
        let solids = case_solids(&ev, "ledavr");
        let mut names: Vec<&str> = solids.iter().map(|solid| solid.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["Base", "Lid"], "the checks find the base and the lid in the board's frame, by their names");
        // Each pipe is linked to its LED: one pipe per light port facing up.
        let board = unit.board.as_deref().expect("the demo has a board");
        let mech = board_mech_with(board, &unit.circuit, &board.drills());
        let program = &compiled.instances[0].script.program;
        let mut linked: Vec<String> = pipes.iter().filter_map(|pipe| {
            let (_, solid) = fab_model::program::split_id(&pipe.object)?;
            program.solids.iter().find(|s| s.id == solid)?.link.clone()
        }).collect();
        linked.sort();
        let mut lights: Vec<String> = mech.ports.iter().filter(|port| port.kind == fab_elec_parts::BodyPortKind::Light)
            .filter_map(|port| fab_elec_mech::keys::feature_key(&fab_elec_core::FeatureRef::Port(fab_elec_core::CompRef { board: mech.unit, comp: port.comp }, port.index)))
            .collect();
        lights.sort();
        assert_eq!(linked, lights, "D1..D8 each have their pipe, linked to the LED's feature");
        let mut piped: Vec<String> = pipes.iter().map(|pipe| pipe.name.clone()).collect();
        piped.sort_unstable();
        let mut by_led: Vec<String> = mech.ports.iter().filter(|port| port.kind == fab_elec_parts::BodyPortKind::Light)
            .map(|port| format!("Light pipe {}", port.refdes)).collect();
        by_led.sort_unstable();
        assert!(by_led.len() == 8 && by_led.iter().all(|name| name.starts_with("Light pipe D")), "the LEDs are designated D: {by_led:?}");
        assert_eq!(piped, by_led, "each pipe reads by the LED it serves");
        // The ISP header is plugged from above: it passes through the lid, which stays low over
        // the parts it covers.
        let header_top = fab_elec_core::to_mm(board.stackup.thickness + mech.envelope[0]);
        assert!(mech.covered < mech.envelope[0], "the header is the tallest part, and a lid need not cover it");
        let lid_top = solids.iter().filter(|solid| solid.name == "Lid").flat_map(|solid| solid.positions.iter().map(|p| p[2])).fold(f64::MIN, f64::max);
        assert!(lid_top < header_top, "the lid ({lid_top} mm) stays under the header's top ({header_top} mm)");
        let findings = case_results(&ev, &design);
        let (_, _, _, found) = findings.iter().find(|(id, ..)| *id == unit.id).expect("the board has its case findings");
        let loud: Vec<_> = found.iter().filter(|v| v.severity != fab_elec_core::Severity::Info)
            .map(|v| format!("{:?} {:?}: {}: {}", v.kind, v.severity, v.rule, v.message)).collect();
        assert!(loud.is_empty(), "the case checks find no errors or warnings: {loud:#?}");

        use fab_elec_circuit::CheckKind;
        use fab_elec_core::{mm, LayerId, LayerSet, Side};
        use fab_elec_design::engines;

        let publications = case_keepouts(&ev, &design);
        let publication = publications.iter().find(|p| p.unit == unit.id).expect("the demo has case keepouts");
        check_led_port_rooms(&ev, &design, publication);
        check_plan_equals_case(&ev, &doc_env(model.docs(), model.elec_assets()).libraries, &mech.splash(), "ledavr", 8);
        let held = apply_case_keepouts(publications, &design);
        let held_board = held.unit(unit.id).unwrap().board.as_deref().unwrap();
        let records = &held_board.keepouts;
        let rooms: Vec<_> = ev.plate.parts.iter().flat_map(|p| &p.keepouts).filter(|k| k.board == unit.name && k.room).collect();
        assert_eq!(rooms.len(), 8, "the lid reports one room per LED");
        for room in rooms {
            assert!(records.iter().all(|k| k.reason != room.name), "{} reached the board as a keepout", room.name);
        }
        let blocks: Vec<_> = records.iter().filter(|k| k.reason.starts_with("Light pipe block ")).collect();
        assert_eq!(blocks.len(), 8, "the board has one light block keepout per LED");
        let top = LayerSet::of(&[LayerId::F_CU]);
        let bottom = LayerSet::of(&[LayerId::B_CU]);
        for port in mech.ports.iter().filter(|port| port.kind == fab_elec_parts::BodyPortKind::Light) {
            let name = format!("Light pipe block {}", port.refdes);
            let block = blocks.iter().find(|k| k.reason == name).expect(&name);
            assert!(block.area.contains(P::new(port.at[0], port.at[1])), "{name} covers its LED");
            assert_eq!(block.layers, top, "{name} keeps the top side clear");
        }
        let roof = blocks[0].z.expect("the block has a band").1;
        // Both holes hold the right edge. The two free corners on the left each get a
        // presser and a pin, with one record for each support even after grouping by name.
        let corners = [P::mm(2.5, 2.5), P::mm(2.5, 27.5)];
        assert_eq!(mech.holes.len(), 2, "the demo's board has its two mounting holes");
        let check_supports = |name: &str, points: &[P], layers, height, diameter| {
            let supports: Vec<_> = records.iter().filter(|k| k.reason == name).collect();
            assert_eq!(supports.len(), points.len(), "{name} count");
            for &at in points {
                let k = supports.iter().find(|k| k.area.bbox().center() == at).expect(name);
                let bounds = k.area.bbox();
                assert_eq!((bounds.width(), bounds.height()), (i64::from(diameter), i64::from(diameter)), "{name} footprint");
                assert!(!k.area.contains(bounds.min), "{name} follows the round column");
                assert_eq!(k.layers, layers, "{name} side");
                assert_eq!(k.z, Some((0, height)), "{name} band");
            }
        };
        check_supports("Corner presser", &corners, top, roof, mm(4.0));
        check_supports("Rest pin", &corners, bottom, mm(4.6), mm(4.0));
        for h in &mech.holes {
            check_supports(&format!("Lid post {}", h.name), &[h.at], top, roof, mm(6.0));
            check_supports(&format!("Standoff {}", h.name), &[h.at], bottom, mm(4.6), mm(8.0));
        }

        let reserved = |checks: &engines::DesignResults, id| {
            checks.boards.iter().flat_map(|b| b.drc.iter())
                .filter(|v| v.items.contains(&Sel::Item(unit.id, id)))
                .cloned().collect::<Vec<_>>()
        };
        let checks = engines::full(&held);
        for block in &blocks {
            let found = reserved(&checks, block.id);
            assert!(found.is_empty(), "DRC spares LEDs under {}: {found:?}", block.reason);
        }
        let inputs = |design: &Design| {
            let unit = design.unit(unit.id).expect("the demo's board unit");
            let board = unit.board.as_deref().expect("the demo has a board");
            let inputs = board_inputs(&unit.name, board, &unit.circuit, &board.drills());
            (inputs.board.hash, inputs.bodies.hash)
        };
        assert_eq!(inputs(&held), inputs(&design), "all the case keepouts leave board(..) and its bodies identical");
        let tall = board.placed.iter().filter(|p| p.side == Side::Top).max_by_key(|p| p.body_z.map(|(_, top)| top)).expect("the board has top parts");
        let presser = records.iter().find(|k| k.reason == "Corner presser" && k.area.contains(corners[0])).expect("a presser over the first free corner");
        for target in [blocks[0], presser] {
            let mut moved = held.clone();
            let by = target.area.bbox().center().sub(tall.at);
            moved.apply(&DesignOp::Board { unit: unit.id, op: BoardOp::Move { item: tall.id, by } }, true).expect("the tallest part moves");
            let found = reserved(&engines::full(&moved), target.id);
            let hit = found.iter().find(|v| v.kind == CheckKind::Keepout && v.items.contains(&Sel::Item(unit.id, tall.id)))
                .unwrap_or_else(|| panic!("{} in {} gives a keepout finding for both items: {found:?}", tall.path, target.reason));
            assert!(hit.message.contains(&target.reason), "the finding should name {}: {}", target.reason, hit.message);
        }
    }

    /// The AI reads the plate's board facts as Splash values through the case language.
    #[test]
    fn the_ai_reads_the_board_the_case_reads() {
        use fab_elec_core::{mm, Nm};
        use makepad_model_script::lang::{evaluate, Arg, Options, RootFn, V, NO_INPUTS};

        fn field<'a>(value: &'a V, name: &str) -> &'a V {
            value.obj().and_then(|object| object.get(name)).unwrap_or_else(|| panic!("the reply has field {name}"))
        }
        fn list(value: &V) -> &[V] {
            let V::Arr(items) = value else { panic!("the reply field is a list") };
            items
        }
        fn text(value: &V) -> &str {
            match value {
                V::Str(text) | V::Id(text) => text,
                _ => panic!("the reply field is text or a symbol"),
            }
        }
        let lengths = |value: &V| list(value).iter().map(|value| {
            (value.num().expect("a length evaluates as an SI quantity") * 1_000_000_000.0).round() as Nm
        }).collect::<Vec<_>>();

        let sources = fab_elec_fixtures::docs("ledctl").expect("the controller has fixture documents");
        let docs: fab_model::Docs = sources.docs().iter().filter(|doc| doc.is_file())
            .map(|doc| (doc.name.clone(), doc.text.clone())).collect();
        let assets = fab_model::ElecAssets::default();
        let reply = board_answer(&docs, &assets, None, None).expect("the plate's only board needs no name");
        let filtered = board_answer(&docs, &assets, None, Some("SW1")).expect("the plate's button can be read by refdes");
        let source = format!("mod.fab.read({reply})\nmod.fab.read({filtered})");
        let program = evaluate(&source, &Options {
            generated: &Default::default(),
            assets: &Default::default(),
            images: &Default::default(),
            extension: "fab",
            libraries: &[],
            inputs: &NO_INPUTS,
            unit: 1000.0,
            roots: &[RootFn { name: "read", args: &["value"], model: false }],
            overrides: Default::default(),
            weave: None,
            solved: Default::default(),
        }).expect("the AI's replies are Splash values the case language can read");
        let replies: Vec<_> = program.roots.iter().map(|root| {
            let Some(Arg::Value(value)) = root.get("value") else { panic!("each read returns its Splash value") };
            value
        }).collect();
        assert_eq!(replies.len(), 2, "both board replies reach the reader");
        let (reply, filtered) = (replies[0], replies[1]);
        assert_eq!(text(field(reply, "board")), "ledctl", "the reply names the plate's board");
        assert_eq!(lengths(field(reply, "size")), [mm(50.0), mm(30.0)], "the reply gives the board's size in millimetres");
        let button = list(field(reply, "parts")).iter().find(|part| text(field(part, "ref")) == "SW1")
            .expect("the reply includes SW1");
        assert_eq!(lengths(field(button, "at")), [mm(38.0), mm(9.0)], "SW1 uses its board placement");
        let parts = list(field(filtered, "parts"));
        assert_eq!(parts.len(), 1, "only SW1 returns one part");
        assert_eq!(text(field(&parts[0], "ref")), "SW1", "the selected part is SW1");
        assert_eq!(lengths(field(filtered, "size")), [mm(50.0), mm(30.0)], "filtering keeps the complete board size");

        let manifest = fab_ai::manifest();
        let tool = manifest.tools.iter().find(|tool| tool.name == "board").expect("Fab offers the board tool to its AI");
        assert_eq!(tool.description, fab_elec_mech::read::ACTIONS[0].tool_description(), "the tool uses the action catalog's description");
        assert_eq!(tool.risk, makepad_ai_services::wire::Risk::Read, "reading a board changes nothing");
        let refusal = board_answer(&fab_model::Docs::default(), &assets, None, None)
            .expect_err("an empty plate must not answer with a stock board");
        assert_eq!(refusal, "This plate has no board: it has no electronics documents.", "the refusal explains the missing board");
        assert_eq!(refusal.lines().count(), 1, "the refusal is one line");
    }

    /// A case script reads its board before the Electronics workspace starts (the model's
    /// standard env, [`script_env`]): registering the env builds no board, and opening a plate
    /// whose case calls `board("ledctl")` builds that board once and the case with it.
    #[test]
    fn a_case_reads_its_board_before_the_electronics_workspace_starts() {
        let boards = Arc::new(LazyBoards::new(design));
        let env = LazyBoards::env(boards.clone());
        assert_eq!(boards.built(), 0, "registering the env builds no board");
        let dir = std::env::temp_dir().join(format!("fab-elec-board-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the plate's folder");
        let path = dir.join("case.cad.splash");
        std::fs::write(&path, "use mod.model.*\nuse mod.fab.*\nlet pcb = board(\"ledctl\")\nlet case = enclosure(pcb, {})\nobject(case.base, name: \"Base\")\nobject(board_solid(pcb, board_bodies(\"ledctl\"), case.frame), name: \"Board\")\n").expect("the case script");
        let mut session = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), dir.clone());
        session.model.set_env(env);
        let opened = session.open(&path);
        std::fs::remove_dir_all(&dir).ok();
        let ev = opened.expect("the plate opens with no electronics host running");
        assert_eq!(boards.built(), 1, "the case's two reads of its board build it once");
        let case = case_solids(&ev, "ledctl");
        assert!(case.iter().any(|c| c.name == "Base"), "the case's base builds through the board's frame: {:?}", case.iter().map(|c| &c.name).collect::<Vec<_>>());
    }

    /// The controller with an imported regulator and a tiny in-memory body mesh; no files or UI.
    fn imported_mesh_plate() -> (Arc<fab_model::Docs>, Arc<fab_model::ElecAssets>) {
        use fab_elec_import::{glb::write_glb, Material, MeshBuilder, Normals};
        let asset = "parts/lcsc/C1.glb";
        // A closed square pyramid in mm: its apex cannot come from the fallback body's box.
        let mut builder = MeshBuilder::new();
        let grey = builder.add_material(Material::named("body"));
        let base = [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0]];
        let normals = Normals::Crease { domain: 0, angle: 0.5 };
        assert!(builder.polygon(grey, &[base[3], base[2], base[1], base[0]], normals), "the base is a polygon");
        for i in 0..4 {
            assert!(builder.polygon(grey, &[base[i], base[(i + 1) % 4], [0.0, 0.0, 2.0]], normals),
                "pyramid side {i} is a triangle");
        }
        let bytes: Arc<[u8]> = write_glb(&builder.finish()).into();
        let assets = Arc::new(fab_model::ElecAssets::from([(asset.into(), bytes)]));
        let stock = fab_elec_fixtures::docs("ledctl").expect("the controller has stock documents");
        // A plate holds files: the stock's empty system document is none (`Doc::is_file`).
        let mut texts: fab_model::Docs = stock.docs().iter()
            .filter(|d| d.is_file()).map(|d| (d.name.clone(), d.text.clone())).collect();
        let circuit = texts.get_mut("ledctl.ckt.splash").expect("the stock controller has a circuit");
        let edited = circuit.replace("params: {part: \"LDO_3V3_SOT23-5\"", "params: {part: \"lcsc:C1\"");
        assert_ne!(edited, circuit.as_ref(), "the regulator now uses the imported part");
        *circuit = edited.into();
        texts.insert("parts/lcsc/C1.part.splash".into(), Arc::from(r#"let e = mod.elec
let f = e.f
e.define("lcsc:C1", {
    kind: @regulator
    symbol: f.ic_symbol("C1", {left: [["VIN", @power_in], ["EN", @in]] right: [["VOUT", @power_out]]
        bottom: [["GND", @power_in]] top: [["NC", @no_connect]]})
    footprint: mod.std.sot23({pins: 5})
    pads: {VIN: 1 GND: 2 EN: 3 NC: 4 VOUT: 5}
    body: {mesh: "C1.glb" height: 2mm offset: [0.3mm, -0.2mm, 0.4mm] rotation: [0deg, 0deg, 30deg]}
})
"#));
        (Arc::new(texts), assets)
    }

    /// A plate's imported body comes from its assets, whether unsaved, saved or recovered with
    /// only its working copy, in the case and in the electronics engine's design and 3D scene.
    /// The posed pyramid apex distinguishes its mesh from the plan box.
    #[test]
    fn a_plate_resolves_part_meshes_from_its_assets() {
        use fab_elec_core::to_mm;
        use fab_elec_import::cache::MeshCache;
        use fab_elec_parts::BodySource;
        use fab_model::{project, ElecAssets, FabModel};
        use std::path::{Path, PathBuf};

        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the epoch").as_nanos();
        let dir = std::env::temp_dir().join(format!("fab-plate-assets-{}-{unique}", std::process::id()));
        std::fs::create_dir(&dir).expect("create the temporary project folder");
        struct TempProject(PathBuf);
        impl Drop for TempProject {
            fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
        }
        let _project = TempProject(dir.clone());
        // Tests run in the package folder. A relative filesystem read cannot stand in for the
        // plate's asset; nothing here changes the process working directory.
        let asset = "parts/lcsc/C1.glb";
        assert!(!Path::new(asset).exists(), "the process folder has no substitute mesh");
        let (docs, assets) = imported_mesh_plate();
        let hash = makepad_core_util::sha256::sha256_hex(assets.get(asset).expect("the imported plate has its mesh"));
        let key = format!("asset:{hash}/{asset}");
        let mut resolved = docs_design(&docs, &assets, None).expect("the plate's documents evaluate with their assets");
        let unit = resolved.unit_by_name("ledctl").expect("the controller evaluates");
        let component = unit.circuit.comp("reg.u").expect("the regulator was placed");
        assert_eq!(component.part.body.as_ref().expect("the part has a body").source, BodySource::Mesh(key.clone()),
            "the body carries its content-addressed asset key");
        let board = unit.board.as_deref().expect("the controller has a board");
        let mech = fab_elec_mech::board_mech(board, &unit.circuit);
        let body = mech.bodies.iter().find(|b| b.path == "reg.u").expect("the mesh has a board pose");
        let [x, y, z] = body.pose.apply_f([0.0, 0.0, 2e6]).map(|v| v * 1e-6);
        let apex = [x - 25.0, 3.0 + z, 15.0 - y];
        let corner = [to_mm(body.max.x) - 25.0, 3.0 + to_mm(mech.thickness + body.height), 15.0 - to_mm(body.max.y)];
        let circuit = unit.circuit.clone();
        let part = component.part.clone();
        resolved.resolve_meshes(&BTreeMap::from([(asset.to_string(), key.clone())]));
        assert!(Arc::ptr_eq(&circuit, &resolved.units[0].circuit), "already resolved meshes keep the shared circuit");
        assert!(Arc::ptr_eq(&part, &resolved.units[0].circuit.comp("reg.u").expect("the regulator remains").part),
            "already resolved meshes keep the shared part");
        let cached = MeshCache::shared().get(Path::new(&key)).expect("the virtual key reads without a file");
        MeshCache::shared().put(&key, b"not parsed again").expect("an existing key is a no-op");
        assert!(Arc::ptr_eq(&cached.mesh, &MeshCache::shared().get(Path::new(&key)).expect("the asset stays cached").mesh),
            "a repeated put preserves the parsed mesh");
        let missing = format!("asset:{hash}/parts/lcsc/missing.glb");
        let issue = MeshCache::shared().get(Path::new(&missing)).expect_err("an unknown virtual key is unavailable");
        assert!(issue.field.contains(&missing), "the cache issue names the missing key");

        // The electronics engine (the Circuit tab, the board's 3D view) loads the same plate: its
        // design reads the body from the assets, and its scene hands the view the asset key.
        let (engine, engine_docs, _, _) = plate_project(&docs, &assets, None);
        assert!(engine_docs.is_some(), "the engine holds the plate's documents");
        let engine_unit = engine.unit_by_name("ledctl").expect("the engine evaluates the controller with its assets");
        let engine_body = engine_unit.circuit.comp("reg.u").expect("the engine places the regulator").part.body.clone();
        assert_eq!(engine_body.expect("the engine's part has a body").source, BodySource::Mesh(key.clone()),
            "the engine's body reads the asset key");
        let scene = scene_of(&engine, &Pose::default());
        let body = scene.boards.iter().flat_map(|b| &b.bodies).find(|b| b.mesh == fab_elec_mech::scene::MeshRef::Mesh(key.clone()))
            .expect("the 3D view's scene names the asset key");
        assert!(!fab_elec_mech::mesh::file_body_meshes(&key, body.class).expect("the view reads the asset without a file").is_empty(),
            "the view draws the asset's mesh");
        let (bare, _, _, _) = plate_project(&docs, &ElecAssets::new(), None);
        let waiting = bare.unit_by_name("ledctl").expect("without its assets the controller still evaluates in the engine")
            .circuit.comp("reg.u").expect("the engine places the regulator").part.body.clone().expect("the regulator keeps a body");
        assert_eq!((waiting.source, waiting.missing_mesh.as_deref()), (BodySource::Extrude, Some("parts/lcsc/C1.glb")),
            "without its asset the regulator is its extruded courtyard, waiting for the mesh");
        assert_eq!(bare.missing_meshes(), ["parts/lcsc/C1.glb"], "the engine's design lists the missing mesh");

        let env = doc_env(&docs, &assets);
        assert!(env.same(&doc_env(&docs, &assets)), "the same documents and assets reuse the env");
        assert!(!env.same(&doc_env(&Arc::new((*docs).clone()), &assets)), "the documents pointer is part of the cache key");
        assert!(!env.same(&doc_env(&docs, &Arc::new((*assets).clone()))), "the assets pointer is part of the cache key");
        let source = r#"use mod.model.*
use mod.fab.*
let pcb = board("ledctl")
let frame = {cx: 25mm cy: 15mm y0: 3mm thickness: pcb.thickness}
object(board_solid(pcb, board_bodies("ledctl"), frame), name: "Board")
"#;
        let mut model = FabModel::with_script("case", "case.cad.splash", source);
        model.set_docs(docs.clone());
        model.set_elec_assets(assets.clone());
        let session = || {
            let mut s = fab_ai::Session::new(fab_plate::machine::Machine::dual_nozzle_350(), dir.join("Documents"));
            s.model.set_env(shared_env());
            s.env_for = Some(Arc::new(doc_env));
            s
        };
        let vertices = |s: &mut fab_ai::Session, ev: &Evaluated| {
            let object = ev.placed_part(0);
            assert_eq!(object.name, "Board", "the case builds the board object");
            s.eval.object_mesh(&object.object).expect("the board has a model-frame mesh").vertices
        };
        let has_vertex = |vertices: &[makepad_csg_math::Vec3d], p: [f64; 3]| vertices.iter().any(|v| {
            (v.x - p[0]).abs() < 1e-6 && (v.y - p[1]).abs() < 1e-6 && (v.z - p[2]).abs() < 1e-6
        });
        let store = fab_ai::plates::PlateStore::under(&dir.join("working"), &dir.join("Documents"));
        let mut unsaved = session();
        unsaved.store = Some(store.clone());
        let ev = unsaved.replace_model(model, None).expect("the unsaved plate builds");
        assert!(unsaved.path.is_none(), "the plate has never been saved to a project");
        assert!(has_vertex(&vertices(&mut unsaved, &ev), apex), "the unsaved Board includes the posed apex");

        let saved_dir = dir.join("saved");
        std::fs::create_dir(&saved_dir).expect("create the saved project folder");
        let path = saved_dir.join("plate.fab.splash");
        project::save(&unsaved.model, &path).expect("save the project with its documents and binary assets");
        let loaded = project::load(&path, &shared_env()).expect("reload the saved project");
        assert_eq!(loaded.docs(), &docs, "saved documents return unchanged");
        assert_eq!(loaded.elec_assets(), &assets, "saved assets return byte for byte");
        assert_eq!(fab_elec_lang::part_assets(&project::elec_dir(&path)).expect("list saved assets"), vec![asset],
            "the shared part walk separates assets from documents");
        let mut reloaded = session();
        let ev = reloaded.replace_model(loaded, Some(path.clone())).expect("the reloaded plate builds");
        assert!(has_vertex(&vertices(&mut reloaded, &ev), apex), "the reloaded Board includes the posed apex");
        drop(reloaded);
        std::fs::remove_dir_all(&saved_dir).expect("remove the saved project before working-copy recovery");
        assert!(!path.exists(), "recovery has no saved project to read");

        unsaved.autosave().expect("autosave the unsaved plate and its assets");
        let plate = unsaved.plate.clone().expect("the working copy belongs to a plate");
        drop(unsaved);
        let mut recovered = session();
        recovered.store = Some(store);
        let ev = recovered.recover().expect("the bed has a working copy").expect("the recovered plate builds");
        assert_eq!(recovered.plate.as_ref(), Some(&plate), "the same plate returns from the working copy");
        assert!(recovered.path.is_none(), "the recovered plate has no project path");
        assert_eq!(recovered.model.docs(), &docs, "working-copy documents return unchanged");
        assert_eq!(recovered.model.elec_assets(), &assets, "working-copy assets return byte for byte");
        assert!(has_vertex(&vertices(&mut recovered, &ev), apex), "the recovered Board includes the posed apex");

        let invalid = Arc::new(ElecAssets::from([(asset.into(), Arc::<[u8]>::from(&b"not a glb"[..]))]));
        assert!(!env.same(&doc_env(&docs, &invalid)), "changed asset bytes do not reuse the old env");
        let fallback = docs_design(&docs, &invalid, None).expect("invalid mesh bytes do not reject the design");
        let body = &fallback.units[0].circuit.comp("reg.u").expect("the imported part remains").part.body;
        let invalid_key = format!("asset:{}/{asset}", makepad_core_util::sha256::sha256_hex(b"not a glb"));
        assert_eq!(body.as_ref().expect("the body remains").source, BodySource::Mesh(invalid_key.clone()),
            "a failed registration still resolves to its key, never to a working-directory path");
        assert!(MeshCache::shared().get(Path::new(&invalid_key)).is_err(), "the failed asset reads as unavailable");
        let mut model = FabModel::with_script("invalid mesh", "case.cad.splash", source);
        model.set_docs(docs);
        model.set_elec_assets(invalid);
        let mut broken = session();
        let ev = broken.replace_model(model, None).expect("a non-glb asset falls back without a panic");
        let fallback = vertices(&mut broken, &ev);
        assert!(!has_vertex(&fallback, apex), "invalid bytes do not reuse the earlier valid pyramid");
        assert!(has_vertex(&fallback, corner), "invalid bytes retain the imported body's plan box");
        assert!(!Path::new(asset).exists(), "no case wrote a substitute mesh to the process folder");
    }

    /// Drive the real evaluation/adoption protocol inline, with no scheduler, fill work or clock.
    fn run_mesh_evaluations(engine: &mut Engine, mailbox: &fab_elec_host::Mailbox, mut step: fab_elec_host::Step) -> fab_elec_host::Step {
        let mut rounds = 0;
        loop {
            assert!(step.rejected.is_empty(), "mesh evaluation accepts its commands: {:?}", step.rejected);
            if step.evals.is_empty() {
                return step;
            }
            // an engine that keeps asking (its documents never caught up) fails here, not hangs
            rounds += 1;
            assert!(rounds <= 8, "the engine settles within a few evaluations");
            assert_eq!(step.evals.len(), 1, "only one mesh evaluation runs at a time");
            for job in step.evals {
                mailbox.result(job.run());
            }
            step = engine.step(mailbox.take(), &mut |_| {});
        }
    }

    /// A verified value edit re-lowers the imported part, then adoption restores its cache key.
    #[test]
    fn an_imported_mesh_survives_a_verified_circuit_edit() {
        use fab_elec_circuit::{CircuitOp, QtyUnit, Quantity, Value};
        use fab_elec_design::EntryKind;
        use fab_elec_parts::BodySource;
        let (texts, assets) = imported_mesh_plate();
        let (design, docs, keys, _) = plate_project(&texts, &assets, None);
        let key = keys.get("parts/lcsc/C1.glb").expect("loading registers the mesh key").clone();
        let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "mesh edit".into())
            .with_docs(docs.expect("the imported plate has documents")).with_mesh_keys(keys);
        let unit = engine.design().unit_by_name("ledctl").expect("the imported controller loads");
        let before = unit.circuit.clone();
        let mailbox = fab_elec_host::Mailbox::default();
        mailbox.push(Cmd::Commit {
            ops: vec![DesignOp::Circuit { unit: unit.id, op: CircuitOp::SetValue {
                comp: unit.circuit.comp("mcu.boot_r").expect("the controller has a boot resistor").id,
                value: Value { text: "4.7k".into(), qty: Some(Quantity { si: 4700.0, unit: QtyUnit::Ohm }), tol: None },
            } }],
            place: vec![], label: "value".into(), circuits: vec![], expect: None, by: By::default(), ticket: None,
        });
        let requested = engine.step(mailbox.take(), &mut |_| {});
        assert_eq!(requested.evals.len(), 1, "the value edit requests an evaluation");
        assert!(requested.evals[0].verify.is_some(), "the value edit takes the verified commit path");
        let finished = run_mesh_evaluations(&mut engine, &mailbox, requested);
        assert!(!finished.waiting, "adoption completes the verified value edit");
        let unit = engine.design().unit_by_name("ledctl").expect("adoption retains the controller");
        assert!(!Arc::ptr_eq(&before, &unit.circuit), "the test exercises adoption of a re-lowered circuit");
        assert_eq!(unit.circuit.comp("mcu.boot_r").and_then(|comp| comp.value.as_ref()).and_then(|value| value.qty).map(|qty| qty.si),
            Some(4700.0), "the adopted circuit has the committed resistor value");
        let body = unit.circuit.comp("reg.u").expect("adoption retains the imported regulator").part.body.as_ref()
            .expect("the imported regulator retains its body");
        assert_eq!(body.source, BodySource::Mesh(key), "a re-evaluated imported body keeps its registered mesh key");
        assert_eq!(engine.log().entries.len(), 1, "the verified edit creates one undo entry");
        assert_eq!(engine.log().entries[0].kind, EntryKind::Eval, "the edit was logged through evaluation adoption");
    }

    /// Registering a missing mesh gives the waiting unit its body without a text edit or undo entry.
    #[test]
    fn an_asset_added_to_the_same_bed_re_evaluates_without_history() {
        use fab_elec_parts::BodySource;
        let (texts, assets) = imported_mesh_plate();
        let (design, docs, keys, _) = plate_project(&texts, &fab_model::ElecAssets::new(), None);
        assert_eq!(design.missing_meshes(), ["parts/lcsc/C1.glb"], "the imported controller evaluates, waiting for its asset");
        let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "mesh addition".into())
            .with_docs(docs.expect("the load holds the plate's documents")).with_mesh_keys(keys);
        let log = engine.log().clone();
        let sources = engine.docs().expect("the engine has documents").sources().clone();
        let keys = asset_keys(&assets);
        let key = keys.get("parts/lcsc/C1.glb").expect("the new mesh is registered").clone();
        let mailbox = fab_elec_host::Mailbox::default();
        mailbox.push(Cmd::Docs {
            set: keys.into_iter().map(|(name, key)| DocSet::Asset { name, key }).collect(),
            label: "assets added".into(), by: By::default(), ticket: None,
        });
        let requested = engine.step(mailbox.take(), &mut |_| {});
        assert_eq!(requested.evals.len(), 1, "asset registration alone requests an evaluation");
        assert!(requested.evals[0].verify.is_none(), "asset registration evaluates without creating a commit");
        let docs = engine.docs().expect("asset registration retains the documents");
        assert!(docs.behind(), "new asset names make the design behind its sources");
        assert!(!docs.same_texts(&sources), "a job from before asset registration is stale even with identical text");
        assert!(requested.evals[0].prev.assets.is_empty(), "reuse remembers the assets of the previous evaluation");
        let finished = run_mesh_evaluations(&mut engine, &mailbox, requested);
        assert!(!finished.waiting, "the new asset evaluation reaches idle");
        let unit = engine.design().unit_by_name("ledctl").expect("the controller evaluates after its asset arrives");
        let body = unit.circuit.comp("reg.u").expect("the new unit has its imported regulator").part.body.as_ref()
            .expect("the imported regulator has a body");
        assert_eq!(body.source, BodySource::Mesh(key), "the newly evaluated unit uses the registered mesh key");
        assert!(!engine.docs().expect("the engine retains documents").behind(), "adoption follows the new asset list");
        assert_eq!(engine.docs().expect("the engine retains documents").texts(), texts, "adding assets leaves the document text unchanged");
        assert_eq!(engine.log(), &log, "asset registration and evaluation leave history unchanged");
    }

    /// The plate's mesh takes precedence over the library's. A cached model is visible on
    /// load and through an edit, and a later library arrival preserves the edit and its history.
    #[test]
    fn library_models_resolve_on_load_and_arrival_and_preserve_project_files() {
        use fab_elec_circuit::{CircuitOp, QtyUnit, Quantity, Value};
        use fab_elec_mech::scene::MeshRef;
        use fab_elec_parts::BodySource;
        // The library's models are EasyEDA's: this test is of a person who turned EasyEDA on.
        crate::part_models::set_sources(fab::model_sources::ModelSource::ALL.iter().fold(0, |bits, s| bits | s.bit()));
        let demo = crate::demo_part::DEMOS.iter().find(|demo| demo.key == "ledavr").expect("the LED demo");
        let docs: fab_model::Docs = demo.files.iter()
            .filter(|(name, _)| fab_elec_lang::DocKind::of_name(name).is_some())
            .map(|(name, text)| (name.to_string(), Arc::<str>::from(*text))).collect();
        let named = plate_project(&docs, &fab_model::ElecAssets::new(), None).0.missing_meshes();
        assert_eq!(named.len(), 8, "without a library the demo waits for its eight models: {named:?}");
        // The library holds all but the last model; the plate holds the first as its own file.
        let glb = imported_mesh_plate().1["parts/lcsc/C1.glb"].clone();
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the epoch").as_nanos();
        let root = std::env::temp_dir().join(format!("fab-part-library-{}-{unique}", std::process::id()));
        let library_file = |name: &str| {
            let number = name.strip_prefix("parts/lcsc/").and_then(|n| n.strip_suffix(".glb")).expect("an LCSC model path");
            root.join("lcsc").join(number).join(format!("{number}.glb"))
        };
        for name in &named[..7] {
            let file = library_file(name);
            std::fs::create_dir_all(file.parent().expect("the entry's folder")).expect("the library entry");
            std::fs::write(&file, &glb).expect("the library's model");
        }
        let (own, lacking) = (&named[0], &named[7]);
        let assets = fab_model::ElecAssets::from([(own.clone(), glb.clone())]);
        let mut expected: BTreeSet<String> = named[1..7].iter().map(|name| library_file(name).display().to_string()).collect();
        expected.insert(asset_keys(&assets)[own].clone());
        let mesh_keys = |design: &Design| -> BTreeSet<String> {
            design.units.iter().flat_map(|unit| unit.circuit.comps.iter())
                .filter_map(|comp| match &comp.part.body.as_ref()?.source {
                    BodySource::Mesh(key) => Some(key.clone()),
                    _ => None,
                }).collect()
        };
        let check = |design: &Design, what: &str| {
            assert_eq!(mesh_keys(design), expected, "{what}: the plate's model is its file, the library's six are the library's files");
            assert_eq!(design.missing_meshes(), [lacking.clone()], "{what}: only the model the library lacks waits for a fetch");
        };

        let (design, engine_docs, keys, library) = plate_project(&docs, &assets, Some(&root));
        check(&design, "the first design");
        let (available, _) = crate::part_models::fetchable(&fab_elec_import::easyeda::Lcsc, &design.missing_meshes());
        assert_eq!(available.len(), 1, "the queue needs only the model missing from both the plate and the library");
        let engine_docs = engine_docs.expect("the plate has documents");
        assert_eq!(engine_docs.sources().assets(), [own.clone()], "the library's models are no plate files");
        let scene = scene_of(&design, &Pose::default());
        let shown = library_file(&named[1]).display().to_string();
        assert!(scene.boards.iter().flat_map(|board| &board.bodies).any(|body| body.mesh == MeshRef::Mesh(shown.clone())),
            "the first scene draws the library's model");
        assert!(fab_elec_import::cache::MeshCache::shared().get(std::path::Path::new(&shown)).is_ok(), "the library's model reads as a mesh");
        // The card's and the AI's read of that part's model: the library's EasyEDA model, by its
        // note; this computer's KiCad choice does not apply to it.
        let with_model = design.units.iter().flat_map(|unit| unit.circuit.comps.iter())
            .find(|comp| comp.part.body.as_ref().is_some_and(|body| body.source == BodySource::Mesh(shown.clone())))
            .expect("a part drawn with the library's model");
        let part = || ("part".to_string(), with_model.path.clone());
        let read = fab_elec_host::model_actions::model_read(&design, None, &[part()]).expect("the model read");
        assert!(read.contains("body: \"easyeda\"") && read.contains("from: \"EasyEDA part service\"") && read.contains("here: true")
            && read.contains("easyeda: true") && read.contains("body_source: \"easyeda\"")
            && read.contains("{source: \"kicad\" offered: false why: \"no KiCad model\"}")
            && read.contains("{source: \"plain\" offered: true why: \"\"}"), "{read}");
        let refused = fab_elec_host::model_actions::model_from(&design, &[part(), ("from".into(), "download".into())])
            .expect_err("an EasyEDA model has no KiCad choice");
        assert!(refused.message.contains("no KiCad library model"), "{}", refused.message);

        let mut engine = Engine::new(design, Arc::new(Factories::new()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), "library".into())
            .with_docs(engine_docs).with_mesh_keys(keys).with_library_keys(library);
        assert!(engine.log().entries.is_empty(), "the library's models are no step to undo");
        let unit = engine.design().unit_by_name("ledavr").expect("the LED chaser loads");
        let before = unit.circuit.clone();
        let mailbox = fab_elec_host::Mailbox::default();
        mailbox.push(Cmd::Commit {
            ops: vec![DesignOp::Circuit { unit: unit.id, op: CircuitOp::SetValue {
                comp: unit.circuit.comp("mcu.rst_r").expect("the chaser has a reset resistor").id,
                value: Value { text: "4.7k".into(), qty: Some(Quantity { si: 4700.0, unit: QtyUnit::Ohm }), tol: None },
            } }],
            place: vec![], label: "value".into(), circuits: vec![], expect: None, by: By::default(), ticket: None,
        });
        let requested = engine.step(mailbox.take(), &mut |_| {});
        assert!(!run_mesh_evaluations(&mut engine, &mailbox, requested).waiting, "the value edit is adopted");
        assert!(!Arc::ptr_eq(&before, &engine.design().unit_by_name("ledavr").expect("the chaser stays").circuit),
            "the edit re-lowered the board");
        check(engine.design(), "after a re-lowering edit");

        check(&docs_design(&docs, &assets, Some(&root)).expect("the plate's env builds"), "the plate's env");

        let last = library_file(lacking);
        std::fs::create_dir_all(last.parent().expect("library entry")).expect("create the entry for the last model");
        std::fs::write(&last, &glb).expect("the last model arrives");
        let history = engine.log().clone();
        let texts = engine.docs().unwrap().texts();
        // Include the plate-owned name too, as when a fetch completes after a project file is added.
        let arrivals = library_meshes(Some(&root), &fab_model::ElecAssets::new());
        mailbox.push(Cmd::LibraryModels(arrivals.into_iter().collect()));
        let requested = engine.step(mailbox.take(), &mut |_| {});
        assert!(requested.docs.is_none(), "registering the library keys emits no project write");
        assert_eq!(requested.evals.len(), 1, "the missing model triggers an evaluation");
        assert!(requested.evals[0].verify.is_none(), "library arrival uses a plain evaluation");
        let finished = run_mesh_evaluations(&mut engine, &mailbox, requested);
        assert!(!finished.waiting && finished.docs.is_none(), "adoption settles without writing project files");
        let refreshed = engine.docs().unwrap();
        assert_eq!(refreshed.texts(), texts, "the project's edited documents are unchanged");
        assert_eq!(refreshed.sources().assets(), [own.clone()], "the plate still owns just its original asset");
        assert_eq!(engine.log(), &history, "the user's edit remains the only history entry");
        assert!(engine.design().missing_meshes().is_empty(), "the last placeholder is replaced");
        expected.insert(last.to_str().unwrap().to_owned());
        assert_eq!(mesh_keys(engine.design()), expected, "the live design uses the plate's own mesh and all seven library files");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn case_findings_follow_the_case_and_the_board() {
        use fab_elec_circuit::CheckKind;
        use fab_elec_core::{mm, CompRef, FeatureRef, Sel, Severity};
        use fab_model::eval::Evaluator;
        use fab_model::{Change, FabModel, PlaceArg};
        use fab_plate::machine::Machine;

        let mut design = ledctl();
        let unit = &design.units[0];
        let id = unit.id;
        let board = unit.board.as_deref().expect("the fixture has a board");
        let generation = board.generation;
        let first_inputs = board_inputs(&unit.name, board, &unit.circuit, &board.drills());
        let case_board = first_inputs.board.clone();
        let mut inputs = CaseInputs::default();
        inputs.update(first_inputs);
        let mut model = FabModel::with_script("case", "case.cad.splash", r#"use mod.model.*
use mod.fab.*
let pcb = board("ledctl")
let case = enclosure(pcb, {})
object(case.base, name: "Base")
object(case.lid_on, name: "LidOn")
object(board_solid(pcb, board_bodies("ledctl"), case.frame), name: "Board")
"#);
        model.set_env(inputs.env());
        let mut evaluator = Evaluator::new(Machine::dual_nozzle_350());
        let place = |object: &str, arg| Change::Place { instance: "case".into(), object: object.into(), arg };
        let changes = [
            place("Base", PlaceArg::At(Some([150.0, 150.0]))),
            place("LidOn", PlaceArg::At(Some([150.0, 150.0]))),
            place("LidOn", PlaceArg::Drop(false)),
            place("LidOn", PlaceArg::Print(false)),
            place("Board", PlaceArg::At(Some([150.0, 150.0]))),
            place("Board", PlaceArg::Drop(false)),
            place("Board", PlaceArg::Print(false)),
        ];
        let head = model.head();
        let (_, built) = model.apply(&mut evaluator, "case", head, &changes, None).expect("the framed case builds");
        let (_, ev) = built.expect("the first placement evaluates the case");
        assert_eq!(case_solids(&ev, &unit.name).len(), 2, "case setup: the base and assembled lid resolve through their board frames");
        let findings = case_results(&ev, &design);
        let (_, found_generation, cased, found) = findings.iter().find(|(unit, ..)| *unit == id).expect("passing case: unit 0 has an entry");
        assert!(*cased, "passing case: the board has its case on the plate");
        assert_eq!(*found_generation, generation, "passing case: findings carry the board's generation");
        assert!(found.is_empty(), "passing case: the fixture has no case findings: {found:#?}");

        let unit = &mut design.units[0];
        let usb = unit.circuit.comp("usb.j").expect("the fixture has its receptacle").id;
        let taller = Arc::make_mut(&mut unit.circuit);
        let part = Arc::make_mut(&mut taller.comps[usb.ix()].part);
        part.body.as_mut().expect("the receptacle has a body").height = mm(9.0);
        let board = Arc::make_mut(unit.board.as_mut().expect("the fixture has a board"));
        board.generation += 1;
        let new_generation = board.generation;
        let mut next_inputs = board_inputs(&unit.name, board, &unit.circuit, &board.drills());
        // As in EMECH's taller-body check, keep the enclosure fixed while rebuilding the
        // board's bodies; otherwise the automatic envelope would raise the lid too.
        next_inputs.board = case_board;
        assert!(inputs.update(next_inputs), "body edit: the taller body changes the plate's inputs");
        design.generation += 2;
        model.set_env(inputs.env());
        let (_, mut rebuilt) = model.evaluate(&mut evaluator, None).expect("the taller board rebuilds");
        let findings = case_results(&rebuilt, &design);
        let (_, found_generation, _, found) = findings.iter().find(|(unit, ..)| *unit == id).expect("taller body: unit 0 has an entry");
        assert_eq!(*found_generation, new_generation, "taller body: findings use the new board generation, not the design generation");
        let body = Sel::Feature(FeatureRef::Body(CompRef { board: id, comp: usb }));
        assert!(found.iter().any(|v| v.kind == CheckKind::Mech && v.severity == Severity::Error && v.items.contains(&body)), "taller body: the receptacle has a mechanical error against the assembled lid: {found:#?}");

        rebuilt.objects.clear();
        let cleared = case_results(&rebuilt, &design);
        assert_eq!(cleared, vec![(id, new_generation, false, vec![])], "removed case: a board without case frames has no case and sends an empty list to clear old findings");
    }
}

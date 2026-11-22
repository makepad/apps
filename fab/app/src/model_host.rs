//! The model worker: a strand owning the [`Session`] (the model
//! document, its evaluator, its file). The UI, the chat and external agents
//! send it tool calls; it answers each with the reply text and, when the
//! head moved, the rebuilt viewport scene, so the view follows every batch
//! live without the UI thread ever evaluating anything.
//!
//! Main-loop cost: posting a job is a strand submit; a finished job arrives as
//! one action carrying an `Arc<Scene>` the shell swaps in.

use crate::elec_host::{merge_files, DocsWrite};
use crate::plate_scene;
use crate::review::{self, PreparedReview};
use fab::api::PlateView;
use fab::model::Scene;
use fab_ai::{RenderRequest, Session};
use fab_model::eval::Evaluated;
use fab_model::params::PValue;
use fab_model::program::ScriptEnv;
use fab_model::Head;
use fab::params::PartParamsView;
#[cfg(test)]
use fab_plate::machine::Machine;
use makepad_model::json::Value;
use makepad_widgets::makepad_platform::retained_instances::RetainedInstances;
use makepad_widgets::*;
use std::path::PathBuf;
use makepad_widgets::makepad_platform::thread::{Class, Strand, TaskPool, Ticket, Version};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use makepad_widgets::jobs::{self, JobHandle, JobId, JobSpec};

/// Where the model worker's jobs run, as the jobs list says it.
pub const HERE: &str = "this computer";

pub enum Job {
    PrintSetup(crate::print::CheckSetup),
    /// A plate's or a demo's picture ended ([`crate::plate_paint`]): the list shows it, and the
    /// next one without a picture is painted.
    Painted(crate::plate_paint::Painted),
    /// The print colours mapped anew (material colours, job tools): the plate takes them
    /// without a rebuild.
    Mapped(Arc<fab_model::eval::Mapped>),
    /// The plate's grown patterns reached the steps they print at (`fab_paint_field::pattern::live`).
    PatternsGrown,
    /// The bends of the plate (by content) were measured after its build: a bond that could not
    /// be is an error on its report.
    BendsMeasured([u8; 32]),
    Overhangs(bool),
    /// The Slicer's choice: the ticked test prints (true) or the whole plate.
    TestMode(bool),
    OverhangList { key: (u64, u64) },
    /// Mesh the field parts too (Mesh shading) or not: the plate built again.
    MeshFields(bool),
    ExactDetail(f64),
    /// A `fab.*` tool call (`tool` is the short name).
    Call { id: u64, tool: String, args: Value },
    /// Open a project (`.fab.splash`), or an object script (`.cad.splash`) as a new project.
    Open { id: u64, path: PathBuf },
    ImportFile { id: u64, path: PathBuf, new_plate: bool },
    MakeCalibration { step: fab_calibrate::Step, setup: crate::print::CheckSetup, colour: [f32; 4] },
    Calibration { id: u64, coupon: Arc<fab_calibrate::Coupon>, colour: [f32; 4] },
    /// Save the newest settled viewport frame in the slot as the project's picture (see
    /// `ModelHost::thumbnail`).
    Thumbnail(ThumbnailSlot),
    /// Show a demo plate as it ships.
    Demo { id: u64, demo: &'static crate::demo_part::Demo },
    /// Read the plate's file again (it has no edits the file lacks).
    Reload { id: u64 },
    /// How the plate was left on a switch or quit. The bed may already hold another plate.
    SaveView { plate: String, view: PlateView },
    /// Move a plate's files and working copy to the trash (the person confirmed it).
    DeletePlate { id: u64, plate: String },
    /// Place object script files on the plate (dropped `.cad.splash` files), arranged, as one
    /// undo step.
    AddScripts { id: u64, paths: Vec<PathBuf> },
    /// Put a mesh the AI hub made of a picture on the plate, as one undo step (`id`: the
    /// call it answers; `render`: with a render of it for the caller).
    ImportMesh { id: u64, made: Box<crate::img2mesh::Made>, render: bool },
    /// Repair an imported mesh on this worker, with one undo step and a status job.
    MakePrintable { id: u64, object: String, opts: Value, render: bool },
    /// Pack the review renderer's frames into PNG sheets.
    EncodeSheets { id: u64, frames: review::Frames, made: bool },
    /// A headset is served now: prepare the current plate for it (and every one after).
    ServeXr,
    /// Move parts as one undo step (a headset grab, a desktop move).
    Place { id: u64, moves: Vec<fab_ai::PartMove> },
    /// Set a parameter as one undo step (the part panel, a shape handle, the headset).
    Param { id: u64, key: String, value: PValue, fallback: Option<(PValue, usize)>, asked: std::time::Instant },
    /// Evaluate a live parameter or source preview, committing nothing. `since` is the count
    /// of `stops` when it was asked: a stop after it ends the build.
    Preview { gen: u64, req: PreviewReq, asked: std::time::Instant, stops: Arc<PreviewStops>, since: u64 },
    /// Commit a project, script or paint file as one undo step, with one parameter's value
    /// when the text was written for it (a selector picked again on a parameter's draft).
    Source { id: u64, file: String, text: String, param: Option<(String, PValue)> },
    /// Commit structured changes of the project and its scripts as one undo step (a direct
    /// plate edit).
    Edit { id: u64, edits: Vec<fab_model::Change> },
    /// Resolve a complete deletion selection against the worker's current head.
    DeleteParts { id: u64, objects: Vec<String>, scripts: bool },
    /// Edits of the test prints, against the plate as it stands: the list's, the fields', the
    /// gizmo's, on the path of `fab.test_print`.
    TestPrints { id: u64, edits: Vec<fab_model::project::TestEdit> },
    /// A click with Lay flat on a test print's piece: the face the plate ray `from + t dir` (mm)
    /// meets goes face down, on `name`'s piece in place or, with none, the ticked ones on the bed.
    LayTest { id: u64, name: Option<String>, from: [f64; 3], dir: [f64; 3] },
    /// Merge colours/strokes against the worker's current source, including queued edits.
    PaintEdit { id: u64, action: fab::api::PartAction, frame: Option<[[f64; 4]; 3]> },
    /// Undo (or redo) one step of the document's history at its head now.
    History { id: u64, undo: bool },
    /// A field a part asked the host for, solved (a graded part's stress,
    /// [`HostState::ask_stress`]; a veined part's tubes, [`HostState::ask_veins`]): set it and
    /// build again.
    Solved(Box<Solved>),
    /// The language's host inputs and libraries (the electronics boards, `board("x")`): set
    /// them and build again; only the scripts reading a moved input recompile.
    SetEnv { bed: u64, env: ScriptEnv },
    /// The electronics host's documents for the bed it loaded (`bed` is that
    /// `Session::switched`, `plate` that bed's plate id when it had one), and the electronics
    /// versions they may replace (`bases`). Other bed files stay the model's own.
    Docs {
        bed: u64, plate: Option<String>, seq: u64, docs: Arc<fab_model::Docs>,
        assets: Arc<BTreeMap<String, Option<Arc<[u8]>>>>, bases: Vec<crate::elec_host::Version>, asked: std::time::Instant,
    },
    /// The co-simulated chips' EEPROMs as the bed `bed`'s runs left them: kept with the plate,
    /// no edit (no undo step, no content).
    Memory { bed: u64, memory: Arc<fab_model::ElecMemory> },
    /// A grown part's newest shape ([`HostState::ask_grow`]): set it and build again.
    Grown(Box<crate::grow_live::Shown>),
    /// The shape a grown part settled at: keep it, and with a file, beside the script.
    GrownSettled(Box<crate::grow_live::Settled>),
    /// The last job before the app quits ([`ModelHost::quit`]): every edit queued ahead of it has
    /// run, so its save holds what the person sees. A failed save is logged and the copy on disk
    /// stays as it was (nothing is pruned); the answer ([`HostAction::Quit`]) comes either way.
    /// `asked`: when the quit was asked, for the log line that says how long it waited.
    Quit { id: u64, asked: std::time::Instant },
}

/// Each veins request of a placed part with the printer's up in its source's frame (where the
/// part stands on the plate, `field`) and the steepest way down out of a surface the plate's
/// settings print without support. A part under a deform or instanced has no one frame, and no
/// tubes.
pub(crate) fn veins_asks(requests: &[makepad_model_script::lang::VeinsRequest], field: &fab_model::eval::PartField, settings: &[(String, String)]) -> Vec<(makepad_model_script::lang::VeinsRequest, [f64; 3], f64)> {
    use makepad_model_script::lang::csg_sdf::part_id;
    let lowest = fab_model::veins::lowest(settings);
    requests
        .iter()
        .filter_map(|r| {
            let places = match fab_optimise::grade::part_places(&field.shape.root, part_id(&r.part)) {
                Ok(p) => p,
                Err(e) => {
                    log!("fab: {}'s veins: {e}", r.part);
                    return None;
                }
            };
            let m = field.to_printer;
            let to = |p: [f64; 3]| -> [f64; 3] {
                let q = places.iter().fold(p, |q, place| place.out_of(q));
                std::array::from_fn(|r| m[r][0] * q[0] + m[r][1] * q[1] + m[r][2] * q[2] + m[r][3])
            };
            // the printer's up is the third row of the turn from the source's frame
            let o = to([0.0; 3]);
            let up: [f64; 3] = std::array::from_fn(|a| {
                let mut e = [0.0; 3];
                e[a] = 1.0;
                let d = to(e);
                (d[2] - o[2]) / ((d[0] - o[0]).powi(2) + (d[1] - o[1]).powi(2) + (d[2] - o[2]).powi(2)).sqrt()
            });
            let l = (up[0] * up[0] + up[1] * up[1] + up[2] * up[2]).sqrt();
            Some((r.clone(), up.map(|x| x / l), lowest))
        })
        .collect()
}

/// What solves a graded part's stress, and a veined part's tubes.
const STRESS_STUDY: &str = "the stress study";
const VEINS_STUDY: &str = "a slime mould's veins";

/// A field a part asked for, solved (a graded part's stress, a veined part's tubes), or none when
/// its solve failed or nothing poses it any more.
pub struct Solved {
    pub file: String,
    pub part: String,
    /// What solved it ("the stress study"), and the build it starts ("Grading x by its stress").
    pub study: &'static str,
    pub doing: String,
    /// What it was solved for ([`FabModel::ask_solved`]): an edit may have outrun it.
    pub asked: Option<u64>,
    pub grid: Option<Arc<makepad_model_script::lang::csg_sdf::SdfGrid>>,
}

/// A settled viewport frame waiting to be saved as the project's picture (`demo`: and as
/// the card of the demo on the plate).
pub struct ThumbnailFrame {
    width: u32,
    height: u32,
    rgba: makepad_widgets::HostBuf,
    demo: Option<&'static str>,
}

/// The newest frame not yet taken by the queued `Job::Thumbnail` (none: no job waits).
pub type ThumbnailSlot = Arc<Mutex<Option<ThumbnailFrame>>>;

/// What a live preview evaluates: a parameter at a dragged value (its instance rebuilt in
/// draft), the code panel's edited text of one script, or object script files dragged over
/// the window, placed as their drop would place them.
#[derive(Clone, Debug)]
pub enum PreviewReq {
    Param { key: String, value: PValue },
    Source { file: String, text: String },
    Drop { paths: Vec<PathBuf> },
}

/// [`PreviewDone::key`] of a drop's preview.
pub const DROP_PREVIEW: &str = "drop";

/// What stops a preview's build: a commit or a cancel; a newer value only when it builds no exact
/// body (a drag back to a shape built before: it shows at once). Any other newer value lets the
/// build go on and show; the values asked meanwhile come down to the newest, which starts after it.
#[derive(Default)]
pub struct PreviewStops {
    count: std::sync::atomic::AtomicU64,
    /// The last stop's commit, a parameter and its value: a build of that value goes on.
    spares: Mutex<Option<(String, PValue)>>,
    /// The newest preview asked, by generation, when it is a parameter's.
    newest: Mutex<Option<(u64, String, PValue)>>,
}

impl PreviewStops {
    /// Whether a stop came after `since` for a build of `building` (a parameter's preview; a
    /// source preview is never spared).
    fn stopped(&self, since: u64, building: Option<(&str, &PValue)>) -> bool {
        if self.count.load(Ordering::Acquire) == since {
            return false;
        }
        let spares = self.spares.lock().unwrap();
        !matches!((spares.as_ref(), building), (Some((k, v)), Some((key, value))) if k == key && v == value)
    }

    /// A parameter's value asked after the preview of generation `gen`, at another value than
    /// `building`: the newest asked, with its generation.
    fn newer(&self, gen: u64, building: (&str, &PValue)) -> Option<(u64, String, PValue)> {
        self.newest.lock().unwrap().clone().filter(|(g, key, value)| *g > gen && (key.as_str(), value) != building)
    }
}

/// A previewed plate (not committed): its scene, markers and parameters.
pub struct PreviewDone {
    pub gen: u64,
    /// The parameter's key, or "source" for the code panel's text.
    pub key: String,
    /// The parameter's value it built (none for a text).
    pub value: Option<PValue>,
    pub result: Result<PlateUpdate, String>,
    /// For a program text that does not build: the 1-based line and column it names.
    pub line: Option<(u32, u32)>,
}

/// A plate the view swaps in: the evaluation, its scene, issue markers, parameter views, and
/// the program with its parameter model (tool ghosts build from them off this thread).
pub type PlateUpdate = (Arc<Evaluated>, Arc<Scene>, RetainedInstances, Vec<Arc<PartParamsView>>, Program, Vec<fab::api::Measurement>, Vec<fab::api::TestPrintView>);

/// A program and its parameter model.
pub type Program = Option<(Arc<fab_model::program::Compiled>, Arc<Vec<fab_model::params::PartParams>>)>;

/// A finished job, posted to the UI.
pub struct Done {
    pub id: u64,
    pub ok: bool,
    pub text: String,
    /// Pictures that go with the answer (a section): (label, PNG).
    pub images: Vec<(String, Vec<u8>)>,
    /// For a render request: the plate prepared for the review renderer
    /// (taken once by the UI: actions are delivered by reference).
    pub review: Mutex<Option<Result<Review, String>>>,
    /// The rebuilt plate and its scene, when the head moved.
    pub update: Option<PlateUpdate>,
    pub status: Status,
    pub model_changed: bool,
    pub check_slice: Option<Arc<Evaluated>>,
    /// The project's files and tab order, refreshed on the worker when the plate changes.
    pub source_files: Option<Arc<fab_ai::SourceFiles>>,
    /// A code panel commit that was not taken: where and why.
    pub source_problem: Option<fab_ai::SourceProblem>,
    /// A parameter commit that did not build as asked: the value committed instead and
    /// why the asked value was refused (the raw failure text).
    pub capped: Option<(PValue, String)>,
}

/// A render request, prepared for the review renderer: what it draws and what reads the frames.
pub enum Review {
    /// One capture, answered with its sheets (fab.render, a look).
    Sheets(PreparedReview),
    /// One capture, read against a reference picture ([`compare`]).
    Compare(PreparedReview, Arc<fab_ai::compare::CompareJob>),
    /// A capture of each variant, or first the survey to fit the reference's view
    /// ([`crate::variant_sheet`]).
    Variants(Vec<PreparedReview>, Arc<fab_ai::variants::VariantJob>),
}

/// What the window shows about the document after a job.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub name: String,
    pub head: Option<Head>,
    /// The newest undo step number, for the shared undo order.
    pub undo: Option<u64>,
    /// The next redo step number, for the shared undo order.
    pub redo: Option<u64>,
    /// The program's solids that build what it prints ([`fab_model::program::printed_solids`]).
    pub solids: usize,
    /// Its marked areas: print settings volumes (`m.settings`), no-trees areas (`m.no_trees`).
    pub volumes: usize,
    pub no_trees: usize,
    /// On the bed: a plate's id, or a demo's key (true).
    pub bed: Option<(String, bool)>,
    /// Where the bed document is saved (None: not yet).
    pub path: Option<std::path::PathBuf>,
    /// Moves when a document goes on the bed ([`fab_ai::Session::switched`]).
    pub switched: u64,
    /// The view from the plate's folder, carried only by the status that puts it on the bed.
    pub saved_view: Option<PlateView>,
    /// The bed document's electronics documents and the files beside them, such as part meshes
    /// (the electronics host loads from them).
    pub docs: Arc<fab_model::Docs>,
    pub elec_assets: Arc<fab_model::ElecAssets>,
    /// Its co-simulated chips' kept EEPROMs ([`fab_model::FabModel::elec_memory`]).
    pub elec_memory: Arc<fab_model::ElecMemory>,
    /// What is picked in the view ([`fab_ai::picks`]), and its revision: the view and the chat's
    /// chips follow it, whoever picked (the person, the AI, an outside agent).
    pub picks: (u64, Arc<Vec<fab_ai::picks::Pick>>),
}

#[derive(Debug)]
pub enum HostAction {
    Ready,
    /// The quit's save ran ([`Job::Quit`] `id`): the app quits now.
    Quit { id: u64 },
    OverhangList(Box<((u64, u64), fab_printability::OverhangList)>),
    /// What a part's panel says of a grown feature.
    GrowLine(fab::params::GrowLine),
    /// A grown part's new shape is on the plate: a drag's live fields read it.
    GrownBuilt,
    /// The plate list changed (a plate made, saved, renamed, edited, switched to, its picture):
    /// the list as it is now, taken once.
    Plates(Mutex<Option<fab::api::PlateList>>),
    /// A picture painter ended ([`crate::plate_paint::start`]).
    Painted(crate::plate_paint::Painted),
    /// The bends of the plate (by content) are measured ([`crate::mech_flex::measure_bends`]).
    BendsMeasured([u8; 32]),
    Done(Box<Done>),
    /// An electronics write left the model worker (landed, or kept aside over an outside change),
    /// after every status of an earlier job: the writes before it can no longer be on the bed.
    DocsLanded { bed: u64, docs: Arc<fab_model::Docs> },
    /// The plate with its display meshes as the view shows them (exact parts' detail, the Mesh
    /// look), and its scene: what shows changes, nothing else.
    Shown(Box<(Arc<Evaluated>, Arc<Scene>)>),
    /// A job that changed the shape is over ([`SliceHold`]): when none is left the live slice
    /// goes on, on the plate it made (its `Done` or `Preview` came first), else on the plate as
    /// it was. `built`: it was a job that builds the plate ([`ModelHost::building`]).
    ShapeJobOver { built: bool },
    /// The review sheets of call `id`: (label, PNG) pairs.
    Sheets { id: u64, result: Result<review::Sheets, String>, made: Option<PathBuf> },
    /// A call's answer, come after its job left the model worker.
    Answered { id: u64, ok: bool, text: String },
    /// A call answered from what the review drew, off the model worker ([`compare`],
    /// [`crate::variant_sheet::read`]): its text and pictures (label, PNG).
    Pictured { id: u64, result: Result<(String, Vec<(String, Vec<u8>)>), String> },
    /// `fab.variants` found the reference's view: the fit and the variants to draw from it
    /// (taken once).
    VariantsFitted { id: u64, round: Mutex<Option<(fab_ai::compare::Fit, Vec<PreparedReview>)>> },
    /// The plate prepared for the headset's view (taken once).
    XrPlate(Mutex<Option<crate::xr::PreparedPlate>>),
    /// A live preview of the code panel's text.
    Preview(Box<PreviewDone>),
    /// A field a part asked for was solved: queue it for the model ([`ModelHost::solved`]).
    Solved(Mutex<Option<Box<Solved>>>),
    /// A grown part has a new shape: queue it for the model ([`ModelHost::grown`]).
    Grown(Box<crate::grow_live::Shown>),
    /// A grown part settled: queue what to keep for the model ([`ModelHost::grown_settled`]).
    GrownSettled(Box<crate::grow_live::Settled>),
}

impl std::fmt::Debug for PreviewDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewDone").field("gen", &self.gen).field("key", &self.key).field("ok", &self.result.is_ok()).finish()
    }
}

impl std::fmt::Debug for Solved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Solved").field("file", &self.file).field("part", &self.part).finish()
    }
}

impl std::fmt::Debug for Done {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Done").field("id", &self.id).field("ok", &self.ok).field("capped", &self.capped).finish()
    }
}

/// A job on its way to the worker, with the job strip's job for it (made when it was
/// queued, so the wait for the worker shows too and a cancel before it starts is instant).
type Queued = (Job, Option<JobHandle>);

pub struct ModelHost {
    /// Numbers come from this shared counter; a reservation takes one early.
    steps: Arc<AtomicU64>,
    /// The newest entry logged by either history.
    logged: Arc<AtomicU64>,
    /// The grown parts' runs, for what a drag asks of them between builds.
    pub grow_runs: Arc<crate::grow_live::Runs>,
    strand: Strand<HostState>,
    words: Arc<Mutex<WorkerWords>>,
    setup_version: Version,
    overhang_list_version: Version,
    /// Jobs a failure can be retried for: what to queue again.
    retry: HashMap<JobId, Job>,
    /// Tool calls' jobs, by call id (the chat's progress reads them).
    call_jobs: HashMap<u64, JobId>,
    /// The jobs that build the model (open, edit, rebuild, refine), oldest first: the
    /// viewport shows the running one's stage over the part ("Rebuilding… fillets 3/7").
    build_jobs: Vec<JobId>,
    next_id: u64,
    xr: bool,
    /// Previews queued with an older ticket never start: the newest asked is the one that runs.
    preview_version: Version,
    preview_stops: Arc<PreviewStops>,
    preview_gen: u64,
    thumbnail: ThumbnailSlot,
    /// The jobs queued or running that change the shape ([`SliceHold`]).
    shape_held: Arc<AtomicUsize>,
    /// The jobs that build the plate, queued or running or whose end the app has not taken yet
    /// ([`Self::building`]).
    building: usize,
    /// The newest load asked for ([`Load`]); a newer one cancels it.
    load: Option<Load>,
    /// The build of what an earlier run left on the bed, while it may still run: the first load
    /// asked for stops it, as it stops an older load (the plate listed before it was built,
    /// [`HostState::init`], is opened in its place).
    recovery: Option<JobId>,
    /// A quit is on its way ([`Self::quit`]): the queued work nobody waits on is left
    /// ([`left_at_quit`]), and no picture starts.
    quitting: Arc<AtomicBool>,
    /// The jobs queued or running that keep something the person made or asked for: all but
    /// those a quit leaves ([`left_at_quit`], [`stale_at_quit`]). A quit with none waits for no
    /// job once the plate is saved ([`Self::saved`]).
    keeping: Arc<AtomicUsize>,
    /// The plate's working copy holds every edit the worker has taken, as of the end of its last
    /// job ([`HostState::note_saved`]): false until then, and while a failed save waits to retry.
    saved: Arc<AtomicBool>,
    persist: bool,
    /// Where a quit that waits for no job writes the plate's view.
    pool: TaskPool,
    /// The newest queued electronics documents, by the bed they were edited for.
    docs_newest: Arc<Mutex<HashMap<u64, u64>>>,
    docs_seq: u64,
    /// Document replacements waiting for the electronics host's last documents.
    held: Vec<(Job, Option<String>)>,
    barrier: bool,
    replacing: Arc<AtomicUsize>,
    switched_now: Arc<AtomicU64>,
}

fn post_xr_plate(ev: &Arc<Evaluated>, job: u64) {
    // Publish empty plates too: clearing or undoing the last part must clear
    // the headset's model, while its bed stays at the same room anchor.
    let mut parts = Vec::with_capacity(ev.plate.parts.len());
    for (i, (part, rm)) in ev.plate.parts.iter().zip(&ev.render).enumerate() {
        let prepared = |model, links| crate::xr::PreparedPart { model, links, object: part.object.clone(), bounds: part.bounds() };
        // Keep a separate link mesh only for the mechanics sim; the plate draws its field.
        if let Some(w) = ev.weaves.iter().find(|w| w.part == i) {
            let slots: Vec<[f32; 4]> = ev.paint.get(i).and_then(|p| p.as_ref()).map_or(Vec::new(), |p| p.colors.iter().map(|c| [c.rgb[0], c.rgb[1], c.rgb[2], 1.0]).collect());
            match review::prepare_links(part, w, &slots) {
                Ok(links) => parts.push(prepared(None, Some(links))),
                Err(e) => {
                    log!("fab xr: the links of '{}' could not be prepared: {e}", part.object);
                    parts.push(prepared(None, None));
                }
            }
            continue;
        }
        // A field part has no mesh: the headset draws its tiles.
        if rm.indices.is_empty() {
            parts.push(prepared(None, None));
            continue;
        }
        match review::prepare_part(ev, i, part, rm) {
            Ok(model) => parts.push(prepared(Some(model), None)),
            Err(e) => {
                log!("fab xr: part '{}' could not be prepared: {e}", part.object);
                parts.push(prepared(None, None));
            }
        }
    }
    for (k, excluded) in ev.plate.excluded.iter().enumerate() {
        // A field part has no mesh, its eye on or off.
        let model = if excluded.render.indices.is_empty() { None } else {
            review::prepare_part(ev, ev.plate.parts.len() + k, &excluded.part, &excluded.render)
                .map_err(|e| log!("fab xr: excluded part '{}' could not be prepared: {e}", excluded.part.object)).ok()
        };
        parts.push(crate::xr::PreparedPart { model, links: None, object: excluded.part.object.clone(), bounds: excluded.part.bounds() });
    }
    let ball = review::prepare_ball().map_err(|e| log!("fab xr: the drape ball could not be prepared: {e}")).ok();
    Cx::post_action(HostAction::XrPlate(Mutex::new(Some(crate::xr::PreparedPlate { parts, bed: ev.machine.bed, ev: ev.clone(), ball, job }))));
}

/// Where new projects and exports go: `~/Documents/Fab`.
pub fn documents_dir() -> PathBuf {
    let home = makepad_fab_settings::home::user_home();
    home.join("Documents").join("Fab")
}

/// The plate store: records and working copies in `~/.makepad/fab/plates`, saved plates'
/// folders in `~/Documents/Fab`, the demos' pictures in `~/.makepad/fab/demos`.
pub fn plate_store() -> fab_ai::plates::PlateStore {
    let home = makepad_fab_settings::home::user_home();
    fab_ai::plates::PlateStore::under(&home.join(".makepad").join("fab"), &documents_dir())
}

/// The worker's state as the app follows it. The picks follow the build first (each described
/// again on it): once per build, one description per pick.
fn status(s: &mut Session) -> Status {
    status_after_job(s.switched, s)
}

fn status_after_job(previous_switch: u64, s: &mut Session) -> Status {
    s.picks_follow_build();
    let count = |f: &dyn Fn(&makepad_model_script::lang::Program) -> usize| -> usize {
        s.compiled.as_ref().map_or(0, |c| c.instances.iter().map(|i| f(&i.script.program)).sum())
    };
    Status {
        name: s.model.name.clone(),
        head: Some(s.model.head()),
        undo: s.model.undo_top(),
        redo: s.model.redo_top(),
        solids: count(&fab_model::program::printed_solids),
        volumes: count(&|p| fab_model::program::marked_areas(p, "settings")),
        no_trees: count(&|p| fab_model::program::marked_areas(p, "no_trees")),
        bed: s.bed(),
        path: s.path.clone(),
        switched: s.switched,
        saved_view: match (&s.store, &s.plate) {
            (Some(store), Some(id)) if s.switched != previous_switch => {
                std::fs::read_to_string(store.view_file(id)).ok().and_then(|text| PlateView::from_file_text(&text))
            }
            _ => None,
        },
        docs: s.model.docs().clone(),
        elec_assets: s.model.elec_assets().clone(),
        elec_memory: s.model.elec_memory().clone(),
        picks: (s.picks.rev, Arc::new(s.picks.list.clone())),
    }
}

/// A grown part's shape, or none: built, and the plate told. Only a level done (`settled`) asks for
/// a slice, though it brought no new shape.
fn show_grown(session: &mut Session, scenes: &mut Scenes, overhangs: bool, file: &str, part: &str, grid: Option<Arc<makepad_model_script::lang::csg_sdf::SdfGrid>>, settled: bool, faded: bool) {
    let changed = session.model.set_grown(file, part, grid);
    if (!changed && !settled) || session.last.is_none() {
        return;
    }
    let started = std::time::Instant::now();
    if changed {
        if let Err(e) = session.rebuild() {
            log!("fab: growing {part}: {e}");
            return;
        }
    }
    if let Some(ev) = session.last.clone() {
        let scene = scenes.of(session, &ev, overhangs);
        // said before the view can have it: what the view says next is of this shape
        if faded {
            log!("fab: {part} faded in");
        }
        Cx::post_action(HostAction::Done(Box::new(Done { images: Vec::new(), id: 0, ok: true, text: String::new(), review: Mutex::new(None), update: Some(plate_update(session, ev, scene)), status: status(session), model_changed: settled, check_slice: None, source_files: None, source_problem: None, capped: None })));
        Cx::post_action(HostAction::GrownBuilt);
    }
    log!("fab: {part} shape built in {:.1} ms", started.elapsed().as_secs_f64() * 1e3);
}

/// What the analysis scene reads of a build, in order: display meshes and overhang colours are
/// in it, as the evaluated content stamp alone does not identify a view mesh. Its bytes, floats
/// as their bits, make the content key ([`scene_content`]).
enum SceneInput<'a> {
    Bytes(std::borrow::Cow<'a, [u8]>),
    Floats(&'a [f32]),
    Words(&'a [u32]),
}

fn scene_inputs(ev: &Evaluated) -> Vec<SceneInput<'_>> {
    use std::borrow::Cow;
    use SceneInput::*;
    let text = |t: String| Bytes(Cow::Owned(t.into_bytes()));
    let count = |n: usize| Bytes(Cow::Owned((n as u64).to_le_bytes().to_vec()));
    let mut inputs = vec![text(format!("{:?}", ev.machine)), count(ev.plate.parts.len())];
    for (i, (part, mesh)) in ev.plate.parts.iter().zip(&ev.render).enumerate() {
        inputs.push(text(format!("{:?}", (&part.name, &part.object, part.color, part.field_bounds, ev.part_color(i), ev.reports.get(i).is_some_and(|r| r.blocking())))));
        mesh_inputs(&mut inputs, mesh);
        match ev.shown_overhang(i) {
            Some(degrees) => inputs.extend([Bytes(Cow::Borrowed(&[1])), count(degrees.len()), Bytes(degrees)]),
            None => inputs.push(Bytes(Cow::Borrowed(&[0]))),
        }
    }
    inputs.push(count(ev.plate.excluded.len()));
    for excluded in &ev.plate.excluded {
        let part = &excluded.part;
        inputs.push(text(format!("{:?}", (&part.name, &part.object, part.color, part.field_bounds))));
        mesh_inputs(&mut inputs, &excluded.render);
    }
    inputs
}

/// A mesh's arrays, each after its length.
fn mesh_inputs<'a>(inputs: &mut Vec<SceneInput<'a>>, mesh: &'a fab_plate::RenderMesh) {
    let count = |n: usize| SceneInput::Bytes(std::borrow::Cow::Owned((n as u64).to_le_bytes().to_vec()));
    for values in [&mesh.positions, &mesh.normals] {
        inputs.extend([count(values.len()), SceneInput::Floats(values.as_flattened())]);
    }
    inputs.extend([count(mesh.indices.len()), SceneInput::Words(&mesh.indices)]);
}

/// The content key of a build's scene inputs. Only the digest survives a build; no geometry is
/// copied into a second cache.
fn scene_content(ev: &Evaluated) -> [u8; 32] {
    hash_inputs(scene_inputs(ev))
}

fn hash_inputs(inputs: Vec<SceneInput>) -> [u8; 32] {
    let mut h = makepad_core_util::sha256::Sha256::new();
    for input in inputs {
        match input {
            SceneInput::Bytes(bytes) => h.update(&bytes),
            SceneInput::Floats(values) => hash_le(&mut h, values, |v: f32| v.to_bits().to_le_bytes()),
            SceneInput::Words(values) => hash_le(&mut h, values, u32::to_le_bytes),
        }
    }
    h.finish()
}

/// Values' little-endian bytes to a hash a block at a time: the bytes and so the digest are
/// those of one `update` per value.
fn hash_le<T: Copy>(h: &mut makepad_core_util::sha256::Sha256, values: &[T], le: impl Fn(T) -> [u8; 4]) {
    let mut block = [0u8; 4096];
    for chunk in values.chunks(block.len() / 4) {
        for (bytes, &v) in block.chunks_exact_mut(4).zip(chunk) {
            bytes.copy_from_slice(&le(v));
        }
        h.update(&block[..chunk.len() * 4]);
    }
}

/// Whether two builds' scene inputs are the same bytes, so their content keys are the same.
fn same_inputs(a: &[SceneInput], b: &[SceneInput]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| match (a, b) {
        (SceneInput::Bytes(a), SceneInput::Bytes(b)) => a == b,
        (SceneInput::Floats(a), SceneInput::Floats(b)) => a.len() == b.len() && a.iter().zip(*b).all(|(a, b)| a.to_bits() == b.to_bits()),
        (SceneInput::Words(a), SceneInput::Words(b)) => a == b,
        _ => false,
    })
}

struct KeptScene {
    build: std::sync::Weak<Evaluated>,
    /// The content key of `build` ([`scene_content`]).
    content: [u8; 32],
    name: String,
    overhangs: bool,
    zones: bool,
    scene: Arc<Scene>,
}

/// Scenes live with the session's retained builds. A new allocation with identical scene
/// inputs adopts the existing scene; both overhang variants stay with that build.
#[derive(Default)]
struct Scenes {
    kept: Vec<KeptScene>,
    /// The pool a scene's build spreads over, its content key made beside it; none: all on the
    /// worker, one after the other.
    pool: Option<TaskPool>,
}

impl Scenes {
    fn on(pool: TaskPool) -> Self {
        Self { kept: Vec::new(), pool: Some(pool) }
    }

    fn of(&mut self, session: &Session, ev: &Arc<Evaluated>, overhangs: bool) -> Arc<Scene> {
        self.of_with(session, ev, overhangs, &mut |_, _| {})
    }

    /// [`Self::of`], saying how a scene it builds goes ([`plate_scene::build_analysis_reusing`]).
    fn of_with(&mut self, session: &Session, ev: &Arc<Evaluated>, overhangs: bool, progress: &mut dyn FnMut(&'static str, f32)) -> Arc<Scene> {
        let name = &session.model.name;
        let zones = plate_scene::SHOW_ZONES.load(Ordering::Relaxed);
        let is = |w: &std::sync::Weak<Evaluated>, ev: &Arc<Evaluated>| std::ptr::eq(w.as_ptr(), Arc::as_ptr(ev));
        // The usual switch/repaint needs neither geometry hashing nor geometry copying, nor does
        // a new allocation of the inputs of a kept build still held (print settings, a colour
        // mapping). A key only a later build can match is made beside this one.
        let mut content = self.kept.iter().find(|k| is(&k.build, ev)).map(|k| k.content).or_else(|| self.held_content(name, ev));
        if content.is_none() && self.kept.iter().any(|k| k.name == *name) {
            content = Some(scene_content(ev));
        }
        let kept = content.and_then(|content| self.kept.iter().find(|k| k.content == content && k.name == *name && k.overhangs == overhangs && k.zones == zones)).map(|k| k.scene.clone());
        let previous = self.kept.iter().rev().find(|k| k.name == *name && k.overhangs == overhangs && k.zones == zones).map(|k| k.scene.as_ref());
        let pool = self.pool.as_ref();
        let build = |progress: &mut dyn FnMut(&'static str, f32)| plate_scene::build_analysis_reusing(ev, name, overhangs, previous, pool, progress);
        let (scene, content) = match (kept, content) {
            (Some(scene), Some(content)) => (scene, content),
            (None, Some(content)) => (build(progress), content),
            (_, None) => match pool {
                Some(pool) => pool.scope(|s| {
                    let key = s.spawn(|| scene_content(ev));
                    let scene = build(progress);
                    (scene, key.join().unwrap_or_else(|_| scene_content(ev)))
                }),
                None => (build(progress), scene_content(ev)),
            },
        };
        let held = session.builds().any(|b| Arc::ptr_eq(b, ev));
        if held {
            for k in &mut self.kept {
                // Migrate an allocation replaced in place, but keep aliases still owned by
                // another retained plate. Editing one must not evict the other's scene.
                if k.content == content && k.name == *name && !session.builds().any(|b| is(&k.build, b)) {
                    k.build = Arc::downgrade(ev);
                }
            }
        }
        self.kept.retain(|k| (!is(&k.build, ev) || k.name == *name) && session.builds().any(|b| is(&k.build, b)));
        if held && !self.kept.iter().any(|k| is(&k.build, ev) && k.name == *name && k.overhangs == overhangs && k.zones == zones) {
            self.kept.push(KeptScene { build: Arc::downgrade(ev), content, name: name.clone(), overhangs, zones, scene: scene.clone() });
        }
        scene
    }

    /// The content key of a kept build of this plate, still held somewhere, whose scene inputs
    /// are `ev`'s, value for value.
    fn held_content(&self, name: &str, ev: &Evaluated) -> Option<[u8; 32]> {
        let mut inputs = None;
        let mut tried: Vec<*const Evaluated> = Vec::new();
        self.kept.iter().filter(|k| k.name == name).find_map(|k| {
            let old = k.build.upgrade()?;
            if tried.contains(&Arc::as_ptr(&old)) {
                return None;
            }
            tried.push(Arc::as_ptr(&old));
            let inputs = inputs.get_or_insert_with(|| scene_inputs(ev));
            same_inputs(&scene_inputs(&old), inputs).then_some(k.content)
        })
    }
}

fn plate_update(session: &mut Session, ev: Arc<Evaluated>, scene: Arc<Scene>) -> PlateUpdate {
    let markers = fab::print::issue_markers(&ev.printability);
    let (params, program) = match session.params() {
        Ok(model) => (crate::param_view::views(&model, &ev), session.compiled.clone().map(|c| (c, model))),
        Err(_) => (Vec::new(), None),
    };
    let measurements = project_measurements(session);
    let test_prints = project_test_prints(session, &ev);
    (ev, scene, markers, params, program, measurements, test_prints)
}

/// The project's test prints where they stand on the plate `ev`, as the views show them.
fn project_test_prints(session: &Session, ev: &Evaluated) -> Vec<fab::api::TestPrintView> {
    session.model.project().test_prints.iter().filter_map(|t| {
        let placed = ev.test_print_box(t)?;
        Some(fab::api::TestPrintView { name: t.name.clone(), object: t.part(), center: placed.center, size: placed.size, turn: placed.turn, pose: t.pose, lay: ev.test_print_lay(t), on: t.on, brim: t.brim })
    }).collect()
}

/// The plate scene is Z-up metres, exactly the project's plate axes scaled by 1/1000.
/// Face rulers keep two extra points: the tips of unit-mm normals at their two anchors.
/// This retains both a parallel gap and a non-parallel angle using the model's point schema.
pub fn measurement_to_model(m: &fab::api::Measurement) -> fab_model::project::Measure {
    let mut points: Vec<[f64; 3]> = m.points.iter().map(|p| [p.x as f64 * 1000.0, p.y as f64 * 1000.0, p.z as f64 * 1000.0]).collect();
    if m.kind == fab::api::MeasureKind::FaceDistance && points.len() == 2 && m.normals.len() >= 2 {
        for i in 0..2 {
            let normal = m.normals[i].normalize();
            let p = points[i];
            points.push([p[0] + normal.x as f64, p[1] + normal.y as f64, p[2] + normal.z as f64]);
        }
    }
    fab_model::project::Measure { name: m.name.clone(), kind: fab::tools::measure::kind_name(m.kind).into(), points }
}

pub fn measurement_from_model(m: &fab_model::project::Measure) -> fab::api::Measurement {
    use fab::tools::measure;
    let kind = measure::parse_kind(&m.kind);
    let mut points: Vec<_> = m.points.iter().map(|p| measure::world(p.map(|v| v * 0.001))).collect();
    let mut normals = Vec::new();
    if kind == Some(fab::api::MeasureKind::FaceDistance) && points.len() >= 2 {
        if m.points.len() == 4 {
            for i in 0..2 {
                normals.push(measure::world(std::array::from_fn(|j| m.points[i + 2][j] - m.points[i][j])).normalize());
            }
            points.truncate(2);
        } else {
            // Two-point rulers from fab.measure describe the gap along their connecting line.
            let direction = points[1] - points[0];
            let normal = if direction.length() > 1e-9 { direction.normalize() } else { vec3(0.0, 0.0, 1.0) };
            normals = vec![normal; 2];
        }
    }
    let units = fab::api::Units::millimeters();
    let mut result = kind.and_then(|kind| measure::commit_with_normals(kind, points.clone(), normals.clone(), &units))
        // Keep a row for every model index, including unknown kinds and incomplete AI rulers.
        .unwrap_or_else(|| fab::api::Measurement {
            name: None, kind: kind.unwrap_or_default(), points, normals,
            value: f64::NAN, label: format!("{}: invalid or incomplete measurement", m.kind), circle: None, gap: None,
        });
    result.name = m.name.clone();
    result
}

fn project_measurements(session: &Session) -> Vec<fab::api::Measurement> {
    session.model.project().measures.iter().map(measurement_from_model).collect()
}

/// Tree and viewport edits use the same single-change transaction as fab.measure.
pub fn measurement_action(action: &fab::api::MeasureAction) -> Option<fab_model::Change> {
    use fab::api::MeasureAction;
    Some(match action {
        MeasureAction::Select(_) => return None,
        MeasureAction::Delete(index) => fab_model::Change::MeasureRemove { index: *index },
        MeasureAction::Rename(index, name) => fab_model::Change::MeasureSet {
            index: *index, points: None, name: Some((!name.is_empty()).then(|| name.clone())),
        },
    })
}

/// What the model worker is on, in words, and the job strip's jobs queued behind it, which say
/// what they wait for.
#[derive(Default)]
struct WorkerWords {
    doing: Option<String>,
    queued: Vec<jobs::JobReporter>,
}

impl WorkerWords {
    fn waiting(&self) -> String {
        match &self.doing {
            Some(what) => format!("waiting for {what}"),
            // Nothing runs on the worker: its next turn waits for a thread of the pool.
            None => "waiting for a free worker thread".to_string(),
        }
    }

    /// The worker went on to `doing`; `started` is the job that left the queue for it.
    fn set(&mut self, doing: Option<String>, started: Option<JobId>) {
        self.doing = doing;
        if let Some(id) = started {
            self.queued.retain(|r| r.id() != id);
        }
        let words = self.waiting();
        for r in &self.queued {
            r.stage(words.clone(), None, None);
        }
    }
}

/// What the worker is on in words, whether the job strip shows that job or not.
fn job_words(job: &Job) -> String {
    match job {
        Job::MakeCalibration { .. } => "the test part".to_string(),
        Job::MeshFields(false) => "meshing the field parts".to_string(),
        Job::Thumbnail(_) => "a thumbnail".to_string(),
        Job::Painted(_) => "the plates' pictures".to_string(),
        Job::PrintSetup(_) => "the print setup".to_string(),
        Job::Mapped(_) => "the colour mapping".to_string(),
        Job::PatternsGrown => "the grown patterns".to_string(),
        Job::BendsMeasured(_) => "the bonds' bends".to_string(),
        Job::Overhangs(_) => "the overhang view".to_string(),
        Job::TestMode(_) => "the test prints' choice".to_string(),
        Job::OverhangList { .. } => "the overhang list".to_string(),
        Job::EncodeSheets { .. } => "the review sheets".to_string(),
        Job::ServeXr => "the headset link".to_string(),
        Job::Preview { .. } => "the live preview".to_string(),
        Job::Solved(s) => s.study.to_string(),
        Job::SetEnv { .. } => "the electronics boards".to_string(),
        Job::Docs { .. } => "the electronics documents".to_string(),
        Job::Memory { .. } => "the chips' EEPROMs".to_string(),
        Job::Grown(_) | Job::GrownSettled(_) => "a grown part".to_string(),
        job => {
            let title = job_title(job).unwrap_or_default();
            let mut c = title.chars();
            c.next().map_or(String::new(), |first| first.to_lowercase().chain(c).collect())
        }
    }
}

/// What the job strip calls a worker job; none for work that never takes long enough
/// to show (or shows its own progress elsewhere).
fn job_title(job: &Job) -> Option<String> {
    let file = |p: &std::path::Path| p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().to_string());
    Some(match job {
        Job::ImportFile { path, .. } => format!("Importing {}", file(path)),
        Job::Calibration { coupon, .. } => format!("Placing {}", coupon.name),
        Job::Open { path, .. } => format!("Opening {}", file(path)),
        Job::Demo { demo, .. } => format!("Opening {}", demo.title),
        Job::Reload { .. } => "Reading the plate's file".to_string(),
        Job::DeletePlate { .. } => "Moving the plate to the trash".to_string(),
        Job::AddScripts { paths, .. } => match paths.as_slice() {
            [path] => format!("Placing {}", file(path)),
            more => format!("Placing {} files", more.len()),
        },
        Job::Call { tool, .. } => tool_title(tool),
        Job::ImportMesh { made, .. } => format!("Placing {}", made.name),
        Job::MakePrintable { .. } => "Making it printable".to_string(),
        Job::Param { .. } | Job::Source { .. } | Job::Edit { .. } | Job::DeleteParts { .. } | Job::PaintEdit { .. } | Job::Place { .. } => "Rebuilding the plate".to_string(),
        Job::TestPrints { .. } | Job::LayTest { .. } => "Updating the test prints".to_string(),
        Job::History { undo, .. } => if *undo { "Undoing" } else { "Redoing" }.to_string(),
        Job::Quit { .. } => "Saving the plate before quitting".to_string(),
        Job::SaveView { .. } => return None,
        Job::MeshFields(true) => "Meshing the field parts".to_string(),
        Job::ExactDetail(_) => "Refining the exact parts".to_string(),
        Job::MakeCalibration { .. } | Job::MeshFields(false) | Job::Thumbnail(_) | Job::Painted(_) | Job::PrintSetup(_) | Job::Mapped(_) | Job::PatternsGrown | Job::BendsMeasured(_) | Job::Overhangs(_) | Job::TestMode(_) | Job::OverhangList { .. } | Job::EncodeSheets { .. } | Job::ServeXr | Job::Preview { .. } | Job::Solved(_) | Job::SetEnv { .. } | Job::Docs { .. } | Job::Memory { .. } | Job::Grown(_) | Job::GrownSettled(_) => return None,
    })
}

/// How much of a load's job its build is (the rest: the view made of it), for its bar.
const LOAD_BUILT: f32 = 0.5;

/// A scene build's stage in a person's words.
fn scene_words(stage: &str) -> &'static str {
    match stage {
        "materials" | "elements" | "batches" => "its parts",
        "geometry" => "their surfaces",
        "overlap" => "where they touch",
        "bvh" => "what the pointer is over",
        _ => "",
    }
}

/// What a load puts on the bed, for the view's progress card to name.
#[derive(Clone)]
pub enum LoadOf {
    /// A plate on the list, by id.
    Plate(String),
    /// A demo, as it ships.
    Demo(&'static crate::demo_part::Demo),
    /// A file opened.
    File(PathBuf),
}

/// Another document on its way to the bed: the call that asked for it and its job, whose steps
/// and cancel the view's progress card shows (the job strip's line leaves it out).
#[derive(Clone)]
pub struct Load {
    pub call: u64,
    pub job: JobId,
    pub of: LoadOf,
}

/// The document a job puts on the bed in place of the one there, if it is one that does.
fn loads(job: &Job) -> Option<LoadOf> {
    match job {
        Job::Open { path, .. } => Some(LoadOf::File(path.clone())),
        Job::Demo { demo, .. } => Some(LoadOf::Demo(demo)),
        Job::Call { tool, args, .. } if tool == "plates" && args.get("action").and_then(Value::as_str) == Some("open") => {
            match (args.get("demo").and_then(Value::as_str), args.get("id").and_then(Value::as_str)) {
                (Some(key), _) => crate::demo_part::demo(key).map(LoadOf::Demo),
                (None, Some(id)) => Some(LoadOf::Plate(id.to_string())),
                (None, None) => None,
            }
        }
        _ => None,
    }
}

/// Whether a job can put another document on the bed, or finish saving it at quit.
fn replaces_document(job: &Job) -> bool {
    match job {
        Job::Open { .. } | Job::Demo { .. } | Job::Reload { .. } | Job::DeletePlate { .. }
        | Job::Calibration { .. } | Job::Quit { .. } | Job::ImportFile { new_plate: true, .. } => true,
        Job::Call { tool, args, .. } => crate::switches_bed(tool, args),
        _ => false,
    }
}

/// A hold on plate loads, for a test or an agent that acts on a load still on its way, whatever
/// the plate: with `hold-loads on` (the app's remote command) each load's build stops on its
/// worker once it has made its first step, its bar past 0 at the next one's start, until
/// `hold-loads off`, its cancel (a newer load, Cancel on its card) or a quit lets it go. The hold
/// is each load's own: the load hands its build a gate for its job alone
/// ([`fab_model::eval::BuildGate`]), so no other build (another job's, a preview's) stops at it.
/// Each answer says whether it is on and which loads wait at it, with their bars' fractions and
/// steps, and the loads it held since it went on: the step each held at, the step it is in now and
/// how it ended (a load its cancel lets go of ends in the step it held at). The worker waits on a
/// condition the command and the cancel signal; the UI thread only sets the flag and wakes it.
mod load_hold {
    use makepad_strict_json::{obj, s, Value};
    use makepad_widgets::jobs::{self, JobId, JobReporter};
    use makepad_widgets::Cx;
    use std::sync::{Condvar, Mutex};

    pub struct LoadHold {
        state: Mutex<State>,
        let_go: Condvar,
    }

    struct State {
        on: bool,
        /// The loads stopped at it now, by their jobs.
        waiting: Vec<JobId>,
        /// The loads it held since it went on, each with its step's words at the hold.
        held: Vec<(JobId, String)>,
    }

    /// The app's, which `hold-loads` sets.
    pub static HOLD: LoadHold = LoadHold::new();

    impl LoadHold {
        pub const fn new() -> LoadHold {
            LoadHold { state: Mutex::new(State { on: false, waiting: Vec::new(), held: Vec::new() }), let_go: Condvar::new() }
        }

        /// Puts the hold on (its loads held so far forgotten), or takes it off and lets every
        /// waiting load go.
        pub fn set(&self, on: bool) {
            let mut state = self.state.lock().unwrap();
            if on && !state.on {
                state.held.clear();
            }
            state.on = on;
            drop(state);
            self.let_go.notify_all();
        }

        /// A load's cancel hook: the loads at the hold look again. (The lock first: a worker
        /// between its look and its wait has the lock, so it waits before this wakes it.)
        pub fn wake(&self) {
            drop(self.state.lock().unwrap());
            self.let_go.notify_all();
        }

        /// Whether it is on, the loads waiting at it, and the loads it held since it went on.
        pub fn waiting(&self) -> (bool, Vec<JobId>, Vec<(JobId, String)>) {
            let state = self.state.lock().unwrap();
            (state.on, state.waiting.clone(), state.held.clone())
        }

        /// The load `job`'s build, at its gate: it waits here while the hold is on and the load
        /// is not cancelled.
        pub fn wait(&self, job: &JobReporter) {
            // (the board's lock before the hold's, never inside it)
            let step = jobs::board().get(job.id()).map(|j| j.stage_words()).unwrap_or_default();
            let mut state = self.state.lock().unwrap();
            if !state.on || job.is_cancelled() {
                return;
            }
            state.held.push((job.id(), step));
            state.waiting.push(job.id());
            while state.on && !job.is_cancelled() {
                state = self.let_go.wait(state).unwrap();
            }
            state.waiting.retain(|id| *id != job.id());
        }
    }

    /// Registers `hold-loads`, for the process.
    pub fn register() {
        makepad_widgets::makepad_platform::remote::register_app_command(
            "hold-loads",
            "[on|off]",
            "with on, each plate load's build stops once it has made its first step (its bar past 0) until off or its cancel; answers {hold, waiting: [{job, at: its bar's fraction, stage}] for the loads stopped at it, held: [{job, title, held_in: its step at the hold, now_in: its step now, end: running|done|failed|cancelled}] for those it held since it went on}",
            |_| "",
            command,
        );
    }

    fn command(_cx: &mut Cx, words: &[String], _kv: &[(String, String)]) -> Vec<String> {
        match words.get(1).map(String::as_str) {
            Some("on") => HOLD.set(true),
            Some("off") => HOLD.set(false),
            None => {}
            Some(other) => return vec![format!("{{\"err\":\"hold-loads takes on or off, not {other}\"}}")],
        }
        let (on, waiting, held) = HOLD.waiting();
        let waiting = waiting.into_iter().map(|id| {
            let snap = jobs::board().get(id);
            obj(vec![
                ("job", Value::Int(id.0 as i64)),
                ("at", snap.as_ref().and_then(|j| j.fraction).map_or(Value::Null, |f| Value::F64(f as f64))),
                ("stage", s(snap.map(|j| j.stage_words()).unwrap_or_default())),
            ])
        });
        let held = held.into_iter().map(|(id, held_in)| {
            let snap = jobs::board().get(id);
            let end = match snap.as_ref().map(|j| &j.end) {
                None => "gone",
                Some(jobs::JobEnd::Running) => "running",
                Some(jobs::JobEnd::Done { .. }) => "done",
                Some(jobs::JobEnd::Failed { .. }) => "failed",
                Some(jobs::JobEnd::Cancelled) => "cancelled",
            };
            obj(vec![
                ("job", Value::Int(id.0 as i64)),
                ("title", s(snap.as_ref().map(|j| j.title.clone()).unwrap_or_default())),
                ("held_in", s(held_in)),
                ("now_in", s(snap.map(|j| j.stage_words()).unwrap_or_default())),
                ("end", s(end.to_string())),
            ])
        });
        vec![obj(vec![("hold", Value::Bool(on)), ("waiting", Value::Arr(waiting.collect())), ("held", Value::Arr(held.collect()))]).to_json()]
    }
}

/// Whether a job builds (or rebuilds) the model on the plate, for the viewport's progress.
fn builds_model(job: &Job) -> bool {
    match job {
        Job::SetEnv { .. } => true,
        Job::ImportFile { .. } | Job::Calibration { .. } | Job::Open { .. } | Job::Demo { .. } | Job::Reload { .. } | Job::DeletePlate { .. } | Job::AddScripts { .. } | Job::ImportMesh { .. } | Job::MakePrintable { .. } | Job::Param { .. } | Job::Source { .. } | Job::Edit { .. } | Job::DeleteParts { .. } | Job::PaintEdit { .. } | Job::Place { .. } | Job::History { .. } => true,
        Job::Call { tool, .. } => matches!(tool.as_str(), "apply" | "script" | "exact" | "edges" | "weave" | "place" | "lay_flat" | "new" | "plates"),
        _ => false,
    }
}

/// Whether a job changes what the plate slices from: one that builds the model, a live value's
/// preview, or the colour mapping (the tools the parts print with: its plate's input goes out
/// with its `Done`, and until then the old mapping's slice is not resumed).
fn shapes_model(job: &Job) -> bool {
    builds_model(job) || matches!(job, Job::Preview { .. } | Job::Mapped(_))
}

/// What the pool runs a job at. A job that is not one of the background's is what a person (or
/// their AI) asked for, or what comes straight of it: it ranks above the tile tracer and the live
/// slice, which leave at their next item. At their class it got a worker only when they were over
/// their share: a resize sat at "waiting for a free worker" for minutes. What a study, a grow or a
/// pattern sends back when it is ready has nobody waiting on it: it goes behind all of that.
fn class_of(job: &Job) -> Class {
    match job {
        Job::Solved(_) | Job::SetEnv { .. } | Job::Docs { .. } | Job::Memory { .. } | Job::Grown(_) | Job::GrownSettled(_) | Job::PatternsGrown => Class::Visible,
        Job::MeshFields(false) | Job::ExactDetail(_) | Job::Thumbnail(_) | Job::Painted(_)
        | Job::EncodeSheets { .. } | Job::MakeCalibration { .. } | Job::PrintSetup(_)
        | Job::Mapped(_) | Job::BendsMeasured(_) | Job::Overhangs(_) | Job::TestMode(_) | Job::OverhangList { .. } => Class::Background,
        _ => Class::Interactive,
    }
}

/// Work a quit leaves when it is still queued ([`ModelHost::quit`]): pictures and what the view
/// derives from the plate, nothing the person made or asked to keep. Each comes again from the
/// plate at the next start.
fn left_at_quit(job: &Job) -> bool {
    matches!(job, Job::Thumbnail(_) | Job::Painted(_) | Job::MeshFields(false) | Job::ExactDetail(_)
        | Job::Mapped(_) | Job::BendsMeasured(_) | Job::Overhangs(_) | Job::TestMode(_))
}

/// Work a quit makes stale by its ticket ([`ModelHost::quit`]): queued, it never starts; it reads
/// what the view shows and keeps nothing.
fn stale_at_quit(job: &Job) -> bool {
    matches!(job, Job::Preview { .. } | Job::PrintSetup(_) | Job::OverhangList { .. })
}

/// One job a quit waits for ([`ModelHost::keeping`]), counted from its queueing until it ends or is
/// dropped unrun.
struct Keeping(Arc<AtomicUsize>);

impl Drop for Keeping {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

/// The plate's view in its folder. Deleting the plate on the bed also switched it: its trashed
/// folder stays gone.
fn write_view(store: &fab_ai::plates::PlateStore, plate: &str, view: &PlateView) {
    if store.dir(plate).is_dir() {
        if let Err(e) = fab_model::project::write_atomic(&store.view_file(plate), view.to_file_text().as_bytes()) {
            log!("fab: the plate's view could not be saved: {e}");
        }
    }
}

/// A job that changes the shape holds the live slice from when it is queued (the old shape's
/// slice stops: [`crate::live_slice::shape_changed`]) until it is over, however it ends: a new
/// plate, the same plate, nothing to build, a failure, superseded or cancelled before it
/// started. Each end comes as [`HostAction::ShapeJobOver`], after the `Done` or `Preview` that
/// carries its plate; the slice goes on once no such job is left ([`ModelHost::shaping`]).
struct SliceHold {
    held: Arc<AtomicUsize>,
    built: bool,
}

impl SliceHold {
    fn take(held: &Arc<AtomicUsize>, built: bool) -> Self {
        held.fetch_add(1, Ordering::AcqRel);
        crate::live_slice::shape_changed();
        SliceHold { held: held.clone(), built }
    }
}

impl Drop for SliceHold {
    fn drop(&mut self) {
        self.held.fetch_sub(1, Ordering::AcqRel);
        Cx::post_action(HostAction::ShapeJobOver { built: self.built });
    }
}

/// Route repairs through the same model-worker job as the Parts and Issues actions.
fn tool_job(id: u64, tool: &str, args: Value) -> Job {
    if tool == "import" {
        if let Some(path) = args.get("path").and_then(Value::as_str) {
            return Job::ImportFile { id, path: PathBuf::from(path), new_plate: args.get("new_plate").and_then(Value::as_bool).unwrap_or(false) };
        }
    }
    if tool == "make_printable" {
        if let Some(object) = args.get("object").and_then(Value::as_str).map(str::to_string) {
            return Job::MakePrintable { id, object, opts: args, render: true };
        }
    }
    Job::Call { id, tool: tool.into(), args }
}

/// `fab.supports {verdicts: true}`: each piece's way out, a search too slow for the model worker. It
/// runs at low priority, one task a piece, the person sees how far it got and can cancel it, and the
/// call's answer waits for it.
fn check_verdicts(pool: &TaskPool, id: u64, check: Arc<fab_ai::supports::VerdictJob>, running: Option<JobHandle>) {
    let reporter = running.as_ref().map(JobHandle::reporter);
    let cancelled = move || reporter.as_ref().is_some_and(jobs::JobReporter::is_cancelled);
    let end = move |running: Option<JobHandle>, ok: bool, text: String| {
        if let Some(j) = running {
            if ok {
                j.done();
            } else if j.is_cancelled() {
                j.cancelled();
            } else {
                j.fail(job_reason(&text), false);
            }
        }
        Cx::post_action(HostAction::Answered { id, ok, text });
    };
    let tasks = pool.clone();
    let started = std::time::Instant::now();
    let submitted = pool.submit_internal(Class::Background, move || {
        if let Some(j) = &running {
            j.stage("reading the part's shape", None, None);
        }
        let checking = match check.prepare(&|| cancelled()) {
            Ok(checking) => Arc::new(checking),
            Err(e) => return end(running, false, format!("{{error: {}}}", fab_ai::out::quote(&e))),
        };
        let count = checking.len();
        let run = Arc::new(VerdictRun { results: Mutex::new(vec![None; count]), left: std::sync::atomic::AtomicUsize::new(count), running: Mutex::new(running) });
        let finish = {
            let (check, checking, run, cancelled) = (check.clone(), checking.clone(), run.clone(), cancelled.clone());
            move || {
                let results = std::mem::take(&mut *run.results.lock().unwrap());
                let running = run.running.lock().unwrap().take();
                if cancelled() {
                    return end(running, false, "{error: \"cancelled\"}".to_string());
                }
                let answer = check.answer(&checking, &results, started.elapsed().as_secs_f64() * 1e3);
                end(running, true, fab_ai::out::text(&answer));
            }
        };
        if count == 0 {
            return finish();
        }
        let finish = Arc::new(finish);
        for i in 0..count {
            let (checking, run, finish, cancelled) = (checking.clone(), run.clone(), finish.clone(), cancelled.clone());
            let task = tasks.submit_internal(Class::Background, move || {
                let verdict = if cancelled() { None } else { checking.check(i, &|| cancelled()) };
                run.results.lock().unwrap()[i] = verdict;
                let left = run.left.fetch_sub(1, std::sync::atomic::Ordering::AcqRel) - 1;
                if left == 0 {
                    finish();
                } else if let Some(j) = run.running.lock().unwrap().as_ref() {
                    j.stage(format!("{} of {count} pieces checked", count - left), None, Some((count - left) as f32 / count as f32));
                }
            });
            // A closed pool (the app ends) drops the rest; so is the call.
            if let Ok(task) = task {
                task.detach();
            }
        }
    });
    if let Ok(task) = submitted {
        task.detach();
    }
}

/// A read of the slice (`fab.weak_spots`, `fab.tubes`), off the model worker at low priority; a new
/// slice or a shape edit ends it, Cancel too, and the call's answer waits for it.
fn off_worker(pool: &TaskPool, id: u64, running: Option<JobHandle>, find: impl FnOnce(&(dyn Fn() -> bool + Sync)) -> Result<fab_ai::out::S, String> + Send + 'static) {
    let key = crate::live_slice::latest_toolpaths_key();
    let submitted = pool.submit_internal(Class::Background, move || {
        let reporter = running.as_ref().map(JobHandle::reporter);
        let stop = || reporter.as_ref().is_some_and(jobs::JobReporter::is_cancelled) || crate::live_slice::latest_toolpaths_key() != key;
        let answer = find(&stop);
        if let Some(j) = running {
            match &answer {
                Ok(_) => j.done(),
                Err(_) if j.is_cancelled() => j.cancelled(),
                Err(e) => j.fail(job_reason(e), false),
            }
        }
        let (ok, text) = match answer {
            Ok(answer) => (true, fab_ai::out::text(&answer)),
            Err(e) => (false, format!("{{error: {}}}", fab_ai::out::quote(&e))),
        };
        Cx::post_action(HostAction::Answered { id, ok, text });
    });
    if let Ok(task) = submitted {
        task.detach();
    }
}

/// The plate store's writes a first start left on the disk's cache
/// ([`fab_ai::plates::Unflushed`]), flushed at low priority off the model worker. A flush that
/// does not finish leaves the fold undone, and the next start folds the plates in again.
fn flush_plates(pool: &TaskPool, unflushed: fab_ai::plates::Unflushed) {
    let flush = move || {
        if let Err(e) = unflushed.finish() {
            log!("fab: the plates of earlier versions are not flushed to the disk, so the next start folds them in again: {e}");
        }
    };
    if let Err(e) = pool.submit_named(Class::Background, "fab plates flush", flush).map(|task| task.detach()) {
        log!("fab: the plates of earlier versions are not flushed to the disk, so the next start folds them in again: {e}");
    }
}

/// `fab.compare`, once the review drew its views: the reference read against them (meshes and
/// the field parts' traced tiles), off the model worker at low priority, as a fit draws a few
/// hundred silhouettes; the answer comes as [`HostAction::Pictured`].
pub fn compare(pool: &TaskPool, id: u64, frames: review::Frames, job: Arc<fab_ai::compare::CompareJob>) {
    let read = move || Cx::post_action(HostAction::Pictured { id, result: fab_ai::compare::answer(&job, &captured_views(&frames), makepad_ai_hub::providers::provider::MAX_TOOL_IMAGE_SIDE) });
    if let Err(e) = pool.submit_named(Class::Background, "fab compare", read).map(|task| task.detach()) {
        Cx::post_action(HostAction::Pictured { id, result: Err(format!("the compare could not start: {e}")) });
    }
}

/// What a capture drew, as the reading tools take it: each view's camera and its field parts'
/// tiles (shared, not copied).
pub fn captured_views(frames: &review::Frames) -> Vec<fab_ai::compare::CapturedView<'_>> {
    use fab_ai::compare::{CapturedView, FieldTiles};
    frames.cameras.iter().zip(&frames.tiles).map(|((_, camera), fields)| CapturedView {
        camera: *camera,
        fields: fields.iter().map(|t| FieldTiles {
            to_printer: t.to_printer,
            quads: t.chunks.iter().flatten().flat_map(|c| c.segments().iter().map(|g| g.data())).collect(),
            stride: fab::fab_sdf_tiles::trace::TILE_SLOTS,
        }).collect(),
    }).collect()
}

/// What a running [`check_verdicts`]' tasks share.
struct VerdictRun {
    results: Mutex<Vec<Option<fab_loose::verdict::PieceVerdict>>>,
    left: std::sync::atomic::AtomicUsize,
    running: Mutex<Option<JobHandle>>,
}

/// A tool call's job, in words.
pub fn tool_title(tool: &str) -> String {
    match tool {
        "export" => "Exporting the plate".to_string(),
        "save" => "Saving the plate".to_string(),
        "render" => "Rendering the review sheets".to_string(),
        fab_ai::variants::TOOL => "Building the variants".to_string(),
        fab_ai::compare::TOOL => "Comparing with the picture".to_string(),
        "simulate" => "Simulating the mechanism".to_string(),
        "joints" => "Sizing the joints".to_string(),
        "check" | "check_mesh" => "Checking the plate".to_string(),
        "make_printable" => "Making it printable".to_string(),
        "overhangs" => "Finding the overhangs".to_string(),
        "overhang_view" => "Looking at an overhang".to_string(),
        "supports" => "Planning the supports".to_string(),
        "apply" | "script" | "exact" | "edges" | "weave" | "place" | "lay_flat" | "settings_volume" => "Rebuilding the plate".to_string(),
        "new" => "Starting a new plate".to_string(),
        "plates" => "Opening the plate".to_string(),
        tool => format!("Running fab.{tool}"),
    }
}

/// The call id a job answers, when it answers one.
fn job_call_id(job: &Job) -> Option<u64> {
    match job {
        Job::ImportFile { id, .. } | Job::Calibration { id, .. } | Job::Call { id, .. } | Job::Open { id, .. } | Job::Demo { id, .. } | Job::Reload { id } | Job::DeletePlate { id, .. } | Job::AddScripts { id, .. } | Job::Place { id, .. } | Job::Param { id, .. } | Job::Source { id, .. } | Job::Edit { id, .. } | Job::DeleteParts { id, .. } | Job::TestPrints { id, .. } | Job::LayTest { id, .. } | Job::PaintEdit { id, .. } | Job::History { id, .. } | Job::ImportMesh { id, .. } | Job::MakePrintable { id, .. } => Some(*id),
        _ => None,
    }
}

/// A job a failure can sensibly be retried for, to queue again: a file that could not be
/// read or written may be there now. A failed edit or build is not retried: the same
/// input fails the same way.
fn retry_copy(job: &Job) -> Option<Job> {
    match job {
        Job::Call { tool, args, .. } if tool == "export" || tool == "save" => Some(Job::Call { id: 0, tool: tool.clone(), args: args.clone() }),
        Job::Open { path, .. } => Some(Job::Open { id: 0, path: path.clone() }),
        Job::ImportFile { path, new_plate, .. } => Some(Job::ImportFile { id: 0, path: path.clone(), new_plate: *new_plate }),
        Job::AddScripts { paths, .. } => Some(Job::AddScripts { id: 0, paths: paths.clone() }),
        _ => None,
    }
}

/// Whether a failure is one that trying again can help: a file or the disk, not the model.
fn retryable_text(text: &str) -> bool {
    let t = reason(text).to_lowercase();
    ["no such file", "not found", "permission", "denied", "disk", "space", "busy", "temporarily", "i/o", "os error"].iter().any(|w| t.contains(w))
}

/// A failed reply's reason in words (its `error` field), else the reply itself.
pub fn reason(text: &str) -> String {
    makepad_script::syntax::parse_value(text)
        .ok()
        .and_then(|v| v.field("error").and_then(|f| f.value.string().map(str::to_string)))
        .unwrap_or_else(|| text.to_string())
}

/// A failed job's reason for the job strip and its list, the why first. A build failure reads
/// "the project would not build, nothing committed: broken.cad.splash:4:10: <why>"; the strip's
/// slot cut that inside the wrapper, so it showed no why at all. The job says
/// "<why> (broken.cad.splash:4:10)": the strip shows the why's start, the list all of it.
pub fn job_reason(text: &str) -> String {
    const BUILD: &str = "the project would not build, nothing committed: ";
    let full = reason(text);
    let Some(failure) = full.trim().strip_prefix(BUILD) else { return full };
    let why = fab::params::failure_reason(failure);
    match failure.split_once(": ") {
        Some((place, _)) if place.rsplitn(3, ':').nth(1).is_some_and(|line| !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit())) => format!("{why} ({place})"),
        _ => why,
    }
}

/// A document put on the bed: built, or the bed as it was when it does not build (or the build
/// was cancelled).
fn on_bed(session: &mut Session, what: &str, put: Result<Arc<Evaluated>, String>) -> fab_ai::Reply {
    match put {
        Ok(ev) => fab_ai::Reply { images: Vec::new(), ok: true, text: format!("{{opened: {}}}", fab_ai::out::quote(what)), update: session.compiled.clone().map(|c| (c, ev)), render: None },
        Err(e) => failed(e),
    }
}

fn failed(e: String) -> fab_ai::Reply {
    fab_ai::Reply { images: Vec::new(), ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(&e)), update: None, render: None }
}

/// The plate list for the app: the records as they are, the demos, each with its picture.
/// The plates (none without a store: a driven or test instance keeps none) and every demo.
/// A plate whose file this Fab could not read when it was listed (one of an earlier format),
/// with no working copy to show instead: its row says why, and it has nothing to paint.
fn unread<'a>(store: Option<&fab_ai::plates::PlateStore>, p: &'a fab_ai::plates::Plate) -> Option<&'a PathBuf> {
    p.file.as_ref().filter(|_| p.saved.is_empty() && !store.is_some_and(|s| s.has_working_copy(&p.id)))
}

/// Why an unread plate's `file` does not read: what it meets in it. Read once for each version
/// of the file, not each time the list is sent (after every job): every read evaluates it again.
/// The read is no run: what it meets is the row's why and is reported nowhere else, not filed
/// as a run's script errors.
fn unread_reason(file: &PathBuf, reasons: &mut HashMap<PathBuf, (std::time::SystemTime, String)>, env: &ScriptEnv) -> String {
    let at = std::fs::metadata(file).and_then(|m| m.modified()).ok();
    if let (Some(at), Some((seen, why))) = (at, reasons.get(file)) {
        if *seen == at {
            return why.clone();
        }
    }
    let why = match makepad_script::inspect_registry::unreported(|| fab_model::project::load(file, env)) {
        Ok(_) => format!("{} reads now: open it to take it in", file.display()),
        // the reason names the file where it met something (the line and column)
        Err(e) if e.contains(&file.display().to_string()) => e,
        Err(e) => format!("{}: {e}", file.display()),
    };
    if let Some(at) = at {
        reasons.insert(file.clone(), (at, why.clone()));
    }
    why
}

/// The files the plate list reads: its pictures, and the reasons unread files do not read.
#[derive(Default)]
struct ListReads {
    pictures: HashMap<PathBuf, (std::time::SystemTime, Arc<[u8]>)>,
    reasons: HashMap<PathBuf, (std::time::SystemTime, String)>,
}

fn plate_list(session: &Session, reads: &mut ListReads) -> fab::api::PlateList {
    let ListReads { pictures, reasons } = reads;
    let store = session.store.as_ref();
    let mut picture = |path: Option<PathBuf>| -> Option<Arc<[u8]>> {
        let path = path?;
        let at = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
        if let Some((seen, png)) = pictures.get(&path).filter(|(seen, _)| *seen == at).map(|(s, p)| (*s, p.clone())) {
            let _ = seen;
            return Some(png);
        }
        let png: Arc<[u8]> = Arc::from(std::fs::read(&path).ok().filter(|png| !png.is_empty())?);
        pictures.insert(path, (at, png.clone()));
        Some(png)
    };
    let today = makepad_civil_time::local_or_utc(makepad_civil_time::now_secs()).day_number();
    let title = |key: &str| crate::demo_part::demo(key).map_or_else(|| key.to_string(), |d| d.title.to_string());
    let plates = session.plates().into_iter().map(|p| {
        let unread = unread(store, &p);
        // what its folder keeps aside (a working copy that did not read back), and the files it
        // took in as they read though Fab did not write them so: its row says so, and why
        let kept = store.map_or_else(Vec::new, |s| s.kept_aside(&p.id));
        let outside = session.outside_edits.get(&p.id);
        let why: Vec<String> = kept.iter().map(|(folder, why)| format!("{why} (kept in {})", folder.display()))
            .chain(outside.into_iter().flatten().cloned())
            .chain(unread.map(|file| unread_reason(file, reasons, session.model.env())))
            .collect();
        fab::api::PlateCard {
        picture: picture(store.and_then(|s| s.picture_of(&p))),
        group: String::new(),
        note: match &p.origin {
            _ if !kept.is_empty() => "files kept aside".to_string(),
            _ if outside.is_some() => "changed outside Fab".to_string(),
            _ if unread.is_some() => "this Fab cannot read its file".to_string(),
            fab_ai::plates::Origin::Plate => String::new(),
            fab_ai::plates::Origin::Made => "made from a picture".to_string(),
            fab_ai::plates::Origin::Demo(key) => format!("copy of {}", title(key)),
        },
        why: why.join("\n"),
        when: fab_ai::plates::short_when(p.edited, today),
        id: p.id,
        demo: false,
        name: p.name,
        modified: p.modified,
        printer: p.printer,
        file: p.file,
        }
    }).collect();
    let demos = crate::demo_part::listed().iter().map(|d| fab::api::PlateCard {
        id: d.key.to_string(),
        demo: true,
        group: d.group.to_string(),
        name: d.title.to_string(),
        modified: false,
        printer: String::new(),
        when: String::new(),
        file: None,
        note: String::new(),
        why: String::new(),
        picture: picture(store.map(|s| s.demo_picture(d.key))),
    }).collect();
    let (on_bed, on_demo) = match (&session.plate, session.demo_key()) {
        (Some(id), _) => (Some(id.clone()), false),
        (None, Some(key)) => (Some(key.to_string()), true),
        (None, None) => (None, false),
    };
    fab::api::PlateList { plates, demos, on_bed, on_demo }
}

struct HostState {
    /// Numbers come from this shared counter; a reservation takes one early.
    steps: Arc<AtomicU64>,
    /// The newest entry logged by either history.
    logged: Arc<AtomicU64>,
    words: Arc<Mutex<WorkerWords>>,
    docs_newest: Arc<Mutex<HashMap<u64, u64>>>,
    replacing: Arc<AtomicUsize>,
    switched_now: Arc<AtomicU64>,
    session: Option<Session>,
    xr: bool,
    overhangs: bool,
    scenes: Scenes,
    /// The plate list as last sent ([`Session::plates_rev`]) and the files read for it.
    plates_seen: Option<u64>,
    list_reads: ListReads,
    /// The pictures being painted ([`crate::plate_paint`]), of a run of them; the plates and demos
    /// looked at for one this session (a plate's id, a demo's key after `demo:`).
    painting: usize,
    painting_run: Option<crate::live_slice::Painting>,
    paint_seen: std::collections::HashSet<String>,
    /// Pictures whose build stopped at the memory budget beside others: each is painted again
    /// alone, once the others ended ([`crate::plate_paint::Painted::Again`]); `painting_alone`
    /// while one is.
    paint_again: std::collections::VecDeque<String>,
    painting_alone: bool,
    /// A quit is on its way ([`ModelHost::quit`]): no picture starts.
    quitting: Arc<AtomicBool>,
    /// [`ModelHost::saved`].
    saved: Arc<AtomicBool>,
    persist: bool,
    retry_after: std::time::Instant,
    /// Where graded parts' stress is solved, the build it last looked at and what it asked.
    pool: TaskPool,
    stress_seen: usize,
    /// The build whose field volumes were last asked for.
    volumes_seen: std::sync::Weak<Evaluated>,
    /// The build the veined parts last looked at, and what each part's tubes were last asked for
    /// (a raise asked for something else stops).
    veins_seen: usize,
    veins_asked: Arc<Mutex<HashMap<(String, String), u64>>>,
    /// The grown parts' runs, the build they last looked at and the hub they share.
    grows: Arc<crate::grow_live::Runs>,
    grow_seen: usize,
    grow_next: u64,
    hub: crate::grow_live::SharedHub,
    /// The job [`HostState::init`] builds what an earlier run left on the bed under (no strip or
    /// card of its own): a plate opened from the list meanwhile cancels it ([`ModelHost::recovery`]).
    recovery: Option<JobHandle>,
}

impl HostState {
    fn init(&mut self) {
        let machine = crate::print::CheckSetup::fab_defaults().machine().expect("Fab defaults have a valid machine");
        let mut session = Session::new(machine, documents_dir());
        session.model.set_steps(self.steps.clone(), self.logged.clone());
        // The scripts' standard env: a case script reads its board whether or not the
        // Electronics workspace has started (that host replaces the env with its own boards),
        // and a plate with electronics documents reads its own.
        session.model.set_env(crate::elec_host::shared_env());
        session.env_for = Some(Arc::new(crate::elec_host::doc_env));
        session.eval.memory_guard = Some(crate::live_slice::model_memory_guard());
        // a grown pattern shows as it grows: the plate is built with its latest state, and the
        // slice waits for the one that prints (`crate::pattern_live`)
        session.eval.patterns_wait = false;
        session.demos = fab_ai::Demos {
            list: crate::demo_part::listed().iter().map(|d| (d.key.to_string(), d.title.to_string())).collect(),
            build: |key| crate::demo_part::demo(key).and_then(|d| d.model().ok()),
        };
        let persist = self.persist;
        if persist {
            session.store = Some(plate_store());
        }
        // What the last run had on the bed (a crash, a quit, a live-patch relaunch) comes back
        // as it was left; else the empty model. The plate list goes first, read from the disk:
        // that plate's build can take long (a big field), and the list shows meanwhile; a plate
        // opened from it stops the build ([`ModelHost::recovery`]) and goes on the bed next.
        let (left, unflushed) = if persist { session.left_on_bed() } else { (None, None) };
        Cx::post_action(HostAction::Plates(Mutex::new(Some(plate_list(&session, &mut self.list_reads)))));
        // What a first start's fold of earlier versions' plates wrote reads back from the disk's
        // cache; its flush to the disk runs off this worker, so neither the list nor the plate
        // opened from it waits on a slow disk.
        if let Some(unflushed) = unflushed {
            flush_plates(&self.pool, unflushed);
        }
        let recovery = self.recovery.take();
        let previous_switch = session.switched;
        let reopened = {
            let _job = jobs::enter(recovery.as_ref().map(|j| j.reporter()));
            left.and_then(|left| session.reopen(left))
        };
        let gave_way = recovery.as_ref().is_some_and(|j| j.is_cancelled());
        if let Some(job) = recovery {
            if gave_way { job.cancelled() } else { job.done() }
        }
        let recovered = match reopened {
            Some(Ok(ev)) => {
                log!("fab: opened '{}' ({} solids) as it was left", session.model.name, session.compiled.as_ref().map_or(0, |c| c.instances.iter().map(|i| i.script.program.solids.len()).sum::<usize>()));
                Ok(ev)
            }
            Some(Err(_)) if gave_way => {
                log!("fab: the plate left on the bed gave way to the one opened");
                session.rebuild()
            }
            Some(Err(e)) => {
                log!("fab: {e}");
                session.rebuild()
            }
            None => session.rebuild(),
        };
        // The plate list again before the first plate, the one on the bed marked: the app knows
        // whether anything came back.
        self.plates_seen = Some(session.plates_rev);
        Cx::post_action(HostAction::Plates(Mutex::new(Some(plate_list(&session, &mut self.list_reads)))));
        // The empty model: the bed alone, so the view has something from the start.
        if let Ok(ev) = recovered {
            let scene = self.scenes.of(&session, &ev, self.overhangs);
            let done = Done { images: Vec::new(), id: 0, ok: true, text: String::new(), review: Mutex::new(None), update: Some(plate_update(&mut session, ev, scene)), status: status_after_job(previous_switch, &mut session), model_changed: true, check_slice: None, source_files: Some(Arc::new(session.source_files())), source_problem: None, capped: None };
            Cx::post_action(HostAction::Done(Box::new(done)));
        }
        self.session = Some(session);
        // The list's missing pictures from the start, the demos' too.
        self.paint_next();
    }

    /// Send the app the plate list when it changed.
    fn post_plates(&mut self) {
        let Some(session) = self.session.as_ref() else { return };
        send_plates(session, &mut self.plates_seen, &mut self.list_reads);
    }

    /// Paint the next pictures the list lacks, several at once: the plates first, then the
    /// demos; what is on the bed takes the view's own. Each is shown as it ends, and the next
    /// starts then: a slow one (a plate of many printed-in-place joints builds for tens of
    /// seconds) holds back none of the others. They hold at most half the pool's workers: each
    /// build fans its own big passes out besides, and the rest of the app's work finds workers
    /// free beside them. On four cores that is one: the CPU budget runs one background job there
    /// anyway, so a slow picture then holds back the next. A painted picture is looked at again once a session (its content may
    /// have changed since). O(plates + demos) per picture started.
    fn paint_next(&mut self) {
        if self.quitting.load(Ordering::Acquire) {
            return;
        }
        // The run goes on while any is painted: its end, here when none is, is a heavy job's.
        let run = self.painting_run.take();
        let at_once = (self.pool.worker_count() / 2).max(1);
        while !self.painting_alone && self.painting < at_once {
            // one that stopped at the memory budget beside others goes alone, once they ended
            let again = match self.paint_again.front() {
                Some(_) if self.painting > 0 => break,
                Some(_) => self.paint_again.pop_front(),
                None => None,
            };
            if let Some(key) = &again {
                self.paint_seen.remove(key);
            }
            let Some(mut order) = self.next_picture(again.as_deref()) else {
                if again.is_some() {
                    continue;
                }
                break;
            };
            order.alone = again.is_some();
            if !crate::plate_paint::start(&self.pool, order) {
                break;
            }
            self.painting += 1;
            self.painting_alone = again.is_some();
        }
        if self.painting > 0 {
            self.painting_run = Some(run.unwrap_or_else(crate::live_slice::Painting::begin));
        }
    }

    /// The next picture the list lacks (only `only`, a key among those looked at, when given),
    /// marked as looked at; none when every row has one.
    fn next_picture(&mut self, only: Option<&str>) -> Option<crate::plate_paint::Order> {
        let session = self.session.as_ref()?;
        let store = session.store.clone()?;
        let painted = |picture: &std::path::Path| fab_ai::plates::painted_content(picture).is_file();
        let seen = &mut self.paint_seen;
        let wanted_key = |key: &str| only.is_none_or(|only| only == key);
        // An unread plate's file builds nothing: its row says why.
        let plate = session.plates().into_iter().filter(|p| session.plate.as_ref() != Some(&p.id) && wanted_key(&p.id) && unread(Some(&store), p).is_none()).find_map(|p| {
            let picture = store.picture(&p.id);
            let wanted = store.picture_of(&p).is_none() || painted(&picture);
            (wanted && seen.insert(p.id.clone())).then(|| crate::plate_paint::Order { key: p.id.clone(), out: picture, subject: crate::plate_paint::Subject::Plate { store: store.clone(), id: p.id, file: p.file }, machine: session.eval.machine.clone(), alone: false })
        });
        plate.or_else(|| {
            crate::demo_part::listed().iter().filter(|d| session.demo_key() != Some(d.key)).find_map(|d| {
                let key = format!("demo:{}", d.key);
                if !wanted_key(&key) {
                    return None;
                }
                let picture = store.demo_picture(d.key);
                let wanted = !fab_ai::plates::has_picture(&picture) || painted(&picture);
                (wanted && seen.insert(key)).then(|| crate::plate_paint::Order { key: d.key.to_string(), out: picture, subject: crate::plate_paint::Subject::Demo(d), machine: session.eval.machine.clone(), alone: false })
            })
        })
    }

    fn run(&mut self, queued: Queued, ticket: Ticket) {
        if let Job::SaveView { plate, view } = queued.0 {
            if let Some(store) = self.session.as_ref().and_then(|s| s.store.as_ref()) {
                write_view(store, &plate, &view);
            }
            return;
        }
        if let Job::Quit { id, asked } = queued.0 {
            // Saved now even when an earlier save failed and its retry waits: this is the last one.
            let persist = self.persist;
            let saved = match self.session.as_mut() {
                Some(session) if persist && session.needs_autosave() => session.autosave(),
                _ => Ok(()),
            };
            match &saved {
                Ok(()) => log!("fab: quitting {:.0} ms after it was asked, the plate's last edits saved", asked.elapsed().as_secs_f64() * 1e3),
                Err(e) => log!("fab: the plate's working copy could not be saved before quitting ({e}); the copy on disk stays as it was"),
            }
            if let Some(j) = queued.1 {
                j.done();
            }
            Cx::post_action(HostAction::Quit { id });
            return;
        }
        if let Job::Painted(painted) = queued.0 {
            self.painting = self.painting.saturating_sub(1);
            if self.painting == 0 {
                self.painting_alone = false;
            }
            match painted {
                crate::plate_paint::Painted::Done(true) => {
                    if let Some(session) = self.session.as_mut() {
                        session.plates_rev += 1;
                    }
                }
                crate::plate_paint::Painted::Done(false) => {}
                crate::plate_paint::Painted::Again(key) => self.paint_again.push_back(key),
            }
            self.post_plates();
            self.paint_next();
            return;
        }
        let listed = self.plates_seen;
        let commit = matches!(queued.0, Job::Param { .. });
        self.words.lock().unwrap().set(Some(job_words(&queued.0)), queued.1.as_ref().map(|j| j.id()));
        self.process(queued, ticket);
        self.words.lock().unwrap().set(Some("the stress and growth studies".to_string()), None);
        self.ask_stress();
        self.ask_grow(commit);
        self.ask_veins();
        self.ask_volumes();
        // Working-copy I/O belongs to the owner job, never a sleeping worker.
        if self.persist && std::time::Instant::now() >= self.retry_after {
            self.words.lock().unwrap().set(Some("the plate's working copy".to_string()), None);
            if let Some(session) = self.session.as_mut().filter(|s| s.needs_autosave()) {
                if let Err(e) = session.autosave() {
                    log!("fab: autosave failed: {e}");
                    self.retry_after = std::time::Instant::now() + std::time::Duration::from_secs(5);
                }
            }
        }
        self.note_saved();
        self.post_plates();
        if self.plates_seen != listed {
            self.paint_next();
        }
        self.words.lock().unwrap().set(None, None);
    }

    /// Whether the plate's working copy holds every edit taken, for a quit to read
    /// ([`ModelHost::quit`]). Cost: O(1) (a few pointer compares).
    fn note_saved(&self) {
        self.saved.store(self.session.as_ref().is_none_or(|s| !s.needs_autosave()), Ordering::Release);
    }

    /// The field parts' volumes of the latest build, read off this strand on the pool's background
    /// class ([`fab_model::eval::FieldVolume`]): no build and no answer waits for them, and what
    /// shows them says pending until they are in. A build no longer held when its turn comes is
    /// not read.
    fn ask_volumes(&mut self) {
        let Self { session, pool, volumes_seen, .. } = self;
        let Some(ev) = session.as_ref().and_then(|s| s.last.clone()) else { return };
        // held weakly, so no later build takes its address
        if std::ptr::eq(volumes_seen.as_ptr(), Arc::as_ptr(&ev)) {
            return;
        }
        *volumes_seen = Arc::downgrade(&ev);
        if !ev.fields.iter().flatten().any(|f| f.volume.as_ref().is_some_and(|v| v.known().is_none())) {
            return;
        }
        let ev = Arc::downgrade(&ev);
        let read = move || {
            if let Some(ev) = ev.upgrade() {
                for v in ev.fields.iter().flatten().filter_map(|f| f.volume.as_ref()) {
                    v.read();
                }
            }
        };
        if let Err(e) = pool.submit_named(Class::Background, "fab field volumes", read).map(|task| task.detach()) {
            log!("fab: the field volumes could not start: {e}");
        }
    }

    /// The graded parts of the latest build (`adapt: @stress`): a placed field part whose script
    /// asks is solved off this strand when what the answer depends on changed, its first
    /// placement standing for all of them; the answer comes back as [`Job::Solved`], and until
    /// then the part keeps grading by its last. With its study no longer posed, it has none.
    fn ask_stress(&mut self) {
        let Self { session, pool, stress_seen, .. } = self;
        let Some(session) = session.as_mut() else { return };
        let (Some(c), Some(ev)) = (session.compiled.clone(), session.last.clone()) else { return };
        if Arc::as_ptr(&c) as usize == *stress_seen {
            return;
        }
        *stress_seen = Arc::as_ptr(&c) as usize;
        let mut placed = std::collections::HashSet::new();
        for (i, part) in ev.plate.parts.iter().enumerate() {
            let Some((inst, obj)) = c.object(&part.object) else { continue };
            let script = inst.script.clone();
            if script.program.stress_requests.is_empty() || !placed.insert(script.file.clone()) {
                continue;
            }
            let (Some(field), Some(to_plate)) = (ev.fields.get(i).and_then(|f| f.clone()), ev.model_to_plate.get(i)) else { continue };
            let posed = script.study.get(&obj.name).filter(|s| s.posed()).zip(crate::study_edit::Frame::new(to_plate));
            let Some((study, frame)) = posed else {
                for r in &script.program.stress_requests {
                    if session.model.ask_solved(&script.file, &r.part, None) {
                        let stress = Solved { file: script.file.clone(), part: r.part.clone(), study: STRESS_STUDY, doing: format!("Grading {} by its stress", r.part), asked: None, grid: None };
                        Cx::post_action(HostAction::Solved(Mutex::new(Some(Box::new(stress)))));
                    }
                }
                continue;
            };
            let loads: Vec<_> = crate::study_edit::loads(study, &frame).into_iter().map(|(l, _)| l).collect();
            // What an answer depends on: the object with its graded parts solid (not their
            // grids, nor a comment), the study, and the part's turn on the plate (not a move).
            use makepad_model_script::lang::csg_sdf::part_id;
            let solid = script.program.stress_requests.iter().fold(field.shape.root.clone(), |root, r| fab_optimise::grade::with_source(&root, part_id(&r.part), &r.source));
            let shape = makepad_model_script::lang::csg_sdf::SdfShape { root: solid.clone(), ..(*field.shape).clone() };
            let turn: [[u64; 3]; 3] = field.to_printer.map(|row| [row[0].to_bits(), row[1].to_bits(), row[2].to_bits()]);
            let posed: Vec<String> = study.loads.iter().map(|l| format!("{:?}|{:?}|{}", l.kind, l.at, l.rigid)).collect();
            for r in &script.program.stress_requests {
                if fab_optimise::grade::part_places(&solid, part_id(&r.part)).is_err() {
                    continue;
                }
                let asked = format!("{:?}|{turn:?}|{posed:?}|{}|{:?}|{:?}|{}", shape.key(), r.part, r.lo, r.hi, r.spacing).bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
                if !session.model.ask_solved(&script.file, &r.part, Some(asked)) {
                    continue;
                }
                let (request, root, to_printer, loads, file) = (r.clone(), solid.clone(), field.to_printer, loads.clone(), script.file.clone());
                let solve = move || {
                    let grid = match fab_optimise::grade::stress_grid(&root, to_printer, &request, &loads, fab_force::study::FIRST_CELLS) {
                        Ok(g) => {
                            log!("fab: {} graded by its stress (reference {:.2} MPa at {:.2} mm cells)", request.part, g.reference, g.h);
                            Some(Arc::new(g.grid))
                        }
                        Err(e) => {
                            log!("fab: {} could not be graded by its stress: {e}", request.part);
                            None
                        }
                    };
                    let stress = Solved { file, doing: format!("Grading {} by its stress", request.part), part: request.part, study: STRESS_STUDY, asked: Some(asked), grid };
                    Cx::post_action(HostAction::Solved(Mutex::new(Some(Box::new(stress)))));
                };
                match pool.submit_named(Class::Background, "fab grade by stress", solve) {
                    Ok(h) => h.detach(),
                    Err(e) => log!("fab: grading by stress could not start: {e:?}"),
                }
            }
        }
    }

    /// The veined parts of the latest build (`m.veins`): a placed field part whose script asks has
    /// its mould grown and its tubes raised off this strand when what they depend on changed (its
    /// source, its mould, the printer's up where it stands, the overhang angle), its first
    /// placement standing for all of them; the answer comes back as [`Job::Solved`], and until
    /// then the part keeps its last tubes. A raise whose part has since asked for something else
    /// stops.
    fn ask_veins(&mut self) {
        let Self { session, pool, veins_seen, veins_asked, .. } = self;
        let Some(session) = session.as_mut() else { return };
        let (Some(c), Some(ev)) = (session.compiled.clone(), session.last.clone()) else { return };
        if Arc::as_ptr(&c) as usize == *veins_seen {
            return;
        }
        *veins_seen = Arc::as_ptr(&c) as usize;
        let mut placed = std::collections::HashSet::new();
        for (i, part) in ev.plate.parts.iter().enumerate() {
            let Some((inst, _)) = c.object(&part.object) else { continue };
            let script = inst.script.clone();
            if script.program.veins_requests.is_empty() || !placed.insert(script.file.clone()) {
                continue;
            }
            let Some(field) = ev.fields.get(i).and_then(|f| f.clone()) else { continue };
            for (request, up, lowest) in veins_asks(&script.program.veins_requests, &field, &ev.plate.settings) {
                let asked = fab_model::veins::asked(&request, up, lowest);
                if !session.model.ask_solved(&script.file, &request.part, Some(asked)) {
                    continue;
                }
                let key = (script.file.clone(), request.part.clone());
                veins_asked.lock().unwrap().insert(key.clone(), asked);
                let latest = veins_asked.clone();
                let raise = move || {
                    let stop = || latest.lock().unwrap().get(&key) != Some(&asked);
                    let grid = match fab_model::veins::answer(&request, up, lowest, &stop) {
                        Ok(g) => g,
                        Err(e) => {
                            log!("fab: {}'s veins could not rise: {e}", request.part);
                            None
                        }
                    };
                    if stop() {
                        return;
                    }
                    let solved = Solved { file: key.0, doing: format!("Raising {}'s veins", request.part), part: request.part, study: VEINS_STUDY, asked: Some(asked), grid };
                    Cx::post_action(HostAction::Solved(Mutex::new(Some(Box::new(solved)))));
                };
                match pool.submit_named(Class::Background, "fab raise veins", raise) {
                    Ok(h) => h.detach(),
                    Err(e) => log!("fab: raising veins could not start: {e:?}"),
                }
            }
        }
    }

    /// The grown parts of the latest build (`m.grow`): a placed field part whose script asks and
    /// whose study holds and loads it has a run ([`crate::grow_live`]), its first placement standing
    /// for all of them. The run's shapes come back as [`Job::Grown`]; a change to its loads or mass
    /// goes to the run, a change to what it grows in starts another (the part keeps its last shape
    /// meanwhile). A part with no such study has no run and no shape.
    /// `commit`: the job just done set a parameter, the build a release waits for.
    fn ask_grow(&mut self, commit: bool) {
        use makepad_model_script::lang::csg_sdf::part_id;
        let Self { session, grows, grow_seen, grow_next, hub, overhangs, scenes, .. } = self;
        // A release asks the runs again when its commit's build is in (the value let go, or the one
        // kept when the commit is refused). Taken by an earlier job, the ask used a build from before
        // the commit, the value the drag began at, and sent the part back there before the commit's
        // build sent it on again.
        let look_again = commit && grows.take_look_again();
        let mut runs = grows.runs.lock().unwrap();
        let grows = &mut *runs;
        let Some(session) = session.as_mut() else { return };
        let (Some(c), Some(ev)) = (session.compiled.clone(), session.last.clone()) else { return };
        if Arc::as_ptr(&c) as usize == *grow_seen && !look_again {
            return;
        }
        *grow_seen = Arc::as_ptr(&c) as usize;
        let mut asked = std::collections::HashSet::new();
        // parts this pass has told what they lack: their line stays
        let mut told = std::collections::HashSet::new();
        let mut placed = std::collections::HashSet::new();
        for (i, part) in ev.plate.parts.iter().enumerate() {
            let Some((inst, obj)) = c.object(&part.object) else { continue };
            let script = inst.script.clone();
            if script.program.grow_requests.is_empty() || !placed.insert(script.file.clone()) {
                continue;
            }
            let Some(field) = ev.fields.get(i).and_then(|f| f.clone()) else { continue };
            let Some(study) = script.study.get(&obj.name).filter(|s| s.posed()) else {
                for r in &script.program.grow_requests {
                    let missing = crate::grow_live::study_missing(script.study.get(&obj.name).map(|s| &**s));
                    told.insert((script.file.clone(), r.part.clone()));
                    Cx::post_action(HostAction::GrowLine(fab::params::GrowLine { object: part.object.clone(), part: r.part.clone(), text: format!("{}: {missing}", r.part) }));
                }
                continue;
            };
            let Some(to_plate) = ev.model_to_plate.get(i) else { continue };
            // What it grows in: the object with its grown parts solid (not their shapes).
            let solid = script.program.grow_requests.iter().fold(field.shape.root.clone(), |root, r| fab_optimise::grade::with_source(&root, part_id(&r.part), &r.source));
            let shape = makepad_model_script::lang::csg_sdf::SdfShape { root: solid, ..(*field.shape).clone() };
            for r in &script.program.grow_requests {
                let Some(loads) = crate::grow_live::loads_of(&field, to_plate, &r.part, study) else {
                    Cx::post_action(HostAction::GrowLine(fab::params::GrowLine { object: part.object.clone(), part: r.part.clone(), text: format!("{} cannot grow: it has no one frame", r.part) }));
                    continue;
                };
                let key = (script.file.clone(), r.part.clone());
                asked.insert(key.clone());
                let space = format!("{:?}|{:?}|{:?}|{}", shape.key(), r.lo, r.hi, r.member).bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
                match grows.get_mut(&key).filter(|run| run.space == space && !run.ended()) {
                    Some(run) => run.ask(loads, r.mass),
                    None => {
                        *grow_next += 1;
                        grows.insert(key, crate::grow_live::Run::start(*grow_next, hub.clone(), script.file.clone(), part.object.clone(), r.clone(), field.shape.root.clone(), space, loads, session.model.stored(&script.file, &r.part)));
                    }
                }
            }
        }
        let gone: Vec<_> = grows.keys().filter(|k| !asked.contains(*k)).cloned().collect();
        for key in gone {
            if let Some(run) = grows.remove(&key).filter(|_| !told.contains(&key)) {
                Cx::post_action(HostAction::GrowLine(fab::params::GrowLine { object: run.object.clone(), part: key.1.clone(), text: String::new() }));
            }
            show_grown(session, scenes, *overhangs, &key.0, &key.1, None, true, false);
        }
    }

    fn process(&mut self, (job, mut running): Queued, ticket: Ticket) {
        let Self { session, xr, overhangs, scenes, grows, pool, plates_seen, list_reads, docs_newest, .. } = self;
        let mut session = session.as_mut().expect("model initialized before jobs");
        Cx::post_action(HostAction::Ready);
        // A job cancelled while it waited never starts.
        if running.as_ref().is_some_and(|j| j.is_cancelled()) {
            if let Some(j) = running.take() {
                j.cancelled();
            }
            if let Some(id) = job_call_id(&job) {
                let text = "{error: \"cancelled\"}".to_string();
                Cx::post_action(HostAction::Done(Box::new(Done { images: Vec::new(), id, ok: false, text, review: Mutex::new(None), update: None, status: status(&mut session), model_changed: false, check_slice: None, source_files: None, source_problem: None, capped: None })));
            }
            return;
        }
        let load = loads(&job).is_some();
        // A load's build is the first share of its job ([`LOAD_BUILT`]), the view made of it the rest.
        let reporter = running.as_ref().map(|j| if load { j.reporter().part(0.0, LOAD_BUILT, "") } else { j.reporter() });
        // its build, its own: the hold on loads stops it (no other build)
        session.eval.gate = running.as_ref().filter(|_| load).map(|j| {
            let job = j.reporter();
            fab_model::eval::BuildGate { job: job.id(), at_second_step: Box::new(move || load_hold::HOLD.wait(&job)) }
        });
        if let Some(r) = &reporter {
            r.stage(if load { "reading the plate" } else { "" }, None, None);
        }
        // Deep code (the evaluator, the B-rep worker) reports on and stops for it.
        let _current = jobs::enter(reporter);
        let can_retry = retry_copy(&job).is_some();
        // A refused parameter commit is reported by its cap line, not by the job strip.
        let is_param = matches!(job, Job::Param { .. });
        // Rulers and test prints alter the document, but never invalidate its printable geometry.
        let measurement_only = match &job {
            Job::Edit { edits, .. } => !edits.is_empty() && edits.iter().all(|e| matches!(e,
                fab_model::Change::MeasureAdd { .. } | fab_model::Change::MeasureSet { .. } | fab_model::Change::MeasureRemove { .. } | fab_model::Change::TestPrint { .. })),
            Job::Call { tool, .. } => tool == "measure" || tool == "test_print",
            Job::TestPrints { .. } | Job::LayTest { .. } => true,
            _ => false,
        };
        let mut source_problem = None;
        let mut capped: Option<(PValue, String)> = None;
        let previous_name = session.model.name.clone();
        let previous_switch = session.switched;
        let (id, reply) = match job {
            Job::PrintSetup(setup) => {
                // the printer's mechanisms profile (calibrated fits) sizes joints and links
                // as-printed checks (fab.weave) slice with the app's own setup and paint
                let own = setup.clone();
                session.print_setup = Some(Arc::new(move |ev: &Evaluated| own.model_job(ev)));
                session.slices = Some(Arc::new(crate::live_slice::AiSlices::new(setup.clone())));
                let machine = match setup.machine() {
                    Ok(machine) => machine,
                    Err(e) => {
                        Cx::post_action(HostAction::Done(Box::new(Done { images: Vec::new(), id: 0, ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(&e)), review: Mutex::new(None), update: None, status: status(&mut session), model_changed: false, check_slice: None, source_files: None, source_problem: None, capped: None })));
                        return;
                    }
                };
                // A new volume builds the plate again; a new name alone only relabels it.
                let machine_changed = !machine.same_volume(&session.eval.machine);
                session.eval.machine = machine;
                let profile = setup.mech_profile();
                if profile != session.eval.profile {
                    log!("fab mech: printer profile now nozzle {} layer {} {} measured {:?}", profile.nozzle, profile.layer, profile.material.name, profile.measured);
                }
                // The job's slice tolerance: exact parts' print meshes are made at it.
                let empty = fab_plate::Plate::default();
                let asked = setup.print_chord(session.last.as_ref().map_or(&empty, |ev| &ev.plate), &session.eval.machine);
                let chord = match asked {
                    Ok(chord) => session.set_print_chord(chord),
                    Err(e) => {
                        log!("fab: the job's slice tolerance: {e}");
                        false
                    }
                };
                // What the job prints round the parts: the layout keeps it in reach.
                let asked = setup.print_rim(session.last.as_ref().map_or(&empty, |ev| &ev.plate), &session.eval.machine);
                let rim = match asked {
                    Ok(rim) => session.set_rim(rim),
                    Err(e) => {
                        log!("fab: the job's skirt and brim: {e}");
                        false
                    }
                };
                let changed = session.set_profile(profile) | machine_changed | chord | rim;
                if changed && session.last.is_some() {
                    let job = JobSpec::new(format!("Rebuilding {} for the printer", session.model.name)).machine(HERE).start();
                    let built = job.enter(|| session.rebuild());
                    if let Err(e) = &built {
                        log!("fab: rebuild for the printer profile: {e}");
                    }
                    job.finish(&built, false);
                    if let Err(e) = built {
                        Cx::post_action(HostAction::Done(Box::new(Done { images: Vec::new(), id: 0, ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(&e)), review: Mutex::new(None), update: None, status: status(&mut session), model_changed: false, check_slice: None, source_files: None, source_problem: None, capped: None })));
                        return;
                    }
                }
                if let Some(ev) = session.last.clone() {
                    let mut ev = (*ev).clone();
                    // The plate keeps the volume it was built for; it shows the printer's name.
                    ev.machine.name.clone_from(&session.eval.machine.name);
                    let error = match setup.materials(&ev.plate, &ev.machine) {
                        Ok(materials) => {
                            session.eval.materials = materials;
                            ev.printability = Arc::new(ev.analyse(&session.eval.materials));
                            ev.add_bend_issues();
                            None
                        }
                        Err(e) => Some(e),
                    };
                    let ev = Arc::new(ev);
                    session.last = Some(ev.clone());
                    let scene = scenes.of(session, &ev, *overhangs);
                    if *xr && changed {
                        post_xr_plate(&ev, 0);
                    }
                    Cx::post_action(HostAction::Done(Box::new(Done { images: Vec::new(), id: 0, ok: error.is_none(), text: error.map(|e| format!("{{error: {}}}", fab_ai::out::quote(&e))).unwrap_or_default(), review: Mutex::new(None), update: Some(plate_update(&mut session, ev, scene)), status: status(&mut session), model_changed: changed, check_slice: None, source_files: None, source_problem: None, capped: None })));
                }
                return;
            }
            Job::Solved(solved) => {
                let Solved { file, part, doing, asked, grid, .. } = *solved;
                if !session.model.set_solved(&file, &part, asked, grid) || session.last.is_none() {
                    return;
                }
                let job = JobSpec::new(doing.clone()).machine(HERE).start();
                let built = job.enter(|| session.rebuild());
                job.finish(&built, false);
                if let Err(e) = built {
                    log!("fab: {doing}: {e}");
                    return;
                }
                if let Some(ev) = session.last.clone() {
                    let scene = scenes.of(session, &ev, *overhangs);
                    Cx::post_action(HostAction::Done(Box::new(Done { images: Vec::new(), id: 0, ok: true, text: String::new(), review: Mutex::new(None), update: Some(plate_update(&mut session, ev, scene)), status: status(&mut session), model_changed: true, check_slice: None, source_files: None, source_problem: None, capped: None })));
                }
                return;
            }
            Job::Memory { bed, memory } => {
                // a bed switched since: the plate its chips ran on is no longer here
                if bed == session.switched {
                    session.model.set_elec_memory(memory);
                }
                return;
            }
            Job::Docs { bed, plate, seq, docs, assets, bases, asked } => {
                {
                    let mut newest = docs_newest.lock().unwrap();
                    if newest.get(&bed) != Some(&seq) {
                        return;
                    }
                    newest.remove(&bed);
                }
                let waited = asked.elapsed();
                if waited > std::time::Duration::from_secs(1) {
                    log!("fab: the electronics documents waited {} ms for the plate's worker", waited.as_millis());
                }
                if bed == session.switched {
                    let landed = docs.clone();
                    match merge_files(session.model.docs(), session.model.elec_assets(), &bases, &docs, &assets) {
                        Some((all, assets)) => {
                            session.model.set_docs(Arc::new(all));
                            if let Some(assets) = assets {
                                session.model.set_elec_assets(Arc::new(assets));
                            }
                        }
                        None => {
                            let why = "the plate's electronics documents changed after the electronics engine read them";
                            match session.keep_docs_aside(plate.as_deref(), &docs, why) {
                                Ok(path) => log!("fab: the electronics documents for bed {bed} were written over a version the plate no longer has; kept aside at {}", path.display()),
                                Err(e) => log!("fab: the electronics documents for bed {bed} were written over a version the plate no longer has and could not be kept aside: {e}"),
                            }
                        }
                    }
                    Cx::post_action(HostAction::DocsLanded { bed, docs: landed });
                } else {
                    let switched = session.switched;
                    let why = format!("electronics documents for another plate (bed {bed}, now {switched}) reached the model");
                    match session.keep_docs_aside(plate.as_deref(), &docs, &why) {
                        Ok(path) => log!("fab: electronics documents for bed {bed} arrived after the bed changed (now {switched}); kept aside at {}", path.display()),
                        Err(e) => log!("fab: electronics documents for bed {bed} arrived after the bed changed and could not be kept aside: {e}"),
                    }
                }
                return;
            }
            Job::SetEnv { bed, env } => {
                if bed != session.switched {
                    log!("fab: the electronics boards for bed {bed} arrived after the bed changed (now {}); not applied", session.switched);
                    return;
                }
                session.model.set_env(env);
                if session.last.is_none() {
                    return;
                }
                let doing = "Updating the electronics boards";
                let job = JobSpec::new(doing).machine(HERE).start();
                let built = job.enter(|| session.rebuild());
                job.finish(&built, false);
                if let Err(e) = built {
                    log!("fab: {doing}: {e}");
                    return;
                }
                if let Some(ev) = session.last.clone() {
                    let scene = scenes.of(session, &ev, *overhangs);
                    Cx::post_action(HostAction::Done(Box::new(Done { images: Vec::new(), id: 0, ok: true, text: String::new(), review: Mutex::new(None), update: Some(plate_update(&mut session, ev, scene)), status: status(&mut session), model_changed: true, check_slice: None, source_files: None, source_problem: None, capped: None })));
                }
                return;
            }
            Job::Grown(shown) => {
                let newest = shown.slot.lock().unwrap().take();
                // a run that has been stopped speaks for nothing
                let current = grows.runs.lock().unwrap().get(&(shown.file.clone(), shown.part.clone())).is_some_and(|run| run.id == shown.run);
                if let (Some(newest), true) = (newest, current) {
                    show_grown(session, scenes, *overhangs, &shown.file, &shown.part, Some(newest.grid), newest.settled, newest.faded);
                }
                return;
            }
            Job::GrownSettled(settled) => {
                let current = grows.runs.lock().unwrap().get(&(settled.file.clone(), settled.part.clone())).is_some_and(|run| run.id == settled.run);
                if current {
                    if let Some(dir) = session.path.as_ref().and_then(|p| p.parent()) {
                        let path = dir.join(fab_model::grown::file_name(&settled.file, &settled.part));
                        if let Err(e) = fab_model::project::write_atomic(&path, &settled.stored.bytes) {
                            log!("fab: keeping {}'s shape: {e}", settled.part);
                        }
                    }
                    session.model.set_stored(&settled.file, &settled.part, Some(settled.stored));
                }
                return;
            }
            Job::Mapped(mapped) => {
                if let Some(ev) = session.map_colours(mapped) {
                    let scene = scenes.of(session, &ev, *overhangs);
                    if *xr {
                        post_xr_plate(&ev, 0);
                    }
                    // The parts print with other tools: the slice is out of date.
                    Cx::post_action(HostAction::Done(Box::new(Done { images: Vec::new(), id: 0, ok: true, text: String::new(), review: Mutex::new(None), update: Some(plate_update(&mut session, ev, scene)), status: status(&mut session), model_changed: true, check_slice: None, source_files: None, source_problem: None, capped: None })));
                }
                return;
            }
            Job::PatternsGrown => {
                for g in session.last.iter().flat_map(|ev| ev.paint.iter().flatten()).flat_map(|p| p.program.patterns()) {
                    if let Some(sim) = &g.sim {
                        log!("fab: a {} pattern grew {} steps on {} cells in {:.2} s ({:.0} steps a second at the end), its first state shown {:.0} ms after it was asked for", g.spec.preset.name, g.spec.steps, sim.cells(), sim.made.elapsed().as_secs_f64(), sim.rate(), sim.first_shown() * 1e3);
                    }
                }
                if let Some(ev) = session.patterns_grown() {
                    let scene = scenes.of(session, &ev, *overhangs);
                    if *xr {
                        post_xr_plate(&ev, 0);
                    }
                    // the paint now prints as it shows: the slice that waited for it goes
                    Cx::post_action(HostAction::Done(Box::new(Done { images: Vec::new(), id: 0, ok: true, text: String::new(), review: Mutex::new(None), update: Some(plate_update(&mut session, ev, scene)), status: status(&mut session), model_changed: true, check_slice: None, source_files: None, source_problem: None, capped: None })));
                }
                return;
            }
            Job::BendsMeasured(content) => {
                if let Some(ev) = session.bends_measured(content) {
                    let scene = scenes.of(session, &ev, *overhangs);
                    Cx::post_action(HostAction::Done(Box::new(Done { images: Vec::new(), id: 0, ok: true, text: String::new(), review: Mutex::new(None), update: Some(plate_update(&mut session, ev, scene)), status: status(&mut session), model_changed: false, check_slice: None, source_files: None, source_problem: None, capped: None })));
                }
                return;
            }
            Job::OverhangList { key } => {
                const LINES: usize = 8;
                let current = session.last.as_ref().zip(session.slices.as_ref()).and_then(|(ev, slices)| {
                    slices.input(ev).ok().map(|(input, _)| (input.plate_id, input.settings_id))
                });
                if current != Some(key) { return; }
                let cancelled = || ticket.is_stale() || crate::live_slice::toolpaths_of(key).is_none() || crate::live_slice::latest_toolpaths_key() != Some(key);
                if cancelled() { return; }
                match session.overhang_list(LINES, Some(&cancelled)) {
                    Ok(list) if !cancelled() => Cx::post_action(HostAction::OverhangList(Box::new((key, list)))),
                    Ok(_) => {},
                    Err(error) if error == "cancelled" => {},
                    Err(error) => log!("fab: overhang list: {error}"),
                }
                return;
            }
            Job::TestMode(on) => {
                session.test_mode = on;
                return;
            }
            Job::Overhangs(on) => {
                *overhangs = on;
                if let Some(ev) = session.last.clone() {
                    let scene = scenes.of(session, &ev, *overhangs);
                    Cx::post_action(HostAction::Done(Box::new(Done { images: Vec::new(), id: 0, ok: true, text: String::new(), review: Mutex::new(None), update: Some(plate_update(&mut session, ev, scene)), status: status(&mut session), model_changed: false, check_slice: None, source_files: None, source_problem: None, capped: None })));
                }
                return;
            }
            Job::MeshFields(on) => {
                let built = session.set_mesh_fields(on);
                if let Some(j) = running.take() {
                    match &built {
                        Some(result) => j.finish(result, false),
                        None => j.done(),
                    }
                }
                match built {
                    Some(Ok(ev)) => {
                        if *xr {
                            post_xr_plate(&ev, 0);
                        }
                        let scene = scenes.of(session, &ev, *overhangs);
                        Cx::post_action(HostAction::Shown(Box::new((ev, scene))));
                    }
                    Some(Err(e)) => log!("fab: meshing the field parts: {e}"),
                    None => {}
                }
                return;
            }
            Job::ExactDetail(chord) => {
                let built = session.set_exact_detail(chord);
                if let Some(j) = running.take() {
                    match &built {
                        Some(result) => j.finish(result, false),
                        None => j.done(),
                    }
                }
                match built {
                    Some(Ok(ev)) => {
                        if *xr {
                            post_xr_plate(&ev, 0);
                        }
                        let scene = scenes.of(session, &ev, *overhangs);
                        Cx::post_action(HostAction::Shown(Box::new((ev, scene))));
                    }
                    Some(Err(e)) => log!("fab: refining the exact parts: {e}"),
                    None => {}
                }
                return;
            }
            Job::Call { id, tool, args } if tool == "board" => {
                let docs = session.model.docs().clone();
                let assets = session.model.elec_assets().clone();
                let running = running.take();
                let read = move || {
                    let answer = crate::elec_host::board_answer(&docs, &assets,
                        args.get("board").and_then(Value::as_str), args.get("only").and_then(Value::as_str));
                    if let Some(j) = running {
                        match &answer {
                            Ok(_) => j.done(),
                            Err(_) if j.is_cancelled() => j.cancelled(),
                            Err(e) => j.fail(job_reason(e), false),
                        }
                    }
                    let (ok, text) = match answer {
                        Ok(text) => (true, text),
                        Err(e) => (false, format!("{{error: {}}}", fab_ai::out::quote(&e))),
                    };
                    Cx::post_action(HostAction::Answered { id, ok, text });
                };
                if let Err(e) = pool.submit_named(Class::Background, "fab board read", read).map(|task| task.detach()) {
                    let text = format!("{{error: {}}}", fab_ai::out::quote(&format!("the board read could not start: {e}")));
                    Cx::post_action(HostAction::Answered { id, ok: false, text });
                }
                return;
            }
            Job::Call { id, tool, args } if tool == "align" => {
                // Read only, off this thread: the boss search and the fit run on the pool with the
                // plate as built and the scripts as they stand.
                let docs = session.model.docs().clone();
                let assets = session.model.elec_assets().clone();
                let Some(ev) = session.last.clone() else {
                    let text = format!("{{error: {}}}", fab_ai::out::quote("the plate has not built yet"));
                    Cx::post_action(HostAction::Answered { id, ok: false, text });
                    return;
                };
                let scripts: Vec<(String, String, String)> = session.model.project().scripts.iter()
                    .filter_map(|s| Some((s.name.clone(), s.file.clone(), session.model.source(&s.file)?.to_string())))
                    .collect();
                let running = running.take();
                let read = move || {
                    let script_of = |instance: &str| scripts.iter().find(|s| s.0 == instance).map(|s| (s.1.clone(), s.2.clone()));
                    let answer = crate::elec_host::align_answer(&docs, &assets, &ev, &script_of,
                        args.get("board").and_then(Value::as_str), args.get("part").and_then(Value::as_str));
                    if let Some(j) = running {
                        match &answer {
                            Ok(_) => j.done(),
                            Err(_) if j.is_cancelled() => j.cancelled(),
                            Err(e) => j.fail(job_reason(e), false),
                        }
                    }
                    let (ok, text) = match answer {
                        Ok(text) => (true, text),
                        Err(e) => (false, format!("{{error: {}}}", fab_ai::out::quote(&e))),
                    };
                    Cx::post_action(HostAction::Answered { id, ok, text });
                };
                if let Err(e) = pool.submit_named(Class::Background, "fab align", read).map(|task| task.detach()) {
                    let text = format!("{{error: {}}}", fab_ai::out::quote(&format!("the align could not start: {e}")));
                    Cx::post_action(HostAction::Answered { id, ok: false, text });
                }
                return;
            }
            Job::Call { id, tool, args } => (id, session.call(&tool, &args, None)),
            Job::ServeXr => {
                *xr = true;
                if let Some(ev) = &session.last {
                    post_xr_plate(ev, 0);
                }
                return;
            }
            Job::Preview { gen, req, asked, stops, since } => {
                let started = std::time::Instant::now();
                let (key, mut line) = match &req {
                    PreviewReq::Param { key, .. } => (key.clone(), None),
                    PreviewReq::Source { .. } => ("source".to_string(), None),
                    PreviewReq::Drop { .. } => (DROP_PREVIEW.to_string(), None),
                };
                // A commit or a cancel stops the build at its next check (what it built so far
                // stays for the next), and so does a newer value of a parameter that builds no
                // exact body: each newer value is compiled once to know, on this thread as it waits.
                let building = match &req {
                    PreviewReq::Param { key, value } => Some((key.as_str(), value)),
                    PreviewReq::Source { .. } | PreviewReq::Drop { .. } => None,
                };
                let built_value = building.map(|(_, v)| v.clone());
                let logged = std::env::var_os("FAB_PARAM_LOG").is_some();
                if let (Some((key, value)), true) = (building, logged) {
                    log!("fab param building {key} = {value:?} gen {gen}");
                }
                let stopped = std::cell::Cell::new(false);
                let checked = std::cell::Cell::new(gen);
                let stop = || {
                    if !stopped.get() && stops.stopped(since, building) {
                        stopped.set(true);
                    }
                    stopped.get()
                };
                let stop_param = |known: &dyn Fn(&str, &PValue) -> bool| {
                    if !stop() {
                        if let Some((newer, key, value)) = building.and_then(|b| stops.newer(checked.get(), b)) {
                            checked.set(newer);
                            stopped.set(known(&key, &value));
                        }
                    }
                    stopped.get()
                };
                let built = match &req {
                    PreviewReq::Param { key, value } => session.preview_param(key, value, Some(&stop_param)),
                    PreviewReq::Source { file, text } => match session.preview_source(file, text, Some(&stop)) {
                        Ok(Some(b)) => Ok(b),
                        // The text is the program: nothing to show but the plate as it is.
                        Ok(None) => Err(String::new()),
                        Err(p) => {
                            line = p.line;
                            Err(p.message)
                        }
                    },
                    PreviewReq::Drop { paths } => session.preview_script_files(paths, Some(&stop)),
                };
                // One that failed once stopped says nothing of its value: the stop may be why, and
                // what stopped it answers for itself.
                if built.is_err() && stopped.get() {
                    if logged {
                        log!("fab param stopped {key} gen {gen}");
                    }
                    return;
                }
                let mut stages = [0.0f64; 4];
                let ms = |t: std::time::Instant| t.elapsed().as_secs_f64() * 1000.0;
                let result = built.map(|(c, ev, model)| {
                    stages[0] = ms(started);
                    let t = std::time::Instant::now();
                    if *xr {
                        post_xr_plate(&ev, 0);
                    }
                    stages[1] = ms(t);
                    let t = std::time::Instant::now();
                    let scene = scenes.of(session, &ev, *overhangs);
                    stages[2] = ms(t);
                    let t = std::time::Instant::now();
                    let markers = fab::print::issue_markers(&ev.printability);
                    let params = crate::param_view::views(&model, &ev);
                    stages[3] = ms(t);
                    let test_prints = project_test_prints(&session, &ev);
                    (ev, scene, markers, params, Some((c, model)), project_measurements(&session), test_prints)
                });
                if logged {
                    let eval = result.as_ref().map(|r| r.0.millis).unwrap_or(0.0);
                    log!("fab param preview {key} gen {gen}: {:.1} ms (eval {eval:.0} + params {:.0}, xr {:.0}, scene {:.0}, views {:.0}){}", ms(started), stages[0] - eval, stages[1], stages[2], stages[3], result.as_ref().err().map(|e| format!(" ({e})")).unwrap_or_default());
                }
                if let Ok(update) = &result {
                    crate::live_slice::model_built(crate::live_slice::plate_id(&update.0), asked, ms(started));
                }
                Cx::post_action(HostAction::Preview(Box::new(PreviewDone { gen, key, value: built_value, result, line })));
                return;
            }
            Job::History { id, undo } => {
                let action = if undo { fab_model::HistoryAction::Undo } else { fab_model::HistoryAction::Redo };
                // An empty history is not a failure: nothing happens, nothing is said.
                let available = if undo { session.model.can_undo() } else { session.model.can_redo() };
                let reply = if available {
                    session.undo_redo(action)
                } else {
                    fab_ai::Reply { images: Vec::new(), ok: true, text: "{}".into(), update: None, render: None }
                };
                log!("fab: {}: {}", if undo { "undo" } else { "redo" }, if reply.ok { "done" } else { reply.text.as_str() });
                (id, reply)
            }
            Job::Source { id, file, text, param } => {
                let (reply, problem) = session.set_source_with(&file, &text, param.as_ref().map(|(k, v)| (k.as_str(), v)));
                source_problem = problem;
                (id, reply)
            }
            Job::Param { id, key, value, fallback, asked } => {
                let started = std::time::Instant::now();
                let mut reply = session.set_param(&key, &value);
                // a lost selector is no cap: its draft shows and a pick names the selector again
                if !reply.ok && !crate::picks::lost_failure(&reply.text) {
                    if let Some((good, decimals)) = fallback {
                        let mut why = reason(&reply.text);
                        let best = if let (PValue::Num(lo), PValue::Num(hi)) = (&good, &value) {
                            let (mut lo, mut hi) = (*lo, *hi);
                            let scale = 10.0_f64.powi(decimals as i32);
                            for _ in 0..24 {
                                let mid = (((lo + hi) / 2.0) * scale).round() / scale;
                                if !((lo < mid && mid < hi) || (hi < mid && mid < lo)) { break; }
                                match session.preview_param(&key, &PValue::Num(mid), None) {
                                    Ok(_) => lo = mid,
                                    Err(e) => { hi = mid; why = e; }
                                }
                            }
                            PValue::Num(lo)
                        } else {
                            good
                        };
                        let again = session.set_param(&key, &best);
                        if again.ok {
                            capped = Some((best, why));
                            reply = again;
                        }
                    }
                }
                if let Some((_, ev)) = &reply.update {
                    crate::live_slice::model_built(crate::live_slice::plate_id(ev), asked, started.elapsed().as_secs_f64() * 1000.0);
                }
                let eval = reply.update.as_ref().map_or(0.0, |e| e.1.millis);
                log!("fab: parameter {key}: {} in {:.0} ms (evaluation {eval:.0} ms){}", if reply.ok { "committed" } else { reply.text.as_str() }, started.elapsed().as_secs_f64() * 1000.0, capped.as_ref().map(|(value, _)| format!(" (capped at {value:?})")).unwrap_or_default());
                (id, reply)
            }
            Job::Edit { id, edits } => {
                let reply = session.commit("edit", edits);
                log!("fab: edit: {}", if reply.ok { "committed" } else { reply.text.as_str() });
                (id, reply)
            }
            Job::DeleteParts { id, objects, scripts } => {
                let edits = session.compiled.as_ref().ok_or_else(|| "No plate is ready to edit".to_string())
                    .and_then(|c| crate::plate_edit::delete_edits(c, &objects, scripts));
                let reply = match edits {
                    Ok(edits) => session.commit("delete parts", edits),
                    Err(error) => fab_ai::Reply { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(&error)), images: Vec::new(), update: None, render: None },
                };
                (id, reply)
            }
            Job::TestPrints { id, edits } => {
                let changes = session.last.as_ref().ok_or_else(|| "No plate is ready to edit".to_string())
                    .and_then(|ev| ev.test_print_changes(session.model.project(), &edits));
                let reply = match changes {
                    Ok(changes) => session.commit("test print", changes),
                    Err(error) => fab_ai::Reply { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(&error)), images: Vec::new(), update: None, render: None },
                };
                (id, reply)
            }
            Job::LayTest { id, name, from, dir } => {
                let changes = session.lay_by_ray(name.as_deref(), from, dir)
                    .and_then(|edit| session.last.as_ref().ok_or_else(|| "No plate is ready to edit".to_string()).and_then(|ev| ev.test_print_changes(session.model.project(), &[edit])));
                let reply = match changes {
                    Ok(changes) => session.commit("test print", changes),
                    Err(error) => fab_ai::Reply { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(&error)), images: Vec::new(), update: None, render: None },
                };
                (id, reply)
            }
            Job::PaintEdit { id, action, frame } => {
                let edits = match (&session.compiled, &session.last) {
                    (Some(c), Some(ev)) => crate::paint_edit::edits(c, ev, &action, frame.as_ref()),
                    _ => Err("No plate is ready to paint".into()),
                };
                let reply = match edits {
                    Ok(edits) => session.commit("paint", edits),
                    Err(error) => fab_ai::Reply { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(&error)), images: Vec::new(), update: None, render: None },
                };
                (id, reply)
            }
            Job::ImportMesh { id, made, render } => {
                let made = *made;
                let stages = fab_ai::out::arr(made.stages.iter().map(|(stage, s)| fab_ai::out::obj(vec![("stage", fab_ai::out::s(stage)), ("seconds", fab_ai::out::n(*s))])));
                let about = vec![("made_on", fab_ai::out::s(&made.node)), ("model", fab_ai::out::s(&made.from.model)), ("stages", stages)];
                let mut reply = session.import_mesh(&made.name, &made.fitted, &made.from, made.printable.as_ref(), about);
                if !render {
                    reply.render = None;
                }
                (id, reply)
            }
            Job::MakePrintable { id, object, opts, render } => {
                let mut reply = session.make_printable(&object, opts);
                if !render { reply.render = None; }
                (id, reply)
            }
            Job::Place { id, moves } => {
                let reply = session.place_parts(&moves);
                log!("fab: placed {} part(s): {}", moves.len(), if reply.ok { "committed" } else { reply.text.as_str() });
                (id, reply)
            }
            Job::EncodeSheets { id, frames, made } => {
                let result = review::encode_sheets(&frames);
                let made = if made && session.made_last.is_some() && result.is_ok() {
                    let mut rgba = frames.frames[0].clone();
                    if !cfg!(target_arch = "wasm32") {
                        for pixel in rgba.chunks_exact_mut(4) {
                            pixel.swap(0, 2);
                        }
                    }
                    match fab_ai::plates::thumbnail_png(frames.side as u32, frames.side as u32, &rgba) {
                        Ok(png) => session.set_made_thumbnail(&png),
                        Err(e) => {
                            log!("fab: the made part's picture could not be made: {e}");
                            None
                        }
                    }
                } else { None };
                Cx::post_action(HostAction::Sheets { id, result, made });
                return;
            }
            // taken by `run`
            Job::Painted(_) | Job::SaveView { .. } | Job::Quit { .. } => return,
            Job::Thumbnail(slot) => {
                let Some(ThumbnailFrame { width, height, rgba, demo }) = slot.lock().unwrap().take() else { return };
                match fab_ai::plates::thumbnail_png(width, height, &rgba) {
                    Ok(png) => {
                        // a demo as it ships: its card's picture
                        if let (Some(key), Some(store)) = (demo.filter(|k| session.demo_key() == Some(*k)), session.store.as_ref()) {
                            let picture = store.demo_picture(key);
                            // the view's own picture: no painted one's content goes with it
                            let _ = std::fs::remove_file(fab_ai::plates::painted_content(&picture));
                            // a cache: never waits on the disk ([`fab_ai::plates::has_picture`])
                            match fab_model::project::write_unflushed(&picture, &png) {
                                Ok(()) => session.plates_rev += 1,
                                Err(e) => log!("fab: the demo's picture could not be saved: {e}"),
                            }
                        }
                        session.set_thumbnail(Arc::from(png));
                    }
                    Err(e) => log!("fab: the viewport's picture could not be made: {e}"),
                }
                return;
            }
            Job::Demo { id, demo } => (id, match demo.model() {
                Ok(model) => {
                    let put = session.open_demo(model, demo.key);
                    on_bed(&mut session, demo.key, put)
                }
                Err(e) => failed(e),
            }),
            Job::Reload { id } => {
                let put = session.reload();
                let what = session.model.name.clone();
                (id, on_bed(&mut session, &what, put))
            }
            Job::DeletePlate { id, plate } => {
                let trash_home = makepad_fab_settings::home::user_home();
                let trash = |path: &std::path::Path| fab_ai::plates::move_to_trash(&trash_home, path);
                (id, match session.delete_plate(&plate, &trash) {
                    Ok(Some(ev)) => {
                        let what = session.model.name.clone();
                        on_bed(&mut session, &what, Ok(ev))
                    }
                    Ok(None) => fab_ai::Reply { images: Vec::new(), ok: true, text: "{deleted: true}".into(), update: None, render: None },
                    Err(e) => failed(e),
                })
            }
            Job::AddScripts { id, paths } => {
                let reply = session.add_script_files(&paths);
                let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
                log!("fab: placed {}: {}", names.join(", "), if reply.ok { "committed" } else { reply.text.as_str() });
                (id, reply)
            }
            Job::ImportFile { id, path, new_plate } => (id, session.import_file(&path, new_plate)),
            Job::MakeCalibration { step, setup, colour } => {
                let made = fab_calibrate::make(step, &setup.coupon_setup())
                    .ok_or_else(|| format!("{} has no test part", step.title())).map(|mut c| {
                        (c.printer, c.filament) = setup.calibration_names();
                        (Arc::new(c), colour)
                    });
                Cx::post_action(crate::calibrate::CalEvent::Made(made));
                return;
            }
            Job::Calibration { id, coupon, colour } => (id, session.calibration(&coupon, colour)),
            Job::Open { id, path } => {
                let put = session.open(&path);
                (id, on_bed(&mut session, &path.display().to_string(), put))
            }
        };
        if *xr {
            if let Some((_, ev)) = &reply.update {
                post_xr_plate(ev, id);
            }
        }
        if let Some(RenderRequest::Verdicts(check)) = &reply.render {
            check_verdicts(pool, id, check.clone(), running.take());
            return;
        }
        if let Some(RenderRequest::WeakSpots(find)) = &reply.render {
            let find = find.clone();
            off_worker(pool, id, running.take(), move |stop| find.answer(stop));
            return;
        }
        if let Some(RenderRequest::Tubes(find)) = &reply.render {
            let find = find.clone();
            off_worker(pool, id, running.take(), move |stop| find.answer(stop));
            return;
        }
        if let Some(RenderRequest::StrengthPlan(find)) = &reply.render {
            let find = find.clone();
            off_worker(pool, id, running.take(), move |stop| find.answer(stop));
            return;
        }
        let t = std::time::Instant::now();
        let view = running.as_ref().filter(|_| load && reply.update.is_some()).map(|j| j.reporter().part(LOAD_BUILT, 1.0, "preparing the view"));
        let update = reply.update.map(|(_, ev)| {
            // the scene, most of it, in the scene build's own words
            let scene_part = view.as_ref().map(|v| v.part(0.0, 0.9, "the plate's scene"));
            let scene = scenes.of_with(session, &ev, *overhangs, &mut |stage, f| {
                if let Some(p) = &scene_part {
                    p.stage(scene_words(stage), None, Some(f));
                }
            });
            if let Some(v) = &view {
                v.stage("its parameters", None, Some(0.9));
            }
            plate_update(&mut session, ev, scene)
        });
        if let Some(v) = &view {
            v.stage("its files", None, Some(0.97));
        }
        if update.is_some() && std::env::var_os("FAB_PARAM_LOG").is_some() {
            log!("fab: plate scene + parameters for job {id}: {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
        }
        let review = match &reply.render {
            Some(RenderRequest::Sheets { views, objects, extra, camera, marks }) => Some(session.review_plate().and_then(|ev| review::prepare(&ev, views, objects, extra.as_deref(), camera, marks)).map(Review::Sheets)),
            Some(RenderRequest::Look(look)) => Some(review::prepare_look(look).map(Review::Sheets)),
            Some(RenderRequest::Pieces(job)) => Some(review::prepare(&job.ev, &job.views, &[], job.extra.as_deref(), &job.camera, &[]).map(Review::Sheets)),
            Some(RenderRequest::Compare(job)) => Some(review::prepare(&job.ev, &job.views, &job.objects, None, &job.camera, &[]).map(|prepared| Review::Compare(prepared, job.clone()))),
            Some(RenderRequest::Variants(job)) => Some(crate::variant_sheet::first(job).map(|prepared| Review::Variants(prepared, job.clone()))),
            _ => None,
        };
        let check_slice = if matches!(reply.render, Some(RenderRequest::CheckSlice)) { session.last.clone() } else { None };
        let source_files = (update.is_some() || session.model.name != previous_name).then(|| Arc::new(session.source_files()));
        if let Some(j) = running.take() {
            if reply.ok {
                j.done();
            } else if j.is_cancelled() {
                j.cancelled();
            } else if let Some((kind, what)) = crate::picks::lost_reply(&reply.text) {
                // a statement lost its selector: nothing failed, its draft shows and waits for a
                // pick (the strip says so; a drag's line goes)
                let job_id = j.reporter().id();
                j.reporter().set_title(format!("Not committed, a draft: {what}"));
                j.done_with_next(format!("Pick the {kind}"));
                if is_param {
                    jobs::board().dismiss(job_id);
                }
            } else {
                let retry = can_retry && retryable_text(&reply.text);
                let job_id = j.reporter().id();
                j.fail(job_reason(&reply.text), retry);
                if is_param {
                    jobs::board().dismiss(job_id);
                }
            }
        }
        let done = Done { images: reply.images, id, ok: reply.ok, text: reply.text, review: Mutex::new(review), update, status: status_after_job(previous_switch, &mut session), model_changed: !measurement_only, check_slice, source_files, source_problem, capped };
        // The list before the answer: what is on the bed now (a switch's plate) is the app's
        // before whoever asked acts on the answer. A click on the plate the switch left found
        // the list still naming it on the bed, and was dropped as a click on that plate.
        send_plates(session, plates_seen, list_reads);
        Cx::post_action(HostAction::Done(Box::new(done)));
    }
}

/// Sends the app the plate list if it changed since the one sent last.
fn send_plates(session: &Session, plates_seen: &mut Option<u64>, list_reads: &mut ListReads) {
    if *plates_seen == Some(session.plates_rev) {
        return;
    }
    *plates_seen = Some(session.plates_rev);
    Cx::post_action(HostAction::Plates(Mutex::new(Some(plate_list(session, list_reads)))));
}

/// The plate builds every host has queued, running or not yet taken ([`ModelHost::building`]),
/// for the remote's `settled`: a build is work whose result changes the frame, so `settled` stays
/// false (`"loads":{"build":n}`) until the app has taken the plate it makes. The platform counts
/// an app pool's queued jobs only, not its running ones, so a plate building for 30 s read as
/// settled, and an agent that waits for `settled` took a look of the plate before (11-16).
static BUILDS: AtomicUsize = AtomicUsize::new(0);

/// The `build` kind of the app's asynchronous loads ([`Cx::register_async_load_probe`]).
pub fn builds_in_flight(_: &Cx) -> usize {
    BUILDS.load(Ordering::Acquire)
}

impl Drop for ModelHost {
    /// A host dropped with builds still counted (a new host replaced it) takes them out.
    fn drop(&mut self) {
        BUILDS.fetch_sub(self.building, Ordering::AcqRel);
    }
}

impl ModelHost {
    /// Remote/test sessions leave the user's crash copy alone.
    pub fn start(cx: &Cx, persist: bool) -> ModelHost {
        load_hold::register();
        let steps = Arc::new(AtomicU64::new(0));
        let logged = Arc::new(AtomicU64::new(0));
        let grow_runs = Arc::<crate::grow_live::Runs>::default();
        let words = Arc::new(Mutex::new(WorkerWords { doing: Some("the model to start".to_string()), queued: Vec::new() }));
        // (a run that keeps no plates brings nothing back)
        let recovery = persist.then(|| JobSpec::new("the plate left on the bed").machine(HERE).shown_elsewhere().start());
        let recovery_job = recovery.as_ref().map(|j| j.id());
        let quitting = Arc::new(AtomicBool::new(false));
        let saved = Arc::new(AtomicBool::new(false));
        let docs_newest = Arc::new(Mutex::new(HashMap::new()));
        let replacing = Arc::new(AtomicUsize::new(0));
        let switched_now = Arc::new(AtomicU64::new(0));
        let state = HostState { steps: steps.clone(), logged: logged.clone(), words: words.clone(), docs_newest: docs_newest.clone(), replacing: replacing.clone(), switched_now: switched_now.clone(), session: None, xr: false, overhangs: false, scenes: Scenes::on(cx.task_pool()), plates_seen: None, list_reads: ListReads::default(),
            painting: 0, painting_run: None, paint_seen: Default::default(), paint_again: Default::default(), painting_alone: false, quitting: quitting.clone(), saved: saved.clone(), persist, retry_after: std::time::Instant::now(),
            pool: cx.task_pool(), stress_seen: 0, volumes_seen: std::sync::Weak::new(), veins_seen: 0, veins_asked: Default::default(), grows: grow_runs.clone(), grow_seen: 0, grow_next: 0, hub: Default::default(), recovery };
        let strand = Strand::with_state(&cx.task_pool(), "fab model", state);
        strand.submit(Class::Visible, Ticket::none(), |state| {
            state.init();
            state.note_saved();
            state.words.lock().unwrap().set(None, None);
            state.switched_now.store(state.session.as_ref().map_or(0, |s| s.switched), Ordering::Release);
        }).detach();
        ModelHost { steps, logged, words, grow_runs, strand, setup_version: Version::new(), overhang_list_version: Version::new(), retry: HashMap::new(), call_jobs: HashMap::new(), build_jobs: Vec::new(), next_id: 1, xr: false, preview_version: Version::new(), preview_stops: Arc::default(), preview_gen: 0, thumbnail: ThumbnailSlot::default(), shape_held: Arc::new(AtomicUsize::new(0)), building: 0, load: None, recovery: recovery_job, quitting,
            keeping: Arc::new(AtomicUsize::new(0)), saved, persist, pool: cx.task_pool(), docs_newest, docs_seq: 0, held: Vec::new(), barrier: false, replacing, switched_now }
    }

    /// Numbers come from this shared counter; a reservation takes one early.
    pub fn steps(&self) -> Arc<AtomicU64> {
        self.steps.clone()
    }

    /// The newest entry logged by either history.
    pub fn logged(&self) -> Arc<AtomicU64> {
        self.logged.clone()
    }

    /// A parameter's live value (a handle or a control dragged): only the newest is evaluated.
    pub fn preview_param(&mut self, key: String, value: PValue) -> u64 {
        self.preview_gen += 1;
        *self.preview_stops.newest.lock().unwrap() = Some((self.preview_gen, key.clone(), value.clone()));
        self.queue(Job::Preview { gen: self.preview_gen, req: PreviewReq::Param { key, value }, asked: std::time::Instant::now(), stops: self.preview_stops.clone(), since: self.preview_stops.count.load(Ordering::Acquire) });
        self.preview_gen
    }

    /// Live project, script or paint text: only the newest is evaluated.
    pub fn preview_source(&mut self, file: String, text: String) -> u64 {
        self.preview_gen += 1;
        *self.preview_stops.newest.lock().unwrap() = None;
        self.queue(Job::Preview { gen: self.preview_gen, req: PreviewReq::Source { file, text }, asked: std::time::Instant::now(), stops: self.preview_stops.clone(), since: self.preview_stops.count.load(Ordering::Acquire) });
        self.preview_gen
    }

    /// Object script files dragged over the window: the plate as their drop would leave it. Only
    /// the newest is evaluated.
    pub fn preview_drop(&mut self, paths: Vec<PathBuf>) -> u64 {
        self.preview_gen += 1;
        *self.preview_stops.newest.lock().unwrap() = None;
        self.queue(Job::Preview { gen: self.preview_gen, req: PreviewReq::Drop { paths }, asked: std::time::Instant::now(), stops: self.preview_stops.clone(), since: self.preview_stops.count.load(Ordering::Acquire) });
        self.preview_gen
    }

    /// Undo or redo one step at the document's head when the worker gets to it (no stale head);
    /// a live preview is dropped first.
    pub fn history(&mut self, undo: bool) -> u64 {
        self.cancel_preview();
        let id = self.reserve_call_id();
        self.queue(Job::History { id, undo });
        id
    }

    /// Commit the code panel's text of `file` as one undo step; the answer comes back
    /// as `HostAction::Done`.
    pub fn set_source(&mut self, file: String, text: String) -> u64 {
        self.cancel_preview();
        let id = self.reserve_call_id();
        self.queue(Job::Source { id, file, text, param: None });
        id
    }

    /// A script's text and one parameter's value as one undo step: a selector picked again on
    /// the draft that value built.
    pub fn set_source_param(&mut self, file: String, text: String, key: String, value: PValue) -> u64 {
        self.cancel_preview();
        let id = self.reserve_call_id();
        self.queue(Job::Source { id, file, text, param: Some((key, value)) });
        id
    }

    /// Whether a job that changes the shape is queued or running ([`SliceHold`]).
    pub fn shaping(&self) -> bool {
        self.shape_held.load(Ordering::Acquire) > 0
    }

    /// Whether the plate is being built again: a job that builds it is queued or running, or the
    /// app has not taken its end (its plate, if it made one, comes first). Counted on the UI
    /// thread, from the queue to the app's taking of its [`HostAction::ShapeJobOver`].
    pub fn building(&self) -> bool {
        self.building > 0
    }

    /// The app took the end of a job that builds the plate.
    pub fn build_over(&mut self) {
        self.building -= 1;
        BUILDS.fetch_sub(1, Ordering::AcqRel);
    }

    /// Drop any preview not yet evaluated and stop the one building; returns the generation a
    /// later result must beat.
    pub fn cancel_preview(&mut self) -> u64 {
        self.stop_previews(None)
    }

    pub fn save_view(&mut self, plate: String, view: PlateView) {
        self.queue(Job::SaveView { plate, view });
    }

    /// Quit once the plate's edits are saved, never after work nobody waits on: queued, that work
    /// never starts (previews and the print setup and overhang list reads by their tickets, the
    /// pictures and the view's refinements by [`left_at_quit`]), and no picture starts. With no
    /// job queued or running that keeps something the person made or asked for (an edit, a load,
    /// the electronics' documents, a view: [`Self::keeping`]), none held for the electronics
    /// host, and the working copy holding every edit taken ([`Self::saved`]), the plate's `view`
    /// is written on the pool and [`HostAction::Quit`] answers with the id returned, whatever the
    /// worker runs: what it runs keeps nothing. Else the view and a last save of the working copy
    /// go behind the jobs that keep something, and that save answers. Nothing waits on the UI
    /// thread. Cost: a few atomic loads, and one pool job or two queued.
    pub fn quit(&mut self, view: Option<(String, PlateView)>) -> u64 {
        let asked = std::time::Instant::now();
        self.quitting.store(true, Ordering::Release);
        // a quit never waits on a hold on loads
        load_hold::HOLD.set(false);
        self.cancel_preview();
        self.setup_version.bump();
        self.overhang_list_version.bump();
        let id = self.reserve_call_id();
        let doing = self.words.lock().unwrap().doing.clone();
        let keeping = self.keeping.load(Ordering::Acquire);
        if keeping == 0 && self.held.is_empty() && !self.barrier && self.saved.load(Ordering::Acquire) {
            let store = self.persist.then(plate_store);
            let written = view.clone();
            let leave = move || {
                if let (Some(store), Some((plate, view))) = (&store, &written) {
                    write_view(store, plate, view);
                }
                let left = doing.map_or(String::new(), |doing| format!("; the worker's job ({doing}) keeps nothing and is left"));
                log!("fab: quitting {:.0} ms after it was asked, the plate's edits saved before it{left}", asked.elapsed().as_secs_f64() * 1e3);
                Cx::post_action(HostAction::Quit { id });
            };
            match self.pool.submit_named(Class::Interactive, "fab quit", leave) {
                Ok(task) => {
                    task.detach();
                    return id;
                }
                Err(e) => log!("fab: the quit's view could not go to the pool ({e:?}); it goes behind the worker's jobs"),
            }
        } else {
            let doing = doing.map_or_else(|| "is idle".to_string(), |doing| format!("runs {doing}"));
            log!("fab: quit asked while the worker {doing}: it goes behind {keeping} job(s) that keep the person's work{}", if self.held.is_empty() && !self.barrier { String::new() } else { format!(" and the electronics host's last documents ({} job(s) held)", self.held.len()) });
        }
        if let Some((plate, view)) = view {
            self.queue(Job::SaveView { plate, view });
        }
        self.queue(Job::Quit { id, asked });
        id
    }

    /// A commit of parameter `key` at `value`: as [`Self::cancel_preview`], but a preview already
    /// building that value goes on (the commit reads its bodies; its result is older than the
    /// commit's).
    pub fn commit_preview(&mut self, key: &str, value: &PValue) -> u64 {
        self.stop_previews(Some((key.to_string(), value.clone())))
    }

    fn stop_previews(&mut self, spares: Option<(String, PValue)>) -> u64 {
        self.preview_version.bump();
        *self.preview_stops.newest.lock().unwrap() = None;
        *self.preview_stops.spares.lock().unwrap() = spares;
        self.preview_stops.count.fetch_add(1, Ordering::AcqRel);
        self.preview_gen += 1;
        self.preview_gen
    }

    /// Commit structured changes as one undo step; the answer comes back as `HostAction::Done`.
    pub fn edit(&mut self, edits: Vec<fab_model::Change>) -> u64 {
        let id = self.reserve_call_id();
        self.queue(Job::Edit { id, edits });
        id
    }

    pub fn delete_parts(&mut self, objects: Vec<String>, scripts: bool) -> u64 {
        self.cancel_preview();
        let id = self.reserve_call_id();
        self.queue(Job::DeleteParts { id, objects, scripts });
        id
    }

    pub fn test_prints(&mut self, edits: Vec<fab_model::project::TestEdit>) -> u64 {
        let id = self.reserve_call_id();
        self.queue(Job::TestPrints { id, edits });
        id
    }

    pub fn lay_test(&mut self, name: Option<String>, from: [f64; 3], dir: [f64; 3]) -> u64 {
        let id = self.reserve_call_id();
        self.queue(Job::LayTest { id, name, from, dir });
        id
    }

    pub fn paint_edit(&mut self, action: fab::api::PartAction, frame: Option<[[f64; 4]; 3]>) -> u64 {
        let id = self.reserve_call_id();
        self.queue(Job::PaintEdit { id, action, frame });
        id
    }

    /// Set a parameter as one undo step; the answer comes back as `HostAction::Done`.
    pub fn set_param(&mut self, key: &str, value: PValue, fallback: Option<(PValue, usize)>) -> u64 {
        let id = self.reserve_call_id();
        self.queue(Job::Param { id, key: key.to_string(), value, fallback, asked: std::time::Instant::now() });
        id
    }

    pub fn overhang_list(&mut self, key: (u64, u64)) {
        self.queue(Job::OverhangList { key });
    }

    fn queue(&mut self, job: Job) {
        let title = job_title(&job);
        self.queue_titled(job, title);
    }

    fn queue_titled(&mut self, job: Job, title: Option<String>) {
        self.queue_inner(job, title, false);
    }

    fn queue_inner(&mut self, job: Job, title: Option<String>, counted: bool) {
        if replaces_document(&job) {
            if !counted {
                self.replacing.fetch_add(1, Ordering::AcqRel);
            }
            if self.barrier {
                self.held.push((job, title));
                return;
            }
        }
        let load = loads(&job);
        let loading = load.is_some();
        // a load's steps and cancel are the view's progress card's, not the job strip line's
        let handle = title.map(|title| {
            let spec = JobSpec::new(title).machine(HERE);
            // (a load held for a test or an agent looks again at its cancel)
            (if loading { spec.shown_elsewhere().on_cancel(|| load_hold::HOLD.wake()) } else { spec }).start()
        });
        if let (Some(of), Some(h), Some(call)) = (load, &handle, job_call_id(&job)) {
            // a load still on its way gives way to this one: the bed gets the newest ask (and so
            // does the build of what the last run left on the bed)
            if let Some(old) = self.load.take() {
                jobs::board().cancel(old.job);
            }
            if let Some(recovery) = self.recovery.take() {
                jobs::board().cancel(recovery);
            }
            self.load = Some(Load { call, job: h.id(), of });
            crate::live_slice::loading(call);
        }
        if let Some(h) = &handle {
            if let Some(copy) = retry_copy(&job) {
                self.retry.insert(h.id(), copy);
            }
            if let Some(id) = job_call_id(&job) {
                self.call_jobs.insert(id, h.id());
            }
            // (a load is not the plate on the bed rebuilding: its card says how it goes)
            if builds_model(&job) && !loading {
                self.build_jobs.push(h.id());
            }
        }
        // Jobs the board no longer holds (out of its history) take their retries with them.
        let held: Vec<JobId> = jobs::snapshot().iter().map(|j| j.id).collect();
        self.retry.retain(|id, _| held.contains(id));
        self.call_jobs.retain(|_, id| held.contains(id));
        self.build_jobs.retain(|id| held.contains(id));
        self.queue_with(job, handle);
    }

    fn queue_with(&mut self, job: Job, handle: Option<JobHandle>) {
        if let Some(h) = &handle {
            let mut words = self.words.lock().unwrap();
            h.stage(words.waiting(), None, None);
            words.queued.push(h.reporter());
        }
        let built = builds_model(&job);
        self.building += usize::from(built);
        BUILDS.fetch_add(usize::from(built), Ordering::AcqRel);
        let hold = shapes_model(&job).then(|| SliceHold::take(&self.shape_held, built));
        let class = class_of(&job);
        let ticket = match &job {
            Job::Preview { .. } => self.preview_version.next(),
            Job::PrintSetup(_) => self.setup_version.next(),
            Job::OverhangList { .. } => self.overhang_list_version.next(),
            _ => Ticket::none(),
        };
        let quitting = left_at_quit(&job).then(|| self.quitting.clone());
        let keeping = (!left_at_quit(&job) && !stale_at_quit(&job)).then(|| {
            self.keeping.fetch_add(1, Ordering::AcqRel);
            Keeping(self.keeping.clone())
        });
        let replacing = replaces_document(&job);
        self.strand.submit(class, ticket.clone(), move |state| {
            let _hold = hold;
            // (ends after the job's save: a quit that reads none kept reads the plate saved)
            let _keeping = keeping;
            if quitting.is_some_and(|q| q.load(Ordering::Acquire)) {
                if let Some(h) = handle {
                    h.cancelled();
                }
            } else {
                state.run((job, handle), ticket);
            }
            state.switched_now.store(state.session.as_ref().map_or(0, |s| s.switched), Ordering::Release);
            if replacing {
                state.replacing.fetch_sub(1, Ordering::AcqRel);
            }
        }).detach();
    }

    /// The electronics host is busy (a command waiting or a step running), so a job that can
    /// replace the bed document waits behind its last documents; normally one frame.
    pub fn hold_document_changes(&mut self, held: bool) {
        self.barrier = held;
        if !held {
            for (job, title) in std::mem::take(&mut self.held) {
                self.queue_inner(job, title, true);
            }
        }
    }

    /// A job that can replace the bed document is held (`hold_document_changes`).
    pub fn document_waits(&self) -> bool {
        !self.held.is_empty()
    }

    /// No job that can replace the bed document is held, queued or running, and the worker's
    /// bed is `bed`: the electronics host may take edits.
    pub fn document_settled(&self, bed: u64) -> bool {
        self.replacing.load(Ordering::Acquire) == 0 && self.switched_now.load(Ordering::Acquire) == bed
    }

    /// Queue a failed job again (the job strip's Retry). False when it cannot be retried.
    pub fn retry(&mut self, job: JobId) -> bool {
        let Some(copy) = self.retry.remove(&job) else { return false };
        let id = self.reserve_call_id();
        let copy = match copy {
            Job::Call { tool, args, .. } => Job::Call { id, tool, args },
            Job::Open { path, .. } => Job::Open { id, path },
            Job::ImportFile { path, new_plate, .. } => Job::ImportFile { id, path, new_plate },
            Job::AddScripts { paths, .. } => Job::AddScripts { id, paths },
            other => other,
        };
        self.queue(copy);
        true
    }

    /// The jobs that build the model, oldest first (see `build_jobs`).
    pub fn build_jobs(&self) -> &[JobId] {
        &self.build_jobs
    }

    /// The job strip's job for call `id`, while the board holds it.
    pub fn call_job(&self, id: u64) -> Option<JobId> {
        self.call_jobs.get(&id).copied()
    }

    /// The newest load asked for (it may have ended): the view's progress card follows it.
    pub fn load(&self) -> Option<&Load> {
        self.load.as_ref()
    }

    /// The headset view is up: plates go to it from now on.
    pub fn set_xr(&mut self, active: bool) {
        if active && !self.xr {
            self.xr = true;
            self.queue(Job::ServeXr);
        }
    }

    /// A tool call an AI made (the chat, an outside agent): the job strip names it as the AI's.
    pub fn chat_call(&mut self, tool: &str, args: Value) -> u64 {
        let id = self.reserve_call_id();
        self.chat_call_as(id, tool, args);
        id
    }

    /// An AI call by the id it was given when it was asked for (it waited for a gesture to end).
    pub fn chat_call_as(&mut self, id: u64, tool: &str, args: Value) {
        let title = format!("AI: {}", tool_title(tool));
        self.queue_titled(tool_job(id, tool, args), Some(title));
    }

    /// Queue a tool call; its answer comes back as `HostAction::Done` with this id.
    pub fn call(&mut self, tool: &str, args: Value) -> u64 {
        let id = self.reserve_call_id();
        self.queue(tool_job(id, tool, args));
        id
    }

    /// App-owned read tools use the same id space without queuing model work.
    pub fn reserve_call_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn encode_sheets(&mut self, id: u64, frames: review::Frames, made: bool) {
        self.queue(Job::EncodeSheets { id, frames, made });
    }

    pub fn print_setup(&mut self, setup: crate::print::CheckSetup) {
        self.queue(Job::PrintSetup(setup));
    }

    /// A picture painter ended ([`HostAction::Painted`]): the list takes the picture, the next is painted.
    pub fn painted(&mut self, painted: crate::plate_paint::Painted) {
        self.queue(Job::Painted(painted));
    }

    /// The bends of the plate are measured ([`HostAction::BendsMeasured`]): one that failed goes
    /// on its report.
    pub fn bends_measured(&mut self, content: [u8; 32]) {
        self.queue(Job::BendsMeasured(content));
    }

    /// A field a part asked for, solved (from `HostAction::Solved`).
    pub fn solved(&mut self, solved: Box<Solved>) {
        self.queue(Job::Solved(solved));
    }

    /// Set the language's host inputs and libraries and rebuild the scripts that read them.
    pub fn set_env(&mut self, bed: u64, env: ScriptEnv) {
        self.queue(Job::SetEnv { bed, env });
    }

    /// One queue push, no wait; queued documents for one bed collapse to the newest.
    pub fn set_docs(&mut self, bed: u64, plate: Option<String>, write: DocsWrite) {
        let DocsWrite { docs, assets, bases } = write;
        self.docs_seq += 1;
        let seq = self.docs_seq;
        self.docs_newest.lock().unwrap().insert(bed, seq);
        self.queue(Job::Docs { bed, plate, seq, docs, assets, bases, asked: std::time::Instant::now() });
    }

    /// The co-simulated chips' EEPROMs of bed `bed`'s runs; one queue push, no wait.
    pub fn set_memory(&mut self, bed: u64, memory: Arc<fab_model::ElecMemory>) {
        self.queue(Job::Memory { bed, memory });
    }

    /// A grown part's newest shape (from `HostAction::Grown`).
    pub fn grown(&mut self, shown: Box<crate::grow_live::Shown>) {
        self.queue(Job::Grown(shown));
    }

    /// What a grown part settled at (from `HostAction::GrownSettled`).
    pub fn grown_settled(&mut self, settled: Box<crate::grow_live::Settled>) {
        self.queue(Job::GrownSettled(settled));
    }

    /// The plate's grown patterns reached the steps they print at.
    pub fn patterns_grown(&mut self) {
        self.queue(Job::PatternsGrown);
    }

    /// The print colours' material colours and job tools (the colour mapping changed).
    pub fn mapped(&mut self, mapped: Arc<fab_model::eval::Mapped>) {
        self.queue(Job::Mapped(mapped));
    }

    pub fn test_mode(&mut self, on: bool) {
        self.queue(Job::TestMode(on));
    }

    pub fn overhangs(&mut self, on: bool) {
        self.queue(Job::Overhangs(on));
    }

    /// The chord exact parts are shown at (mm): their display meshes, no build.
    pub fn exact_detail(&mut self, chord: f64) {
        self.queue(Job::ExactDetail(chord));
    }

    /// Mesh the field parts too (Mesh shading) or not.
    pub fn mesh_fields(&mut self, on: bool) {
        self.queue(Job::MeshFields(on));
    }

    /// Move parts on the plate as one undo step; the answer comes back as `HostAction::Done`.
    pub fn place(&mut self, moves: Vec<fab_ai::PartMove>) -> u64 {
        let id = self.reserve_call_id();
        self.queue(Job::Place { id, moves });
        id
    }

    pub fn import_file(&mut self, path: PathBuf, new_plate: bool) -> u64 {
        let id = self.reserve_call_id();
        self.queue(Job::ImportFile { id, path, new_plate });
        id
    }

    pub fn make_calibration(&mut self, step: fab_calibrate::Step, setup: crate::print::CheckSetup, colour: [f32; 4]) {
        self.queue(Job::MakeCalibration { step, setup, colour });
    }

    pub fn calibration(&mut self, coupon: Arc<fab_calibrate::Coupon>, colour: [f32; 4]) -> u64 {
        let id = self.reserve_call_id();
        self.queue(Job::Calibration { id, coupon, colour });
        id
    }

    pub fn open(&mut self, path: PathBuf) -> u64 {
        let id = self.reserve_call_id();
        self.queue(Job::Open { id, path });
        id
    }

    /// The viewport's settled frame: saved as the project's picture. It replaces a frame
    /// still waiting, so one job and one frame wait however many frames settle while the
    /// worker is busy (a slice redraws and settles the view hundreds of times, and a job
    /// per frame held every frame's pixels until the worker came to it).
    pub fn thumbnail(&mut self, frame: &fab::api::ViewportFrame, demo: Option<&'static str>) {
        let mut slot = self.thumbnail.lock().unwrap();
        let waiting = slot.take();
        let queued = waiting.is_some();
        let demo = demo.or(waiting.and_then(|w| w.demo));
        *slot = Some(ThumbnailFrame { width: frame.width, height: frame.height, rgba: frame.rgba.clone(), demo });
        drop(slot);
        if !queued {
            self.queue(Job::Thumbnail(self.thumbnail.clone()));
        }
    }

    /// Put plate `id` on the bed as it was left (the plate on the bed keeps its edits).
    pub fn open_plate(&mut self, id: String) -> u64 {
        self.plate_call("open", &id, None)
    }

    /// `fab.plates` `action` on plate `id` (`name`: rename's new name).
    pub fn plate_call(&mut self, action: &str, id: &str, name: Option<&str>) -> u64 {
        let mut args = vec![("action".to_string(), Value::Str(action.to_string())), ("id".to_string(), Value::Str(id.to_string()))];
        if let Some(name) = name {
            args.push(("name".to_string(), Value::Str(name.to_string())));
        }
        self.call("plates", Value::Obj(args.into_iter().collect()))
    }

    /// Move plate `plate`'s files and working copy to the trash.
    pub fn delete_plate(&mut self, plate: String) -> u64 {
        let id = self.reserve_call_id();
        self.queue(Job::DeletePlate { id, plate });
        id
    }

    /// Read the plate's file again.
    pub fn reload(&mut self) -> u64 {
        let id = self.reserve_call_id();
        self.queue(Job::Reload { id });
        id
    }

    /// Show a demo plate as it ships.
    pub fn demo(&mut self, demo: &'static crate::demo_part::Demo) -> u64 {
        let id = self.reserve_call_id();
        self.queue(Job::Demo { id, demo });
        id
    }

    /// Put a mesh made from a picture on the plate (`id`: a reserved call id).
    pub fn import_mesh(&mut self, id: u64, made: crate::img2mesh::Made, render: bool) {
        self.queue(Job::ImportMesh { id, made: Box::new(made), render });
    }

    pub fn make_printable(&mut self, object: &str) -> u64 {
        let id = self.reserve_call_id();
        self.queue(Job::MakePrintable { id, object: object.into(), opts: Value::Obj(Vec::new()), render: false });
        id
    }

    /// Place object script files on the plate, arranged, as one undo step.
    pub fn add_scripts(&mut self, paths: Vec<PathBuf>) -> u64 {
        let id = self.reserve_call_id();
        self.queue(Job::AddScripts { id, paths });
        id
    }
}


#[cfg(test)]
mod measurement_tests {
    use super::*;
    use fab::api::{MeasureAction, MeasureKind, Units};
    use fab::tools::measure;
    use fab_model::{project, Change, FabModel, HistoryAction};

    #[test]
    fn measurement_points_and_kinds_round_trip_in_plate_millimetres() {
        let cases = [
            (MeasureKind::Distance, vec![[10.0, -20.0, 5.0], [40.0, 20.0, 5.0]]),
            (MeasureKind::Area, vec![[0.0, 0.0, 2.0], [30.0, 0.0, 2.0], [30.0, 40.0, 2.0], [0.0, 40.0, 2.0]]),
            (MeasureKind::Angle, vec![[10.0, 0.0, 0.0], [0.0; 3], [0.0, 20.0, 0.0]]),
            (MeasureKind::EdgeLength, vec![[10.0, 20.0, 0.0], [10.0, 20.0, 30.0], [50.0, 20.0, 30.0]]),
            (MeasureKind::Radius, vec![[10.0, 0.0, 7.0], [0.0, 10.0, 7.0], [-10.0, 0.0, 7.0], [0.0, -10.0, 7.0]]),
        ];
        for (kind, points) in cases {
            let saved = project::Measure { name: Some("Dimension".into()), kind: measure::kind_name(kind).into(), points };
            let shown = measurement_from_model(&saved);
            assert_eq!(shown.kind, kind);
            assert!(shown.value.is_finite());
            assert_eq!(shown.name, saved.name);
            for (point, mm) in shown.points.iter().zip(&saved.points) {
                for (metres, mm) in measure::position(*point).iter().zip(mm) {
                    assert!((metres * 1000.0 - mm).abs() < 0.0001);
                }
            }
            let back = measurement_to_model(&shown);
            assert_eq!(back.kind, saved.kind);
            assert_eq!(back.name, saved.name);
            assert_eq!(back.points.len(), saved.points.len());
            for (got, expected) in back.points.iter().flatten().zip(saved.points.iter().flatten()) {
                assert!((got - expected).abs() < 0.0001);
            }
            let again = measurement_from_model(&back);
            assert!((again.value - shown.value).abs() < 1e-6);
            assert_eq!(again.label, shown.label);
        }
    }

    #[test]
    fn face_rulers_preserve_parallel_gap_and_nonparallel_angle() {
        for second in [vec3(0.0, 0.0, 1.0), vec3(1.0, 0.0, 0.0)] {
            let m = measure::commit_with_normals(MeasureKind::FaceDistance,
                vec![vec3(0.03, 0.02, 0.005), vec3(0.07, 0.04, 0.025)],
                vec![vec3(0.0, 0.0, 1.0), second], &Units::millimeters()).unwrap();
            let saved = measurement_to_model(&m);
            assert_eq!(saved.kind, "face_distance");
            assert_eq!(saved.points.len(), 4);
            let loaded = measurement_from_model(&saved);
            assert_eq!(loaded.points, m.points);
            assert_eq!(loaded.normals, m.normals);
            assert_eq!(loaded.label, m.label);
            assert!((loaded.value - m.value).abs() < 1e-6);
            assert_eq!(loaded.gap.unwrap().parallel, m.gap.unwrap().parallel);
        }
    }

    #[test]
    fn measurement_delete_action_is_undoable_and_rename_keeps_points() {
        let mut model = FabModel::new("measurements");
        let mut eval = fab_model::eval::Evaluator::new(Machine::dual_nozzle_350());
        let saved = project::Measure { name: None, kind: "distance".into(), points: vec![[0.0; 3], [30.0, 40.0, 0.0]] };
        model.apply(&mut eval, "add", model.head(), &[Change::MeasureAdd { measure: saved.clone() }], None).unwrap();
        let rename = measurement_action(&MeasureAction::Rename(0, "Width".into())).unwrap();
        model.apply(&mut eval, "rename", model.head(), &[rename], None).unwrap();
        assert_eq!(model.project().measures[0].points, saved.points);
        assert_eq!(model.project().measures[0].name.as_deref(), Some("Width"));
        let change = measurement_action(&MeasureAction::Delete(0)).unwrap();
        assert!(matches!(change, Change::MeasureRemove { index: 0 }));
        model.apply(&mut eval, "remove", model.head(), &[change], None).unwrap();
        assert!(model.project().measures.is_empty());
        model.history(&mut eval, model.head(), HistoryAction::Undo, None).unwrap();
        assert_eq!(model.project().measures.len(), 1);
        let shown = measurement_from_model(&model.project().measures[0]);
        assert!((shown.value - 0.05).abs() < 1e-6);
        assert!(measurement_action(&MeasureAction::Select(0)).is_none());
        assert_eq!(project::parse(&model.project_text()).unwrap().measures, model.project().measures);
    }

    #[test]
    fn incomplete_or_unknown_rulers_keep_their_model_indices() {
        let saved = project::Measure { name: Some("Pending".into()), kind: "future_kind".into(), points: Vec::new() };
        let shown = measurement_from_model(&saved);
        assert_eq!(shown.name, saved.name);
        assert!(!shown.value.is_finite());
        assert!(shown.label.contains("future_kind"));
    }
}

#[cfg(test)]
mod worker_words_tests {
    use super::*;

    #[test]
    fn a_queued_job_says_what_it_waits_for() {
        let (a, b) = (JobSpec::new("first").start(), JobSpec::new("second").start());
        let words_of = |h: &JobHandle| jobs::board().get(h.id()).unwrap().stage_words();
        let mut words = WorkerWords { doing: Some("the print setup".to_string()), queued: vec![a.reporter(), b.reporter()] };
        words.set(Some("the print setup".to_string()), None);
        assert_eq!(words_of(&b), "waiting for the print setup");
        // The first takes its turn: the second names it, and the first's words stay its own.
        a.stage("cutting", None, None);
        words.set(Some("rebuilding the plate".to_string()), Some(a.id()));
        assert_eq!(words_of(&a), "cutting");
        assert_eq!(words_of(&b), "waiting for rebuilding the plate");
        words.set(None, None);
        assert_eq!(words_of(&b), "waiting for a free worker thread");
        a.done();
        b.done();
    }
}

#[cfg(test)]
mod scene_tests {
    use super::*;

    fn block(name: &str) -> fab_model::FabModel {
        let source = format!("{}let block = m.box({{size: [10mm 10mm 10mm]}})\nobject(block)\n", fab_model::program::EMPTY);
        fab_model::FabModel::with_script(name, &format!("{name}.cad.splash"), &source)
    }

    /// Why: a plate switch must never write one plate's board into another, 11-15.
    #[test]
    fn electronics_documents_reach_only_the_bed_they_were_made_for() {
        let root = std::env::temp_dir().join(format!("fab-electronics-bed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("Documents").join("Fab");
        let store = fab_ai::plates::PlateStore::under(&root.join("fab"), &home);
        let _cx = crate::cx_shared();
        let cx = Cx::new(Box::new(|_, _| {}));
        let host = ModelHost::start(&cx, false);
        let (done, ran) = std::sync::mpsc::channel();
        host.strand.submit(Class::Interactive, Ticket::none(), move |state| {
            let mut s = Session::new(Machine::dual_nozzle_350(), home.clone());
            s.store = Some(store.clone());
            s.replace_model(block("a"), None).unwrap();
            let a = s.plate.clone().unwrap();
            s.replace_model(block("b"), None).unwrap();
            let b = s.plate.clone().unwrap();
            let texts = |text: &str| Arc::new(fab_model::Docs::from([("board.pcb.splash".to_string(), Arc::<str>::from(text))]));
            let b_docs = texts("board b\n");
            s.model.set_docs(b_docs.clone());
            s.switch(&a).unwrap();
            let bed = s.switched;
            state.session = Some(s);
            let docs = texts("board a\n");
            let job = |seq, docs, base| Job::Docs {
                bed, plate: Some(a.clone()), seq, docs, assets: Arc::default(),
                bases: vec![crate::elec_host::Version { docs: base, assets: Arc::default() }], asked: std::time::Instant::now(),
            };
            state.docs_newest.lock().unwrap().insert(bed, 2);
            let before = state.session.as_ref().unwrap().model.docs().clone();
            let base = Arc::new(crate::elec_host::electronics(&before));
            state.process((job(1, texts("older board a\n"), base.clone()), None), Ticket::none());
            assert_eq!(state.session.as_ref().unwrap().model.docs(), &before, "an older queued set is skipped");
            assert_eq!(state.docs_newest.lock().unwrap().get(&bed), Some(&2), "the newest set keeps its turn");
            let current = job(2, docs.clone(), base.clone());
            assert!(job_title(&current).is_none() && !builds_model(&current) && !shapes_model(&current), "documents have no title or rebuild");
            assert!(!left_at_quit(&current) && loads(&current).is_none() && retry_copy(&current).is_none(), "documents must be saved, without a load or retry");
            assert_eq!(class_of(&current), Class::Visible);
            state.process((current, None), Ticket::none());
            assert!(!state.docs_newest.lock().unwrap().contains_key(&bed), "the newest set consumed its turn");
            let s = state.session.as_mut().unwrap();
            assert_eq!(s.model.docs(), &docs, "documents reach their current bed");
            assert_eq!(status(s).docs, docs, "status carries the bed's documents");
            assert!(s.needs_autosave(), "the owner has documents to save");
            s.autosave().unwrap();
            let mut recovered = Session::new(Machine::dual_nozzle_350(), home);
            recovered.store = Some(store.clone());
            recovered.recover().expect("a plate was left on the bed").unwrap();
            assert_eq!(recovered.model.docs(), &docs, "the next autosave recovers equal documents");

            let before_rejected = state.session.as_ref().unwrap().model.docs().clone();
            let rejected = texts("board a over an unseen change\n");
            state.docs_newest.lock().unwrap().insert(bed, 3);
            state.process((job(3, rejected.clone(), base), None), Ticket::none());
            let kept = store.kept_aside(&a);
            assert!(Arc::ptr_eq(state.session.as_ref().unwrap().model.docs(), &before_rejected)
                && kept.len() == 1
                && kept[0].1 == "the plate's electronics documents changed after the electronics engine read them"
                && std::fs::read_to_string(kept[0].0.join("board.pcb.splash")).unwrap() == rejected["board.pcb.splash"].as_ref(),
                "a write over an unknown version leaves the bed unchanged and is kept aside intact with why");

            let s = state.session.as_mut().unwrap();
            let mut with_notes = (**s.model.docs()).clone();
            with_notes.insert("notes.txt".into(), Arc::from("the bed's notes\n"));
            s.model.set_docs(Arc::new(with_notes));
            let base = Arc::new(crate::elec_host::electronics(s.model.docs()));
            let moved = texts("board a moved again\n");
            state.docs_newest.lock().unwrap().insert(bed, 4);
            state.process((job(4, moved.clone(), base), None), Ticket::none());
            let s = state.session.as_mut().unwrap();
            assert!(s.model.docs().get("notes.txt").map(|text| text.as_ref()) == Some("the bed's notes\n")
                && crate::elec_host::same_files(&crate::elec_host::electronics(s.model.docs()), &moved),
                "a write over the right base replaces electronics and keeps the bed's other files");
            // The note tests the in-memory merge; the switch below autosaves only .splash documents.
            s.model.set_docs(moved.clone());
            s.switch(&b).unwrap();
            let switched = s.switched;
            assert_ne!(bed, switched, "the other plate has another bed identity");
            assert_eq!(s.model.docs(), &b_docs, "the other plate has its own board");
            let late = texts("late board a, text intact\n");
            state.docs_newest.lock().unwrap().insert(bed, 5);
            state.process((job(5, late.clone(), moved), None), Ticket::none());
            assert_eq!(state.session.as_ref().unwrap().model.docs(), &b_docs, "late documents never reach the other plate");
            let kept = store.kept_aside(&a);
            assert_eq!(kept.len(), 2, "late documents are kept in their original plate's folder beside the rejected write");
            let why = format!("electronics documents for another plate (bed {bed}, now {switched}) reached the model");
            let (path, _) = kept.iter().find(|(_, reason)| reason == &why).expect("the late documents are kept with why they missed the bed");
            assert_eq!(std::fs::read_to_string(path.join("board.pcb.splash")).unwrap(), late["board.pcb.splash"].as_ref(), "all late text is intact");
            assert_eq!(std::fs::read_to_string(path.join("why")).unwrap(), why, "the kept documents say why they missed the bed");
            done.send(()).unwrap();
        }).detach();
        ran.recv().expect("the document handlers finish (a failed assert above says which)");
        drop(host);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Why: a chip's EEPROM writes still waiting for their once-a-second send were lost when the
    /// plate switched (11-18). A plate switch asked while they wait is held until they have gone
    /// to their own plate, ahead of it: a firmware writes its count every 20 ms, the plate is
    /// switched away at once and back, and it keeps the count the chip had written.
    #[test]
    fn eeprom_writes_waiting_at_a_plate_switch_reach_their_own_plate() {
        use fab_elec_bench::SimCmd;
        use fab_elec_view::live::sim::SimAction;
        use std::time::Duration;

        const COUNTER: &str = "let led = output(PD6)\nvar n: i32 = 0\nlet tick = every(ms(20), fn() {\n    n = (n + 1) & 255\n    eeprom_write(0, n)\n})\n";
        const BENCH: &str = "bench(\"firmware\", {circuit: \"ledavr\" run: {stop: 10s}}, fn(b, c) {\n    b.firmware(c.mcu.u, \"counter.fw.splash\", {})\n})\n";
        const FILE: &str = "ledavr.mcu.u.eeprom";
        let root = std::env::temp_dir().join(format!("fab-eeprom-switch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("Documents").join("Fab");
        let store = fab_ai::plates::PlateStore::under(&root.join("fab"), &home);
        let docs = Arc::new(fab_model::Docs::from([
            ("parts/avr/ATMEGA328P_TQFP32.part.splash".into(),
                Arc::from(include_str!("../../elec/fixtures/parts/avr/ATMEGA328P_TQFP32.part.splash"))),
            ("ledavr.ckt.splash".into(), Arc::from(include_str!("../../elec/fixtures/ledavr.ckt.splash"))),
            ("counter.bench.splash".into(), Arc::from(BENCH)),
            ("counter.fw.splash".into(), Arc::from(COUNTER)),
        ]));
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut host = ModelHost::start(&cx, false);
        let on_bed = |host: &ModelHost| {
            let (tx, rx) = std::sync::mpsc::channel();
            host.strand.submit(Class::Interactive, Ticket::none(), move |state| {
                let s = state.session.as_ref().expect("a session");
                tx.send((s.plate.clone(), s.switched, s.model.elec_memory().get(FILE).cloned())).unwrap();
            }).detach();
            rx.recv_timeout(Duration::from_secs(60)).expect("the model host gets through its queue")
        };
        let plate_docs = docs.clone();
        let (made, plates) = std::sync::mpsc::channel();
        host.strand.submit(Class::Interactive, Ticket::none(), move |state| {
            let mut s = Session::new(Machine::dual_nozzle_350(), home.clone());
            s.store = Some(store.clone());
            s.replace_model(block("b"), None).unwrap();
            let b = s.plate.clone().unwrap();
            s.replace_model(block("a"), None).unwrap();
            s.model.set_docs(plate_docs);
            let a = s.plate.clone().unwrap();
            state.switched_now.store(s.switched, Ordering::Release);
            state.session = Some(s);
            made.send((a, b)).unwrap();
        }).detach();
        let (a, b) = plates.recv_timeout(Duration::from_secs(60)).expect("the plates are made");
        let (_, bed, _) = on_bed(&host);
        let mut elec = crate::elec_host::ElecHost::start(&mut cx, bed, Some(a.clone()), docs, Arc::default(),
            Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        elec.set_memory(bed, Arc::default());
        elec.event(&mut cx, &Event::Actions(vec![Box::new(SimAction::Cmd(SimCmd::Play))]));
        // a few counts in, one write waits for its send: the run's own progress, failing at once if
        // the run ends or stands before it
        let written = loop {
            elec.event(&mut cx, &Event::Signal);
            elec.sync_model(&mut host);
            let count = elec.published_eeprom().map(|bytes| bytes[0]).filter(|&n| n != 0xFF && n >= 3);
            if let (Some(count), true) = (count, elec.memory_pending()) {
                break count;
            }
            elec.assert_plays_on("a few counts in, the counter's writes wait to go");
            std::thread::sleep(Duration::from_millis(1));
        };
        host.open_plate(b.clone());
        assert!(host.document_waits(), "the switch waits for the chip's writes");
        elec.sync_model(&mut host);
        assert!(!elec.memory_pending() && !host.document_waits(), "the writes went ahead of the switch, which then went");
        let (plate, _, _) = on_bed(&host);
        assert_eq!(plate.as_ref(), Some(&b), "the other plate is on the bed");
        host.open_plate(a.clone());
        let (plate, _, kept) = on_bed(&host);
        assert_eq!(plate.as_ref(), Some(&a), "the first plate is back");
        let kept = kept.expect("the plate keeps its chip's EEPROM");
        assert_eq!((kept.len(), kept[0]), (1024, written), "the plate keeps the count the chip had written when the switch was asked");
        elec.event(&mut cx, &Event::Actions(vec![Box::new(SimAction::Cmd(SimCmd::Stop))]));
        drop(host);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A mesh hashed a block at a time has the digest of a hash of each value in turn.
    #[test]
    fn a_mesh_hashed_in_blocks_has_the_digest_of_one_value_at_a_time() {
        use makepad_core_util::sha256::Sha256;
        let positions: Vec<[f32; 3]> = (0..1501).map(|i| [i as f32 * 0.25, -(i as f32), 1e-3 * i as f32]).collect();
        let normals: Vec<[f32; 3]> = (0..1501).map(|i| [0.0, (i % 7) as f32, 1.0]).collect();
        let mesh = fab_plate::RenderMesh { positions, normals, indices: (0..3001u32).rev().collect() };
        let mut one = Sha256::new();
        for values in [&mesh.positions, &mesh.normals] {
            one.update(&(values.len() as u64).to_le_bytes());
            for v in values.iter().flatten() {
                one.update(&v.to_bits().to_le_bytes());
            }
        }
        one.update(&(mesh.indices.len() as u64).to_le_bytes());
        for i in &mesh.indices {
            one.update(&i.to_le_bytes());
        }
        let mut inputs = Vec::new();
        mesh_inputs(&mut inputs, &mesh);
        assert_eq!(hash_inputs(inputs), one.finish());
    }

    /// A plate switched back to shows its kept build with the scene it had, not one built again;
    /// a scene goes when its build does, so only the plates that keep their build keep one.
    #[test]
    fn a_plate_switched_back_to_installs_the_scene_it_had() {
        let root = std::env::temp_dir().join(format!("fab-scenes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("Documents").join("Fab");
        let mut s = Session::new(Machine::dual_nozzle_350(), home.clone());
        s.store = Some(fab_ai::plates::PlateStore::under(&root.join("fab"), &home));
        let mut scenes = Scenes::default();
        let on_bed = |s: &mut Session, scenes: &mut Scenes, overhangs: bool| {
            let ev = s.last.clone().expect("a build on the bed");
            scenes.of(s, &ev, overhangs)
        };
        s.replace_model(block("a"), None).unwrap();
        let a = s.plate.clone().unwrap();
        let a_scene = on_bed(&mut s, &mut scenes, false);
        s.replace_model(block("b"), None).unwrap();
        let b = s.plate.clone().unwrap();
        let b_scene = on_bed(&mut s, &mut scenes, false);
        s.switch(&a).unwrap();
        assert!(Arc::ptr_eq(&on_bed(&mut s, &mut scenes, false), &a_scene), "a back on the bed installs the scene it had");
        s.switch(&b).unwrap();
        assert!(Arc::ptr_eq(&on_bed(&mut s, &mut scenes, false), &b_scene), "and b");
        // the overhang colours are another scene of the same build, kept in its place
        let b_overhangs = on_bed(&mut s, &mut scenes, true);
        assert!(!Arc::ptr_eq(&b_overhangs, &b_scene), "overhangs shown: a scene of their own");
        assert!(Arc::ptr_eq(&on_bed(&mut s, &mut scenes, false), &b_scene), "toggling back retains the ordinary scene");
        assert!(Arc::ptr_eq(&on_bed(&mut s, &mut scenes, true), &b_overhangs), "both colour variants survive");
        let ev = s.rebuild().unwrap();
        assert!(Arc::ptr_eq(&scenes.of(&s, &ev, true), &b_overhangs), "identical rebuild adopts the scene despite a new allocation");
        let transient = Arc::new((*ev).clone());
        assert!(scenes.held_content(&s.model.name, &transient).is_some(), "a copy of a held build takes its key, compared with it, not hashed");
        assert!(Arc::ptr_eq(&scenes.of(&s, &transient, true), &b_overhangs), "an unretained equivalent evaluation can borrow the scene");
        assert!(Arc::ptr_eq(&scenes.of(&s, &ev, true), &b_overhangs), "borrowing must not evict the retained build's scene");
        let mut changed = (*ev).clone();
        changed.render[0].positions[0][0] += 1.0;
        let changed = Arc::new(changed);
        assert!(scenes.held_content(&s.model.name, &changed).is_none(), "changed display geometry is not the held build's");
        s.last = Some(changed.clone());
        assert!(!Arc::ptr_eq(&scenes.of(&s, &changed, true), &b_overhangs), "same source stamp with changed display geometry must miss");
        s.last = Some(ev);
        let b_overhangs = on_bed(&mut s, &mut scenes, true);
        s.switch(&a).unwrap();
        on_bed(&mut s, &mut scenes, true);
        s.switch(&b).unwrap();
        assert!(Arc::ptr_eq(&on_bed(&mut s, &mut scenes, true), &b_overhangs), "b back with its overhangs scene");
        // Print-only settings make a new build, but its display inputs still match.
        let r = s.commit("infill", vec![fab_model::Change::PlateSettings { set: vec![("sparse_infill_density".into(), "12%".into())], reset: Vec::new() }]);
        assert!(r.ok, "{}", r.text);
        let edited = on_bed(&mut s, &mut scenes, true);
        assert!(Arc::ptr_eq(&edited, &b_overhangs), "print-only settings keep the identical display scene");
        // the scenes are those of the builds the session holds; b, three plates back, has none
        for name in ["c", "d", "e"] {
            s.replace_model(block(name), None).unwrap();
            on_bed(&mut s, &mut scenes, true);
        }
        assert_eq!(scenes.kept.len(), s.builds().count(), "one scene for each build held, no more");
        s.switch(&b).unwrap();
        assert!(!Arc::ptr_eq(&on_bed(&mut s, &mut scenes, true), &edited), "b, built again, has a new scene");
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// The script errors a test files, as the error log hears them: the error hook is the process's
/// and tests run side by side, so each hears only what its own thread files (a plate's scripts
/// run on the thread that opens it). Every test that listens sets the same hook.
#[cfg(test)]
pub(crate) mod filed {
    use std::cell::RefCell;

    std::thread_local! {
        static HEARD: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }

    fn hear(line: &str) {
        HEARD.with(|h| {
            if let Some(heard) = h.borrow_mut().as_mut() {
                heard.push(line.to_string());
            }
        });
    }

    /// Hear what this thread files from now on.
    pub fn listen() {
        makepad_script::inspect_registry::set_error_hook(hear);
        HEARD.with(|h| {
            h.borrow_mut().get_or_insert_with(Vec::new);
        });
    }

    /// What this thread filed since it listened or last asked.
    pub fn take() -> Vec<String> {
        HEARD.with(|h| h.borrow_mut().as_mut().map(std::mem::take).unwrap_or_default())
    }
}

#[cfg(test)]
mod plate_list_tests {
    use super::*;

    /// The errors this thread filed since it last asked that say the file is in an earlier format.
    fn met() -> usize {
        filed::take().iter().filter(|l| l.contains("unknown named arg nozzle")).count()
    }

    /// A plate an earlier Fab kept, in its format (`place` took `filament:` and `nozzle:`), is on
    /// the list as one this Fab cannot read, with why, and that row is the one place it is said:
    /// neither the first start's fold-in nor the list's read of the file files script errors (a
    /// run's read of it still does). Why is read once for each version of the file, not each
    /// time the list is sent: every read evaluates the file. A new version is read again.
    #[test]
    fn an_unread_plate_is_read_once_for_each_version_of_its_file() {
        filed::listen();
        let root = std::env::temp_dir().join(format!("fab-unread-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (fab, home) = (root.join("fab"), root.join("Documents").join("Fab"));
        let file = fab.join("previous").join("1793600982-vase").join("vase.fab.splash");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let old = "use mod.fab.*\n\nscript(\"vase.cad.splash\")\n\nplace(\"vase\", object: \"vase\", filament: 3, color: #x3f89da, nozzle: @right)\n";
        std::fs::write(&file, old).unwrap();
        let mut s = Session::new(Machine::dual_nozzle_350(), home.clone());
        s.store = Some(fab_ai::plates::PlateStore::under(&fab, &home));
        assert!(s.recover().is_none(), "nothing was on the bed");
        let mut reads = ListReads::default();
        let card = |list: &fab::api::PlateList| list.plates.iter().find(|c| c.name == "vase").cloned().expect("the old plate is listed");
        let first = card(&plate_list(&s, &mut reads));
        assert_eq!(first.note, "this Fab cannot read its file");
        assert!(first.why.contains("unknown named arg nozzle"), "{}", first.why);
        assert_eq!(met(), 0, "the fold-in's and the list's reads file no script error: the row says why");
        assert!(fab_model::project::load(&file, &ScriptEnv::default()).is_err());
        assert!(met() > 0, "a run's read of the file files its errors");
        // The file changed under the same modified time: the list sent again does not read it.
        let at = std::fs::metadata(&file).and_then(|m| m.modified()).unwrap();
        std::fs::write(&file, old.replace("\n\nplace", "\n\n\nplace")).unwrap();
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(at).unwrap();
        for _ in 0..5 {
            assert_eq!(card(&plate_list(&s, &mut reads)).why, first.why);
        }
        assert_eq!(met(), 0, "the list's reads file nothing");
        // The file in this Fab's format, a new version of it: read again, it reads now.
        let script = format!("{}let vase = m.box({{size: [10mm 10mm 10mm]}})\nobject(vase)\n", fab_model::program::EMPTY);
        std::fs::write(file.with_file_name("vase.cad.splash"), script).unwrap();
        std::fs::write(&file, "use mod.fab.*\n\nscript(\"vase.cad.splash\")\n\nplace(\"vase\")\n").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(later).unwrap();
        let again = card(&plate_list(&s, &mut reads));
        assert!(again.why.contains("reads now"), "{}", again.why);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A plate whose working copy had a file changed outside Fab says so on its row, naming the
    /// file; one whose working copy did not read back says it is kept aside, where and why.
    #[test]
    fn a_plate_row_says_what_changed_outside_fab_and_what_is_kept_aside() {
        let root = std::env::temp_dir().join(format!("fab-kept-row-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (fab, home) = (root.join("fab"), root.join("Documents").join("Fab"));
        let session = || {
            let mut s = Session::new(Machine::dual_nozzle_350(), home.clone());
            s.store = Some(fab_ai::plates::PlateStore::under(&fab, &home));
            s
        };
        let source = format!("{}let block = m.box({{size: [10mm 10mm 10mm]}})\nobject(block)\n", fab_model::program::EMPTY);
        let mut s = session();
        let mut plate = |name: &str| {
            s.replace_model(fab_model::FabModel::with_script(name, &format!("{name}.cad.splash"), &source), None).unwrap();
            s.autosave().unwrap();
            s.plate.clone().unwrap()
        };
        let (a, b) = (plate("a"), plate("b"));
        drop(s);
        let store = fab_ai::plates::PlateStore::under(&fab, &home);
        let script = |id: &str, name: &str| {
            let scripts = store.dir(id).join("scripts");
            let map = std::fs::read_to_string(scripts.join("map")).unwrap();
            let hash = map.lines().find_map(|l| l.strip_suffix(&format!("\tscripts/{name}.cad.splash"))).unwrap().to_string();
            scripts.join(format!("{hash}.cad.splash"))
        };
        std::fs::write(script(&a, "a"), source.replace("10mm 10mm 10mm", "12mm 10mm 10mm")).unwrap();
        std::fs::write(script(&b, "b"), [0xff, 0xfe]).unwrap();
        let mut s = session();
        s.switch(&a).unwrap();
        s.switch(&b).unwrap();
        let list = plate_list(&s, &mut ListReads::default());
        let card = |id: &str| list.plates.iter().find(|c| c.id == id).cloned().expect("listed");
        let (changed, kept) = (card(&a), card(&b));
        assert_eq!(changed.note, "changed outside Fab");
        assert!(changed.why.contains("scripts/a.cad.splash was changed outside Fab"), "{}", changed.why);
        assert_eq!(kept.note, "files kept aside");
        assert!(kept.why.contains("did not read back") && kept.why.contains(&format!("kept in {}", store.dir(&b).join("kept-aside").display())), "{}", kept.why);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The plate a fresh Fab builds when nothing was on the bed (the empty model) files no script
    /// error. Every demo, the plate Fab opens with among them, is heard as it builds
    /// (`demo_part.rs`, `builds_and_slices`).
    #[test]
    fn the_empty_plate_builds_with_no_script_error() {
        filed::listen();
        // the hook hears this thread: a project of an earlier format files its error
        let old = "use mod.fab.*\n\nplace(\"vase\", object: \"vase\", filament: 3)\n";
        assert!(fab_model::FabModel::load("old", old, Default::default()).is_err());
        let heard = filed::take();
        assert!(heard.iter().any(|l| l.contains("unknown named arg filament")), "the hook hears this thread's script errors: {heard:?}");
        Session::new(Machine::dual_nozzle_350(), std::env::temp_dir()).rebuild().expect("the empty plate builds");
        let heard = filed::take();
        assert!(heard.is_empty(), "the empty plate: {heard:?}");
    }
}

#[cfg(test)]
mod person_first {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Condvar;
    use std::time::{Duration, Instant};

    /// What a person asks for: a resize or a move, a parameter, a script, a load.
    fn asked() -> Vec<Job> {
        vec![
            Job::Place { id: 0, moves: Vec::new() },
            Job::Edit { id: 0, edits: Vec::new() },
            Job::Param { id: 0, key: "height".into(), value: PValue::Num(1.0), fallback: None, asked: Instant::now() },
            Job::Source { id: 0, file: "part.cad.splash".into(), text: String::new(), param: None },
            Job::Call { id: 0, tool: "script".into(), args: Value::Null },
            Job::Open { id: 0, path: PathBuf::from("plate") },
            Job::Reload { id: 0 },
        ]
    }

    /// What a run sends back when it is ready: nobody is waiting on these, so what a person asks
    /// for goes ahead of them.
    #[test]
    fn what_nobody_waits_on_goes_behind_what_a_person_asks_for() {
        let (file, part) = (String::new(), String::new());
        let stored = fab_model::grown::Stored { inputs: 0, space: 0, level: 0, h: 0.0, bytes: Arc::from(Vec::new()) };
        for job in [
            Job::PatternsGrown,
            Job::Solved(Box::new(Solved { file: file.clone(), part: part.clone(), study: "a study", doing: String::new(), asked: None, grid: None })),
            Job::Grown(Box::new(crate::grow_live::Shown { file: file.clone(), part: part.clone(), run: 0, slot: Default::default() })),
            Job::GrownSettled(Box::new(crate::grow_live::Settled { file, part, run: 0, stored: Arc::new(stored) })),
        ] {
            assert_eq!(class_of(&job), Class::Visible);
        }
    }

    /// The CPU share of the process [`what_a_person_asks_for_starts_within_an_item_of_visible_work_on_every_worker`]
    /// runs in: "every worker" is that process's pool's, which no other test's work holds (in the
    /// test binary's shared pool other tests' fan-outs held two of them, so the slice never held
    /// every worker).
    const OWN_PROCESS_CORES: &str = "8";
    /// Set in that process.
    const OWN_PROCESS: &str = "FAB_PERSON_FIRST_OWN_PROCESS";

    /// Runs test `name` of this module in a child of the test binary with a pool of its own
    /// ([`OWN_PROCESS_CORES`]), failing with the child's panic.
    fn in_own_process(name: &str) {
        let name = format!("model_host::person_first::{name}");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "--test-threads=1", &name])
            .env(OWN_PROCESS, "1")
            .env("MAKEPAD_CPU_CORES", OWN_PROCESS_CORES)
            .output()
            .unwrap();
        let all = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let panic = all.lines().find(|l| l.contains("panicked at") || l.contains("\"kind\":\"panic\"")).unwrap_or("no panic line");
        assert!(output.status.success() && all.contains(&format!("test {name} ... ok")), "{name} failed in its own process ({}): {panic}\n{all}", output.status);
    }

    /// Lets every item of the round go when the test ends, passed or failed: an item left waiting
    /// held its worker for good, and every other test of the process waited behind it.
    struct LetGo(Arc<(Mutex<(u64, bool)>, Condvar)>);

    impl Drop for LetGo {
        fn drop(&mut self) {
            let (state, moved) = &*self.0;
            state.lock().unwrap_or_else(|e| e.into_inner()).1 = true;
            moved.notify_all();
        }
    }

    /// Why (user): "i cant even resize the mesh it keeps waiting for workers", with the live slice
    /// on every worker at the class the resize ran at. Whatever holds the workers at Visible, what a
    /// person asked for starts when their items in hand end: one item each, no more.
    #[test]
    fn what_a_person_asks_for_starts_within_an_item_of_visible_work_on_every_worker() {
        if std::env::var_os(OWN_PROCESS).is_none() {
            return in_own_process("what_a_person_asks_for_starts_within_an_item_of_visible_work_on_every_worker");
        }
        for job in asked() {
            assert_eq!(class_of(&job), Class::Interactive, "{:?}", job_title(&job));
        }
        let _cx = crate::cx_shared();
        let pool = Cx::new(Box::new(|_, _| {})).task_pool();
        let workers = pool.worker_count();
        // each item holds its worker until the next round of items is let go
        let round = Arc::new((Mutex::new((0u64, false)), Condvar::new()));
        let let_go = LetGo(round.clone());
        let on_items = Arc::new(AtomicUsize::new(0));
        let (slice_pool, slice_round, slice_on) = (pool.clone(), round.clone(), on_items.clone());
        let slice = pool
            .submit_named(Class::Visible, "a live slice", move || {
                slice_pool.fan_out(workers * 4096, |_| {
                    slice_on.fetch_add(1, Ordering::AcqRel);
                    let (state, moved) = &*slice_round;
                    let mut at = state.lock().unwrap();
                    let began = at.0;
                    while at.0 == began && !at.1 {
                        at = moved.wait(at).unwrap();
                    }
                    drop(at);
                    slice_on.fetch_sub(1, Ordering::AcqRel);
                });
            })
            .unwrap();
        let waiting = Instant::now();
        while on_items.load(Ordering::Acquire) < workers {
            assert!(waiting.elapsed() < Duration::from_secs(120), "the slice never held every worker");
            std::thread::sleep(Duration::from_millis(1));
        }
        for job in asked() {
            let started = Arc::new(AtomicBool::new(false));
            let start = started.clone();
            pool.submit_named(class_of(&job), "a person's job", move || start.store(true, Ordering::Release)).unwrap().detach();
            // the items in hand end, and only those
            round.0.lock().unwrap().0 += 1;
            round.1.notify_all();
            let waiting = Instant::now();
            while !started.load(Ordering::Acquire) {
                assert!(waiting.elapsed() < Duration::from_secs(60), "{:?} found no worker", job_title(&job));
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        drop(let_go);
        // a test thread may not block on a pool task (`join` refuses: WouldBlockUi): the let-go
        // slice's items end, each released above, and the task with them
        while !slice.is_finished() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Why: a quit dropped every edit still queued behind a long job (the pool's close drops a
    /// strand's queued jobs), and the code panel's last typing with them. A quit's answer comes
    /// after the model host has run and saved every edit queued before it, so what the person saw
    /// is in the working copy when the app quits on that answer. Work nobody waits on, queued
    /// before the quit, is left (here the test prints' choice): the quit waits for edits only.
    /// With none queued or running and the plate saved, it waits for no job: a quit that waited
    /// 8 s behind the worker's job under a slow first run of its code answers while a long job
    /// still holds the worker.
    #[test]
    fn a_quit_answers_once_the_edit_queued_behind_a_long_job_is_saved() {
        let _cx = crate::cx_owned();
        if std::env::var_os(OWN_PROCESS).is_none() {
            return in_own_process("a_quit_answers_once_the_edit_queued_behind_a_long_job_is_saved");
        }
        let root = std::env::temp_dir().join(format!("fab-quit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // this process's own home: the store below it is the test's (`user_home`)
        std::env::set_var("MAKEPAD_TEST_HOME", &root);
        let answered = Arc::new(Mutex::new(Vec::new()));
        let seen = answered.clone();
        let mut cx = Cx::new(Box::new(move |_, event| {
            if let Event::Actions(actions) = event {
                for action in actions {
                    if let Some(HostAction::Quit { id }) = action.downcast_ref::<HostAction>() {
                        seen.lock().unwrap().push(*id);
                    }
                }
            }
        }));
        let mut host = ModelHost::start(&cx, true);
        let source = |x: u32| format!("{}let block = m.box({{size: [{x}mm 10mm 10mm]}})\nobject(block)\n", fab_model::program::EMPTY);
        let first = source(10);
        host.strand.submit(Class::Interactive, Ticket::none(), move |state| {
            let session = state.session.as_mut().expect("the model host's first job made its session");
            session.replace_model(fab_model::FabModel::with_script("block", "block.cad.splash", &first), None).unwrap();
            session.autosave().unwrap();
        }).detach();
        // A long job holds the model worker while the edit and the quit queue behind it.
        let held = Arc::new((Mutex::new(false), Condvar::new()));
        let hold = held.clone();
        host.strand.submit(Class::Interactive, Ticket::none(), move |_| {
            let (open, moved) = &*hold;
            let mut open = open.lock().unwrap();
            while !*open {
                open = moved.wait(open).unwrap();
            }
        }).detach();
        let typed = source(20);
        host.set_source("block.cad.splash".into(), typed.clone());
        host.queue(Job::TestMode(true));
        let quit = host.quit(None);
        {
            let (open, moved) = &*held;
            *open.lock().unwrap() = true;
            moved.notify_all();
        }
        // The strand runs in order: a job queued after the quit ends after the quit has answered.
        let (after, ran) = std::sync::mpsc::channel();
        host.strand.submit(Class::Interactive, Ticket::none(), move |state| after.send(state.session.as_ref().map(|s| s.test_mode)).unwrap()).detach();
        let test_mode = ran.recv_timeout(Duration::from_secs(60)).expect("the model worker gets through its queue");
        assert_eq!(test_mode, Some(false), "the test prints' choice queued before the quit is left");
        cx.handle_action_receiver();
        cx.handle_actions();
        assert_eq!(*answered.lock().unwrap(), vec![quit], "the quit answers once, with its id");
        let mut s = Session::new(Machine::dual_nozzle_350(), documents_dir());
        s.store = Some(plate_store());
        assert!(s.recover().is_some_and(|built| built.is_ok()), "the plate left on the bed comes back");
        assert_eq!(s.model.state().scripts.get("block.cad.splash"), Some(&typed), "the edit queued behind the long job is in the working copy when the quit answers");
        drop(host);

        // the next run: a job of its own ends (its end saves the plate it found and notes it
        // saved), then a long job holds the worker and work that keeps nothing waits behind it
        let mut host = ModelHost::start(&cx, true);
        host.queue(Job::TestMode(false));
        let held = Arc::new((Mutex::new(false), Condvar::new()));
        let hold = held.clone();
        let (started, holding) = std::sync::mpsc::channel();
        host.strand.submit(Class::Interactive, Ticket::none(), move |_| {
            started.send(()).unwrap();
            let (open, moved) = &*hold;
            let mut open = open.lock().unwrap();
            while !*open {
                open = moved.wait(open).unwrap();
            }
        }).detach();
        holding.recv_timeout(Duration::from_secs(60)).expect("the next run's start ends and its long job holds the worker");
        host.queue(Job::TestMode(true));
        let quit = host.quit(None);
        let asked = std::time::Instant::now();
        while !answered.lock().unwrap().contains(&quit) && asked.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(5));
            cx.handle_action_receiver();
            cx.handle_actions();
        }
        assert!(answered.lock().unwrap().contains(&quit), "a quit with nothing kept and the plate saved answers while the worker still runs its job");
        {
            let (open, moved) = &*held;
            *open.lock().unwrap() = true;
            moved.notify_all();
        }
        drop(host);
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod load_hold_tests {
    use super::load_hold::LoadHold;
    use super::*;
    use fab_model::eval::{BuildGate, Evaluator};
    use std::time::{Duration, Instant};

    /// The hold on loads is each load's own: its build stops at the hold past its first step,
    /// its bar at the next step's start and staying there, while a build for another job runs
    /// through, whether it was handed no gate or the held load's. Let go, the held build goes on
    /// to its end; cancelled (its cancel hook wakes it), it ends there, in the step it held at. The
    /// hold keeps each load it held, with that step.
    #[test]
    fn a_held_load_stops_its_own_build_and_no_other() {
        let source = format!("{}object(m.box({{size: [1mm 1mm 1mm]}}))\n", fab_model::program::EMPTY);
        let hold = Arc::new(LoadHold::new());
        let gate = |job: &JobHandle| {
            let (hold, job) = (hold.clone(), job.reporter());
            BuildGate { job: job.id(), at_second_step: Box::new(move || hold.wait(&job)) }
        };
        // `job`'s build of the box with `gate` on its evaluator, on a thread of its own: whether it built
        let build = |job: &JobHandle, gate: Option<BuildGate>| {
            let (source, job) = (source.clone(), job.reporter());
            std::thread::spawn(move || {
                let mut eval = Evaluator::new(Machine::dual_nozzle_350());
                eval.gate = gate;
                let _current = jobs::enter(Some(job));
                fab_model::FabModel::with_script("held", "block.cad.splash", &source).evaluate(&mut eval, None).is_ok()
            })
        };
        for cancel in [false, true] {
            hold.set(true);
            let woken = hold.clone();
            let held = JobSpec::new("held").on_cancel(move || woken.wake()).start();
            let other = JobSpec::new("other").start();
            let loading = build(&held, Some(gate(&held)));
            let asked = Instant::now();
            while hold.waiting().1 != [held.id()] {
                assert!(!loading.is_finished(), "the held load's build ended before the hold");
                assert!(asked.elapsed() < Duration::from_secs(120), "the held load's build never reached the hold");
                std::thread::sleep(Duration::from_millis(1));
            }
            let bar = || jobs::board().get(held.id()).map(|j| (j.fraction, j.stage_words())).expect("the held job");
            let at = bar();
            assert!(at.0.is_some_and(|f| f > 0.0 && f < 1.0), "the held bar is not past 0: {at:?}");
            assert!(["building", "measuring the parts", "laying out the plate"].iter().any(|w| at.1.starts_with(w)), "the held bar names no build step: {at:?}");
            assert!(build(&other, None).join().unwrap(), "another job's build did not run while a load was held");
            assert!(build(&other, Some(gate(&held))).join().unwrap(), "another job's build handed the held load's gate did not run");
            assert_eq!(hold.waiting().1, vec![held.id()], "another job's build stopped at the hold");
            assert!(!loading.is_finished(), "the held load's build went on while held");
            assert_eq!(bar(), at, "the held load's bar moved");
            if cancel {
                assert!(jobs::board().cancel(held.id()), "the held load could not be cancelled");
            } else {
                hold.set(false);
            }
            let built = loading.join().unwrap();
            assert_eq!(built, !cancel, "let go, the held build did not build; cancelled, it built on");
            let (_, waiting, held_loads) = hold.waiting();
            assert!(waiting.is_empty(), "a load still waits at the hold");
            assert_eq!(held_loads, vec![(held.id(), at.1.clone())], "the hold does not keep the load it held, at its step");
            if cancel {
                assert_eq!(bar().1, at.1, "the cancelled load began a step past the hold");
            }
        }
    }
}

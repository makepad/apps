//! The Force render mode's study: the selected part (a distance-field solid, or its mesh) under
//! the fixtures and loads picked on it, as it will print. With none selected it is the part
//! studied last, else the plate's first, so the mode always shows a part. A study runs on the
//! app's pool while Design shows the mode: it reads the part's print settings (the slicer's, with the
//! part's own overrides), samples the part's field at one cell size after another (each half the
//! last) and solves each, so the first answer shows in a fraction of the time. A study takes
//! three sizes on its own. At the second it shows the lower safety factor of two alignments,
//! marked provisional, with the next cell size and about how long its check will take. Its
//! worker goes on to that size. The last pair gives the verdict ([`Reading`]): where their
//! safety factors disagree the panel says so, and where they agree it checks the shifted
//! alignment before reporting convergence. Refine offers one more size for a disagreement or
//! a thin feature. Its time is predicted from the active cells and the measured time per cell.
//! A change starts the study over; a stale study stops within one solver iteration.
//!
//! The fixtures and loads are the part's script's (`m.fixed`, `m.force`, ...: see
//! `crate::study_edit`): read from each evaluated plate, and the panel's and the view's
//! changes written back as statements, so a load lives with the design and an edit of the
//! shape or of a load solves again on the models already sampled, each step from its last
//! answer (see [`fab_force::solve::start`]).
//!
//! There is one model: the plate view always draws the real part, and a solve's fields colour it
//! at the point it hit ([`Values`]). The study's own surface is what a click reads (the exact hit
//! on the part, then the quad nearest it) and what the exaggerated deformation draws. Nothing else
//! looks at it.
//!
//! Reading a part so a click can read it is a short job the person waits on, so it goes ahead of
//! the slicer (Interactive). Its other cell sizes follow on their own whether or not anything
//! holds or loads the part (no solve, no line on the board, stale as a study is): the next one,
//! the surface the person sees sharpen, is on screen work (Visible, beside a slice someone asked
//! for), the ones past it Background; a click reads the finest surface, and a later solve starts
//! from those samples. The sampling in a study is Background work like the slicer's, and the
//! panel says when it waits for it. So
//! is the infill floor, found on request once the study is up to date ([`floor`]): the densities
//! where the safety factor falls to 1 and to 2, at up to two cell sizes.
//!
//! The solves run on the AI hub ([`fab_force::hub`], the Force program on the device the hub
//! picks): a study's on this machine's hub at the class someone waits on, each step's analysed
//! level kept here and the coarse levels the hub builds of it kept there in the step's slot, so an
//! edit of a load sends the load and the last answer; the floor's a round at a time as one
//! background batch over every hub that answers (this machine's, and the fleet's nodes with
//! networked AI hub on). The lab holds its link to the hub across studies (the hub's slots live
//! as long as it). With no hub answering, the study waits: the panel says so and offers to start
//! the AI Hub app ([`start_hub`]), and the lab looks for a hub again every [`LOOK_AGAIN`] s while
//! the mode shows. Nothing is solved here.
//!
//! The shifted check moves the grid half a cell along every axis ([`study::shift`]). An
//! extreme at a corner or a hold's edge can move with the cut as much as with a step to
//! finer cells, so the reading takes the lower of the two alignments.
//!
//! As printed, a part's walls and skins are first estimated from its print settings (depth under
//! the surface, [`fab_force::grid`]); once the slicer has sliced the plate as it stands, the
//! study reads the part's own beads from the slice ([`fab_force::sliced`]), regrades the cells it
//! sampled and solves again from the estimate's answer, labelled with the slice's time.
//! As sliced, the study stays on two sizes and checks the last on the shifted grid until
//! FORCE A16. An edit that makes the slice stale goes back to the estimate until the next slice.

use fab::api::*;
use fab_force::bc::Kind;
use fab_force::display::{Display, Marks, Values};
use fab_force::hub::{self, Hub, Job, Solve};
use fab_force::material::{filament_of_type, Infill, Materials, Print, Zone, FILAMENTS};
use fab_force::mesh::Mesh;
use fab_force::results::{carry, Reading, Verdict};
use fab_force::sliced::{Bead, Printed, Role as BeadRole};
use fab_force::solve::Options;
use fab_force::thin::Thickness;
use fab_force::study::{self, Load, Model};
use fab_force::surface::{Pick, Region};
use fab::api::StudyEdit;
use fab_force::{Outcome, Posed, V3};
use makepad_csg_math::Vec3d;
use makepad_widgets::jobs::{JobHandle, JobReporter, JobSpec};
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle};
use makepad_widgets::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::borrow::Cow;
use std::sync::{mpsc, Arc};

/// The finest cells a study goes to (mm): a fifth of a typical line.
const FINEST: f64 = 0.1;
/// A study's own sizes. Only the last pair gives its verdict (FORCE A15 B).
const LEVELS: usize = 3;
/// The step whose lower alignment gives the provisional number while the next size solves.
const PROVISIONAL: usize = 1;
/// An as-sliced study takes two sizes until FORCE A16. The infill floor uses these sizes too.
const HELD: usize = 2;

/// The part a study is of.
#[derive(Clone)]
struct Subject {
    object: String,
    name: String,
    /// Its index among the plate's placed parts (for its print settings and its mesh).
    index: usize,
    shape: Shape,
    lo: V3,
    hi: V3,
}

/// How long the lab waits before it looks for an AI hub again while none answers (s).
const LOOK_AGAIN: f64 = 2.0;

/// The study's state while no AI hub answers: the busy line heads it (up to the colon) and offers
/// to start the hub, the status line says the rest.
const NO_HUB: &str = "No AI hub is running: Force solves on the AI hub. Start it and the study goes on.";
/// How the study's job on the board ends when no AI hub answers.
const NO_HUB_JOB: &str = "no AI hub is running";
/// How the study's job on the board ends while the AI Hub app started from the panel comes up.
const STARTING_JOB: &str = "the AI hub has not started yet";
/// The study's state while the AI Hub app started from the panel comes up.
const STARTING_HUB: &str = "Starting the AI hub: the study goes on once it answers (a first start builds it).";

/// Starts this machine's AI Hub app (the app registry's `aihub`) on its own, so it outlives Fab: a
/// makepad WM session asks its WM (the dock's launch-or-focus); elsewhere `cargo run --release` of
/// it in this checkout (cargo compiles what changed first), else the installation's built binary.
/// What it says when it ends (its exit and its last line: it serves until it quits), where there
/// is a process to watch.
fn start_hub() -> Result<Option<mpsc::Receiver<String>>, String> {
    use makepad_app_registry as registry;
    use std::io::BufRead;
    let app = registry::find("aihub").ok_or("the app registry has no AI Hub")?;
    #[cfg(target_os = "linux")]
    if let Some(socket) = registry::machine::wm_socket() {
        return registry::machine::request_launch(&socket, app.id, false).map(|_| None);
    }
    let target = app.target();
    let mut command = match registry::repo_root().filter(|root| registry::has_sources(&target, root)) {
        Some(root) => {
            let (cargo, args) = registry::run_argv(&target, &root, &[]);
            registry::cargo_command(&cargo, &args, &root)
        }
        None => std::process::Command::new(registry::built_binary(&target, None).ok_or("the AI Hub app is not built here")?),
    };
    registry::child_env::scrub(&mut command);
    registry::child_env::scrub_lane(&mut command);
    command.envs(registry::machine::launch_env(app.id, false));
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let log = child.stderr.take();
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("fab ai hub start".into())
        .spawn(move || {
            // its log read to its end (only the last line kept), then its exit
            let last = log.and_then(|log| std::io::BufReader::new(log).lines().map_while(Result::ok).filter(|l| !l.trim().is_empty()).last());
            let exit = child.wait().map_or_else(|e| e.to_string(), |s| s.to_string());
            let _ = tx.send(match last {
                Some(line) => format!("{exit}: {}", line.trim()),
                None => exit,
            });
            SignalToUI::set_ui_signal();
        })
        .map_err(|e| e.to_string())?;
    Ok(Some(rx))
}

/// The study's solves on step `i`: kept in the hub's slot for it (the steps a study takes on its
/// own), else nothing kept (Refine's size, solved once).
fn keep_slot(i: usize, own: usize) -> String {
    if i < own { format!("fab-force-step-{i}") } else { String::new() }
}

fn shifted_slot(i: usize, own: usize) -> String {
    if i >= own {
        String::new()
    } else if i == PROVISIONAL {
        "fab-force-shifted".into()
    } else {
        format!("fab-force-shifted-{i}")
    }
}

/// The study's own sizes and, when asked for, one more for Refine.
fn sizes(s: &Subject, held: bool, finer: bool) -> Vec<f64> {
    if held {
        study::steps(s.lo, s.hi, HELD + usize::from(finer), FINEST)
    } else {
        study::halvings(study::first(s.lo, s.hi, LEVELS, FINEST), LEVELS + usize::from(finer), FINEST)
    }
}

/// What a part is made of.
#[derive(Clone)]
enum Shape {
    /// One distance-field solid.
    Field(PartSdf),
    /// Triangles (a boundary model, an imported mesh): the placed part's mesh, known by its
    /// mesh's identity and place (the evaluation's own while it is not printed).
    Mesh { key: (usize, [f64; 3]) },
}

impl Subject {
    fn same_field(&self, o: &Subject) -> bool {
        let same = match (&self.shape, &o.shape) {
            (Shape::Field(a), Shape::Field(b)) => a.key == b.key && a.to_printer == b.to_printer,
            (Shape::Mesh { key: a }, Shape::Mesh { key: b }) => a == b,
            _ => false,
        };
        same && self.lo == o.lo && self.hi == o.hi
    }
}

/// What a study sends back as it goes, under its ticket.
enum Note {
    /// The material it studies with: the print settings read (or why not), the filament's type
    /// and the family it is studied as (None: no strength data, and nothing picked).
    /// `sliced`: when the slice the study reads its walls and skins from came (Unix seconds), else
    /// they are estimated from the settings.
    /// `process`: the print profile they come from, in a line.
    Material { print: Result<Print, String>, filament: Option<usize>, typed: Option<String>, sliced: Option<i64>, process: String, zones: Vec<Zone> },
    /// What the part is sampled from, in a line.
    Shape(String),
    /// How thick the part is under its surface, and the cell size of the step that measured it.
    Thickness(Thickness, f64),
    /// The aligned model's active cells, so the panel can update its estimate before the solve.
    Sampled { index: usize, active: usize },
    Step(Box<Step>),
    /// A step solved again on its grid shifted half a cell.
    Shifted { index: usize, last: bool, kept: Box<Kept> },
    /// The AI hub's link closed under the study (the hub quit), and why the solve stopped: the
    /// lab looks for a hub again and the study goes on there.
    HubGone(String),
    Failed(String),
    /// The infill floor as far as it got (see [`floor`]).
    Floor(ForceFloor),
}

struct Step {
    index: usize,
    /// The study's last step (its finest).
    last: bool,
    /// Its model was sampled now (not kept from before, nor regraded).
    sampled: bool,
    model: Arc<Model>,
    /// Active cells on this model, counted on the worker when it is ready.
    active: usize,
    /// The loads' regions on the model, one per load.
    regions: Vec<Region>,
    answer: Option<Answered>,
    /// The analysed level the answer was solved on.
    level: Option<Arc<fab_force::solve::Level>>,
    field_ms: f64,
    solve_ms: f64,
    /// Solved: where, in words ([`solved_on`]).
    on: String,
    /// Solved: its engine and GPU memory.
    ran: Option<hub::Ran>,
    /// The materials key the model's cells follow.
    materials: u64,
    /// This step's solve on the shifted grid follows.
    checking: bool,
}

/// A solve's answer and what the view shows of it.
#[derive(Clone)]
struct Answered {
    answer: Arc<Outcome>,
    values: Arc<Values>,
    display: Arc<Display>,
}

impl Answered {
    /// `answer`'s fields, on the surface of the model it was solved on.
    fn new(answer: Arc<Outcome>, model: &Model) -> Answered {
        let values = Values::of(&answer);
        let display = Display::new(&model.surface, &answer, &values);
        Answered { answer, values: Arc::new(values), display: Arc::new(display) }
    }
}

/// A step as last built: its model and the materials key its cells follow, the answer it last
/// solved to (an edit's solve starts there) and the analysed level that answer was solved on,
/// built for this model (the hub keeps the coarse levels it built of it in the step's slot).
#[derive(Clone)]
struct Kept {
    model: Arc<Model>,
    key: u64,
    answer: Option<Arc<Outcome>>,
    level: Option<Arc<fab_force::solve::Level>>,
}

impl Kept {
    /// The analysed level to solve `posed` on again: the kept one while `posed` is of the same
    /// model and holds the same nodes (only the loads' sizes, directions or places changed).
    fn level_for(&self, model: &Arc<Model>, posed: &Posed) -> Option<Arc<fab_force::solve::Level>> {
        let same = Arc::ptr_eq(&self.model, model) && self.answer.as_ref()?.held == posed.held;
        same.then(|| self.level.clone()).flatten()
    }
}

/// What a study needs from the app: the evaluated plate and the print setup for the state's
/// print settings (both at its start).
pub struct Inputs<'a> {
    pub plate: Option<&'a Arc<fab_model::eval::Evaluated>>,
    pub setup: &'a dyn Fn(&AppState) -> crate::print::CheckSetup,
}

#[derive(Default)]
pub struct ForceLab {
    subject: Option<Subject>,
    /// The subject's loads as its script writes them (printer frame), and the evaluated
    /// plate and part index they were read from.
    loads: Vec<Load>,
    read_from: (usize, usize),
    /// The load picked (in the feature tree, or by a click of the Force tool on it): the plate
    /// part and its index in the part's study.
    focus: Option<(String, usize)>,
    /// What a click of the Force tool would take under the pointer, and the kind and size it
    /// was told for.
    hover: Option<Hover>,
    hover_kind: ForceKind,
    hover_size: f64,
    /// The subject's placed frame: the tip gives a load's centre and normal in its model frame,
    /// as `fab.force` does.
    frame: Option<crate::study_edit::Frame>,
    /// The study's buckling, once its stresses are up to date ([`crate::buckling`]).
    buckle: crate::buckling::Stage,
    /// A change of the study for the app to write into the part's script.
    edit: Option<(String, StudyEdit)>,
    /// The infill floor was asked for (it runs once the study is up to date), and the ticket
    /// of the job that found the one shown (a newer ticket: the study changed since).
    floor_want: bool,
    floor_ticket: u64,
    /// Kept solves the lab let go of (and their levels and models), dropped on the pool at the
    /// end of the event ([`ForceLab::sweep`]), never on the UI thread: a fine study's shifted
    /// check frees about a GiB.
    retired: Vec<Box<dyn std::any::Any + Send>>,
    /// The study's steps as last built and solved (the models are reused while only the loads
    /// change, the analysed levels while the loads hold the same nodes).
    models: Vec<Kept>,
    /// The study's own shifted models by step and which answers belong to the current study.
    shifted: Vec<Option<Kept>>,
    shifted_now: Vec<bool>,
    /// How many steps have shown.
    of: usize,
    /// The study under way reads a slice.
    held: bool,
    /// The model clicks read and the regions are of: the finest sampled so far. The loads'
    /// regions on it, one per load.
    shown: Option<Arc<Model>>,
    regions: Vec<Region>,
    /// The last answer: it stays up while the next one solves.
    answered: Option<Answered>,
    /// The safety factor's reading the panel's rows show (`fab.force` answers the same).
    sf_reading: Option<Reading>,
    /// The job under way only samples the study's surfaces, and a run of it failed (it would fail
    /// again: the next restart tries again).
    surfacing: bool,
    surfaces_failed: bool,
    /// What the study's part is sampled from (a field, a mesh and how closed it is).
    made_of: String,
    /// The study and the infill floor on the job board (the panel's busy line and the status
    /// bar show them, their Cancel stops them). A study's job lasts across the restarts an edit
    /// makes, and ends once the study is up to date, fails, or is stopped.
    busy: Option<JobHandle>,
    floor_busy: Option<JobHandle>,
    /// The study under way: its ticket (a newer one in `live` stops it), the job, its notes.
    live: Arc<AtomicU64>,
    ticket: u64,
    job: Option<TaskHandle<()>>,
    rx: Option<mpsc::Receiver<(u64, Note)>>,
    started: f64,
    /// When the study's first answer showed (seconds after it started).
    first: Option<f64>,
    /// The study is out of date.
    want: bool,
    /// The Force render mode showed at the last event.
    showing: bool,
    /// The study under way has sent a note since it was submitted (it has started).
    heard: bool,
    /// The last job submitted read the part (its solves follow on the same clock).
    reading: bool,
    /// Refine asked for one cell size finer than a study takes on its own; any other restart
    /// drops it.
    finer: bool,
    /// Active cells by step, and ms per active cell at the provisional size or finer: sampling (the
    /// latest fresh sample, a surface's too) and solving (the latest solve).
    active: Vec<usize>,
    field_rate: Option<f64>,
    solve_rate: Option<f64>,
    /// The latest solve at the provisional size or finer: its step, engine and GPU memory.
    ran: Option<(usize, hub::Ran)>,
    /// The next cell size and its predicted seconds while the provisional reading shows.
    checking: Option<(f64, Option<f64>)>,
    /// Refine's offer: its cell size and predicted seconds, the finer solve's and its shifted
    /// check's ([`Self::refine_seconds`]); from Refine's press until its study ends, `refining`,
    /// logged against the time it took.
    offer: Option<(f64, f64, f64)>,
    refining: Option<(f64, f64, f64)>,
    revision: u64,
    /// The AI hub the study solves on (kept across studies: what the hub keeps for the steps
    /// lives as long as this link), the look for one under way, and the wait after a look that
    /// found none (the next one goes when it ends).
    hub: Option<Arc<Hub>>,
    finding: Option<TaskHandle<Hub>>,
    look: Timer,
    /// The AI Hub app started from the panel: what it said when it ended (it serves until it
    /// quits, so an end while the study waits for it is a start that failed).
    hub_app: Option<mpsc::Receiver<String>>,
    /// The slice the study reads its walls and skins from (as printed), and what it was last
    /// looked up at (the plate, the newest toolpaths' revision, as printed).
    slice: Option<Slice>,
    slice_seen: (usize, Option<u64>, bool),
    /// When the slice the study under way reads came (else it estimates from the settings).
    sliced_at: Option<i64>,
    /// The print profile the study under way reads, in a line.
    process: String,
    /// How thick the part is under its surface; measured once a shape's finer surface is sampled.
    thickness: Option<Thickness>,
    /// The cell size the panel reads that against: the study's finest so far.
    thin_h: f64,
}

/// A slice of the plate the study reads its part's walls and skins from: the revision it was
/// taken at, when it was made, the plate's toolpaths, and what of the part it was made from
/// ([`part_key`]).
struct Slice {
    revision: u64,
    at: i64,
    toolpaths: Arc<makepad_fab_slice::live::PlatePaths>,
    part: u64,
}

/// What a slice of plate part `s` follows: the slice settings, the part's shape and place,
/// its own settings and filament. An edit that leaves it alone (a load, another part) leaves
/// the part's slice standing.
fn part_key(ev: &fab_model::eval::Evaluated, s: &Subject, settings: u64) -> u64 {
    let shape = match &s.shape {
        Shape::Field(sdf) => format!("{:?}|{:?}", sdf.key, sdf.to_printer),
        Shape::Mesh { key } => format!("{key:?}"),
    };
    let part = ev.plate.placed_part(s.index).map(|p| format!("{:?}|{}", p.print, p.filament)).unwrap_or_default();
    format!("{settings}|{}|{shape}|{:?}|{:?}|{part}", s.index, s.lo, s.hi).bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// The study's material key: what its cells' materials follow (`zones`: the part's paint key
/// when it has regions with settings of their own, else 0; `slice`: the revision of the slice
/// their walls and skins are read from).
fn materials_key(filament: usize, as_printed: bool, print: &Print, zones: u64, slice: Option<u64>) -> u64 {
    format!("{filament}|{as_printed}|{print:?}|{zones}|{slice:?}").bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// The print settings of plate part `index` as the slicer reads them (reads preset files:
/// off the UI thread), its filament's type, the raft's lift under the part (mm), and its
/// regions with settings of their own.
fn print_of(setup: &crate::print::CheckSetup, ev: &fab_model::eval::Evaluated, index: usize) -> Result<(Print, String, f64, Vec<Zone>), String> {
    let part = ev.plate.placed_part(index).ok_or("the part is no longer on the plate")?;
    let job = makepad_fab_settings::machine::Job::new(setup.job_setup(&ev.plate, &ev.machine)?.build_config());
    let job = if part.print.is_empty() { job } else { job.with_overrides(&part.print) };
    let filament = (part.filament as usize).saturating_sub(1);
    let p = makepad_fab_toolpath::Params::from_job(&job, filament);
    let pattern = p.sparse_pattern.name();
    let infill = Infill::from_setting(pattern).ok_or_else(|| format!("the infill pattern {pattern} has no stiffness model"))?;
    let print = Print {
        layer_height: p.layer_height,
        outer_width: p.w_outer,
        inner_width: p.w_inner,
        wall_loops: p.wall_loops,
        top_layers: p.top_layers,
        bottom_layers: p.bottom_layers,
        density: p.density.clamp(0.0, 1.0),
        infill,
        infill_angle: p.infill_direction.to_degrees(),
    };
    let zones = part.paint.as_ref().map_or(Ok(Vec::new()), |paint| zones_of(&job, &paint.settings, filament))?;
    Ok((print, job.cfg.raw("filament_type", filament).to_string(), p.raft_lift, zones))
}

/// A part's regions with settings of their own (`m.settings`, printer mm) as Force's zones:
/// each region's values over the part's `job`, read as the part's own settings are.
fn zones_of(job: &makepad_fab_settings::machine::Job, regions: &[fab_plate::SettingsRegion], filament: usize) -> Result<Vec<Zone>, String> {
    regions
        .iter()
        .map(|r| {
            let p = makepad_fab_toolpath::Params::from_job(&job.with_overrides(&r.values), filament);
            let sets = |key: &str| r.values.iter().any(|(k, _)| k == key);
            let infill = match sets("sparse_infill_pattern") {
                true => {
                    let pattern = p.sparse_pattern.name();
                    Some(Infill::from_setting(pattern).ok_or_else(|| format!("the infill pattern {pattern} has no stiffness model"))?)
                }
                false => None,
            };
            let density = sets("sparse_infill_density").then(|| p.density.clamp(0.0, 1.0));
            Ok(Zone { at: r.region.at.clone(), bounds: r.region.bounds, density, infill, wall_loops: sets("wall_loops").then_some(p.wall_loops) })
        })
        .collect()
}

/// The walls and skins the slicer printed for plate part `object`, in printer mm (`lift`: the
/// raft under the part; the slice's heights are over it). Sparse infill stays the settings'
/// homogenised density; ironing, supports, skirts and the like carry no load.
fn beads_of(t: &makepad_fab_toolpath::Toolpaths, object: usize, lift: f64) -> Vec<Bead> {
    use makepad_fab_toolpath::Role as R;
    let role = |r: R| match r {
        R::OuterWall | R::InnerWall | R::OverhangWall | R::GapFill | R::FloatingVerticalShell => Some(BeadRole::Wall),
        R::TopSurface | R::BottomSurface | R::InternalSolidInfill | R::Bridge | R::InternalBridge => Some(BeadRole::Skin),
        _ => None,
    };
    let mut beads = Vec::new();
    // a path's segments that extrude, each on its layer's top (`z`) plus its points' own offsets
    let mut add = |p: &makepad_fab_toolpath::ExtrusionPath, z: f64| {
        let Some(role) = role(p.role) else { return };
        for i in 1..p.pts.len() {
            if p.dry.get(i - 1).copied().unwrap_or(false) {
                continue;
            }
            let top = z + 0.5 * (p.z.get(i - 1).copied().unwrap_or(0.0) + p.z.get(i).copied().unwrap_or(0.0));
            let (a, b) = (p.pts[i - 1], p.pts[i]);
            let width = p.width * p.flow.get(i - 1).copied().unwrap_or(1.0);
            beads.push(Bead { a: [a.x_mm(), a.y_mm(), top], b: [b.x_mm(), b.y_mm(), top], width, height: p.height, role });
        }
    };
    for layer in &t.layers {
        for ol in layer.objects.iter().filter(|o| o.object == object && !o.support_only) {
            for island in &ol.islands {
                island.walls.iter().chain(&island.fills).for_each(|p| add(p, layer.print_z - lift));
            }
        }
    }
    for block in t.np_blocks.iter().filter(|b| b.object == object) {
        block.paths.iter().for_each(|p| add(p, block.z - lift));
    }
    beads
}

/// The part's material in a line, and where it is set: its filament type (and what it is
/// studied as when there is no strength data for it), then the tray its print colour is
/// mapped to on the printer, else the process's filament.
fn material_line(state: &AppState, typed: Option<&str>, studied: Option<usize>) -> String {
    let what = match (typed.filter(|t| !t.is_empty()), studied) {
        (Some(t), _) if filament_of_type(t).is_some() => t.to_string(),
        (t, Some(i)) => format!("{}, studied as {}", t.unwrap_or("No filament type"), FILAMENTS[i].name),
        (Some(t), None) => format!("{t} (no strength data here)"),
        (None, None) => "No filament type set".into(),
    };
    use fab::print::colours::colour_word;
    let part = state.force.index.and_then(|i| state.plate.parts.get(i));
    let colors = &state.print.colors;
    let from = match part.and_then(|p| colors.row(&p.print_color)) {
        Some(r) if !r.unassigned() => {
            let at = r.pos.map(|p| p.label(&colors.layout.heads)).unwrap_or_default();
            match r.spool() {
                Some([red, g, b, _]) => format!("the spool in {at} ({})", colour_word([red, g, b])),
                None => format!("the spool in {at}"),
            }
        }
        _ if !colors.layout.live && !colors.layout.known => "the process's filament (no printer linked)".into(),
        Some(r) => format!("the process's filament (no tray picked for {})", r.name),
        None => "the process's filament".into(),
    };
    format!("{what} · {from}")
}

/// The print settings in a line.
fn describe(p: &Print) -> String {
    format!(
        "{:.2} mm layers, {} walls ({:.2} mm), {} top and {} bottom layers, {:.0}% {:?} infill",
        p.layer_height,
        p.wall_loops,
        p.wall_thickness(),
        p.top_layers,
        p.bottom_layers,
        p.density * 100.0,
        p.infill
    )
}

/// The zones after the print settings: each one's density, pattern and wall count, where set.
fn describe_zones(zones: &[Zone]) -> String {
    if zones.is_empty() {
        return String::new();
    }
    let settings: Vec<_> = zones.iter().map(|z| {
        let mut parts = Vec::new();
        if let Some(density) = z.density { parts.push(format!("{:.0}%", density * 100.0)); }
        if let Some(infill) = z.infill { parts.push(format!("{infill:?}")); }
        if let Some(walls) = z.wall_loops { parts.push(format!("{walls} wall{}", if walls == 1 { "" } else { "s" })); }
        parts.join(" ")
    }).collect();
    format!(" · {} zone{} {}", zones.len(), if zones.len() == 1 { "" } else { "s" }, settings.join(", "))
}

/// What is too thin for the cells of `h`, and what Refine (where it has not run and can) does
/// about it.
fn thin_text(thin: fab_force::thin::Thin, h: f64, refined: bool) -> String {
    use fab_force::thin::{reads_high, READS_HIGH_RESOLVED, RESOLVED_CELLS};
    let cells = thin.mm / h;
    let said = format!("about {:.1} mm over {:.0}% of the surface: {cells:.1} cells at {h:.2} mm, not resolved", thin.mm, thin.share * 100.0);
    let high = match reads_high(cells) {
        Some(times) => format!("up to {times:.1}x too high here"),
        None => "over 4x too high here".to_string(),
    };
    let reads = format!("In bending the safety factor can read {high}, {READS_HIGH_RESOLVED:.1}x at {RESOLVED_CELLS:.0} cells");
    let need = thin.mm / RESOLVED_CELLS;
    match (!refined && h * 0.5 >= FINEST).then_some(thin.mm / (h * 0.5)) {
        Some(finer) if finer >= RESOLVED_CELLS => format!("{said}. {reads}. Refine reduces it and resolves them ({finer:.1} cells)"),
        Some(finer) => format!("{said}. {reads}. Refine reduces it ({finer:.1} cells); {RESOLVED_CELLS:.0} need {need:.2} mm cells"),
        None => format!("{said}. {reads}; {RESOLVED_CELLS:.0} need {need:.2} mm cells"),
    }
}

/// The field of a placed part in printer mm: points mapped into the shape's frame
/// (`to_printer` is a rotation, a uniform scale and a move), its distances scaled back.
/// A study's field that gives way between its calls while a job of a higher class waits for a
/// CPU (the slice the person asked for, say), and goes on once it has run. The lattice asks for a
/// slab of samples per call. A lock-free test per call; the budget's lock only when one waits.
fn giving_way(mut field: impl FnMut(&[V3], &mut [f64])) -> impl FnMut(&[V3], &mut [f64]) {
    use makepad_widgets::makepad_platform::thread::{checkpoint, worker_wanted};
    move |ps: &[V3], out: &mut [f64]| {
        if worker_wanted() {
            // (a stale study ends at its next step; the slab in hand is sampled first)
            let _ = checkpoint();
        }
        field(ps, out)
    }
}

fn printer_field(sdf: &PartSdf) -> impl FnMut(&[V3], &mut [f64]) {
    let field = sdf.shape.field();
    let m = sdf.to_printer;
    let col = |c: usize| [m[0][c], m[1][c], m[2][c]];
    let scale = (col(0).iter().map(|v| v * v).sum::<f64>()).sqrt().max(1e-12);
    let cols = [col(0), col(1), col(2)].map(|c| c.map(|v| v / scale));
    let t = [m[0][3], m[1][3], m[2][3]];
    let mut q = Vec::new();
    move |ps: &[V3], out: &mut [f64]| {
        q.clear();
        q.extend(ps.iter().map(|p| {
            let d = [p[0] - t[0], p[1] - t[1], p[2] - t[2]];
            let r = |c: [f64; 3]| (c[0] * d[0] + c[1] * d[1] + c[2] * d[2]) / scale;
            Vec3d::new(r(cols[0]), r(cols[1]), r(cols[2]))
        }));
        field.distance_many(&q, out);
        out.iter_mut().for_each(|d| *d *= scale);
    }
}

/// The 1-based tag a load kind gives its region's cells.
fn tag_of(k: ForceKind) -> u16 {
    match k {
        ForceKind::Fixed => 1,
        ForceKind::Force => 2,
        ForceKind::Pressure => 3,
        ForceKind::Torque => 4,
    }
}

/// What a click of the Force tool would take where the pointer is: on which model's surface,
/// the quad and whether Alt flips the reading, and the reading with its region.
struct Hover {
    model: Arc<Model>,
    /// Where on the part the pointer is, and the quad nearest it.
    at: V3,
    quad: u32,
    flip: bool,
    pick: Pick,
    region: Region,
}

impl Hover {
    /// The reading of a click at `at`, on the quad nearest it: the surface's own, or with `flip`
    /// the other one. None on a model with no surface.
    fn at(model: &Arc<Model>, at: V3, flip: bool) -> Option<Hover> {
        model.surface.nearest(at).map(|quad| Hover::read(model, at, quad, flip))
    }

    fn read(model: &Arc<Model>, at: V3, quad: u32, flip: bool) -> Hover {
        let (pick, region) = match model.surface.reading(quad) {
            (pick, region) if !flip => (pick, region),
            (Pick::Hole, _) => (Pick::Face, model.surface.grow(quad, Pick::Face)),
            (Pick::Face, _) => (Pick::Hole, model.surface.grow(quad, Pick::Hole)),
        };
        Hover { model: model.clone(), at, quad, flip, pick, region }
    }

    /// What a click adds, in the pointer's tip: where, and what it does there (`size`: the new
    /// load's, as the Force strip's dial sets it).
    fn text(&self, kind: ForceKind, size: f64) -> String {
        let r = &self.region;
        let what = match (self.pick, r.cylinder) {
            (Pick::Hole, Some((_, _, radius))) => format!("Hole wall, {:.1} mm across", 2.0 * radius),
            (Pick::Hole, None) => format!("Hole wall, {:.0} mm\u{b2}", r.area),
            (Pick::Face, _) => format!("Face, {:.0} mm\u{b2}", r.area),
        };
        let size = fab::ui::force::size_text(size);
        let does = match kind {
            ForceKind::Fixed => "click to hold it".to_string(),
            ForceKind::Force => format!("click to push {size} N"),
            ForceKind::Pressure => format!("click to press {size} MPa on it"),
            ForceKind::Torque => format!("click to turn it {size} N mm"),
        };
        let other = match self.pick {
            Pick::Hole => "Alt reads it as a face",
            Pick::Face => "Alt reads it as a hole's wall",
        };
        format!("{what}: {does}\n{other}")
    }
}

/// What shows the study: the Force panel, the plate view (the part's colours) and the tool overlay over
/// it (the loads, the key, the study's state).
pub const TABS: &[&[LiveId]] = &[ids!(tab_force), ids!(viewport), ids!(tool_overlay)];

/// What the study still needs before it runs, in a line (None: something holds the part and
/// something loads it).
fn missing(loads: &[Load]) -> Option<String> {
    let held = loads.iter().any(|l| l.kind == Kind::Fixed);
    let loaded = loads.iter().any(|l| l.kind != Kind::Fixed);
    let tool = format!("Force tool ({})", fab::ui::keymap::shortcut(fab::ui::keymap::ShortcutId::Force));
    match (held, loaded) {
        (true, true) => None,
        (false, true) => Some(format!("Add a fixture: {tool} › Fixed, then click the face or hole where the part is bolted or clamped (or write m.fixed(..) in its script).")),
        (true, false) => Some(format!("Add a load: {tool} › Force, Pressure or Torque, then click the face or hole it pushes on (or write m.force(..) in its script).")),
        (false, false) => Some(format!("Add a fixture and a load: {tool} › Fixed, then click where the part is held; › Force, then click where it is pushed (or write m.fixed(..) and m.force(..) in its script).")),
    }
}

impl ForceLab {
    fn own(&self) -> usize {
        if self.held { HELD } else { LEVELS }
    }

    fn current_shifted(&self, index: usize) -> Option<&Kept> {
        self.shifted.get(index)?.as_ref().filter(|_| self.shifted_now[index])
    }

    /// The study is out of date: start it over (now if the mode shows).
    fn restart(&mut self) {
        self.want = true;
        self.surfaces_failed = false;
        self.reading = false;
        self.finer = false;
        self.refining = None;
        self.stop();
    }

    /// The study under way goes stale: the solve it waits on is cancelled on the hub, and its
    /// buckling with it.
    fn stop(&mut self) {
        self.live.fetch_add(1, Ordering::Relaxed);
        self.buckle.stale();
    }

    /// Buckling takes several solves per step. It reads the first two and the second shifted,
    /// or the last pair and the shifted check when the study reads a slice.
    fn buckle_steps(&self) -> Option<Vec<crate::buckling::Input>> {
        let input = |k: &Kept, keep: String| {
            Some(crate::buckling::Input { level: k.level.clone()?, answer: k.answer.clone()?, keep: if keep.is_empty() { "fab-force-buckling".into() } else { keep }, h: k.model.h })
        };
        let fine = self.of.checked_sub(1)?;
        let fine = if self.held { fine } else { fine.min(PROVISIONAL) };
        let own = self.own();
        let mut steps = Vec::new();
        if let Some(coarse) = fine.checked_sub(1) {
            steps.push(input(self.models.get(coarse)?, keep_slot(coarse, own))?);
        }
        let at = self.models.get(fine)?;
        steps.push(input(at, keep_slot(fine, own))?);
        let shifted = self.current_shifted(fine).filter(|k| fine > 0 && (k.model.h - at.model.h).abs() < 1e-9);
        if let Some(i) = shifted.and_then(|k| input(k, shifted_slot(fine, own))) {
            steps.push(i);
        }
        Some(steps)
    }

    /// The panel's Buckling row (after the safety factor's) and its line in the busy line's tip.
    fn show_buckling(&self, state: &mut AppState) {
        let f = &mut state.force;
        if !f.stats.iter().any(|(n, _)| n == "Largest von Mises") {
            return;
        }
        let thin = self.thickness.as_ref().and_then(|t| t.thin_at(self.thin_h)).map(|t| t.mm / self.thin_h);
        let row = self.buckle.row(thin);
        match f.stats.iter_mut().find(|(n, _)| n == "Buckling") {
            Some((_, v)) => *v = row,
            None => {
                let at = f.stats.iter().rposition(|(n, _)| matches!(n.as_str(), "Safety factor" | "Near the fixtures" | "Thin features")).map_or(0, |i| i + 1);
                f.stats.insert(at, ("Buckling".into(), row));
            }
        }
        f.how.retain(|(k, _)| k != "Buckling");
        if let Some(how) = self.buckle.how() {
            f.how.push(("Buckling".into(), how));
        }
    }

    /// Whether the AI hub the study solves on answers; else a look for one starts (at once,
    /// then [`LOOK_AGAIN`] s after the last look found none).
    fn hub_ready(&mut self, cx: &mut Cx) -> bool {
        if self.hub.as_ref().is_some_and(|h| h.is_open()) {
            return true;
        }
        // its link closed (the hub quit): what the hub kept for the study went with it
        self.hub = None;
        if self.finding.is_none() && self.look.is_empty() {
            let look = cx.task_pool().submit_named(Class::Interactive, "fab force hub", || {
                let hub = Hub::find();
                SignalToUI::set_ui_signal();
                hub
            });
            if let Ok(h) = look {
                self.finding = Some(h);
            }
        }
        false
    }

    /// A look for the AI hub came back: one answers, or the study waits for one (the panel
    /// says so and offers to start it). Whether the panel changed.
    fn take_look(&mut self, cx: &mut Cx, state: &mut AppState, found: Option<Hub>) -> bool {
        match found.filter(|h| h.is_open()) {
            Some(hub) => {
                self.hub = Some(Arc::new(hub));
                self.hub_app = None;
                let f = &mut state.force;
                let changed = f.hub != ForceHub::Answering;
                f.hub = ForceHub::Answering;
                changed
            }
            None => {
                self.look = cx.start_timeout(LOOK_AGAIN);
                // a job that waits for a hub ends: the panel says why
                let ended = self.busy.is_some() || self.floor_busy.is_some();
                let why = if state.force.hub == ForceHub::Starting { STARTING_JOB } else { NO_HUB_JOB };
                self.end_busy(state, End::Failed(why.into()));
                if self.floor_busy.is_some() {
                    self.end_busy(state, End::FloorFailed(why.into()));
                }
                // the panel's line stays as it was told (no hub, the hub starting, why it did not)
                let f = &mut state.force;
                if f.hub != ForceHub::Answering {
                    return ended;
                }
                f.hub = ForceHub::Missing;
                f.solving = false;
                f.refine = None;
                f.status = NO_HUB.into();
                true
            }
        }
    }

    /// Ends the study's or the floor's job on the board, as `end` says, and stops showing it.
    fn end_busy(&mut self, state: &mut AppState, end: End) {
        let slot = match end {
            End::Done | End::Failed(_) | End::Stopped => &mut self.busy,
            End::FloorDone | End::FloorFailed(_) | End::FloorStopped => &mut self.floor_busy,
        };
        let Some(job) = slot.take() else { return };
        match end {
            End::Done | End::FloorDone => job.done(),
            End::Failed(why) | End::FloorFailed(why) => job.fail(why, false),
            End::Stopped | End::FloorStopped if job.is_cancelled() => job.cancelled(),
            // the mode went off or the part went away: nobody asked, but it did stop
            End::Stopped | End::FloorStopped => job.fail("stopped: the study is no longer shown", true),
        }
        self.show_busy(state);
    }

    /// The job the panel's busy line follows: the floor's while it runs, else the study's.
    fn show_busy(&self, state: &mut AppState) {
        state.force.busy = self.floor_busy.as_ref().or(self.busy.as_ref()).map(|j| j.id());
    }

    /// Takes the plate's newest slice when it is the slice of the plate as it stands and
    /// another than the study's; drops the study's when the part's own inputs changed since
    /// (back to the settings' estimate). Whether the study's slice changed.
    fn follow_slice(&mut self, ev: Option<&Arc<fab_model::eval::Evaluated>>, as_printed: bool) -> bool {
        let (Some(ev), Some(subject), true) = (ev, &self.subject, as_printed) else { return self.slice.take().is_some() };
        let key = crate::live_slice::slice_key(ev);
        let part = part_key(ev, subject, key.1);
        match (crate::live_slice::toolpaths_of(key), &mut self.slice) {
            // the same plan again (a revision that changed nothing of it)
            (Some(now), Some(slice)) if Arc::ptr_eq(&now.toolpaths, &slice.toolpaths) => {
                slice.part = part;
                false
            }
            (Some(now), _) => {
                self.slice = Some(Slice { revision: now.revision, at: now.at, toolpaths: now.toolpaths, part });
                true
            }
            // not sliced as it stands yet: the slice read stays while the part's own inputs do
            (None, Some(slice)) if slice.part != part => {
                self.slice = None;
                true
            }
            (None, _) => false,
        }
    }

    /// A Force panel or plate view action; whether what shows the study changed.
    pub fn action(&mut self, action: &ForceAction, state: &mut AppState) -> bool {
        let f = &mut state.force;
        match action {
            // after a Cancel: on from where it stopped; else one cell size finer
            ForceAction::Refine => {
                let finer = !f.stopped && f.refine.is_some();
                f.stopped = false;
                self.restart();
                self.finer = finer;
                self.refining = self.offer.filter(|_| finer);
                if let Some((h, solve, check)) = self.refining {
                    let us = |rate: Option<f64>| rate.map_or("?".into(), |r| format!("{:.1} us", r * 1e3));
                    log!(
                        "fab force: Refine to {h:.2} mm cells, {} predicted: its solve {solve:.0} s and its shifted check {check:.0} s (sampled at {} and solved at {} an active cell)",
                        about(solve + check),
                        us(self.field_rate),
                        us(self.solve_rate)
                    );
                }
            }
            ForceAction::StudyAs(i) if *i < FILAMENTS.len() && f.study_as != Some(*i) => {
                f.study_as = Some(*i);
                self.restart();
            }
            ForceAction::AsPrinted(p) if *p != f.as_printed => {
                f.as_printed = *p;
                self.restart();
            }
            ForceAction::Place { point, flip, edit } => {
                let (Some(model), Some(subject)) = (&self.shown, &self.subject) else { return false };
                let Some(quad) = model.surface.nearest(*point) else { return false };
                // a new statement of the part's script, read as the tip said; it shows when the
                // plate comes back
                // on a load: picks it (the strip's dial and Delete then act on it), a double-click
                // opens its typed editor
                if let Some(i) = self.load_at(quad) {
                    self.focus = Some((subject.object.clone(), i));
                    if *edit {
                        f.typing = Some(i);
                    }
                    self.sync_loads(state);
                    return true;
                }
                if *edit {
                    return false;
                }
                let hover = match self.hover.take() {
                    Some(h) if Arc::ptr_eq(&h.model, model) && h.quad == quad && h.flip == *flip => h,
                    _ => Hover::read(model, *point, quad, *flip),
                };
                let kind = state.ui.force_kind;
                let pick = match hover.pick {
                    Pick::Face => ForcePick::Face,
                    Pick::Hole => ForcePick::Hole,
                };
                let value = kind.value(state.ui.force_size[kind as usize], hover.region.normal);
                self.edit = Some((subject.object.clone(), StudyEdit::Add { kind, pick, at: *point, value }));
                self.hover = Some(hover);
            }
            ForceAction::Hover { at } => {
                // the reading changes (Some), or stays: within the region shown, with the same
                // Alt, a click reads the same
                let next = match (at, self.shown.clone()) {
                    (Some((point, flip)), Some(model)) => model.surface.nearest(*point).and_then(|quad| {
                        let same = self.hover.as_mut().filter(|h| Arc::ptr_eq(&h.model, &model) && h.flip == *flip && (h.quad == quad || h.region.faces.contains(&quad)));
                        match same {
                            Some(h) => {
                                h.at = *point;
                                None
                            }
                            None => Some(Some(Hover::read(&model, *point, quad, *flip))),
                        }
                    }),
                    (Some(_), None) => None,
                    (None, _) => self.hover.is_some().then_some(None),
                };
                let Some(next) = next else { return false };
                self.hover = next;
                self.show_hover(state);
            }
            ForceAction::Type(t) => f.typing = t.filter(|i| *i < f.loads.len()),
            ForceAction::Remove(i) if *i < f.loads.len() => {
                let Some(subject) = &self.subject else { return false };
                f.typing = None;
                self.edit = Some((subject.object.clone(), StudyEdit::Remove { index: *i }));
            }
            ForceAction::Value(i, v) if *i < f.loads.len() => {
                let Some(subject) = &self.subject else { return false };
                f.loads[*i].value = *v;
                self.edit = Some((subject.object.clone(), StudyEdit::Set { index: *i, value: *v }));
            }
            ForceAction::Focus { object, index } => {
                let focus = index.map(|i| (object.clone(), i));
                if focus == self.focus {
                    return false;
                }
                self.focus = focus;
                f.typing = None;
                self.sync_loads(state);
            }
            ForceAction::Field(x) if *x != f.field => f.field = *x,
            ForceAction::Deform(d) if d.clamp(0.0, 1.0) != f.deform => f.deform = d.clamp(0.0, 1.0),
            ForceAction::Clear if f.loads.iter().any(|l| l.top) => {
                let Some(subject) = &self.subject else { return false };
                self.edit = Some((subject.object.clone(), StudyEdit::Clear));
            }
            // the AI Hub app, while none answers: the lab looks for it at once, and again until
            // it does
            ForceAction::StartHub if f.hub == ForceHub::Missing => match start_hub() {
                Ok(ended) => {
                    self.hub_app = ended;
                    self.look = Timer::empty();
                    f.hub = ForceHub::Starting;
                    f.status = STARTING_HUB.into();
                }
                Err(e) => f.status = format!("The AI hub did not start: {e}"),
            },
            ForceAction::FindFloor if !f.floor.running => {
                f.floor = match (f.as_printed, Model::posed(&self.loads)) {
                    (false, _) => ForceFloor { status: "The infill floor is a question of the part as printed: turn off Solid plastic (under More).".into(), ..Default::default() },
                    (true, false) => ForceFloor { status: "The infill floor needs the part held and loaded.".into(), ..Default::default() },
                    (true, true) => {
                        self.floor_want = true;
                        ForceFloor { status: "The infill floor follows the study.".into(), running: true, rows: Vec::new() }
                    }
                };
            }
            _ => return false,
        }
        true
    }

    /// A change of the study for the app to write into the part's script: the plate part
    /// and the change.
    pub fn take_edit(&mut self) -> Option<(String, StudyEdit)> {
        self.edit.take()
    }

    /// Whether a `fab.force` call is answered now: the study is up to date with the plate as
    /// the lab last read it, or it cannot go on by itself (not shown, no part, nothing to
    /// solve, no AI hub, stopped, failed); a hub started from the panel is waited for. The
    /// infill floor and the surfaces sampled after a study are no part of it. The app asks once
    /// the lab has seen an event since the call. O(1).
    pub fn settled(&self, state: &AppState) -> bool {
        if !state.force_shown() || self.subject.is_none() || state.force.hub == ForceHub::Missing {
            return true;
        }
        !self.want && (self.job.is_none() || self.surfacing || state.force.floor.running)
    }

    /// The study as `fab.force` answers it: what the Force view and its panel show (their rows
    /// word for word, the same numbers), points and directions in the part's model frame (mm),
    /// so they read against its script. O(loads + the panel's rows); nothing is solved here.
    pub fn report(&self, state: &AppState, plate: Option<&Arc<fab_model::eval::Evaluated>>) -> fab_ai::out::S {
        use fab_ai::out::{arr, flag, mm, mm3, n, name, obj, opt, s, S};
        let f = &state.force;
        if !state.force_shown() {
            return obj(vec![("state", name("off")), ("why", s("The Force view is not shown: it solves in Design's Force render mode."))]);
        }
        let Some(subject) = &self.subject else {
            return obj(vec![("state", name("no_part")), ("why", s(f.note.clone()))]);
        };
        let frame = plate.and_then(|ev| ev.placed_frame(subject.index)).and_then(crate::study_edit::Frame::new);
        let point = |p: V3| mm3(frame.map_or(p, |fr| fr.model_point(p)));
        let turn = |v: V3| frame.map_or(v, |fr| fr.model_turn(v));
        let state_word = if f.hub != ForceHub::Answering {
            "no_hub"
        } else if missing(&self.loads).is_some() {
            "needs_loads"
        } else if f.stopped {
            "stopped"
        } else if self.surfaces_failed || self.answered.is_none() {
            "failed"
        } else {
            "done"
        };
        let mut fields = vec![
            ("state", name(state_word)),
            ("status", s(f.status_line())),
            ("part", s(f.part.clone())),
            ("object", s(subject.object.clone())),
            ("material", s(f.material.clone())),
            ("as_printed", flag(f.as_printed)),
            ("print", s(f.print.clone())),
        ];
        let loads = f.loads.iter().map(|l| {
            let value = match l.kind {
                ForceKind::Fixed => S::Nil,
                ForceKind::Force => arr(turn(l.value).into_iter().map(n)),
                ForceKind::Pressure | ForceKind::Torque => n(l.value[0]),
            };
            obj(vec![
                ("kind", name(&l.kind.label().to_lowercase())),
                ("on", s(l.on.clone())),
                ("value", value),
                ("unit", s(l.kind.unit())),
                ("area_mm2", n(l.area)),
                ("centre", point(l.centroid)),
                ("normal", arr(turn(l.normal).into_iter().map(n))),
            ])
        });
        fields.push(("loads", arr(loads)));
        if let Some(a) = self.answered.as_ref().filter(|_| Model::posed(&self.loads)) {
            let (a, r) = (&a.answer, &a.answer.results);
            let g = &a.grid;
            let verdict = |v: Verdict| match v {
                Verdict::Unchecked(_) => name("unchecked"),
                Verdict::Converged { .. } => name("converged"),
                Verdict::Apart { .. } => name("apart"),
            };
            if let Some(reading) = self.sf_reading {
                fields.push(("safety_factor", opt(reading.sf().map(n))));
                fields.push(("verdict", verdict(reading.away)));
                fields.push(("provisional", flag(self.checking.is_some())));
                // the readings the reported value is from (it carries the margin)
                let read = match reading.away {
                    Verdict::Unchecked(v) => obj(vec![("fine", n(v))]),
                    Verdict::Converged { fine, coarse, shifted } | Verdict::Apart { fine, coarse, shifted } => {
                        obj(vec![("fine", n(fine)), ("coarse", n(coarse)), ("shifted", opt(shifted.map(n)))])
                    }
                };
                fields.push(("safety_factor_read", read));
                let near = reading.near.value().unwrap_or_else(|| match reading.near {
                    Verdict::Unchecked(v) | Verdict::Converged { fine: v, .. } | Verdict::Apart { fine: v, .. } => v,
                });
                fields.push(("near_fixtures", obj(vec![("safety_factor", n(near)), ("verdict", verdict(reading.near))])));
            }
            if let Some((at, sf)) = f.scene.as_ref().and_then(|sc| sc.weakest) {
                fields.push(("weakest", obj(vec![("at", point(at.map(f64::from))), ("safety_factor", n(sf))])));
            }
            let cell = |c: usize| point(g.cell_center(c));
            let read = |q: Option<u32>| s(if q.is_some() { "surface" } else { "cell" });
            fields.push(("von_mises_mpa", obj(vec![("max", n(r.max_vm.0)), ("at", cell(r.max_vm.1)), ("read", read(r.at_quad[0]))])));
            fields.push(("principal_mpa", obj(vec![("max", n(r.max_p1.0)), ("at", cell(r.max_p1.1)), ("read", read(r.at_quad[1]))])));
            fields.push(("movement", obj(vec![("max", mm(r.max_u.0)), ("at", point(g.node_pos(r.max_u.1)))])));
            if let Some((_, cells)) = f.how.iter().find(|(k, _)| k == "Cells") {
                fields.push(("cells", s(cells.clone())));
            }
            // solved: no solve the rows read stopped above its tolerance (else their Solve row
            // says which, and the figures are approximate); this answer's own iterations and residual
            let unsolved = f.stats.iter().any(|(k, _)| k == "Solve");
            fields.push(("solve", obj(vec![("solved", flag(!unsolved)), ("iterations", n(a.solution.iterations as f64)), ("residual", n(a.solution.residual))])));
            // the Buckling row's own words, its state, and (once found) the factor it says
            use crate::buckling::Phase;
            let b = &self.buckle.phase;
            let word = match b {
                Phase::Waiting => "waiting",
                Phase::Running(_) => "running",
                Phase::Stale => "stale",
                Phase::NoCompression => "no_compression",
                Phase::Done(_) => "done",
                Phase::Failed(_) => "failed",
            };
            let mut buckling = vec![("state", name(word))];
            if let Some((_, row)) = f.stats.iter().find(|(k, _)| k == "Buckling") {
                buckling.push(("row", s(row.clone())));
            }
            if let Phase::Done(found) = b {
                buckling.push(("factor", opt(found.stated().map(n))));
                // solved: every step's eigensolver settled, on stresses solved to their tolerance
                buckling.push(("solved", flag(found.settled && found.stresses_unsolved.is_none())));
                buckling.push(("verdict", verdict(found.verdict())));
                buckling.push(("coarse", opt(found.coarse.map(n))));
                buckling.push(("fine", n(found.fine)));
                buckling.push(("shifted", opt(found.shifted.map(n))));
            }
            if let Some((_, how)) = f.how.iter().find(|(k, _)| k == "Buckling") {
                buckling.push(("how", s(how.clone())));
            }
            fields.push(("buckling", obj(buckling)));
        }
        fields.push(("rows", arr(f.stats.iter().map(|(label, value)| obj(vec![("label", s(label.clone())), ("value", s(value.clone()))])))));
        obj(fields)
    }

    /// The subject's loads as its script now writes them (`ev`, the evaluated plate): new
    /// ones start the study over on the models already sampled. Whether the panel's rows
    /// changed.
    fn read_loads(&mut self, state: &mut AppState, ev: &Arc<fab_model::eval::Evaluated>) -> bool {
        let Some(s) = &self.subject else { return false };
        let from = (Arc::as_ptr(ev) as usize, s.index);
        // the plate the UI shows and the evaluated one may differ for a moment: read the part
        // only from a plate that has it at its index
        if from == self.read_from || ev.plate.placed_part(s.index).is_none_or(|p| p.object != s.object) {
            return false;
        }
        self.read_from = from;
        let read = match (ev.placed_study(s.index), ev.placed_frame(s.index).and_then(crate::study_edit::Frame::new)) {
            (Some(study), Some(frame)) => crate::study_edit::loads(study, &frame),
            _ => Vec::new(),
        };
        let (loads, mut rows): (Vec<Load>, Vec<ForceLoad>) = read.into_iter().unzip();
        let same_rows = rows.len() == state.force.loads.len() && rows.iter().zip(&state.force.loads).all(|(a, b)| a.kind == b.kind && a.on == b.on && a.top == b.top && a.value == b.value);
        if loads == self.loads && same_rows {
            return false;
        }
        if loads != self.loads {
            self.loads = loads;
            self.regions = self.shown.as_ref().map(|m| m.conditions(&self.loads).into_iter().map(|c| c.region).collect()).unwrap_or_default();
            self.restart();
        }
        for (row, old) in rows.iter_mut().zip(&state.force.loads) {
            (row.area, row.centroid, row.normal, row.axis) = (old.area, old.centroid, old.normal, old.axis);
        }
        state.force.loads = rows;
        self.sync_loads(state);
        true
    }

    /// What a click of the Force tool would take, in the pointer's tip and drawn over the loads.
    fn show_hover(&mut self, state: &mut AppState) {
        let kind = state.ui.force_kind;
        // a load's region: its centre (mm) and outward normal in the part's model frame, the
        // numbers `fab.force` gives
        let place = |l: &ForceLoad| {
            let (c, n) = self.frame.map_or((l.centroid, l.normal), |f| (f.model_point(l.centroid), f.model_turn(l.normal)));
            let list = |v: V3, k: f64| v.map(|x| ((x * k).round() / k + 0.0).to_string()).join(", ");
            format!("\ncentre [{}] mm, normal [{}]", list(c, 100.0), list(n, 1000.0))
        };
        state.force.hover = match &self.hover {
            Some(h) => match self.load_at(h.quad).and_then(|i| state.force.loads.get(i)) {
                Some(l) if l.kind == ForceKind::Fixed => format!("{}: click to pick it{}", fab::ui::force::load_name(l), place(l)),
                Some(l) => format!("{}, {} {}: click to pick it{}", fab::ui::force::load_name(l), fab::ui::force::size_text(l.size()), l.kind.unit(), place(l)),
                None => h.text(kind, state.ui.force_size[kind as usize]),
            },
            None => String::new(),
        };
        self.hover_kind = kind;
        self.hover_size = state.ui.force_size[kind as usize];
        self.sync_loads(state);
    }

    /// The load whose region holds `quad` on the model shown (the last one drawn over the
    /// others there).
    fn load_at(&self, quad: u32) -> Option<usize> {
        self.regions.iter().rposition(|r| r.faces.contains(&quad))
    }

    /// The loads' regions on the model shown into the panel's rows, and the scene's marks (the
    /// focused load's over the others).
    fn sync_loads(&mut self, state: &mut AppState) {
        let (tool, kind) = (state.force_tool(), state.ui.force_kind);
        let f = &mut state.force;
        let object = self.subject.as_ref().map(|s| s.object.as_str());
        f.focus = self.focus.as_ref().filter(|(o, i)| Some(o.as_str()) == object && *i < f.loads.len()).map(|(_, i)| *i);
        let Some(model) = self.shown.clone() else { return };
        let mut marks = Marks::new(&model.grid);
        let mut order: Vec<usize> = (0..f.loads.len().min(self.regions.len())).collect();
        order.sort_by_key(|i| f.focus == Some(*i));
        for i in order {
            let (row, r) = (&mut f.loads[i], &self.regions[i]);
            row.area = r.area;
            row.centroid = r.centroid;
            row.normal = r.normal;
            row.axis = r.cylinder.map(|(_, a, _)| a);
            marks.mark(&model.surface, &r.faces, tag_of(row.kind) + if f.focus == Some(i) { 4 } else { 0 });
        }
        // what a click of the Force tool would take, over the loads (drawn as a picked one)
        if let Some(h) = self.hover.as_ref().filter(|h| Arc::ptr_eq(&h.model, &model) && tool) {
            // on a load, that load lights up as picked
            let kind = self.load_at(h.quad).and_then(|i| f.loads.get(i)).map_or(kind, |l| l.kind);
            marks.mark(&model.surface, &h.region.faces, tag_of(kind) + 4);
        }
        // with nothing to solve, the part as it is, and what the study still needs; else the last
        // answer stays up while the next one solves
        if let Some(why) = missing(&self.loads) {
            state.force.status = why;
            state.force.stats.clear();
            self.sf_reading = None;
            self.checking = None;
            state.force.how.retain(|(k, _)| k == "Sampled from" || k == "Cells");
        }
        self.publish(state, Arc::new(marks), Model::posed(&self.loads));
    }

    fn publish(&mut self, state: &mut AppState, marks: Arc<Marks>, solved: bool) {
        let Some(s) = &self.subject else { return };
        self.revision += 1;
        let answered = self.answered.as_ref().filter(|_| solved);
        let weakest = answered.and_then(|a| {
            let (sf, c) = a.answer.results.min_sf;
            sf.is_finite().then(|| (a.answer.grid.cell_center(c).map(|v| v as f32), sf))
        });
        let peak = answered.and_then(|a| {
            let (vm, c) = a.answer.results.max_vm;
            (vm > 0.0).then(|| a.answer.grid.cell_center(c).map(|v| v as f32))
        });
        state.force.scene = Some(Arc::new(ForceScene {
            revision: self.revision,
            answer: answered.map(|a| ForceAnswer { values: a.values.clone(), display: a.display.clone() }),
            marks,
            lo: s.lo.map(|v| v as f32),
            hi: s.hi.map(|v| v as f32),
            weakest,
            peak,
        }));
    }

    /// One of the study's own cell sizes has no model yet.
    fn surfaces_left(&self) -> bool {
        let Some(s) = self.subject.as_ref().filter(|_| !self.surfaces_failed && self.shown.is_some()) else { return false };
        sizes(s, self.held, false).iter().enumerate().any(|(i, h)| self.models.get(i).is_none_or(|k| (k.model.h - h).abs() > 1e-9))
    }

    /// The part to study now: the selected one, else the one studied last while it is on the
    /// plate, else the plate's first, so the mode shows a part whenever there is one.
    fn subject_of(state: &AppState, plate: Option<&Arc<fab_model::eval::Evaluated>>, last: Option<&Subject>) -> Result<Subject, String> {
        let parts = &state.plate.parts;
        let kept = last.and_then(|l| match parts.get(l.index) {
            Some(p) if p.object == l.object => Some(l.index),
            _ => parts.iter().position(|p| p.object == l.object),
        });
        let i = fab::ui::parts::active_part(state).or(kept).or((!parts.is_empty()).then_some(0)).ok_or("Nothing on the plate to study yet.")?;
        let p = &parts[i];
        let shape = match &p.sdf {
            Some(sdf) => Shape::Field(sdf.clone()),
            None => {
                let ev = plate.ok_or("Reading the part...")?;
                let part = ev.plate.placed_part(i).filter(|q| q.object == p.object).ok_or("Reading the part...")?;
                if part.mesh.triangles.is_empty() {
                    return Err(format!("{} has no solid to study.", p.name));
                }
                let key = match ev.frames.get(i).filter(|_| i < ev.plate.parts.len()) {
                    Some(f) => (Arc::as_ptr(&f.local) as usize, f.offset),
                    None => (Arc::as_ptr(ev) as usize, [0.0; 3]),
                };
                Shape::Mesh { key }
            }
        };
        Ok(Subject { object: p.object.clone(), name: p.name.clone(), index: i, shape, lo: p.min_mm, hi: p.max_mm })
    }

    /// Keep the study going while Design shows the Force render mode; call on every event.
    /// Whether what shows the study changed (the app redraws [`TABS`] at once: an action from
    /// here would run this again in the next round of the action pass, a feedback loop).
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event, state: &mut AppState, inputs: Inputs) -> bool {
        if state.force.filaments.is_empty() {
            state.force.filaments = FILAMENTS.iter().map(|f| f.name.to_string()).collect();
        }
        let shown = state.force_shown();
        let mut changed = false;
        // the mode went off: the study under way stops (its pool time and its solve on the hub
        // go) and starts over when it shows again
        if std::mem::replace(&mut self.showing, shown) && !shown && self.job.is_some() {
            self.restart();
            self.end_busy(state, End::Stopped);
        }
        if shown {
            match Self::subject_of(state, inputs.plate, self.subject.as_ref()) {
                Ok(s) => {
                    let same = self.subject.as_ref().is_some_and(|o| o.same_field(&s) && o.index == s.index);
                    if !same {
                        // another part: its own loads, read from its script below
                        if self.subject.as_ref().is_none_or(|o| o.object != s.object) {
                            state.force.loads.clear();
                            self.loads.clear();
                        }
                        self.read_from = (0, usize::MAX);
                        self.regions.clear();
                        state.force.part = s.name.clone();
                        state.force.index = Some(s.index);
                        state.force.note.clear();
                        self.retired.push(Box::new(std::mem::take(&mut self.models)));
                        self.thickness = None;
                        self.retired.push(Box::new(std::mem::take(&mut self.shifted)));
                        self.shifted_now.clear();
                        self.checking = None;
                        self.active.clear();
                        self.shown = None;
                        self.answered = None;
                        self.sf_reading = None;
                        state.force.scene = None;
                        // another study: its buckling starts afresh
                        self.buckle.stale();
                        self.buckle.phase = crate::buckling::Phase::Waiting;
                        self.subject = Some(s);
                        self.restart();
                        changed = true;
                    }
                }
                Err(why) => {
                    if self.subject.is_some() || state.force.note != why {
                        self.subject = None;
                        self.retired.push(Box::new(std::mem::take(&mut self.models)));
                        self.thickness = None;
                        self.retired.push(Box::new(std::mem::take(&mut self.shifted)));
                        self.shifted_now.clear();
                        self.checking = None;
                        self.active.clear();
                        self.shown = None;
                        self.answered = None;
                        self.sf_reading = None;
                        self.regions.clear();
                        self.stop();
                        self.end_busy(state, End::Stopped);
                        state.force.part.clear();
                        state.force.index = None;
                        state.force.note = why;
                        state.force.scene = None;
                        state.force.stats.clear();
                        state.force.how.clear();
                        state.force.status.clear();
                        changed = true;
                    }
                }
            }
        }
        self.frame = self.subject.as_ref().filter(|_| shown).and_then(|s| inputs.plate.and_then(|ev| ev.placed_frame(s.index))).and_then(crate::study_edit::Frame::new);
        if let (true, Some(ev)) = (shown, inputs.plate) {
            changed |= self.read_loads(state, ev);
        }
        // the part's slice, once the slicer has the plate as it stands: the study reads its walls
        // and skins from it (from the settings before it, and again after an edit of the part)
        if shown {
            let seen = (inputs.plate.map_or(0, |p| Arc::as_ptr(p) as usize), crate::live_slice::latest_toolpaths_revision(), state.force.as_printed);
            if seen != self.slice_seen {
                self.slice_seen = seen;
                if self.follow_slice(inputs.plate, seen.2) {
                    // a read under way shows the part as it is; its solves follow with the slice
                    if self.reading && self.job.is_some() {
                        self.want = true;
                    } else {
                        self.restart();
                    }
                }
            }
        }
        // the Force tool's hover: gone with the tool, told again for another kind or size
        if self.hover.is_some() && (!state.force_tool() || self.hover_kind != state.ui.force_kind || self.hover_size != state.ui.force_size[state.ui.force_kind as usize]) {
            if !state.force_tool() {
                self.hover = None;
                state.force.typing = None;
            }
            self.show_hover(state);
            changed = true;
        }
        changed |= self.take_notes(state);
        // a Cancel (the busy line's, the job strip's): the run under way goes stale at once,
        // its solve on the hub with it
        let cancelled = self.busy.as_ref().is_some_and(|j| j.is_cancelled()) || self.floor_busy.as_ref().is_some_and(|j| j.is_cancelled());
        if cancelled && self.job.is_some() && self.live.load(Ordering::Relaxed) == self.ticket {
            self.stop();
        }
        // the AI hub: the wait for the next look ended, a look came back, the AI Hub app started
        // from the panel ended
        if self.look.is_event(event).is_some() {
            self.look = Timer::empty();
        }
        if let Some(found) = self.finding.as_mut().and_then(|h| h.try_take()) {
            self.finding = None;
            changed |= self.take_look(cx, state, found.ok());
        }
        if let Some(why) = self.hub_app.as_ref().and_then(|ended| ended.try_recv().ok()) {
            self.hub_app = None;
            if state.force.hub == ForceHub::Starting {
                state.force.hub = ForceHub::Missing;
                state.force.status = format!("The AI hub did not start: {why}");
                changed = true;
            }
        }
        if let Some(h) = &mut self.job {
            if let Some(r) = h.try_take() {
                self.job = None;
                // The last note can arrive between the event's read and the worker finishing.
                // Take it before a restart or the floor replaces this study's receiver (the
                // frame is redrawn below whatever it changed).
                self.take_notes(state);
                if let Err(e) = r {
                    state.force.status = format!("The study stopped: {e:?}");
                }
                state.force.solving = false;
                // its job: stopped by a Cancel, else ended when nothing more is wanted (a
                // restart's next run carries it on)
                if self.floor_busy.as_ref().is_some_and(|j| j.is_cancelled()) {
                    self.end_busy(state, End::FloorStopped);
                }
                if self.busy.as_ref().is_some_and(|j| j.is_cancelled()) {
                    self.want = false;
                    self.end_busy(state, End::Stopped);
                    state.force.stopped = true;
                    state.force.status = match self.models.iter().filter(|k| k.answer.is_some()).count() {
                        0 => "Stopped before the first answer.".into(),
                        k => format!("Stopped at step {k}."),
                    };
                } else if !self.want {
                    self.end_busy(state, End::Done);
                }
                // a floor that ended without its answer (an edit made it stale, or it failed)
                if self.floor_ticket == self.ticket && state.force.floor.running {
                    state.force.floor = ForceFloor { status: "The infill floor stopped before it was found.".into(), ..Default::default() };
                    self.end_busy(state, End::FloorStopped);
                }
                changed = true;
            }
        }
        // the study (its solves wait for an AI hub that answers; reading the part does not)
        if self.job.is_none() && self.want && shown && self.subject.is_some() {
            let solves = Model::posed(&self.loads) && self.shown.is_some();
            if !solves && state.force.hub == ForceHub::Missing {
                state.force.hub = ForceHub::Answering;
            }
            if !solves || self.hub_ready(cx) {
                self.submit(cx, state, &inputs, Submit::Study);
                changed = true;
            }
        }
        // buckling, once the study is up to date: its own job, never ahead of the stresses
        let mut buckled = self.buckle.take();
        if shown && !self.want && self.busy.is_none() && !state.force.solving && self.answered.is_some() && Model::posed(&self.loads) && self.buckle.wants() {
            if let (Some(steps), Some(hub), Some(s)) = (self.buckle_steps(), self.hub.clone().filter(|h| h.is_open()), self.subject.as_ref()) {
                let part = s.name.clone();
                self.buckle.start(cx, hub, &part, steps);
                buckled = true;
            }
        }
        if buckled || changed {
            self.show_buckling(state);
            changed |= buckled;
        }
        // the floor asked for, once the study is up to date
        if self.job.is_none() && !self.want && self.floor_want && shown && self.subject.is_some() && self.hub_ready(cx) {
            self.floor_want = false;
            self.submit(cx, state, &inputs, Submit::Floor);
            changed = true;
        }
        // nothing else to do, or a solve that waits for an AI hub: the surfaces go on
        if self.job.is_none() && shown && self.surfaces_left() {
            self.submit(cx, state, &inputs, Submit::Surfaces);
        }
        // a floor found before the study changed is not the part's any more
        let floor = &mut state.force.floor;
        if self.floor_ticket != 0 && self.ticket != self.floor_ticket && !floor.rows.is_empty() {
            *floor = ForceFloor { status: "The study changed since the infill floor was found: find it again.".into(), ..Default::default() };
            self.floor_ticket = 0;
            changed = true;
        }
        // the study waits for the slicer: its job has not started while a slice runs
        let slicing = matches!(state.print.status, fab::print::SliceStatus::Running { .. });
        let waiting = self.job.is_some() && !self.heard && slicing && !self.surfacing;
        if state.force.waiting != waiting {
            state.force.waiting = waiting;
            changed = true;
        }
        self.sweep(cx);
        changed
    }

    /// Drops what the lab let go of ([`ForceLab::retired`]) on the pool. A closed pool (shutdown)
    /// hands the job back and it drops here.
    fn sweep(&mut self, cx: &mut Cx) {
        if self.retired.is_empty() {
            return;
        }
        let retired = std::mem::take(&mut self.retired);
        match cx.task_pool().submit(Class::Background, move || drop(retired)) {
            Ok(h) => h.detach(),
            Err(e) => log!("fab force: kept solves were not queued for freeing ({e:?})"),
        }
    }

    fn take_notes(&mut self, state: &mut AppState) -> bool {
        let Some(rx) = &self.rx else { return false };
        let mut notes = Vec::new();
        while let Ok(n) = rx.try_recv() {
            notes.push(n);
        }
        let any = !notes.is_empty();
        // a study started over since: what it sent is stale
        let now = self.live.load(Ordering::Relaxed);
        for (ticket, n) in notes {
            if ticket != now {
                continue;
            }
            self.heard = true;
            match n {
                Note::Material { print, filament, typed, sliced, process, zones } => {
                    state.force.material = material_line(state, typed.as_deref(), filament);
                    let f = &mut state.force;
                    f.strengthless = typed.filter(|t| filament_of_type(t).is_none());
                    self.sliced_at = sliced;
                    f.print = match (&print, f.as_printed, sliced) {
                        (Ok(p), true, Some(at)) => format!("As sliced (slice of {}, {process}): {}{}", makepad_civil_time::local_or_utc(at).hm(), describe(p), describe_zones(&zones)),
                        (Ok(p), true, None) => format!("Estimated from settings ({process}): {}{}", describe(p), describe_zones(&zones)),
                        (Ok(_), false, _) => "Solid and even throughout, as moulded plastic: no walls, infill or layers".into(),
                        (Err(e), ..) => format!("The print settings could not be read ({e})"),
                    };
                    self.process = process;
                }
                Note::Shape(s) => self.made_of = s,
                // said at once, under the coarser step's answer: the finer one may fail
                Note::Thickness(t, h) => {
                    self.thickness = Some(t);
                    self.thin_h = h;
                    if !state.force.stats.is_empty() {
                        self.show_thin(&mut state.force.stats);
                    }
                }
                Note::Step(s) => self.take_step(*s, state),
                Note::Sampled { index, active } => {
                    if let Some((next, _)) = self.checking {
                        if self.models.get(index - 1).is_some_and(|k| k.model.h * 0.5 == next) {
                            self.show_provisional(&mut state.force, next, self.seconds(active as f64, true));
                        }
                    }
                }
                Note::Shifted { index, last, kept } => self.take_shifted(index, last, *kept, state),
                // the study goes on once a hub answers again (this run ends now)
                Note::HubGone(why) => {
                    log!("fab force: the AI hub's link closed under the study: {why}");
                    self.hub = None;
                    self.want = true;
                    if self.floor_ticket == self.ticket {
                        self.floor_want = true;
                    }
                }
                Note::Floor(floor) => {
                    match (&self.floor_busy, floor.running) {
                        (Some(j), true) => j.stage(floor.status.trim_end_matches("...").replacen("Finding the infill floor", "infill floor", 1), None, None),
                        (Some(_), false) if floor.rows.is_empty() => self.end_busy(state, End::FloorFailed(floor.status.clone())),
                        (Some(_), false) => self.end_busy(state, End::FloorDone),
                        (None, _) => {}
                    }
                    state.force.floor = floor;
                }
                Note::Failed(why) => {
                    // it would fail again: the next change starts it over
                    self.want = false;
                    self.surfaces_failed = true;
                    let end = if self.floor_ticket == self.ticket { End::FloorFailed(why.clone()) } else { End::Failed(why.clone()) };
                    self.end_busy(state, end);
                    state.force.status = why;
                    state.force.solving = false;
                }
            }
        }
        any
    }

    fn take_step(&mut self, s: Step, state: &mut AppState) {
        let own = self.own();
        if self.models.len() <= s.index {
            self.models.resize(s.index + 1, Kept { model: s.model.clone(), key: s.materials, answer: None, level: None });
        }
        let kept = &mut self.models[s.index];
        if !Arc::ptr_eq(&kept.model, &s.model) {
            self.retired.push(Box::new((std::mem::replace(&mut kept.model, s.model.clone()), kept.level.take())));
        }
        kept.key = s.materials;
        if let Some(a) = &s.answer {
            let was = (kept.answer.replace(a.answer.clone()), std::mem::replace(&mut kept.level, s.level.clone().filter(|_| s.index < own)));
            // a step past a study's own sizes (Refine's) is solved once: the next study goes back
            // to those sizes and drops it, so nothing solves on its level again
            if was.0.is_some() || was.1.is_some() {
                self.retired.push(Box::new(was));
            }
        }
        self.of = s.index + 1;
        let active = s.active;
        if self.active.len() <= s.index {
            self.active.resize(s.index + 1, 0);
        }
        self.active[s.index] = active;
        if s.index >= PROVISIONAL && active > 0 {
            if s.sampled {
                self.field_rate = Some(s.field_ms / active as f64);
            }
            if s.answer.is_some() {
                self.solve_rate = Some(s.solve_ms / active as f64);
            }
            if let Some(ran) = s.ran {
                self.ran = Some((s.index, ran));
            }
        }
        self.shown = Some(s.model.clone());
        self.regions = s.regions;
        if s.answer.is_some() {
            self.answered = s.answer.clone();
        }
        // the pointer has not moved: what a click takes there, on the finer surface
        if let Some(h) = self.hover.take() {
            self.hover = Hover::at(&s.model, h.at, h.flip);
        }
        let last = s.last;
        // a refined study's finer model goes with the next study that does not take it
        if last {
            if self.models.len() > s.index + 1 {
                self.retired.push(Box::new(self.models.split_off(s.index + 1)));
            }
            self.active.truncate(s.index + 1);
        }
        let posed = Model::posed(&self.loads);
        // the last step solved, or a surface with nothing to solve: up to date (a posed part's
        // solves follow its surface)
        if (last && s.answer.is_some()) || (s.answer.is_none() && !posed) {
            self.want = false;
        }
        // a surface with no answer of its own under the last answer: that answer's lines stay
        if s.answer.is_none() && posed && self.answered.is_some() {
            self.sync_loads(state);
            return;
        }
        let f = &mut state.force;
        f.stats.clear();
        f.how.clear();
        f.how.push(("Sampled from".into(), self.made_of.clone()));
        match &s.answer {
            Some(a) => {
                if self.first.is_none() {
                    self.first = Some(Cx::time_now() - self.started);
                }
                let a = &a.answer;
                let r = &a.results;
                // Keep the lower of the two alignments on screen until the study's own last pair
                // gives its verdict. Refine's steps show the verdict they refine.
                if !self.held && s.index > PROVISIONAL && s.index < LEVELS && last && s.checking {
                    self.show_provisional(f, s.model.h, self.shifted_seconds(s.index, active));
                } else if self.finer && (s.index < own || s.checking) {
                    // Refine solves the study's own sizes again on what they kept, and its own
                    // step before that step's shifted check: until a finer verdict reads, the
                    // verdict refined stays, its shifted check with it
                    self.checking = None;
                    self.show_refined(f);
                } else {
                    self.checking = None;
                    self.show_reading(f, s.index, None);
                }
                if last || s.index + 1 >= own {
                    self.thin_h = s.model.h;
                }
                self.show_thin(&mut f.stats);
                let at = |q: Option<u32>| if q.is_some() { " at the surface" } else { "" };
                f.stats.push(("Largest von Mises".into(), format!("{:.2} MPa{}", r.max_vm.0, at(r.at_quad[0]))));
                f.stats.push(("Largest principal".into(), format!("{:.2} MPa{}", r.max_p1.0, at(r.at_quad[1]))));
                f.stats.push(("Largest movement".into(), format!("{:.4} mm", r.max_u.0)));
                if a.loose.pieces > 0 {
                    f.stats.push(("Left out".into(), format!("{}, not analysed", a.loose.describe())));
                }
                f.how.push(("Cells".into(), format!("{active} of {:.2} mm", s.model.h)));
                f.how.push(("Solve".into(), format!("{} iterations on {}, residual {:.0e}, {:.0} ms", a.solution.iterations, s.on, a.solution.residual, s.solve_ms)));
                f.how.push(("Field sampled".into(), format!("{:.0} ms", s.field_ms)));
                if s.checking {
                    let time = if !self.held && s.index == PROVISIONAL { String::new() } else { time_suffix(self.shifted_seconds(s.index, active)) };
                    f.status = format!("Step {} shown; checking it on cells shifted half a cell{time}...", s.index + 1);
                } else if last {
                    f.refine = self.refine_offer(s.index);
                    f.status = self.done(f.as_printed);
                    self.log_refined();
                } else if self.checking.is_none() {
                    f.status = format!("Step {} shown; refining...", s.index + 1);
                }
            }
            None => {
                f.how.push(("Cells".into(), format!("{active} of {:.2} mm", s.model.h)));
                f.status = match (missing(&self.loads), f.hub) {
                    (Some(why), _) => why,
                    // its solves wait for an AI hub
                    (None, ForceHub::Missing) => NO_HUB.into(),
                    (None, ForceHub::Starting) => STARTING_HUB.into(),
                    (None, ForceHub::Answering) => "Solving...".into(),
                };
            }
        }
        self.sync_loads(state);
    }

    /// The panel's line on what is too thin for the study's cells, after the safety factor's.
    fn show_thin(&self, stats: &mut Vec<(String, String)>) {
        stats.retain(|(name, _)| name != "Thin features");
        let Some(thin) = self.thickness.as_ref().and_then(|t| t.thin_at(self.thin_h)) else { return };
        let at = stats.iter().position(|(name, _)| name == "Near the fixtures").map_or(0, |i| i + 1);
        stats.insert(at, ("Thin features".into(), thin_text(thin, self.thin_h, self.finer)));
    }

    /// The coarser step when this pair can give a verdict.
    fn coarse(&self, index: usize) -> Option<usize> {
        index.checked_sub(1).filter(|_| self.held || index > PROVISIONAL)
    }

    /// The worst solve the rows read that stopped at its iteration limit above its tolerance:
    /// step `index`'s, the coarser one its reading compares against, or the shifted check's (its
    /// cells, its answer). Its cells and answer.
    fn unsolved<'a>(&'a self, index: usize, shifted: Option<(f64, &'a Outcome)>) -> Option<(f64, &'a Outcome)> {
        let tolerance = Options::default().tolerance;
        let steps = self.coarse(index).into_iter().chain([index]).filter_map(|i| self.models.get(i)).filter_map(|k| Some((k.model.h, k.answer.as_deref()?)));
        steps.chain(shifted).filter(|(_, a)| !a.solution.solved(tolerance)).max_by(|x, y| x.1.solution.residual.total_cmp(&y.1.solution.residual))
    }

    /// The safety factor's reading, the cell size it reads and the coarser size it compares.
    fn reading_at(&self, index: usize, shifted: Option<&Outcome>) -> Option<(Reading, (f64, Option<f64>))> {
        let kept = self.models.get(index)?;
        let coarse = self.coarse(index).and_then(|i| self.models.get(i)).and_then(|k| Some((k.model.h, k.answer.as_deref()?)));
        Some((Reading::of(kept.answer.as_deref()?, coarse.map(|(_, a)| a), shifted), (kept.model.h, coarse.map(|(h, _)| h))))
    }

    /// The study's last line: how long it took, and what its walls and skins are read from.
    fn done(&self, as_printed: bool) -> String {
        let first = self.first.map_or(String::new(), |t| format!("first answer in {t:.1} s, "));
        // after the colon, the status line under the busy line's "Done in"
        let from = match (as_printed, self.sliced_at) {
            (true, Some(at)) => format!(": As sliced (slice of {}, {})", makepad_civil_time::local_or_utc(at).hm(), self.process),
            (true, None) => format!(": Estimated from settings ({})", self.process),
            (false, _) => String::new(),
        };
        format!("Done in {:.1} s ({first}{} steps){from}", Cx::time_now() - self.started, self.of)
    }

    /// What the busy line's Refine offers once a study is done: one cell size finer than this step
    /// and about how long it takes, where the safety factor away from the fixtures is not
    /// converged (near them the hold's edge never is) and the study did not refine already.
    fn refine_offer(&mut self, index: usize) -> Option<String> {
        self.offer = None;
        let h = self.models.get(index)?.model.h;
        let apart = self.sf_reading.is_some_and(|r| r.away.value().is_none());
        let thin = self.thickness.as_ref().is_some_and(|t| t.thin_at(h).is_some());
        if !((apart || thin) && !self.finer && h * 0.5 >= FINEST && self.fits_finer(index)) {
            return None;
        }
        self.offer = self.refine_seconds(index).map(|(solve, check)| (h * 0.5, solve, check));
        Some(match self.offer {
            Some((_, solve, check)) => format!("Refine ({})", about(solve + check)),
            None => "Refine".into(),
        })
    }

    /// Refine's predicted seconds after step `index`: the finer size's solve, then its shifted
    /// check on about as many cells, always sampled (a size past the study's own keeps no shifted
    /// model). The study's own sizes solve again on what they kept before them: warm, under a
    /// second on the demo bracket, not counted.
    fn refine_seconds(&self, index: usize) -> Option<(f64, f64)> {
        let (cells, held) = self.next_cells(index)?;
        Some((self.seconds(cells, held)?, self.seconds(cells, false)?))
    }

    /// A Refine's study ended: the time it took against its offer's prediction, in the log.
    fn log_refined(&mut self) {
        if let Some((h, solve, check)) = self.refining.take() {
            let took = Cx::time_now() - self.started;
            log!("fab force: Refine to {h:.2} mm cells done in {took:.1} s, predicted {:.0} s (measured/predicted {:.2})", solve + check, took / (solve + check));
        }
    }

    /// Whether the size after step `index` fits the GPU that solved it: its GPU route's need
    /// there, about eight times this step's (twice the cells along each axis), within the
    /// device's budget and its largest buffer. Its predicted time is this GPU engine's; past
    /// what the device holds Refine is not offered (a solve too large for its GPU neither splits
    /// nor goes to the CPU engine yet).
    fn fits_finer(&self, index: usize) -> bool {
        let Some((at, ran)) = self.ran.filter(|(at, _)| *at == index) else { return false };
        let Some((budget, one)) = ran.gpu_limits else { return false };
        let fits = ran.gpu && 8.0 * ran.gpu_need <= budget && 8.0 * ran.gpu_largest <= one;
        if !fits {
            log!("fab force: Refine not offered: step {at}'s finer size needs about {:.1} GiB of the GPU ({:.1} GiB in its largest buffer), the device gives {:.1} GiB ({:.1} GiB in one){}", 8.0 * ran.gpu_need / GIB, 8.0 * ran.gpu_largest / GIB, budget / GIB, one / GIB, if ran.gpu { "" } else { "; this size ran on the CPU engine" });
        }
        fits
    }

    /// A solve's predicted seconds on `cells` active cells, with its sampling unless its model is
    /// held already.
    fn seconds(&self, cells: f64, held: bool) -> Option<f64> {
        let field = if held { 0.0 } else { self.field_rate? };
        Some((self.solve_rate? + field) * cells / 1e3)
    }

    /// The predicted seconds of the size after step `index` (its aligned solve).
    fn next_seconds(&self, index: usize) -> Option<f64> {
        let (cells, held) = self.next_cells(index)?;
        self.seconds(cells, held)
    }

    /// The size after step `index`: its active cells and whether the study holds its model. On
    /// that model (its surface sampled ahead), else eight times this step's, sampled first.
    fn next_cells(&self, index: usize) -> Option<(f64, bool)> {
        let h = self.models.get(index)?.model.h * 0.5;
        let held = self.models.get(index + 1).filter(|k| (k.model.h - h).abs() < 1e-9).and(self.active.get(index + 1).copied()).filter(|&a| a > 0);
        match held {
            Some(cells) => Some((cells as f64, true)),
            None => Some((8.0 * *self.active.get(index).filter(|&&a| a > 0)? as f64, false)),
        }
    }

    /// The predicted seconds of step `index` on the shifted grid, about as many cells as aligned.
    fn shifted_seconds(&self, index: usize, cells: usize) -> Option<f64> {
        let h = self.models.get(index)?.model.h;
        let held = self.shifted.get(index).and_then(Option::as_ref).is_some_and(|k| (k.model.h - h).abs() < 1e-9);
        self.seconds(cells as f64, held)
    }

    /// The reading in its two rows, with the Solve row first if any solve it reads stopped short.
    fn show_reading(&mut self, f: &mut ForceView, index: usize, shifted: Option<&Outcome>) {
        f.stats.retain(|(name, _)| !matches!(name.as_str(), "Solve" | "Safety factor" | "Near the fixtures"));
        let reading = self.reading_at(index, shifted);
        self.sf_reading = reading.map(|(r, _)| r);
        let Some((reading, hs)) = reading else { return };
        let unsolved = self.unsolved(index, shifted.map(|a| (hs.0, a)));
        let short = unsolved.map(|(_, a)| a.solution.residual);
        let unchecked = match self.checking {
            Some((next, secs)) => {
                let approximate = short.map_or(String::new(), |r| format!("; {}", noted(Some(r), "")));
                format!("provisional: the lower of two alignments at {:.2} mm cells; checking at {next:.2} mm cells{}{approximate}", hs.0, time_suffix(secs))
            }
            None => noted(short, "one cell size, unchecked"),
        };
        f.stats.insert(0, ("Near the fixtures".into(), near_text(reading.near, hs, short, &unchecked)));
        f.stats.insert(0, ("Safety factor".into(), safety_text(&reading, hs, short, &unchecked)));
        if let Some(row) = unsolved.map(unsolved_row) {
            f.stats.insert(0, ("Solve".into(), row));
        }
    }

    fn show_provisional(&mut self, f: &mut ForceView, next: f64, seconds: Option<f64>) {
        self.checking = Some((next, seconds));
        let shifted = self.current_shifted(PROVISIONAL).and_then(|k| k.answer.clone());
        self.show_reading(f, PROVISIONAL, shifted.as_deref());
        f.status = format!("Checking at {next:.2} mm cells{}...", time_suffix(seconds));
    }

    /// Keep a shifted answer at a study's own size so an edit can solve on it again.
    fn take_shifted(&mut self, index: usize, last: bool, kept: Kept, state: &mut AppState) {
        let answer = kept.answer.clone();
        let h = kept.model.h;
        if index < self.own() {
            if self.shifted.len() <= index {
                self.shifted.resize(index + 1, None);
                self.shifted_now.resize(index + 1, false);
            }
            if let Some(was) = self.shifted[index].replace(kept) {
                self.retired.push(Box::new(was));
            }
            self.shifted_now[index] = true;
        } else {
            // a size past the study's own keeps no shifted model: its level and answer go
            self.retired.push(Box::new(kept));
        }
        let f = &mut state.force;
        if last {
            self.checking = None;
            self.show_reading(f, index, answer.as_deref());
            f.refine = self.refine_offer(index);
            f.status = self.done(f.as_printed);
            self.log_refined();
        } else if self.finer {
            self.checking = None;
            self.show_refined(f);
        } else {
            self.show_provisional(f, h * 0.5, self.next_seconds(index));
        }
    }

    /// While Refine runs: the verdict it refines, at the study's last own size with that size's
    /// shifted check.
    fn show_refined(&mut self, f: &mut ForceView) {
        let index = self.own() - 1;
        let shifted = self.current_shifted(index).and_then(|k| k.answer.clone());
        self.show_reading(f, index, shifted.as_deref());
    }

    /// Starts the study, the infill floor of the study as it stands, or the sampling of the
    /// study's surfaces.
    fn submit(&mut self, cx: &mut Cx, state: &mut AppState, inputs: &Inputs, what: Submit) {
        let Some(subject) = self.subject.clone() else { return };
        let Some(ev) = inputs.plate.cloned() else { return };
        let floor = what == Submit::Floor;
        let surfaces = what == Submit::Surfaces;
        if what == Submit::Study {
            self.held = state.force.as_printed && self.slice.is_some();
        }
        // a surface to click on and nothing to solve: the study waits for its loads
        if !surfaces && !Model::posed(&self.loads) && (self.shown.is_some() || floor) {
            self.want = false;
            if floor {
                state.force.floor = ForceFloor { status: "The infill floor needs the part held and loaded.".into(), ..Default::default() };
            }
            return;
        }
        self.ticket = self.live.fetch_add(1, Ordering::Relaxed) + 1;
        let (ticket, live) = (self.ticket, self.live.clone());
        let setup = (inputs.setup)(state);
        let loads = self.loads.clone();
        let f = &state.force;
        let (study_as, as_printed) = (f.study_as, f.as_printed);
        let held = self.held;
        let models = self.models.clone();
        let shifted_steps = if floor || surfaces { Vec::new() } else { self.shifted.clone() };
        // the floor at its cell sizes (the study's own reading there, when it stopped there)
        let at_floor = study::steps(subject.lo, subject.hi, HELD, FINEST);
        let answer = |h: f64| self.models.iter().find(|k| (k.model.h - h).abs() < 1e-9).and_then(|k| k.answer.clone());
        // the study's own answer on the shifted grid, at the floor's finest cells
        let shifted = at_floor.last().filter(|_| floor).and_then(|h| {
            let i = self.models.iter().position(|k| (k.model.h - h).abs() < 1e-9)?;
            self.current_shifted(i).filter(|k| (k.model.h - h).abs() < 1e-9).cloned()
        });
        let aligned = shifted.as_ref().and_then(|k| k.answer.clone());
        // (the floor estimates from the settings: a study read from the slice is not one of its own)
        let now = (floor && self.slice.is_none())
            .then(|| {
                let fine = answer(*at_floor.last()?)?;
                let (coarse, aligned) = match at_floor.len() {
                    1 => (None, None),
                    n => (Some(answer(at_floor[n - 2])?), Some(aligned?)),
                };
                Some(Reading::of(&fine, coarse.as_deref(), aligned.as_deref()))
            })
            .flatten();
        let steps = if floor { at_floor } else { sizes(&subject, held, self.finer && !surfaces) };
        // nothing of the part is read yet: read it alone first, ahead of the slicer (a click waits
        // on it); the solves are Background work like the slicer's
        let bare = self.shown.is_none() && !floor;
        let solve = !bare && !floor && !surfaces;
        let slice = self.slice.as_ref().filter(|_| solve).map(|s| (s.revision, s.at, s.toolpaths.clone()));
        // the surface the person sees next, past the one read: on screen work, as a slice someone
        // asked for is; the sizes past it follow as their own Background job
        let next_only = surfaces && self.models.len() <= 1;
        let (class, name) = match (bare, what) {
            (true, _) => (Class::Interactive, "fab force read"),
            (false, Submit::Floor) => (Class::Background, "fab force floor"),
            (false, Submit::Surfaces) if next_only => (Class::Visible, "fab force surface"),
            (false, Submit::Surfaces) => (Class::Background, "fab force surfaces"),
            (false, Submit::Study) => (Class::Background, "fab force study"),
        };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.heard = false;
        self.surfacing = surfaces;
        // the surfaces are no job anyone waits on: no line on the board, nothing to cancel
        let report = if surfaces {
            None
        } else if floor {
            let job = self.floor_busy.insert(JobSpec::new(format!("Infill floor of {}", subject.name)).stage("starting").start());
            Some(job.reporter())
        } else {
            let (title, stage) = (format!("Force study of {}", subject.name), if bare { "reading the part" } else { "solving" });
            state.force.stopped = false;
            Some(match &self.busy {
                Some(job) => {
                    job.set_title(title);
                    job.stage(stage, None, None);
                    job.reporter()
                }
                None => self.busy.insert(JobSpec::new(title).stage(stage).start()).reporter(),
            })
        };
        self.show_busy(state);
        if floor {
            self.floor_ticket = ticket;
            state.force.floor = ForceFloor { status: "Finding the infill floor...".into(), running: true, rows: Vec::new() };
        } else if !surfaces {
            // a study's clock starts at its read, or at its solves when the part already showed
            if bare || !self.reading {
                self.started = Cx::time_now();
                self.first = None;
            }
            self.reading = bare;
            // Refine keeps the study as it stands: its own sizes' shifted checks still hold
            if !self.finer {
                self.shifted_now.fill(false);
            }
            state.force.solving = true;
            state.force.refine = None;
            state.force.status = if bare { "Reading the part...".into() } else { "Solving...".into() };
        }
        // (a read solves nothing: it goes with no hub)
        let hub = self.hub.clone().unwrap_or_else(|| Arc::new(Hub::of(Vec::new())));
        match cx.task_pool().submit_named(class, name, move || {
            run(Run { ticket, live, report, hub, subject, ev, setup, loads, study_as, as_printed, held, steps, models, shifted_steps, shifted, bare, next_only, solve, floor, now, slice }, &tx);
            SignalToUI::set_ui_signal();
        }) {
            Ok(h) => self.job = Some(h),
            // (the next event tries again: the pool is going away)
            Err(_) if surfaces => {}
            Err(e) if floor => {
                state.force.floor = ForceFloor { status: format!("Could not start the infill floor: {e:?}"), ..Default::default() };
                self.end_busy(state, End::FloorFailed(format!("{e:?}")));
            }
            Err(e) => {
                state.force.solving = false;
                state.force.status = format!("Could not start the study: {e:?}");
                self.end_busy(state, End::Failed(format!("{e:?}")));
            }
        }
    }
}

/// What [`ForceLab::submit`] starts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Submit {
    Study,
    Floor,
    Surfaces,
}

struct Run {
    ticket: u64,
    live: Arc<AtomicU64>,
    /// Its job on the board: where it says how far it is, and whose Cancel stops it.
    report: Option<JobReporter>,
    /// The AI hub its solves run on.
    hub: Arc<Hub>,
    subject: Subject,
    ev: Arc<fab_model::eval::Evaluated>,
    setup: crate::print::CheckSetup,
    loads: Vec<Load>,
    study_as: Option<usize>,
    as_printed: bool,
    held: bool,
    steps: Vec<f64>,
    /// Each step as last built and solved (the next solve starts there).
    models: Vec<Kept>,
    /// The study's shifted models by step, for an edit to solve on again.
    shifted_steps: Vec<Option<Kept>>,
    /// The shifted model at the infill floor's finest cells, when the study checked them.
    shifted: Option<Kept>,
    /// Only the first step's surface, so a click can read the part (what follows is its own job).
    bare: bool,
    /// Only the first surface not sampled yet: the one the person sees next (what follows is its
    /// own job).
    next_only: bool,
    /// Solve its steps once the part is held and loaded (else it only samples their surfaces).
    solve: bool,
    /// The infill floor at its cell sizes ([`HELD`]), instead of the study ([`floor`]); the study's own
    /// reading there, if it has it.
    floor: bool,
    now: Option<Reading>,
    /// The part's slice to read its walls and skins from (else the settings' estimate).
    slice: Option<(u64, i64, Arc<makepad_fab_slice::live::PlatePaths>)>,
}

/// A study on the pool: the material, then each step's model and answer, sent as they come.
fn run(r: Run, tx: &mpsc::Sender<(u64, Note)>) {
    let stale = || r.live.load(Ordering::Relaxed) != r.ticket || r.report.as_ref().is_some_and(|j| j.is_cancelled());
    let send = |n: Note| {
        let _ = tx.send((r.ticket, n));
        SignalToUI::set_ui_signal();
    };
    let read = print_of(&r.setup, &r.ev, r.subject.index);
    let typed = read.as_ref().ok().map(|(_, t, ..)| t.clone());
    // the filament's family, else what the person studies it as: never a quiet stand-in
    let filament = typed.as_deref().and_then(filament_of_type).and_then(|f| FILAMENTS.iter().position(|x| x.name == f.name)).or(r.study_as);
    let lift = read.as_ref().map_or(0.0, |(_, _, l, _)| *l);
    let zones = read.as_ref().map_or_else(|_| Vec::new(), |(.., z)| z.clone());
    // what the zones' places are: the part's paint (0 without any)
    let zones_key = r.ev.plate.placed_part(r.subject.index).and_then(|p| p.paint.as_ref()).filter(|p| !p.settings.is_empty()).map_or(0, |p| p.key);
    let print = read.map(|(p, ..)| p);
    // the walls and skins the slicer printed for the part, when the study reads its slice
    let sliced = r.slice.as_ref().filter(|_| r.as_printed && print.is_ok()).map(|(revision, at, t)| (*revision, *at, beads_of(&t.read(), r.subject.index, lift))).filter(|(.., b)| !b.is_empty());
    // the floor is a what-if of the settings: the panel's material stays the study's
    if r.solve || r.bare {
        // what the part or the plate changes of it, the profile's name does not say
        let changed = r.ev.plate.placed_part(r.subject.index).is_some_and(|p| !p.print.is_empty()) || !r.ev.plate.settings.is_empty();
        let process = format!("{}{}", r.setup.process(), if changed { ", changed for this part" } else { "" });
        send(Note::Material { print: print.clone(), filament, typed: typed.clone(), sliced: sliced.as_ref().map(|s| s.1), process, zones: zones.clone() });
    }
    let print = match print {
        Ok(p) => p,
        Err(e) => return send(Note::Failed(format!("The print settings could not be read: {e}"))),
    };
    let Some(filament) = filament else {
        let why = match typed.as_deref() {
            Some(t) if !t.is_empty() => format!("There is no strength data for {t} here"),
            _ => "The filament has no type set".into(),
        };
        return send(Note::Failed(format!("{why}: pick what to study it as.")));
    };
    let m = Materials::new(FILAMENTS[filament], &print, r.as_printed).with_zones(zones.clone());
    let key = materials_key(filament, r.as_printed, &print, zones_key, sliced.as_ref().map(|s| s.0));
    let (lo, hi) = (r.subject.lo, r.subject.hi);
    // a step's samples over the part's box moved by `shift` (the grid shifted with it)
    let at = |v: V3, shift: V3| -> V3 { [v[0] + shift[0], v[1] + shift[1], v[2] + shift[2]] };
    type Sample = Box<dyn FnMut(f64, V3, &Materials) -> Model>;
    type Thick = Box<dyn FnMut(&Model) -> Thickness>;
    let (mut sample, mut thick): (Sample, Thick) = match &r.subject.shape {
        Shape::Field(sdf) => {
            send(Note::Shape("its distance field".into()));
            let mut distance = giving_way(printer_field(sdf));
            let mut through = giving_way(printer_field(sdf));
            (
                Box::new(move |h, shift, m| Model::new(at(lo, shift), at(hi, shift), h, &mut distance, m)),
                Box::new(move |model| Thickness::of_field(&model.surface, model.spacing(), &mut through)),
            )
        }
        Shape::Mesh { .. } => {
            let Some(part) = r.ev.plate.placed_part(r.subject.index) else { return send(Note::Failed("The part is no longer on the plate.".into())) };
            let mesh = Arc::new(Mesh::new(&part.mesh.positions, &part.mesh.triangles));
            send(Note::Shape(match mesh.open_edges() {
                0 => format!("its mesh, {} triangles, closed", mesh.triangles()),
                open => format!("its mesh, {} triangles, open at {open} edges", mesh.triangles()),
            }));
            let through = mesh.clone();
            (
                Box::new(move |h, shift, m| Model::of_mesh(at(lo, shift), at(hi, shift), h, &mesh, m)),
                Box::new(move |model| Thickness::of_mesh(&model.surface, model.spacing(), &through)),
            )
        }
    };
    let posed = r.solve && Model::posed(&r.loads);
    let opts = Options::default();
    // a solve that stopped: the hub's link closed (the lab looks for a hub again), else it failed
    let stopped = |e: String| if r.hub.is_open() { Note::Failed(e) } else { Note::HubGone(e) };
    if r.floor {
        let found = floor(&r, &Materials::new(FILAMENTS[filament], &print, true).with_zones(zones.clone()), &mut *sample, &opts, &send, &stale);
        return match found {
            Err(Stop::Failed(why)) if !r.hub.is_open() => send(Note::HubGone(why)),
            Err(Stop::Failed(why)) => send(Note::Floor(ForceFloor { status: format!("The infill floor could not be found: {why}"), ..Default::default() })),
            Err(Stop::Stale) | Ok(()) => {}
        };
    }
    // the samples' cells regraded: their walls and skins the slice's, else the estimate's
    let regraded = |old: &Model| match (&sliced, &old.lattice) {
        (Some((.., beads)), Some(lattice)) => old.as_sliced(&m, &Printed::new(lattice, beads)),
        _ => old.with_materials(&m),
    };
    // a step's model: the one kept at its cell size (regraded when the materials changed, if
    // its samples were made true as deep as the new walls and skins reach), else sampled at
    // `shift`
    let mut model_at = |kept: Option<&Kept>, h: f64, shift: V3| match kept {
        Some(old) if (old.model.h - h).abs() < 1e-9 && old.key == key => old.model.clone(),
        Some(old) if (old.model.h - h).abs() < 1e-9 && old.model.regrades(&m) => Arc::new(regraded(&old.model)),
        _ if sliced.is_some() => Arc::new(regraded(&sample(h, shift, &m))),
        _ => Arc::new(sample(h, shift, &m)),
    };
    let own = if r.held { HELD } else { LEVELS };
    // The shifted check finishes its step and counts with it.
    let of = r.steps.len() as u32;
    let check_shifted = |i: usize, fine: &Outcome, model_at: &mut dyn FnMut(Option<&Kept>, f64, V3) -> Arc<Model>| -> Result<(), Stop> {
        if stale() {
            return Err(Stop::Stale);
        }
        let h = r.steps[i];
        if let Some(j) = &r.report {
            j.stage(format!("checking on {h:.2} mm cells shifted half a cell"), None, Some((i as f32 + 0.5) / of as f32));
        }
        let kept = r.shifted_steps.get(i).and_then(Option::as_ref).filter(|k| i < own && (k.model.h - h).abs() < 1e-9);
        let model = solved_once(i >= own, model_at(kept, h, study::shift(h)));
        let posed = model.pose(&model.conditions(&r.loads))?;
        // From this alignment's last answer, else the aligned step's carried over.
        let guess = match kept.and_then(|k| k.answer.as_ref()).filter(|a| same_lattice(&a.grid, &posed.grid)) {
            Some(a) => a.solution.u.clone(),
            None => carry(&fine.grid, &fine.solution.u, &posed.grid),
        };
        let level = kept.and_then(|k| k.level_for(&model, &posed));
        let mut told = |p: Option<&hub::Progress>| {
            if let (Some(j), Some(p)) = (&r.report, p) {
                j.stage(format!("checking on {h:.2} mm cells shifted half a cell: {} iterations", p.iterations), None, Some((i as f32 + 0.5 + 0.5 * p.fraction as f32) / of as f32));
            }
            stale()
        };
        let s = solve_on(&r.hub, posed, &m, level, Some(Cow::Owned(guess)), &opts, &shifted_slot(i, own), &mut told)?;
        let Some(s) = s.filter(|_| !stale()) else { return Err(Stop::Stale) };
        let kept = Box::new(Kept { model, key, answer: Some(Arc::new(s.answer)), level: s.level });
        send(Note::Shifted { index: i, last: i + 1 == r.steps.len(), kept });
        Ok(())
    };
    let mut previous: Option<Arc<Outcome>> = None;
    for (i, &h) in r.steps.iter().enumerate() {
        if stale() {
            return;
        }
        let t0 = std::time::Instant::now();
        let kept = r.models.get(i);
        let sampled = !kept.is_some_and(|k| (k.model.h - h).abs() < 1e-9 && (k.key == key || k.model.regrades(&m)));
        // nothing to solve: a surface kept stays
        if !posed && !sampled {
            continue;
        }
        // the job's stage and step: how far, by the steps done
        let stage = |work: &str| {
            if posed && !r.held && i > PROVISIONAL { format!("checking at {h:.2} mm cells: {work}") } else { format!("{work} {h:.2} mm cells") }
        };
        let report = |stage: String, fraction: f32| if let Some(j) = &r.report {
            j.stage(stage, Some((i as u32 + 1, of)), Some((i as f32 + fraction) / of as f32));
        };
        report(if r.bare { "reading the part".into() } else { stage("sampling") }, 0.0);
        let model = solved_once(i >= own, model_at(kept, h, [0.0; 3]));
        let field_ms = t0.elapsed().as_secs_f64() * 1e3;
        let active = (0..model.grid.cells()).filter(|&c| model.grid.active(c)).count();
        if posed && i >= PROVISIONAL {
            send(Note::Sampled { index: i, active });
        }
        // on the finest surface a plain study samples: the coarser one can miss what is thin
        if sampled && i + 1 == own.min(r.steps.len()) && !r.bare {
            send(Note::Thickness(thick(&model), h));
        }
        let conditions = model.conditions(&r.loads);
        if !posed {
            let regions = conditions.into_iter().map(|c| c.region).collect();
            send(Note::Step(Box::new(Step { index: i, last: i + 1 == r.steps.len(), sampled, model, active, regions, answer: None, level: None, field_ms, solve_ms: 0.0, on: String::new(), ran: None, materials: key, checking: false })));
            if r.bare || r.next_only {
                return;
            }
            continue;
        }
        let t1 = std::time::Instant::now();
        let solving = stage("solving");
        report(solving.clone(), 0.0);
        // the pieces nothing holds left out (a load on one fails the study)
        let posed = match model.pose(&conditions) {
            Ok(p) => p,
            Err(e) => return send(Note::Failed(e)),
        };
        // an edit solves from the step's last answer on the same lattice (an edited shape too:
        // the guess only shortens the solve, never changes its answer)
        let last = kept.and_then(|k| k.answer.as_ref()).filter(|a| same_lattice(&a.grid, &posed.grid));
        let guess = last.map(|a| Cow::Borrowed(a.solution.u.as_slice()));
        let level = kept.and_then(|k| k.level_for(&model, &posed));
        // its stage as the hub's solve goes: iterations, and how far from the start to the tolerance
        let mut told = |p: Option<&hub::Progress>| {
            if let Some(p) = p {
                report(format!("{solving}: {} iterations", p.iterations), p.fraction as f32);
            }
            stale()
        };
        let Solved { answer, level, on, ran } = match solve_on(&r.hub, posed, &m, level, guess, &opts, &keep_slot(i, own), &mut told) {
            Ok(Some(solved)) => solved,
            // stale: cancelled on the hub
            Ok(None) => return,
            Err(e) => return send(stopped(e)),
        };
        if stale() {
            return;
        }
        let answered = Answered::new(Arc::new(answer), &model);
        let solve_ms = t1.elapsed().as_secs_f64() * 1e3;
        let regions = conditions.into_iter().map(|c| c.region).collect();
        let last = i + 1 == r.steps.len();
        let fine = answered.answer.clone();
        let checking = if r.held {
            last && i > 0
        } else {
            i == PROVISIONAL || (last && i > PROVISIONAL && !matches!(Reading::of(&fine, previous.as_deref(), None).away, Verdict::Apart { .. }))
        };
        send(Note::Step(Box::new(Step { index: i, last, sampled, model, active, regions, answer: Some(answered), level, field_ms, solve_ms, on, ran: Some(ran), materials: key, checking })));
        if checking {
            match check_shifted(i, &fine, &mut model_at) {
                Ok(()) => {}
                Err(Stop::Stale) => return,
                Err(Stop::Failed(e)) => return send(stopped(e)),
            }
        }
        previous = Some(fine);
    }
}

/// A solve's outcome, the analysed level it was solved on (while a later solve of its step
/// starts on it), and where it ran: in words ([`solved_on`]) and its engine and GPU memory.
struct Solved {
    answer: Outcome,
    level: Option<Arc<fab_force::solve::Level>>,
    on: String,
    ran: hub::Ran,
}

/// One solve on the AI hub at the class someone waits on, on `kept` (the step's analysed level,
/// built for this model and these held nodes) else a new one, kept there in slot `keep`;
/// `progress` is told as it iterates and says whether it went stale. None when it did (the hub
/// cancelled it). Once the hub has the job its guess goes, and the level unless the step keeps
/// a slot (a size solved once keeps neither while it waits).
#[allow(clippy::too_many_arguments)]
fn solve_on(hub: &Hub, posed: Posed, m: &Materials, kept: Option<Arc<fab_force::solve::Level>>, guess: Option<Cow<'_, [V3]>>, opts: &Options, keep: &str, progress: &mut dyn FnMut(Option<&hub::Progress>) -> bool) -> Result<Option<Solved>, String> {
    use makepad_widgets::makepad_platform::thread::{checkpoint, worker_wanted};
    // The study's worker only waits while the hub solves, its CPU held: at each look at the
    // hub's progress it gives that CPU to a job above it that waits for one (the person's slice)
    // and goes on once that one has run.
    let mut told = |p: Option<&hub::Progress>| {
        if worker_wanted() {
            let _ = checkpoint();
        }
        progress(p)
    };
    let level = kept.unwrap_or_else(|| Arc::new(fab_force::solve::Level::new(posed.grid.clone(), &posed.held, m)));
    let Some(solve) = Solve::start_on(hub, &level, posed, m, guess.as_deref(), opts, hub::Class::Interactive, keep, &mut || told(None))? else {
        return Ok(None);
    };
    drop(guess);
    let level = (!keep.is_empty()).then_some(level);
    let node = solve.node().to_string();
    Ok(solve.wait_ran(&mut told)?.map(|(answer, ran)| Solved { answer, level, on: solved_on(&node, ran.gpu), ran }))
}

/// `model`, without its samples when its size is solved once (past the study's own: Refine's):
/// nothing regrades it, and they are as large as its cells.
fn solved_once(once: bool, model: Arc<Model>) -> Arc<Model> {
    match once && model.lattice.is_some() {
        true => Arc::new(model.shed()),
        false => model,
    }
}

/// Bytes in a GiB, for the log's memory lines.
const GIB: f64 = (1u64 << 30) as f64;

/// Where a solve ran, in words: this machine's AI hub or a fleet node's, on its GPU or its CPUs
/// (the engine the program took, not whether the node has a GPU).
fn solved_on(node: &str, gpu: bool) -> String {
    let device = if gpu { "GPU" } else { "CPUs" };
    let host = node.split("://").last().unwrap_or(node);
    match host.starts_with("127.0.0.1:") || host.starts_with("localhost:") {
        true => format!("this machine's AI hub ({device})"),
        false => format!("the AI hub at {host} ({device})"),
    }
}

/// Two grids on the same lattice: a displacement on one is one on the other.
fn same_lattice(a: &fab_force::grid::Grid, b: &fab_force::grid::Grid) -> bool {
    a.n == b.n && a.h == b.h && a.origin == b.origin
}

/// Why a solve ended early: a newer study made it stale, or the solve failed.
enum Stop {
    Stale,
    Failed(String),
}

impl From<String> for Stop {
    fn from(why: String) -> Stop {
        Stop::Failed(why)
    }
}

/// The densities the floor tries: whole percents, from [`FLOOR_LOWEST`].
const FLOOR_STEP: f64 = 0.01;
const FLOOR_LOWEST: f64 = 0.05;

/// Where a safety factor is first held over the densities tried.
enum Held {
    At(f64),
    /// Already at the lowest density tried.
    Lowest,
    /// Not even solid.
    Never,
}

/// One cell size of the floor's solves: the part's samples there (only the materials of the
/// cells change), its loads' regions, and the last answer solved here there (the next solve
/// here starts from it).
struct Level {
    base: Arc<Model>,
    conditions: Vec<fab_force::bc::Condition>,
    last: Option<Arc<Outcome>>,
}

/// A density as the floor tries it: a whole percent, from [`FLOOR_LOWEST`].
fn whole(d: f64) -> f64 {
    ((d / FLOOR_STEP).round() * FLOOR_STEP).clamp(FLOOR_LOWEST, 1.0)
}

/// What the densities tried say of one safety factor's floor.
enum Search {
    Found(Held),
    /// An end of the range, to try.
    End(f64),
    /// Densities inside the bracket, to try.
    Inside(Vec<f64>),
}

/// The floor's solves: each density at the floor's cell sizes and the finest one on the
/// shifted grid, the finest read and checked against the others ([`Reading`]). A round's
/// densities go as one background batch over every AI hub that answers ([`hub::analyse`]).
struct Sweep<'a> {
    /// The part's materials as printed: its filament, and its zones, which keep their settings
    /// under each print.
    part: &'a Materials,
    /// Coarser first, then (`shifted`) the finest on the shifted grid.
    levels: Vec<Level>,
    shifted: bool,
    opts: &'a Options,
    send: &'a dyn Fn(Note),
    stale: &'a dyn Fn() -> bool,
    hub: &'a Hub,
    solves: usize,
    /// (density, its reading) at the walls printed.
    tried: Vec<(f64, Reading)>,
}

impl Sweep<'_> {
    /// The safety factor's readings under each of `prints`, each solved at every cell size: one
    /// batch.
    fn readings(&mut self, prints: &[Print]) -> Result<Vec<Reading>, Stop> {
        let total = self.levels.len();
        // the levels at the steps' own grids
        let n = total - self.shifted as usize;
        // job i is level i % total of its print, from that level's last answer (the shifted
        // level's, else the finest one's carried over)
        let mut jobs = Vec::new();
        for p in prints {
            let m = Materials::new(self.part.filament, p, true).with_zones(self.part.zones.clone());
            for (k, level) in self.levels.iter().enumerate() {
                let posed = level.base.with_materials(&m).pose(&level.conditions)?;
                let own = level.last.as_ref().filter(|a| same_lattice(&a.grid, &posed.grid));
                let from = (self.shifted && k == n).then(|| self.levels[n - 1].last.as_ref()).flatten();
                let guess = match (own, from) {
                    (Some(a), _) => Some(a.solution.u.clone()),
                    (None, Some(f)) => Some(carry(&f.grid, &f.solution.u, &posed.grid)),
                    (None, None) => None,
                };
                jobs.push(Job { posed, materials: m.clone(), guess });
            }
        }
        let answers = hub::analyse(self.hub, jobs, self.opts, self.stale, hub::Class::Background);
        if (self.stale)() {
            return Err(Stop::Stale);
        }
        let answers = answers.into_iter().collect::<Result<Vec<Outcome>, String>>()?;
        self.solves += answers.len();
        let shifted = self.shifted;
        let readings = answers.chunks(total).map(|a| Reading::of(&a[n - 1], n.checked_sub(2).map(|i| &a[i]), shifted.then(|| &a[n]))).collect();
        // each level's next solves start from its answer at the batch's last density
        let last = answers.len() - total;
        for (k, a) in answers.into_iter().skip(last).enumerate() {
            self.levels[k].last = Some(Arc::new(a));
        }
        Ok(readings)
    }

    /// The readings at densities `ds` and the walls printed, those not tried yet.
    fn at(&mut self, print: &Print, ds: &[f64]) -> Result<(), Stop> {
        let mut new: Vec<f64> = Vec::new();
        for d in ds.iter().map(|&d| whole(d)) {
            if !self.tried.iter().map(|t| t.0).chain(new.iter().copied()).any(|x| (x - d).abs() < 1e-9) {
                new.push(d);
            }
        }
        if new.is_empty() {
            return Ok(());
        }
        let list = new.iter().map(|d| format!("{:.0}", 100.0 * d)).collect::<Vec<_>>().join(", ");
        (self.send)(Note::Floor(ForceFloor { status: format!("Finding the infill floor: {list} %, solve {}...", self.solves + 1), running: true, rows: vec![("Tried".into(), tried(&self.tried))] }));
        let prints: Vec<Print> = new.iter().map(|&d| Print { density: d, ..print.clone() }).collect();
        let readings = self.readings(&prints)?;
        self.tried.extend(new.into_iter().zip(readings));
        Ok(())
    }

    /// The lowest density at which the safety factor is at least `target`, to a percent, as far
    /// as the densities tried tell: found, or what to try next. Inside the bracket, `slots`
    /// densities spread across it; with one, a line through the bracket on the logs of both (a
    /// power law's straight line), halved when two such guesses in a row landed on the same side
    /// (`stuck`). It goes by the value its rows show: the reported safety factor where the
    /// cell sizes agree, else the finest cells' reading (the rows say which).
    fn search(&self, target: f64, slots: usize, stuck: bool) -> Search {
        let sf = |t: &(f64, Reading)| (t.0, t.1.shown());
        let below = self.tried.iter().map(sf).filter(|t| t.1 < target).max_by(|a, b| a.0.total_cmp(&b.0));
        let over = self.tried.iter().map(sf).filter(|t| t.1 >= target).min_by(|a, b| a.0.total_cmp(&b.0));
        let (lo, hi) = match (below, over) {
            (_, None) if self.tried.iter().any(|t| t.0 >= 1.0) => return Search::Found(Held::Never),
            (_, None) => return Search::End(1.0),
            (None, Some(_)) if self.tried.iter().any(|t| t.0 <= FLOOR_LOWEST + 1e-9) => return Search::Found(Held::Lowest),
            (None, Some(_)) => return Search::End(FLOOR_LOWEST),
            (Some(lo), Some(hi)) => (lo, hi),
        };
        if hi.0 - lo.0 <= FLOOR_STEP + 1e-9 {
            return Search::Found(Held::At(hi.0));
        }
        let inside = |d: f64| ((d / FLOOR_STEP).round() * FLOOR_STEP).clamp(lo.0 + FLOOR_STEP, hi.0 - FLOOR_STEP);
        if slots > 1 {
            let mut ds: Vec<f64> = (1..=slots).map(|k| inside(lo.0 + (hi.0 - lo.0) * k as f64 / (slots + 1) as f64)).collect();
            ds.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
            return Search::Inside(ds);
        }
        let line = (lo.1 > 0.0 && hi.1.is_finite()).then(|| {
            let t = (target.ln() - lo.1.ln()) / (hi.1.ln() - lo.1.ln());
            (lo.0.ln() + t * (hi.0.ln() - lo.0.ln())).exp()
        });
        Search::Inside(vec![inside(line.filter(|_| !stuck).unwrap_or(0.5 * (lo.0 + hi.0)))])
    }
}

/// The densities tried and their safety factors, in order of density.
fn tried(t: &[(f64, Reading)]) -> String {
    let mut t = t.to_vec();
    t.sort_by(|a, b| a.0.total_cmp(&b.0));
    t.iter().map(|(d, r)| format!("{:.0} %: {}", 100.0 * d, reading_text(r))).collect::<Vec<_>>().join(" \u{b7} ")
}

/// A reading in a floor row: its reported value, or its finest where the cell sizes disagree,
/// marked so.
fn reading_text(r: &Reading) -> String {
    match r.sf() {
        Some(sf) => sf_text(sf),
        None => format!("{} (not converged)", sf_text(r.finest())),
    }
}

/// How a study's or a floor's job on the board ends.
enum End {
    Done,
    Failed(String),
    /// Stopped: by its Cancel, or because the study is no longer shown.
    Stopped,
    FloorDone,
    FloorFailed(String),
    FloorStopped,
}

fn sf_text(sf: f64) -> String {
    if sf.is_finite() { format!("{sf:.2}") } else { "unloaded".into() }
}

/// The infill floor ([`ForceFloor`]) at its cell sizes (`r.steps`): the density where
/// the part's safety factor (its finest cells', away from the fixtures) falls to 1 and to 2,
/// then the safety factor at one wall loop fewer and two more at the density printed. Each
/// density is solved at every step, so its reading is checked against the cells before. The
/// samples are the study's when it has them at those sizes. `m`: the part's materials as
/// printed.
fn floor(r: &Run, m: &Materials, sample: &mut dyn FnMut(f64, V3, &Materials) -> Model, opts: &Options, send: &dyn Fn(Note), stale: &dyn Fn() -> bool) -> Result<(), Stop> {
    let begun = std::time::Instant::now();
    let print = &m.print;
    let mut level = |kept: Option<&Kept>, h: f64, shift: V3| {
        // (a model that shed its samples does not regrade: sampled again)
        let base = match kept.filter(|k| k.model.lattice.is_some()) {
            Some(k) => k.model.clone(),
            None => Arc::new(sample(h, shift, m)),
        };
        let conditions = base.conditions(&r.loads);
        Level { base, conditions, last: kept.and_then(|k| k.answer.clone()) }
    };
    let mut levels: Vec<Level> = r.steps.iter().map(|&h| level(r.models.iter().find(|k| (k.model.h - h).abs() < 1e-9), h, [0.0; 3])).collect();
    // the finest cells on the shifted grid too: each density's verdict sees the alignment
    let shifted = r.steps.len() >= 2;
    if let (true, Some(&h)) = (shifted, r.steps.last()) {
        levels.push(level(r.shifted.as_ref().filter(|k| (k.model.h - h).abs() < 1e-9), h, study::shift(h)));
    }
    // a round tries as many densities inside a bracket as there are hubs to solve them at once
    let hubs = r.hub.nodes().len();
    let slots = hubs.max(1);
    let mut s = Sweep { part: m, levels, shifted, opts, send, stale, hub: &r.hub, solves: 0, tried: Vec::new() };
    let density = whole(print.density);
    // the study's own reading at the density printed, when it has one at these cells
    let known = r.now.filter(|_| (density - print.density).abs() < 1e-9);
    // the first round: the density printed, and the wall loops around it there
    let walls = print.wall_loops;
    let others: Vec<_> = [walls.saturating_sub(1), walls + 1, walls + 2].into_iter().filter(|&w| w >= 1 && w != walls).collect();
    let mut prints: Vec<Print> = known.is_none().then(|| Print { density, ..print.clone() }).into_iter().collect();
    prints.extend(others.iter().map(|&w| Print { wall_loops: w, density, ..print.clone() }));
    (send)(Note::Floor(ForceFloor { status: format!("Finding the infill floor: {:.0} % and its wall loops, solve 1...", 100.0 * density), running: true, rows: Vec::new() }));
    let mut first = s.readings(&prints)?.into_iter();
    let now = match known {
        Some(now) => now,
        None => first.next().ok_or_else(|| Stop::Failed("no reading at the density printed".into()))?,
    };
    s.tried.push((density, now));
    let mut by_walls = vec![(walls, now)];
    by_walls.extend(others.into_iter().zip(first));
    by_walls.sort_by_key(|(w, _)| *w);
    // then where the safety factor falls to 1 and to 2, a round of both at a time
    let targets = [1.0, 2.0];
    let mut found: [Option<Held>; 2] = [None, None];
    // each search's lone guesses inside its bracket: over its target or not
    let mut sides: [Vec<bool>; 2] = [Vec::new(), Vec::new()];
    while found.iter().any(Option::is_none) {
        let open = found.iter().filter(|f| f.is_none()).count();
        let mut want: Vec<(usize, f64)> = Vec::new();
        let mut guesses: [Option<f64>; 2] = [None, None];
        for k in 0..2 {
            if found[k].is_some() {
                continue;
            }
            let stuck = sides[k].len() >= 2 && sides[k][sides[k].len() - 1] == sides[k][sides[k].len() - 2];
            match s.search(targets[k], (slots / open).max(1), stuck) {
                Search::Found(held) => found[k] = Some(held),
                Search::End(d) => want.push((k, d)),
                Search::Inside(ds) => {
                    if let [d] = ds[..] {
                        guesses[k] = Some(d);
                    }
                    want.extend(ds.into_iter().map(|d| (k, d)));
                }
            }
        }
        if want.is_empty() {
            continue;
        }
        s.at(print, &want.iter().map(|w| w.1).collect::<Vec<_>>())?;
        for k in 0..2 {
            if let Some(&(_, r)) = guesses[k].and_then(|d| s.tried.iter().find(|t| (t.0 - d).abs() < 1e-9)) {
                sides[k].push(r.shown() >= targets[k]);
            }
        }
    }
    // the loop ends once both are found
    let [breaks, holds] = found.map(Option::unwrap);
    let held = |t: f64, h: &Held| match h {
        Held::At(d) => format!("SF {t:.0} at {:.0} %", 100.0 * d),
        Held::Lowest => format!("SF {t:.0} at {:.0} % or less", 100.0 * FLOOR_LOWEST),
        Held::Never => format!("SF {t:.0} not even at 100 %"),
    };
    let rows = vec![
        ("Printed now".into(), format!("{:.0} % infill, {walls} wall loops: SF {}", 100.0 * density, reading_text(&now))),
        ("Wall loops".into(), format!("{} (at {:.0} %)", by_walls.iter().map(|(w, r)| format!("{w}: {}", reading_text(r))).collect::<Vec<_>>().join(" \u{b7} "), 100.0 * density)),
        ("Tried".into(), tried(&s.tried)),
        ("Took".into(), match hubs {
            1 => format!("{} solves in {:.1} s on the AI hub", s.solves, begun.elapsed().as_secs_f64()),
            n => format!("{} solves in {:.1} s on {n} AI hubs", s.solves, begun.elapsed().as_secs_f64()),
        }),
    ];
    // its resolution in its line: its finest cells (finer waits on a faster solver), and
    // where those cells and the ones before disagree
    let h = *r.steps.last().unwrap();
    let checked = r.steps.len().checked_sub(2).map_or(String::new(), |i| format!(" against {:.2} mm and shifted half a cell", r.steps[i]));
    let apart = s.tried.iter().filter(|t| t.1.sf().is_none()).count();
    let note = if apart > 0 { format!(". Not converged at {apart} of {} densities tried", s.tried.len()) } else { String::new() };
    send(Note::Floor(ForceFloor { status: format!("Infill floor ({h:.2} mm cells{checked}): {}, {}{note}", held(1.0, &breaks), held(2.0, &holds)), running: false, rows }));
    Ok(())
}

/// The Solve row of an answer stopped at its iteration limit above its tolerance: its cells,
/// iterations and residual.
fn unsolved_row((h, a): (f64, &Outcome)) -> String {
    let tolerance = Options::default().tolerance;
    format!(
        "approximate: the solve on {h:.2} mm cells stopped at {} iterations, residual {:.0e} of the load (its tolerance {tolerance:.0e})",
        a.solution.iterations, a.solution.residual
    )
}

fn about(seconds: f64) -> String {
    if seconds < 90.0 {
        format!("about {:.0} s", seconds.round().max(1.0))
    } else {
        format!("about {:.0} min", (seconds / 60.0).round())
    }
}

fn time_suffix(seconds: Option<f64>) -> String {
    seconds.map_or(String::new(), |s| format!(", {}", about(s)))
}

/// A study's safety factor as the panel reads it, at cells of `hs.0` against `hs.1`: a number
/// where the cell sizes agree, else that they do not; approximate where a solve it reads stopped
/// short of its tolerance, at residual `short` (never "converged" then, the sizes agreeing or not).
fn safety_text(r: &Reading, hs: (f64, Option<f64>), short: Option<f64>, unchecked: &str) -> String {
    let Some(v) = r.sf() else { return apart_text("not converged", r.away, hs, short) };
    // the verdict the value is from: near the fixtures where they agree there and it is lower
    let from = match r.near {
        Verdict::Converged { .. } if r.near.reported() == v => r.near,
        _ => r.away,
    };
    match (from, hs.1) {
        (Verdict::Converged { fine, coarse, shifted }, Some(c)) => format!("{} ({})", sf_text(v), converged(short, &reads(v, fine, coarse, shifted, hs.0, c))),
        _ => format!("{} ({unchecked})", sf_text(v)),
    }
}

/// The safety factor near the fixtures, at cells of `hs.0` against `hs.1`, approximate as the
/// safety factor's.
fn near_text(v: Verdict, hs: (f64, Option<f64>), short: Option<f64>, unchecked: &str) -> String {
    match v {
        Verdict::Unchecked(v) => format!("{} ({unchecked})", sf_text(v)),
        Verdict::Converged { fine, coarse, shifted } => {
            let r = v.reported();
            format!("{} ({})", sf_text(r), converged(short, &reads(r, fine, coarse, shifted, hs.0, hs.1.unwrap_or(hs.0))))
        }
        Verdict::Apart { .. } => apart_text("stress concentration at the fixture, not converged", v, hs, short),
    }
}

fn apart_text(what: &str, v: Verdict, hs: (f64, Option<f64>), short: Option<f64>) -> String {
    match (v, hs.1) {
        (Verdict::Apart { fine, coarse, shifted }, Some(c)) => {
            let change = if fine == coarse {
                "held steady".into()
            } else {
                let direction = if fine < coarse { "fell" } else { "rose" };
                let percent = (fine / coarse - 1.0).abs() * 100.0;
                if percent.is_finite() { format!("{direction} {percent:.0} %") } else { direction.into() }
            };
            let hf = hs.0;
            let readings = noted(short, &format!("{} at {hf:.2} mm, {}", sf_text(fine), against(coarse, shifted, c)));
            format!("{what} ({change} from {c:.2} to {hf:.2} mm cells; {readings})")
        }
        _ if short.is_some() => format!("{what} ({})", noted(short, "")),
        _ => what.to_string(),
    }
}

/// Cell sizes that agree, `against` them: "converged", unless a solve read stopped short of its
/// tolerance at residual `short`.
fn converged(short: Option<f64>, against: &str) -> String {
    match short {
        Some(_) => noted(short, against),
        None => format!("converged: {against}"),
    }
}

/// `rest` in a row's brackets, after the word that a solve its figures read stopped at residual
/// `short`, above its tolerance, where one did.
fn noted(short: Option<f64>, rest: &str) -> String {
    match (short, rest.is_empty()) {
        (Some(r), true) => format!("approximate: its solve stopped at residual {r:.0e}"),
        (Some(r), false) => format!("approximate: its solve stopped at residual {r:.0e}; {rest}"),
        (None, _) => rest.to_string(),
    }
}

/// The readings a reported value `v` is from: the finest cells' (of `hf` mm) and what they were
/// checked against, and that the margin is in it where it took `v` under all of them.
fn reads(v: f64, fine: f64, coarse: f64, shifted: Option<f64>, hf: f64, hc: f64) -> String {
    let lowest = shifted.map_or(fine, |s| s.min(fine)).min(coarse);
    let margin = if sf_text(v) != sf_text(lowest) { "; margin included" } else { "" };
    format!("{} at {hf:.2} mm cells, {}{margin}", sf_text(fine), against(coarse, shifted, hc))
}

/// What the finest cells' value was checked against: the coarser cells' (of `hc` mm), and the
/// finest cells' on the grid shifted half a cell once solved.
fn against(coarse: f64, shifted: Option<f64>, hc: f64) -> String {
    match shifted {
        Some(s) => format!("{} at {hc:.2} mm cells, {} shifted half a cell", sf_text(coarse), sf_text(s)),
        None => format!("{} at {hc:.2} mm cells", sf_text(coarse)),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_slicer::geom::Pt;
    use makepad_fab_toolpath::{ExtrusionPath, Island, Layer, ObjectLayer, Role as R, Toolpaths};

    /// The settings line names only what each zone sets, with singular and plural counts.
    #[test]
    fn the_settings_line_names_its_zones() {
        let core = Zone { at: Arc::new(|_| -1.0), bounds: None, density: Some(1.0), infill: None, wall_loops: None };
        assert_eq!(describe_zones(&[]), "", "no zones leave the settings line alone");
        assert_eq!(describe_zones(&[core.clone()]), " · 1 zone 100%", "one zone sets only its density");
        let walls = Zone { density: None, wall_loops: Some(5), ..core.clone() };
        assert_eq!(describe_zones(&[core.clone(), walls]), " · 2 zones 100%, 5 walls", "each zone names its own settings");
        let all = Zone { density: Some(0.255), infill: Some(Infill::Grid), wall_loops: Some(1), ..core.clone() };
        assert_eq!(describe_zones(&[core, all]), " · 2 zones 100%, 26% Grid 1 wall", "density, pattern and walls stay in order");
    }

    fn path(role: R, pts: &[[f64; 2]]) -> ExtrusionPath {
        ExtrusionPath { role, pts: pts.iter().map(|p| Pt::from_mm(p[0], p[1])).collect(), width: 0.45, height: 0.2, ..Default::default() }
    }

    /// A part's beads from its slice: its walls' and skins' extruding segments on their layer's
    /// top over the raft, each its own width; another part's paths, sparse infill and supports
    /// left out.
    #[test]
    fn a_parts_slice_gives_its_wall_and_skin_beads() {
        let mut wall = path(R::OuterWall, &[[0.0, 0.0], [10.0, 0.0], [10.0, 5.0], [0.0, 5.0]]);
        // the middle segment travels; the last one is a narrower bead
        wall.dry = vec![false, true, false];
        wall.flow = vec![1.0, 1.0, 0.5];
        let skin = path(R::TopSurface, &[[1.0, 1.0], [9.0, 1.0]]);
        let sparse = path(R::SparseInfill, &[[1.0, 2.0], [9.0, 2.0]]);
        let part = |object: usize, support_only: bool| ObjectLayer { object, support_only, islands: vec![Arc::new(Island { walls: vec![wall.clone()], fills: vec![skin.clone(), sparse.clone()] })], ..Default::default() };
        let layer = Layer { index: 0, print_z: 1.2, height: 0.2, objects: vec![part(1, false), part(0, false), part(1, true)] };
        let t = Toolpaths { layers: vec![layer], ..Default::default() };
        let beads = beads_of(&t, 1, 1.0);
        let walls: Vec<&Bead> = beads.iter().filter(|b| b.role == BeadRole::Wall).collect();
        let skins: Vec<&Bead> = beads.iter().filter(|b| b.role == BeadRole::Skin).collect();
        assert_eq!((walls.len(), skins.len()), (2, 1), "two wall segments extrude, one skin line: {beads:?}");
        assert!(beads.iter().all(|b| (b.a[2] - 0.2).abs() < 1e-9 && (b.b[2] - 0.2).abs() < 1e-9 && b.height == 0.2), "on the layer's top over the raft: {beads:?}");
        assert!((walls[0].b[0] - 10.0).abs() < 1e-6 && (walls[1].a[1] - 5.0).abs() < 1e-6, "the travel left out: {walls:?}");
        assert!((walls[0].width - 0.45).abs() < 1e-9 && (walls[1].width - 0.225).abs() < 1e-9, "each bead its own width: {walls:?}");
    }

    /// A part's `m.settings` region reaches its Force study as a zone, read from the part's own
    /// settings with the region's over them: a 100 % core holds in the cube's middle, in the
    /// frame the study samples the part in, and not near its corner, and the study's materials
    /// give the middle the core's infill.
    #[test]
    fn a_parts_settings_region_is_a_zone_of_its_study() {
        let source = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet cube = m.box({size: [20mm 20mm 20mm] at: [0 10mm 0]})\nobject(cube)\nm.settings(m.box({size: [8mm 8mm 8mm] at: [0 10mm 0]}), {sparse_infill_density: \"100%\"})\n";
        let model = fab_model::FabModel::with_script_files("part", "part.cad.splash", source, Default::default(), Default::default());
        let setup = crate::print::CheckSetup::fab_defaults();
        let mut e = fab_model::eval::Evaluator::new(setup.machine().expect("Fab's default printer"));
        let ev = model.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
        let (print, _, _, zones) = print_of(&setup, &ev, 0).expect("the part's settings");
        let read: Vec<_> = zones.iter().map(|z| (z.density, z.infill, z.wall_loops)).collect();
        assert_eq!(read, vec![(Some(1.0), None, None)], "one zone, of 100 % infill and nothing else: {zones:?}");
        // the cube's middle and a point 2 mm in from its corner, in the frame its study samples
        // it in (its mesh's, printer mm)
        let b = ev.plate.parts[0].bounds();
        assert!((0..3).all(|a| (b.max[a] - b.min[a] - 20.0).abs() < 1e-3), "the cube's box: {b:?}");
        let (mid, corner): (V3, V3) = (std::array::from_fn(|a| 0.5 * (b.min[a] + b.max[a])), b.min.map(|v| v + 2.0));
        assert!(zones[0].holds(mid) && !zones[0].holds(corner), "the core holds in the middle ({mid:?}) and not near the corner ({corner:?})");
        let m = Materials::new(FILAMENTS[0], &print, true).with_zones(zones);
        let (inside, outside) = (m.at(mid).0 as usize, m.at(corner).0 as usize);
        assert_eq!((m.fills[inside].density, m.fills[outside].density), (1.0, print.density), "the middle's infill is the core's, the corner's the part's");
    }

    /// The Force tool's tip says what a click takes: on a block with a 3 mm hole, the hole's wall
    /// by its size and the top by its area, each with what the kind does there and what Alt
    /// reads instead; with Alt the wall reads as a face.
    #[test]
    fn the_force_tools_tip_names_the_hole_or_the_face_and_what_a_click_does() {
        use fab_force::bench::{boxed, isotropic};
        let (lo, hi, r) = ([0.0; 3], [12.0, 8.0, 4.0], 1.5);
        let mut distance = |ps: &[V3], out: &mut [f64]| ps.iter().zip(out.iter_mut()).for_each(|(p, o)| *o = boxed(*p, lo, hi).max(r - ((p[0] - 6.0).powi(2) + (p[1] - 4.0).powi(2)).sqrt()));
        let model = Arc::new(Model::new(lo, hi, 0.5, &mut distance, &isotropic()));
        let wall = Hover::at(&model, [6.0 + r, 4.0, 2.0], false).unwrap();
        let text = wall.text(ForceKind::Fixed, 0.0);
        assert!(text.starts_with("Hole wall, ") && text.ends_with(" mm across: click to hold it\nAlt reads it as a face"), "{text}");
        let top = Hover::at(&model, [2.0, 2.0, 4.0], false).unwrap();
        assert!(top.text(ForceKind::Force, 10.0).starts_with("Face, 89 mm\u{b2}: click to push 10 N\n"), "{}", top.text(ForceKind::Force, 10.0));
        assert!(top.text(ForceKind::Pressure, 0.25).contains("click to press 0.25 MPa on it"), "the dial's size: {}", top.text(ForceKind::Pressure, 0.25));
        assert_eq!(Hover::at(&model, [6.0 + r, 4.0, 2.0], true).unwrap().pick, Pick::Face, "Alt reads the wall as a face");
    }

    /// The Force strip's dial sizes a load: a force keeps its direction, a pressure or a torque
    /// its sign; a new force pushes into the face clicked.
    #[test]
    fn the_dial_sizes_a_load_keeping_its_direction_and_sign() {
        let load = |kind, value| ForceLoad { kind, value, top: true, normal: [0.0, 0.0, 1.0], ..Default::default() };
        let pushed = load(ForceKind::Force, [3.0, 0.0, -4.0]);
        assert_eq!(pushed.size(), 5.0);
        assert_eq!(pushed.sized(50.0), [30.0, 0.0, -40.0]);
        assert_eq!(load(ForceKind::Pressure, [-0.2, 0.0, 0.0]).sized(0.5), [-0.5, 0.0, 0.0], "a pull stays a pull");
        assert_eq!(load(ForceKind::Torque, [1000.0, 0.0, 0.0]).sized(250.0), [250.0, 0.0, 0.0]);
        assert_eq!(ForceKind::Force.value(10.0, [0.0, 0.0, 1.0]), [0.0, 0.0, -10.0], "a new force pushes into the face");
        assert_eq!(fab::ui::force::size_text(0.1), "0.1");
        assert_eq!(fab::ui::force::size_text(12.5), "12.5");
        assert_eq!(fab::ui::force::size_text(1000.0), "1000");
    }
}

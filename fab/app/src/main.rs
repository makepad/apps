//! Fab: the 3D modeler for printing. The shell (viewport, navigation,
//! tools, panels) shows the plate the model worker evaluates; the docked
//! chat and outside agents build the model through the `fab.*` tools.

mod arm;
mod buckling;
mod force;
mod grow_live;
mod calibrate;
mod chat;
mod closed_box;
mod device_assembly;
mod motion_link;
mod head;
mod img2mesh;
mod code_edit;
mod demo_part;
mod elec_flash;
mod elec_radio;
mod elec_fw;
mod elec_mic;
mod elec_net;
mod elec_screen;
mod elec_host;
mod elec_export;
mod live_slice;
mod marks;
mod mass;
mod picks;
mod mech_check;
mod mech_flex;
mod mech_loose;
mod mechanics;
mod model_host;
mod ghost;
mod sdf_live;
mod param_view;
mod part_models;
mod kicad_models;
mod paint_edit;
mod pattern_live;
mod plate_edit;
mod plate_scene;
mod plate_paint;
mod switch_watch;
mod print;
mod review;
mod scan;
mod shape_edit;
mod sla;
mod study_edit;
mod undo_order;
mod turn_undo;
mod variant_sheet;
mod voice;
mod xr;
mod xr_edit;
mod xr_handles;
mod xr_mech;

/// Tests and the process's action channel. `Cx::new` replaces the process-wide action sender, so
/// a test that makes a Cx holds `cx_shared()` while it runs, and a test that reads the actions
/// posted to its Cx holds `cx_owned()`: no other test's Cx takes those actions meanwhile. A test
/// that also holds `live_slice::HUB_TESTS` takes that first.
#[cfg(test)]
static CX_TESTS: std::sync::RwLock<()> = std::sync::RwLock::new(());

#[cfg(test)]
pub(crate) fn cx_shared() -> std::sync::RwLockReadGuard<'static, ()> {
    CX_TESTS.read().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
pub(crate) fn cx_owned() -> std::sync::RwLockWriteGuard<'static, ()> {
    CX_TESTS.write().unwrap_or_else(|e| e.into_inner())
}

use makepad_widgets::*;
use fab::{
    api::*, loader::{self, LoadCoordinator}, tools,
};
use fab::ui::stage::StageAction;
use fab::ui::topbar::Caption;
use fab::workspaces::{ListOp, WorkspaceId, Workspaces};
use makepad_ai_services::wire::{ServiceManifest, TaskAction, TaskCard, ToolResult};
use makepad_app_shell::chat::{self as shell_chat, ChatDock, ChatHost};
use makepad_model::json::{self, Value};
use model_host::{HostAction, ModelHost, Status};
use undo_order::{Target, Tops, UndoOrder};
use makepad_widgets::makepad_platform::restart::RestartFields;
use makepad_widgets::makepad_platform::thread::{to_ui_oneshot, Class, ToUIOneshotReceiver};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::mpsc::TryRecvError;
use makepad_fab_slice::piece;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

// The viewport hands over its picture at the plate list's size: the list's shrink only copies it.
const _: () = assert!(PICTURE_SIDE == fab_ai::plates::THUMB_SIDE);

app_main!(App);

/// Counts each thread's allocations, so the live circuit run measures what its snapshots hold
/// (`fab_elec_sim::alloc`, the snapshots' byte budget): a relaxed load a call, and a thread-local
/// add or subtract while a snapshot is measured. Under the count, large blocks go straight to the
/// OS on macOS, so that a freed one leaves the process (`makepad_large_alloc`).
#[global_allocator]
static COUNTING: fab_elec_sim::alloc::Counting<makepad_large_alloc::LargeDirect<std::alloc::System>> =
    fab_elec_sim::alloc::Counting(makepad_large_alloc::LargeDirect(std::alloc::System));

/// The mechanics engine's log handler: each message as a line of the app log.
fn log_mechanics(msg: &makepad_dynamics::util_errmem::LogMessage<'_>) {
    makepad_widgets::log!("mechanics: {}", msg.text());
}

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.title: "Fab"
                window.inner_size: vec2(1600, 1000)
                pass +: {
                    clear_color: fab.color_area
                }
                // One bar: the window's controls where the platform keeps
                // them, and Fab's caption (`FabTopBar`) in the rest. The
                // stock title goes (the document's name is the title).
                caption_bar +: {
                    // Its own list: a frame that changes nothing in it reuses it.
                    new_batch: true
                    // Where the platform gives no caption height (no window
                    // chrome of its own), the controls' own height.
                    height: 38
                    draw_bg.color: fab.color_topbar
                    caption_label +: {
                        flow: Right spacing: 0 align: Align{x: 0.0 y: 0.5}
                        label +: {visible: false}
                        caption_icon +: {width: 0 height: 0 margin: 0}
                        // the traffic lights' room where macOS draws them over the caption
                        caption_window_gap := View{width: 72 height: 1}
                        caption := FabTopBar{}
                    }
                }
                body +: {
                    flow: Down
                    spacing: 0
                    margin: 0
                    padding: 0
                    // The one chat (F10 or Cmd+K shows and hides it) docks
                    // to the right of the shell.
                    chat_split := Splitter{width: Fill height: Fill axis: SplitterAxis.Horizontal align: SplitterAlign.FromB(380) size: 6
                        // Folded here, not from code: a live patch re-applies this template.
                        collapse: SplitterCollapse.B
                        fold: SplitterCollapse.B
                        a: View{width: Fill height: Fill
                            shell := FabShell{}
                        }
                        // its own list: the chat redraws itself as it changes
                        b: View{width: Fill height: Fill new_batch: true
                            ai_chat := AiChatPanel{width: Fill height: Fill}
                        }
                    }
                    // Offscreen review passes for fab.render (1x1, invisible).
                    review := mod.widgets.FabReviewRenderer{}
                    // The headset's view, in a process that serves one (1x1, invisible).
                    xr := mod.widgets.FabXr{}
                }
            }
        }
    }
}

const PROFILE_EXTENSIONS: &[&str] = &["json", "bbscfg", "bbsflmt"];

/// The parts of `new` (plate indices) whose toolpaths differ from `old`'s: another shape,
/// place, filament, nozzle, colours or per-part settings. `None` when the plate is not the
/// same list of parts or its settings changed: every part is dated.
fn changed_parts(old: &fab_model::eval::Evaluated, new: &fab_model::eval::Evaluated) -> Option<Vec<u16>> {
    let (a, b) = (&old.plate, &new.plate);
    if a.parts.len() != b.parts.len() || a.settings != b.settings || old.frames.len() != new.frames.len() || old.fields.len() != new.fields.len() || old.paint.len() != new.paint.len() {
        return None;
    }
    let mut out = Vec::new();
    for (i, (pa, pb)) in a.parts.iter().zip(&b.parts).enumerate() {
        if pa.object != pb.object {
            return None;
        }
        let (fa, fb) = (&old.frames[i], &new.frames[i]);
        let shape = match (&old.fields[i], &new.fields[i]) {
            (Some(x), Some(y)) => x.key == y.key && x.to_printer == y.to_printer,
            (None, None) => Arc::ptr_eq(&fa.local, &fb.local) || *fa.local == *fb.local,
            _ => false,
        };
        let paint = match (&old.paint[i], &new.paint[i]) {
            (None, None) => true,
            (Some(x), Some(y)) => Arc::ptr_eq(x, y),
            _ => false,
        };
        if !(shape && paint && fa.offset == fb.offset && pa.filament == pb.filament && pa.nozzle == pb.nozzle && pa.print == pb.print) {
            out.push(i as u16);
        }
    }
    Some(out)
}

/// A drag held at the highest value that built: what the plate notice says, and the background
/// build (a bisection step or the held build again) it is waiting for.
#[derive(Clone, Debug)]
struct Held {
    key: String,
    hold: fab::params::ParamValue,
    /// The newest raw failure text.
    why: String,
    message: String,
    /// The one preview this cap asked for: (generation, is_re_preview).
    next: Option<(u64, bool)>,
}

/// Files from outside dragged over the window: their names on the pointer's chip, and the drop
/// preview of their object scripts (its generation; none: none asked of the plate as it is).
#[derive(Clone, Debug)]
struct FileDrag {
    names: String,
    gen: Option<u64>,
    /// The object scripts among them: what the preview places.
    scripts: Vec<std::path::PathBuf>,
}

/// A parameter commit on the worker, to say how it ended.
#[derive(Clone, Debug)]
struct ParamCommit {
    id: u64,
    key: String,
    /// The cap line the drag already said (the asked value was swapped for the held one).
    said: Option<String>,
    /// The value asked: a commit refused for a lost selector shows it as a draft.
    value: fab_model::params::PValue,
}

/// A layout call held until the person's hand lets go or the layout call before it applied.
struct HeldLayout {
    id: u64,
    turn: u64,
    tool: String,
    args: Value,
    since: f64,
    /// The person's pointer, a finger or a key was down while it waited (the answer says so).
    hand: bool,
}

/// The temporary layout this turn opened, and per row workspace it changed the layout history's
/// depth before its first change and after its last, and whether another change came between.
struct TurnLayout {
    turn: u64,
    temp: Option<u64>,
    changed: Vec<TurnChange>,
}

struct TurnChange {
    id: WorkspaceId,
    before: usize,
    after: usize,
    /// A change not the turn's came between two of its changes: undoing to `before` would undo it.
    mixed: bool,
}

impl TurnLayout {
    fn temp_open(&self, workspaces: &Workspaces) -> bool {
        workspaces.temp_id().is_some() && self.temp == Some(workspaces.temp_gen())
    }

    /// Its Undo layout change: none when it changed no row workspace or a change of it is undone,
    /// else on, or off with why.
    fn undo_offer(&self, workspaces: &Workspaces) -> Option<Result<(), &'static str>> {
        let mut offer = Ok(());
        for change in &self.changed {
            let current = workspaces.history_len(change.id);
            if current < change.after {
                return None;
            }
            if current > change.after {
                offer = Err("Later layout changes came after it: View › Undo layout change goes back one at a time");
            } else if change.mixed && offer.is_ok() {
                offer = Err("Other layout changes came between its own: View › Undo layout change goes back one at a time");
            }
        }
        (!self.changed.is_empty()).then_some(offer)
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    /// The Scan workspace's scan: the headset's capture, kept and shown as it comes.
    #[rust]
    scan: scan::ScanLab,
    #[rust]
    state: AppState,
    #[rust]
    loader: LoadCoordinator,
    #[rust]
    loaders_registered: bool,
    #[rust]
    started: bool,
    /// Startup may resume Monitor once, until the person starts working or the first
    /// printer status and saved jobs have been considered (including idle/unknown jobs).
    #[rust]
    startup_monitor_done: bool,
    /// Mirrored from the menu layer's ordered open/closed broadcast.
    #[rust]
    menu_owner: Option<LiveId>,
    #[rust]
    delete_jobs: std::collections::HashSet<u64>,
    #[rust]
    calibration_job: Option<u64>,
    /// The model worker (started on the first event).
    #[rust]
    host: Option<ModelHost>,
    /// The electronics engine strand, started when its workspace is first opened.
    #[rust]
    elec: Option<elec_host::ElecHost>,
    /// The serial queue for models named by the open electronics design.
    #[rust]
    part_models: Option<part_models::PartModels>,
    /// The queue for KiCad library models named by the open electronics design.
    #[rust]
    kicad_models: Option<kicad_models::KicadModels>,
    /// Wakes the KiCad queue when a library host's pause expires.
    #[rust]
    kicad_timer: Timer,
    /// This computer's choices for KiCad models as last kept, and the model board's change count
    /// they were kept at: a part card's or an action's change is kept and its model asked again.
    #[rust]
    model_choices: (u64, std::collections::BTreeMap<String, fab_elec_parts::model_board::Choice>),
    /// The committed design whose missing models went to the queue and whose board masses were asked for.
    #[rust]
    models_from: Option<std::sync::Weak<fab_elec_design::Design>>,
    /// One result for the current design. Polled without waiting while its board masses are pending.
    #[rust]
    board_masses_rx: Option<ToUIOneshotReceiver<BTreeMap<String, fab_elec_mech::BoardMass>>>,
    /// Wakes the queue when the part service's pause expires.
    #[rust]
    models_timer: Timer,
    /// Pending history presses, routed after the preceding press settles.
    #[rust]
    undo_order: UndoOrder,
    /// The person's turn of the chat call being dispatched (`ChatHost::chat_turn_call`).
    #[rust]
    chat_turn: u64,
    /// The AI's turns and their steps, for Edit > Undo the AI's Last Turn and the buttons under
    /// each answer in the chat.
    #[rust]
    turn_undo: turn_undo::TurnUndo,
    #[rust]
    turn_layouts: Vec<TurnLayout>,
    /// The document and layout histories and temporary layout the chat's turn buttons reflect.
    #[rust]
    turn_shown: Option<(Tops, Tops, u64, u64, Option<WorkspaceId>, u64)>,
    /// The model worker meshes the field parts too (the view shows meshes: Mesh shading).
    #[rust]
    meshing_fields: bool,
    #[rust]
    chat: ChatDock,
    /// The headset's microphone into the chat (`voice`).
    #[rust]
    voice: voice::XrVoice,
    #[rust]
    calls: chat::Calls,
    /// What of the person's hand is down now: an AI layout change waits for it to let go.
    #[rust]
    hand: Hand,
    /// `fab.workspace_edit` and `fab.show` calls waiting for the hand to let go, or for the call
    /// before them to apply, in order. Each applies (or fails) on release.
    #[rust]
    held_layout: VecDeque<HeldLayout>,
    /// A layout call's actions are sent and not yet applied.
    #[rust]
    layout_in_flight: bool,
    /// Pictures being made into parts by the AI hub (`img2mesh.rs`).
    #[rust]
    img2mesh: img2mesh::Img2Mesh,
    /// A chat call's import answer, waiting for the render of the new part that goes with it.
    #[rust]
    img2mesh_answers: std::collections::HashMap<u64, String>,
    /// A picture aimed at an overhang goes out with what it shows.
    look_answers: std::collections::HashMap<u64, String>,
    /// `fab.compare` calls waiting for their views: what reads them.
    #[rust]
    compare_jobs: std::collections::HashMap<u64, Arc<fab_ai::compare::CompareJob>>,
    /// `fab.variants` calls between their captures.
    #[rust]
    variant_runs: std::collections::HashMap<u64, variant_sheet::VariantRun>,
    /// Render requests waiting for the review renderer.
    #[rust]
    review_queue: VecDeque<(u64, review::PreparedReview)>,
    /// The capture the review renderer draws, as of the last pump.
    #[rust]
    drawing: Option<review::CaptureProgress>,
    /// What is being made of a call's views once they are drawn, in words, until it answers.
    #[rust]
    after_views: std::collections::HashMap<u64, &'static str>,
    /// The plate the view shows.
    #[rust]
    plate: Option<Arc<fab_model::eval::Evaluated>>,
    /// The live slice while the desktop shows the Slice render mode.
    #[rust]
    live_view: Option<live_slice::LiveSlice>,
    /// The plate's slice is waited for where no view shows it (the Mechanics check of its supports).
    #[rust]
    toolpaths_wanted: Option<live_slice::ToolpathsWanted>,
    #[rust]
    slice_host: live_slice::SliceHost,
    /// The part move the live slice last heard of.
    #[rust]
    live_nudge: Option<(ElementId, fab::model::state::Pose)>,
    /// The slice view as last saved to this machine's preferences.
    #[rust]
    saved_slice_view: bool,
    /// The Slice panel's overhang list: the toolpaths it is of, and the toolpaths it was last
    /// asked for (the model host answers once per slice).
    #[rust]
    overhang_list_key: Option<(u64, u64)>,
    #[rust]
    overhang_list_asked: Option<(u64, u64)>,
    /// Something wanted slices when last looked (`slice_wanted`).
    #[rust]
    slice_was_wanted: bool,
    /// The colour mapping the model worker is putting on the plate: no slice goes out until
    /// the plate has it (sliced before, its parts would print on the tools of the mapping
    /// before, which the job's new materials refuse).
    #[rust]
    mapping_sent: Option<Arc<fab_model::eval::Mapped>>,
    /// The Mechanics workspace's live sim.
    #[rust]
    mech: mechanics::MechSim,
    /// Mechanics' closed box: a board's case closed, running its board.
    #[rust]
    closed_box: closed_box::ClosedBox,
    #[rust]
    device_assembly: device_assembly::DeviceAssembly,
    /// A board printed on the plate, moved by the Mechanics sim, tilts its IMU.
    #[rust]
    motion_link: motion_link::MotionLink,
    #[rust]
    mic: elec_mic::MicPlayback,
    /// The twin's firmware network: the Firmware pane's switch and tokens, to the runner.
    #[rust]
    elec_net: elec_net::ElecNet,
    /// The Arm workspace's lab (experimental).
    #[rust]
    arm: arm::ArmLab,
    /// The Force render mode's study.
    #[rust]
    force: force::ForceLab,
    /// `fab.force` calls waiting for the study to catch up with the plate: each call's id, and
    /// whether the lab has had an event since it came.
    #[rust]
    force_calls: Vec<(u64, bool)>,
    /// The Slice render mode's layers on a resin printer.
    #[rust]
    resin: sla::ResinView,
    #[rust]
    status: Status,
    #[rust]
    framed: bool,
    /// How each plate was left, by what was on the bed ([`Status::bed`]): a switch back shows it so.
    #[rust]
    views: std::collections::HashMap<(String, bool), PlateView>,
    /// How the document just put on the bed was left: it shows so once its scene installs.
    #[rust]
    restore: Option<PlateView>,
    /// AI calls that put another document on the bed, held while a gesture of the person's is
    /// under way: its edit lands on its own plate first.
    #[rust]
    held_switches: Vec<(u64, String, Value)>,
    /// `fab.picture` and `fab.colour_read` calls that take a picture the person sent in the chat,
    /// held until its file is written (on the task pool: [`makepad_app_aichat::PictureKept`]
    /// wakes the app then).
    #[rust]
    held_pictures: Vec<(u64, String, Value, makepad_ai_services::engine::SentPicture)>,
    /// The print state's revision the last drawn frame showed: `fab.slice` says whether the frame
    /// on screen shows its state (the ui tree is that frame's).
    #[rust]
    drawn_print: u64,
    /// A restart in place gave the view back: the first plate keeps it (no framing).
    #[rust]
    keep_view: bool,
    /// The colour pictures asked for (`fab.picture` calls): each new one is framed from below.
    #[rust]
    picture_calls: Vec<u64>,
    /// The strokes the worker is resolving: its call and the mark's id on the view.
    #[rust]
    mark_calls: Vec<(u64, u64)>,
    /// The mass read the Part panel's Mass and balance row and the view's marker wait for.
    #[rust]
    mass_call: Option<u64>,
    /// The AI's mass reads in flight in the person's turn now.
    #[rust]
    mass_ai_calls: Vec<u64>,
    /// The marks' text for the picture a line goes with: it rides in the chat target's context
    /// while the marks are on the view.
    #[rust]
    mark_text: Option<String>,
    /// The worker's reading of the marks for a picture: its call and, once in, the answer.
    #[rust]
    mark_read: Option<(u64, Option<Result<String, String>>)>,
    /// The picks as the worker keeps them ([`picks`]): the chat's chips and what they tell.
    #[rust]
    picks: Arc<Vec<fab_ai::picks::Pick>>,
    /// The hover call out (its id, the pointer then), and the newest ray waiting for its answer.
    #[rust]
    pick_hover: Option<(u64, fab::tools::picks::Hover)>,
    #[rust]
    pick_hover_next: Option<fab::tools::picks::Hover>,
    /// Select Other's call out: its id, where it opens, whether a row adds.
    #[rust]
    pick_other: Option<(u64, DVec2, bool)>,
    /// The click picks out: a refused one says why.
    #[rust]
    pick_calls: Vec<u64>,
    /// The view's size in points when its picture was asked for.
    #[rust]
    mark_view: [f64; 2],
    /// What `restart::set_unsaved` was last told (the code panel's text not committed yet).
    #[rust]
    unsaved_published: bool,
    /// Slicing and the print preview (`print.rs`).
    #[rust]
    printing: print::Printing,
    /// The Calibrate panel's steps and results (`calibrate.rs`).
    #[rust]
    calibrate: calibrate::Calibrate,
    /// The plate's full scene, and whether the bed-only one shows instead
    /// (the print preview with the model hidden).
    #[rust]
    model_scene: Option<Arc<fab::model::Scene>>,
    #[rust]
    bed_scene: Option<Arc<fab::model::Scene>>,
    #[rust]
    showing_bed: bool,
    /// The playback clock's frames.
    #[rust]
    print_frame: NextFrame,
    #[rust]
    print_frame_time: Option<f64>,
    #[rust]
    caption: Option<Caption>,
    /// Request ids of Fab's own edits.
    #[rust]
    #[rust]
    plate_revision: u64,
    /// Eyes toggled and not in the plate yet: (the edit's job, object, printed). They show at
    /// once; the model and the slice catch up behind them.
    #[rust]
    eyes: Vec<(u64, String, bool)>,
    /// A whole live revision's print time is estimated after 300 ms without a newer input.
    #[rust]
    estimate_timer: Timer,
    /// The committed plate's parameter views (a cancelled drag goes back to them).
    #[rust]
    committed_params: Vec<Arc<fab::params::PartParamsView>>,
    /// Previews of this generation or older are stale (a commit or a cancel came after them).
    #[rust]
    preview_floor: u64,
    /// A preview plate is showing instead of the committed one.
    #[rust]
    previewing: bool,
    /// Files from outside over the window.
    #[rust]
    file_drag: Option<FileDrag>,
    /// The commit of dropped object scripts: their preview shows until its plate does (refused,
    /// the plate goes back to the committed one).
    #[rust]
    drop_call: Option<u64>,
    /// The Test prints view.
    #[rust]
    test_view: TestView,
    /// The newest parameter commit's job: its answer (whatever it is: built, unchanged,
    /// refused, superseded) ends the part's "Rebuilding…".
    #[rust]
    param_job: Option<u64>,
    /// What the drag in progress has learned about how far its parameter goes.
    #[rust]
    drag_cap: Option<fab::params::DragCap>,
    /// Previews asked of the worker and not answered yet: generation -> (key, value).
    #[rust]
    preview_asks: std::collections::BTreeMap<u64, (String, fab::params::ParamValue)>,
    /// The newest parameter preview asked (its generation).
    #[rust]
    preview_latest: u64,
    /// The value (and key) of the newest build a parameter preview showed.
    #[rust]
    shown_ok: Option<(String, fab::params::ParamValue)>,
    /// The cap in force in this drag: its held value, failure, notice and outstanding preview.
    #[rust]
    held: Option<Held>,
    /// The newest parameter commit.
    #[rust]
    param_commit: Option<ParamCommit>,
    /// A draft showing whose statements lost their selector: the next pick names one again.
    #[rust]
    repick: Option<picks::RePick>,
    /// The job of a typed commit refused for a lost selector: its strip line goes with the draft.
    #[rust]
    draft_job: Option<makepad_widgets::jobs::JobId>,
    /// The gizmo's newest move commit: its part shows at the placement preview until the
    /// committed plate has it; a refused move takes the preview back.
    #[rust]
    place_job: Option<u64>,
    /// The colour or brush edit on the worker: its refusal appears in the plate view.
    paint_job: Option<u64>,
    /// A print settings box's setting taken out on the worker: its refusal appears in the plate view.
    #[rust]
    reset_job: Option<u64>,
    /// The released sculpt strokes on the worker, oldest first: each one's stroke shows live
    /// until its plate is in (or it is refused).
    #[rust]
    sculpt_jobs: std::collections::VecDeque<u64>,
    /// The live sculpt strokes' change count the slice follows (`tools::sculpt::revision`).
    #[rust]
    sculpt_seen: u64,
    /// The brush height the slice's focus was last moved to (`App::brush_z`).
    #[rust]
    brush_focus: Option<f64>,
    /// The Force tab's study edit the model worker is writing into a script.
    #[rust]
    study_job: Option<u64>,
    /// The Overrides editor's or a menu's print settings volume on the worker.
    volume_call: Option<u64>,
    /// The Strength editor's call under way: a find, or an Apply.
    #[rust]
    strength_call: Option<u64>,
    /// The slice the list was read from: another means it's stale.
    #[rust]
    strength_key: Option<(u64, u64)>,
    /// A weak spot's test print on its way.
    #[rust]
    strength_test: Option<u64>,
    /// The code panel's text not yet committed, and the rest that commits it.
    #[rust]
    source_text: Option<String>,
    #[rust]
    source_timer: Timer,
    /// The model host's last save before quitting is on its way ([`App::quit`]): its job id.
    #[rust]
    quitting: Option<u64>,
    /// Commits in flight, keyed by job and file so switching tabs cannot misroute an answer.
    #[rust]
    source_commits: std::collections::BTreeMap<u64, (String, Arc<String>)>,
    #[rust]
    source_selection: Vec<String>,
    /// A feature added from the Add menu: picked once the program has it.
    #[rust]
    select_after: Option<String>,
    /// A test print's drag on its way to the plate, and the job that takes it there.
    #[rust]
    test_place: Option<(u64, fab::api::TestDrag)>,
    /// A Lay flat click on a test print's piece on its way to the plate: why not, if it misses.
    #[rust]
    test_lay: Option<u64>,
    /// Start with the demo plate unless the worker recovered a document.
    #[rust]
    demo_pending: bool,
    /// The call of the load the view's progress card shows ([`model_host::Load`]): its answer
    /// ends it.
    #[rust]
    load_call: Option<u64>,
    /// The newest load's call the card has taken (a later one takes it over).
    #[rust]
    load_seen: u64,
    /// The job of the supports the view's progress card shows while a slice plans them.
    #[rust]
    /// The demo on the plate whose card has no picture yet, and whether its plate has arrived:
    /// the first settled frame after that is the card's picture.
    #[rust]
    demo_card: Option<(&'static str, u64, bool)>,
    /// The scripts' texts as last committed (file -> text).
    #[rust]
    scripts: Arc<std::collections::BTreeMap<String, String>>,
    /// The job Monitor last framed.
    #[rust]
    framed_job: Option<String>,
    /// Tool ghosts (their own thread) and the program they build from.
    #[rust]
    ghosts: Option<ghost::GhostWorker>,
    #[rust]
    program: model_host::Program,
    #[rust]
    ghost_gen: u64,
    /// The newest no-trees areas asked for: only they show.
    #[rust]
    areas_gen: u64,
    /// The number rows with a handle of the plate the newest areas are built from (its size and
    /// place as built: the drawn box follows its rows from there).
    #[rust]
    areas_rows: Vec<(String, fab::params::AreaRow)>,
    /// The parameter the newest ghost asked for shows (built or still building).
    #[rust]
    ghost_key: Option<String>,
    /// The selected cutter carried by the newest ghost request.
    #[rust]
    ghost_node: Option<String>,
    /// The tree edit (move, rename) asked for on the text shown (what, `to`, the text's
    /// identity): a button's click reaches the panel twice (its own capture and the window's
    /// actions), and an edit computed on the same text must not undo or repeat the first.
    #[rust]
    last_move: Option<(String, String, usize)>,
    /// The selection a ghost last showed under: changing it ends the ghost.
    #[rust]
    ghost_selection: Vec<String>,
    /// The committed plate's notice, and whether a preview replaced it.
    #[rust]
    committed_notice: String,
    #[rust]
    notice_is_preview: bool,
    /// The chord exact parts are shown at (mm).
    #[rust]
    exact_detail: f64,
    /// Live distance fields of a drag (their own thread).
    #[rust]
    live: Option<sdf_live::LiveWorker>,
    /// Grown patterns' live runs.
    #[rust]
    patterns: Option<pattern_live::PatternLive>,
    #[rust]
    live_gen: u64,
    /// The newest live request whose fields showed, or that a cancel or a commit made stale:
    /// an older answer never shows.
    #[rust]
    live_shown: u64,
}

/// `action` moves between workspaces: one shown by its kind (an app's own move, as a job coming
/// back), the person's pick (the caption's row, its keys, View › Workspace), or an edit of the
/// list (a new or duplicated one shows; a rename or a move is the person's act all the same).
fn shows_workspace(action: &ShellAction) -> bool {
    matches!(action, ShellAction::SetWorkspace(_) | ShellAction::SwitchWorkspace(_) | ShellAction::EditWorkspaces(_))
}

impl App {
    /// The plate mode `action` puts in force when it shows a workspace by kind or by id.
    fn mode_after(&self, action: &ShellAction) -> Option<PlateMode> {
        let ui = &self.state.ui;
        match action {
            ShellAction::SetWorkspace(w) => w.mode_from(ui.plate_mode),
            ShellAction::SwitchWorkspace(id) => ui.workspaces.get(*id).filter(|d| d.shows(Family::Plate)).map(|d| d.mode.unwrap_or(ui.plate_mode)),
            _ => None,
        }
    }

    /// Route one action through the loader, the core state and the tools.
    fn dispatch(&mut self, cx: &mut Cx, action: &ShellAction) -> bool {
        if shows_workspace(action) {
            self.startup_monitor_done = true;
        }
        if self.mode_after(action) == Some(PlateMode::Mechanics) && self.state.tool != Tool::Select {
            // Mechanics gives empty-space presses to navigation when the mode changes.
            self.dispatch(cx, &ShellAction::SetTool(Tool::Select));
        }
        match action {
            ShellAction::AddMeasurement(measurement) => {
                if let Some(host) = &mut self.host {
                    host.edit(vec![fab_model::Change::MeasureAdd { measure: model_host::measurement_to_model(measurement) }]);
                }
                return true;
            }
            ShellAction::SetMeasurement { index, measurement } => {
                if let Some(host) = &mut self.host {
                    host.edit(vec![fab_model::Change::MeasureSet {
                        index: *index, points: Some(model_host::measurement_to_model(measurement).points), name: None,
                    }]);
                }
                return true;
            }
            _ => {}
        }
        if let ShellAction::OpenDemo(key) = action {
            self.open_demo(key);
            self.show_demo_electronics(cx, key);
            return true;
        }
        if let ShellAction::CloseProgressCard = action {
            self.state.progress_card = None;
            self.load_call = None;
            fab::ui::redraw_now(cx, &self.ui, &[ids!(progress_card)]);
            return true;
        }
        if let ShellAction::OpenDialog = action {
            self.open_dialog(cx);
            return true;
        }
        if let ShellAction::ImportDialog = action {
            self.import_dialog(cx);
            return true;
        }
        if let ShellAction::ImportProfilesDialog = action {
            self.import_profiles_dialog(cx);
            return true;
        }
        if let ShellAction::ImageToPartDialog = action {
            self.image_to_part_dialog(cx);
            return true;
        }
        if let ShellAction::ColourPictureDialog = action {
            let dialog = FileDialog::new().set_title("Print a picture in colour".into()).set_id(live_id!(fab_colour_picture))
                .add_filter("Pictures".into(), img2mesh::PICTURE_EXTENSIONS.iter().map(|e| e.to_string()).collect());
            cx.open_select_file_dialog(dialog);
            return true;
        }
        if let ShellAction::ImportFile(path) = action {
            if fab_ai::import::is_mesh_file(path) {
                self.import_file(path.clone(), false);
                return true;
            }
            return self.dispatch(cx, &ShellAction::OpenFile(path.clone()));
        }
        match action {
            ShellAction::DragFiles(paths) => {
                self.drag_files(cx, paths);
                return true;
            }
            ShellAction::DropFiles(paths) => {
                self.drop_files(cx, paths);
                return true;
            }
            ShellAction::DragFilesEnd => {
                self.drag_files_end(cx);
                return true;
            }
            _ => {}
        }
        // A setting's override picked, or none (a click on a part or on nothing, Esc): the box
        // picked before is not, so one pick shows.
        if let ShellAction::PickOverride(id) = action {
            if !id.as_deref().is_some_and(fab::ui::overrides::is_box_id) {
                self.unpick_box(cx);
            }
        }
        if let Some(done) = self.plate_action(action) {
            return done;
        }
        if let ShellAction::ViewportFrame(frame) = action {
            // A frame of a plate that has changed since is not the project's picture.
            if frame.revision == self.state.plate.revision {
                let demo = self.demo_card.filter(|(_, _, armed)| *armed).map(|(key, _, _)| key);
                if demo.is_some() {
                    self.demo_card = None;
                }
                if let Some(host) = &mut self.host {
                    host.thumbnail(frame, demo);
                }
            }
            // the picture goes to the plate's file: nothing shown changed
            return false;
        }
        if let ShellAction::OpenFile(path) = action {
            let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
            // Printer, filament and process profiles (preset files and bundles) are imported.
            if PROFILE_EXTENSIONS.contains(&ext.as_str()) {
                self.import_profiles(path.clone());
                return true;
            }
            if fab_ai::import::is_mesh_file(path) {
                self.import_file(path.clone(), true);
                return true;
            }
            if fab_model::project::is_project(path) {
                self.open_project(path.clone());
                return true;
            }
            // A picture prints in full colour (Parts > Add > Colour picture…, or dropped); the AI
            // hub makes a 3D part of one from Parts > Add > From image….
            if img2mesh::is_picture(path) {
                self.colour_picture(path.clone());
                return true;
            }
            // An object script is placed on the plate, like a mesh file (it rebuilds from its
            // parameters).
            if fab_model::project::is_script(path) {
                if let Some(host) = &mut self.host {
                    host.add_scripts(vec![path.clone()]);
                }
                return true;
            }
        }
        let mut changed = false;
        changed |= loader::apply(cx, &mut self.loader, &mut self.state, action);
        let previous_workspace = self.state.ui.workspaces.current;
        let sources_before = self.state.model_sources.bits();
        let core_changed = self.state.apply_core(action);
        changed |= core_changed;
        if self.state.model_sources.bits() != sources_before {
            self.model_sources_changed(cx, sources_before);
        }
        if previous_workspace != self.state.ui.workspaces.current && !self.state.ui.shows(Family::Plate) {
            // The old dock still has the stage here. Once the new layout loads it gets no draw.
            let plate = self.ui.dock(cx, ids!(dock)).item(live_id!(tab_viewport));
            if let Some(mut stage) = plate.borrow_mut::<fab::ui::stage::FabStage>() {
                stage.park(cx, &mut self.state);
            };
        }
        // The electronics host starts with its editors, whichever action put them on screen (a
        // workspace choice or a demo), or with Mechanics' closed box.
        if self.state.ui.shows(Family::Electronics) || closed_box::ClosedBox::showing(&self.state) {
            self.start_elec(cx);
        }
        changed |= tools::apply(cx, &mut self.state, action);
        match action {
            ShellAction::LayoutGroup(false) => self.layout_in_flight = false,
            ShellAction::EditWorkspaces(ListOp::UndoLayout(id)) if core_changed => {
                // A later edit can reach the same depth. Retire the offer as its snapshot leaves.
                let popped = self.state.ui.workspaces.history_len(*id) + 1;
                for layout in &mut self.turn_layouts {
                    if layout.changed.iter().any(|c| c.id == *id && c.before < popped && popped <= c.after) {
                        layout.changed.clear();
                    }
                }
            }
            ShellAction::Quit => self.quit(cx),
            ShellAction::Command(name) => {
                self.run_command(cx, name);
            }
            ShellAction::ShowBoard(on) => {
                let objects = self.state.plate.parts.iter().filter(|p| p.is_board()).map(|p| p.object.clone()).collect();
                self.part_action(cx, &PartAction::SetHidden { objects, hidden: !on });
            }
            _ => {}
        }
        changed
    }

    /// `fab.elec_read` / `fab.elec_edit` call `id`: its `action` with its `args` run through
    /// the electronics host as the AI (`ElecHost::run_action`), the path a person's command takes
    /// too. `help` answers an action's entry from the catalog. The answer comes through
    /// `elec_answers`; a call that cannot start is answered here. Cost: O(args).
    fn elec_call(&mut self, cx: &mut Cx, id: u64, tool: &str, args: &Value) {
        if tool == fab_ai::ELEC_TEXT_TOOL {
            return self.elec_text_call(cx, id, args);
        }
        let offered = fab_ai::elec_actions(tool);
        let names = || offered.iter().map(|def| def.name).collect::<Vec<_>>().join(", ");
        let refuse = |calls: &mut chat::Calls, error: String| {
            calls.finish(id, chat::Answer { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(&error)), images: Vec::new() });
        };
        let Some(action) = args.get("action").and_then(Value::as_str) else {
            return refuse(&mut self.calls, format!("fab.{tool} takes action: one of {}, or help", names()));
        };
        let fields: Vec<(String, String)> = match args.get("args") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Obj(fields)) => fields.iter().map(|(name, value)| (name.clone(), splash_arg(value))).collect(),
            Some(_) => return refuse(&mut self.calls, "args is an object of the action's fields by name".into()),
        };
        if action == "help" {
            let of = fields.iter().find(|(name, _)| name == "of").map(|(_, value)| value.as_str());
            let Some(def) = of.and_then(fab_elec_design::action::find) else {
                return refuse(&mut self.calls, format!("help takes of: an action's name; fab.{tool} has {}", names()));
            };
            let help = fab_ai::elec_action_help(def);
            let text = if def.name == fab_elec_mech::read::ACTIONS[0].name {
                format!("elec.board.read is fab.board's read: call fab.board.\n\n{help}")
            } else if let Some(other) = fab_ai::elec_tool_of(def.risk).filter(|&other| other != tool) {
                format!("{} is a fab.{other} action: call it through fab.{other}.\n\n{help}", def.name)
            } else {
                help
            };
            return self.calls.finish(id, chat::Answer { ok: true, text, images: Vec::new() });
        }
        if !offered.iter().any(|def| def.name == action) {
            let elsewhere = fab_elec_design::action::find(action).and_then(|def| fab_ai::elec_tool_of(def.risk));
            return refuse(&mut self.calls, match elsewhere {
                Some(other) => format!("{action} is offered by fab.{other}, not fab.{tool}"),
                None => format!("fab.{tool} has no action {action}; it has {}", names()),
            });
        }
        if self.status.docs.is_empty() {
            return refuse(&mut self.calls, "This plate has no electronics: it has no electronics documents.".into());
        }
        // What the person typed lands first, as before a bed switch.
        self.send_typing();
        self.start_elec(cx);
        if let Some(elec) = &mut self.elec {
            elec.run_action(cx, action, fields, fab_elec_design::By::Ai { turn: self.chat_turn, call: id }, Some(id));
        }
        self.elec_answers();
    }

    /// `fab.elec_text`: a change of an electronics document's text, as typing in its code pane
    /// does (`ElecHost::run_text`): `doc` and the `rev` it was read at, with `edits` of
    /// `{old new}` or the whole `text`.
    fn elec_text_call(&mut self, cx: &mut Cx, id: u64, args: &Value) {
        use fab_elec_host::doc_actions::TextChange;
        let refuse = |calls: &mut chat::Calls, error: &str| {
            calls.finish(id, chat::Answer { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(error)), images: Vec::new() });
        };
        let Some(doc) = args.get("doc").and_then(Value::as_str) else {
            return refuse(&mut self.calls, "doc: the document's name, as elec.doc.list gives it");
        };
        let rev = match args.get("rev") {
            Some(Value::Int(rev)) if *rev >= 0 => *rev as u64,
            Some(Value::F64(rev)) if *rev >= 0.0 && rev.fract() == 0.0 => *rev as u64,
            Some(Value::Str(rev)) if rev.trim().parse::<u64>().is_ok() => rev.trim().parse().unwrap_or_default(),
            _ => return refuse(&mut self.calls, "rev: the revision elec.doc.read gave, a whole number"),
        };
        let change = match (args.get("edits"), args.get("text")) {
            (Some(_), Some(_)) => return refuse(&mut self.calls, "give edits or text, not both"),
            (None, Some(Value::Str(text))) => TextChange::Whole(text.clone()),
            (None, Some(_)) => return refuse(&mut self.calls, "text: the document's whole new text, a string"),
            (Some(Value::Arr(edits)), None) => {
                let mut pairs = Vec::with_capacity(edits.len());
                for (at, edit) in edits.iter().enumerate() {
                    let (Some(old), Some(new)) = (edit.get("old").and_then(Value::as_str), edit.get("new").and_then(Value::as_str)) else {
                        return refuse(&mut self.calls, &format!("edit {}: an edit is {{old new}}, both strings", at + 1));
                    };
                    pairs.push((old.to_string(), new.to_string()));
                }
                TextChange::Replace(pairs)
            }
            (Some(_), None) => return refuse(&mut self.calls, "edits: an array of {old new}"),
            (None, None) => return refuse(&mut self.calls, "give edits: [{old new}], or the whole text"),
        };
        if self.status.docs.is_empty() {
            return refuse(&mut self.calls, "This plate has no electronics: it has no electronics documents.");
        }
        // What the person typed lands first: the call's revision is checked against it.
        self.send_typing();
        self.start_elec(cx);
        if let Some(elec) = &mut self.elec {
            let ask = crate::elec_host::TextAsk { doc: doc.to_string(), rev, change };
            elec.run_text(cx, ask, fab_elec_design::By::Ai { turn: self.chat_turn, call: id }, Some(id));
        }
        self.elec_answers();
    }

    /// The electronics host's finished calls, to the chat.
    fn elec_answers(&mut self) {
        let Some(elec) = &mut self.elec else { return };
        for (id, ok, text) in elec.take_answers() {
            self.calls.finish(id, chat::Answer { ok, text, images: Vec::new() });
        }
    }

    /// Start the electronics host if it is not running.
    fn start_elec(&mut self, cx: &mut Cx) {
        if self.elec.is_none() {
            let steps = self.host.as_ref().map_or_else(|| Arc::new(AtomicU64::new(0)), |host| host.steps());
            let logged = self.host.as_ref().map_or_else(|| Arc::new(AtomicU64::new(0)), |host| host.logged());
            let mut elec = elec_host::ElecHost::start(cx, self.status.switched, bed_plate(&self.status), self.status.docs.clone(), self.status.elec_assets.clone(), steps, logged);
            elec.set_memory(self.status.switched, self.status.elec_memory.clone());
            self.elec = Some(elec);
        }
    }

    /// Once per committed design: the models it names but neither the plate nor the part library
    /// holds go to the serial queue, and its board masses are worked out on the pool. Between
    /// changes the UI compares a pointer and drains the queue's worker and the masses without
    /// waiting. Library I/O stays on the pool.
    fn elec_models(&mut self, cx: &mut Cx, event: &Event) {
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        if let Some(elec) = &self.elec {
            let design = elec.design();
            if let Some(design) = design.filter(|design| {
                self.models_from.as_ref().is_none_or(|old| !std::ptr::eq(old.as_ptr(), Arc::as_ptr(design)))
            }) {
                self.models_from = Some(Arc::downgrade(design));
                let (tx, rx) = to_ui_oneshot();
                let snapshot = Arc::clone(design);
                self.board_masses_rx = match cx.task_pool().submit_named(Class::Background, "board masses", move || {
                    let masses = snapshot.units.iter().filter_map(|unit| {
                        let board = unit.board.as_deref()?;
                        let mech = fab_elec_mech::board_mech_with(board, &unit.circuit, &board.drills());
                        Some((unit.name.clone(), fab_elec_mech::board_mass(&mech)))
                    }).collect();
                    let _ = tx.send(masses);
                }) {
                    Ok(task) => { task.detach(); Some(rx) }
                    Err(e) => { log!("fab mech: could not calculate board masses: {e}"); None }
                };
                let missing = design.missing_meshes();
                {
                    use fab::model_sources::ModelSource;
                    if part_models::source_on(ModelSource::KicadLocal) || part_models::source_on(ModelSource::KicadDownload) {
                        let folder = self.state.model_sources.kicad_dir.clone().map(std::path::PathBuf::from);
                        let kicad = self.kicad_models.get_or_insert_with(|| kicad_models::KicadModels::new(folder));
                        kicad.want(&cx.task_pool(), &missing);
                    }
                }
                if part_models::source_on(fab::model_sources::ModelSource::EasyEda) {
                    let (models, _) = part_models::fetchable(&fab_elec_import::easyeda::Lcsc, &missing);
                    if !models.is_empty() {
                        let fetch = self.part_models.get_or_insert_with(part_models::PartModels::new);
                        fetch.want(&cx.task_pool(), models);
                    }
                } else if let Some(note) = part_models::easyeda_off_note(&missing) {
                    // Nothing goes to EasyEDA while it is off; say once per design what that leaves.
                    log!("part models: {note}");
                    self.state.ui.notices.say(NoticeView::Device, note);
                }
            } else if design.is_none() && self.models_from.take().is_some() {
                self.board_masses_rx = None;
                self.mech.set_board_masses(cx, Arc::default(), &mut self.state);
            }
        }
        if let Some(rx) = &self.board_masses_rx {
            match rx.try_recv() {
                Ok(map) => {
                    self.board_masses_rx = None;
                    self.mech.set_board_masses(cx, Arc::new(map), &mut self.state);
                }
                Err(TryRecvError::Disconnected) => {
                    self.board_masses_rx = None;
                    log!("fab mech: board mass calculation stopped");
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        // A KiCad library host's pause: one timer to its end (at least i32::MAX ms fits a platform
        // timer; a longer wait wakes, finds the pause still on and sets it again).
        let pause_timer = |cx: &mut Cx, timer: &mut Timer, until: u64| {
            let delay = Duration::from_secs(until).saturating_sub(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default());
            cx.stop_timer(std::mem::take(timer));
            *timer = cx.start_timeout(delay.min(Duration::from_millis(i32::MAX as u64)).as_secs_f64().max(1.0));
        };
        if self.kicad_timer.is_event(event).is_some() {
            self.kicad_timer = Timer::empty();
            if let Some(kicad) = &mut self.kicad_models {
                kicad.resume(&cx.task_pool());
                if let Some(until) = kicad.resume_at() {
                    pause_timer(cx, &mut self.kicad_timer, until);
                }
            }
        }
        // A part card or an action changed this computer's choice for a KiCad model: keep it, and
        // ask for each changed model again by its new choice. One atomic load per event.
        let changed = fab_elec_parts::model_board::choices_changed();
        if changed != self.model_choices.0 {
            let now: std::collections::BTreeMap<_, _> = fab_elec_parts::model_board::choices().into_iter().collect();
            let old = std::mem::replace(&mut self.model_choices, (changed, now.clone())).1;
            kicad_models::save_choices(&now);
            let paths: Vec<String> = old.keys().chain(now.keys()).filter(|path| old.get(*path) != now.get(*path)).cloned().collect();
            let folder = self.state.model_sources.kicad_dir.clone().map(std::path::PathBuf::from);
            let kicad = self.kicad_models.get_or_insert_with(|| kicad_models::KicadModels::new(folder));
            for path in &paths {
                log!("part models: {path}: this computer now gets it {}", match fab_elec_parts::model_board::choice(path) {
                    fab_elec_parts::model_board::Choice::Auto => "from KiCad's library here, else its download",
                    fab_elec_parts::model_board::Choice::Local => "from KiCad's library on this computer only",
                    fab_elec_parts::model_board::Choice::Download => "from KiCad's library download only",
                });
                kicad.again(&cx.task_pool(), path);
            }
        }
        if let Some(kicad) = self.kicad_models.as_mut().filter(|k| k.running()) {
            if let Some(done) = kicad.poll(&cx.task_pool()) {
                if let Some(until) = kicad.resume_at() {
                    pause_timer(cx, &mut self.kicad_timer, until);
                }
                if done.cancelled {
                    log!("part models: KiCad 3D models: the run was cancelled");
                }
                for failure in &done.failed {
                    log!("part models: {failure}");
                }
                if let Some(line) = kicad_models::origins_line(&done.models) {
                    log!("part models: {line}");
                    use fab::model_sources::ModelSource;
                    let local = done.models.iter().any(|m| matches!(m.origin, fab_elec_import::kicad3d::Origin::Local { .. }));
                    let downloaded = done.models.iter().any(|m| matches!(m.origin, fab_elec_import::kicad3d::Origin::Download { .. }));
                    let now = makepad_civil_time::now_secs();
                    if local { self.state.model_sources.set_last_used(ModelSource::KicadLocal, now); }
                    if downloaded { self.state.model_sources.set_last_used(ModelSource::KicadDownload, now); }
                    fab::model_sources::save(&self.state.model_sources);
                }
                if let Some(elec) = &mut self.elec {
                    for path in &done.drop {
                        elec.drop_library_models(path);
                    }
                    elec.library_models(done.models.into_iter().map(|model| (model.path, model.key)).collect());
                }
                // A failure changes no design: the part cards read the model board's new notes.
                cx.redraw_all();
            }
        }
        let Some(fetch) = &mut self.part_models else { return };
        let woke = self.models_timer.is_event(event).is_some();
        if !woke && !fetch.running() { return; }
        let pool = cx.task_pool();
        if woke {
            self.models_timer = Timer::empty();
            fetch.resume(&pool);
        }
        if let Some(done) = fetch.poll(&pool) {
            if done.cancelled {
                log!("part models: {}", done.line);
            } else {
                for failure in &done.failed {
                    log!("part models: {failure}");
                }
            }
            if !done.models.is_empty() {
                self.state.model_sources.set_last_used(fab::model_sources::ModelSource::EasyEda, makepad_civil_time::now_secs());
                fab::model_sources::save(&self.state.model_sources);
                fab_elec_parts::model_board::note_all(done.models.iter()
                    .map(|model| (part_models::easyeda_note(&model.path), Some(model.key.clone()))).collect());
            }
            if let Some(elec) = &mut self.elec {
                elec.library_models(done.models.into_iter().map(|model| (model.path, model.key)).collect());
            }
        } else if !woke {
            return;
        }
        cx.stop_timer(std::mem::take(&mut self.models_timer));
        if let Some(until) = fetch.resume_at() {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
            let delay = Duration::from_secs(until).saturating_sub(now);
            // A platform timer can represent at least i32::MAX milliseconds. For a longer
            // Retry-After the next wake checks the original deadline again.
            self.models_timer = cx.start_timeout(delay.min(Duration::from_millis(i32::MAX as u64)).as_secs_f64());
        }
    }

    /// The person changed the 3D model sources: the workers read the new choice, a source turned
    /// off sends nothing more and its models leave the open design (its parts keep their own
    /// bodies), and the open design asks again for what it lacks from the sources now on. Cost:
    /// one re-evaluation of the open design.
    fn model_sources_changed(&mut self, cx: &mut Cx, before: u8) {
        use fab::model_sources::ModelSource;
        let bits = self.state.model_sources.bits();
        part_models::set_sources(bits);
        let easyeda = ModelSource::EasyEda.bit();
        if before & easyeda != 0 && bits & easyeda == 0 {
            // Dropping the queue cancels its run: no further request reaches the service.
            self.part_models = None;
            cx.stop_timer(std::mem::take(&mut self.models_timer));
            if let Some(elec) = &mut self.elec {
                elec.drop_library_models("parts/lcsc/");
            }
        }
        let kicad = ModelSource::KicadLocal.bit() | ModelSource::KicadDownload.bit();
        if before & kicad != bits & kicad {
            // The KiCad models leave the design and are asked for again from the sources now on.
            self.kicad_models = None;
            cx.stop_timer(std::mem::take(&mut self.kicad_timer));
            if let Some(elec) = &mut self.elec {
                elec.drop_library_models(fab_elec_import::kicad3d::PROJECT_PREFIX);
            }
        }
        let on_off = |bit: u8| if bits & bit != 0 { "on" } else { "off" };
        log!("part models: 3D model sources: KiCad install {}, KiCad download {}, EasyEDA {}",
            on_off(ModelSource::KicadLocal.bit()), on_off(ModelSource::KicadDownload.bit()), on_off(easyeda));
        self.models_from = None;
    }

    /// The F3 palette / menus speak in command names.
    fn run_command(&mut self, cx: &mut Cx, name: &str) {
        enum CommandResult {
            Handled,
            Action(ShellAction),
            Unknown,
        }
        use CommandResult::{Action, Handled, Unknown};

        let n = name.trim().to_lowercase();
        let action = match n.as_str() {
            "frame all" | "view.frame_all" => Action(ShellAction::FrameAll),
            "frame selected" | "view.frame_selected" => Action(ShellAction::FrameSelected),
            "front" | "front view" => Action(ShellAction::PresetView(PresetView::Front)),
            "top" | "top view" => Action(ShellAction::PresetView(PresetView::Top)),
            "right" | "right view" => Action(ShellAction::PresetView(PresetView::Right)),
            "iso" | "isometric" => Action(ShellAction::PresetView(PresetView::Isometric)),
            "ortho" | "perspective" | "toggle ortho" => Action(ShellAction::ToggleOrtho),
            "hide selected" => Action(ShellAction::HideSelected),
            "unhide all" => Action(ShellAction::UnhideAll),
            "isolate" | "isolate selected" => Action(ShellAction::IsolateSelected),
            "measure distance" => Action(ShellAction::SetTool(Tool::Measure(MeasureKind::Distance))),
            "measure area" => Action(ShellAction::SetTool(Tool::Measure(MeasureKind::Area))),
            "measure angle" => Action(ShellAction::SetTool(Tool::Measure(MeasureKind::Angle))),
            "measure edge" => Action(ShellAction::SetTool(Tool::Measure(MeasureKind::EdgeLength))),
            "measure radius" => Action(ShellAction::SetTool(Tool::Measure(MeasureKind::Radius))),
            "measure faces" => Action(ShellAction::SetTool(Tool::Measure(MeasureKind::FaceDistance))),
            "open" => Action(ShellAction::OpenDialog),
            "import" => Action(ShellAction::ImportDialog),
            "plates" | "browse plates" => Action(ShellAction::ShowPlates),
            "reload plate" => {
                if let Some(host) = &mut self.host {
                    self.framed = false;
                    host.reload();
                }
                Handled
            }
            "open demo" | "demo" => Action(ShellAction::OpenDemo("demo".into())),
            "solid" => Action(ShellAction::SetShading(Shading::Solid)),
            "realtime" => Action(ShellAction::SetShading(Shading::Realtime)),
            "undo" => {
                self.history_step(cx, true);
                Handled
            }
            "undo ai turn" => {
                self.undo_turn(cx);
                Handled
            }
            "redo" => {
                self.history_step(cx, false);
                Handled
            }
            "export stl" | "stl" => {
                self.export("stl");
                Handled
            }
            "export 3mf" | "3mf" | "export" => {
                self.export("3mf");
                Handled
            }
            "save plate" => {
                self.model_command("save", &[], false);
                Handled
            }
            "new plate" | "new" => {
                self.new_plate();
                Handled
            }
            "chat" | "toggle chat" | "ai" => {
                shell_chat::toggle(self, cx);
                Handled
            }
            "slice" | "slice plate" => {
                cx.action(ShellAction::SetRenderMode(fab::api::RenderMode::Slice));
                cx.action(fab::print::PrintAction::Slice);
                Handled
            }
            "printed surface" => {
                cx.action(fab::print::PrintAction::PrintedSurface(!self.state.print.finish));
                Handled
            }
            "show supports" => {
                cx.action(fab::print::PrintAction::ShowSupports(!self.state.print.play.supports));
                Handled
            }
            "show travel" => {
                cx.action(fab::print::PrintAction::ShowTravel(!self.state.print.play.travel));
                Handled
            }
            "show parts" => {
                cx.action(fab::print::PrintAction::ShowModel(!self.state.print.play.show_model));
                Handled
            }
            "cut at layer" => {
                cx.action(fab::print::PrintAction::CutAtLayer(!self.state.print.play.cut));
                Handled
            }
            "toggle code" => {
                cx.action(ShellAction::ShowCode(!self.state.ui.code_shown()));
                Handled
            }
            "insert selector" => {
                self.insert_selectors(cx);
                Handled
            }
            "drop to plate" => {
                for object in self.selected_objects() {
                    cx.action(PartAction::Drop { object });
                }
                Handled
            }
            "duplicate linked" | "duplicate as copy" => {
                if !self.state.ui.in_mode(PlateMode::Mechanics) && !self.state.slice_blocks_edits() {
                    fab::ui::parts::duplicate_selected(cx, &self.state, n == "duplicate as copy");
                }
                Handled
            }
            "arrange plate" => {
                cx.action(PartAction::Arrange);
                Handled
            }
            "slice view" => {
                cx.action(ShellAction::SetRenderMode(self.slice_toggled()));
                Handled
            }
            "toggle zones" => {
                cx.action(StageAction::Zones(!self.state.ui.show_zones));
                Handled
            }
            "toggle overhangs" => {
                cx.action(fab::print::PrintAction::Overhangs(!self.state.print.overhangs));
                Handled
            }
            "export print" | "export gcode" => {
                cx.action(fab::print::PrintAction::Export);
                Handled
            }
            "keymap" | "help" => Action(ShellAction::ShowKeymapHelp(true)),
            "about" => Action(ShellAction::ShowAbout(true)),
            "3d model sources" | "model sources" | "elec.model_sources" => Action(ShellAction::ShowModelSources(Some(fab::api::SourcesPage::Settings))),
            "licences" | "licenses" | "external resources and licences" => Action(ShellAction::ShowModelSources(Some(fab::api::SourcesPage::Licences))),
            "quit" => Action(ShellAction::Quit),
            _ => Unknown,
        };
        match action {
            Action(a) => cx.action(a),
            Handled => {}
            Unknown => {
                self.state.ui.notices.say(NoticeView::Plate, format!("Unknown command: {name}"));
            }
        }
    }

    /// Global keys, before the widget tree sees the event. Anything with a
    /// text field focused is left alone except the function keys.
    fn global_key(&mut self, cx: &mut Cx, ke: &KeyEvent) -> bool {
        let m = ke.modifiers;

        if !ke.is_repeat {
            let action = match ke.key_code {
                KeyCode::F1 => Some(ShellAction::ShowKeymapHelp(!self.state.ui.keymap_help_open)),
                KeyCode::F3 if m.shift => Some(ShellAction::TogglePerf),
                KeyCode::F3 => Some(ShellAction::ToggleCommandPalette),
                KeyCode::KeyS if m.is_primary() => {
                    self.model_command("save", &[], false);
                    return true;
                }
                // In marker mode Cmd/Ctrl+Z takes back the last stroke and never the model's history.
                KeyCode::KeyZ if m.is_primary() && !m.shift && self.state.ui.marker && !cx.key_focus_takes_text() && fab::tools::annotate::count() > 0 => {
                    self.pop_mark(cx);
                    return true;
                }
                KeyCode::KeyZ if m.is_primary() && !cx.key_focus_takes_text() => {
                    self.history_step(cx, !m.shift);
                    return true;
                }
                KeyCode::KeyE if m.is_primary() => {
                    self.export(if m.shift { "stl" } else { "3mf" });
                    return true;
                }
                KeyCode::KeyR if m.is_primary() => {
                    cx.action(ShellAction::SetRenderMode(fab::api::RenderMode::Slice));
                    cx.action(fab::print::PrintAction::Slice);
                    return true;
                }
                KeyCode::KeyK if m.is_primary() => {
                    shell_chat::toggle(self, cx);
                    return true;
                }
                KeyCode::KeyN if m.is_primary() => {
                    self.new_plate();
                    return true;
                }
                KeyCode::KeyO if m.is_primary() && m.shift => Some(ShellAction::OpenDemo("demo".into())),
                KeyCode::KeyO if m.is_primary() => Some(ShellAction::OpenDialog),
                KeyCode::KeyI if m.is_primary() => Some(ShellAction::ImportDialog),
                KeyCode::KeyM if m.is_primary() && m.shift => Some(ShellAction::ShowPlates),
                KeyCode::KeyQ if m.is_primary() => Some(ShellAction::Quit),
                _ => None,
            };
            if let Some(a) = action {
                cx.action(a);
                return true;
            }
        }

        // Viewport-context keys that must work without viewport focus — but
        // never while the key focus takes typed text (a text field, the code editor):
        // these are bare letters and would swallow typing.
        let modal_open = self.state.ui.keymap_help_open
            || self.state.ui.about_open
            || self.state.ui.command_palette_open;
        if !modal_open && !cx.key_focus_takes_text()
            && !ke.is_repeat && ke.key_code == KeyCode::KeyD && m.is_primary() && !m.alt
            && !self.state.ui.in_mode(PlateMode::Mechanics) && !self.state.slice_blocks_edits()
            && self.parts_have_focus(cx)
        {
            fab::ui::parts::duplicate_selected(cx, &self.state, m.shift);
            return true;
        }
        if modal_open || cx.key_focus_takes_text() {
            return false;
        }

        if ke.is_repeat {
            return false;
        }
        // The part tools and edits, bare keys over the plate.
        let plain = !m.control && !m.alt && !m.shift && !m.logo;
        // Cmd/Ctrl+1..9: the caption's first nine workspaces; Ctrl+PageUp/PageDown step along it.
        let digits = [KeyCode::Key1, KeyCode::Key2, KeyCode::Key3, KeyCode::Key4, KeyCode::Key5, KeyCode::Key6, KeyCode::Key7, KeyCode::Key8, KeyCode::Key9];
        let workspace = match digits.iter().position(|k| *k == ke.key_code) {
            Some(i) if m.is_primary() && !m.shift && !m.alt => Some(self.state.ui.offered_ids().get(i).copied()),
            _ if m.control && !m.shift && !m.alt && !m.logo => match ke.key_code {
                KeyCode::PageUp => Some(self.state.ui.step_workspace(-1)),
                KeyCode::PageDown => Some(self.state.ui.step_workspace(1)),
                _ => None,
            },
            _ => None,
        };
        if let Some(id) = workspace {
            if let Some(id) = id {
                cx.action(ShellAction::SwitchWorkspace(id));
            }
            return true;
        }
        if ke.key_code == KeyCode::Backtick && plain && self.state.ui.kind() == Workspace::Design {
            cx.action(ShellAction::ShowCode(!self.state.ui.code_shown()));
            return true;
        }
        if self.state.ui.in_mode(PlateMode::Mechanics) && plain {
            let action = match ke.key_code {
                KeyCode::KeyG => Some(fab::MechAction::Tool(fab::MechTool::Grab)),
                KeyCode::KeyT => Some(fab::MechAction::Tool(fab::MechTool::Throw)),
                KeyCode::KeyP => Some(fab::MechAction::Tool(fab::MechTool::Pin)),
                KeyCode::KeyD => Some(fab::MechAction::Tool(fab::MechTool::Dangle)),
                KeyCode::KeyK => Some(fab::MechAction::Tool(fab::MechTool::Poke)),
                KeyCode::Space => Some(if self.state.mech.running { fab::MechAction::Pause } else { fab::MechAction::Play }),
                KeyCode::KeyR => Some(fab::MechAction::Reset),
                _ => None,
            };
            if let Some(action) = action {
                cx.action(action);
                return true;
            }
        }
        // The plate's bare keys. Mechanics has its own above; Electronics parks the plate, and its
        // sheet and board take their tool keys themselves (W, L, Q, B, P, A, V ...).
        let model_keys = !self.state.ui.in_mode(PlateMode::Mechanics) && !self.state.ui.shows(Family::Electronics);
        // The sculpt strip's brushes override plate keys only while Sculpt is the tool.
        if plain && model_keys && tools::sculpt::active(&self.state) {
            use fab_paint_field::StrokeOp;
            let st = &self.state.ui;
            let action = match ke.key_code {
                KeyCode::KeyR => Some(StageAction::SculptOp(StrokeOp::Raise)),
                KeyCode::KeyL => Some(StageAction::SculptOp(StrokeOp::Lower)),
                KeyCode::KeyC => Some(StageAction::SculptOp(StrokeOp::Crease)),
                KeyCode::LBracket => Some(StageAction::SculptRadius(st.sculpt_radius / 1.25)),
                KeyCode::RBracket => Some(StageAction::SculptRadius(st.sculpt_radius * 1.25)),
                _ => None,
            };
            if let Some(action) = action {
                cx.action(action);
                return true;
            }
        }
        if ke.key_code == KeyCode::KeyL && plain && model_keys {
            cx.action(ShellAction::SetRenderMode(self.slice_toggled()));
            return true;
        }
        // The Force strip's kinds, and Delete for the load the feature tree picked, while Force
        // is the tool.
        if plain && model_keys && self.state.force_tool() {
            let kind = match ke.key_code {
                KeyCode::KeyX => Some(fab::api::ForceKind::Fixed),
                KeyCode::KeyF => Some(fab::api::ForceKind::Force),
                KeyCode::KeyP => Some(fab::api::ForceKind::Pressure),
                KeyCode::KeyT => Some(fab::api::ForceKind::Torque),
                _ => None,
            };
            if let Some(kind) = kind {
                cx.action(StageAction::ForceKind(kind));
                return true;
            }
            if matches!(ke.key_code, KeyCode::Delete | KeyCode::Backspace) && self.state.force.focus.is_some() && self.parts_have_focus(cx) {
                fab::ui::stage::remove_focused_load(cx, &self.state);
                return true;
            }
        }
        // The paint strip's tools, while Paint is the tool.
        if plain && model_keys && tools::paint::active(&self.state) {
            let st = &self.state.ui;
            let mut acts: Vec<StageAction> = Vec::new();
            match ke.key_code {
                KeyCode::KeyB => { acts.push(StageAction::PaintErase(false)); acts.push(StageAction::PaintMode(PaintMode::Brush)); }
                KeyCode::KeyX => acts.push(StageAction::PaintErase(!st.paint_erase)),
                KeyCode::KeyF => acts.push(StageAction::PaintMode(PaintMode::Feature)),
                KeyCode::KeyP => acts.push(StageAction::PaintMode(PaintMode::Part)),
                KeyCode::KeyN => acts.push(StageAction::PaintMode(PaintMode::Flood)),
                KeyCode::KeyV => acts.push(StageAction::PaintMode(PaintMode::Pattern)),
                KeyCode::LBracket => acts.push(StageAction::PaintRadius(st.paint_radius / 1.25)),
                KeyCode::RBracket => acts.push(StageAction::PaintRadius(st.paint_radius * 1.25)),
                _ => {}
            }
            if !acts.is_empty() {
                for a in acts { cx.action(a); }
                return true;
            }
        }
        if model_keys && !m.control && !m.logo {
            let mut handled = true;
            match ke.key_code {
                KeyCode::KeyH if m.alt && !m.shift => tools::isolate::unhide_all(cx),
                KeyCode::KeyH if m.shift && !m.alt => tools::isolate::isolate_selected(cx),
                KeyCode::KeyH if plain => tools::isolate::hide_selected(cx),
                KeyCode::Slash if plain => {
                    let hint = tools::isolate::toggle_solo(cx, &self.state);
                    tools::session::with(|s| s.hint = hint.into());
                }
                KeyCode::KeyM if plain => {
                    // Cycle the measure kinds; from any other tool, start
                    // with distance.
                    let next = match self.state.tool {
                        Tool::Measure(MeasureKind::Distance) => MeasureKind::Area,
                        Tool::Measure(MeasureKind::Area) => MeasureKind::Angle,
                        Tool::Measure(MeasureKind::Angle) => MeasureKind::EdgeLength,
                        Tool::Measure(MeasureKind::EdgeLength) => MeasureKind::Radius,
                        Tool::Measure(MeasureKind::Radius) => MeasureKind::FaceDistance,
                        Tool::Measure(MeasureKind::FaceDistance) => MeasureKind::Distance,
                        _ => MeasureKind::Distance,
                    };
                    tools::session::with(|s| s.measure.clear());
                    cx.action(ShellAction::SetTool(Tool::Measure(next)));
                }
                KeyCode::KeyC if m.alt && !m.shift => {
                    tools::session::with(|s| s.section_anim = None);
                    cx.action(ShellAction::SetSection(fab::api::SectionState::default()));
                }
                KeyCode::KeyC if m.shift && !m.alt => {
                    let b = tools::section::box_from_bounds(&self.state.scene.bounds, 0.12);
                    let target = tools::section::boxed(b);
                    let bounds = self.state.scene.bounds;
                    let (anim, first) = tools::section::animate_to(&self.state.scene_state.section, target, &bounds);
                    tools::session::with(|s| s.section_anim = Some(anim));
                    cx.action(ShellAction::SetSection(first));
                    cx.action(ShellAction::SetTool(Tool::Section));
                }
                KeyCode::KeyC if plain => cx.action(ShellAction::SetTool(Tool::Section)),
                KeyCode::KeyF if plain && self.state.tool == Tool::Section => {
                    let mut section = self.state.scene_state.section.clone();
                    for p in &mut section.planes {
                        *p = tools::section::flip(p);
                    }
                    cx.action(ShellAction::SetSection(section));
                }
                _ => handled = false,
            }
            if handled { return true; }
        }
        // F: the Force tool, and the Force render mode with it
        if ke.key_code == KeyCode::KeyF && plain && self.state.ui.in_mode(PlateMode::Design) {
            cx.action(ShellAction::SetTool(Tool::Select));
            cx.action(StageAction::Gizmo(GizmoMode::Force));
            if !self.state.force.active {
                cx.action(ShellAction::SetRenderMode(fab::api::RenderMode::Force));
            }
            return true;
        }
        let gizmo = match ke.key_code {
            KeyCode::KeyB if plain && model_keys => Some(GizmoMode::Paint),
            KeyCode::KeyK if plain && self.state.ui.in_mode(PlateMode::Design) => Some(GizmoMode::Sculpt),
            KeyCode::KeyQ if plain && model_keys => Some(GizmoMode::Off),
            KeyCode::KeyG if plain && model_keys => Some(GizmoMode::Move),
            KeyCode::KeyR if plain && model_keys => Some(GizmoMode::Rotate),
            KeyCode::KeyS if plain && model_keys => Some(GizmoMode::Scale),
            KeyCode::KeyE if plain && model_keys => Some(GizmoMode::Shape),
            _ => None,
        };
        if let Some(mode) = gizmo {
            cx.action(ShellAction::SetTool(Tool::Select));
            cx.action(StageAction::Gizmo(mode));
            return true;
        }
        // The branch picked in the Sliced view goes from its part's supports.
        if matches!(ke.key_code, KeyCode::Delete | KeyCode::Backspace) && plain && model_keys && self.parts_have_focus(cx) {
            if let (Some(_), Some((object, tips))) = (self.state.print.picked_branch(), self.state.print.picked_branch.clone()) {
                cx.action(fab::print::PrintAction::PickBranch(None));
                cx.action(PartAction::DeleteTree { object, tips });
                return true;
            }
            // So does the marked area picked there (or in the tree): its solid and statement.
            if let Some(object) = self.picked_area().filter(|_| self.state.render_mode() == fab::api::RenderMode::Slice) {
                cx.action(PartAction::DeleteArea { object });
                return true;
            }
        }
        if matches!(ke.key_code, KeyCode::Delete | KeyCode::Backspace) && plain && model_keys && self.parts_have_focus(cx) {
            if self.state.slice_blocks_edits() {
                self.state.ui.notices.say(NoticeView::Plate, "Switch to Design to delete parts from the slice view.".into());
                cx.redraw_all();
                return true;
            }
            if tools::delete_measurement(cx, &self.state, ke.key_code) {
                return true;
            }
            fab::ui::parts::delete_selected(cx, &self.state);
            return true;
        }
        if ke.key_code == KeyCode::Space && plain && model_keys && self.state.print.showing().is_some() {
            cx.action(fab::print::PrintAction::Play(!self.state.print.play.playing));
            return true;
        }
        // The Sliced view's supports, shown or hidden (its view bar's toggle).
        if ke.key_code == KeyCode::KeyU && plain && model_keys && self.state.print.play.active {
            cx.action(fab::print::PrintAction::ShowSupports(!self.state.print.play.supports));
            return true;
        }
        let action = match ke.key_code {
            KeyCode::KeyZ if model_keys && m.alt && !m.control && !m.logo && !m.shift => Some(ShellAction::ToggleXray),
            KeyCode::Home => Some(ShellAction::FrameAll),
            _ => None,
        };
        if let Some(a) = action {
            cx.action(a);
            return true;
        }
        false
    }

    /// What the command line asks for: `--open <file>` (or a bare path) opens a file, `--demo <key>`
    /// a bundled demo as the Demos list opens it ([`demo_part::DEMOS`]' keys; the demo wins over a
    /// file named too).
    fn open_from_args(&mut self, cx: &mut Cx) {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut path: Option<String> = None;
        let mut demo: Option<String> = None;
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if a == "--open" {
                path = args.get(i + 1).cloned();
                i += 1;
            } else if let Some(p) = a.strip_prefix("--open=") {
                path = Some(p.to_string());
            } else if a == "--demo" {
                demo = args.get(i + 1).cloned();
                i += 1;
            } else if let Some(key) = a.strip_prefix("--demo=") {
                demo = Some(key.to_string());
            } else if !a.starts_with("--") {
                path = Some(a.clone());
            }
            i += 1;
        }
        if let Some(key) = demo {
            if let Some(p) = &path {
                log!("fab: --demo {key} opens that demo; {p} is not opened");
            }
            cx.action(ShellAction::OpenDemo(key));
            return;
        }
        match path {
            Some(p) if p.ends_with(".fab.splash") || p.ends_with(".cad.splash") => self.open_project(std::path::PathBuf::from(p)),
            Some(p) if fab_ai::import::is_mesh_file(std::path::Path::new(&p)) || img2mesh::is_picture(std::path::Path::new(&p)) => {
                cx.action(ShellAction::OpenFile(std::path::PathBuf::from(p)));
            }
            // Other supported scene formats use the viewer.
            Some(p) => self.loader.open(cx, std::path::PathBuf::from(p)),
            // Nothing to open: start with the demo plate, unless a test or an
            // automation client drives this session (`--remote`): it starts empty.
            None if args.iter().any(|a| a == "--remote") => {}
            // (After the worker's first answer: a document recovered from the crash copy wins.)
            None => self.demo_pending = true,
        }
    }

    /// Retry on the caption's job strip: a model job queued again, a load or an import read again, a
    /// slice run again.
    fn retry_job(&mut self, cx: &mut Cx, job: makepad_widgets::jobs::JobId) -> bool {
        if self.host.as_mut().is_some_and(|h| h.retry(job)) || self.loader.retry(cx, job) || self.img2mesh.retry(job) {
            return true;
        }
        match self.printing.retry(job) {
            Some(print::Retry::Slice) => {
                self.print_action(cx, &fab::print::PrintAction::Slice);
            }
            None => return false,
        }
        true
    }

    /// The caption's next step for a finished job that waits for the user.
    fn next_step(&mut self, cx: &mut Cx, job: makepad_widgets::jobs::JobId) {
        if self.draft_step(cx, job) {
            return;
        }
        log!("fab: job {} has no next step here", job.0);
    }

    /// File > Open…: the system's open dialog with Fab's file kinds. Its answer comes back as
    /// a `FileDialogAction` (see `handle_actions`). Mesh files open a new plate.
    fn open_dialog(&mut self, cx: &mut Cx) {
        let kinds = |d: FileDialog, label: &str, ext: &[&str]| d.add_filter(label.into(), ext.iter().map(|e| e.to_string()).collect());
        let dialog = FileDialog::new().set_title("Open a model".into()).set_id(live_id!(fab_open));
        let dialog = kinds(dialog, "Fab plates and scripts", &["splash"]);
        let dialog = kinds(dialog, "Meshes and plates", &["stl", "obj", "glb", "gltf", "3mf"]);
        let dialog = kinds(dialog, "Pictures (made into a part by the AI hub)", img2mesh::PICTURE_EXTENSIONS);
        let dialog = kinds(dialog, "Printer profiles", PROFILE_EXTENSIONS);
        let dialog = match model_host::documents_dir() {
            dir if dir.is_dir() => dialog.set_location(dir),
            _ => dialog,
        };
        cx.open_select_file_dialog(dialog);
    }

    fn import_dialog(&mut self, cx: &mut Cx) {
        let dialog = FileDialog::new().set_title("Import meshes".into()).set_id(live_id!(fab_import))
            .add_filter("Meshes and plates".into(), fab_ai::import::EXTENSIONS.iter().map(|e| e.to_string()).collect());
        cx.open_select_file_dialog(dialog);
    }

    fn import_profiles_dialog(&mut self, cx: &mut Cx) {
        let dialog = FileDialog::new().set_title("Import printer profiles".into()).set_id(live_id!(fab_import_profiles))
            .add_filter("Printer profiles".into(), PROFILE_EXTENSIONS.iter().map(|e| e.to_string()).collect());
        cx.open_select_file_dialog(dialog);
    }

    fn import_profiles(&mut self, path: std::path::PathBuf) {
        self.printing.import_profiles(path, true);
    }

    fn image_to_part_dialog(&mut self, cx: &mut Cx) {
        let dialog = FileDialog::new().set_title("Make a part from an image".into()).set_id(live_id!(fab_image_to_part))
            .add_filter("Pictures".into(), img2mesh::PICTURE_EXTENSIONS.iter().map(|e| e.to_string()).collect());
        cx.open_select_file_dialog(dialog);
    }

    /// A picture printed in full colour: a tile of it on the plate (`fab.picture`), in the
    /// project's print colours.
    fn colour_picture(&mut self, path: std::path::PathBuf) {
        let Some(host) = &mut self.host else { return };
        self.picture_calls.push(host.call("picture", json::obj(vec![("image", json::s(&path.display().to_string()))])));
    }

    fn image_to_part(&mut self, path: std::path::PathBuf) {
        let request = fab_ai::img2mesh::Request { image: path.display().to_string(), size_mm: fab_ai::img2mesh::SIZE_DEFAULT_MM, detail: fab_ai::img2mesh::DETAIL_DEFAULT, name: None, seed: None, printable: true };
        self.img2mesh.start(None, request, img2mesh::Picture::File(path));
    }

    /// Files from outside came over the window: the chip names them and, for object scripts, the
    /// plate previews where their drop puts them (a dry run on the model host; until it is built
    /// the plate stays as it is).
    fn drag_files(&mut self, cx: &mut Cx, paths: &[std::path::PathBuf]) {
        let names = match paths {
            [one] => one.file_name().map_or_else(|| one.display().to_string(), |n| n.to_string_lossy().into_owned()),
            more => format!("{} files", more.len()),
        };
        let scripts: Vec<_> = paths.iter().filter(|p| fab_model::project::is_script(p)).cloned().collect();
        if self.file_drag.as_ref().is_some_and(|d| d.gen.is_some()) {
            self.end_drop_preview(cx);
        }
        let gen = match &mut self.host {
            Some(host) if !scripts.is_empty() => Some(host.preview_drop(scripts.clone())),
            _ => None,
        };
        let text = if scripts.is_empty() { names.clone() } else { format!("{names} · arranging…") };
        self.state.ui.drop_chip = Some(fab::api::DropChip { text, warn: false });
        self.file_drag = Some(FileDrag { names, gen, scripts });
    }

    /// A file drag shows the plate as it is with its files dropped: its preview is asked again
    /// once it can no longer show (a cancel overtook it, or a committed plate replaced the one it
    /// was built on), and asked at all once the model host is there for files that came before.
    /// One compare per event otherwise.
    fn renew_drop_preview(&mut self) {
        let Some(drag) = &mut self.file_drag else { return };
        if drag.scripts.is_empty() || drag.gen.is_some_and(|g| g > self.preview_floor) {
            return;
        }
        let Some(host) = &mut self.host else { return };
        drag.gen = Some(host.preview_drop(drag.scripts.clone()));
    }

    /// The drop: the object scripts go on the plate together, as one undo step, where their
    /// preview showed them (it stays until the committed plate replaces it); every other file
    /// as a dropped file always did.
    fn drop_files(&mut self, cx: &mut Cx, paths: &[std::path::PathBuf]) {
        let previewed = self.file_drag.take().is_some_and(|d| d.gen.is_some());
        self.state.ui.drop_chip = None;
        let (scripts, others): (Vec<_>, Vec<_>) = paths.iter().cloned().partition(|p| fab_model::project::is_script(p));
        if let (false, Some(host)) = (scripts.is_empty(), &mut self.host) {
            self.preview_floor = host.cancel_preview();
            self.drop_call = Some(host.add_scripts(scripts));
        } else if previewed {
            self.end_drop_preview(cx);
        }
        for path in others {
            self.dispatch(cx, &ShellAction::ImportFile(path));
        }
    }

    /// The files left the window, or the drag ended without a drop: the plate as it is.
    fn drag_files_end(&mut self, cx: &mut Cx) {
        self.state.ui.drop_chip = None;
        if self.file_drag.take().is_some_and(|d| d.gen.is_some()) {
            self.end_drop_preview(cx);
        }
    }

    fn end_drop_preview(&mut self, cx: &mut Cx) {
        if let Some(host) = &mut self.host {
            self.preview_floor = host.cancel_preview();
        }
        self.restore_committed(cx);
    }

    /// The plate as the files dragged over the window would leave it: shown with their parts as
    /// ghosts, the chip saying how many parts they add, or what would go wrong (no room for them,
    /// a script that does not build: the plate stays as it is).
    fn drop_previewed(&mut self, cx: &mut Cx, p: &model_host::PreviewDone) {
        let Some(names) = self.file_drag.as_ref().filter(|d| d.gen == Some(p.gen)).map(|d| d.names.clone()) else { return };
        let chip = match &p.result {
            Ok(update) => {
                let committed: std::collections::HashSet<String> = self.plate.iter().flat_map(|ev| ev.plate.placed_parts().map(|p| p.object.clone())).collect();
                let added = self.show_preview_plate(cx, update, &|part| !committed.contains(&part.object));
                let before = self.plate.as_ref().map(|ev| ev.warnings.clone()).unwrap_or_default();
                let new: Vec<&String> = update.0.warnings.iter().filter(|w| !before.contains(w)).collect();
                // a part with no room first: it would land off the plate
                match new.iter().find(|w| w.starts_with("no room")).or(new.first()) {
                    Some(w) => fab::api::DropChip { text: format!("{names} · {w}"), warn: true },
                    None => fab::api::DropChip { text: format!("{names} · {added} {}", if added == 1 { "part" } else { "parts" }), warn: false },
                }
            }
            Err(e) => {
                self.restore_committed(cx);
                fab::api::DropChip { text: format!("{names} · {}", shown_error(e)), warn: true }
            }
        };
        self.state.ui.drop_chip = Some(chip);
        self.ui.redraw(cx);
    }

    fn import_file(&mut self, path: std::path::PathBuf, new_plate: bool) {
        // Only opening a new plate imports and selects its presets.
        if new_plate && path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("3mf")) {
            self.printing.import_profiles(path.clone(), true);
        }
        if new_plate {
            self.send_typing();
            self.demo_card = None;
        }
        if let Some(host) = &mut self.host { host.import_file(path, new_plate); }
    }

    /// The plate list's actions: a plate put on the bed, copied, renamed, taken off the list or
    /// moved to the trash. None: not one of them.
    fn plate_action(&mut self, action: &ShellAction) -> Option<bool> {
        if matches!(action, ShellAction::OpenPlate(_) | ShellAction::RemovePlate(_) | ShellAction::DeletePlate(_)) {
            self.send_typing();
        }
        if matches!(action, ShellAction::OpenPlate(_)) {
            self.bed_asked();
        }
        let host = self.host.as_mut()?;
        match action {
            ShellAction::OpenPlate(id) => {
                self.demo_card = None;
                host.open_plate(id.clone());
            }
            ShellAction::DuplicatePlate(id) => {
                host.plate_call("duplicate", id, None);
            }
            ShellAction::RenamePlate(id, name) => {
                host.plate_call("rename", id, Some(name));
            }
            ShellAction::RemovePlate(id) => {
                host.plate_call("remove", id, None);
            }
            ShellAction::DeletePlate(id) => {
                host.delete_plate(id.clone());
            }
            _ => return None,
        }
        Some(true)
    }

    /// Something else is asked for the bed (a plate, a demo, a file, a new plate): the switch is
    /// timed from here, and the demo a start with nothing to open would show is not wanted (the
    /// plate the last run left may have given way to it).
    fn bed_asked(&mut self) {
        switch_watch::asked();
        self.demo_pending = false;
    }

    fn open_project(&mut self, path: std::path::PathBuf) {
        self.bed_asked();
        self.send_typing();
        self.demo_card = None;
        if let Some(host) = &mut self.host {
            host.open(path);
        }
    }

    /// The electronics host and the model host in step ([`elec_host::ElecHost::sync_model`]).
    fn elec_sync(&mut self) {
        if let (Some(elec), Some(host)) = (self.elec.as_mut(), self.host.as_mut()) {
            elec.sync_model(host);
        }
    }

    /// The model's and the electronics' history tops.
    fn history_tops(&self) -> (Tops, Tops) {
        let model = Tops { undo: self.status.undo, redo: self.status.redo };
        let elec = self.elec.as_ref().map_or_else(Tops::default, |e| Tops { undo: e.undo_step(), redo: e.redo_step() });
        (model, elec)
    }

    /// The AI's turn steps the electronics committed, and both histories' tops, followed for what
    /// each turn's steps allow: Edit > Undo the AI's Last Turn (the menu reads it) and the buttons
    /// under each answer in the chat. O(the turns) per event; the buttons are worked out again
    /// only when a history, a turn or the temporary layout changes. Pending document history
    /// presses settle first so the buttons reflect the whole turn.
    fn follow_turn(&mut self) {
        if let Some(elec) = self.elec.as_mut() {
            for (turn, step) in elec.take_turn_steps() {
                self.turn_undo.committed(turn, step);
            }
        }
        let (model, elec) = self.history_tops();
        self.turn_undo.follow(model.undo.max(elec.undo));
        let last = self.turn_undo.last().and_then(|turn| self.turn_undo.offer(turn, model, elec));
        self.state.ui.undo_turn = match last {
            Some(turn_undo::Offer::Undo(presses)) => Some(presses),
            _ => None,
        };
        let model_busy = self.host.as_ref().is_some_and(|h| h.building());
        let elec_busy = self.elec.as_ref().is_some_and(|e| e.busy());
        if self.undo_order.busy(model_busy, elec_busy) {
            return;
        }
        // the turns whose answers the chat no longer shows go; the last stays for the menu (its
        // first call reaches the chat after `chat_turn_call` returns)
        let (chat, last) = (&self.chat, self.turn_undo.last());
        self.turn_undo.retain(|turn| Some(turn) == last || chat.has_turn(turn));
        let workspaces = &self.state.ui.workspaces;
        let key = (model, elec, self.turn_undo.rev, workspaces.temp_gen(), workspaces.temp_id(), workspaces.history_rev());
        if self.turn_shown == Some(key) {
            return;
        }
        self.turn_shown = Some(key);
        self.turn_layouts.retain(|layout| chat.has_turn(layout.turn));
        let mut turns: HashMap<_, _> = self.turn_undo.turns().into_iter().map(|turn| (turn, None)).collect();
        for layout in &self.turn_layouts {
            turns.insert(layout.turn, Some(layout));
        }
        for (turn, layout) in turns {
            let buttons = turn_buttons(self.turn_undo.offer(turn, model, elec), layout, workspaces);
            self.chat.set_turn_actions(turn, buttons);
        }
    }

    /// Edit > Undo the AI's Last Turn: as many presses of Undo as the turn made steps, while they
    /// are the newest; else the plate view says why.
    fn undo_turn(&mut self, cx: &mut Cx) {
        self.follow_turn();
        match self.state.ui.undo_turn {
            Some(presses) => self.press_history(cx, true, presses),
            None => self.state.ui.notices.say(NoticeView::Plate, "The AI's last turn is not the newest change: undo it a step at a time".into()),
        }
    }

    /// `presses` presses of Undo (or Redo), in the shared order.
    fn press_history(&mut self, cx: &mut Cx, undo: bool, presses: usize) {
        for _ in 0..presses {
            self.undo_order.press(undo);
        }
        self.pump_history(cx);
    }

    /// One undo or redo press, queued in the shared order whichever pane has the focus.
    fn history_step(&mut self, cx: &mut Cx, undo: bool) {
        self.undo_order.press(undo);
        self.pump_history(cx);
    }

    /// Send one press at a time so the next uses the history tops the previous press left.
    fn pump_history(&mut self, cx: &mut Cx) {
        self.elec_sync();
        loop {
            let (model, elec) = self.history_tops();
            let model_busy = self.host.as_ref().is_some_and(|h| h.building());
            let elec_busy = self.elec.as_ref().is_some_and(|e| e.busy());
            match self.undo_order.next(model, elec, model_busy, elec_busy) {
                Some((undo, Target::Model)) => {
                    if self.undo_redo(undo) {
                        self.undo_order.sent(Target::Model);
                    }
                }
                Some((undo, Target::Elec)) => {
                    if let Some(elec) = &mut self.elec {
                        if undo { elec.undo(cx); } else { elec.redo(cx); }
                    }
                    self.elec_sync();
                    self.undo_order.sent(Target::Elec);
                }
                None => break,
            }
        }
    }

    /// A preview's plate in place of the committed one; `arriving` picks the parts of files
    /// dragged over the window (ghosts until dropped). Returns how many it picked.
    fn show_preview_plate(&mut self, cx: &mut Cx, update: &model_host::PlateUpdate, arriving: &dyn Fn(&fab::api::PlatePart) -> bool) -> usize {
        let (ev, scene, markers, params, program, measurements, test_prints) = update;
        tools::measure::install(&mut self.state, measurements.clone());
        self.state.test_prints = test_prints.clone();
        fab::ui::test_prints::show_rows(&mut self.state);
        self.previewing = true;
        self.state.params.parts = params.clone();
        self.plate_revision += 1;
        self.state.plate = plate_edit::plate_info(program.as_ref().or(self.program.as_ref()).map(|(c, _)| c.as_ref()), ev, self.plate_revision);
        self.state.plate.preview = true;
        let mut picked = 0;
        for part in &mut self.state.plate.parts {
            part.arriving = arriving(part);
            picked += part.arriving as usize;
        }
        self.show_eyes();
        // The parts keep their elements in the installed scene until the new one is in: a plate
        // without them reads as "nothing selected" and the editor follows that (view_source
        // cancels the preview that was just built).
        self.state.sync_plate_elements();
        self.state.print.model_markers = Some(markers.clone());
        if !self.wants_bed_only() {
            cx.action(ShellAction::SceneUpdated { scene: scene.clone(), frame: false });
        }
        picked
    }

    /// Undo or redo the document's committed history and cancel its parameter preview; false
    /// without a model host.
    /// Code-tab drafts, pending typing and in-flight source commits keep their state.
    fn undo_redo(&mut self, undo: bool) -> bool {
        self.forget_drag();
        self.state.params.dragging = None;
        live_slice::set_drag_live(false);
        let Some(host) = &mut self.host else { return false };
        host.grow_runs.cancel(self.live_gen);
        host.history(undo);
        self.preview_floor = host.cancel_preview();
        true
    }

    /// Export the plate's model (`stl` or `3mf`): the caption's job strip shows it (a part drawn
    /// from its distance field is meshed first).
    fn export(&mut self, format: &str) {
        self.model_command("export", &[("format", json::s(format))], false);
    }

    /// A model tool from Fab's own commands (menus, keys): the same tool
    /// path the chat and outside agents use. `head` adds the current head as
    /// `expected` (the UI always means "the model as shown").
    fn model_command(&mut self, tool: &str, fields: &[(&str, Value)], head: bool) {
        let Some(host) = &mut self.host else { return };
        let mut args: Vec<(&str, Value)> = fields.to_vec();
        if head {
            let Some(h) = self.status.head else { return };
            args.push(("expected", json::s(h.text())));
        }
        host.call(tool, json::obj(args));
    }

    /// A finished worker job: swap the plate in, answer the chat's call.
    /// `fab.force`'s start: the part asked for selected and Design's Force view shown, as the
    /// person would (the feature tree's click, the F key). Why it can't be.
    fn show_force(&mut self, cx: &mut Cx, args: &Value) -> Result<(), String> {
        if let Some(name) = args.get("object").and_then(Value::as_str) {
            let parts = &self.state.plate.parts;
            let index = parts.iter().position(|p| p.name == name).or_else(|| parts.iter().position(|p| p.object == name));
            let Some(index) = index else {
                let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
                return Err(format!("no plate part '{name}'; the plate has: {}", names.join(", ")));
            };
            let element = parts[index].element.ok_or_else(|| format!("{name} is not on the plate's scene yet: ask again once the plate is built"))?;
            self.dispatch(cx, &ShellAction::SelectOnly(element));
        }
        if !self.state.force_shown() {
            self.dispatch(cx, &ShellAction::SetRenderMode(fab::api::RenderMode::Force));
        }
        if !self.state.force_shown() {
            return Err(if self.state.print.monitor_holds() {
                "the Force view can't show while Monitor holds the plate for a print".into()
            } else {
                "the Force view shows in Design only: the plate is in another mode".into()
            });
        }
        cx.redraw_all();
        Ok(())
    }

    /// The `fab.force` calls whose study is up to date (or can't go on by itself) get the
    /// view's answer; the others wait for the next event. Called after the lab's event pass while
    /// calls wait: O(calls), and the answer's O(loads + rows) once per settling.
    fn answer_force_calls(&mut self, cx: &mut Cx) {
        let building = self.host.as_ref().is_some_and(|h| h.building());
        let settled = !building && self.force.settled(&self.state);
        let mut answer: Option<String> = None;
        let mut waiting = Vec::new();
        for (id, seen) in std::mem::take(&mut self.force_calls) {
            // a call the chat dropped (cancelled) is gone
            if !self.calls.pending.contains_key(&id) {
                continue;
            }
            if seen && settled {
                let text = answer.get_or_insert_with(|| fab_ai::out::text(&self.force.report(&self.state, self.plate.as_ref()))).clone();
                self.calls.finish(id, chat::Answer { ok: true, text, images: Vec::new() });
            } else {
                waiting.push((id, true));
            }
        }
        // a lab with nothing under way has no event of its own to come: ask for one
        if !waiting.is_empty() && settled {
            cx.new_next_frame();
        }
        self.force_calls = waiting;
    }

    /// Capture the bed's view, including the electronics editors whose tabs are hidden.
    fn plate_view(&self, cx: &mut Cx) -> PlateView {
        let mut view = self.state.plate_view();
        if let Some(mut shell) = self.ui.widget(cx, ids!(shell)).borrow_mut::<fab::ui::shell::FabShell>() {
            (view.sheet, view.board, view.scopes) = shell.elec_views(cx);
        }
        view
    }

    fn host_done(&mut self, cx: &mut Cx, done: &model_host::Done) {
        live_slice::load_over(done.id);
        self.picks_done(cx, done);
        self.mass_done(cx, done);
        if let Some((call, answer)) = &mut self.mark_read {
            if *call == done.id {
                *answer = Some(if done.ok { Ok(done.text.clone()) } else { Err(done.text.clone()) });
            }
        }
        if let Some(at) = self.mark_calls.iter().position(|c| c.0 == done.id) {
            let (_, mark) = self.mark_calls.remove(at);
            fab::tools::annotate::resolved(mark, marks::resolved(&self.state, done.ok, &done.text));
            fab::ui::redraw(cx, &[ids!(tool_overlay)]);
        }
        // A colour picture put on the plate is framed on its face (further down), not as a plate
        // opens.
        let picture = self.picture_calls.iter().position(|&c| c == done.id).map(|i| self.picture_calls.remove(i)).is_some();
        // The load the card shows is over: on the bed, or cancelled with the bed as it was. A
        // load that failed keeps its card, which says why, until it is closed.
        if self.load_call == Some(done.id) {
            let failed = self.state.progress_card.as_ref().and_then(|c| makepad_widgets::jobs::board().get(c.job)).is_some_and(|j| matches!(j.end, makepad_widgets::jobs::JobEnd::Failed { .. }));
            if !failed {
                self.state.progress_card = None;
                self.load_call = None;
            }
            fab::ui::redraw_now(cx, &self.ui, &[ids!(progress_card)]);
        }
        // Another document is on the bed: the one it replaced keeps how it was looked at, and one
        // looked at before shows so again (else it is framed).
        if done.status.switched != self.status.switched {
            if let Some(fetch) = &mut self.part_models {
                fetch.close_board();
            }
            switch_watch::shown();
            self.forget_marks();
            // The boards of the plate it replaced go with it: the new plate's come with its
            // electronics load, and a plate without electronics has none.
            self.state.elec_scene = None;
            if let Some(bed) = self.status.bed.clone() {
                let view = self.plate_view(cx);
                if !bed.1 {
                    if let Some(host) = &mut self.host {
                        host.save_view(bed.0.clone(), view.clone());
                    }
                }
                self.views.insert(bed, view);
            }
            self.restore = done.status.bed.as_ref().and_then(|b| self.views.get(b)).or(done.status.saved_view.as_ref()).cloned();
            fab_elec_view::instruments::logic_view::LogicScope::leaving_plate(cx);
            if let Some(mut shell) = self.ui.widget(cx, ids!(shell)).borrow_mut::<fab::ui::shell::FabShell>() {
                let (sheet, board, scopes) = self.restore.as_ref().map(|view| (view.sheet, view.board, view.scopes.clone())).unwrap_or_default();
                shell.restore_elec_views(cx, sheet, board, scopes);
            }
            self.framed = false;
        }
        // A model step the AI's call made counts in its turn.
        if let Some(turn) = self.turn_undo.turn_of(done.id) {
            if let Some(step) = done.status.undo.filter(|step| done.status.switched == self.status.switched && Some(*step) > self.status.undo) {
                self.turn_undo.committed(turn, step);
            }
        }
        self.status = done.status.clone();
        self.follow_turn();
        // The electronics engine edits the bed's documents: another bed, another load.
        if let Some(elec) = &mut self.elec {
            elec.reload(cx, self.status.switched, bed_plate(&self.status), self.status.docs.clone(), self.status.elec_assets.clone());
            elec.set_memory(self.status.switched, self.status.elec_memory.clone());
        }
        // Dropped scripts refused: no plate brings them, their preview goes.
        if self.drop_call == Some(done.id) {
            self.drop_call = None;
            if done.update.is_none() {
                self.restore_committed(cx);
            }
        }
        let eye_job = self.eyes.iter().any(|e| e.0 == done.id);
        self.eyes.retain(|e| e.0 != done.id);
        // Refused or cancelled, no plate brings them: the eyes go back to what the model holds.
        if let (true, None, Some(ev)) = (eye_job, &done.update, &self.plate) {
            for p in &mut self.state.plate.parts {
                p.printed = ev.plate.parts.iter().any(|m| m.object == p.object);
            }
            self.show_eyes();
            self.plate_revision += 1;
            self.state.plate.revision = self.plate_revision;
            cx.redraw_all();
        }
        // The Strength editor's call: an Apply's answer is followed by the find that waits for its
        // slice and says what it did.
        if self.strength_test == Some(done.id) {
            self.strength_test = None;
            if !done.ok {
                self.state.ui.strength.note = model_host::reason(&done.text);
                cx.redraw_all();
            }
        }
        if self.strength_call == Some(done.id) {
            self.strength_call = None;
            if done.ok && makepad_script::syntax::parse_value(&done.text).is_ok_and(|v| v.field("done").is_some()) {
                self.state.ui.strength.after.clear();
                self.find_weak_spots();
                self.state.ui.strength.busy = Some(fab::ui::strength::WAITING_FOR_SLICE);
            } else {
                self.strength_found(done.ok, &done.text);
            }
            cx.redraw_all();
        }
        // A volume from the Overrides editor or a menu: picked once it is built, or why not in the status.
        if self.volume_call == Some(done.id) {
            self.volume_call = None;
            let reply = makepad_script::syntax::parse_value(&done.text).ok();
            let solid = reply.as_ref().and_then(|v| v.field("solids")).and_then(|f| f.value.items()).and_then(|l| l.first().and_then(|n| n.string().map(str::to_string)));
            match solid.filter(|_| done.ok) {
                Some(solid) => self.select_after = Some(solid),
                None => {
                    log!("fab: settings volume refused: {}", done.text);
                    self.state.ui.notices.say(NoticeView::Plate, model_host::reason(&done.text));
                }
            }
        }
        if self.study_job == Some(done.id) {
            self.study_job = None;
            if !done.ok {
                log!("fab: study edit refused: {}", done.text);
                self.state.ui.notices.say(NoticeView::Plate, study_edit::refusal(&done.text));
            }
        }
        if self.test_lay == Some(done.id) {
            self.test_lay = None;
            if !done.ok {
                log!("fab: test print lay refused: {}", done.text);
                self.state.ui.notices.say(NoticeView::Plate, model_host::reason(&done.text));
            }
        }
        if self.reset_job == Some(done.id) {
            self.reset_job = None;
            if !done.ok {
                log!("fab: box setting reset refused: {}", done.text);
                self.state.ui.notices.say(NoticeView::Plate, format!("Not reset: {}", done.text));
            }
        }
        if self.paint_job == Some(done.id) {
            self.paint_job = None;
            if !done.ok {
                log!("fab: paint edit refused: {}", done.text);
                self.state.ui.notices.say(NoticeView::Plate, paint_edit::refusal(&done.text));
                fab::tools::paint::cancel();
            }
        }
        if self.sculpt_jobs.front() == Some(&done.id) {
            self.sculpt_jobs.pop_front();
            if !done.ok {
                log!("fab: sculpt edit refused: {}", done.text);
                self.state.ui.notices.say(NoticeView::Plate, paint_edit::refusal(&done.text));
            }
            // in the plate now (this job's plate swaps in below), or refused
            fab::tools::sculpt::landed();
        }
        if !done.ok || done.update.is_none() {
            if let Some(mut view) = self.ui.widget(cx, ids!(xr)).borrow_mut::<xr::FabXr>() {
                view.failed(done.id);
            }
        }
        if self.place_job == Some(done.id) {
            self.place_job = None;
            if !done.ok || done.update.is_none() {
                self.state.scene_state.set_nudge(None);
            }
        }
        if let Some((_, id, armed)) = &mut self.demo_card {
            if *id == done.id && done.update.is_some() {
                *armed = true;
            }
        }
        if self.demo_pending && done.id == 0 && done.update.is_some() {
            self.demo_pending = false;
            // nothing came back (the plate list, sent first, names nothing on the bed)
            if self.status.solids == 0 && self.state.plates.on_bed.is_none() {
                self.open_demo("demo");
            }
        }
        let committed = self.source_commits.remove(&done.id);
        // typed text whose statement lost its selector: its draft shows, the line says so
        let source_lost = (!done.ok && picks::lost_failure(&done.text)).then(|| committed.clone()).flatten();
        if let Some(files) = &done.source_files {
            let shown = self.state.source.tab().map(|t| (t.file.clone(), t.text.clone()));
            self.scripts = files.texts.clone();
            let compiled = done.update.as_ref().and_then(|u| u.4.as_ref()).or(self.program.as_ref()).map(|(c, _)| c.as_ref());
            let params = done.update.as_ref().map(|u| u.3.as_slice()).unwrap_or(&self.committed_params);
            if compiled.is_some_and(|c| code_edit::instance_of(c, &self.state.source.file).is_none()) {
                self.state.source.file = compiled.and_then(|c| c.instances.first()).map(|i| i.script.file.clone()).unwrap_or_default();
                self.state.source.instance.clear();
            }
            code_edit::tabs(&mut self.state.source, files, compiled, params);
            let text = Arc::new(self.scripts.get(&self.state.source.file).cloned().unwrap_or_default());
            code_edit::committed(&mut self.state.source, text, true, compiled);
            if self.state.source.tab().is_none() {
                self.source_text = None;
                self.state.source.viewed = files.tabs.first().map(|(file, _)| file.clone());
                self.state.source.rev += 1;
            } else if let Some((file, text)) = shown {
                let own = committed.as_ref().is_some_and(|(f, _)| *f == file);
                if let Some(tab) = self.state.source.tab_mut() {
                    if !own && tab.text != text && tab.draft.is_none() {
                        tab.problem = None;
                        self.state.source.rev += 1;
                    }
                }
            }
        }
        if let Some((file, text)) = committed {
            if let Some(tab) = self.state.source.tabs.iter_mut().find(|t| t.file == file) {
                // A newer edit may already be previewing or waiting for the worker.
                if tab.draft.as_ref() == Some(&text) {
                    tab.problem = done.source_problem.as_ref().map(|p| (p.line.map(|(l, _)| l.saturating_sub(1) as usize), shown_error(&p.message)));
                    if done.ok {
                        tab.draft = None;
                        // The model wrote more than was typed (an object's name, see
                        // `fab_model::program::named_objects`): the panel shows its text.
                        if done.source_files.is_some() && tab.text.as_str() != text.as_str() && self.state.source.viewed.as_ref() == Some(&file) {
                            self.state.source.rev += 1;
                        }
                    } else if tab.problem.is_none() {
                        tab.problem = Some((None, shown_error(&done.text)));
                    }
                }
            }
        }
        switch_watch::mark("code panel");
        self.state.source.editing = self.state.source.tab().is_some_and(|t| t.draft.is_some());
        self.state.source.problem = self.state.source.tabs.iter().find(|t| t.file == self.state.source.file).and_then(|t| t.problem.clone());
        if let Some(update) = &done.update {
            // the selector report of this build marks its solids' rows
            self.state.source.drifted = update.0.drifted.iter().map(|d| (d.part.clone(), d.short.clone())).collect();
            if self.state.test_prints != update.6 {
                let was = self.state.test_prints.len();
                makepad_error_log::flight::mark_ab(if update.6.len() > was { "fab.test_print.add" } else { "fab.test_print.edit" }, update.6.iter().filter(|t| t.on).map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ").as_str(), update.6.len() as u64, was as u64);
            }
            self.state.test_prints = update.6.clone();
            fab::ui::test_prints::show_rows(&mut self.state);
            self.sync_tests();
        }
        // The drag's box stops showing when the plate has it, or says why not; one begun since stays.
        if let Some((_, drag)) = self.test_place.take_if(|(id, _)| *id == done.id) {
            if self.state.ui.test_drag.as_ref() == Some(&drag) {
                self.state.ui.test_drag = None;
            }
        }
        if let Some(id) = self.select_after.clone() {
            if let Some(f) = self.state.source.features.iter().find(|f| f.id == id || f.object == id).map(|f| f.id.clone()) {
                self.select_after = None;
                self.source_action(cx, &fab::ui::code::SourceAction::Select { id: f, reveal: true });
            } else if !done.ok {
                self.select_after = None;
            }
        }

        let commit = if self.param_commit.as_ref().is_some_and(|c| c.id == done.id) { self.param_commit.take() } else { None };
        // A drag that left a statement without its selector stays a draft, shown and marked,
        // until a pick names the selector again: its commit refused, nothing goes back.
        let lost_draft = !done.ok && commit.is_some() && picks::lost_failure(&done.text);
        // A value set without a drag's draft (typed in its box): the draft shows now.
        if let (true, None, Some(c), Some(host)) = (lost_draft, &self.repick, &commit, &mut self.host) {
            host.preview_param(c.key.clone(), c.value.clone());
        }
        if source_lost.is_some() {
            self.keep_draft_job(done.id);
        }
        // the commit dropped the text's preview: its draft shows now, unless typing went on
        if let (Some((file, text)), None, Some(host)) = (&source_lost, &self.repick, &mut self.host) {
            if self.state.source.tabs.iter().any(|t| t.file == *file && t.draft.as_ref() == Some(text)) {
                host.preview_source(file.clone(), text.to_string());
            }
        }
        let mut cap_line: Option<String> = source_lost.map(|(file, _)| match self.repick.as_ref().and_then(picks::RePick::hint) {
            Some(hint) => format!("{file} · not committed, a draft: {hint}"),
            None => format!("{file} · not committed: a statement lost its selector, its draft shows"),
        });
        if let Some(c) = &commit {
            if done.update.is_some() {
                if let Some((hold, why)) = &done.capped {
                    let hold = param_view::value(hold);
                    cap_line = self.param_view(&c.key).map(|pv| fab::params::cap_message(&pv, &hold, why));
                    self.state.params.refuse(&c.key, hold, Cx::time_now());
                } else {
                    cap_line = c.said.clone();
                }
            } else if lost_draft {
                cap_line = Some(match self.repick.as_ref().and_then(picks::RePick::hint) {
                    Some(hint) => format!("{} · not committed, a draft: {hint}", c.key),
                    None => format!("{} · not committed: a statement lost its selector, its draft shows", c.key),
                });
            } else if !done.ok && model_host::reason(&done.text) != "cancelled" {
                if let Some(pv) = self.param_view(&c.key) {
                    cap_line = Some(fab::params::cap_message(&pv, &pv.value, &model_host::reason(&done.text)));
                    self.state.params.refuse(&c.key, pv.value.clone(), Cx::time_now());
                }
            }
        }

        // The newest commit answered: nothing of it is being built any more. Its values are the
        // model's now (or refused), so only a drag still in progress stays pending; the built
        // values below are not compared with what was typed (a snapped or reformatted value
        // would otherwise keep the part "rebuilding" for ever).
        if self.param_job.is_some_and(|id| id <= done.id) {
            self.param_job = None;
            self.state.params.rebuilding = None;
            let dragging = self.state.params.dragging.as_ref().map(|(k, _)| k.clone());
            self.state.params.pending.retain(|(k, _)| Some(k) == dragging.as_ref());
            // Refused or unchanged, no plate comes: a preview still showing goes, and the
            // controls show what the model holds (a refusal says why below).
            if done.update.is_none() && !lost_draft {
                self.restore_committed(cx);
                self.drop_ghost();
                self.slice_live_fields(cx);
            }
        }
        if let Some((ev, scene, markers, params, program, measurements, _)) = done.update.clone() {
            // a committed plate is no draft
            self.clear_lost(cx);
            if self.selected_ghost().is_some() {
                self.drop_ghost();
            }
            if !self.framed { self.state.selected_measure = None; }
            tools::measure::install(&mut self.state, measurements);
            if program.is_some() {
                self.program = program;
                self.publish_trees();
            }
            // Values set and now built stop being pending; newer ones set meanwhile stay.
            self.state.params.pending.retain(|(key, value)| {
                let built = params.iter().flat_map(|pp| pp.params.iter()).find(|p| p.key == *key).map(|p| &p.value);
                built.is_some_and(|b| b != value)
            });
            if self.state.params.pending.is_empty() {
                self.state.params.rebuilding = None;
                // The rebuilt part has the cut: its tool's ghost fades, and one still being
                // built never shows.
                self.ghost_gen += 1;
                if let Some(g) = &mut self.state.params.ghost {
                    if g.fade_from.is_none() {
                        Arc::make_mut(g).fade_from = Some(Cx::time_now());
                    }
                }
            }
            let prefix = format!("{}/", self.state.source.instance);
            self.state.source.sites = params.iter().filter(|pp| pp.object.starts_with(&prefix)).flat_map(|pp| pp.params.iter()).filter_map(|p| p.site.clone().map(|r| (r, p.key.clone()))).collect();
            // The committed plate replaces any preview showing (a cancel goes back to it); a file
            // drag's preview is asked again of it.
            self.previewing = false;
            if let Some(drag) = &mut self.file_drag {
                drag.gen = None;
            }
            self.committed_params = params.clone();
            self.state.params.parts = params;
            if let Some(f) = &self.state.params.focus {
                if !ev.objects.iter().any(|o| &o.name == f) {
                    self.state.params.focus = None;
                }
            }
            self.plate_revision += 1;
            let had_parts = !self.state.plate.parts.is_empty();
            let before = self.plate.clone();
            let machine_changed = before.as_ref().is_some_and(|old| !old.machine.same_volume(&ev.machine));
            switch_watch::mark("params");
            if machine_changed {
                self.ghosts = Some(ghost::GhostWorker::start(cx, ev.machine.clone()));
                self.drop_ghost();
            }
            let old_objects: Vec<String> = if picture { self.state.plate.parts.iter().map(|p| p.object.clone()).collect() } else { Vec::new() };
            let mut info = plate_edit::plate_info(self.program.as_ref().map(|(c, _)| c.as_ref()), &ev, self.plate_revision);
            // Under the Test prints view the pieces stay until this plate's are made.
            if self.state.plate.whole.is_some() {
                info.whole = Some(Arc::new(std::mem::replace(&mut info.parts, std::mem::take(&mut self.state.plate.parts))));
            }
            self.state.plate = info;
            switch_watch::mark("plate info");
            // Until its scene installs, the parts are the old scene's elements: a selected part
            // stays selected, and the code panel on its script (with the caret where it was).
            self.state.sync_plate_elements();
            self.show_eyes();
            // a committed plate: its fields are the truth once no drag is live
            if self.state.params.dragging.is_none() {
                self.state.params.sdf_live.clear();
                self.live_shown = self.live_gen;
            }
            self.state.scene_state.set_nudge(None);
            self.state.ui.document_name = self.status.name.clone();
            // A board that rides in a case is also an insert: it counts once, as a board.
            let (mut inserts, mut boards) = (0, 0);
            for p in &ev.plate.excluded {
                if p.link.as_deref().is_some_and(|l| l.starts_with("board:")) { boards += 1 } else if p.insert.is_some() { inserts += 1 }
            }
            let hidden = ev.plate.excluded.len() - inserts - boards;
            self.state.ui.document_info = counts_text(ev.plate.parts.len(), hidden, inserts, boards, &self.status);
            self.state.print.model_markers = Some(markers);
            // Frame on the first plate, and again when parts first appear on it.
            let frame = (!self.framed || (!had_parts && !self.state.plate.parts.is_empty())) && !std::mem::take(&mut self.keep_view);
            self.framed = true;
            let blocking = ev.blocking_issues();
            self.plate = Some(ev.clone());
            if self.place_boards() {
                fab::ui::redraw_now(cx, &self.ui, &[ids!(viewport)]);
            }
            self.request_areas(&ev);
            if let Some(patterns) = &self.patterns {
                patterns.plate_in(&ev);
            }
            // its flexing pieces' bends are measured now, in the background
            crate::mech_flex::measure_bends(cx, &ev);
            if machine_changed {
                self.request_selected_ghost();
            }
            if done.model_changed {
                let previous = self.state.print.settings.clone();
                let (was_stale, prior) = (self.state.print.stale, self.state.print.stale_parts.take());
                self.printing.plate_changed(&mut self.state.print, &ev);
                // Only the parts this edit changed are out of date in the shown slice (with those
                // of earlier edits since it); any other change dates all of it.
                let p = &mut self.state.print;
                p.stale_parts = match before.as_deref().and_then(|b| changed_parts(b, &ev)) {
                    Some(changed) if p.stale && !was_stale => Some(changed.into()),
                    Some(changed) if p.stale => prior.map(|old| {
                        let mut all: Vec<u16> = old.iter().copied().chain(changed).collect();
                        all.sort_unstable();
                        all.dedup();
                        all.into()
                    }),
                    _ => None,
                };
                if previous != self.state.print.settings {
                    self.mech.supports_stale(&mut self.state);
                    if let Some(host) = &mut self.host { host.print_setup(self.printing.check_setup(&self.state.print)); }
                    live_slice::set_print_setup(self.printing.check_setup(&self.state.print));
                }
                self.remap();
                self.request_reslice();
            } else {
                self.state.print.printability = ev.printability.clone();
            }
            switch_watch::mark("print state");
            self.bed_scene = None;
            self.model_scene = Some(scene.clone());
            // a new colour picture: its face, which prints face down, seen from below the right
            // way up (its picture's top at the back, +Y)
            let face = picture.then(|| self.state.plate.parts.iter().find(|p| !old_objects.contains(&p.object)).and_then(|p| scene.elements.iter().find(|e| e.name == p.object)).map(|e| e.bounds)).flatten();
            // A plate back on the bed shows as it was left. Monitor retains its own camera. A
            // visible opening is framed once from the declared plate at layout, never first from
            // a partial display scene at 16:9.
            let restore = self.restore.take().filter(|_| frame);
            if frame {
                self.state.device.restore = Some(match restore.as_ref().and_then(|view| view.device) {
                    Some(camera) => DeviceRestore::Camera(camera),
                    None => DeviceRestore::Frame,
                });
            }
            let camera = restore.as_ref().is_some_and(|view| {
                let size = fab::ui::viewport_of(cx, &self.ui, self.state.view.active_plate()).area().rect(cx).size;
                let aspect = if size.x > 0.0 && size.y > 0.0 { (size.x / size.y) as f32 } else { 16.0 / 9.0 };
                view.camera.sees(scene.bounds, aspect)
            });
            let frame = frame && !camera && face.is_none();
            let behind_monitor = frame && self.state.print.monitor_preview().is_some();
            if self.wants_bed_only() {
                self.showing_bed = false;
                self.sync_scene(cx, behind_monitor);
            } else {
                self.showing_bed = false;
                if self.state.plate.whole.is_none() {
                    cx.action(ShellAction::SceneUpdated { scene, frame: behind_monitor });
                }
            }
            // Monitor frames its job, and the plate's model may arrive behind it.
            if frame && self.state.print.monitor_preview().is_none() {
                cx.action(ShellAction::FrameOpening(self.state.plate.view_box()));
            }
            if let Some(view) = restore {
                let source = &mut self.state.source;
                if let Some(file) = view.viewed.as_ref().filter(|f| source.viewed.as_ref() != Some(*f) && source.tabs.iter().any(|t| &t.file == *f)) {
                    source.viewed = Some(file.clone());
                    source.rev += 1;
                }
                cx.action(ShellAction::RestoreView { view, camera });
            }
            if let Some(bounds) = face {
                cx.action(ShellAction::FrameFrom { bounds, look: vec3(0.0, 0.0, 1.0), up: vec3(0.0, 1.0, 0.0) });
            }
            switch_watch::mark("scene");
            let mut problems = Vec::new();
            if blocking > 0 {
                problems.push(format!("{blocking} part(s) off the printable volume or a nozzle's reach"));
                for report in ev.reports.iter().filter(|r| r.blocking()) {
                    if let Some(part) = ev.plate.parts.get(report.part) {
                        let issues: Vec<String> = report.issues.iter().filter(|i| i.is_blocking()).map(|i| i.describe(&ev.machine)).collect();
                        problems.push(format!("{}: {}", part.name, issues.join(", ")));
                    }
                }
            }
            if let Some(w) = ev.warnings.first() {
                problems.push(w.to_string());
            }
            self.committed_notice = problems.join(" · ");
            self.state.ui.notices.stand(NoticeView::Plate, self.committed_notice.clone());
            if let Some(m) = cap_line.take() {
                self.state.ui.notices.say(NoticeView::Plate, m);
            }
            self.notice_is_preview = false;
            self.ui.window(cx, ids!(main_window)).set_title(cx, &format!("{} · Fab", self.status.name));
        } else if !done.ok && !self.calls.pending.contains_key(&done.id) && !self.host.as_ref().and_then(|h| h.call_job(done.id)).and_then(|j| makepad_widgets::jobs::board().get(j)).is_some_and(|j| j.shown_elsewhere) {
            // A command of Fab's own failed: say why (a job shown in a place of its own, a load
            // on the view's progress card, says it there, and a cancelled load leaves the bed's
            // line as it was).
            let message = makepad_script::syntax::parse_value(&done.text)
                .ok()
                .and_then(|v| v.field("error").and_then(|f| f.value.string().map(str::to_string)))
                .unwrap_or(done.text.clone());
            // A failure the job strip already shows in full (its line is not cut short) is said
            // once, there; a longer reason keeps its plain text.
            let job_reason = model_host::job_reason(&done.text);
            let on_strip = makepad_widgets::jobs::snapshot().iter().any(|j| {
                matches!(&j.end, makepad_widgets::jobs::JobEnd::Failed { reason, .. } if *reason == job_reason) && j.line().chars().count() <= fab::ui::job_strip::JOB_LINE_CHARS
            });
            match cap_line.take() {
                Some(m) => self.state.ui.notices.say(NoticeView::Plate, m),
                None if on_strip => self.state.ui.notices.clear(NoticeView::Plate),
                None => self.state.ui.notices.stand(NoticeView::Plate, message),
            }
        }
        if self.delete_jobs.remove(&done.id) && done.ok {
            self.committed_notice.clear();
        }
        self.request_reslice();
        if self.calibration_job == Some(done.id) {
            self.calibration_job = None;
            if done.ok {
                self.print_action(cx, &fab::print::PrintAction::Slice);
                cx.action(ShellAction::SetWorkspace(Workspace::Design));
                cx.action(ShellAction::SetRenderMode(fab::api::RenderMode::Slice));
            } else {
                self.state.print.calibrate.status = model_host::reason(&done.text);
            }
        }
        switch_watch::mark("status");
        let review = done.review.lock().ok().and_then(|mut r| r.take());
        if let Some(ev) = &done.check_slice {
            self.printing.check_slice(done.id, &self.state.print, ev);
            // Panels in dock tabs draw in their own lists: redraw everything.
        cx.redraw_all();
            return;
        }
        match review {
            Some(Ok(review)) => {
                // An imported part's answer goes out with its render.
                if self.calls.pending.get(&done.id).is_some_and(|t| t == fab_ai::img2mesh::TOOL) {
                    self.img2mesh_answers.insert(done.id, done.text.clone());
                }
                if self.calls.pending.get(&done.id).is_some_and(|t| t == fab_ai::supports::VIEW_TOOL) {
                    self.look_answers.insert(done.id, done.text.clone());
                }
                match review {
                    model_host::Review::Sheets(prepared) => self.review_queue.push_back((done.id, prepared)),
                    model_host::Review::Compare(prepared, job) => {
                        self.compare_jobs.insert(done.id, job);
                        self.review_queue.push_back((done.id, prepared));
                    }
                    model_host::Review::Variants(prepared, job) => {
                        self.variant_runs.insert(done.id, variant_sheet::VariantRun::new(job, prepared.len()));
                        self.review_queue.extend(prepared.into_iter().map(|p| (done.id, p)));
                    }
                }
            }
            Some(Err(e)) => self.calls.finish(done.id, chat::Answer { ok: false, text: format!("{{\"error\":{:?}}}", e), images: Vec::new() }),
            None => self.calls.finish(done.id, chat::Answer { ok: done.ok, text: if done.ok { done.text.clone() } else { shown_error(&done.text) }, images: done.images.iter().map(|(l, png)| (l.clone(), std::sync::Arc::from(png.as_slice()))).collect() }),
        }
        // Panels in dock tabs draw in their own lists: redraw everything.
        cx.redraw_all();
    }

    /// The held picture calls whose picture has its file now go to the model worker; one whose
    /// picture will have none is answered with why.
    fn release_pictures(&mut self) {
        let Some(host) = &mut self.host else { return };
        let mut at = 0;
        while at < self.held_pictures.len() {
            let Some(kept) = self.held_pictures[at].3.kept_now() else {
                at += 1;
                continue;
            };
            let (id, tool, args, picture) = self.held_pictures.remove(at);
            match kept {
                Ok(path) => host.chat_call_as(id, &tool, picture_args(&tool, args, &picture.name, &path)),
                Err(error) => self.calls.finish(id, chat::Answer { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(&error)), images: Vec::new() }),
            }
        }
    }

    /// A gesture of the person's is under way (a value or a part dragged, a stroke drawn): its
    /// edit is not in the model yet.
    fn gesture_live(&self) -> bool {
        self.state.params.dragging.is_some()
            || self.state.scene_state.nudge.is_some()
            || fab::tools::paint::live_stroke().is_some()
            || fab::tools::sculpt::brush().is_some_and(|(_, drawing)| drawing)
    }

    fn forget_drag(&mut self) {
        self.drag_cap = None;
        self.held = None;
        self.shown_ok = None;
        self.preview_asks.clear();
    }

    /// Follow the newest cap result with a probe, or restore the held build once probing is done.
    fn cap_step(&mut self, key: &str) {
        let Some(pv) = self.param_view(key) else { return };
        let Some(cap) = self.drag_cap.as_ref().filter(|c| c.key() == key) else { return };
        let Some(held) = self.held.as_mut().filter(|h| h.key == key) else { return };
        let decimals = fab::params::precision(pv.kind, pv.step);
        let next = if let Some(value) = cap.probe(decimals) {
            Some((value, false))
        } else if self.shown_ok.as_ref().is_none_or(|(k, value)| k.as_str() != key || value != &held.hold) {
            Some((held.hold.clone(), true))
        } else {
            None
        };
        held.next = None;
        if let (Some((value, is_re_preview)), Some(host)) = (next, &mut self.host) {
            let key = key.to_string();
            let gen = host.preview_param(key.clone(), param_view::model_value(&value));
            self.preview_asks.insert(gen, (key, value));
            self.preview_latest = gen;
            held.next = Some((gen, is_re_preview));
        }
    }

    /// A parameter edit from the part panel, a shape handle or the headset. While a gesture is
    /// live the value shows at once (the controls, the handles, a field part's field); a mesh
    /// part previews in draft. The gesture's end commits one undo step, evaluated off the UI
    /// thread (a newer edit supersedes it); a mesh part is marked as rebuilding until the
    /// result is in, a field part is current all along.
    fn param_action(&mut self, cx: &mut Cx, a: &fab::params::ParamAction) {
        use fab::params::ParamAction as P;
        if !matches!(a, P::Hover(_) | P::Focus(_)) {
            self.startup_monitor_done = true;
        }
        match a {
            P::Hover(k) => {
                if self.state.params.hover != *k {
                    self.state.params.hover = k.clone();
                    // A hovered parameter shows its tool's ghost (while nothing is being edited).
                    if self.state.params.dragging.is_none() && self.state.params.pending.is_empty() && self.selected_ghost().is_none() {
                        match k.as_ref().and_then(|k| self.param_value(k)) {
                            Some((_, key, value)) => self.request_ghost(&key, &value),
                            None => self.drop_ghost(),
                        }
                    }
                }
            }
            P::Focus(o) => self.state.params.focus = o.clone(),
            P::Weigh { object, open } => self.weigh(cx, object, *open),
            P::Place { object, feature, gesture, done } => {
                // The gizmo's gesture as the feature's placement in its script (from the
                // committed plate, where the drag began): previewed and committed like a value.
                let gesture = match *gesture {
                    fab::params::PlaceGesture::Move(d) => fab_model::params::Gesture::Move(d),
                    fab::params::PlaceGesture::Turn(t) => fab_model::params::Gesture::Turn(t),
                    fab::params::PlaceGesture::Scale(f) => fab_model::params::Gesture::Scale(f),
                };
                let placed = match (&self.program, &self.plate) {
                    (Some((c, _)), Some(ev)) => ev.plate.placed_parts().position(|p| p.object == *object).ok_or_else(|| format!("'{object}' is not on the plate")).and_then(|part| fab_model::params::placed(c, ev, part, feature, gesture)),
                    _ => Err("nothing is built yet".to_string()),
                };
                match placed {
                    Ok((key, value)) => {
                        let (object, value) = (object.clone(), param_view::value(&value));
                        self.param_action(cx, &if *done { P::Commit { object, key, value } } else { P::Preview { object, key, value } });
                    }
                    Err(e) => {
                        self.state.ui.notices.say(NoticeView::Plate, e);
                        cx.redraw_all();
                    }
                }
                return;
            }
            // A pattern's row, or a region's print setting, commits only: the shape stays, the
            // pattern regrows (the plate slices again) from the commit.
            P::Preview { key, .. } if fab_model::params::commits_only(key) => {}
            P::Preview { object, key, value } => {
                if self.drag_cap.as_ref().is_none_or(|c| c.key() != key.as_str()) {
                    self.forget_drag();
                    let start = self.committed_params.iter().find_map(|pp| pp.param(key)).map(|p| p.value.clone()).unwrap_or_else(|| value.clone());
                    self.shown_ok = Some((key.clone(), start.clone()));
                    self.drag_cap = Some(fab::params::DragCap::new(key, &start));
                }
                if let Some(hold) = self.drag_cap.as_ref().and_then(|c| c.blocked(value)) {
                    self.state.params.cap(key, hold, Cx::time_now(), true);
                    cx.redraw_all();
                    return;
                }
                // A preview is a gesture in progress, wherever it comes from (a view handle,
                // the panel, the headset): what waits for its end (the live slice) waits.
                self.state.params.dragging = Some((key.clone(), value.clone()));
                live_slice::set_drag_live(true);
                self.state.params.set_pending(key, value.clone());
                self.request_ghost(key, value);
                // A part drawn from its field shows the live value in its field; any other
                // part (or one shown as its mesh): its instance rebuilt in draft (newest wins).
                if self.field_drawn(object) {
                    self.request_live(key, value);
                } else if let Some(host) = &mut self.host {
                    let gen = host.preview_param(key.clone(), param_view::model_value(value));
                    self.preview_asks.insert(gen, (key.clone(), value.clone()));
                    self.preview_latest = gen;
                }
            }
            P::Commit { object, key, value } => {
                let drag = self.drag_cap.take().filter(|c| c.key() == key.as_str());
                let held = self.held.take().filter(|h| h.key == *key);
                self.preview_asks.clear();
                self.shown_ok = None;
                let said = held.map(|h| h.message);
                let decimals = self.param_view(key).map(|pv| fab::params::precision(pv.kind, pv.step)).unwrap_or(1);
                let (asked, good) = match &drag {
                    Some(cap) => cap.commit_plan(value, decimals),
                    None => (value.clone(), None),
                };
                // A gesture that ends where the model is changed nothing. Compared with the
                // committed plate: `state.params.parts` is a preview's while one shows, and the
                // preview of the value let go is usually in by the release.
                if good.is_none() && self.param_job.is_none() && self.committed_params.iter().find_map(|pp| pp.param(key)).is_some_and(|p| p.value == asked) {
                    self.param_action(cx, &P::Cancel { object: object.clone() });
                    if let Some(m) = said {
                        self.state.ui.notices.say(NoticeView::Plate, m);
                    }
                    return;
                }
                // The value stays where it was let go while the worker builds it. Only a value
                // the drag already found does not build shows where the drag was held; `good` is
                // the worker's fallback, and a capped answer moves the control there (refuse).
                let held_back = drag.as_ref().is_some_and(|cap| cap.blocked(value).is_some());
                let shown = if held_back { good.clone().unwrap_or_else(|| asked.clone()) } else { asked.clone() };
                let field = self.field_drawn(object);
                if !fab_model::params::commits_only(key) {
                    self.request_ghost(key, &shown);
                    if field {
                        self.request_live(key, &shown);
                    }
                }
                // After that last step: the build of this commit speaks for a grown part from here.
                if let Some(host) = &self.host {
                    host.grow_runs.release(self.live_gen);
                }
                self.state.params.dragging = None;
                live_slice::set_drag_live(false);
                self.state.params.set_pending(key, shown.clone());
                // The view waits for a mesh part's rebuild; a field part's view is current.
                if !field && self.state.params.rebuilding.as_ref().is_none_or(|r| r.object != *object) {
                    self.state.params.rebuilding = Some(fab::params::Rebuilding { object: object.clone(), since: Cx::time_now() });
                }
                let Some(host) = &mut self.host else { return };
                // A preview still to come is older than the commit: never shown over it. One
                // building the value let go goes on: the commit reads what it builds.
                self.preview_floor = host.commit_preview(key, &param_view::model_value(&asked));
                let id = host.set_param(key, param_view::model_value(&asked), good.map(|g| (param_view::model_value(&g), decimals)));
                self.param_job = Some(id);
                self.param_commit = Some(ParamCommit { id, key: key.clone(), said, value: param_view::model_value(&asked) });
            }
            P::Cancel { .. } => {
                self.forget_drag();
                self.state.params.dragging = None;
                live_slice::set_drag_live(false);
                self.state.params.pending.clear();
                self.state.params.sdf_live.clear();
                self.live_shown = self.live_gen;
                if let Some(host) = &mut self.host {
                    host.grow_runs.cancel(self.live_gen);
                    self.preview_floor = host.cancel_preview();
                }
                self.restore_committed(cx);
                self.drop_ghost();
                // A drag that resliced live: the committed plate again.
                self.slice_live_fields(cx);
                live_slice::shape_published();
                // Nothing committed is outstanding: nothing is rebuilding.
                if self.param_job.is_none() {
                    self.state.params.rebuilding = None;
                }
            }
        }
        if matches!(a, P::Hover(_) | P::Focus(_) | P::Preview { .. }) {
            // Live parameter values and ghosts appear in the viewport and part panel; the handles (the picked print settings box among them) in this frame.
            fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_viewport), ids!(shape_handles), ids!(tab_part), ids!(xr)]);
        } else {
            cx.redraw_all();
        }
    }

    /// Back to the committed plate after a cancelled preview.
    fn restore_committed(&mut self, cx: &mut Cx) {
        self.clear_lost(cx);
        if std::mem::take(&mut self.notice_is_preview) {
            self.state.ui.notices.stand(NoticeView::Plate, self.committed_notice.clone());
        }
        if !self.previewing {
            return;
        }
        self.previewing = false;
        self.state.params.parts = self.committed_params.clone();
        if let Some(ev) = self.plate.clone() {
            self.plate_revision += 1;
            self.state.plate = plate_edit::plate_info(self.program.as_ref().map(|(c, _)| c.as_ref()), &ev, self.plate_revision);
            self.show_eyes();
            self.state.sync_plate_elements();
        }
        if let Some(scene) = self.model_scene.clone() {
            if !self.wants_bed_only() {
                cx.action(ShellAction::SceneUpdated { scene, frame: false });
            }
        }
        self.ui.redraw(cx);
    }

    /// The code panel and the feature tree.
    fn source_action(&mut self, cx: &mut Cx, a: &fab::ui::code::SourceAction) {
        use fab::ui::code::SourceAction as S;
        if matches!(a, S::Edited { .. } | S::Move { .. } | S::Reorder { .. } | S::Rename { .. } | S::Bypass { .. } | S::SetRole { .. } | S::Remove(_)) {
            self.startup_monitor_done = true;
        }
        match a {
            S::None => {}
            S::View(file) => self.view_source(cx, file),
            S::FollowLink(key) => {
                use fab_elec_design::{Focus, FocusKind, FocusOrigin};
                let Some(feature) = fab_elec_mech::keys::feature_of_key(key) else {
                    log!("source link {key:?} names no source the app knows");
                    return;
                };
                if self.state.workspace_shown(Workspace::Electronics) {
                    cx.action(ShellAction::SetWorkspace(Workspace::Electronics));
                }
                cx.action(fab_elec_view::ViewAction::Focus(Focus {
                    kind: FocusKind::Feature,
                    sel: fab_elec_core::Sel::Feature(feature),
                    origin: FocusOrigin::Fab,
                }));
            }
            S::Edited { file, text, gesture } => {
                let Some(tab) = self.state.source.tabs.iter_mut().find(|t| t.file == *file) else { return };
                tab.draft = Some(Arc::new(text.clone()));
                tab.problem = None;
                if *file == self.state.source.file {
                    self.state.source.problem = None;
                }
                // A value popup can finish its gesture in the same event that changes tabs.
                // Its final edit still belongs to the document which emitted it.
                if self.state.source.viewed.as_ref() != Some(file) {
                    if let Some(host) = &mut self.host {
                        let id = host.set_source(file.clone(), text.clone());
                        self.source_commits.insert(id, (file.clone(), Arc::new(text.clone())));
                        self.preview_floor = host.cancel_preview();
                    }
                    return;
                }
                self.state.source.editing = true;
                self.source_text = Some(text.clone());
                if let Some(host) = &mut self.host {
                    host.preview_source(file.clone(), text.clone());
                }
                cx.stop_timer(self.source_timer);
                match gesture {
                    // A value control's last step commits its gesture.
                    Some((_, true)) => self.commit_source(cx),
                    Some((_, false)) => {}
                    None => self.source_timer = cx.start_timeout(code_edit::REST),
                }
            }
            S::Hover(key) => {
                let key = key.as_deref().and_then(|k| code_edit::param_key(&self.state.params, k));
                self.param_action(cx, &fab::params::ParamAction::Hover(key));
            }
            S::HoverFeature(id) => {
                // The feature's own parameter (its first field the parameter model lists).
                let key = id.as_ref().and_then(|id| {
                    let prefix = format!("{}/{id}.", self.state.source.instance);
                    self.state.params.parts.iter().flat_map(|pp| pp.params.iter()).find(|p| p.key.starts_with(&prefix)).map(|p| p.key.clone())
                });
                self.param_action(cx, &fab::params::ParamAction::Hover(key));
            }
            S::Focus(instance) => self.focus_instance(cx, instance),
            S::Fold(id) => {
                if !self.state.source.collapsed.remove(id) {
                    self.state.source.collapsed.insert(id.clone());
                }
                self.ui.redraw(cx);
            }
            S::Select { id, reveal } => {
                // no id: the picked feature is let go (a part's row was picked)
                if id.is_empty() {
                    if self.state.source.selected.take().is_some() {
                        self.drop_ghost();
                        self.ui.redraw(cx);
                    }
                    return;
                }
                let Some(f) = self.state.source.feature(id).cloned() else { return };
                let test_print = f.detail == fab::ui::code::TEST_PRINT;
                if test_print {
                    self.state.ui.picked_override = None;
                }
                // (a test print has no code to show)
                if *reveal && !test_print {
                    self.view_source(cx, &self.state.source.file.clone());
                    self.state.source.reveal = (self.state.source.reveal.0 + 1, f.line);
                }
                if self.state.source.selected.as_ref() != Some(id) {
                    self.drop_ghost();
                }
                self.state.source.selected = Some(f.id.clone());
                // its folds open to show it; a loop is left as it is (a copy picked in the view shows
                // on the loop's row while it is folded, and a loop's id in `collapsed` means open)
                let mut parent = f.parent.clone();
                while let Some(id) = parent {
                    let up = self.state.source.feature(&id).map(|f| (f.parent.clone(), f.group && f.op != "call"));
                    if !up.as_ref().is_some_and(|(_, is_loop)| *is_loop) {
                        self.state.source.collapsed.remove(&id);
                    }
                    parent = up.and_then(|(p, _)| p);
                }
                // Its part picked with it, its handles shown, as a pick in the view does: the
                // plate part of the row's object or of the nearest row above it that is one, else
                // (a solid no object emits, as a no-trees area) the first of its script's.
                if let Some(index) = code_edit::part_of_row(&self.state.source, &self.state.plate.parts, &f) {
                    self.part_action(cx, &PartAction::Select { index, toggle: false });
                }
                if !f.object.is_empty() {
                    self.param_action(cx, &fab::params::ParamAction::Focus(Some(f.object.clone())));
                }
                // a fixture or load: the Force mode shows it, its region strongest
                let index = f.study.as_ref().map(|s| s.index);
                if index.is_some() && !self.state.force_shown() {
                    self.dispatch(cx, &ShellAction::SetRenderMode(fab::api::RenderMode::Force));
                }
                self.force.action(&fab::api::ForceAction::Focus { object: f.object.clone(), index }, &mut self.state);
                cx.redraw_all();
            }
            S::Move { id, up } => {
                if let Some(to) = code_edit::sibling(&self.state.source, id, *up) {
                    self.reorder(id, &to);
                }
            }
            S::Reorder { id, to } => self.reorder(id, to),
            S::Rename { id, to } => {
                // the field's Return reaches the panel twice, as a button's click does
                let key = (format!("rename {id}"), to.clone(), Arc::as_ptr(&self.state.source.text) as usize);
                if self.last_move.as_ref() == Some(&key) {
                    return;
                }
                self.last_move = Some(key);
                let edit = fab_model::Edit::Rename { solid: id.clone(), to: to.clone() };
                let Some(inst) = self.program.as_ref().and_then(|(p, _)| p.instance(&self.state.source.instance)) else { return };
                if let Err(message) = makepad_model_script::lang::edit::apply(&inst.script.program, &edit) {
                    self.state.ui.notices.say(NoticeView::Plate, message);
                    self.ui.redraw(cx);
                    return;
                }
                if let Some(host) = &mut self.host {
                    host.edit(vec![fab_model::Change::Script { file: self.state.source.file.clone(), edit }]);
                    self.select_after = Some(to.clone());
                    self.state.source.selected = Some(to.clone());
                    if self.state.source.collapsed.remove(id) { self.state.source.collapsed.insert(to.clone()); }
                }
            }
            S::Bypass { id, off } => {
                let Some(row) = self.state.source.feature(id).filter(|f| f.editable && !f.root && f.op != "call") else { return };
                if row.off == *off { return; }
                // A loop row edits every call of the loop, through one instance of each.
                let solids: Vec<_> = if row.group {
                    let mut ranges = std::collections::BTreeSet::new();
                    self.state.source.features.iter().filter(|f| f.parent.as_ref() == Some(id) && ranges.insert((f.range.start, f.range.end))).map(|f| f.id.clone()).collect()
                } else { vec![id.clone()] };
                if solids.is_empty() { return; }
                let edits = solids.into_iter().map(|solid| {
                    let edit = if *off {
                        fab_model::Edit::Field { solid, field: "off".into(), code: "true".into() }
                    } else {
                        fab_model::Edit::Unset { solid, field: "off".into() }
                    };
                    fab_model::Change::Script { file: self.state.source.file.clone(), edit }
                }).collect();
                if let Some(host) = &mut self.host {
                    host.edit(edits);
                }
            }
            S::SetRole { id, role } => {
                let Some(inst) = self.program.as_ref().and_then(|(p, _)| p.instance(&self.state.source.instance)) else { return };
                let edits = code_edit::role_changed(&self.state.source, inst, id, role).and_then(|edits| {
                    // the rewrite must still parse and evaluate before it goes to the plate
                    for c in &edits {
                        if let fab_model::Change::Script { edit, .. } = c {
                            makepad_model_script::lang::edit::apply(&inst.script.program, edit)?;
                        }
                    }
                    Ok(edits)
                });
                match edits {
                    Ok(edits) if edits.is_empty() => {}
                    Ok(edits) => {
                        if let Some(host) = &mut self.host {
                            host.edit(edits);
                            self.select_after = Some(id.clone());
                        }
                    }
                    Err(message) => {
                        self.state.ui.notices.say(NoticeView::Plate, message);
                        self.ui.redraw(cx);
                    }
                }
            }
            S::Remove(id) => {
                if let Some(edits) = code_edit::removed(&self.state.source, id) {
                    if let Some(host) = &mut self.host {
                        host.edit(edits);
                    }
                }
            }
        }
    }

    /// Add a primitive part, or an op's ready feature (a modifier works on the picked feature's
    /// object, else the selected part's), as one undo step; the new feature is then picked.
    fn add_feature(&mut self, cx: &mut Cx, pick: code_edit::AddPick) {
        match pick {
            code_edit::AddPick::Shape(shape) => self.part_action(cx, &PartAction::Add { shape }),
            code_edit::AddPick::Op(op) => {
                let picked = self.state.source.selected.as_ref().and_then(|id| self.state.source.feature(id)).map(|f| f.object.clone()).filter(|o| !o.is_empty());
                let selected = picked.or_else(|| fab::ui::parts::active_part(&self.state).map(|i| self.state.plate.parts[i].object.clone()));
                let Some((program, _)) = &self.program else { return };
                let Some(inst) = code_edit::instance_of(program, &self.state.source.file) else { return };
                let Some((edits, id)) = code_edit::with_op(inst, op, selected.as_deref()) else { return };
                if let Some(host) = &mut self.host {
                    host.edit(edits);
                }
                self.select_after = Some(id);
            }
        }
    }

    /// Quit once the plate's last edits are saved: the code panel's text not committed yet goes
    /// to the model first, then the model host answers once every edit queued is saved, without
    /// waiting for work that keeps nothing ([`model_host::ModelHost::quit`]), and its answer
    /// quits. The caption's job strip shows a save it waits for; nothing waits on this thread.
    /// Asked again meanwhile, the app quits at once.
    fn quit(&mut self, cx: &mut Cx) {
        if self.quitting.is_some() {
            cx.quit();
            return;
        }
        cx.stop_timer(self.source_timer);
        self.send_typing();
        // the chips' newest EEPROMs reach the plate before its last save
        if let Some(elec) = &mut self.elec {
            elec.flush_memory();
        }
        self.elec_sync();
        let view = bed_plate(&self.status).map(|plate| (plate, self.plate_view(cx)));
        let Some(host) = &mut self.host else {
            cx.quit();
            return;
        };
        self.quitting = Some(host.quit(view));
        cx.redraw_all();
    }

    /// Commit the code panel's text (typing rested, or a value gesture ended).
    fn commit_source(&mut self, cx: &mut Cx) {
        cx.stop_timer(self.source_timer);
        self.send_typing();
    }

    /// The code panel's text not committed yet goes to the model now (its timer, if still set,
    /// finds nothing left): before another document goes on the bed, it lands on its own.
    fn send_typing(&mut self) {
        let Some(text) = self.source_text.take() else { return };
        let Some(file) = self.state.source.viewed.clone() else { return };
        let Some(host) = &mut self.host else { return };
        let sent = Arc::new(text.clone());
        let id = host.set_source(file.clone(), text);
        self.preview_floor = host.cancel_preview();
        self.source_commits.insert(id, (file, sent));
    }

    /// The plate as it is with its display meshes as the view shows them (exact parts' detail,
    /// the Mesh look): the view shows them (once no preview shows over it), nothing else changes.
    /// Keeps the electronics boards on a free spot of the plate: the last one while it stays
    /// free, else a new one from the plate's placement path (the bed's centre when none fits).
    /// Runs when a scene or a plate arrives: O(parts + outline vertices). True when it moved.
    fn place_boards(&mut self) -> bool {
        let (Some(scene), Some(ev)) = (&self.state.elec_scene, &self.plate) else { return false };
        // A board a plate part holds (linked `board:<name>` with that frame: a case's insert, or
        // the printed board itself) is drawn there.
        let placed = ev.plate.parts.len() + ev.plate.excluded.len();
        let held = |name: &str| (0..placed).any(|i| {
            plate_edit::linked_place(ev, i).is_some_and(|(key, _)| key.strip_prefix("board:") == Some(name))
        });
        let spot = elec_host::scene_footprint(scene, |name| !held(name)).and_then(|size| {
            let kept = self.state.elec_spot.filter(|&c| fab_model::eval::spot_still_free(ev, c, size));
            let spot = kept.or_else(|| fab_model::eval::free_spot(ev, size, 0.0, &[]));
            if spot.is_none() {
                log!("no free spot on the bed for the boards ({:.0} x {:.0} mm): shown at its centre", size[0], size[1]);
            }
            spot
        });
        let moved = spot != self.state.elec_spot;
        if let (true, Some(spot)) = (moved, spot) {
            log!("electronics boards on the plate at ({:.1}, {:.1}) mm", spot[0], spot[1]);
        }
        self.state.elec_spot = spot;
        moved
    }

    fn host_shown(&mut self, cx: &mut Cx, ev: &Arc<fab_model::eval::Evaluated>, scene: &Arc<fab::model::Scene>) {
        // A newer plate came first: it has its own at that detail.
        if !self.plate.as_ref().is_some_and(|p| p.content == ev.content) {
            return;
        }
        self.plate = Some(ev.clone());
        self.model_scene = Some(scene.clone());
        if !self.previewing && !self.showing_bed && self.state.plate.whole.is_none() {
            cx.action(ShellAction::SceneUpdated { scene: scene.clone(), frame: false });
        }
    }

    /// A live preview arrived: show it unless a commit or cancel overtook it.
    fn host_preview(&mut self, cx: &mut Cx, p: &model_host::PreviewDone) {
        if p.gen <= self.preview_floor {
            return;
        }
        if p.key == model_host::DROP_PREVIEW {
            self.drop_previewed(cx, p);
            return;
        }
        let asked = if p.key != "source" {
            let a = self.preview_asks.remove(&p.gen);
            self.preview_asks = self.preview_asks.split_off(&(p.gen + 1));
            a
        } else { None };
        if p.key == "source" {
            let problem = match &p.result {
                Err(e) if !e.is_empty() => Some((p.line.map(|(l, _)| l.saturating_sub(1) as usize), shown_error(e))),
                _ => None,
            };
            let Some(tab) = self.state.source.tab_mut() else { return };
            tab.problem = problem.clone();
            if self.state.source.viewed.as_ref() == Some(&self.state.source.file) {
                self.state.source.problem = problem;
            }
            match &p.result {
                // The text is the program as it is: the committed plate.
                Err(e) if e.is_empty() => {
                    self.restore_committed(cx);
                    return;
                }
                Err(_) => {
                    cx.redraw_all();
                    return;
                }
                Ok(_) => {}
            }
        }
        match &p.result {
            Ok(update) => {
                let ev = &update.0;
                if let Some((key, value)) = &asked {
                    if let Some(cap) = self.drag_cap.as_mut().filter(|c| c.key() == key.as_str()) {
                        cap.built(value);
                    }
                    self.shown_ok = Some((key.clone(), value.clone()));
                    if std::env::var_os("FAB_PARAM_LOG").is_some() {
                        log!("fab param shown {key} = {:?}", param_view::model_value(value));
                    }
                }
                self.show_preview_plate(cx, update, &|_| false);
                self.mark_lost(cx, &ev.lost, update.4.as_ref().map(|(c, _)| c), p.value.clone().map(|v| (p.key.clone(), v)));
                if let Some(lost) = ev.lost.first() {
                    self.state.ui.notices.say(NoticeView::Plate, format!("{} · draft built in {:.0} ms: {}", p.key, ev.millis, lost.hint));
                } else {
                    self.state.ui.notices.clear(NoticeView::Plate);
                }
                if p.key != "source" {
                    let next = self.held.as_ref().filter(|h| h.key == p.key && p.gen >= self.preview_latest)
                        .and_then(|h| h.next.filter(|(gen, _)| *gen == p.gen));
                    if let (Some((key, value)), Some((_, is_re_preview))) = (&asked, next) {
                        let pv = self.param_view(key);
                        if let Some(held) = &mut self.held {
                            held.next = None;
                            if !is_re_preview {
                                held.hold = value.clone();
                                self.state.params.cap(key, value.clone(), Cx::time_now(), false);
                                held.message = pv.as_ref().map(|pv| fab::params::cap_message(pv, &held.hold, &held.why)).unwrap_or_else(|| held.why.clone());
                            }
                            self.state.ui.notices.say(NoticeView::Plate, held.message.clone());
                        }
                        if !is_re_preview { self.cap_step(key); }
                    } else {
                        self.held = None;
                    }
                }
                // A mesh part's parameter dragged under the slice view reslices from its preview.
                if p.key != "source" {
                    let ev = ev.clone();
                    self.slice_live_preview(cx, &ev);
                }
            }
            Err(e) if p.key != "source" => {
                let capped = match (&asked, self.drag_cap.as_mut()) {
                    (Some((key, value)), Some(cap)) if cap.key() == key.as_str() => Some((key, cap.failed(value))),
                    _ => None,
                };
                if p.gen >= self.preview_latest {
                    if let Some((key, hold)) = capped {
                        let pv = self.param_view(key);
                        let next = self.held.as_ref().filter(|h| h.key == *key)
                            .and_then(|h| h.next.filter(|(gen, _)| *gen == p.gen));
                        if let Some((_, is_re_preview)) = next {
                            if let Some(held) = &mut self.held {
                                held.why = e.clone();
                                held.message = pv.as_ref().map(|pv| fab::params::cap_message(pv, &held.hold, e)).unwrap_or_else(|| e.clone());
                                held.next = None;
                                self.state.ui.notices.say(NoticeView::Plate, held.message.clone());
                            }
                            if !is_re_preview { self.cap_step(key); }
                        } else {
                            let message = pv.as_ref().map(|pv| fab::params::cap_message(pv, &hold, e)).unwrap_or_else(|| e.clone());
                            self.state.params.cap(key, hold.clone(), Cx::time_now(), true);
                            self.state.ui.notices.say(NoticeView::Plate, message.clone());
                            self.held = Some(Held { key: key.clone(), hold, why: e.clone(), message, next: None });
                            self.cap_step(key);
                        }
                    } else {
                        self.state.ui.notices.say(NoticeView::Plate, e.clone());
                    }
                }
            }
            Err(e) => self.state.ui.notices.say(NoticeView::Plate, e.clone()),
        }
        self.notice_is_preview = true;
        self.ui.redraw(cx);
    }

    /// Monitor follows the printer's job: the view goes to its layer, and frames run while the
    /// estimated nozzle moves.
    /// Map the print colours again (a printer report, a project or setup change): the job's
    /// materials follow, and the plate takes the new mapping on the model worker (it slices
    /// once it has it).
    fn remap(&mut self) {
        let before = self.state.print.settings.materials.clone();
        let mapped = self.printing.refresh_printer(&mut self.state.print, self.plate.as_ref());
        let changed = before != self.state.print.settings.materials;
        if changed {
            if let Some(host) = &mut self.host { host.print_setup(self.printing.check_setup(&self.state.print)); }
            live_slice::set_print_setup(self.printing.check_setup(&self.state.print));
        }
        if let (Some(mapped), Some(host)) = (mapped, &mut self.host) {
            self.mapping_sent = Some(mapped.clone());
            host.mapped(mapped);
        }
        if changed {
            self.request_reslice();
        }
    }

    fn follow_printer(&mut self, cx: &mut Cx) {
        if fab::print::follow::place(&mut self.state.print, Cx::time_now()) && self.print_frame_time.is_none() {
            self.print_frame = cx.new_next_frame();
        }
        self.sync_scene(cx, false);
    }

    fn resume_monitor(&mut self, cx: &mut Cx) {
        if self.startup_monitor_done || !self.state.print.follow.jobs_read {
            return;
        }
        // Connection progress is not a printer status. Wait for its first actual report,
        // regardless of whether the jobs or the report arrived first.
        if !self.state.print.printer.as_ref().is_some_and(|p| p.state.gcode_state.is_some()) {
            return;
        }
        self.startup_monitor_done = true;
        // printer_changed only supplies a job while busy, and its id only for a saved copy.
        if self.state.print.follow.job.as_ref().is_some_and(|job| job.id.is_some()) {
            self.dispatch(cx, &ShellAction::SetWorkspace(Workspace::Monitor));
            self.follow_printer(cx);
            cx.redraw_all();
        }
    }

    /// A parameter's committed view, or its shown view before the first commit arrives.
    fn param_view(&self, key: &str) -> Option<fab::params::ParamView> {
        self.committed_params.iter().find_map(|pp| pp.param(key))
            .or_else(|| self.state.params.parts.iter().find_map(|pp| pp.param(key))).cloned()
    }

    /// A parameter of the selected part: its part object, key and the value it shows.
    fn param_value(&self, key: &str) -> Option<(String, String, fab::params::ParamValue)> {
        let s = &self.state.params;
        s.parts.iter().find_map(|pp| pp.param(key).map(|p| (pp.object.clone(), key.to_string(), s.shown_value(p))))
    }

    /// Build the ghost of the tool bodies `key` shapes at `value` (newest request wins).
    /// The plate's distance-field parts at a dragged value (compiled, not meshed).
    fn request_live(&mut self, key: &str, value: &fab::params::ParamValue) {
        let (Some(live), Some((program, model)), Some(ev), Some(host)) = (&self.live, &self.program, &self.plate, &self.host) else { return };
        // each field part's whole placement: the live shape is the model's, before a stretch
        let parts: Vec<(String, [[f64; 4]; 3])> = ev.plate.placed_parts().enumerate().filter_map(|(i, p)| ev.placed_field(i).map(|f| (p.object.clone(), f.to_printer))).collect();
        if parts.is_empty() {
            return;
        }
        self.live_gen += 1;
        // the slice of the old value stops now; the new fields' input restarts it
        live_slice::shape_changed();
        live.request(sdf_live::LiveRequest { gen: self.live_gen, program: program.clone(), model: model.clone(), ev: ev.clone(), key: key.to_string(), value: param_view::model_value(value), parts, asked: std::time::Instant::now(), grow_runs: host.grow_runs.clone() });
    }

    /// Distance-field parts' live fields resliced: the committed plate with the sculpted
    /// (`SdfLayer::sculpt_fields`) and dragged (`state.params.sdf_live`) fields in place, the
    /// layer the view shows (or the brush's) first. Every step of a drag or stroke reslices (the
    /// newest step wins); the commit slices the committed plate, which is what Print uses.
    fn slice_live_fields(&mut self, cx: &mut Cx) {
        if self.live_view.is_none() || !self.slice_wanted() {
            return;
        }
        let Some(ev) = self.plate.clone() else { return };
        let sculpted: Vec<fab::api::LiveSdf> = self.state.sdf.as_ref().map(|l| l.borrow().sculpt_fields().cloned().collect()).unwrap_or_default();
        let live: Vec<&fab::api::LiveSdf> = sculpted.iter().chain(&self.state.params.sdf_live).collect();
        let swaps: Vec<live_slice::FieldSwap> = live.iter().filter_map(|l| {
            let part = ev.plate.parts.iter().position(|p| p.object == l.object)?;
            ev.fields.get(part)?.as_ref()?;
            Some(live_slice::FieldSwap { part, sdf: l.sdf.clone() })
        }).collect();
        if let Some(field) = live.first() {
            live_slice::model_built(live_slice::edited_plate_id(&ev, &swaps), field.asked, field.compile_ms);
        }
        self.focus_slice();
        self.printing.live_input(&mut self.state.print, Some(&ev), swaps.into(), None);
        self.state.print.bump();
        // Live fields affect the viewport and the panels which show slice status or summaries.
        fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_viewport), ids!(tab_part), ids!(tab_slice), ids!(tab_settings), ids!(tab_printer), ids!(caption), ids!(xr)]);
    }

    /// A preview plate resliced (its draft meshes), the layer the view shows first. Every step
    /// of a drag reslices (the newest step wins); the release's commit slices the committed
    /// plate, which is what Print uses.
    fn slice_live_preview(&mut self, cx: &mut Cx, ev: &Arc<fab_model::eval::Evaluated>) {
        if self.live_view.is_none() || !self.slice_wanted() {
            return;
        }
        self.focus_slice();
        self.printing.live_input(&mut self.state.print, Some(ev), Arc::from([]), None);
        self.state.print.bump();
        self.ui.redraw(cx);
    }

    /// The height (printer mm) of the brush while a sculpt or paint stroke is drawn or waits to
    /// land in the slice view.
    fn brush_z(&self) -> Option<f64> {
        if !self.state.print.play.active {
            return None;
        }
        fab::tools::sculpt::brush().map(|(z, _)| z).or_else(fab::tools::paint::brush_z)
    }

    /// The layer the slice view shows is sliced first. While the view shows the whole stack
    /// (its top layer), a parameter being dragged puts its handle's height first: where the
    /// edit shows, not the part's top. While a brush stroke is live in the slice view, the
    /// brush's height goes first, wherever the view is: the layers a stroke reaches are the
    /// ones that change. Its landing gives the view its layer back (that input's focus).
    fn focus_slice(&self) {
        if let Some(z) = self.brush_z() {
            live_slice::set_focus_z(Some(z));
            return;
        }
        let p = &self.state.print;
        let shown = p.showing();
        let z = shown.and_then(|pv| pv.layers.iter().find(|l| l.layer == p.play.layer_hi)).map(|l| l.z as f64);
        let whole = shown.is_some_and(|pv| pv.layers.last().is_none_or(|l| l.layer <= p.play.layer_hi));
        let s = &self.state.params;
        let key = s.dragging.as_ref().map(|(k, _)| k).or(s.pending.last().map(|(k, _)| k));
        let handle_z = key.filter(|_| whole).and_then(|k| s.parts.iter().find_map(|pp| pp.param(k))).and_then(|pv| pv.handle.as_ref()).map(|h| match h {
            fab::params::ParamHandle::Linear { to, .. } => to[2],
            fab::params::ParamHandle::Radius { center, .. } | fab::params::ParamHandle::Angle { center, .. } => center[2],
            fab::params::ParamHandle::Point { at, .. } | fab::params::ParamHandle::Count { at, .. } => at[2],
            fab::params::ParamHandle::Profile { origin, .. } => origin[2],
        });
        live_slice::set_focus_z(handle_z.or(z));
    }

    /// Every part of `object`'s instance is drawn from its distance field: a value shows in the
    /// field at once, and nothing the view shows waits for the model's evaluation. A part shown
    /// as its mesh (the mesh look, a field the GPU cannot draw) previews from the model like any
    /// other.
    fn field_drawn(&self, object: &str) -> bool {
        let instance = object.split('/').next().unwrap_or(object);
        let layer = self.state.sdf.as_ref().map(|l| l.borrow());
        let mut parts = self.state.plate.parts.iter().filter(|p| p.object.split('/').next() == Some(instance)).peekable();
        parts.peek().is_some() && parts.all(|p| p.sdf.is_some() && !layer.as_ref().is_some_and(|l| l.shows_mesh(&p.object)))
    }

    /// Row `id` to sibling `to`'s slot, once per text shown.
    fn reorder(&mut self, id: &str, to: &str) {
        let key = (id.to_string(), to.to_string(), Arc::as_ptr(&self.state.source.text) as usize);
        if self.last_move.as_ref() == Some(&key) {
            return;
        }
        if let (Some(edits), Some(host)) = (code_edit::reordered(&self.state.source, id, to), &mut self.host) {
            host.edit(edits);
            self.last_move = Some(key);
        }
    }

    /// Tool bodies at a live value. Hover ghosts skip fields; a selected cutter shows on
    /// both field and mesh parts.
    fn request_ghost(&mut self, key: &str, value: &fab::params::ParamValue) {
        let selected = self.selected_ghost();
        if selected.as_ref().is_some_and(|o| o.split('/').next() != key.split('/').next()) {
            return self.request_selected_ghost();
        }
        let frames: Vec<(String, [[f64; 4]; 3])> = match &self.plate {
            Some(ev) => ev.plate.placed_parts().enumerate().filter(|(_, p)| selected.is_some() || !self.field_drawn(&p.object)).filter_map(|(i, p)| ev.placed_frame(i).map(|f| (p.object.clone(), *f))).collect(),
            None => Vec::new(),
        };
        let (Some(ghosts), Some((program, model))) = (&self.ghosts, &self.program) else { return };
        if frames.is_empty() {
            return self.drop_ghost();
        }
        self.ghost_gen += 1;
        self.ghost_key = Some(key.to_string());
        self.ghost_node = selected.clone();
        ghosts.request(ghost::GhostRequest { gen: self.ghost_gen, program: program.clone(), model: model.clone(), key: key.to_string(), value: Some(param_view::model_value(value)), solid: selected, frames });
    }

    /// A selected subtractive row keeps its own tool visible, without needing a parameter; so
    /// does a no-trees area (its grip in the view).
    fn selected_ghost(&self) -> Option<String> {
        self.state.source.selected.as_ref().and_then(|id| self.state.source.feature(id))
            .filter(|f| (f.role == "-" || f.detail == fab::ui::code::NO_TREES) && !f.object.is_empty()).map(|f| f.object.clone())
    }

    /// Request the committed cutter on field and mesh parts alike.
    fn request_selected_ghost(&mut self) {
        let Some(object) = self.selected_ghost() else { return };
        let (Some(ghosts), Some((program, model)), Some(ev)) = (&self.ghosts, &self.program, &self.plate) else { return };
        let frames = ev.plate.placed_parts().enumerate().filter_map(|(i, p)| ev.placed_frame(i).map(|f| (p.object.clone(), *f))).collect();
        self.ghost_gen += 1;
        self.ghost_key = Some(object.clone());
        self.ghost_node = Some(object.clone());
        ghosts.request(ghost::GhostRequest { gen: self.ghost_gen, program: program.clone(), model: model.clone(), key: object.clone(), value: None, solid: Some(object), frames });
    }

    /// The plate's marked areas (no trees, print settings), built off the UI thread
    /// ([`ghost::GhostWorker::areas`]): none when no script marks one.
    fn request_areas(&mut self, ev: &fab_model::eval::Evaluated) {
        self.areas_gen += 1;
        self.areas_rows = self.committed_params.iter().flat_map(|pp| pp.params.iter()).filter_map(|p| match (&p.value, &p.handle, p.objects.as_slice()) {
            (fab::params::ParamValue::Num(x), Some(h @ (fab::params::ParamHandle::Linear { .. } | fab::params::ParamHandle::Point { .. })), [object]) => {
                Some((object.clone(), fab::params::AreaRow { key: p.key.clone(), built: *x, handle: h.clone() }))
            }
            _ => None,
        }).collect();
        let (Some(ghosts), Some((program, _))) = (&self.ghosts, &self.program) else { return };
        if !program.instances.iter().any(|i| !fab_model::program::area_solids(&i.script.program).is_empty()) {
            self.state.params.areas = Arc::default();
            return;
        }
        let frames = ev.plate.placed_parts().enumerate().filter_map(|(i, p)| ev.placed_frame(i).map(|f| (p.object.clone(), *f))).collect();
        ghosts.areas(ghost::AreasRequest { gen: self.areas_gen, program: program.clone(), frames });
    }

    /// No tool ghost: none showing, and one still being built never shows.
    fn drop_ghost(&mut self) {
        self.ghost_gen += 1;
        self.ghost_key = None;
        self.ghost_node = None;
        self.state.params.ghost = None;
    }

    /// A selected cutter persists; other ghosts last while their parameter is active.
    fn sync_ghost(&mut self, cx: &mut Cx) {
        let selected = self.selected_objects();
        let moved_on = selected != self.ghost_selection;
        self.ghost_selection = selected;
        let s = &self.state.params;
        let editing = s.dragging.is_some() || !s.pending.is_empty();
        if moved_on && !editing {
            self.state.params.hover = None;
        }
        let node = self.selected_ghost();
        if node != self.ghost_node {
            self.drop_ghost();
            if node.is_some() { self.request_selected_ghost(); }
            // Ghost geometry and its presence readout live in the viewport and part panel.
            fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_viewport), ids!(tab_part), ids!(xr)]);
        }
        let s = &self.state.params;
        let wanted = node.is_some() || self.ghost_key.as_ref().is_some_and(|key| s.hover.as_ref() == Some(key) || s.dragging.as_ref().is_some_and(|(k, _)| k == key) || s.pending.iter().any(|(k, _)| k == key));
        // A fade runs out on its own (the view draws it for 0.4 s).
        let fading = s.ghost.as_ref().and_then(|g| g.fade_from).is_some_and(|t| Cx::time_now() - t < 0.4);
        if !wanted && !fading && (self.ghost_key.is_some() || s.ghost.is_some()) {
            self.drop_ghost();
            // Ghost geometry and its presence readout live in the viewport and part panel.
            fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_viewport), ids!(tab_part), ids!(xr)]);
        }
    }

    /// The print preview hides the model's parts unless asked to keep them: the scene loses
    /// them only where they cannot be edited under it (Design keeps them pickable, see
    /// `AppState::parts_under_slice`), and only where the preview shows
    /// (`AppState::slice_workspace`): Mechanics, Electronics and the others keep the model.
    fn wants_bed_only(&self) -> bool {
        self.state.slice_workspace() && (self.state.print.monitor_holds() || (!self.state.ui.in_mode(PlateMode::Design) && self.state.print.showing().is_some() && !self.state.print.play.show_model))
    }

    /// Part edits act only while the view or the parts list has the keys.
    fn parts_have_focus(&self, cx: &Cx) -> bool {
        self.state.ui.parts_have_focus(cx.keyboard.key_focus())
    }

    /// L: the slice render mode, and back to the look the parts have.
    fn slice_toggled(&self) -> fab::api::RenderMode {
        use fab::api::{PartLook, RenderMode};
        if !self.state.print.play.active {
            return RenderMode::Slice;
        }
        match self.state.view.look {
            PartLook::Field => RenderMode::Field,
            PartLook::Mesh => RenderMode::Mesh,
        }
    }

    /// The plate's printer is a resin one: the resin slicer cuts its layers
    /// (`sla::ResinView`) and nothing slices for filament.
    fn resin_printer(&self) -> bool {
        self.plate.as_ref().is_some_and(|ev| ev.machine.resin_screen().is_some())
    }

    /// Something shows slices: the Slice render mode, or Monitor without a slice of its job (a
    /// job the printer runs is matched to Fab's slice).
    fn slice_shown(&self) -> bool {
        if self.resin_printer() {
            return false;
        }
        if self.state.ui.in_mode(PlateMode::Monitor) {
            return self.state.print.monitor_preview().is_none();
        }
        self.state.print.play.active
    }

    fn support_list_visible(&self) -> bool {
        self.state.ui.in_mode(PlateMode::Design) && self.state.print.play.active
    }

    /// The Strength editor's find: the tool the AI calls, off the model worker, which waits for the
    /// plate's slice when none is made.
    fn find_weak_spots(&mut self) {
        let view = &mut self.state.ui.strength;
        // the panel shows every row: a reply's size bounds what the AI is sent, not a list on screen
        let mut args = vec![("goal", json::s(if view.goal == 1 { "supports" } else { "strength" })), ("whole", json::Value::Bool(true))];
        if let Some(t) = view.min_thickness.trim().trim_end_matches("mm").trim().parse::<f64>().ok().filter(|t| *t > 0.0) {
            args.push(("min_thickness", json::Value::F64(t)));
        }
        if let Some(a) = view.max_angle.trim().parse::<f64>().ok().filter(|a| *a > 0.0) {
            args.push(("max_angle", json::Value::F64(a)));
        }
        // a plate of several parts plans the one that's picked
        if let Some(object) = fab::ui::parts::active_part(&self.state).and_then(|k| self.state.plate.parts.get(k)).map(|part| part.object.clone()) {
            args.push(("object", json::s(object)));
        }
        if let Some(host) = &mut self.host {
            self.state.ui.strength.busy = Some(fab::ui::strength::READING);
            self.strength_key = live_slice::latest_toolpaths_key();
            self.strength_call = Some(host.call(fab_ai::strength_plan::TOOL, json::obj(args)));
        }
    }

    /// The Strength editor's Apply: its ticked rows, as the tool the AI calls takes them.
    fn apply_strength(&mut self) {
        let Some(head) = self.status.head else {
            self.state.ui.notices.say(NoticeView::Plate, "Strength: the plate is still being built; ask again once its parts show".to_string());
            return;
        };
        let view = &mut self.state.ui.strength;
        let fixes: Vec<json::Value> = view.ticked().into_iter().map(|pick| {
            let mut one = vec![("fix", json::s(pick.fix))];
            for (key, word) in [("spot", pick.spot), ("tube", pick.tube), ("object", pick.object)] {
                if let Some(word) = word {
                    one.push((key, json::s(word)));
                }
            }
            for (key, number) in [("to", pick.to), ("spin", pick.spin), ("max_angle", pick.max_angle)] {
                if let Some(number) = number {
                    one.push((key, json::Value::F64(number)));
                }
            }
            if let Some(up) = pick.up {
                one.push(("up", json::Value::Arr(up.iter().map(|x| json::Value::F64(*x)).collect())));
            }
            json::obj(one)
        }).collect();
        if let (Some(host), false) = (&mut self.host, fixes.is_empty()) {
            view.busy = Some(fab::ui::strength::APPLYING);
            let args = vec![("expected", json::s(head.text())), ("fixes", json::Value::Arr(fixes))];
            self.strength_call = Some(host.call(fab_ai::strengthen::TOOL, json::obj(args)));
        }
    }

    /// A weak spot's test print: the box the tool's row says, through the tool the AI calls.
    fn test_print_spot(&mut self, id: &str) {
        let Some(head) = self.status.head else {
            self.state.ui.notices.say(NoticeView::Plate, "Test print: the plate is still being built; ask again once its parts show".to_string());
            return;
        };
        let Some((object, name, at, size)) = self.state.ui.strength.rows.iter().find(|r| r.id == id).and_then(|r| r.test_print.clone()) else { return };
        let numbers = |p: [f64; 3]| json::Value::Arr(p.iter().map(|x| json::Value::F64(*x)).collect());
        let one = json::obj(vec![("object", json::s(object)), ("name", json::s(name)), ("at", numbers(at)), ("size", numbers(size))]);
        if let Some(host) = &mut self.host {
            self.strength_test = Some(host.call("test_print", json::obj(vec![
                ("request_id", json::s(format!("strength-test-{id}-{}", head.text()))),
                ("expected", json::s(head.text())),
                ("action", json::s("add")),
                ("boxes", json::Value::Arr(vec![one])),
            ])));
        }
    }

    /// A find's answer, or why a call failed.
    fn strength_found(&mut self, ok: bool, text: &str) {
        let view = &mut self.state.ui.strength;
        if ok {
            view.found(text);
        } else {
            view.busy = None;
            view.note = model_host::reason(text);
        }
    }

    /// A new slice: a list read from another is stale.
    fn strength_follow(&mut self) {
        if !self.state.ui.strength.rows.is_empty() && self.strength_call.is_none() && live_slice::latest_toolpaths_key() != self.strength_key {
            self.state.ui.strength.stale = true;
        }
    }

    /// The overhang list follows the plate's whole toolpaths: it goes with them, and the Slice
    /// panel asks for the newest ones' once.
    fn overhang_list_follow(&mut self) {
        let latest = live_slice::latest_toolpaths_key();
        if self.state.print.overhang_list.is_some() && latest != self.overhang_list_key {
            self.state.print.overhang_list = None;
            self.overhang_list_key = None;
        }
        if self.support_list_visible() && latest.is_some() && latest != self.overhang_list_asked {
            if let (Some(key), Some(host)) = (latest, &mut self.host) {
                host.overhang_list(key);
                self.overhang_list_asked = latest;
            }
        }
    }

    /// Slices are wanted while something shows them, or while the AI, Mechanics' check of the
    /// supports or a Mechanics sim of field parts waits for the plate's toolpaths (the slice the
    /// Slice view shows, never one of its own). Otherwise slicing does no work: Print and Export
    /// ask for their own slice, and the printability checks have their own engine.
    fn slice_wanted(&self) -> bool {
        self.slice_shown() || live_slice::ai_waiting() || (!self.resin_printer() && self.state.ui.in_mode(PlateMode::Mechanics) && (self.state.mech.scene == fab::api::MechScene::Supports || self.mech.waits_for_slice()))
    }

    /// Why the plate it holds has no live slice coming, when it has none: what an AI wait for
    /// the plate's toolpaths hears.
    fn slice_refusal(&self) -> Option<String> {
        if self.resin_printer() {
            return Some("this printer prints resin: its slice has no filament supports".into());
        }
        if self.plate.as_ref().is_none_or(|ev| ev.plate.parts.is_empty()) {
            return Some("there is nothing on the plate to slice".into());
        }
        match &self.state.print.status {
            fab::print::SliceStatus::Failed(why) => Some(why.clone()),
            _ => None,
        }
    }

    /// The render mode or workspace may have changed: the slice view is kept, the scene follows,
    /// and slicing starts or stops with what shows it.
    fn slice_view_follow(&mut self, cx: &mut Cx) {
        self.sync_test_view(cx);
        let active = self.state.print.play.active;
        // Mechanics has no slice view and Monitor's is its own.
        if !matches!(self.state.ui.mode(), Some(PlateMode::Mechanics | PlateMode::Monitor)) && active != self.saved_slice_view {
            self.saved_slice_view = active;
            fab::view_prefs::save_slice(active);
        }
        self.sync_scene(cx, false);
        // Live tiles serve whatever shows a slice, the background too. A headset keeps its own handle.
        let live = self.slice_shown();
        if live != self.live_view.is_some() {
            self.live_view = live.then(live_slice::LiveSlice::start);
            if !live {
                self.state.print.live_contours = None;
            }
        }
        self.overhang_list_follow();
        let wanted = self.slice_wanted();
        // What waits for the toolpaths without a view still has the slice made as shown work.
        if (wanted && !live) != self.toolpaths_wanted.is_some() {
            self.toolpaths_wanted = (wanted && !live).then(live_slice::ToolpathsWanted::start);
        }
        if wanted == self.slice_was_wanted {
            return;
        }
        self.slice_was_wanted = wanted;
        if wanted {
            let p = &self.state.print;
            if (p.preview.is_none() || p.stale || !p.settled) && !matches!(p.status, fab::print::SliceStatus::Running { .. }) {
                self.request_reslice();
            }
        } else {
            if matches!(self.state.print.status, fab::print::SliceStatus::Running { .. }) && !self.printing.slice_asked() {
                self.printing.stop_slice(&mut self.state.print);
                self.state.print.bump();
            }
            // A refusal said why it would not slice; now nothing is trying to.
            if self.printing.machine().is_ok() && matches!(self.state.print.status, fab::print::SliceStatus::Failed(_)) {
                self.state.print.status = fab::print::SliceStatus::Idle;
                self.state.print.bump();
            }
        }
    }

    /// The scene the view shows off the bed: the Test prints view's, or the plate's.
    fn shown_scene(&self) -> Option<Arc<fab::model::Scene>> {
        self.test_view.scene.clone().filter(|_| self.state.plate.whole.is_some()).or_else(|| self.model_scene.clone())
    }

    /// Show the bed alone or the whole plate, as the preview wants.
    fn sync_scene(&mut self, cx: &mut Cx, frame: bool) {
        let bed = self.wants_bed_only();
        if bed == self.showing_bed && !frame {
            return;
        }
        let scene = if bed {
            let machine = self.plate.as_ref().map(|ev| &ev.machine);
            let Some(machine) = machine else { return };
            self.bed_scene.get_or_insert_with(|| plate_scene::build_bed(machine, &self.status.name)).clone()
        } else {
            match self.shown_scene() {
                Some(s) => s,
                None => return,
            }
        };
        self.showing_bed = bed;
        cx.action(ShellAction::SceneUpdated { scene, frame });
    }

    /// A Print editor action: settings and Slice to `printing`, playback to the shell.
    /// The Slicer slices what's chosen: the whole plate, or the ticked test prints as they stand now.
    fn sync_tests(&mut self) {
        let tests = self.state.print.test_mode.then(|| self.ticked_tests());
        if self.printing.set_tests(&mut self.state.print, self.plate.as_ref(), tests) {
            self.state.print.bump();
        }
    }

    /// The ticked test prints as they stand now.
    fn ticked_tests(&self) -> Vec<piece::TestPrint> {
        self.state.test_prints.iter().filter(|t| t.on).map(|t| piece::TestPrint {
            name: t.name.clone(),
            cube: piece::Cube { center: t.center, size: t.size, turn: t.turn },
            pose: t.lay,
            brim: t.brim,
        }).collect()
    }

    /// Test prints in Design: the view shows the ticked pieces where they print, in Normal and
    /// Sliced alike, cut off the UI thread; else the plate, at once.
    fn sync_test_view(&mut self, cx: &mut Cx) {
        let wanted = self.state.print.test_mode && self.state.ui.in_mode(PlateMode::Design) && !self.state.force_shown() && !self.previewing;
        let Some(ev) = self.plate.clone().filter(|_| wanted) else {
            self.test_view.asked = None;
            self.test_view.scene = None;
            if let Some(whole) = self.state.plate.whole.take() {
                self.plate_revision += 1;
                self.state.plate.parts = Arc::unwrap_or_clone(whole);
                self.state.plate.revision = self.plate_revision;
                self.show_eyes();
                self.state.sync_plate_elements();
                // Another workspace shows a scene of its own; Design the plate's, as it was left.
                if let Some(scene) = self.model_scene.clone().filter(|_| self.state.ui.in_mode(PlateMode::Design) && !self.wants_bed_only()) {
                    let kept = std::mem::take(&mut self.test_view.selected);
                    let picked: Vec<_> = scene.elements.iter().filter(|e| kept.contains(&e.name)).map(|e| e.id).collect();
                    cx.action(ShellAction::SceneUpdated { scene, frame: false });
                    if !picked.is_empty() {
                        cx.action(ShellAction::SelectSet(picked));
                    }
                }
                self.ui.redraw(cx);
            }
            return;
        };
        let tests = self.ticked_tests();
        if self.test_view.asked.as_ref().is_some_and(|a| a.content == ev.content && a.view == ev.view && a.tests == tests && a.machine.same_volume(&ev.machine)) {
            return;
        }
        self.test_view.asked = Some(TestViewOf { content: ev.content, view: ev.view, tests: tests.clone(), machine: ev.machine.clone() });
        self.test_view.gen += 1;
        let gen = self.test_view.gen;
        // A cube that cuts nothing shows nothing: the Slicer says why.
        let plan = piece::test_plan(&ev.plate, &ev.machine, &tests).unwrap_or(piece::TestPlan { reached: Vec::new(), shifts: Vec::new() });
        let name = self.status.name.clone();
        live_slice::run_shown("fab test view", move || {
            let t = std::time::Instant::now();
            let plate = fab_ai::test_view::plate(&ev, &tests, &plan, true);
            let scene = plate_scene::build(&plate, &name);
            log!("fab test print: the view's {} parts in {:.0} ms", plate.plate.parts.len(), t.elapsed().as_secs_f64() * 1e3);
            Cx::post_action(TestViewMade { gen, plate, scene });
        });
    }

    /// The newest pieces made are what the view shows.
    fn show_test_view(&mut self, cx: &mut Cx, made: &TestViewMade) {
        if made.gen != self.test_view.gen || self.test_view.asked.is_none() {
            return;
        }
        self.plate_revision += 1;
        let pieces = plate_edit::plate_info(None, &made.plate, self.plate_revision).parts;
        if self.state.plate.whole.is_none() {
            // What was picked on the plate is picked again on the way back.
            let s = &self.state;
            self.test_view.selected = s.scene_state.selection.set.iter().filter_map(|e| s.scene.element(*e)).map(|e| e.name.clone()).collect();
            self.state.plate.whole = Some(Arc::new(std::mem::replace(&mut self.state.plate.parts, pieces)));
        } else {
            self.state.plate.parts = pieces;
        }
        self.state.plate.revision = self.plate_revision;
        self.show_eyes();
        self.state.sync_plate_elements();
        self.test_view.scene = Some(made.scene.clone());
        self.test_view.revision = self.plate_revision;
        if !self.wants_bed_only() {
            cx.action(ShellAction::SceneUpdated { scene: made.scene.clone(), frame: false });
        }
        self.ui.redraw(cx);
    }

    fn print_action(&mut self, cx: &mut Cx, a: &fab::print::PrintAction) -> bool {
        use fab::print::PrintAction as A;
        if let A::TestMode(on) = a {
            self.state.print.test_mode = *on;
            if let Some(host) = &mut self.host {
                host.test_mode(*on);
            }
            log!("fab test print: {}", if *on { "the test prints slice" } else { "the whole plate slices" });
            makepad_error_log::flight::mark_ab("fab.test_print.mode", if *on { "test prints" } else { "whole plate" }, self.state.test_prints.iter().filter(|t| t.on).count() as u64, 0);
            self.sync_tests();
            self.request_reslice();
            fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_viewport), ids!(tab_slice), ids!(tab_settings)]);
            return true;
        }
        if matches!(a, A::OpenSupportCheck) {
            self.dispatch(cx, &ShellAction::SetWorkspace(Workspace::Mechanics));
            let building = self.host.as_ref().is_some_and(|h| h.building());
            self.mech.action(cx, &fab::api::MechAction::Scene(fab::api::MechScene::Supports), &mut self.state, self.plate.as_ref(), building);
            self.slice_view_follow(cx);
            return true;
        }
        // a resin printer's export is its layer stack
        if matches!(a, A::Export) && self.state.print.resin.is_some() {
            self.resin.ask_export();
            return true;
        }
        if self.resin_print_action(cx, a) {
            return true;
        }
        if matches!(a, A::Slice) || (matches!(a, A::Export | A::AskPrint(true)) && !self.state.print.can_send()) {
            // A live input settles verbatim; otherwise slice the committed plate.
            self.focus_slice();
        }
        if matches!(a, A::SendToPrinter) {
            self.printing.plate_scripts = self.scripts.clone();
        }
        let mut changed = self.printing.action(&mut self.state.print, a, self.plate.as_ref());
        if changed { self.state.print.bump(); }
        let printer_only = matches!(a, A::AskPrint(_) | A::AskStop(_) | A::SetPrintOption(..) | A::SendToPrinter | A::CancelSend | A::CloseJob | A::PrinterControl(_) | A::PickJob(_) | A::AskStartJob(_) | A::StartJob(_) | A::RemoveJob(_));
        // Start print went: Monitor shows it from here on.
        if matches!(a, A::SendToPrinter) && self.state.print.sending.is_some() {
            cx.action(ShellAction::SetWorkspace(Workspace::Monitor));
        }
        if changed && !printer_only && !matches!(a, A::Slice | A::Export) {
            self.state.print.stale = self.state.print.preview.is_some();
            self.state.print.stale_parts = None;
            if let Some(host) = &mut self.host { host.print_setup(self.printing.check_setup(&self.state.print)); }
            live_slice::set_print_setup(self.printing.check_setup(&self.state.print));
            self.request_reslice();
        }
        if changed { self.remap(); }
        // Every issue entry point navigates the plate, including from a held Monitor job.
        if let A::SelectIssue(index) = a {
            if self.state.print.report().issues.get(*index).is_some() {
                if self.state.print.monitor_holds() {
                    self.dispatch(cx, &ShellAction::SetWorkspace(Workspace::Design));
                }
                self.dispatch(cx, &ShellAction::SetRenderMode(fab::api::RenderMode::Slice));
            }
        }
        changed |= fab::print::apply(&mut self.state.print, a);
        if let fab::print::PrintAction::Overhangs(on) = a {
            if let Some(host) = &mut self.host { host.overhangs(*on); }
        }
        if let fab::print::PrintAction::SelectIssue(index) = a {
            if let Some(issue) = self.state.print.report().issues.get(*index) {
                let b = issue.footprint;
                let z = issue.at_mm[2] as f32 * 0.001;
                let min = vec3(b.min[0] as f32 * 0.001 - 0.003, b.min[1] as f32 * 0.001 - 0.003, z - 0.003);
                let max = vec3(b.max[0] as f32 * 0.001 + 0.003, b.max[1] as f32 * 0.001 + 0.003, z + 0.003);
                cx.action(ShellAction::FrameBox(fab::Aabb { min, max }));
            }
        }
        if matches!(a, fab::print::PrintAction::Play(true)) {
            self.print_frame_time = None;
            self.print_frame = cx.new_next_frame();
        }
        self.sync_scene(cx, false);
        changed
    }

    /// The Print editor's actions on a resin printer, or on its job: its print goes over its own
    /// link (`sla::ResinView`). False when the action isn't one of these.
    fn resin_print_action(&mut self, cx: &mut Cx, a: &fab::print::PrintAction) -> bool {
        use fab::print::PrintAction as A;
        let (resin, job) = (self.state.print.resin.is_some(), self.state.print.resin_job.is_some());
        match a {
            A::AskPrint(true) if resin => self.resin.look_for_printer(),
            A::AskPrint(false) | A::AskStop(_) | A::AcceptErrors(_) | A::PickStick(_) if resin || job => {}
            A::SaveOrientationTest if resin => self.resin.ask_test(),
            A::SendToPrinter if resin => {
                self.resin.ask_print();
                // Start print went: Monitor shows it from here on
                cx.action(ShellAction::SetWorkspace(Workspace::Monitor));
            }
            A::PrinterControl(control) if job => self.resin.control(*control),
            A::CloseJob if job => self.resin.close_job(&mut self.state.print),
            _ => return false,
        }
        fab::print::apply(&mut self.state.print, a);
        self.state.print.bump();
        cx.redraw_all();
        true
    }

    /// A job Monitor comes to show fills its view, once: its camera is the person's from then on.
    fn frame_shown_job(&mut self, cx: &mut Cx) {
        let p = &self.state.print;
        let Some(job) = p.monitor_preview().and(p.follow.shown()) else { return };
        if self.framed_job.as_deref() == Some(job.meta.id.as_str()) {
            return;
        }
        let bounds = p.monitor_preview().map(|pv| pv.bounds);
        self.framed_job = Some(job.meta.id.clone());
        if let Some(bounds) = bounds {
            let at = |p: [f32; 3]| vec3(p[0] * 0.001, p[1] * 0.001, p[2] * 0.001);
            cx.action(ShellAction::FrameBox(fab::Aabb { min: at(bounds[0]), max: at(bounds[1]) }));
        }
    }

    /// The headset's edits: a finished grab goes to the model worker as one undo step, the
    /// same path the desktop's edits take; undo and redo are the desktop's.
    fn xr_action(&mut self, cx: &mut Cx, a: &xr::FabXrAction) {
        match a {
            xr::FabXrAction::Place(moves) => {
                let Some(host) = &mut self.host else {
                    if let Some(mut view) = self.ui.widget(cx, ids!(xr)).borrow_mut::<xr::FabXr>() {
                        view.failed_unsent();
                    }
                    return;
                };
                let id = host.place(moves.clone());
                if let Some(mut view) = self.ui.widget(cx, ids!(xr)).borrow_mut::<xr::FabXr>() {
                    view.sent(id);
                }
            }
            xr::FabXrAction::Undo => self.history_step(cx, true),
            xr::FabXrAction::Redo => self.history_step(cx, false),
        }
    }

    /// Once a headset is served, the worker prepares every rebuilt plate for it too.
    fn pump_xr(&mut self, cx: &mut Cx) {
        let active = self.ui.widget(cx, ids!(xr)).borrow::<xr::FabXr>().is_some_and(|v| v.active());
        if let Some(host) = &mut self.host {
            host.set_xr(active);
        }
    }

    /// Run the review renderer: start a queued request, collect frames.
    fn pump_review(&mut self, cx: &mut Cx) {
        let widget = self.ui.widget(cx, ids!(review));
        let Some(mut renderer) = widget.borrow_mut::<review::FabReviewRenderer>() else { return };
        if let Some(captured) = renderer.poll(cx) {
            match captured.result {
                Ok(frames) => {
                    if let Some(job) = self.compare_jobs.remove(&captured.job) {
                        self.after_views.insert(captured.job, "comparing with the picture");
                        model_host::compare(&cx.task_pool(), captured.job, frames, job);
                    } else if let Some(run) = self.variant_runs.get_mut(&captured.job) {
                        if let Some(round) = run.captured(frames) {
                            if round.last {
                                self.variant_runs.remove(&captured.job);
                            }
                            self.after_views.insert(captured.job, if round.last { "laying out the sheets" } else { "fitting the view to the picture" });
                            variant_sheet::read(&cx.task_pool(), captured.job, round);
                        }
                    } else if let Some(host) = &mut self.host {
                        self.after_views.insert(captured.job, "writing the sheets");
                        host.encode_sheets(captured.job, frames, self.img2mesh_answers.contains_key(&captured.job));
                    }
                }
                Err(e) => {
                    // the call's other captures go with it
                    self.compare_jobs.remove(&captured.job);
                    self.variant_runs.remove(&captured.job);
                    self.review_queue.retain(|(job, _)| *job != captured.job);
                    self.calls.finish(captured.job, chat::Answer { ok: false, text: format!("{{\"error\":{:?}}}", e), images: Vec::new() })
                }
            }
        }
        if !renderer.busy() {
            if let Some((job, prepared)) = self.review_queue.pop_front() {
                renderer.start(cx, job, prepared);
            }
        }
        self.drawing = renderer.progress();
        let pending = &self.calls.pending;
        self.after_views.retain(|id, _| pending.contains_key(id));
    }

    /// File > New Plate: the plate on the bed is saved (it stays in Open Recent), an empty one
    /// takes its place.
    fn new_plate(&mut self) {
        self.bed_asked();
        self.send_typing();
        self.model_command("new", &[], false);
    }

    /// A demo plate, by key ([`demo_part::DEMOS`]).
    fn open_demo(&mut self, key: &str) {
        let Some(demo) = demo_part::demo(key) else {
            log!("fab: no demo {key}");
            return;
        };
        self.bed_asked();
        self.send_typing();
        if let Some(host) = &mut self.host {
            let id = host.demo(demo);
            self.demo_card = Some((demo.key, id, false));
        }
    }

    /// A demo with electronics documents opens in an electronics workspace, with its Scope in
    /// front when it has a bench (the wave view of the bench it plays as soon as it evaluates;
    /// user, 11-18: the Scope first), else its firmware.
    fn show_demo_electronics(&mut self, cx: &mut Cx, key: &str) {
        use fab_elec_lang::DocKind;
        let Some(demo) = demo_part::demo(key) else { return };
        let kinds: Vec<DocKind> = demo.files.iter().filter_map(|(file, _)| DocKind::of_name(file)).collect();
        let panel = if kinds.contains(&DocKind::Bench) {
            fab::ui::editors::Editor::Scope
        } else if kinds.contains(&DocKind::Firmware) {
            fab::ui::editors::Editor::Firmware
        } else if !kinds.is_empty() {
            fab::ui::editors::Editor::Circuit
        } else {
            return;
        };
        cx.action(ShellAction::SetWorkspace(Workspace::Electronics));
        cx.action(fab::ui::shell::ShowPanelIn(Workspace::Electronics, panel));
    }

    /// Change editor files without selecting a part. Finish the outgoing typing burst and
    /// invalidate its preview before another tab can receive its answer.
    fn view_source(&mut self, cx: &mut Cx, file: &str) {
        if self.state.source.viewed.as_deref() == Some(file) || !self.state.source.tabs.iter().any(|t| t.file == file) {
            return;
        }
        self.commit_source(cx);
        if let Some(host) = &mut self.host {
            self.preview_floor = host.cancel_preview();
        }
        self.restore_committed(cx);
        self.param_action(cx, &fab::params::ParamAction::Hover(None));
        self.state.source.viewed = Some(file.to_string());
        self.state.source.rev += 1;
        self.state.source.editing = self.state.source.tab().is_some_and(|t| t.draft.is_some());
        cx.redraw_all();
    }

    /// Only a changed viewport/list selection resets tab focus. The feature tree keeps
    /// following that selection even while the editor shows the plate or another file.
    fn follow_selection(&mut self, cx: &mut Cx) {
        // trees missing with a program built (a restart that kept the program): made once
        if self.state.source.trees.is_empty() && self.program.is_some() {
            self.publish_trees();
        }
        let selected = self.selected_objects();
        let changed = selected != self.source_selection;
        self.source_selection = selected;
        let inst = self.program.as_ref().and_then(|(p, _)| {
            self.source_selection.first().and_then(|o| p.object(o)).map(|(i, _)| i)
                .or_else(|| p.instance(&self.state.source.instance))
                .or_else(|| p.instances.first())
        });
        let file = inst.map(|i| i.script.file.clone()).unwrap_or_default();
        let instance = inst.map(|i| i.name.clone()).unwrap_or_default();
        if file != self.state.source.file || instance != self.state.source.instance {
            self.state.source.file = file.clone();
            self.state.source.instance = instance;
            self.state.source.text = Arc::new(self.scripts.get(&file).cloned().unwrap_or_default());
            self.state.source.features = inst.map(code_edit::rows).unwrap_or_default();
            self.drop_ghost();
            self.state.source.selected = None;
            fab::ui::test_prints::show_rows(&mut self.state);
            self.state.source.problem = self.state.source.tabs.iter().find(|t| t.file == file).and_then(|t| t.problem.clone());
            let prefix = format!("{}/", self.state.source.instance);
            self.state.source.sites = self.committed_params.iter().filter(|p| p.object.starts_with(&prefix)).flat_map(|p| p.params.iter()).filter_map(|p| p.site.clone().map(|r| (r, p.key.clone()))).collect();
            if let Some(tab) = self.state.source.tabs.iter_mut().find(|t| t.file == file) {
                tab.sites = self.state.source.sites.clone();
            }
            cx.redraw_all();
        }
        if changed {
            let focused = if self.source_selection.is_empty() { self.state.source.tabs.first().map(|t| t.file.clone()) } else { Some(file) };
            if let Some(file) = focused {
                self.view_source(cx, &file);
            }
        }
    }

    /// The edited plate goes to whatever wants a slice now. Print, Export or Slice asks for the settle.
    fn request_reslice(&mut self) {
        if let Some(mapped) = &self.mapping_sent {
            if self.plate.as_ref().is_some_and(|ev| *ev.mapped != **mapped) {
                return;
            }
            self.mapping_sent = None;
        }
        if self.slice_wanted() {
            if self.live_view.is_none() && self.slice_shown() { self.live_view = Some(live_slice::LiveSlice::start()); }
            self.focus_slice();
            self.printing.live_input(&mut self.state.print, self.plate.as_ref(), Arc::from([]), None);
            self.state.print.bump();
        }
        // A slice Print, Export or Slice waited for went with the edit: it goes on with this plate.
        if self.printing.resume_asked(&mut self.state.print, self.plate.as_ref()) {
            self.state.print.bump();
        }
    }

    /// Brush strokes in the slice view. Sculpt strokes reslice as they are drawn: their live
    /// fields in place of the committed ones, the brush's layers first; their landing (or a
    /// cancel) slices the committed plate. A paint stroke reslices once it lands (its band
    /// first); while one is drawn, slicing still in flight goes to the brush's layers first.
    fn follow_brush(&mut self, cx: &mut Cx) {
        let revision = fab::tools::sculpt::revision();
        if revision != self.sculpt_seen && self.state.print.play.active {
            // A stroke being drawn is a gesture: its viewed layers stay shown through newer inputs.
            live_slice::set_drag_live(fab::tools::sculpt::brush().is_some_and(|(_, drawing)| drawing));
            // the view's live fields now, from this event's points (its draw finds them made)
            if let Some(layer) = &self.state.sdf {
                layer.borrow_mut().set_sculpt(&self.state);
            }
            self.slice_live_fields(cx);
        }
        self.sculpt_seen = revision;
        let z = self.brush_z();
        if z.is_some() && z != self.brush_focus {
            self.focus_slice();
        }
        self.brush_focus = z;
    }

    /// A part moving under the slice sends its placement as a live input. Bed translations
    /// share the part's cuts and plans; the end of the gesture sends the committed plate.
    fn follow_part_move(&mut self, cx: &mut Cx) {
        let nudge = self.state.scene_state.nudge.filter(|_| self.live_view.is_some());
        if nudge == self.live_nudge {
            return;
        }
        self.live_nudge = nudge;
        let Some((element, pose)) = nudge else {
            self.request_reslice();
            return;
        };
        let Some(part) = self.state.plate.part_of(element).filter(|i| self.state.plate.parts[*i].printed) else { return };
        // Metres to millimetres.
        let m: [[f64; 4]; 3] = std::array::from_fn(|r| std::array::from_fn(|c| if c == 3 { pose[r][c] as f64 * 1000.0 } else { pose[r][c] as f64 }));
        self.focus_slice();
        self.printing.live_input(&mut self.state.print, self.plate.as_ref(), Arc::from([]), Some((part, m)));
        self.state.print.bump();
        // Live placement affects the view, placement/slice summaries and slicing status.
        fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_viewport), ids!(tab_part), ids!(tab_slice), ids!(tab_settings), ids!(tab_printer), ids!(caption), ids!(xr)]);
    }

    /// One batch of model operations as one undoable transaction, against
    /// the head the view shows.
    fn selected_objects(&self) -> Vec<String> {
        let s = &self.state;
        s.scene_state.selection.set.iter().filter_map(|e| s.plate.part_of(*e)).map(|i| s.plate.parts[i].object.clone()).collect()
    }

    /// A direct plate edit from the panels, the gizmo or the keys.
    fn img2mesh_event(&mut self, cx: &mut Cx, e: &img2mesh::Img2MeshEvent) {
        use img2mesh::Img2MeshEvent as E;
        match e {
            E::Progress { job, stage, permille, node } => {
                let Some(r) = self.img2mesh.get_mut(*job) else { return };
                if r.stage != "cancelling" {
                    r.stage = stage.clone();
                    let stage = makepad_ai_services::progress_eta::plain_stage(stage);
                    if r.stages.last() != Some(&stage) {
                        r.stages.push(stage);
                        if r.stages.len() > 12 {
                            r.stages.remove(0);
                        }
                    }
                }
                r.permille = *permille;
                if node.is_some() {
                    r.node = node.clone();
                }
            }
            E::Made { job, made } => {
                let Some(r) = self.img2mesh.finish(*job) else { return };
                let Some(made) = made.lock().ok().and_then(|mut m| m.take()) else { return };
                let Some(host) = &mut self.host else { return };
                let id = r.call.unwrap_or_else(|| host.reserve_call_id());
                if let Some(call) = r.call {
                    let mut card = r.task();
                    card.stage = "putting it on the plate".into();
                    self.calls.tasks.insert(call, card);
                }
                host.import_mesh(id, made, r.call.is_some());
            }
            E::Failed { job, error, cancelled } => {
                let Some(r) = self.img2mesh.finish(*job) else { return };
                if let Some(call) = r.call {
                    if *cancelled {
                        self.calls.pending.remove(&call);
                    } else {
                        let mut card = r.task();
                        card.stage = "failed".into();
                        card.result = error.lines().next().unwrap_or_default().to_string();
                        self.calls.tasks.insert(call, card);
                        self.calls.finish(call, chat::Answer { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(error)), images: Vec::new() });
                    }
                }
            }
        }
        cx.redraw_all();
    }

    fn part_action(&mut self, cx: &mut Cx, a: &PartAction) {
        self.startup_monitor_done = true;
        let deletion = match a {
            PartAction::Delete { object } => Some((std::slice::from_ref(object), false)),
            PartAction::DeleteScript { object } => Some((std::slice::from_ref(object), true)),
            PartAction::DeleteParts { objects } => Some((objects.as_slice(), false)),
            PartAction::DeleteScripts { objects } => Some((objects.as_slice(), true)),
            _ => None,
        };
        if let Some((objects, scripts)) = deletion {
            if objects.is_empty() { return; }
            if self.state.ui.in_mode(PlateMode::Mechanics) || self.state.slice_blocks_edits() {
                self.state.ui.notices.say(NoticeView::Plate, "Switch to Design to delete parts.".into());
            } else if let Some(host) = &mut self.host {
                let id = host.delete_parts(objects.to_vec(), scripts);
                self.delete_jobs.insert(id);
                self.preview_floor = host.cancel_preview();
            } else {
                self.state.ui.notices.say(NoticeView::Plate, "The plate is not ready to delete parts yet.".into());
            }
            cx.redraw_all();
            return;
        }
        match a {
            PartAction::Select { index, toggle } => {
                if let Some(e) = self.state.plate.parts.get(*index).and_then(|p| p.element) {
                    cx.action(if *toggle { ShellAction::SelectToggle(e) } else { ShellAction::SelectOnly(e) });
                }
            }
            PartAction::NoTrees { area } => {
                self.no_trees_edit(*area);
                cx.redraw_all();
            }
            PartAction::SettingsHere { area, z } => {
                // the box goes on the picked part
                let picked = fab::ui::parts::active_part(&self.state).and_then(|i| self.state.plate.parts.get(i)).map(|p| p.object.clone());
                let edit = match (&self.program, &self.plate, picked) {
                    (_, _, None) => Err(format!("Print settings here: {}, the box goes on it", fab::ui::stage::SELECT_A_PART)),
                    (Some((program, _)), Some(ev), Some(object)) => shape_edit::settings_on_plate(program, ev, &object, *area, *z),
                    _ => Err("Print settings here: the plate is still being built; draw the volume again once its parts show".to_string()),
                };
                self.settings_here_edit(edit, &format!("{area:?} at z {z:.2}"));
                cx.redraw_all();
            }
            PartAction::SettingsVolume { object, feature, shape } => {
                let Some(head) = self.status.head else {
                    self.state.ui.notices.say(NoticeView::Plate, "Print settings: the plate is still being built; ask again once its parts show".to_string());
                    return;
                };
                let mut args = vec![("shape", json::s(*shape)), ("expected", json::s(head.text()))];
                if let Some(object) = object {
                    args.push(("object", json::s(object)));
                }
                if let Some(feature) = feature {
                    args.push(("feature", json::s(feature)));
                }
                if let Some(host) = &mut self.host {
                    self.volume_call = Some(host.call(fab_ai::volumes::TOOL, json::obj(args)));
                }
                cx.redraw_all();
            }
            PartAction::PickArea { object } => self.pick_area(cx, object),
            PartAction::DeleteArea { object } => {
                let Some((instance, solid)) = fab_model::program::split_id(object) else { return };
                let Some(inst) = self.program.as_ref().and_then(|(p, _)| p.instance(instance)) else { return };
                let edit = fab_model::Edit::Remove { solid: solid.to_string() };
                let change = fab_model::Change::Script { file: inst.script.file.clone(), edit };
                if let Some(host) = &mut self.host {
                    host.edit(vec![change]);
                }
                if self.state.source.selected.as_deref() == Some(solid) {
                    self.state.source.selected = None;
                    self.drop_ghost();
                }
                cx.redraw_all();
            }
            PartAction::SetVisible { objects, visible } => {
                let objects: std::collections::HashSet<_> = objects.iter().map(String::as_str).collect();
                for part in &self.state.plate.parts {
                    if let Some(e) = part.element.filter(|_| objects.contains(part.object.as_str())) {
                        self.state.scene_state.set_hidden(e, !*visible);
                    }
                }
                cx.redraw_all();
            }
            PartAction::Preview(_) => {}
            PartAction::MakePrintable { object } => {
                if let Some(host) = &mut self.host { host.make_printable(object); }
            }
            _ => {
                let Some(ev) = self.plate.clone() else { return };
                let paints_tray = matches!(a, PartAction::AssignTray { paint, .. } if !paint.is_empty());
                if paints_tray || matches!(a, PartAction::SetPaintColor { .. } | PartAction::PaintStroke { .. } | PartAction::SculptStroke { .. } | PartAction::PaintFill { .. } | PartAction::PaintFlood { .. } | PartAction::PaintPattern { .. } | PartAction::Study { .. } | PartAction::ResetAreaSetting { .. }) {
                    let frame = match a {
                        PartAction::PaintStroke { object, .. } | PartAction::PaintFlood { object, .. } | PartAction::PaintPattern { object, .. } | PartAction::Study { object, .. } => ev.plate.placed_parts().position(|p| p.object == *object).and_then(|i| ev.placed_frame(i)).copied(),
                        _ => None,
                    };
                    if let Some(host) = &mut self.host {
                        let job = host.paint_edit(a.clone(), frame);
                        match a {
                            PartAction::Study { .. } => self.study_job = Some(job),
                            PartAction::SculptStroke { .. } => self.sculpt_jobs.push_back(job),
                            PartAction::ResetAreaSetting { .. } => self.reset_job = Some(job),
                            _ => self.paint_job = Some(job),
                        }
                    }
                    return;
                }
                if let PartAction::Shape { object, op } = a {
                    self.shape_edit(&ev, object, *op);
                    cx.redraw_all();
                    return;
                }
                // Moves share the headset's edit path; everything else is a model batch.
                if let Some(moves) = plate_edit::moves(&ev, a) {
                    if let (false, Some(host)) = (moves.is_empty(), &mut self.host) {
                        self.place_job = Some(host.place(moves));
                    }
                    return;
                }
                let Some((program, _)) = &self.program else { return };
                let edits = match plate_edit::edits(program, &ev, a) {
                    Ok(edits) => edits,
                    Err(why) => {
                        self.state.ui.notices.say(NoticeView::Plate, why);
                        cx.redraw_all();
                        return;
                    }
                };
                if let (false, Some(host)) = (edits.is_empty(), &mut self.host) {
                    let job = host.edit(edits);
                    if let PartAction::SetPrint { objects, print } = a {
                        self.eyes.extend(objects.iter().map(|object| (job, object.clone(), *print)));
                        self.show_eyes();
                        self.plate_revision += 1;
                        self.state.plate.revision = self.plate_revision;
                        cx.redraw_all();
                    }
                    // the eye answers at once; the evaluation's own flags follow with the edit
                    if let PartAction::SetHidden { objects, hidden } = a {
                        for part in self.state.plate.parts.iter_mut().filter(|p| objects.contains(&p.object)) {
                            part.hidden = *hidden;
                            if let Some(e) = part.element {
                                self.state.scene_state.set_hidden(e, *hidden);
                            }
                        }
                        self.plate_revision += 1;
                        self.state.plate.revision = self.plate_revision;
                        cx.redraw_all();
                    }
                }
            }
        }
    }

    /// The eyes on their way to the model, on the plate as shown.
    fn show_eyes(&mut self) {
        for (_, object, print) in &self.eyes {
            if let Some(p) = self.state.plate.parts.iter_mut().find(|p| p.object == *object) {
                p.printed = *print;
            }
        }
    }

    /// A shape edit of `object`'s script on the feature picked in the tree (none picked, or one
    /// of another script: the object's own solid); the tool it adds is picked once it is built.
    fn shape_edit(&mut self, ev: &fab_model::eval::Evaluated, object: &str, op: fab::api::ShapeOp) {
        let Some((program, _)) = &self.program else { return };
        let Some((inst, _)) = program.object(object) else { return };
        let Some((_, own)) = fab_model::program::split_id(object) else { return };
        // A cut through the whole part goes on the object; add and subtract go on the selected node.
        let whole = matches!(op, fab::api::ShapeOp::CutBottom | fab::api::ShapeOp::CutPlane | fab::api::ShapeOp::Split);
        let target = self.state.source.selected.clone().filter(|_| !whole && self.state.source.instance == inst.name).filter(|id| inst.script.program.solid(id).is_some()).unwrap_or_else(|| own.to_string());
        let Some(i) = ev.plate.placed_parts().position(|p| p.object == object) else { return };
        let (Some(frame), Some(part)) = (ev.placed_frame(i), ev.plate.placed_part(i)) else { return };
        let placed = shape_edit::Placed { frame: *frame, bounds: part.bounds() };
        match shape_edit::changes(inst, &target, op, &placed, &ev.machine.bed) {
            Ok((changes, tool)) => {
                if let Some(host) = &mut self.host {
                    host.edit(changes);
                    self.select_after = Some(tool);
                }
            }
            Err(e) => self.state.ui.notices.say(NoticeView::Plate, e),
        }
    }

    /// The marked area (no trees, print settings) picked in the tree (`instance/solid`).
    fn picked_area(&self) -> Option<String> {
        let id = self.state.source.selected.as_ref()?;
        let f = self.state.source.feature(id)?;
        [fab::ui::code::NO_TREES, fab::ui::code::SETTINGS_HERE].contains(&f.detail.as_str()).then(|| f.object.clone())
    }

    /// The print settings box picked, picked no more: its row, its drawing, its handles.
    fn unpick_box(&mut self, cx: &mut Cx) {
        if fab::ui::test_prints::picked(&self.state).is_some() {
            self.state.source.selected = None;
            cx.redraw_all();
            return;
        }
        let Some(object) = fab::ui::param_panel::picked_volume(&self.state) else { return };
        self.state.source.selected = None;
        self.drop_ghost();
        if self.state.params.focus.as_deref() == Some(object.as_str()) {
            self.param_action(cx, &fab::params::ParamAction::Focus(None));
        }
        cx.redraw_all();
    }

    /// A test print's row in the Test prints editor clicked: its part selected (the tree shows that
    /// script), and that one picked.
    fn pick_test_print(&mut self, cx: &mut Cx, name: &str) {
        let Some(object) = self.state.test_prints.iter().find(|t| t.name == name).map(|t| t.object.clone()) else { return };
        if let Some(e) = self.state.plate.parts.iter().find(|p| p.object == object).and_then(|p| p.element) {
            self.dispatch(cx, &ShellAction::SelectOnly(e));
        }
        self.follow_selection(cx);
        let id = fab::ui::code::FeatureRow::test_print(name, &object).id;
        self.source_action(cx, &fab::ui::code::SourceAction::Select { id, reveal: false });
    }

    /// What the Test prints editor's list, the fields and the gizmo ask of a test print: one undo step
    /// each, on the path `fab.test_print` takes.
    fn test_print_action(&mut self, cx: &mut Cx, a: &fab::api::TestPrintAction) {
        use fab::api::TestPrintAction as A;
        use fab_model::project::TestEdit;
        let set = |name: &str, center, size, turn, on, to| TestEdit::Set { name: name.to_string(), center, size, turn, pose: None, on, brim: None, to };
        let edit = match a {
            A::Pick { name } => return self.pick_test_print(cx, name),
            A::Add { object } => {
                let Some(part) = self.state.plate.parts.iter().find(|p| p.object == *object) else { return };
                let (lo, hi) = (part.min_mm, part.max_mm);
                let taken: Vec<String> = self.state.test_prints.iter().map(|t| t.name.clone()).collect();
                let name = fab_model::project::next_test_name(&taken);
                self.select_after = Some(fab::ui::code::FeatureRow::test_print(&name, object).id);
                let side = (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max).clamp(1.0, fab::ui::test_prints::NEW_SIDE);
                TestEdit::Add { object: object.clone(), center: std::array::from_fn(|k| 0.5 * (lo[k] + hi[k])), size: [side; 3], turn: [0.0; 3], name: Some(name), on: true, in_model: false }
            }
            // The pose goes with the box only when it differs from the plate's, so a box moved
            // while a lay flat is on its way does not put the old pose back.
            A::Place { name, center, size, turn, pose } => {
                let pose = (self.state.test_prints.iter().find(|t| t.name == *name).map(|t| t.pose) != Some(*pose)).then_some(*pose);
                TestEdit::Set { name: name.clone(), center: Some(*center), size: Some(*size), turn: Some(*turn), pose, on: None, brim: None, to: None }
            }
            A::Rename { name, to } => {
                if let Some(t) = self.state.test_prints.iter().find(|t| t.name == *name) {
                    if self.state.source.selected.as_deref() == Some(fab::ui::code::FeatureRow::test_print(name, &t.object).id.as_str()) {
                        self.select_after = Some(fab::ui::code::FeatureRow::test_print(to, &t.object).id);
                    }
                }
                set(name, None, None, None, None, Some(to.clone()))
            }
            A::Tick { name, on } => set(name, None, None, None, Some(*on), None),
            A::Delete { name } => TestEdit::Remove { name: name.clone() },
            A::Brim { name, on } => TestEdit::Set { name: name.clone(), center: None, size: None, turn: None, pose: None, on: None, brim: Some(*on), to: None },
            A::LayFlat { name, from, dir } => {
                let Some(host) = &mut self.host else {
                    self.state.ui.notices.say(NoticeView::Plate, "Test print: the plate is not ready for edits yet".into());
                    return;
                };
                log!("fab test print: lay flat {name:?} at {from:?} along {dir:?}");
                self.test_lay = Some(host.lay_test(name.clone(), *from, *dir));
                cx.redraw_all();
                return;
            }
        };
        let Some(host) = &mut self.host else {
            self.state.ui.notices.say(NoticeView::Plate, "Test print: the plate is not ready for edits yet".into());
            return;
        };
        log!("fab test print: {edit:?}");
        let id = host.test_prints(vec![edit]);
        if let A::Place { .. } = a {
            self.test_place = self.state.ui.test_drag.clone().map(|d| (id, d));
        }
        cx.redraw_all();
    }

    /// Every placed instance's feature tree, for the Parts outliner: O(solids) per committed
    /// program, never per frame.
    fn publish_trees(&mut self) {
        self.state.source.trees = self.program.as_ref().map_or_else(Default::default, |(p, _)| {
            p.instances.iter().map(|i| (i.name.clone(), Arc::new(code_edit::rows(i)))).collect()
        });
    }

    /// An outliner tree of another part pressed: a part of its instance is selected (unless one
    /// is), so the tree and the code follow that script before the press's own edit applies.
    fn focus_instance(&mut self, cx: &mut Cx, instance: &str) {
        if self.state.source.instance == instance {
            return;
        }
        let ours = |o: &str| fab_model::program::split_id(o).is_some_and(|(i, _)| i == instance);
        // the tree follows the first selected object: it must be one of this instance
        if !self.selected_objects().first().is_some_and(|o| ours(o)) {
            let Some(e) = self.state.plate.parts.iter().find(|p| ours(&p.object)).and_then(|p| p.element) else { return };
            self.dispatch(cx, &ShellAction::SelectOnly(e));
        }
        self.follow_selection(cx);
    }

    /// A marked area clicked in the Sliced view: a part of its script is selected (the tree
    /// shows that script) and its row picked, as a click on the row picks it.
    fn pick_area(&mut self, cx: &mut Cx, object: &str) {
        let Some((instance, solid)) = fab_model::program::split_id(object) else { return };
        let ours = |o: &str| fab_model::program::split_id(o).is_some_and(|(i, _)| i == instance);
        if !self.selected_objects().iter().any(|o| ours(o)) {
            let Some(e) = self.state.plate.parts.iter().find(|p| ours(&p.object)).and_then(|p| p.element) else { return };
            self.dispatch(cx, &ShellAction::SelectOnly(e));
        }
        self.follow_selection(cx);
        let id = solid.to_string();
        self.source_action(cx, &fab::ui::code::SourceAction::Select { id, reveal: true });
    }

    /// A no-trees area drawn on the bed: a solid of the script of the part it is for
    /// ([`shape_edit::no_trees_on_plate`]), marked by `m.no_trees`; the solid is picked once it is
    /// built. A refusal appears in the plate view.
    fn no_trees_edit(&mut self, area: fab::api::BedArea) {
        let edit = match (&self.program, &self.plate) {
            (Some((program, _)), Some(ev)) => shape_edit::no_trees_on_plate(program, ev, area),
            _ => Err("No trees here: the plate is still being built; draw the area again once its parts show".to_string()),
        };
        let (said, refused) = match (edit, &mut self.host) {
            (Ok((changes, solid, name)), Some(host)) => {
                host.edit(changes);
                self.select_after = Some(solid);
                (format!("No trees here: added to {name}; its supports plan again"), false)
            }
            (Ok(_), None) => ("No trees here: the plate is not ready for edits yet".to_string(), true),
            (Err(why), _) => (why, true),
        };
        log!("fab no trees: {area:?}: {said}");
        if refused {
            self.state.ui.notices.say(NoticeView::Plate, said);
        }
    }

    /// A print settings volume drawn on the shown layer (at plate height `z`): a solid of the
    /// script of the part it is for with `m.settings` ([`shape_edit::settings_on_plate`]); the
    /// solid is picked once it is built, its panel rows beside its size. A refusal appears in the plate view.
    /// A print settings volume's edit (drawn, or over a part from a menu, `what`): written, one
    /// undo step, and picked once the plate rebuilds; or why not, in the plate view.
    fn settings_here_edit(&mut self, edit: Result<(Vec<fab_model::Change>, String, String), String>, what: &str) {
        let (said, refused) = match (edit, &mut self.host) {
            (Ok((changes, solid, name)), Some(host)) => {
                host.edit(changes);
                self.select_after = Some(solid);
                (format!("Print settings here: added to {name}, a solid infill to start; it slices again"), false)
            }
            (Ok(_), None) => ("Print settings here: the plate is not ready for edits yet".to_string(), true),
            (Err(why), _) => (why, true),
        };
        log!("fab settings here: {what}: {said}");
        if refused {
            self.state.ui.notices.say(NoticeView::Plate, said);
        }
    }

    fn register_loaders(&mut self) {
        if self.loaders_registered {
            return;
        }
        self.loader.register(makepad_fab_loader_gltf::GltfLoader);
        self.loaders_registered = true;
    }

    /// The electronics host's newest body poses (a drag's, from its gesture) move the 3D bodies
    /// this frame, ahead of their scene.
    fn take_elec_poses(&mut self, cx: &mut Cx) {
        if let Some(poses) = self.elec.as_mut().and_then(|e| e.take_poses()) {
            self.state.elec_poses = Some(poses);
            fab::ui::redraw_plates(cx, &self.ui, &[ids!(viewport)]);
        }
    }
}

impl MatchEvent for App {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        // The status follows the engine's answer even if its action never arrived.
        if self.printing.reconcile(&mut self.state.print) {
            self.state.print.bump();
            cx.redraw_all();
        }
        let mut changed = false;
        let mut follow = false;
        for action in actions {
            if let Some(redraw) = action.downcast_ref::<fab::ui::Redraw>() {
                fab::ui::redraw_now(cx, &self.ui, redraw.0);
            }
            if let Some(a) = action.downcast_ref::<ShellAction>() {
                changed |= self.dispatch(cx, a);
                follow |= shows_workspace(a);
            }
            follow |= action.downcast_ref::<fab::print::PrintAction>().is_some() || action.downcast_ref::<print::PrintEvent>().is_some();
            if let Some(FileDialogAction::FileSelected { id, paths }) = action.downcast_ref::<FileDialogAction>() {
                if *id == live_id!(fab_import) {
                    for path in paths { cx.action(ShellAction::ImportFile(path.clone())); }
                }
                if *id == live_id!(fab_open) {
                    for path in paths {
                        cx.action(ShellAction::OpenFile(path.clone()));
                    }
                }
                if *id == live_id!(fab_import_profiles) {
                    for path in paths {
                        self.import_profiles(path.clone());
                        changed = true;
                    }
                }
                if *id == live_id!(fab_image_to_part) {
                    for path in paths {
                        self.image_to_part(path.clone());
                        changed = true;
                    }
                }
                if *id == live_id!(fab_colour_picture) {
                    for path in paths {
                        self.colour_picture(path.clone());
                        changed = true;
                    }
                }
            }
            if let Some(a) = action.downcast_ref::<PartAction>() {
                self.part_action(cx, a);
            }
            if let Some(a) = action.downcast_ref::<fab::tools::annotate::MarkAction>() {
                self.mark_action(cx, a);
            }
            if let Some(a) = action.downcast_ref::<fab::api::TestPrintAction>() {
                self.test_print_action(cx, a);
            }
            // An electronics view's action (an edit, a preview, focus), plain or from its widget
            // (`cx.widget_action`): its host takes it.
            let view_action = action.downcast_ref::<fab_elec_view::ViewAction>().or_else(|| {
                action.as_widget_action().and_then(|w| w.action.downcast_ref::<fab_elec_view::ViewAction>())
            });
            if let Some(a) = view_action.filter(|_| self.elec.is_some()) {
                self.elec_sync();
                if let Some(elec) = &mut self.elec {
                    elec.act(cx, a.clone());
                    // a part selected in a view is ringed in the 3D views
                    let selected = elec.selected_parts();
                    if *self.state.elec_selected != selected[..] {
                        self.state.elec_selected = selected.into();
                        fab::ui::redraw_now(cx, &self.ui, &[ids!(viewport), ids!(tab_device)]);
                    }
                }
                self.elec_sync();
                self.take_elec_poses(cx);
            }
            if let Some(a) = action.downcast_ref::<MeasureAction>() {
                match a {
                    MeasureAction::Select(index) => changed |= tools::measure::select(&mut self.state, *index),
                    _ => {
                        if let (Some(edit), Some(host)) = (model_host::measurement_action(a), &mut self.host) {
                            host.edit(vec![edit]);
                        }
                    }
                }
            }
            if let Some(e) = action.downcast_ref::<img2mesh::Img2MeshEvent>() {
                self.img2mesh_event(cx, e);
            }
            if let Some(a) = action.downcast_ref::<fab::api::MechAction>() {
                // the closed box's play, pause, speed and Listen go to its board
                let closed = closed_box::ClosedBox::showing(&self.state) && self.closed_box.action(cx, a, &self.device_assembly);
                if !closed {
                    let building = self.host.as_ref().is_some_and(|h| h.building());
                    self.mech.action(cx, a, &mut self.state, self.plate.as_ref(), building);
                }
                // the closed box runs its board: the electronics host starts with it
                if closed_box::ClosedBox::showing(&self.state) {
                    self.start_elec(cx);
                }
            }
            if let Some(a) = action.downcast_ref::<fab::api::ArmAction>() {
                if self.arm.action(cx, a, &mut self.state) {
                    fab::ui::redraw_now(cx, &self.ui, arm::TABS);
                }
            }
            if let Some(a) = action.downcast_ref::<fab::api::ScanAction>() {
                if self.scan.action(cx, a, &mut self.state) {
                    fab::ui::redraw_now(cx, &self.ui, scan::TABS);
                }
            }
            if let Some(a) = action.downcast_ref::<fab::api::ForceAction>() {
                if self.force.action(a, &mut self.state) {
                    fab::ui::redraw_now(cx, &self.ui, force::TABS);
                }
                // a fixture or load added, changed or removed: a statement of the part's script
                if let Some((object, edit)) = self.force.take_edit() {
                    self.part_action(cx, &PartAction::Study { object, edit });
                }
            }
            if let Some(a) = action.downcast_ref::<fab::params::ParamAction>() {
                self.param_action(cx, a);
            }
            if let Some(a) = action.downcast_ref::<fab::tools::picks::PickAction>() {
                self.pick_action(cx, a);
            }
            if let Some(a) = action.downcast_ref::<fab::ui::code::SourceAction>() {
                self.source_action(cx, a);
            }
            if let Some(sdf_live::LiveAction { gen, fields, params, key }) = action.downcast_ref::<sdf_live::LiveAction>() {
                // newer than what shows (a step still being built for a newer value shows
                // meanwhile: the shape keeps up with the drag); the fields whose key changed
                // (the rest draw as they are)
                if *gen > self.live_shown {
                    self.live_shown = *gen;
                    // every handle at the value these fields are of (once its commit is in, the
                    // committed plate's are the truth; a part not drawn from its field has its preview)
                    if !self.state.params.pending.is_empty() && self.field_drawn(key) {
                        self.state.params.parts = params.clone();
                    }
                    let plate = &self.state.plate.parts;
                    self.state.params.sdf_live = fields
                        .iter()
                        .filter(|l| !plate.iter().any(|p| p.object == l.object && p.sdf.as_ref().is_some_and(|s| s.key == l.sdf.key)))
                        .cloned()
                        .collect();
                    cx.action(ShellAction::SdfLive);
                    self.slice_live_fields(cx);
                }
                // the newest value's fields are in: a held slice goes on (its new input resumed it already)
                if *gen == self.live_gen {
                    live_slice::shape_published();
                }
            }
            if let Some(ghost::AreasAction(gen, areas)) = action.downcast_ref::<ghost::AreasAction>() {
                if *gen == self.areas_gen {
                    let mut areas = (**areas).clone();
                    for a in &mut areas {
                        a.rows = self.areas_rows.iter().filter(|(o, _)| *o == a.object).map(|(_, r)| r.clone()).collect();
                    }
                    self.state.params.areas = Arc::new(areas);
                    fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_viewport)]);
                }
            }
            if let Some(ghost::GhostAction(gen, view)) = action.downcast_ref::<ghost::GhostAction>() {
                // Only the newest request's ghost shows (a slower older one never overwrites it).
                if *gen == self.ghost_gen {
                    self.state.params.ghost = view.clone().filter(|g| !g.bodies.is_empty());
                    // Ghost geometry and its presence readout live in the viewport and part panel.
                    fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_viewport), ids!(tab_part), ids!(xr)]);
                }
            }
            if let Some(a) = action.downcast_ref::<fab::ui::strength::StrengthAction>() {
                fab::ui::strength::apply(&mut self.state, a);
                match a {
                    fab::ui::strength::StrengthAction::Find => self.find_weak_spots(),
                    fab::ui::strength::StrengthAction::Apply => self.apply_strength(),
                    fab::ui::strength::StrengthAction::TestPrint(id) => self.test_print_spot(id),
                    _ => {}
                }
                cx.redraw_all();
            }
            if let Some(a) = action.downcast_ref::<StageAction>() {
                fab::ui::stage::apply(&mut self.state, a);
                if let StageAction::Zones(on) = a {
                    // The overlay is part of the plate's scene: rebuild it.
                    plate_scene::SHOW_ZONES.store(*on, std::sync::atomic::Ordering::Relaxed);
                    self.bed_scene = None;
                    if let Some(host) = &mut self.host {
                        host.overhangs(self.state.print.overhangs);
                    }
                }
                if matches!(a, StageAction::PaintRadius(_) | StageAction::PaintAngle(_) | StageAction::SculptRadius(_) | StageAction::SculptStrength(_)) {
                    // Brush dials and their cursor feedback live in the viewport and XR view.
                    fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_viewport), ids!(xr)]);
                } else {
                    changed = true;
                }
            }
            if let Some(a) = action.downcast_ref::<fab::print::PrintAction>() {
                let local_playhead = self.state.print.play.active && self.state.print.following().is_none() && a.moves_playhead();
                if self.print_action(cx, a) {
                    if local_playhead {
                        // A step of an already-visible slice: its view, layer slider and readouts.
                        fab::ui::redraw_plates(cx, &self.ui, fab::ui::stage::PLAYHEAD);
                    } else {
                        cx.redraw_all();
                    }
                }
            }
            // The Calibrate panel follows the job's printer and first filament.
            {
                let st = &self.state.print;
                let filament = st.settings.materials.first().map(|f| f.preset.as_str()).unwrap_or("");
                if st.calibrate.steps.is_empty() || st.calibrate.printer != st.settings.machine || st.calibrate.filament != filament {
                    let (printer, filament) = (st.settings.machine.clone(), filament.to_string());
                    self.calibrate.refresh(&mut self.state.print.calibrate, &printer, &filament);
                }
            }
            if let Some(a) = action.downcast_ref::<fab::print::calibrate::CalibrateAction>() {
                let colour = self.state.print.settings.materials.first().map(|f| f.color).unwrap_or([0.8, 0.8, 0.8, 1.0]);
                let setup = self.printing.check_setup(&self.state.print);
                if self.calibrate.action(&mut self.state.print.calibrate, a, setup, colour, self.host.as_mut()) {
                    cx.redraw_all();
                }
            }
            if let Some(calibrate::CalEvent::Made(made)) = action.downcast_ref::<calibrate::CalEvent>() {
                self.calibrate.made(&mut self.state.print.calibrate, made);
                // The test part's slice shows: a job that is over gives Monitor's view back.
                if made.is_ok() && self.state.print.follow.job.is_none() {
                    fab::print::apply(&mut self.state.print, &fab::print::PrintAction::CloseJob);
                }
                if let (Ok((coupon, colour)), Some(host)) = (made, &mut self.host) {
                    self.demo_card = None;
                    self.calibration_job = Some(host.calibration(coupon.clone(), *colour));
                }
                cx.redraw_all();
            }
            if let Some(ev) = action.downcast_ref::<print::PrintEvent>() {
                // a slice can run a field's loop into its stop where the view never looked
                if self.state.mark_field_loop_stops() {
                    self.ui.redraw(cx);
                }
                if let print::PrintEvent::Checked { id, report, error } = ev {
                    let mut value = fab_ai::check_value(report);
                    if let (fab_ai::out::S::Object(_, fields), Some(error)) = (&mut value, error) {
                        fields.push(("slice_error".into(), fab_ai::out::s(error.clone())));
                    }
                    self.calls.finish(*id, chat::Answer { ok: error.is_none(), text: fab_ai::out::text(&value), images: Vec::new() });
                }
                if self.printing.event(&mut self.state.print, ev) {
                    self.state.print.bump();
                    if matches!(ev, print::PrintEvent::Catalog(_) | print::PrintEvent::ProfilesImported { .. }) {
                        if let Some(host) = &mut self.host { host.print_setup(self.printing.check_setup(&self.state.print)); }
                        live_slice::set_print_setup(self.printing.check_setup(&self.state.print));
                        self.request_reslice();
                    }
                    self.remap();
                    self.sync_scene(cx, false);
                    cx.redraw_all();
                }
            }
            if let Some(made) = action.downcast_ref::<TestViewMade>() {
                self.show_test_view(cx, made);
            }
            if let Some(live_slice::LiveAction::AiWait) = action.downcast_ref::<live_slice::LiveAction>() {
                self.slice_view_follow(cx);
                live_slice::ai_answer(self.slice_refusal());
            }
            if let Some(live_slice::LiveAction::Updated) = action.downcast_ref::<live_slice::LiveAction>() {
                self.overhang_list_follow();
                if let Some(r) = self.live_view.as_ref().and_then(|l| l.latest()) {
                    let p = &self.state.print;
                    let before = (p.revision, p.status.clone(), p.estimate.is_some(), p.growing.is_some());
                    if self.printing.live_updated(&mut self.state.print, &r) {
                        cx.stop_timer(self.estimate_timer);
                        // a print time kept for these toolpaths comes at once; the time model's
                        // waits for the edits to rest
                        if live_slice::estimate_kept() {
                            live_slice::estimate();
                        } else {
                            self.estimate_timer = cx.start_timeout(0.3);
                        }
                    }
                    // Tile/progress publications can update status without replacing the preview (a new
                    // preview or new issues bump the print revision themselves).
                    let p = &self.state.print;
                    if p.status != before.1 || p.estimate.is_some() != before.2 || p.growing.is_some() != before.3 {
                        self.state.print.bump();
                    }
                    let editing = self.plate.as_ref().is_some_and(|ev| !ev.plate.parts.is_empty()) && (self.state.print.stale || !self.state.print.settled);
                    // The newest contours, also the newly traced layers of the same revision
                    // (only the changed layers' beads are new).
                    if editing && self.state.print.live_contours.as_ref().is_none_or(|c| c.revision < r.revision || (c.revision == r.revision && c.contours.all.id() != r.contours.all.id())) {
                        self.state.print.live_contours = Some(fab::print::LiveContours { revision: r.revision, contours: r.contours.clone() });
                    }
                    // Live tiles update the view. The slice/job/printer summaries, and the plate
                    // summary the part panel shows while no part is (a part's own panel shows
                    // nothing of the slice), sync to the print revision: they redraw when it moved.
                    // The caption's sync (after this event) redraws what changes in it.
                    fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_viewport), ids!(xr)]);
                    if self.state.print.revision != before.0 {
                        fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_slice), ids!(tab_settings), ids!(tab_printer)]);
                        if fab::ui::parts::active_part(&self.state).is_none() {
                            fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_part)]);
                        }
                    }
                }
            }
            // Without a view no tiles come: the whole toolpaths say the slice is done.
            if let Some(live_slice::LiveAction::Toolpaths { revision }) = action.downcast_ref::<live_slice::LiveAction>() {
                self.overhang_list_follow();
                self.strength_follow();
                cx.redraw_all();
                if self.live_view.is_none() && self.printing.live_toolpaths(&mut self.state.print, *revision) {
                    self.state.print.bump();
                    fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_slice), ids!(tab_settings), ids!(tab_printer), ids!(caption)]);
                }
            }
            if let Some(live_slice::LiveAction::Failed { revision, error }) = action.downcast_ref::<live_slice::LiveAction>() {
                self.printing.live_failed(&mut self.state.print, *revision, error);
                self.state.print.bump();
                // Slice failure is shown by the stage, slice/job/printer panels and caption.
                fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_viewport), ids!(tab_slice), ids!(tab_settings), ids!(tab_printer), ids!(caption)]);
            }
            if let Some(a) = action.downcast_ref::<xr::FabXrAction>() {
                self.xr_action(cx, a);
            }
            if let Some(a) = action.downcast_ref::<HostAction>() {
                match a {
                    HostAction::Ready => {}
                    HostAction::Quit { id } => {
                        if self.quitting == Some(*id) {
                            cx.quit();
                        }
                    }
                    HostAction::OverhangList(result) => {
                        let (key, list) = &**result;
                        if live_slice::latest_toolpaths_key() == Some(*key) {
                            self.state.print.overhang_list = Some(list.clone());
                            self.overhang_list_key = Some(*key);
                            fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_slice)]);
                        }
                    }
                    HostAction::Painted(painted) => {
                        if let Some(host) = &mut self.host {
                            host.painted(painted.clone());
                        }
                    }
                    HostAction::BendsMeasured(content) => {
                        // a bond that failed goes on the plate's report; Mechanics takes the bends
                        if let Some(host) = &mut self.host {
                            host.bends_measured(*content);
                        }
                        cx.redraw_all();
                    }
                    HostAction::Plates(list) => {
                        if let Some(list) = list.lock().ok().and_then(|mut l| l.take()) {
                            self.state.plates = list;
                            cx.redraw_all();
                        }
                    }
                    HostAction::Done(done) => {
                        if done.model_changed {
                            if let (Some(update), Some(elec)) = (&done.update, &mut self.elec) {
                                elec.plate_built(cx, update.0.clone());
                            }
                        }
                        self.host_done(cx, done);
                    }
                    // after the job's live input went out (its Done or Preview came first): the
                    // held slice goes on once no shape job is left, also on an unchanged or
                    // failed plate
                    HostAction::ShapeJobOver { built } => {
                        if let Some(host) = self.host.as_mut().filter(|_| *built) {
                            host.build_over();
                        }
                        if !self.host.as_ref().is_some_and(|h| h.shaping()) {
                            live_slice::shape_published();
                        }
                    }
                    HostAction::Shown(shown) => {
                        self.host_shown(cx, &shown.0, &shown.1);
                    }
                    HostAction::Preview(p) => {
                        self.host_preview(cx, p);
                    }
                    HostAction::Solved(solved) => {
                        if let (Some(solved), Some(host)) = (solved.lock().ok().and_then(|mut s| s.take()), self.host.as_mut()) {
                            host.solved(solved);
                        }
                    }
                    HostAction::Grown(shown) => {
                        if let Some(host) = self.host.as_mut() {
                            host.grown(shown.clone());
                        }
                    }
                    HostAction::GrownSettled(settled) => {
                        if let Some(host) = self.host.as_mut() {
                            host.grown_settled(settled.clone());
                        }
                    }
                    HostAction::DocsLanded { bed, docs } => {
                        if let Some(elec) = self.elec.as_mut() {
                            elec.docs_landed(*bed, docs);
                        }
                    }
                    HostAction::GrownBuilt => {
                        if let Some((key, value)) = self.state.params.dragging.clone() {
                            self.request_live(&key, &value);
                        }
                    }
                    HostAction::GrowLine(line) => {
                        let lines = &mut self.state.params.grow_lines;
                        lines.retain(|l| l.object != line.object || l.part != line.part);
                        if !line.text.is_empty() {
                            lines.push(line.clone());
                        }
                        cx.redraw_all();
                    }
                    HostAction::XrPlate(plate) => {
                        if let Some(plate) = plate.lock().ok().and_then(|mut p| p.take()) {
                            if let Some(mut view) = self.ui.widget(cx, ids!(xr)).borrow_mut::<xr::FabXr>() {
                                view.set_plate(plate);
                            }
                        }
                    }
                    HostAction::Answered { id, ok, text } if self.strength_call == Some(*id) => {
                        self.strength_call = None;
                        self.strength_found(*ok, text);
                        cx.redraw_all();
                    }
                    HostAction::Answered { id, ok, text } => {
                        self.calls.finish(*id, chat::Answer { ok: *ok, text: if *ok { text.clone() } else { shown_error(text) }, images: Vec::new() });
                    }
                    HostAction::VariantsFitted { id, round } => {
                        let round = round.lock().ok().and_then(|mut r| r.take());
                        if let (Some((fit, prepared)), Some(run)) = (round, self.variant_runs.get_mut(id)) {
                            run.fitted(fit, prepared.len());
                            self.review_queue.extend(prepared.into_iter().map(|p| (*id, p)));
                        }
                    }
                    HostAction::Pictured { id, result } => {
                        self.variant_runs.remove(id);
                        let answer = match result {
                            Ok((text, images)) => chat::Answer { ok: true, text: text.clone(), images: images.iter().map(|(l, png)| (l.clone(), Arc::from(png.as_slice()))).collect() },
                            Err(e) => chat::Answer { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(e)), images: Vec::new() },
                        };
                        self.calls.finish(*id, answer);
                    }
                    HostAction::Sheets { id, result, made } => {
                        let look = self.look_answers.remove(id);
                        let answer = match result {
                            Ok(sheets) if look.is_some() => chat::Answer { ok: true, text: look.unwrap_or_default(), images: sheets.pictures.iter().map(|(l, png)| (l.clone(), Arc::from(png.as_slice()))).collect() },
                            Ok(sheets) => {
                                let labels: Vec<&str> = sheets.pictures.iter().map(|(l, _)| l.as_str()).collect();
                                let ev = self.plate.as_ref();
                                let summary = ev.map(|e| fab_ai::out::text(&fab_ai::plate_value(e))).unwrap_or_else(|| "nil".into());
                                let labels = fab_ai::out::text(&fab_ai::out::arr(labels.iter().map(|l| fab_ai::out::s(*l))));
                                chat::Answer {
                                    ok: true,
                                    text: format!("{{sheets: {labels} views: \"Each sheet's views row-major, two across\" cameras: {} rays: {} plate: {summary}}}", sheets.cameras, fab_ai::out::quote(fab_ai::camera::RAYS)),
                                    images: sheets.pictures.iter().map(|(l, png)| (l.clone(), Arc::from(png.as_slice()))).collect(),
                                }
                            }
                            Err(e) => chat::Answer { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(e)), images: Vec::new() },
                        };
                        let answer = match self.img2mesh_answers.remove(id) {
                            Some(import) => {
                                if let Some(card) = self.calls.tasks.get_mut(id) {
                                    if let Ok(v) = makepad_script::syntax::parse_value(&import) {
                                        if let Some(name) = v.field("imported").and_then(|f| f.value.string()) {
                                            card.title = "Made a part".into();
                                            card.result = format!("on the plate as {name}");
                                        }
                                        if let Some(object) = v.field("objects").and_then(|f| f.value.items()).and_then(|items| items.first()).and_then(|v| v.string()) {
                                            card.actions.push(TaskAction { id: format!("select:{object}"), label: "Select".into() });
                                        }
                                    }
                                    if let Some(file) = made {
                                        card.result_picture = fab_model::project::thumbnail_path(file).display().to_string();
                                        card.actions.push(TaskAction { id: format!("open:{}", file.display()), label: "Open".into() });
                                    }
                                }
                                chat::Answer { text: format!("{{import: {import} render: {}}}", answer.text), ok: true, ..answer }
                            }
                            None => answer,
                        };
                        self.calls.finish(*id, answer);
                    }
                }
            }
        }
        // The caption's jobs: a retry queues the job again; a next step is the job's own.
        for action in makepad_widgets::job_actions(actions) {
            match action {
                makepad_widgets::JobAction::Retry(job) => {
                    if !self.retry_job(cx, *job) {
                        log!("fab: job {} cannot be retried", job.0);
                    }
                }
                makepad_widgets::JobAction::Next(job) => self.next_step(cx, *job),
            }
        }
        let caption = self.ui.widget(cx, ids!(caption));
        fab::ui::topbar::handle_actions(cx, &caption, actions, &self.state);
        // The Design workspace's Add menu (the feature tree's bar).
        let add_menu = live_id!(fab_add_feature);
        if let Some(anchor) = fab::ui::popover::dropdown_clicked(actions, add_menu) {
            fab::ui::popover::open_menu(cx, add_menu, code_edit::add_menu(), anchor, fab::ui::popover::MenuPlace::Below);
        }
        if let Some(pick) = fab::ui::popover::menu_picked(actions, add_menu).and_then(code_edit::add_pick) {
            self.add_feature(cx, pick);
        }
        let shape_menu = live_id!(fab_shape_edit);
        if let Some(anchor) = fab::ui::popover::dropdown_clicked(actions, shape_menu) {
            fab::ui::popover::open_menu(cx, shape_menu, code_edit::shape_menu(), anchor, fab::ui::popover::MenuPlace::Below);
        }
        if let Some(op) = fab::ui::popover::menu_picked(actions, shape_menu).and_then(code_edit::shape_pick) {
            if let Some(object) = fab::ui::parts::active_part(&self.state).map(|i| self.state.plate.parts[i].object.clone()) {
                self.part_action(cx, &PartAction::Shape { object, op });
            } else {
                self.state.ui.notices.say(NoticeView::Plate, "Select a part first".into());
                self.ui.redraw(cx);
            }
        }
        shell_chat::handle_actions(self, cx, actions);
        if actions.iter().any(|a| a.downcast_ref::<voice::VoiceAction>().is_some()) {
            // (a refusal is a line in the chat, already said)
            let _ = self.voice.toggle(cx, None);
        }
        self.sync_ghost(cx);
        if follow {
            self.follow_printer(cx);
        }
        self.slice_view_follow(cx);
        // Field parts have meshes only while the view shows meshes.
        let meshes = self.state.view.look == fab::api::PartLook::Mesh;
        if meshes != self.meshing_fields {
            if let Some(host) = &mut self.host {
                self.meshing_fields = meshes;
                host.mesh_fields(meshes);
            }
        }
        // Exact parts refine as the view closes in, a level at a time, off the UI thread.
        let detail = exact_detail_of(self.state.view.camera(self.state.view.lead_plate()));
        if detail != self.exact_detail {
            if let Some(host) = &mut self.host {
                self.exact_detail = detail;
                host.exact_detail(detail);
            }
        }
        if changed {
            self.ui.redraw(cx);
        }
    }
}

/// The plate's parts, inserts and solids as the caption counts them:
/// parts with their eye off said so; boards (never printed, so never "hidden") and inserts counted
/// separately; the solids that build them; and the areas marked on them (print settings volumes,
/// no-trees areas) apart, when there are any.
fn counts_text(printed: usize, hidden: usize, inserts: usize, boards: usize, status: &Status) -> String {
    // A plate of electronics documents alone: its circuits and benches are in the Electronics
    // workspace, and the plate prints nothing until a board or case is placed on it.
    if printed + hidden + inserts + boards == 0 && !status.docs.is_empty() {
        return "electronics only · nothing to print yet".into();
    }
    let plural = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    let parts = plural(printed + hidden, "part");
    let parts = if hidden > 0 { format!("{parts} ({hidden} hidden)") } else { parts };
    let parts = match boards {
        0 => parts,
        n if printed + hidden == 0 => plural(n, "board"),
        n => format!("{parts} · {}", plural(n, "board")),
    };
    let mut out = format!("{parts} · {}", plural(status.solids, "solid"));
    for (n, one) in [(inserts, "insert"), (status.volumes, "settings volume"), (status.no_trees, "no-trees area")] {
        if n > 0 {
            out += &format!(" · {}", plural(n, one));
        }
    }
    out
}

/// The buttons under a turn's answer, with document undo or redo first.
fn turn_buttons(offer: Option<turn_undo::Offer>, layout: Option<&TurnLayout>, workspaces: &Workspaces) -> Vec<makepad_app_aichat::TurnButton> {
    use turn_undo::Offer;
    let button = |id: &str, label: &str, why: &str| makepad_app_aichat::TurnButton { id: id.into(), label: label.into(), enabled: why.is_empty(), why: why.into() };
    let mut buttons = match offer {
        None => Vec::new(),
        Some(Offer::Undo(_)) => vec![button("undo", "Undo these changes", "")],
        Some(Offer::Redo(_)) => vec![button("redo", "Redo these changes", "")],
        Some(Offer::Under(Some(_))) => vec![button("undo", "Undo these changes", "A later answer's changes come after it: undo those first")],
        Some(Offer::Under(None)) => vec![button("undo", "Undo these changes", "Other changes came after it: Undo goes back a step at a time")],
        Some(Offer::After) => vec![button("redo", "Redo these changes", "Earlier changes are undone: redo those first")],
        Some(Offer::Partly) => vec![button("undo", "Undo these changes", "Partly undone: Undo and Redo go a step at a time")],
    };
    if let Some(layout) = layout {
        if layout.temp_open(workspaces) {
            buttons.push(button("ws_keep", "Keep", ""));
            buttons.push(button("ws_back", "Back", ""));
        }
        if let Some(offer) = layout.undo_offer(workspaces) {
            buttons.push(button("ws_undo", "Undo layout change", offer.err().unwrap_or("")));
        }
    }
    buttons
}

/// The answer when an `image` names neither a file nor a picture sent in the chat.
fn no_such_picture(image: &str, pictures: &makepad_app_aichat::ChatPictures) -> String {
    let names: Vec<String> = pictures.names().iter().map(|n| format!("{n:?}")).collect();
    format!("no picture file at '{image}' and no picture of that name in this chat (chat.pictures lists them: [{}])", names.join(" "))
}

/// The argument naming the picture file a call reads, as a path into its arguments: `image` for
/// `fab.picture` and `fab.colour_read`, `picture` for `fab.compare`, `compare.picture` for
/// `fab.variants`.
fn picture_key(tool: &str) -> Option<&'static [&'static str]> {
    match tool {
        "picture" | "colour_read" => Some(&["image"]),
        fab_ai::compare::TOOL => Some(&["picture"]),
        fab_ai::variants::TOOL => Some(&["compare", "picture"]),
        _ => None,
    }
}

/// A call that reads a picture file ([`picture_key`]): one named that is no file but the name of
/// a picture the person sent in the chat is that picture ([`picture_args`] once its file is
/// written).
fn sent_picture(cx: &mut Cx, key: &[&str], args: &Value) -> Result<Option<makepad_ai_services::engine::SentPicture>, String> {
    let Some(image) = key.iter().try_fold(args, |v, k| v.get(k)).and_then(Value::as_str) else { return Ok(None) };
    if std::path::Path::new(image).is_file() {
        return Ok(None);
    }
    let pictures = cx.global::<makepad_app_aichat::ChatPictures>();
    pictures.get(Some(image)).cloned().map(Some).ok_or_else(|| no_such_picture(image, pictures))
}

/// The call's picture ([`picture_key`]) as the sent picture's file, as it was dropped (the chat
/// keeps it); a new tile is named after the picture rather than its file.
fn picture_args(tool: &str, args: Value, name: &str, path: &std::path::Path) -> Value {
    /// `value` with the field at `key` (a path) the file's.
    fn put(value: Value, key: &[&str], path: &std::path::Path) -> Value {
        match (key.split_first(), value) {
            (None, _) => Value::Str(path.display().to_string()),
            (Some((first, rest)), Value::Obj(mut fields)) => {
                for (k, v) in fields.iter_mut() {
                    if k == first {
                        *v = put(std::mem::replace(v, Value::Null), rest, path);
                    }
                }
                Value::Obj(fields)
            }
            (Some(_), other) => other,
        }
    }
    let args = match picture_key(tool) {
        Some(key) => put(args, key, path),
        None => args,
    };
    let Value::Obj(mut fields) = args else { return args };
    if tool == "picture" && !fields.iter().any(|(key, _)| key == "name") {
        let stem = std::path::Path::new(name).file_stem().map_or_else(|| name.to_string(), |s| s.to_string_lossy().to_string());
        fields.push(("name".to_string(), Value::Str(stem)));
    }
    Value::Obj(fields)
}

/// The plate id of the document on the bed (none for a demo or before one is on it).
fn bed_plate(status: &Status) -> Option<String> {
    status.bed.as_ref().filter(|(_, demo)| !demo).map(|(id, _)| id.clone())
}

/// An AI call that puts another document on the bed.
/// A tool argument as the Splash text an electronics action reads: a string as it is, a number
/// or a flag as its text, a list as `[a b]` of its items.
fn splash_arg(value: &Value) -> String {
    match value {
        Value::Str(text) => text.clone(),
        Value::Arr(items) => format!("[{}]", items.iter().map(splash_arg).collect::<Vec<_>>().join(" ")),
        other => other.to_json(),
    }
}

fn switches_bed(tool: &str, args: &Value) -> bool {
    match tool {
        "new" => true,
        "import" => args.get("new_plate").and_then(Value::as_bool) == Some(true),
        "plates" => matches!(args.get("action").and_then(Value::as_str), Some("open" | "remove")),
        _ => false,
    }
}

/// A model failure as the code panel and a tool's answer show it: the kernel's text with its
/// floats rounded for reading.
fn shown_error(text: &str) -> String {
    fab::params::round_numbers(text, fab::params::ERROR_DECIMALS)
}

/// The chord (mm) an exact part is shown at: about half a pixel of a typical view, in steps, so a
/// zoom passes few levels. The slicer has its own, finer one.
fn exact_detail_of(c: &fab::api::Camera) -> f64 {
    const VIEW_PIXELS: f64 = 1000.0;
    let height = if c.ortho { c.ortho_height as f64 } else { 2.0 * c.distance() as f64 * (c.fov_y_deg as f64 * 0.5).to_radians().tan() };
    let half_pixel = 0.5 * height / VIEW_PIXELS;
    [0.0125, 0.025, 0.05, 0.1].into_iter().find(|level| half_pixel <= *level).unwrap_or(0.1)
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        #[cfg(feature = "tweaker")]
        makepad_widgets_tweaker::link(vm);
        // Standalone (no window manager handing its sheet down) Fab wears its
        // host's desktop sheet in the host's appearance, before the theme loads.
        fab::theme::wear_host_sheet(vm);
        makepad_widgets::script_mod(vm);
        makepad_app_aichat::script_mod(vm);
        fab::script_mod(vm);
        review::script_mod(vm);
        makepad_xr_ui::script_mod(vm);
        xr::script_mod(vm);
        self::script_mod(vm)
    }

    /// A restart in place (a landed change the JIT could not patch in) keeps the document by
    /// the worker's crash copy; this carries what the person is looking at. What is saved on this
    /// machine (shading, lighting, overlays: `view_prefs`) comes back as any run starts.
    fn restart_state(&mut self, _cx: &mut Cx) -> Option<String> {
        let c = self.state.view.camera(self.state.view.lead_plate());
        let mut f = RestartFields::default();
        for (name, v) in [("eye", c.eye), ("target", c.target), ("up", c.up)] {
            f.put(&format!("{name}.x"), &v.x);
            f.put(&format!("{name}.y"), &v.y);
            f.put(&format!("{name}.z"), &v.z);
        }
        f.put("fov_y_deg", &c.fov_y_deg);
        f.put("ortho", &c.ortho);
        f.put("ortho_height", &c.ortho_height);
        f.put("hung", &c.hung);
        f.put("xray", &self.state.view.xray);
        f.put("see_through", &self.state.view.see.on);
        f.put("see_opacity", &self.state.view.see.opacity);
        f.put("see_glow", &self.state.view.see.glow);
        Some(f.to_text())
    }

    fn restore_state(&mut self, _cx: &mut Cx, state: &str) {
        let f = RestartFields::from_text(state);
        let v3 = |name: &str| Some(vec3(f.get::<f32>(&format!("{name}.x"))?, f.get::<f32>(&format!("{name}.y"))?, f.get::<f32>(&format!("{name}.z"))?));
        let (Some(eye), Some(target), Some(up)) = (v3("eye"), v3("target"), v3("up")) else { return };
        let lead = self.state.view.lead_plate();
        let c = self.state.view.camera_mut(lead);
        c.eye = eye;
        c.target = target;
        c.up = up;
        f.restore("fov_y_deg", &mut c.fov_y_deg);
        f.restore("ortho", &mut c.ortho);
        f.restore("ortho_height", &mut c.ortho_height);
        f.restore("hung", &mut c.hung);
        f.restore("xray", &mut self.state.view.xray);
        f.restore("see_through", &mut self.state.view.see.on);
        f.restore("see_opacity", &mut self.state.view.see.opacity);
        f.restore("see_glow", &mut self.state.view.see.glow);
        self.state.view.mark_camera_changed(lead);
        self.keep_view = true;
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.state.drain_released_panes();
        self.hand.event(event);
        // Stop before widget dispatch: typing may still be an uncommitted draft, and a
        // press can start a drag before its first edit action reaches the model worker.
        if matches!(event, Event::MouseDown(_) | Event::TouchUpdate(_) | Event::KeyDown(_)
            | Event::TextInput(_) | Event::TextRangeReplace(_) | Event::TextCut(_)
            | Event::Clipboard(_) | Event::MacosMenuCommand(_)) {
            self.startup_monitor_done = true;
        }
        if self.slice_host.event(cx, event) {
            // A live patch changed the slicing code: the live slice is made again by it, and a
            // slice settled before is the old code's (Print and Export slice again).
            self.state.print.stale = self.state.print.preview.is_some();
            self.state.print.stale_parts = None;
            self.state.print.bump();
            fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_slice), ids!(tab_settings), ids!(tab_printer), ids!(caption)]);
        }
        // The system's audio devices: Listen opens the default output while anything is heard.
        if let Event::AudioDevices(devices) = event {
            fab_elec_view::live::listen::audio_devices(cx, devices);
        }
        if let Some(elec) = &mut self.elec {
            if elec.event(cx, event) && self.state.ui.shows(Family::Electronics) {
                fab::ui::redraw_now(cx, &self.ui, &[ids!(viewport)]);
            }
        }
        let file = self.status.path.as_deref().filter(|_| !self.status.bed.as_ref().is_some_and(|(_, demo)| *demo));
        self.mic.handle_event(cx, event, self.elec.as_mut().and_then(elec_host::ElecHost::sim_mut), self.status.switched, file);
        if let Some(elec) = &mut self.elec {
            elec.set_project(file);
            for (view, notice) in elec.take_notices() {
                match notice {
                    Some(text) => self.state.ui.notices.say(view, text),
                    None => self.state.ui.notices.clear(view),
                }
            }
        }
        self.elec_net.handle_event(cx, event, &mut self.state, file, self.elec.as_mut());
        self.elec_answers();
        self.follow_turn();
        self.elec_models(cx, event);
        self.pump_history(cx);
        // The electronics boards reach the bed plate's scripts.
        if let (Some(elec), Some(host)) = (self.elec.as_mut(), &mut self.host) {
            if let Some(env) = elec.take_env() {
                host.set_env(elec.bed(), env);
            }
        }
        // An edit's preview is motion for the 3D views; its end lets them converge.
        let previewing = self.elec.as_ref().is_some_and(|e| e.previewing());
        if previewing != self.state.elec_previewing {
            self.state.elec_previewing = previewing;
            fab::ui::redraw_now(cx, &self.ui, &[ids!(viewport)]);
        }
        if let Some((scene, committed)) = self.elec.as_mut().and_then(|e| Some((e.take_scene()?, e.scene_committed()))) {
            self.state.elec_scene = Some(scene);
            self.state.elec_scene_committed = committed;
            self.place_boards();
            fab::ui::redraw_plates(cx, &self.ui, &[ids!(viewport)]);
        }
        self.take_elec_poses(cx);
        // A scene on its way: the 3D views do not report themselves settled until it has come.
        let coming = self.host.as_ref().is_some_and(model_host::ModelHost::building) || self.elec.as_ref().is_some_and(elec_host::ElecHost::scene_coming);
        if coming != self.state.scene_coming {
            self.state.scene_coming = coming;
            fab::ui::redraw_now(cx, &self.ui, &[ids!(viewport), ids!(device_viewport)]);
        }
        if let Event::Actions(actions) = event {
            for action in fab::ui::popover::ui_actions(actions) {
                match action {
                    fab::ui::popover::FabUiAction::MenuOpened { owner, .. } => self.menu_owner = Some(*owner),
                    fab::ui::popover::FabUiAction::MenuClosed { .. } => self.menu_owner = None,
                    _ => {}
                }
            }
        }
        // Keyboard input belongs to the top overlay. Modals retain the global
        // shortcuts; menus own those too. Paste arrives as TextInput or Clipboard.
        if matches!(event,
            Event::KeyDown(_) | Event::KeyUp(_) | Event::TextInput(_)
                | Event::TextRangeReplace(_) | Event::TextCut(_) | Event::TextCopy(_)
                | Event::Clipboard(_) | Event::ImeAction(_) | Event::PhysicalKeyboard(_)
                | Event::MacosMenuCommand(_)
        ) {
            let owner = if self.menu_owner.is_some() {
                Some(ids!(menu_layer))
            } else if self.state.ui.command_palette_open {
                Some(ids!(command_palette))
            } else if self.state.ui.about_open {
                Some(ids!(about))
            } else if self.state.ui.keymap_help_open {
                Some(ids!(keymap_help))
            } else {
                None
            };
            if let Some(owner) = owner {
                if self.menu_owner.is_none() {
                    if let Event::KeyDown(ke) = event {
                        if self.global_key(cx, ke) {
                            return;
                        }
                    }
                }
                self.ui.widget(cx, owner)
                    .handle_event(cx, event, &mut Scope::with_data(&mut self.state));
                return;
            }
        }
        let _slow = xr::SlowEvent::start(cx, event);
        let _switch = switch_watch::event(event);
        makepad_xr_stream::XrDesktopFreeze::handle_event(cx, event);
        // The newest load asked for, on the view's progress card: it takes the card over from one
        // still on its way, which the host has cancelled.
        if let Some(load) = self.host.as_ref().and_then(|h| h.load()).filter(|l| l.call > self.load_seen).cloned() {
            self.load_seen = load.call;
            self.load_call = Some(load.call);
            let title = match &load.of {
                model_host::LoadOf::Plate(id) => self.state.plates.plates.iter().find(|p| &p.id == id).map_or_else(|| "the plate".to_string(), |p| p.name.clone()),
                model_host::LoadOf::Demo(demo) => demo.title.to_string(),
                model_host::LoadOf::File(path) => path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().to_string()),
            };
            self.state.progress_card = Some(fab::api::ProgressCard { title: format!("Loading {title}"), cancel_keeps: "the plate on the bed stays as it is".into(), failed: format!("{title} did not load"), job: load.job });
            fab::ui::redraw_now(cx, &self.ui, &[ids!(progress_card)]);
        }
        // The model's build jobs, for the view's progress chip (a copy only when they change).
        if let Some(host) = &self.host {
            if self.state.params.build_jobs.as_slice() != host.build_jobs() {
                self.state.params.build_jobs = host.build_jobs().to_vec();
                // the view's chip follows the running one from here (its own frames while it runs);
                // the caption's job strip follows the job board by itself
                fab::ui::redraw_now(cx, &self.ui, &[ids!(shape_handles)]);
            }
        }
        // Code typed but not committed yet is the one work a restart would lose (every committed
        // step is in the crash copy): a restart for a landed change waits for it.
        let unsaved = !self.source_commits.is_empty() || self.state.source.tabs.iter().any(|t| t.draft.is_some());
        if unsaved != self.unsaved_published {
            self.unsaved_published = unsaved;
            makepad_widgets::makepad_platform::restart::set_unsaved("fab-code", unsaved.then_some("code being typed in Fab's code panel"));
        }
        if let Event::Startup | Event::WindowGotFocus(_) = event {
            fab::theme::follow_host_appearance(cx);
        }
        if !self.started {
            if let Event::Startup | Event::Draw(_) = event {
                self.started = true;
                // The mechanics engine's warnings, and the sim's, go to the app log.
                makepad_dynamics::util_errmem::set_log_handler(Some(log_mechanics));
                self.register_loaders();
                let driven = std::env::args().any(|a| a == "--remote");
                // a driven run (tests) starts from no preference unless one is named
                if !driven || std::env::var_os("FAB_VIEW_PREFS").is_some() {
                    self.state.apply_view_prefs();
                }
                part_models::set_sources(self.state.model_sources.bits());
                self.model_choices = (fab_elec_parts::model_board::choices_changed(), kicad_models::load_choices());
                // A lane's test instance is not the person: it neither brings back nor changes the
                // machine's plates, unless it asks for a plate store of its own (`FAB_PLATES=1`,
                // under a home of its own).
                let test_lane = std::env::var_os("MAKEPAD_TEST_LANE").is_some();
                let own_plates = std::env::var_os("FAB_PLATES").is_some_and(|v| v == "1");
                self.host = Some(ModelHost::start(cx, (!driven && !test_lane) || own_plates));
                // a plate still building keeps the remote's `settled` false
                cx.register_async_load_probe("build", model_host::builds_in_flight);
                self.state.units = fab::model::Units { source_to_meters: 1.0, precision: 1, ..fab::model::Units::millimeters() };
                live_slice::init(cx);
                live_slice::open_slice_cache(&makepad_fab_settings::home::user_home().join(".makepad").join("fab").join("cache").join("slices"));
                self.live = Some(sdf_live::LiveWorker::start(cx));
                self.patterns = Some(pattern_live::PatternLive::start());
                self.printing.start(&mut self.state.print);
                self.ghosts = Some(ghost::GhostWorker::start(cx, self.printing.machine().expect("Fab defaults have a valid machine").clone()));
                live_slice::set_print_setup(self.printing.check_setup(&self.state.print));
                // The AI column starts folded: the header's spark button opens it.
                shell_chat::set_open(self, cx, false);
                self.open_from_args(cx);
            }
        }
        // A quit from the app menu, a signal or the main window's close button waits for the
        // plate's last save ([`App::quit`]); asked again while it waits, it goes at once.
        match event {
            Event::QuitRequested(e) if self.quitting.is_none() && self.host.is_some() => {
                e.handled.set(true);
                self.quit(cx);
            }
            Event::WindowCloseRequested(e) if self.quitting.is_none() && self.host.is_some()
                && self.ui.window(cx, ids!(main_window)).window_id() == Some(e.window_id) => {
                e.accept_close.set(false);
                self.quit(cx);
            }
            _ => {}
        }
        // Quit or relaunch: the slow workers end now (each finishes at most the job it runs), so
        // the domain's wait for its threads is short. An end nothing could hold (a relaunch) saves
        // only what the running job saves.
        if let Event::Shutdown = event {
            self.host = None;
            self.ghosts = None;
            self.printing.stop();
            crate::live_slice::stop();
            makepad_ai_services::engine::local_model::stop_hub_find();
            makepad_csg_math::shutdown();
        }
        if let Event::KeyDown(ke) = event {
            if self.global_key(cx, ke) {
                return;
            }
        }
        if self.source_timer.is_event(event).is_some() {
            self.commit_source(cx);
        }
        if !self.held_pictures.is_empty() {
            self.release_pictures();
        }
        if !self.held_switches.is_empty() && !self.gesture_live() {
            self.send_typing();
            if let Some(host) = &mut self.host {
                for (id, tool, args) in std::mem::take(&mut self.held_switches) {
                    host.chat_call_as(id, &tool, args);
                }
            }
        }
        if self.estimate_timer.is_event(event).is_some() && self.printing.live_whole() {
            live_slice::estimate();
        }
        if let Some(ne) = self.print_frame.is_event(event) {
            let dt = self.print_frame_time.map(|t| ne.time - t).unwrap_or(if self.state.print.play.playing { 1.0 / 60.0 } else { 1.0 / 30.0 });
            // The estimated follow position needs 30 Hz; ordinary playback keeps display cadence.
            // The margin takes a display's beat that lands a hair early at 60 and 120 Hz: without
            // it such a beat waits for the next, and the head moves unevenly.
            if !self.state.print.play.playing && self.state.print.monitor_preview().is_some() && dt < 1.0 / 30.0 - 0.004 {
                self.print_frame = cx.new_next_frame();
            } else {
                self.print_frame_time = Some(ne.time);
                let playing = fab::print::tick(&mut self.state.print, dt);
                let following = fab::print::follow::place(&mut self.state.print, Cx::time_now());
                if playing || following {
                    self.print_frame = cx.new_next_frame();
                } else {
                    self.print_frame_time = None;
                }
                fab::ui::redraw_plates(cx, &self.ui, fab::ui::stage::PLAYHEAD);
            }
        }
        self.follow_part_move(cx);
        if let (Event::Signal, Some(patterns)) = (event, self.patterns.as_mut()) {
            let moved = patterns.signal(cx, &mut self.state, self.plate.as_ref(), self.plate_revision);
            if moved.views {
                fab::ui::redraw_now(cx, &self.ui, &[ids!(viewport)]);
            }
            if moved.grown {
                if let Some(host) = &mut self.host {
                    host.patterns_grown();
                }
            }
            // the part panel's step counter
            if moved.status {
                fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_part)]);
            }
            if moved.status && self.printing.growth_moved(&mut self.state.print, self.plate.as_ref()) {
                self.state.print.bump();
                cx.redraw_all();
            }
        }
        let building = self.host.as_ref().is_some_and(|h| h.building());
        // a board's bench leading the Mechanics sim: the sim's views draw the instant the bench's show
        let instant = self.motion_link.instant(&self.mech, self.elec.as_mut());
        self.mech.show_instant(cx, instant);
        self.mech.handle_event(cx, event, &mut self.state, self.plate.as_ref(), building);
        let closed = closed_box::ClosedBox::sync_panes(&mut self.state);
        let device_shown = self.state.device_shown();
        self.device_assembly.handle_event(cx, event, &mut self.state, self.plate.as_ref(), device_shown, closed);
        self.closed_box.handle_event(cx, &mut self.state, self.plate.is_some(), &self.device_assembly);
        if motion_link::MotionLink::wants_elec(&self.state, self.plate.as_ref()) {
            self.start_elec(cx);
        }
        // a board shown on the plate is drawn by the electronics host: it starts with one
        if self.elec.is_none() && self.state.board_shown() == Some(true) {
            self.start_elec(cx);
        }
        self.motion_link.handle_event(cx, &mut self.state, self.plate.as_ref(), &self.mech, self.elec.as_mut());
        // a sim of field parts that came to wait for the plate's slice has the plate sliced
        if self.slice_wanted() != self.slice_was_wanted {
            self.slice_view_follow(cx);
        }
        if self.arm.handle_event(cx, event, &mut self.state) {
            fab::ui::redraw_now(cx, &self.ui, arm::TABS);
        }
        if self.scan.handle_event(cx, event, &mut self.state) {
            fab::ui::redraw_now(cx, &self.ui, scan::TABS);
        }
        let printing = &self.printing;
        if self.force.handle_event(cx, event, &mut self.state, force::Inputs { plate: self.plate.as_ref(), setup: &|s: &fab::AppState| printing.check_setup(&s.print) }) {
            fab::ui::redraw_now(cx, &self.ui, force::TABS);
        }
        if !self.force_calls.is_empty() {
            self.answer_force_calls(cx);
        }
        if self.resin.handle_event(cx, &mut self.state, self.plate.as_ref(), self.plate_revision, &self.status.name) {
            fab::ui::redraw_now(cx, &self.ui, &[ids!(viewport), ids!(slice_ui.resin_ui), ids!(slice_ui.right_col)]);
        }
        if std::mem::take(&mut self.resin.issues_changed) {
            // the print's rules broke: Printability and the print card list where
            fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_slice), ids!(tab_printer)]);
        }
        if std::mem::take(&mut self.resin.print_changed) {
            // the resin printer and its job: the Printer panel and Monitor's job card say them
            self.state.print.bump();
            cx.redraw_all();
        }
        if std::mem::take(&mut self.resin.status_changed) {
            // the stack's status line: Printability (with no issues) and the Job panel say it
            fab::ui::redraw_now(cx, &self.ui, &[ids!(tab_slice), ids!(tab_settings)]);
        }
        if matches!(event, Event::XrUpdate(_)) && !self.mech.haptics.is_empty() {
            if let Some(mut view) = self.ui.widget(cx, ids!(xr)).borrow_mut::<xr::FabXr>() {
                view.sim_events(cx, &mut self.mech.haptics);
            }
        }
        if std::mem::take(&mut self.mech.restore_scene) | std::mem::take(&mut self.resin.restore_scene) {
            // the sim (links as separate objects) or the resin slice (its supports) showed a scene
            // of its own: the model's comes back
            if let Some(scene) = self.shown_scene() {
                if !self.wants_bed_only() {
                    cx.action(ShellAction::SceneUpdated { scene, frame: false });
                }
            }
        }
        self.chat.handle_event(event);
        switch_watch::mark("before the app's actions");
        self.match_event(cx, event);
        switch_watch::mark("the app's actions");
        if self.printing.poll_printer(&mut self.state.print) {
            self.state.print.bump();
            self.remap();
            self.follow_printer(cx);
            cx.redraw_all();
        }
        self.resume_monitor(cx);
        self.printing.sync_jobs(&mut self.state.print);
        self.frame_shown_job(cx);
        // The confirm card says what stops Start print as it is now, before this event's draw (a
        // resin printer's card asks its own).
        if self.state.print.confirm_print && self.state.print.resin.is_none() {
            if self.printing.confirm_slice(&mut self.state.print, self.plate.as_ref()) {
                self.state.print.bump();
                cx.redraw_all();
            }
            let blocked = self.printing.send_blocked(&self.state.print);
            if blocked != self.state.print.send_blocked {
                self.state.print.send_blocked = blocked;
                cx.redraw_all();
            }
        }
        if matches!(event, Event::LiveEdit | Event::ScriptReapply) { self.caption = None; }
        switch_watch::mark("before the widgets");
        self.ui
            .handle_event(cx, event, &mut Scope::with_data(&mut self.state));
        if let Event::Draw(_) = event {
            self.drawn_print = self.state.print.revision;
        }
        switch_watch::mark("the widgets");
        self.follow_brush(cx);
        if self.host.is_some() {
            self.voice.pump(cx);
            let row = self.voice.ui(cx);
            if let Some(mut xr) = self.ui.widget(cx, ids!(xr)).borrow_mut::<xr::FabXr>() {
                xr.set_voice(row);
            }
            self.release_held_layout(cx);
            shell_chat::pump(self, cx);
            self.pump_review(cx);
            self.pump_xr(cx);
        }
        if let Event::Actions(_) | Event::MouseUp(_) | Event::KeyUp(_) = event {
            self.follow_selection(cx);
        }
        self.renew_drop_preview();
        // The caption draws in its own list: it follows the state after the event that changed
        // it, whose redraws re-record it (redraws in the draw itself are not seen).
        if !matches!(event, Event::Draw(_)) {
            let caption = Caption::of(&self.state);
            if self.caption.as_ref() != Some(&caption) {
                let bar = self.ui.widget(cx, ids!(caption));
                fab::ui::topbar::sync(cx, &bar, &caption);
                self.caption = Some(caption);
            }
            // the electronics views name what they show by the project's name
            if cx.has_global::<fab_elec_view::ElecLive>() {
                let live = cx.get_global::<fab_elec_view::ElecLive>();
                if live.title != self.state.ui.document_name {
                    live.title.clone_from(&self.state.ui.document_name);
                    live.chosen_bench = fab::view_prefs::load_bench(&live.title).map(Arc::from);
                    cx.redraw_all();
                    cx.new_next_frame();
                }
            }
        }
    }
}

impl App {
    /// A call whose views the review renderer draws: the capture of the round (a fab.variants
    /// call's variant, or its survey of the plate) and the view as the line, and the whole call's
    /// permille with the view's tracing in it (the row's bar and its one number); or that its
    /// views wait their turn. None: none to draw.
    fn drawing_progress(&self, id: u64) -> Option<(String, u16)> {
        let drawing = self.drawing.as_ref().filter(|d| d.job == id);
        if drawing.is_none() && !self.review_queue.iter().any(|(job, _)| *job == id) {
            return None;
        }
        let (survey, done, of) = self.variant_runs.get(&id).map_or((false, 0, 1), |run| run.progress());
        let of = of.max(1);
        let Some(d) = drawing else {
            return Some(("waiting for the renderer".into(), (done * 1000 / of) as u16));
        };
        let mut line = Vec::new();
        if self.variant_runs.contains_key(&id) {
            line.push(if survey { "survey of the plate".to_string() } else { format!("variant {} of {of}", (done + 1).min(of)) });
        }
        line.push(format!("view {} of {}", d.view + 1, d.views));
        let views = (d.view as f32 + d.traced.unwrap_or(0.0)) / d.views.max(1) as f32;
        let share = (done as f32 + views) / of as f32;
        Some((line.join(" · "), ((share * 1000.0).round() as u16).clamp(1, 1000)))
    }
}

impl App {
    /// `fab.test_print`'s mode and read: the Slicer's choice and what its slice does, which only
    /// the running app knows.
    fn test_print_answer(&mut self, cx: &mut Cx, args: &Value) -> chat::Answer {
        use fab_ai::out;
        if args.get("action").and_then(Value::as_str) == Some("mode") {
            let Some(on) = args.get("tests").and_then(Value::as_bool) else {
                return chat::Answer { ok: false, text: "{error: \"mode needs 'tests': true for the ticked test prints, false for the whole plate\"}".to_string(), images: Vec::new() };
            };
            self.print_action(cx, &fab::print::PrintAction::TestMode(on));
        }
        let p = &self.state.print;
        let (state, fraction) = p.slice_state();
        let sliced = p.preview.as_ref().filter(|pv| p.test_mode && !p.stale && !pv.pieces.is_empty());
        let mm3 = |v: [f64; 3]| out::arr(v.iter().map(|x| out::n(*x)));
        let figure = |seconds: f64, grams: f64| out::obj(vec![("seconds", out::n(seconds)), ("time", out::s(fab::print::format_time(seconds))), ("grams", out::n(grams))]);
        let pieces = self.state.test_prints.iter().map(|t| {
            let mut fields = vec![("name", out::s(t.name.clone())), ("object", out::s(t.object.clone())), ("on", out::flag(t.on)), ("at", mm3(t.center)), ("size", mm3(t.size)), ("turn", mm3(t.turn))];
            // How its piece lies: the model's degrees, on layers of its own; or as printed.
            fields.push(("pose", t.pose.map_or_else(|| out::s("as printed"), mm3)));
            fields.push(("brim", out::s(if t.brim { "its own" } else { "the profile's" })));
            fields.push(("uses", out::arr(fab::ui::test_prints::uses(&self.state, t).into_iter().map(|u| out::s(u.text)))));
            if let Some(f) = sliced.and_then(|pv| pv.pieces.iter().find(|f| f.name == t.name)) {
                fields.push(("print", figure(f.seconds, f.grams)));
                fields.push(("support_grams", out::n(f.support_grams)));
                fields.push(("brim_grams", out::n(f.brim_grams)));
                fields.push(("bed", out::arr(f.bed.iter().map(|v| mm3(*v)))));
                fields.push(("layers", out::arr([f.layers.0, f.layers.1].iter().map(|l| out::int(*l as i64)))));
                fields.push(("footprint", out::arr(f.footprint.iter().map(|v| out::n(*v)))));
                fields.push(("height_over_footprint", out::n(f.height_over_footprint)));
                // What the slice's checks flag of it, as they word it.
                let flags = p.report().issues.iter().filter(|i| i.object.as_ref().is_some_and(|o| f.objects.contains(o)));
                fields.push(("flags", out::arr(flags.map(|i| out::obj(vec![("kind", out::s(i.kind)), ("message", out::s(i.message.clone())), ("hint", out::s(i.hint.clone())), ("area_mm2", out::n(i.area_mm2))])))));
            }
            out::obj(fields)
        });
        let mut fields = Vec::new();
        // What the view shows of them, on the bed.
        if self.state.plate.whole.is_some() {
            fields.push(("view", out::arr(self.state.plate.parts.iter().map(|p| out::obj(vec![("name", out::s(p.name.clone())), ("object", out::s(p.object.clone())), ("drawn", out::flag(p.element.is_some())), ("bed", out::arr([p.min_mm, p.max_mm].iter().map(|v| mm3(*v))))])))));
            // The scene installed, and the plate revision it was made at when it is the pieces'
            // own: the pieces and their scene install one action apart (the scene goes through
            // ShellAction::SceneUpdated), and a read between the two finds the pieces' parts
            // missing from the plate's scene.
            fields.push(("plate_revision", out::int(self.state.plate.revision)));
            fields.push(("scene_revision", out::int(self.state.scene_revision)));
            if self.test_view.scene.as_ref().is_some_and(|s| Arc::ptr_eq(s, &self.state.scene)) {
                fields.push(("scene_for", out::int(self.test_view.revision)));
            }
            fields.push(("scene_elements", out::arr(self.state.scene.elements.iter().filter(|e| !e.name.starts_with(fab::api::CONTEXT_PREFIX)).map(|e| out::s(e.name.clone())))));
        }
        // Which slice this is, and which G-code: another slice has another id, another program another key.
        if let Some(pv) = p.preview.as_ref().filter(|_| !p.stale) {
            fields.push(("slice_id", out::int(pv.id)));
            if let Some(key) = pv.gcode_key.filter(|_| p.settled) {
                fields.push(("gcode_key", out::s(format!("{key:016x}"))));
            }
        }
        fields.extend([("slicing", out::s(if p.test_mode { "test prints" } else { "whole plate" })), ("state", out::s(state)), ("fraction", out::n(fraction as f64)), ("current", out::flag(sliced.is_some())), ("pieces", out::arr(pieces))]);
        if let Some(pv) = sliced {
            fields.push(("shared", figure(pv.shared.0, pv.shared.1)));
            fields.push(("total", figure(pv.total, pv.filament_use.iter().map(|f| f.grams).sum())));
        }
        fields.push(("whole_plate", match p.whole_figures {
            Some((seconds, grams)) => figure(seconds, grams),
            None => out::s("not sliced"),
        }));
        // How long the slice standing took, from the input to its settled G-code.
        if let (fab::print::SliceStatus::Done { seconds }, Some(_)) = (&p.status, sliced) {
            fields.push(("slice_seconds", out::n(*seconds)));
        }
        if let fab::print::SliceStatus::Failed(error) = &p.status {
            fields.push(("error", out::s(error.lines().next().unwrap_or("internal slicer error"))));
        }
        chat::Answer { ok: true, text: out::text(&out::obj(fields)), images: Vec::new() }
    }
}

impl App {
    /// A layout call of `turn` (its result `probe`) added to the row workspaces' layout
    /// histories: per workspace the depth before the turn's first change and after its last.
    fn record_turn_layout(&mut self, turn: u64, probe: &Workspaces, temp: Option<u64>) {
        let live = &self.state.ui.workspaces;
        let changed: Vec<_> = probe.list.iter().filter_map(|d| {
            let (before, after) = (live.history_len(d.id), probe.history_len(d.id));
            (after > before).then_some(TurnChange { id: d.id, before, after, mixed: false })
        }).collect();
        if temp.is_none() && changed.is_empty() {
            return;
        }
        let Some(layout) = self.turn_layouts.iter_mut().find(|layout| layout.turn == turn) else {
            self.turn_layouts.push(TurnLayout { turn, temp, changed });
            return;
        };
        layout.temp = temp.or(layout.temp);
        for change in changed {
            match layout.changed.iter_mut().find(|c| c.id == change.id) {
                Some(c) => {
                    c.mixed |= c.after != change.before;
                    c.after = change.after;
                }
                None => layout.changed.push(change),
            }
        }
    }

    /// fab.workspace and fab.workspace_edit, answered here on the UI thread. The edits are tried
    /// on a copy of the workspaces (all or nothing), then sent as the very actions the person's
    /// gestures send, which the next action pass applies as it applies theirs; the answer is the
    /// copy, which the same changes made. The read gives each area's rectangle as last drawn.
    /// O(statements x areas) and one copy of the workspace list per edit call.
    fn workspace_answer(&mut self, cx: &mut Cx, tool: &str, args: &Value, turn: u64, waited: Option<f64>) -> chat::Answer {
        use fab::ui::workspace_tool::{self, WsEdit};
        let refuse = |why: &str| chat::Answer { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(why)), images: Vec::new() };
        let ui = &self.state.ui;
        let offered = |d: &fab::workspaces::WorkspaceDef| ui.offered(d);
        if tool == "workspace_edit" || tool == "show" {
            // fab.show is the statements it stands for, sent the same way
            let (edits, shown) = if tool == "show" {
                match self.show_edits(args) {
                    Ok((edits, shown)) => (edits, Some(shown)),
                    Err(why) => return refuse(&why),
                }
            } else {
                let Some(edits) = args.get("edits").and_then(Value::as_str) else {
                    return refuse("edits: Splash statements, one change each");
                };
                (edits.to_string(), None)
            };
            let mut probe = ui.workspaces.clone();
            let mut mode = ui.plate_mode;
            let list = match workspace_tool::run(&edits, &mut probe, &mut mode, &offered) {
                Ok(list) => list,
                Err(why) => return refuse(&why),
            };
            // every focus found in the design before anything moves: all or nothing
            let mut points = Vec::new();
            for (instance, thing, board) in WsEdit::points(&list) {
                match self.point_at(instance, thing, board) {
                    Ok(point) => points.push(point),
                    Err(why) => return refuse(&format!("focus {}: {why}", thing.field().1)),
                }
            }
            let mut text = workspace_tool::describe(&probe, mode, &[], false, &offered);
            let lit = WsEdit::pointed(&list, &probe);
            // one call is one layout change: one "Undo layout change" takes it all back
            cx.action(ShellAction::LayoutGroup(true));
            for edit in &list {
                cx.action(edit.action());
            }
            cx.action(ShellAction::LayoutGroup(false));
            // the next layout call waits until these apply, so its copy starts from them
            self.layout_in_flight = true;
            if turn > 0 {
                let temp = list.iter().any(|edit| matches!(edit, WsEdit::List(ListOp::Temporary { .. }))).then(|| probe.temp_gen());
                self.record_turn_layout(turn, &probe, temp);
            }
            for point in points {
                match point {
                    Point::View(focus) => cx.action(fab_elec_view::ViewAction::Focus(focus)),
                    Point::Lane(tab, net) => cx.action(fab::ui::shell::ShowLane { tab, net }),
                }
            }
            if !lit.is_empty() {
                cx.action(fab::ui::shell::LightEditors(lit));
            }
            if let (Some(seconds), Some(open)) = (waited, text.find('{')) {
                let note = format!("the person's pointer or a key was down: applied when it let go, {seconds:.1} s later");
                text.insert_str(open + 1, &format!("\n  waited: {}", fab_ai::out::quote(&note)));
            }
            if let (Some(shown), Some(open)) = (shown, text.find('{')) {
                text.insert_str(open + 1, &format!("\n  shown: {}", fab_ai::out::quote(&shown)));
            }
            return chat::Answer { ok: true, text, images: Vec::new() };
        }
        let catalog = args.get("catalog").and_then(Value::as_bool).unwrap_or(false);
        let rects = self.ui.widget(cx, ids!(shell)).borrow::<fab::ui::shell::FabShell>().map(|shell| shell.area_rects(cx)).unwrap_or_default();
        let ui = &self.state.ui;
        let text = workspace_tool::describe(&ui.workspaces, ui.plate_mode, &rects, catalog, &|d| ui.offered(d));
        chat::Answer { ok: true, text, images: Vec::new() }
    }

    /// What `focus` points `instance` at, found in the loaded design (on `board` when named): a
    /// net, a part or a pin, as the shared selection's views show it, or a net's lane in a Scope.
    /// O(parts + nets).
    fn point_at(&self, instance: fab::ui::areas::Instance, thing: &fab::ui::workspace_tool::Thing, board: Option<&str>) -> Result<Point, String> {
        use fab::ui::editors::Editor;
        use fab_elec_design::{Focus, FocusKind, FocusOrigin};
        let (kind, sel) = self.find_thing(thing, board)?;
        let text = thing.field().1;
        let word = match kind { FocusKind::Net => "net", FocusKind::Part => "part", _ => "pin" };
        match (instance.editor, kind) {
            (Editor::Scope, FocusKind::Net) => Ok(Point::Lane(instance.tab(), text.to_string())),
            (Editor::Scope, _) => Err(format!("a Scope shows nets; {text} is a {word}")),
            (Editor::Device, FocusKind::Part) | (Editor::Circuit | Editor::Board, _) => Ok(Point::View(Focus { kind, sel, origin: FocusOrigin::Ai })),
            (editor, _) => Err(format!("the {} shows parts; {text} is a {word}", editor.title())),
        }
    }

    /// The net, part or pin `thing` names in the loaded design (on `board` when named), and its
    /// kind; a name alone is a part first, then a net, then a pin. O(parts + nets).
    fn find_thing(&self, thing: &fab::ui::workspace_tool::Thing, board: Option<&str>) -> Result<(fab_elec_design::FocusKind, fab_elec_core::Sel), String> {
        use fab::ui::workspace_tool::Thing;
        use fab_elec_core::{CompRef, NetRef, PinIx, PinRef, Sel};
        use fab_elec_design::FocusKind;
        let Some(design) = self.elec.as_ref().and_then(|elec| elec.design()) else {
            return Err("no electronics design is loaded".into());
        };
        let names = || design.units.iter().map(|unit| format!("\"{}\"", unit.name)).collect::<Vec<_>>().join(", ");
        if let Some(board) = board.filter(|b| !design.units.iter().any(|unit| unit.name == *b)) {
            return Err(format!("no board \"{board}\"; the boards are {}", names()));
        }
        let units = || design.units.iter().filter(|unit| board.is_none_or(|b| unit.name == b));
        let (field, text) = thing.field();
        let parts = |name: &str| units().flat_map(|unit| unit.circuit.comps.iter().filter(move |c| c.refdes == name || c.path == name).map(move |c| (unit, c))).collect::<Vec<_>>();
        let nets = || units().flat_map(|unit| unit.circuit.nets.iter().filter(|net| net.name == text).map(move |net| (unit.name.as_str(), Sel::Net(NetRef { board: unit.id, net: net.id })))).collect::<Vec<_>>();
        let pins = || -> Result<Vec<(&str, Sel)>, String> {
            let Some((part, pin)) = text.rsplit_once('.') else { return Ok(Vec::new()) };
            let mut hits = Vec::new();
            for (unit, comp) in parts(part) {
                let at = comp.part.pins.iter().position(|p| p.name == pin).ok_or_else(|| format!("{} has no pin {pin}", comp.refdes))?;
                hits.push((unit.name.as_str(), Sel::Pin(PinRef { board: unit.id, comp: comp.id, pin: PinIx(at as u16) })));
            }
            Ok(hits)
        };
        let comps = || parts(text).into_iter().map(|(unit, c)| (unit.name.as_str(), Sel::Comp(CompRef { board: unit.id, comp: c.id }))).collect::<Vec<_>>();
        // the one hit; one on several boards asks for the board
        let one = |hits: Vec<(&str, Sel)>, what: &str| -> Result<Option<Sel>, String> {
            match hits.as_slice() {
                [] => Ok(None),
                [(_, sel)] => Ok(Some(sel.clone())),
                [(a, _), (b, _), ..] => Err(format!("{text} is a {what} on {a} and on {b}: add board: \"{a}\"")),
            }
        };
        let found = match thing {
            Thing::Net(_) => one(nets(), "net")?,
            Thing::Part(_) => one(comps(), "part")?,
            Thing::Pin(_) => one(pins()?, "pin")?,
            // a part wins over a net of the same name, a net over a pin, as the elec reads have it
            Thing::Any(_) => match one(comps(), "part")? {
                Some(sel) => Some(sel),
                None => match one(nets(), "net")? {
                    Some(sel) => Some(sel),
                    None => one(pins()?, "pin")?,
                },
            },
        };
        let what = if field == "focus" { "net, part or pin" } else { field };
        let on = board.map_or(String::new(), |b| format!(" on {b}"));
        let sel = found.ok_or_else(|| format!("no {what} {text}{on}; fab.elec_read elec.design.read lists them"))?;
        let kind = match sel {
            Sel::Net(_) => FocusKind::Net,
            Sel::Comp(_) => FocusKind::Part,
            _ => FocusKind::Pin,
        };
        Ok((kind, sel))
    }

    /// `fab.show`'s things, each found in the design for its kind, as the `fab.workspace_edit`
    /// statements that show them, and what the answer says it showed. O(things x (parts + nets)).
    fn show_edits(&self, args: &Value) -> Result<(String, String), String> {
        use fab::ui::workspace_tool::{self, Shown, Thing};
        use fab_elec_design::FocusKind;
        let Some(Value::Arr(items)) = args.get("things") else {
            return Err("things: the nets, parts and pins to show, e.g. [\"VCC\", {part: \"U1\"}]".into());
        };
        if items.is_empty() {
            return Err("things: name at least one net, part or pin".into());
        }
        let mut things = Vec::new();
        for (n, item) in items.iter().enumerate() {
            let at = |why: &str| format!("things[{n}]: {why}");
            let (thing, board) = match item {
                Value::Str(name) => (Thing::Any(name.clone()), None),
                Value::Obj(fields) => {
                    let mut thing = None;
                    let mut board = None;
                    for (key, value) in fields {
                        let Value::Str(text) = value else { return Err(at(&format!("{key}: expected a name"))) };
                        let kind = match key.as_str() {
                            "net" => Thing::Net(text.clone()),
                            "part" => Thing::Part(text.clone()),
                            "pin" => Thing::Pin(text.clone()),
                            "board" => {
                                board = Some(text.clone());
                                continue;
                            }
                            _ => return Err(at(&format!("unknown field {key}: net, part, pin or board"))),
                        };
                        if thing.replace(kind).is_some() {
                            return Err(at("one of net, part or pin"));
                        }
                    }
                    (thing.ok_or_else(|| at("expected net, part or pin"))?, board)
                }
                _ => return Err(at("a name, or {net|part|pin: name, board: name}")),
            };
            let (kind, _) = self.find_thing(&thing, board.as_deref()).map_err(|why| at(&why))?;
            let text = thing.field().1.to_string();
            let thing = match kind {
                FocusKind::Net => Thing::Net(text),
                FocusKind::Part => Thing::Part(text),
                _ => Thing::Pin(text),
            };
            things.push(Shown { thing, board });
        }
        let words = things.iter().map(Shown::words).collect::<Vec<_>>().join(", ");
        let note = match args.get("note").and_then(Value::as_str).map(str::trim) {
            Some(note) if !note.is_empty() => note.to_string(),
            _ => words.chars().next().map(|c| c.to_uppercase().chain(words.chars().skip(1)).collect()).unwrap_or_default(),
        };
        let ui = &self.state.ui;
        let (edits, editors) = workspace_tool::show_statements(&ui.workspaces, &|d| ui.offered(d), &things, &note);
        let titles = editors.iter().map(|e| e.title()).collect::<Vec<_>>().join(", ");
        let shown = if edits.starts_with("temporary(") {
            format!("{words} in the temporary layout {}: {titles}", fab_ai::out::quote(&note))
        } else {
            format!("{words} on screen: {titles}")
        };
        Ok((edits, shown))
    }

    /// The first layout change held while the person's hand was down or the one before it was on
    /// its way, once it lets go and that applied: one per event, so each sees the change before it.
    fn release_held_layout(&mut self, cx: &mut Cx) {
        if self.hand.down() {
            self.held_layout.iter_mut().for_each(|held| held.hand = true);
            return;
        }
        if self.layout_in_flight {
            return;
        }
        let Some(held) = self.held_layout.pop_front() else { return };
        let waited = held.hand.then(|| Cx::time_now() - held.since);
        let answer = self.workspace_answer(cx, &held.tool, &held.args, held.turn, waited);
        self.calls.finish(held.id, answer);
    }
}

/// Where a `focus` points: the shared selection (the Schematic, the Board, the 3D view answer it)
/// or a net's lane in the Scope whose dock tab it is.
enum Point {
    View(fab_elec_design::Focus),
    Lane(LiveId, String),
}

/// What of the person's hand is down (buttons, fingers, keys): an AI layout change waits for it to
/// let go (design FABWS: never under the person's hand). A window losing focus lets go of all of
/// it, as its releases may never come. O(keys held) per key event, O(1) otherwise.
#[derive(Default)]
struct Hand {
    buttons: u32,
    fingers: bool,
    keys: Vec<KeyCode>,
}

impl Hand {
    fn down(&self) -> bool {
        self.buttons > 0 || self.fingers || !self.keys.is_empty()
    }

    fn event(&mut self, event: &Event) {
        match event {
            Event::MouseDown(_) => self.buttons += 1,
            Event::MouseUp(_) => self.buttons = self.buttons.saturating_sub(1),
            Event::TouchUpdate(e) => {
                self.fingers = e.touches.iter().any(|t| !matches!(t.state, event::TouchState::Stop | event::TouchState::Cancel));
            }
            Event::KeyDown(e) if !e.is_repeat && !self.keys.contains(&e.key_code) => self.keys.push(e.key_code),
            Event::KeyUp(e) => self.keys.retain(|key| *key != e.key_code),
            Event::WindowLostFocus(_) | Event::Pause | Event::Background => *self = Hand::default(),
            _ => {}
        }
    }
}

impl ChatHost for App {
    fn chat_dock(&mut self) -> &mut ChatDock {
        &mut self.chat
    }

    fn chat_window(&self) -> WidgetRef {
        self.ui.clone()
    }

    fn chat_services(&mut self) -> Vec<ServiceManifest> {
        chat::services()
    }

    /// The turn goes with the call: its electronics steps carry it (`By::Ai`), and its model
    /// steps are matched to it when they land (`App::host_done`).
    fn chat_turn_call(&mut self, cx: &mut Cx, tool: &str, args: Value, turn: u64) -> u64 {
        self.chat_turn = turn;
        let weighs = chat::short_tool(tool) == "measure" && args.get("what").and_then(Value::as_str) == Some("mass");
        let id = self.chat_call(cx, tool, args);
        self.chat_turn = 0;
        if weighs {
            self.mass_ai_calls.push(id);
        }
        self.turn_undo.call(id, turn);
        self.follow_turn();
        id
    }

    fn chat_turn_started(&mut self, cx: &mut Cx, _turn: u64) {
        self.mass_turn_started(cx);
    }

    fn chat_call(&mut self, cx: &mut Cx, tool: &str, args: Value) -> u64 {
        let tool = chat::short_tool(tool).to_string();
        if let Err(refused) = fab_ai::check_arguments(&tool, &args) {
            let Some(host) = &mut self.host else { return 0 };
            let id = host.reserve_call_id();
            self.calls.pending.insert(id, tool);
            self.calls.finish(id, chat::Answer { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(&refused)), images: Vec::new() });
            return id;
        }
        // Another document for the bed: what the person typed or is dragging lands on this one first.
        if switches_bed(&tool, &args) {
            self.bed_asked();
            self.send_typing();
            if self.gesture_live() {
                let Some(host) = &mut self.host else { return 0 };
                let id = host.reserve_call_id();
                self.calls.pending.insert(id, tool.clone());
                self.held_switches.push((id, tool, args));
                return id;
            }
        }
        if matches!(tool.as_str(), "apply" | "script" | "exact" | "edges" | "weave" | "place" | "lay_flat" | "new") || tool == fab_ai::img2mesh::TOOL {
            self.startup_monitor_done = true;
        }
        let Some(host) = &mut self.host else { return 0 };
        if fab_ai::ELEC_TOOLS.iter().any(|(name, _, _)| *name == tool) || tool == fab_ai::ELEC_TEXT_TOOL {
            let id = host.reserve_call_id();
            self.calls.pending.insert(id, tool.clone());
            self.elec_call(cx, id, &tool, &args);
            return id;
        }
        if tool == "print" {
            // Opens the confirm row only: the user's Start print is the one way to start a print.
            let id = host.reserve_call_id();
            self.calls.pending.insert(id, tool);
            let verdict = match &self.state.print.print_check {
                _ if self.state.print.printer.is_none() => "No printer is linked.".to_string(),
                _ if self.state.print.stale => "The slice is out of date: slice again first.".to_string(),
                Some(Ok(what)) => format!("The Print editor's confirm row is open; the safety check {what}. The user must press Start print."),
                Some(Err(why)) => format!("Refused: {why}"),
                None => "Slice the plate first.".to_string(),
            };
            cx.action(fab::print::PrintAction::AskPrint(true));
            self.calls.finish(id, chat::Answer { ok: true, text: verdict, images: Vec::new() });
            return id;
        }
        if tool == "workspace" || tool == "workspace_edit" || tool == "show" {
            let id = host.reserve_call_id();
            self.calls.pending.insert(id, tool.clone());
            // never under the person's hand: a layout change while a button, a finger or a key is
            // down applies once it lets go, after any held before it; and every call after the
            // change before it applied, so it starts from (or reads) that one's result
            if (tool != "workspace" && self.hand.down()) || self.layout_in_flight || !self.held_layout.is_empty() {
                let hand = self.hand.down();
                self.held_layout.push_back(HeldLayout { id, turn: self.chat_turn, tool, args, since: Cx::time_now(), hand });
                return id;
            }
            let answer = self.workspace_answer(cx, &tool, &args, self.chat_turn, None);
            self.calls.finish(id, answer);
            return id;
        }
        if tool == "voice" {
            let id = host.reserve_call_id();
            self.calls.pending.insert(id, tool);
            let listen = match &args {
                Value::Obj(fields) => fields.iter().find(|(k, _)| k == "listen").and_then(|(_, v)| if let Value::Bool(b) = v { Some(*b) } else { None }),
                _ => None,
            };
            let answer = match listen {
                Some(_) => self.voice.toggle(cx, listen),
                None => Ok(self.voice.describe(cx)),
            };
            let (ok, text) = match answer {
                Ok(text) => (true, text),
                Err(text) => (false, text),
            };
            self.calls.finish(id, chat::Answer { ok, text: format!("{{voice: {}}}", fab_ai::out::quote(&text)), images: Vec::new() });
            return id;
        }
        if tool == "test_print" && matches!(args.get("action").and_then(Value::as_str), Some("mode" | "read")) {
            let id = host.reserve_call_id();
            self.calls.pending.insert(id, tool);
            let answer = self.test_print_answer(cx, &args);
            self.calls.finish(id, answer);
            return id;
        }
        if tool == "force" {
            let id = host.reserve_call_id();
            self.calls.pending.insert(id, tool);
            match self.show_force(cx, &args) {
                Ok(()) => self.force_calls.push((id, false)),
                Err(why) => self.calls.finish(id, chat::Answer { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(&why)), images: Vec::new() }),
            }
            return id;
        }
        if tool == "slice" {
            use fab_ai::out;
            let id = host.reserve_call_id();
            self.calls.pending.insert(id, tool);
            let p = &self.state.print;
            let (state, fraction) = p.slice_state();
            let mut fields = vec![("slicing", out::s(if p.test_mode { "test prints" } else { "whole plate" })), ("state", out::s(state)), ("fraction", out::n(fraction as f64)), ("whole", out::flag(p.preview.as_ref().is_some_and(|pv| pv.chunks.is_empty()))), ("on_screen", out::flag(self.drawn_print == p.revision))];
            // the live slice's input and served revisions: a new slice moves them
            if let Some((revision, served)) = live_slice::revisions() {
                fields.push(("revision", out::int(revision)));
                fields.push(("served", out::int(served)));
            }
            // Where the shown slice waits for items placed into it: the layer each pause comes
            // before (1-based, as the Sliced view counts), its print z and what goes in.
            if let Some(pv) = p.preview.as_ref().filter(|pv| !pv.pauses.is_empty()) {
                fields.push(("pauses", out::arr(pv.pauses.iter().map(|q| {
                    out::obj(vec![("before_layer", out::int(q.layer as i64 + 1)), ("z", out::n(q.z as f64)), ("place", out::arr(q.items.iter().map(|i| out::s(i.clone()))))])
                }))));
            }
            // The heat-set inserts to melt in after it: how many of a size into which part, and
            // the iron's temperature (the print temperature of the part's filament; 0: not known).
            if let Some(pv) = p.preview.as_ref().filter(|pv| !pv.melts.is_empty()) {
                fields.push(("melt_after", out::arr(pv.melts.iter().map(|m| {
                    out::obj(vec![("size", out::s(m.size.clone())), ("count", out::int(m.count as i64)), ("into", out::s(m.into.clone())), ("iron_c", out::int(m.iron as i64))])
                }))));
            }
            match &p.status {
                fab::print::SliceStatus::Running { stage, .. } => fields.push(("stage", out::s(stage.clone()))),
                fab::print::SliceStatus::Failed(error) => fields.push(("error", out::s(error.lines().next().unwrap_or("internal slicer error")))),
                _ => {}
            }
            if args.get("drawn").and_then(Value::as_bool) == Some(true) {
                if let Some(revision) = args.get("revision").and_then(Value::as_u64) {
                    let from = args.get("from").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
                    let (tiles, left) = live_slice::drawn_tiles(revision, from);
                    fields.push(("tiles", out::arr(tiles.iter().map(|t| {
                        out::obj(vec![("part", out::int(t.part)), ("layer", out::int(t.layer)), ("revision", out::int(t.revision)), ("ms", out::int(t.ms.round() as i64))])
                    }))));
                    fields.push(("left", out::int(left)));
                } else {
                    fields.push(("drawn", out::arr(live_slice::drawn_runs().iter().map(|(revision, run)| {
                        out::obj(vec![("revision", out::int(*revision)), ("frames", out::int(run.frames)), ("ghosts", out::int(run.ghosts)), ("before_whole", out::int(run.before_whole)), ("tiles", out::int(run.tiles.len()))])
                    }))));
                }
            }
            self.calls.finish(id, chat::Answer { ok: true, text: out::text(&out::obj(fields)), images: Vec::new() });
            return id;
        }
        if tool == "printer" {
            let id = host.reserve_call_id();
            self.calls.pending.insert(id, tool);
            let colours = print::colours_json(&self.state.print.colors);
            let text = self.state.print.printer.as_ref().map(|p| p.inspect(colours.clone()).to_json())
                .unwrap_or_else(|| makepad_strict_json::obj(vec![("status", makepad_strict_json::s("Not configured")), ("ams", makepad_strict_json::Value::Arr(Vec::new())), ("colours", colours)]).to_json());
            self.calls.finish(id, chat::Answer { ok: true, text, images: Vec::new() });
            return id;
        }
        if tool == fab_ai::img2mesh::TOOL {
            let id = host.reserve_call_id();
            self.calls.pending.insert(id, tool);
            let started = fab_ai::img2mesh::parse_args(&args).and_then(|request| {
                let path = std::path::PathBuf::from(&request.image);
                if path.is_file() {
                    return Ok((request, img2mesh::Picture::File(path)));
                }
                // A picture the person sent in this chat, by its name: as it was dropped.
                let pictures = cx.global::<makepad_app_aichat::ChatPictures>();
                if let Some(picture) = pictures.get(Some(&request.image)) {
                    return Ok((request, img2mesh::Picture::Sent(picture.clone())));
                }
                Err(no_such_picture(&request.image, pictures))
            });
            match started {
                Ok((request, picture)) => {
                    self.img2mesh.start(Some(id), request, picture);
                    }
                Err(error) => self.calls.finish(id, chat::Answer { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(&error)), images: Vec::new() }),
            }
            return id;
        }
        // A picture the person sent in the chat, by its name: the call goes once its file is.
        let picture = match picture_key(&tool) {
            Some(key) => sent_picture(cx, key, &args),
            None => Ok(None),
        };
        match picture {
            Ok(None) => {}
            Ok(Some(picture)) => {
                let id = host.reserve_call_id();
                self.calls.pending.insert(id, tool.clone());
                self.held_pictures.push((id, tool, args, picture));
                self.release_pictures();
                return id;
            }
            Err(error) => {
                let id = host.reserve_call_id();
                self.calls.pending.insert(id, tool);
                self.calls.finish(id, chat::Answer { ok: false, text: format!("{{error: {}}}", fab_ai::out::quote(&error)), images: Vec::new() });
                return id;
            }
        }
        let id = host.chat_call(&tool, args);
        self.calls.pending.insert(id, tool);
        id
    }

    fn chat_voice(&mut self, cx: &mut Cx) -> makepad_app_aichat::VoiceUi {
        self.voice.ui(cx)
    }

    fn chat_voice_toggled(&mut self, cx: &mut Cx) {
        // (a refusal is a line in the chat, already said)
        let _ = self.voice.toggle(cx, None);
    }

    fn chat_voice_says(&mut self, _cx: &mut Cx) -> Vec<makepad_app_shell::chat::VoiceSays> {
        self.voice.take_says()
    }

    fn chat_attachment_removed(&mut self, _cx: &mut Cx, id: &str) {
        self.pick_removed(id);
    }

    fn chat_attachment_hovered(&mut self, cx: &mut Cx, id: Option<&str>) {
        self.pick_lit(cx, id);
    }

    fn chat_cancel(&mut self, _cx: &mut Cx, id: u64) {
        if let Some(elec) = &mut self.elec {
            if elec.cancel_call(id) {
                self.elec_answers();
            }
        }
        if let Some(at) = self.held_pictures.iter().position(|held| held.0 == id) {
            self.held_pictures.remove(at);
            self.calls.finish(id, chat::Answer { ok: false, text: "{error: \"cancelled before its picture was kept\"}".into(), images: Vec::new() });
        }
        if let Some(job) = self.img2mesh.of_call(id).map(|r| r.job) {
            self.img2mesh.cancel(job);
        }
        // a force read stops waiting; the study itself goes on, as the view shows it
        if let Some(at) = self.force_calls.iter().position(|c| c.0 == id) {
            self.force_calls.remove(at);
            self.calls.finish(id, chat::Answer { ok: false, text: "{error: \"cancelled before the study was up to date\"}".into(), images: Vec::new() });
        }
    }

    fn chat_answer(&mut self, call_id: &str, id: u64) -> Option<ToolResult> {
        let answer = self.calls.answers.remove(&id)?;
        let mut result = chat::tool_result(call_id, answer);
        if let Some(task) = self.calls.tasks.remove(&id) {
            result.task = Some(task);
        }
        Some(result)
    }

    fn chat_task(&self, id: u64) -> Option<TaskCard> {
        self.img2mesh.of_call(id).map(|r| r.task()).or_else(|| self.calls.tasks.get(&id).cloned())
    }

    /// Recheck the turn's offer at the time of the press.
    fn chat_turn_action(&mut self, cx: &mut Cx, turn: u64, id: &str) {
        self.follow_turn();
        if matches!(id, "ws_keep" | "ws_back" | "ws_undo") {
            if !self.chat.has_turn(turn) {
                return;
            }
            let Some(layout) = self.turn_layouts.iter_mut().find(|layout| layout.turn == turn) else { return };
            let workspaces = &self.state.ui.workspaces;
            match id {
                "ws_keep" | "ws_back" if layout.temp_open(workspaces) => {
                    layout.temp = None;
                    let op = if id == "ws_keep" { ListOp::Keep(None) } else { ListOp::Dismiss };
                    cx.action(ShellAction::EditWorkspaces(op));
                }
                "ws_undo" if layout.undo_offer(workspaces) == Some(Ok(())) => {
                    // back to before the turn's first change: one step per change it made
                    cx.action(ShellAction::LayoutGroup(true));
                    for change in layout.changed.drain(..) {
                        for _ in change.before..change.after {
                            cx.action(ShellAction::EditWorkspaces(ListOp::UndoLayout(change.id)));
                        }
                    }
                    cx.action(ShellAction::LayoutGroup(false));
                }
                _ => {}
            }
            return;
        }
        let (model, elec) = self.history_tops();
        match (id, self.turn_undo.offer(turn, model, elec)) {
            ("undo", Some(turn_undo::Offer::Undo(presses))) => self.press_history(cx, true, presses),
            ("redo", Some(turn_undo::Offer::Redo(presses))) => self.press_history(cx, false, presses),
            _ => {}
        }
    }

    fn chat_task_action(&mut self, cx: &mut Cx, action: &str) {
        if let Some(object) = action.strip_prefix("select:") {
            if let Some(element) = self.state.plate.parts.iter().find(|p| p.object == object).and_then(|p| p.element) {
                cx.action(ShellAction::SelectOnly(element));
                return;
            }
        } else if let Some(path) = action.strip_prefix("open:") {
            let path = std::path::Path::new(path).canonicalize();
            let made = model_host::documents_dir().canonicalize();
            if let (Ok(path), Ok(made)) = (path, made) {
                if path.starts_with(&made) && path != made {
                    self.open_project(path);
                    return;
                }
            }
        }
        log!("fab: unknown or unavailable task action {action}");
    }

    /// What a call is doing, from its job on the caption's strip: the same words and fraction.
    fn chat_progress(&self, id: u64) -> Option<(String, u16)> {
        let tool = self.calls.pending.get(&id)?;
        if let Some(r) = self.img2mesh.of_call(id) {
            return Some((r.line(), r.permille));
        }
        if tool == fab_ai::img2mesh::TOOL {
            return Some(("putting the part on the plate".into(), 1000));
        }
        if let Some(drawn) = self.drawing_progress(id) {
            return Some(drawn);
        }
        // the chat's row names the call: its job's stage, else what the job is
        let job = self.host.as_ref().and_then(|h| h.call_job(id)).and_then(|j| makepad_widgets::jobs::board().get(j));
        match job {
            Some(job) if job.end.is_running() => {
                let stage = job.stage_words();
                let line = if stage.is_empty() { job.title.strip_prefix("AI: ").unwrap_or(&job.title).to_string() } else { stage };
                Some((line, job.fraction.map_or(0, |f| (f * 1000.0) as u16)))
            }
            // its job is through, its answer on its way: what is being made of its views
            Some(_) => Some((self.after_views.get(&id).copied().unwrap_or("answering").to_string(), 0)),
            None => Some((model_host::tool_title(tool), 0)),
        }
    }

    fn chat_target(&mut self, _cx: &mut Cx) -> (makepad_ai_services::Target, Vec<makepad_ai_services::Suggestion>) {
        let (parts, issues) = self.plate.as_ref().map(|e| (e.plate.parts.len(), e.blocking_issues())).unwrap_or((0, 0));
        let (mut target, suggestions) = chat::target(&self.status.name, parts, issues);
        // the person's marks ride with every line while they are on the view
        if let (Some(text), true) = (&self.mark_text, fab::tools::annotate::count() > 0) {
            target.context.push_str("\n");
            target.context.push_str(text);
        }
        for (i, notice) in self.state.ui.notices.iter().enumerate() {
            target.context.push_str(if i == 0 { "\nThe views say now: " } else { "; " });
            target.context.push_str(match notice.view {
                NoticeView::Plate => "Plate",
                NoticeView::Board => "Board",
                NoticeView::Circuit => "Schematic",
                NoticeView::Device => "3D view",
                NoticeView::Scope => "Scope",
            });
            target.context.push_str(": ");
            target.context.push_str(&notice.text);
            if notice.standing {
                target.context.push_str(" (stands until its cause is gone)");
            }
        }
        // and what they picked, as chips
        target.attachments = picks::attachments(&self.picks);
        (target, suggestions)
    }

    fn chat_marks(&mut self, _cx: &mut Cx) -> Option<makepad_app_aichat::MarksUi> {
        Some(makepad_app_aichat::MarksUi { offered: true, on: self.state.ui.marker, count: fab::tools::annotate::count(), resolving: fab::tools::annotate::resolving() })
    }

    fn chat_marker(&mut self, cx: &mut Cx, on: bool) {
        self.state.ui.marker = on;
        fab::ui::redraw(cx, &[ids!(tool_overlay)]);
    }

    fn chat_marks_clear(&mut self, cx: &mut Cx) {
        self.clear_marks(cx);
    }

    fn chat_marks_capture(&mut self, cx: &mut Cx) -> Option<Texture> {
        self.mark_capture(cx)
    }

    fn chat_marks_picture(&mut self, cx: &mut Cx, pixels: Option<&mut shell_chat::picture::Pixels>) {
        self.mark_picture(cx, pixels);
    }

    fn chat_marks_text(&mut self, _cx: &mut Cx) -> Option<Result<(), String>> {
        match self.mark_read.take() {
            Some((call, None)) => {
                self.mark_read = Some((call, None));
                None
            }
            Some((_, Some(Ok(text)))) => {
                self.mark_text = Some(text);
                Some(Ok(()))
            }
            Some((_, Some(Err(why)))) => {
                self.mark_text = Some(format!("The marks could not be read: {why}"));
                Some(Err(why))
            }
            None => Some(Ok(())),
        }
    }

    fn chat_marks_sent(&mut self, cx: &mut Cx, line: usize) {
        self.marks_sent(cx, line);
    }
}

#[cfg(test)]
mod desktop_style_tests {
    include!("../../../../../widgets/tests/support/app_style.rs");
}

/// The Test prints view: the ticked pieces where they print, for as long as Design shows them.
#[derive(Default)]
struct TestView {
    /// Tells the newest pieces asked for from any before them.
    gen: u64,
    /// What they were last asked from. None: the view is the plate's.
    asked: Option<TestViewOf>,
    /// The scene of the pieces showing.
    scene: Option<Arc<fab::model::Scene>>,
    /// The plate revision the pieces showing (and their scene) were made at.
    revision: u64,
    /// The plate's elements picked when it showed, by name.
    selected: Vec<String>,
}

/// What a Test prints view is made from.
struct TestViewOf {
    content: [u8; 32],
    view: fab_model::eval::View,
    tests: Vec<piece::TestPrint>,
    machine: fab_plate::machine::Machine,
}

/// The pieces asked for, cut and ready to show.
struct TestViewMade {
    gen: u64,
    plate: fab_model::eval::Evaluated,
    scene: Arc<fab::model::Scene>,
}

impl std::fmt::Debug for TestViewMade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TestViewMade({}, {} parts)", self.gen, self.plate.plate.parts.len())
    }
}

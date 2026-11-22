//! Fab in a headset, in mixed reality. The always-available caption control starts the
//! framework's XR server and pairs a headset by its PIN. This widget draws a forward colour +
//! depth XR pass over the room (passthrough); the server takes it at the pass's end. The
//! optional Headset panel mirrors its left eye. The desktop keeps its own cameras and UI.
//!
//! The bed is set down at real size in front of the head, below the eyes and facing them, once
//! the head settles in each tracking space (a headset session, a recenter: the headset's origin
//! moves between them, and a bed left at its old place would be anywhere in the room); grabbing
//! empty space with both hands moves, turns and scales it (shared Free navigation), so it can be
//! set down on the real desk. Holding the menu button (or the hand's menu gesture) puts it back
//! in front of the head the same way. A handheld (a phone or tablet) sees less far down: its bed
//! goes on its view's centre ray instead, and its "Place here" puts the bed on the real surface
//! the person tapped (a menu hold whose left aim's tip is that point). On it: the parts (one model each, so a grabbed
//! part moves alone), the printability markers, and in Preview the slice with its current layer
//! highlighted and the layers above it hidden. The wrist menu has one row per workspace: the
//! desktop's own (`AppState::ui.workspaces`, switched the way the caption switches it), and only
//! those rows change it. The hands edit through [`crate::xr_edit`]: a grab moves parts and commits
//! one edit through the model worker (the desktop's edit path, one undo step), and the live slice
//! ([`crate::live_slice`]) shows the moving plate's layer contours while the grab lasts and its
//! printability once let go, until the committed edit's reslice arrives.
//!
//! In Mechanics the sim's newest snapshot (`SceneState::moves`, stepped on the sim's own thread)
//! poses what is drawn: a part's model or field, and a part of links as one instance of its link
//! model per link. The hands take bodies through [`crate::xr_mech`].
//!
//! The SDF parts share the viewport's CPU tile tracers through `AppState::sdf`.
//! Both eye cameras guide refinement; the lit tiles draw in the XR pass with real depth.
//! Their meshes hide while the field look is active.
//!
//! Interaction uses a filtered copy (aims, tips, menu pointing and edits). Navigation and
//! controller/hand rendering keep the original tracking. Drags claim presses before menu
//! activation and selection.
//!
//! Main-loop cost: nothing until an XR display exists. Per headset frame: the edit step
//! (constant per hand, one bounds test per part), the model list rebuilt only when a part or the
//! plate moved or the sim published a snapshot (then one transform per body), one retained bead or contour draw, at most two nozzle heads, the wrist menu and
//! readout cards (`makepad_xr_ui`) redrawn only when what they show changes. Geometry is prepared on the model, slice and live slice
//! workers; nothing here builds per frame.

use crate::live_slice::{LiveResult, LiveSlice};
use crate::xr_edit::{self, Affine, Command, EditOut, SliceAxis, XrEdit};
use crate::xr_mech::{MechOut, SceneBodies, XrMech};
use fab::api::{ArmAction, ForceAction, ForceField, MechAction, MechLight, MechScene, MechTool, Family, PlateMode, Workspace};
use fab::model::ElementId;
use fab::print::{PrintAction, PrinterControl};
use fab::viewport::print::{draw_overlay_placed, draw_print_placed, DrawFabFar, DrawFabHead, DrawFabPath, DrawFabStrip, FarTiers, PrintFrame, PrintPalette, PrintPlacement};
use fab::fab_sdf_tiles::draw::TileLight;
use fab_model::eval::Evaluated;
use makepad_render::shadow::{BASE_SHADOW_ALPHA, MAX_SHADOW_DROP};
use makepad_render::shadow_mesh::{build_blob_shadow, Receiver, ShadowMeshBuilder, SHADOW_VERTEX_FLOATS};
use makepad_render::{preview_scene_state, SunLight, DrawSceneAlpha, DrawSceneCube, DrawSceneShadow, DrawSceneSkinned, DrawSceneSky, DrawSceneTerrain, ModelInstance, PreparedStaticPreview, PreviewLook, PreviewStage, Renderer, SceneDraws};
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle, TaskPool};
use makepad_widgets::*;
use makepad_xr_nav::{synth_controllers, ClipBox, XrNav, XrNavConfig, XrNavMode, XrNavView};
use makepad_xr_stream::XrServer;
use makepad_xr_ui::haptic;
use makepad_xr_ui::input::{self, Buttons};
use makepad_xr_ui::{Card, EditSink, Height, InteractionFilter, MenuStep, Tone, Width, XrPanel, XrShapes, XrTips, XrWristMenu};
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};

/// The bed's footprint centre (m): table height above the floor, in front of the start pose.
const TABLE: [f32; 3] = [0.0, 0.9, -0.6];
/// How far below the eyes a view reset sets the bed's top (m): under the hands, from the head
/// alone (the headset's floor can be off, which would put a table-height bed at eye level).
const RESET_DROP_M: f32 = 0.45;
/// How far along a handheld's view its bed is set down (m). A phone or tablet held upright sees
/// about 32 degrees down, so the headset's place (37 degrees down) is under its bottom edge; on
/// the view's centre ray the bed shows in the middle of the screen, within a finger's reach.
const HANDHELD_AHEAD_M: f32 = 0.6;
/// A head settled enough to set the bed down in front of: held still this long (s of pose time),
/// moving slower than these from frame to frame (m/s, rad/s). A headset being put on moves faster.
const SETTLE_S: f64 = 0.3;
const STILL_M_S: f32 = 0.3;
const STILL_RAD_S: f32 = 0.6;
const CONFIG: XrNavConfig = XrNavConfig {
    mode: XrNavMode::Free,
    box_start: ClipBox {
        center: Vec3f { x: TABLE[0], y: TABLE[1], z: TABLE[2] },
        ..XrNavConfig::FOLLOW.box_start
    },
    scale_start: 1.0,
    anchor: Vec3f { x: 0.0, y: 0.0, z: 0.0 },
    ..XrNavConfig::FOLLOW
};
/// Part model names (one per plate part).
const PART_PREFIX: &str = "__fab_xr_part_";
/// One link's model of a part of links (by the part's index).
const LINK_PREFIX: &str = "__fab_xr_link_";
/// The ball a cloth drapes over (a unit sphere, placed by the sim's scene).
const BALL_MODEL: &str = "__fab_xr_ball";
/// Issue rows the menu shows at once.
const ISSUE_ROWS: usize = 4;

/// The wrist menu's cells.
mod item {
    pub const REAL_SIZE: u32 = 2;
    pub const PLAY: u32 = 3;
    pub const UNDO: u32 = 4;
    pub const REDO: u32 = 5;
    pub const PREV_ISSUE: u32 = 6;
    pub const NEXT_ISSUE: u32 = 7;
    pub const MECH_PLAY: u32 = 8;
    pub const MECH_RESET: u32 = 9;
    pub const MECH_SHAKE: u32 = 10;
    /// The active tool, in [`super::MECH_TOOLS`] order.
    pub const MECH_TOOL: u32 = 11;
    /// The sim's scenes, in [`super::MECH_SCENES`]' order.
    pub const MECH_SCENE: u32 = 20;
    /// Its speeds, in [`super::MECH_SPEEDS`]' order.
    pub const MECH_SPEED: u32 = 30;
    pub const MECH_PREV_PIECE: u32 = 40;
    pub const MECH_NEXT_PIECE: u32 = 41;
    /// Add a part ([`super::ADD_PARTS`]' order).
    pub const ADD_PART: u32 = 50;
    pub const DUPLICATE_LINKED: u32 = 55;
    pub const DUPLICATE_COPY: u32 = 56;
    pub const PRINT_PART: u32 = 57;
    /// The Force study's infill floor, found on request.
    pub const FORCE_FLOOR: u32 = 58;
    pub const PRINTER_PAUSE: u32 = 60;
    pub const PRINTER_STOP: u32 = 61;
    pub const PRINTER_STOP_YES: u32 = 62;
    pub const PRINTER_STOP_NO: u32 = 63;
    pub const FOLLOW_PRINTER: u32 = 64;
    pub const ARM_PLAY: u32 = 65;
    pub const ARM_RESET: u32 = 66;
    pub const ARM_CHECKS: u32 = 67;
    /// The Force study's deformed shape on or off, and every fixture and load off.
    pub const FORCE_DEFORM: u32 = 68;
    pub const FORCE_CLEAR: u32 = 69;
    /// The plate view's render modes (`RenderMode::ALL`' order), Slice among them.
    pub const RENDER_MODE: u32 = 70;
    /// A workspace's row (`Workspace::ALL`' order).
    pub const WORKSPACE: u32 = 80;
    /// The microphone into the chat.
    pub const VOICE: u32 = 79;
    /// The Force study's field (`ForceField::ALL`' order).
    pub const FORCE_FIELD: u32 = 90;
    /// The `n`th issue row (from the first shown).
    pub const ISSUE_ROW: u32 = 100;
}

const MECH_TOOLS: [(MechTool, &str); 5] = [(MechTool::Grab, "Grab"), (MechTool::Throw, "Throw"), (MechTool::Pin, "Pin"), (MechTool::Dangle, "Dangle"), (MechTool::Poke, "Poke")];
/// The support pieces a wrist lists.
const MECH_PIECE_ROWS: usize = 6;
const MECH_SCENES: [(MechScene, &str); 4] = [(MechScene::Drop, "Drop"), (MechScene::Drape, "Drape"), (MechScene::Hang, "Hang"), (MechScene::Supports, "Supports")];
const MECH_SPEEDS: [(f64, &str); 3] = [(1.0, "1x"), (0.25, "1/4"), (0.1, "1/10")];
/// What the Add rows add (`PartAction::Add`, the Parts panel's), and what they call it.
const ADD_PARTS: [(&str, &str); 5] = [("box", "Box"), ("cylinder", "Cylinder"), ("sphere", "Sphere"), ("chain", "Chain"), ("chainmail", "Chainmail")];

/// A part of links as the sim moves it: one link's model, drawn once per link.
pub struct PreparedLinks {
    pub model: PreparedStaticPreview,
    /// Per link: link's model frame -> plate, Fab's scene frame (Z up, metres).
    pub rest: Vec<Mat4f>,
    /// Per link: its colour (the model is white).
    pub tints: Vec<Vec4f>,
}

/// One plate part prepared on the model worker.
pub struct PreparedPart {
    /// A mesh part; field parts have no static model.
    pub model: Option<PreparedStaticPreview>,
    pub links: Option<PreparedLinks>,
    pub object: String,
    /// As evaluated (printer mm).
    pub bounds: fab_plate::Bounds,
}

/// One model-worker publication. An empty plate still carries its bed and clears old parts.
pub struct PreparedPlate {
    pub parts: Vec<PreparedPart>,
    pub bed: fab_plate::machine::Rect,
    pub ev: Arc<Evaluated>,
    /// The unit ball of a draped cloth's scene.
    pub ball: Option<PreparedStaticPreview>,
    /// The worker job that made it (0: not a job of ours to wait for).
    pub job: u64,
}

impl std::fmt::Debug for PreparedPlate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedPlate").field("parts", &self.parts.len()).field("job", &self.job).finish()
    }
}

/// What the headset asks the app for (the document edits go through the model worker).
#[derive(Clone, Debug)]
pub enum FabXrAction {
    /// Commit a finished grab as one edit; answer with [`FabXr::sent`].
    Place(Vec<fab_ai::PartMove>),
    Undo,
    Redo,
}

/// The physical bed in the same metre/Y-up frame as `review::plate_glb` and the lit beads.
#[derive(Clone, Copy)]
struct Bed {
    center: Vec3f,
    size: Vec3f,
}

impl Bed {
    fn of(bed: fab_plate::machine::Rect) -> Self {
        Self {
            center: vec3f((bed.min[0] + bed.max[0]) as f32 * 0.0005, 0.0, -(bed.min[1] + bed.max[1]) as f32 * 0.0005),
            size: vec3f(bed.width() as f32 * 0.001, crate::plate_scene::BED_THICKNESS * 0.001, bed.depth() as f32 * 0.001),
        }
    }
}

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.math.*
    use mod.shader.*

    mod.widgets.FabXrBase = #(FabXr::register_widget(vm))
    mod.widgets.FabXr = set_type_default() do mod.widgets.FabXrBase{
        width: 1 height: 1
        draw_cube +: {light_dir: vec3(0.35, 0.8, 0.45)}
        draw_alpha +: {light_dir: vec3(0.35, 0.8, 0.45)}
        draw_terrain +: {light_dir: vec3(0.35, 0.8, 0.45)}
        draw_models +: {light_dir: vec3(0.35, 0.8, 0.45)}
        // the bed is half see-through: the room shows through it (blended premultiplied, as the
        // pass and the stream carry colour)
        draw_bed +: {
            depth_write: true alpha_blend: true color: #x34343880 light_dir: vec3(0.35, 0.8, 0.45)
            pixel: fn() {
                let c = self.lit_color
                return vec4(c.xyz * c.w, c.w)
            }
        }
        draw_mark +: {depth_write: true color: #x30c060 light_dir: vec3(0.35, 0.8, 0.45)}
        shapes: mod.widgets.XrShapes{hidden_alpha: #(fab::handle_style::OCCLUDED_ALPHA)}
        draw_path: mod.draw.DrawFabStrip{}
        draw_far: mod.draw.DrawFabFar{}
        draw_overlay: mod.draw.DrawFabPath{}
        draw_head: mod.draw.DrawFabHead{}
        // about a phone's size on the wrist: readable at arm's length
        menu: mod.widgets.XrWristMenu{panel +: {height: 688.0 meters: 0.1}}
        readout: mod.widgets.XrPanel{height: 128.0 meters: 0.12}
        // a held shape handle's value: one line, twice the font high (`CHIP_HEIGHT_EMS`)
        chip: mod.widgets.XrPanel{width: 512.0 height: 44.0 pad: 4.0 fit_width: true}
        status: mod.widgets.XrPanel{height: 76.0 meters: 0.09}
        tips: mod.widgets.XrTips{}
        draw_tick +: {color: #0000}
        sdf_tile_lit: mod.draw.SdfTileLit{ alpha_blend: false }
    }
}

/// The 1x1 tick that draws the headset's view, in the window's own draw list: redrawn only
/// while a headset displays it (its start redraws everything). On the desktop alone a redraw
/// would re-record the whole window for an invisible tick.
#[derive(Default)]
struct XrTick(Area);

impl XrTick {
    fn redraw(&self, cx: &mut Cx) {
        if cx.xr_display() {
            self.0.redraw(cx);
        }
    }

    fn area(&self) -> Area {
        self.0
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabXr {
    #[uid]
    uid: WidgetUid,
    /// The microphone row of the wrist menu.
    #[rust]
    voice: makepad_app_aichat::VoiceUi,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[live]
    draw_cube: DrawSceneCube,
    #[live]
    draw_alpha: DrawSceneAlpha,
    #[live]
    draw_sky: DrawSceneSky,
    #[live]
    draw_terrain: DrawSceneTerrain,
    #[live]
    draw_shadow: DrawSceneShadow,
    #[live]
    draw_models: DrawSceneSkinned,
    #[live]
    draw_bed: DrawCube,
    /// On a handheld, in place of the bed: a light contact shadow under each part, on the real
    /// surface the bed was placed on (`draw_contact_shadows`).
    #[live]
    draw_contact: DrawSceneShadow,
    #[rust]
    contact: ContactShadows,
    /// Footprints, the aim beam.
    #[live]
    draw_mark: DrawCube,
    #[live]
    shapes: XrShapes,
    #[live]
    draw_path: DrawFabStrip,
    /// The beads a pixel or less across (`far_tiers`).
    #[live]
    draw_far: DrawFabFar,
    #[live]
    draw_overlay: DrawFabPath,
    #[live]
    draw_head: DrawFabHead,
    /// The wrist menu and the layer readout card.
    #[live]
    menu: XrWristMenu,
    #[live]
    readout: XrPanel,
    /// The value chip by a held shape handle: a constant small size on screen (`chip_card`).
    #[live]
    chip: XrPanel,
    #[rust]
    chip_place: ChipPlace,
    /// Over a part being rebuilt: "Rebuilding…", as the desktop's view says.
    #[live]
    status: XrPanel,
    /// A controller stylus or fingertip dot, lit while it touches what it would take.
    #[live]
    tips: XrTips,
    #[rust]
    interaction_filter: InteractionFilter,
    /// This frame's tips (room poses), and what each touches: a part, a sim body, or a handle.
    #[rust]
    tip_at: [Option<Pose>; 2],
    #[rust]
    touch: [Touch; 2],
    #[live]
    draw_tick: DrawColor,
    #[redraw]
    #[rust]
    tick: XrTick,
    /// The parts' tiles in the XR pass.
    #[live]
    sdf_tile_lit: fab::fab_sdf_tiles::draw::DrawSdfTileLit,
    /// `FAB_XR_PROF=1`: GPU ms of the XR pass and whole frame, medians logged every 2 s.
    #[rust]
    prof: XrProf,
    #[rust(XrCheck::new())]
    check: XrCheck,
    #[rust]
    xr: Option<XrPass>,
    #[rust]
    renderer: Renderer,
    #[rust]
    print_palette: PrintPalette,
    #[rust]
    far_tiers: FarTiers,
    #[rust]
    overlay_palette: PrintPalette,
    /// A rebuilt plate to install at the next XR frame.
    #[rust]
    pending: Option<PreparedPlate>,
    #[rust]
    plate: Option<Plate>,
    #[rust]
    edit: XrEdit,
    #[rust]
    out: EditOut,
    #[rust]
    buttons: Buttons,
    #[rust]
    menu_step: MenuStep,
    /// The first issue the menu's rows show.
    #[rust]
    issue_first: usize,
    /// The selected part's shape handles (grabbed with a trigger or grip).
    #[rust]
    handles: crate::xr_handles::XrHandles,
    #[rust]
    hout: crate::xr_handles::HandlesOut,
    #[rust]
    live: Option<LiveSlice>,
    #[rust]
    live_shown: Option<Arc<LiveResult>>,
    /// The live slice's current layer (when no fresh preview drives it).
    #[rust(usize::MAX)]
    live_layer: usize,
    /// Issue layers of the report shown, by its identity.
    #[rust]
    issues: IssueLayers,
    /// Last model submission: stationary frames do not rebuild the placed-model list.
    #[rust]
    placed: Option<PlacedKey>,
    /// The hands on the sim's bodies.
    #[rust]
    mech: XrMech,
    #[rust]
    mout: MechOut,
    /// The body the hands last took and the point they took it by (its rest frame, printer
    /// metres): where the sim has that point is the held point as drawn.
    #[rust]
    mech_track: Option<(ElementId, Vec3f)>,
    /// Which elements the links are in the scene shown: per plate part, per link.
    #[rust]
    link_elements: LinkElements,
    #[rust]
    ball_installed: bool,
    /// The rebuild the status card was logged for.
    #[rust]
    status_shown: Option<u64>,
    /// `FAB_XR_MECH_SYNTH=1`: its pictures to read back (`FAB_XR_GRAB=<dir>`).
    #[rust]
    grabs: XrGrabs,
    /// Bumped when any part's delta or the selection changed.
    #[rust]
    parts_rev: u64,
    /// Counts the grabs.
    #[rust]
    parts_rev_grab: u64,
}

/// What a hand's tip touches (lights it, and lights the part or the sim's body).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Touch {
    part: Option<usize>,
    body: Option<ElementId>,
    handle: bool,
}

impl Touch {
    fn any(&self) -> bool {
        self.part.is_some() || self.body.is_some() || self.handle
    }
}

/// What the placed-model list was last built from: (the room's matrix, the model shows, the parts'
/// revision, the field parts, the sim's snapshot, the shared pose set, the scene, in Mechanics).
type PlacedKey = (Mat4f, bool, u64, u64, usize, Option<(fab::api::PoseSetId, u64)>, usize, bool);

#[derive(Default)]
struct LinkElements {
    /// The scene and the plate they are of.
    of: (usize, usize),
    parts: Vec<Vec<Option<ElementId>>>,
}

/// `FAB_XR_PROF=1` (read once).
pub(crate) fn xr_prof_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("FAB_XR_PROF").as_deref() == Ok("1"))
}

/// `FAB_XR_PROF=1`: an app event that holds the main thread past a headset frame and a half
/// (`fab xr: slow event <kind> <ms> ms`), the headset's frames wait behind it. Held for the
/// event's handling; it logs when dropped.
/// It also logs a stall outside the app (`fab xr: main thread outside the app <ms> ms before
/// <kind>`): from the end of one app event to the start of the next while the headset waits
/// (the platform's own work: presenting, uploads, shader builds).
pub(crate) struct SlowEvent(Option<(&'static str, f64)>);

/// When the last app event ended (f64 bits; 0: none yet).
static LAST_EVENT_END: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl SlowEvent {
    pub(crate) fn start(cx: &Cx, event: &Event) -> SlowEvent {
        if !xr_prof_on() || !cx.xr_display() {
            return SlowEvent(None);
        }
        let kind = match event {
            Event::Draw(_) => "draw",
            Event::XrUpdate(_) => "xr update",
            Event::NextFrame(_) => "next frame",
            Event::Signal => "signal",
            Event::Timer(_) => "timer",
            Event::Actions(_) => "actions",
            _ => "other",
        };
        let now = Cx::time_now();
        let last = f64::from_bits(LAST_EVENT_END.load(std::sync::atomic::Ordering::Relaxed));
        if last > 0.0 && now - last > 0.05 {
            log!("fab xr: main thread outside the app {:.0} ms before {kind}", (now - last) * 1000.0);
        }
        SlowEvent(Some((kind, now)))
    }
}

impl Drop for SlowEvent {
    fn drop(&mut self) {
        if let Some((kind, at)) = self.0 {
            let end = Cx::time_now();
            LAST_EVENT_END.store(end.to_bits(), std::sync::atomic::Ordering::Relaxed);
            let ms = (end - at) * 1000.0;
            if ms > 20.0 {
                log!("fab xr: slow event {kind} {ms:.0} ms");
            }
        }
    }
}

/// `FAB_XR_PROF=1`: GPU ms per frame from the frame timer (`gpu_prof`).
#[derive(Default)]
struct XrProf {
    from: u64,
    at: f64,
    /// when the recording was last asked for
    asked: f64,
    /// (XR pass, whole frame) per frame with an XR pass
    frames: Vec<(f64, f64)>,
    /// The headset's frame intervals (s) and the workspace they are of.
    dts: Vec<f32>,
    workspace: Option<Workspace>,
}

impl XrProf {
    fn step(&mut self, cx: &mut Cx, fields: usize, workspace: Workspace) {
        if std::env::var("FAB_XR_PROF").as_deref() != Ok("1") {
            return;
        }
        if self.workspace != Some(workspace) {
            self.workspace = Some(workspace);
            self.frames.clear();
            self.dts.clear();
            self.at = Cx::time_now();
        }
        // An answer is the whole recording kept (the HAL encodes it, this decodes it): asked for
        // at every headset frame, a recording kept 20 s took most of the UI thread (encode and
        // copy, 85% of its samples on a Mac, 11-09). Asked once a second, a recording kept three
        // seconds still holds every frame since the last answer.
        let now = Cx::time_now();
        if now - self.asked < 1.0 {
            return;
        }
        self.asked = now;
        let Some(rec) = cx.gpu_prof_recording(3) else { return };
        for f in rec.frames.iter().filter(|f| f.id > self.from && !f.split) {
            let (mut xr, mut all) = (0u64, 0u64);
            for p in f.passes.iter().filter(|p| p.end > p.start) {
                let d = p.end - p.start;
                let name = rec.s(p.name);
                all += d;
                if name == "fab xr" {
                    xr += d;
                }
            }
            if xr > 0 {
                self.frames.push((xr as f64 / 1e6, all as f64 / 1e6));
            }
        }
        self.from = rec.frames.last().map_or(self.from, |f| f.id);
        if now - self.at >= 2.0 && self.frames.len() >= 10 {
            let med = |k: usize| {
                let mut v: Vec<f64> = self.frames.iter().map(|f| [f.0, f.1][k]).collect();
                v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                (v[v.len() / 2], v[v.len() * 9 / 10])
            };
            let ((x, x9), (a, a9)) = (med(0), med(1));
            log!("fab xr gpu: {fields} field part(s), {} frames: xr pass {x:.2} (p90 {x9:.2}) ms, frame {a:.2} (p90 {a9:.2}) ms", self.frames.len());
            self.dts.sort_by(f32::total_cmp);
            if let Some(max) = self.dts.last().copied() {
                let at = |q: f32| self.dts[((self.dts.len() - 1) as f32 * q) as usize] * 1000.0;
                log!("fab xr pace: {} {} frames: interval p50 {:.1} p95 {:.1} p99 {:.1} max {:.1} ms, gpu {a:.2} (p90 {a9:.2}) ms", workspace.label(), self.dts.len(), at(0.5), at(0.95), at(0.99), max * 1000.0);
            }
            self.dts.clear();
            self.frames.clear();
            self.at = now;
        }
    }
}

/// What a headset picture covers: per eye, every `XrCheck::STEP`-th pixel's alpha (the room
/// shows where it is 0).
struct Coverage {
    /// Samples per eye across and down.
    w: usize,
    h: usize,
    eyes: [Vec<bool>; 2],
}

type Readback = Arc<std::sync::Mutex<Option<Result<(usize, usize, makepad_widgets::HostBuf), String>>>>;

/// `FAB_XR_CHECK=1`: the parts' tile silhouettes against their meshes. The headset
/// picture holds only the parts (no bed, markers, handles or cards); once the field look has
/// drawn `SETTLE` frames as tiles its coverage is read back, and once the mesh look (View › Shading ›
/// Mesh) has, that one is, and the two compared per eye: `fab xr check: <eye> eye: ...` (with
/// the picture as text: `#` both, `F` the field only, `M` the mesh only). For a still head
/// (`MAKEPAD_XR_REMOTE_SELFTEST_STILL=1`).
#[derive(Default)]
struct XrCheck {
    on: bool,
    /// Frames drawn in the current look (fields or meshes).
    frames: u32,
    fields: bool,
    field: Option<Coverage>,
    pending: Option<(bool, Readback)>,
    done: bool,
    /// The look being read back, for `FAB_XR_GRAB` to keep as a picture.
    picture: Option<&'static str>,
}

impl XrCheck {
    const SETTLE: u32 = 120;
    const STEP: usize = 4;

    fn new() -> XrCheck {
        XrCheck { on: std::env::var("FAB_XR_CHECK").as_deref() == Ok("1"), ..XrCheck::default() }
    }

    /// After the XR pass of a frame (`fields`: its parts drew as fields), the eyes `eye_px`
    /// each, side by side in `color`.
    fn step(&mut self, cx: &mut Cx, color: &Texture, fields: bool, eye_px: Option<DVec2>) {
        if !self.on || self.done {
            return;
        }
        if fields != self.fields {
            self.fields = fields;
            self.frames = 0;
        }
        self.frames += 1;
        if let Some((was_field, rb)) = &self.pending {
            let Some(res) = rb.lock().unwrap().take() else { return };
            let was_field = *was_field;
            self.pending = None;
            let (w, h, px) = match res {
                Ok(r) => r,
                Err(e) => {
                    log!("fab xr check: readback failed: {e}");
                    self.done = true;
                    return;
                }
            };
            let ew = eye_px.map_or(w / 2, |e| e.x as usize).min(w / 2);
            let eh = eye_px.map_or(h, |e| e.y as usize).min(h);
            let (cw, ch) = (ew / Self::STEP, eh / Self::STEP);
            let eyes = [0, 1].map(|e| {
                let mut v = Vec::with_capacity(cw * ch);
                for y in 0..ch {
                    for x in 0..cw {
                        let (px_x, px_y) = (e * ew + x * Self::STEP + Self::STEP / 2, y * Self::STEP + Self::STEP / 2);
                        v.push(xr_covered(&px, w * h, px_y * w + px_x));
                    }
                }
                v
            });
            let cov = Coverage { w: cw, h: ch, eyes };
            if was_field {
                let n = cov.eyes.each_ref().map(|e| e.iter().filter(|c| **c).count());
                log!("fab xr check: field captured ({w}x{h}, eyes {ew}x{eh}): left {} px, right {} px", n[0], n[1]);
                self.field = Some(cov);
            } else if let Some(field) = self.field.take() {
                self.done = true;
                if (field.w, field.h) != (cov.w, cov.h) {
                    log!("fab xr check: the eye size changed between the looks");
                    return;
                }
                for (e, name) in ["left", "right"].iter().enumerate() {
                    let (f, m) = (&field.eyes[e], &cov.eyes[e]);
                    let nf = f.iter().filter(|c| **c).count();
                    let nm = m.iter().filter(|c| **c).count();
                    let diff = f.iter().zip(m).filter(|(a, b)| a != b).count();
                    // the differing samples away from the mesh's edges (its 3x3 alike): the
                    // silhouettes' own disagreement, not their edges' sub-pixel one
                    let (w, h) = (cov.w, cov.h);
                    let inner = (1..h.saturating_sub(1)).flat_map(|y| (1..w - 1).map(move |x| (x, y))).filter(|&(x, y)| {
                        let c = m[y * w + x];
                        f[y * w + x] != c && (0..9).all(|k| m[(y + k / 3 - 1) * w + x + k % 3 - 1] == c)
                    }).count();
                    // rows of the picture where the looks differ (the top, bottom, left, right)
                    let (mut top, mut bottom, mut left, mut right) = (usize::MAX, 0, usize::MAX, 0);
                    for y in 0..cov.h {
                        for x in 0..cov.w {
                            if f[y * cov.w + x] != m[y * cov.w + x] {
                                (top, bottom, left, right) = (top.min(y), bottom.max(y), left.min(x), right.max(x));
                            }
                        }
                    }
                    log!("fab xr check: {name} eye: field {nf} px, mesh {nm} px, differing {diff}, off the edges {inner} ({:.2}% of the mesh), in rows {}..{} columns {}..{} of {}x{}", inner as f64 * 100.0 / nm.max(1) as f64, top.min(bottom) * Self::STEP, bottom * Self::STEP, left.min(right) * Self::STEP, right * Self::STEP, cov.w * Self::STEP, cov.h * Self::STEP);
                    // the picture as text, 8 samples a character
                    for y in (0..cov.h).step_by(8) {
                        let row: String = (0..cov.w).step_by(4).map(|x| match (f[y * cov.w + x], m[y * cov.w + x]) { (true, true) => '#', (true, false) => 'F', (false, true) => 'M', _ => '.' }).collect();
                        if row.bytes().any(|b| b != b'.') {
                            log!("fab xr check: {name} {:4} {row}", y * Self::STEP);
                        }
                    }
                }
            }
            return;
        }
        let want = if fields { self.field.is_none() } else { self.field.is_some() };
        if want && self.frames >= Self::SETTLE {
            let rb: Readback = Arc::default();
            let out = rb.clone();
            match cx.read_render_texture(color, move |r| *out.lock().unwrap() = Some(r.map_err(|e| format!("{e:?}")))) {
                Ok(()) => {
                    self.pending = Some((fields, rb));
                    self.picture = Some(if fields { "check_field" } else { "check_mesh" });
                }
                Err(e) => {
                    log!("fab xr check: readback request failed: {e:?}");
                    self.done = true;
                }
            }
        }
    }
}

/// The light of the tile parts in the headset's passthrough room.
fn xr_sdf_light() -> TileLight {
    TileLight { display: true, ..TileLight::default() }
}

/// Fab's scene frame (printer metres, Z up) to the bed's Y-up metres (`review::plate_glb`).
fn yup_from_fab() -> Mat4f {
    Mat4f { v: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0] }
}

fn fab_from_yup() -> Mat4f {
    Mat4f { v: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0] }
}

/// The plate shows the slice (Design's Slice render mode).
fn slice_shown(state: Option<&fab::AppState>) -> bool {
    state.is_some_and(|s| s.ui.in_mode(PlateMode::Design) && s.print.play.active)
}

/// The workspace the desktop and the headset are in.
fn workspace_of(state: Option<&fab::AppState>) -> Workspace {
    state.map_or(Workspace::default(), |s| s.ui.kind())
}

/// The plate mode in force on the desktop and in the headset (`UiState::mode`).
fn mode_of(state: Option<&fab::AppState>) -> Option<PlateMode> {
    state.map_or(Some(PlateMode::default()), |s| s.ui.mode())
}

/// A sim pose (rest -> now in Fab's scene frame) as a matrix.
fn pose_matrix(a: &[[f32; 4]; 3]) -> Mat4f {
    Mat4f { v: [a[0][0], a[1][0], a[2][0], 0.0, a[0][1], a[1][1], a[2][1], 0.0, a[0][2], a[1][2], a[2][2], 0.0, a[0][3], a[1][3], a[2][3], 1.0] }
}

#[derive(Default)]
struct IssueLayers {
    report: usize,
    layers: Vec<u32>,
}

struct Plate {
    bed: Bed,
    ev: Arc<Evaluated>,
    bounds: Vec<fab_plate::Bounds>,
    objects: Vec<String>,
    models: Vec<String>,
    /// Per part: a part of links as the sim moves it.
    links: Vec<Option<PlateLinks>>,
}

struct PlateLinks {
    model: String,
    rest: Vec<Mat4f>,
    tints: Vec<Vec4f>,
}

struct XrPass {
    pass: DrawPass,
    /// The root holds the model's child list, then the bed and beads in the same depth pass.
    root: DrawList,
    draw_list: DrawList,
    /// Attachments read by the server's frame sink at the pass's end.
    _color: Texture,
    _depth: Texture,
    nav: XrNav,
    centered: bool,
    stepped_at: Option<f64>,
    /// The tracking space the bed was set down in front of the head for (`XrState::space`). A
    /// new one (a headset session, a recenter: its origin may have moved) sets it down again once
    /// the head settles.
    placed: Option<u32>,
    /// Since when (pose time, s) the head has held still in a space the bed is not set down for.
    still_since: Option<f64>,
    /// The headset's frame interval, smoothed (s): a step more than twice it apart is a
    /// frame gap, logged (`fab xr: frame gap`; the flight recorder has its frame).
    frame_dt: f32,
    /// `FAB_XR_NAV_SYNTH=1`: shared synthetic controllers, with the server's selftest head.
    synth: bool,
    synth_clock: SynthClock,
    /// `FAB_XR_EDIT_SYNTH=1`: a scripted edit session (grab, move, drop, slice, layer grab,
    /// menu, undo) on synthetic controllers: [`synth_edit`].
    edit_synth: Option<SynthEdit>,
    /// `FAB_XR_HANDLE_SYNTH=1`: a scripted shape-handle drag on synthetic controllers:
    /// [`HandleSynth`].
    handle_synth: Option<HandleSynth>,
    /// `FAB_XR_MECH_SYNTH=1`: every workspace from the wrist menu's rows, the sim's scenes and a
    /// throw, on synthetic controllers: [`MechSynth`].
    mech_synth: Option<MechSynth>,
    /// The frame's input as the navigation gets it (the hands editing masked out).
    nav_state: Box<XrState>,
    /// Real size asked for: the scale it zooms from and when.
    real_size: Option<(f32, f64)>,
    head: Vec3f,
    logged_at: f64,
    logged_model: bool,
    logged_preview: Option<u64>,
    logged_heads: Option<u64>,
    logged_live: Option<u64>,
    /// The view last logged (scale, origin, yaw, the bed's centre in the room) and when.
    logged_view: Option<([f32; 8], f64)>,
}

/// Printer mm affine to a world (glTF frame) matrix.
/// The XR world from Fab's scene frame for plate part `object`: the bed's placement in the
/// room, the hands' move of the part (as its mesh), Y up.
fn plate_world(plate: &Plate, edit: &XrEdit, transform: Mat4f, object: &str) -> Option<Mat4f> {
    let i = plate.objects.iter().position(|o| o == object)?;
    Some(Mat4f::mul(&part_matrix(edit, transform, i), &yup_from_fab()))
}

/// Where plate part `i` is drawn: its world (`world_of` of printer mm) into the room, the bed's
/// placement and the hands' move of the part. The part's model, its field and its shape handles
/// all draw with this one matrix.
fn part_matrix(edit: &XrEdit, transform: Mat4f, i: usize) -> Mat4f {
    Mat4f::mul(&transform, &edit.deltas.get(i).map_or(Mat4f::identity(), world_matrix))
}

fn world_matrix(d: &Affine) -> Mat4f {
    // w = A p / 1000 with A: (x, y, z) -> (x, z, -y)
    let col = |p: [f64; 3]| [p[0] as f32, p[2] as f32, -p[1] as f32];
    let ex = col([d[0][0], d[1][0], d[2][0]]);
    let ey = col([d[0][1], d[1][1], d[2][1]]);
    let ez = col([d[0][2], d[1][2], d[2][2]]);
    let t = col([d[0][3] * 0.001, d[1][3] * 0.001, d[2][3] * 0.001]);
    // world axes: wx = A ex_p, wy = A ez_p, wz = -A ey_p
    let wy = ez;
    let wz = [-ey[0], -ey[1], -ey[2]];
    Mat4f { v: [ex[0], ex[1], ex[2], 0.0, wy[0], wy[1], wy[2], 0.0, wz[0], wz[1], wz[2], 0.0, t[0], t[1], t[2], 1.0] }
}

impl FabXr {
    /// The microphone row of the wrist menu: what the chat's button is now.
    pub fn set_voice(&mut self, voice: makepad_app_aichat::VoiceUi) {
        self.voice = voice;
    }

    /// Whether this process serves a headset (the XR pass is set up).
    pub fn active(&self) -> bool {
        self.xr.is_some()
    }

    /// The plate to show (prepared on the model worker): the headset's next frame draws it.
    pub fn set_plate(&mut self, plate: PreparedPlate) {
        self.pending = Some(plate);
    }

    /// The app sent the oldest finished grab to the model worker as job `job`.
    pub fn sent(&mut self, job: u64) {
        self.edit.edits.sent(job);
    }

    /// A refused grab restores its parts to the document's positions.
    pub fn failed(&mut self, job: u64) {
        let deltas = &mut self.edit.deltas;
        self.edit.edits.failed(job, |i| deltas[i] = xr_edit::IDENTITY);
    }

    /// The oldest finished grab could not be sent: its parts go back to the document's positions.
    pub fn failed_unsent(&mut self) {
        let deltas = &mut self.edit.deltas;
        self.edit.edits.failed_unsent(|i| deltas[i] = xr_edit::IDENTITY);
    }

    /// What the sim hit since the last frame, felt in the hand holding a body.
    pub fn sim_events(&mut self, cx: &mut Cx, events: &mut Vec<makepad_csg_mech::runner::Event>) {
        use makepad_csg_mech::runner::Event as Sim;
        let hand = self.mech.held_hand();
        for e in events.drain(..) {
            let Some(hand) = hand else { continue };
            match e {
                Sim::Hit { speed, .. } if speed > 100.0 => haptic::SNAP.play(cx, hand),
                Sim::Limit { .. } => haptic::LIMIT.play(cx, hand),
                _ => {}
            }
        }
    }

    fn activate(&mut self, cx: &mut Cx) {
        // The eyes hold linear light (half floats): the MSAA resolve averages light, not display
        // values (an edge pixel half covered by a bead shows the light its two halves give), and
        // the stream encodes once after it.
        let color = Texture::new_with_format(cx, TextureFormat::RenderRGBAf16 { size: TextureSize::Auto, initial: true });
        let depth = Texture::new_with_format(cx, TextureFormat::DepthD32Sampled { size: TextureSize::Auto, initial: true });
        let pass = DrawPass::new_with_name(cx, "fab xr");
        // Premultiplied on nothing: the room shows wherever the plate is not.
        pass.set_color_texture(cx, &color, DrawPassClearColor::ClearWith(vec4(0.0, 0.0, 0.0, 0.0)));
        pass.set_depth_texture(cx, &depth, DrawPassClearDepth::ClearWith(1.0));
        pass.set_as_xr_pass(cx);
        pass.set_linear_light(cx, true);
        cx.xr_start_presenting();
        cx.xr_set_see_through(true);
        // The model already carries the navigation matrix. The identity MR stage uses the
        // headset's eye frusta for culling instead of a desktop preview camera's frustum.
        self.renderer.set_stage(makepad_render::stage::Stage::mr_diorama(vec3f(0.0, 0.0, 0.0), 0.0, 1.0));
        self.renderer.set_gpu_lightmap_mode(makepad_render::gpu_lightmap::GpuLightmapMode::Realtime);
        self.renderer.set_clustered_lighting(true);
        self.live = Some(LiveSlice::start());
        log!("fab xr: serving the plate to a headset at real size, mixed reality");
        self.xr = Some(XrPass {
            pass, root: DrawList::new(cx), draw_list: DrawList::new(cx), _color: color, _depth: depth,
            nav: XrNav::new(CONFIG), centered: false, stepped_at: None, placed: None, still_since: None, frame_dt: 0.0,
            synth: std::env::var("FAB_XR_NAV_SYNTH").as_deref() == Ok("1"), synth_clock: SynthClock::default(),
            edit_synth: (std::env::var("FAB_XR_EDIT_SYNTH").as_deref() == Ok("1")).then(SynthEdit::default),
            handle_synth: (std::env::var("FAB_XR_HANDLE_SYNTH").as_deref() == Ok("1"))
                .then(|| HandleSynth { part: std::env::var("FAB_XR_HANDLE_PART").ok().and_then(|p| p.parse().ok()).unwrap_or(0), ..Default::default() }),
            mech_synth: match std::env::var("FAB_XR_MECH_SYNTH").as_deref() {
                Ok("1") => Some(MechSynth::default()),
                Ok("pace") => Some(MechSynth { pace_only: true, ..Default::default() }),
                Ok("supports") => Some(MechSynth { supports: true, ..Default::default() }),
                _ => None,
            },
            nav_state: Box::default(), real_size: None, head: Vec3f::default(),
            logged_at: 0.0, logged_model: false, logged_preview: None, logged_heads: None, logged_live: None, logged_view: None,
        });
    }

    /// A rebuilt plate: install its part models, keep the deltas of parts still moving.
    fn install(&mut self, cx: &mut Cx, plate: PreparedPlate, state: Option<&fab::AppState>) {
        self.renderer.enter_realm();
        let same = self.plate.as_ref().is_some_and(|p| p.objects.len() == plate.parts.len() && p.objects.iter().zip(&plate.parts).all(|(o, n)| *o == n.object));
        let old = self.plate.as_ref().map_or(0, |p| p.models.len());
        let old_links: Vec<String> = self.plate.iter().flat_map(|p| p.links.iter().flatten().map(|l| l.model.clone())).collect();
        if let Some(ball) = plate.ball.filter(|_| !self.ball_installed) {
            self.renderer.install_static_preview(cx, BALL_MODEL, ball, false);
            self.ball_installed = true;
        }
        let mut bounds = Vec::with_capacity(plate.parts.len());
        let mut objects = Vec::with_capacity(plate.parts.len());
        let mut models = Vec::with_capacity(plate.parts.len());
        let mut links = Vec::with_capacity(plate.parts.len());
        for (i, part) in plate.parts.into_iter().enumerate() {
            let id = format!("{PART_PREFIX}{i}");
            if let Some(model) = part.model {
                self.renderer.install_static_preview(cx, &id, model, false);
            }
            links.push(part.links.map(|l| {
                let model = format!("{LINK_PREFIX}{i}");
                self.renderer.install_static_preview(cx, &model, l.model, false);
                PlateLinks { model, rest: l.rest, tints: l.tints }
            }));
            bounds.push(part.bounds);
            objects.push(part.object);
            models.push(id);
        }
        for i in models.len()..old {
            self.renderer.unload_model(&format!("{PART_PREFIX}{i}"));
        }
        for model in old_links.iter().filter(|m| !links.iter().flatten().any(|l: &PlateLinks| l.model == **m)) {
            self.renderer.unload_model(model);
        }
        self.edit.plate(models.len(), plate.job, same);
        let bed = Bed::of(plate.bed);
        if let Some(xr) = self.xr.as_mut() {
            if !xr.centered {
                // Both GLB and beads keep bed origin: (x, y, z) mm -> (x, z, -y) m.
                // Centre the bed once, not each rebuilt model, so edits never jump in the room.
                xr.nav.center_on(bed.center);
                xr.centered = true;
            }
        }
        if let Some(live) = &self.live {
            let h = state.map_or(0.2, |s| s.print.settings.layer_height);
            live.set_plate(plate.ev.clone(), h, Default::default());
        }
        self.plate = Some(Plate { bed, ev: plate.ev, bounds, objects, models, links });
        self.placed = None;
        self.parts_rev = self.parts_rev.wrapping_add(1);
    }

    /// The slice axis the hands scrub: the fresh preview's layers, else the live slice's:
    /// (layers, current layer, along it). Refreshes the issue layers of the report shown.
    fn slice_axis(&mut self, state: Option<&fab::AppState>) -> Option<(usize, usize, f32)> {
        if let Some(r) = state.map(|s| s.print.report()) {
            let key = Arc::as_ptr(r) as usize;
            if self.issues.report != key {
                self.issues.report = key;
                self.issues.layers.clear();
                self.issues.layers.extend(r.issues.iter().filter_map(|i| i.layer));
                self.issues.layers.sort_unstable();
                self.issues.layers.dedup();
            }
        }
        if let Some(pv) = state.and_then(fresh_preview) {
            let t = state.unwrap().print.play.time;
            let li = pv.layer_at(t);
            let l = pv.layers.get(li)?;
            let along = if l.t_end > l.t_start { ((t - l.t_start) / (l.t_end - l.t_start)).clamp(0.0, 1.0) as f32 } else { 1.0 };
            return Some((pv.layers.len(), li, along));
        }
        let live = self.live_shown.as_ref()?;
        let layers = live.contours.top_layer as usize + 1;
        self.live_layer = self.live_layer.min(layers - 1);
        Some((layers, self.live_layer, 1.0))
    }

    fn command(&mut self, cx: &mut Cx, command: Command, state: Option<&fab::AppState>) {
        match command {
            Command::SetWorkspace(w) => cx.action(fab::api::ShellAction::SetWorkspace(w)),
            Command::SliceView(on) => {
                use fab::api::{PartLook, RenderMode};
                let back = match state.map_or(PartLook::Field, |s| s.view.look) {
                    PartLook::Field => RenderMode::Field,
                    PartLook::Mesh => RenderMode::Mesh,
                };
                // the render mode slices what is not sliced yet (the app follows it)
                cx.action(fab::api::ShellAction::SetRenderMode(if on { RenderMode::Slice } else { back }));
            }
            Command::RealSize => {
                if let Some(xr) = self.xr.as_mut() {
                    // a quick zoom about where the plate stands
                    xr.real_size = Some((xr.nav.view().scale, Cx::time_now()));
                }
            }
            Command::TogglePlay => {
                // Play never changes the render mode: there is nothing to play without the slice.
                if slice_shown(state) {
                    let playing = state.is_some_and(|s| s.print.play.playing);
                    cx.action(PrintAction::Play(!playing));
                }
            }
            Command::Undo => cx.action(FabXrAction::Undo),
            Command::Redo => cx.action(FabXrAction::Redo),
            Command::Issue(i) => {
                self.issue_first = i.saturating_sub(1);
                cx.action(PrintAction::SelectIssue(i));
            }
            Command::PrevIssue | Command::NextIssue => {
                let count = state.map_or(0, |s| s.print.report().issues.len());
                self.issue_first = if command == Command::NextIssue {
                    (self.issue_first + 1).min(count.saturating_sub(1))
                } else {
                    self.issue_first.saturating_sub(1)
                };
                if count > 0 {
                    cx.action(PrintAction::SelectIssue(self.issue_first));
                }
            }
            Command::StepLayer(d) => self.scrub_step(cx, d, state),
        }
    }

    fn scrub_step(&mut self, cx: &mut Cx, d: i32, state: Option<&fab::AppState>) {
        if let Some((layers, current, _)) = self.slice_axis(state) {
            let layer = (current as i64 + d as i64).clamp(0, layers as i64 - 1) as usize;
            if layer != current {
                let issue = self.issues.layers.binary_search(&(layer as u32)).is_ok();
                (if issue { haptic::LIMIT } else { haptic::SNAP }).play(cx, 0);
                self.scrub(cx, layer, 1.0, state);
            }
        }
    }

    fn scrub(&mut self, cx: &mut Cx, layer: usize, along: f32, state: Option<&fab::AppState>) {
        if let Some(pv) = state.and_then(fresh_preview) {
            if let Some(l) = pv.layers.get(layer) {
                cx.action(PrintAction::Seek(l.t_start + (l.t_end - l.t_start) * along as f64));
            }
        } else {
            self.live_layer = layer;
            // the layers at the hands slice first
            if let Some(l) = &self.live_shown {
                crate::live_slice::set_focus_z(Some((layer as f64 + 0.5) * l.layer_height as f64));
            }
        }
    }

    /// One headset frame's input: the edit step, then the navigation with what the edit leaves.
    fn step(&mut self, cx: &mut Cx, input: &XrState, dt: f32, state: Option<&fab::AppState>) {
        let Some(xr) = self.xr.as_mut() else { return };
        let view = xr.nav.view();
        xr.head = input.head_pose.position;
        // the sim's answer to this frame's drag shows next frame (its spring, pulled by a target
        // that moves at the hand's speed, trails it by less than a frame's lead: measured, see
        // the carry of `FAB_XR_MECH_SYNTH`)
        let lead = xr.frame_dt;
        let now = Cx::time_now();
        let raw_input = input;
        let filtered = self.interaction_filter.apply(raw_input, raw_input.time);
        let input = &filtered;
        self.tips.set_input(input);
        let axis = self.slice_axis(state).filter(|_| slice_shown(state));
        let was_grabbing = self.edit.grabbing();
        let mode = mode_of(state);
        let mut out = std::mem::take(&mut self.out);
        self.buttons.step(input);
        self.fill_menu(state);
        self.menu_step = self.menu.prepare(input, &self.buttons, now);
        for (i, held) in self.edit.held_hands().into_iter().enumerate() {
            if held { self.buttons.claim(i); }
        }
        if let Some(i) = self.handles.held_hand() { self.buttons.claim(i); }
        if let Some(i) = self.mech.held_hand() { self.buttons.claim(i); }
        if let Some(i) = self.mech.carrying_hand() { self.buttons.claim(i); }
        if self.menu_step.slider.is_some() { self.buttons.claim(1); }
        // the hands on the sim's bodies: a hand on one is the sim's alone
        let bodies = state.filter(|s| mode == Some(PlateMode::Mechanics) && s.mech.ready).map(|state| SceneBodies { state });
        let mut mout = std::mem::take(&mut self.mout);
        self.mech.step(bodies.as_ref().map(|b| b as &dyn crate::xr_mech::BodyPick), state.map_or(MechTool::default(), |s| s.mech.tool), input, &view, now, lead, &self.buttons, &self.menu_step, &mut mout);
        if let Some((element, point)) = mout.grab {
            cx.action(if mout.dangle { MechAction::Dangle { element, point } } else { MechAction::Grab { element, point, hand: true } });
            self.mech_track = state.and_then(|s| Some((element, crate::xr_mech::rest_point(s, element, point)?)));
        }
        if let Some((element, point)) = mout.pin {
            cx.action(MechAction::Pin { element, point });
        }
        if let Some((element, point, dv)) = mout.poke {
            cx.action(MechAction::Poke { element, point, dv });
        }
        if let Some(point) = mout.drag {
            cx.action(MechAction::Drag { point });
        }
        if mout.release {
            cx.action(MechAction::Release { velocity: mout.throw });
        }
        if mout.carry_taken {
            cx.action(MechAction::Take);
        }
        if let Some(pose) = mout.carry {
            cx.action(MechAction::Carry { pose });
        }
        for i in 0..2 {
            if mout.consumed[i] { self.buttons.claim(i); }
        }
        // the shape handles first: a hand on a handle is not a part grab
        let mut hout = std::mem::take(&mut self.hout);
        {
            let part = (mode == Some(PlateMode::Design) && !slice_shown(state) && !self.edit.grabbing()).then(|| self.edit.selected.iter().position(|s| *s)).flatten();
            // at the values they show (a held handle stays with the hand before the rebuild)
            let pp = part.zip(state).and_then(|(i, s)| s.params.part(i).map(|pp| s.params.shown_part(pp)));
            let shown: Vec<&fab::params::ParamView> = match (&pp, state) {
                (Some(pp), Some(s)) => fab::tools::handles::shown_params(s, pp),
                _ => Vec::new(),
            };
            let head = input.head_pose.position;
            let delta = part.and_then(|i| self.edit.deltas.get(i).copied()).unwrap_or(xr_edit::IDENTITY);
            self.handles.step(input, &view, pp.as_ref(), &shown, head, &delta, &self.buttons, &mut hout);
            if let Some((object, key, value)) = hout.preview.take() {
                cx.action(fab::params::ParamAction::Preview { object, key, value });
            }
            if let Some((object, key, value)) = hout.commit.take() {
                cx.action(fab::params::ParamAction::Commit { object, key, value });
            }
            if let Some(object) = hout.cancel.take() {
                cx.action(fab::params::ParamAction::Cancel { object });
            }
        }
        let consumed = [hout.consumed[0] || mout.consumed[0], hout.consumed[1] || mout.consumed[1]];
        for i in 0..2 {
            if consumed[i] { self.buttons.claim(i); }
        }
        {
            let bounds: &[fab_plate::Bounds] = self.plate.as_ref().map_or(&[], |p| &p.bounds);
            let axis = axis.map(|(layers, layer, along)| SliceAxis { layers, layer, along, issue_layers: &self.issues.layers });
            self.edit.step(mode, input, &view, bounds, axis, now, &mut self.buttons, &self.menu_step, &mut out);
        }
        for i in 0..2 {
            if consumed[i] {
                out.nav_grip[i] = false;
            }
        }
        self.menu.interact(input, &self.buttons, &mut self.menu_step);
        if let Some(id) = self.menu_step.pressed {
            self.menu_action(cx, id, state);
            if let Some(c) = self.menu_command(id, state) {
                out.command = Some(c);
            }
        }
        out.haptics.merge(self.menu_step.haptics);
        out.haptics.merge(hout.haptics);
        out.haptics.merge(mout.haptics);
        // the tips and what they touch (a change relights the touched part or body)
        self.tip_at = input::tips(input);
        let touch = [0, 1].map(|i| Touch { part: out.touch[i], body: mout.touch[i], handle: hout.touch[i] });
        if touch.map(|t| (t.part, t.body)) != self.touch.map(|t| (t.part, t.body)) {
            self.parts_rev = self.parts_rev.wrapping_add(1);
        }
        if touch != self.touch && xr_prof_on() {
            log!("fab xr: tips touch {touch:?}");
        }
        self.touch = touch;
        // the hand the menu stands on is not pointing: no stylus on it
        if self.menu_step.open {
            self.tip_at[0] = None;
        }
        self.hout = hout;
        self.mout = mout;
        if self.edit.grabbing() && !was_grabbing {
            self.parts_rev_grab = self.parts_rev_grab.wrapping_add(1);
            // the live slice takes over from the preview at the layer it was at
            if let Some((_, layer, _)) = axis {
                self.live_layer = layer;
            }
        }
        // the navigation sees the grips and sticks the edit left it
        let xr = self.xr.as_mut().unwrap();
        let nav_in: &XrState = if out.nav_grip == [true, true] && out.nav_stick == [true, true] {
            raw_input
        } else {
            *xr.nav_state = raw_input.clone();
            for (i, c) in [&mut xr.nav_state.left_controller, &mut xr.nav_state.right_controller].into_iter().enumerate() {
                if !out.nav_grip[i] {
                    c.grip = 0.0;
                }
                if !out.nav_stick[i] {
                    c.stick = Vec2f::default();
                }
            }
            for (i, h) in [&mut xr.nav_state.left_hand, &mut xr.nav_state.right_hand].into_iter().enumerate() {
                if !out.nav_grip[i] {
                    h.flags &= !XrHand::PINCH_MIDDLE;
                }
            }
            &xr.nav_state
        };
        xr.nav.update(nav_in, dt);
        if let Some((from, at)) = xr.real_size {
            // 0.25 s, even in log scale, ending at 1:1; a plate out of view then comes in front
            let k = (((now - at) / 0.25) as f32).clamp(0.0, 1.0);
            let k = k * k * (3.0 - 2.0 * k);
            xr.nav.set_scale((from.max(1e-4).ln() * (1.0 - k)).exp());
            if k >= 1.0 {
                xr.real_size = None;
                let bed = self.plate.as_ref().map_or(vec3f(0.0, 0.0, 0.0), |p| p.bed.center);
                let head = input.head_pose.position;
                let fwd = input.head_pose.orientation.rotate_vec3(&vec3f(0.0, 0.0, -1.0));
                let flat = vec3f(fwd.x, 0.0, fwd.z);
                let flat = if flat.length() > 1e-4 { flat.normalize() } else { vec3f(0.0, 0.0, -1.0) };
                let to = xr.nav.view().room_from_world(bed) - head;
                if to.length() > 2.5 || to.dot(flat) < 0.2 {
                    xr.nav.clip.center = vec3f(head.x + flat.x * -TABLE[2], TABLE[1], head.z + flat.z * -TABLE[2]);
                    xr.nav.center_on(bed);
                }
                log!("fab xr: real size, scale {:.3}", xr.nav.view().scale);
            }
        }
        out.haptics.play(cx);
        if out.moved || out.selection_changed {
            self.parts_rev = self.parts_rev.wrapping_add(1);
        }
        if out.moved {
            if let Some(live) = &self.live {
                for (i, d) in self.edit.deltas.iter().enumerate() {
                    if *d != xr_edit::IDENTITY && !self.edit.edits.waiting(i) && self.plate.as_ref().is_some_and(|p| i < p.ev.plate.parts.len()) {
                        live.set_transform(i, *d);
                    }
                }
            }
        }
        if let Some((layer, along)) = out.scrub {
            self.scrub(cx, layer, along, state);
        }
        if let Some(c) = out.command {
            self.command(cx, c, state);
        }
        if let Some(commit) = out.commit.take() {
            if let Some(live) = &self.live {
                live.release();
            }
            if let Some(plate) = &self.plate {
                let moves: Vec<fab_ai::PartMove> = commit.iter().map(|(i, d)| fab_ai::PartMove { object: plate.objects[*i].clone(), delta: *d }).collect();
                if !moves.is_empty() {
                    log!("fab xr: grab committed, {} part(s) moved", moves.len());
                    let items = commit.iter().map(|(i, _)| *i).collect();
                    self.edit.edits.commit(items, moves, &mut ModelWorker(cx));
                }
            }
        }
        self.out = out;
    }

    /// The wrist menu's rows, rewritten when what they show changed: one row per workspace, the
    /// plate's own actions, then what the workspace's panel has.
    fn fill_menu(&mut self, state: Option<&fab::AppState>) {
        let target = self.duplicate_target(state);
        let duplicate = target.is_some();
        let printed = target.and_then(|o| state?.plate.parts.iter().find(|p| p.object == o)).map(|p| p.printed || p.insert);
        let playing = state.is_some_and(|s| s.print.play.playing);
        // the issues: the fresh slice's or the plate's, else the live slice's of the moved plate
        let report = self.report(state);
        let workspace = workspace_of(state);
        let mode = mode_of(state);
        let first = self.issue_first;
        let mech = state.map(|s| &s.mech);
        let pieces = self.plate.as_ref().map_or(0, |p| p.ev.weaves.len());
        let scale = self.xr.as_ref().map_or(1.0, |x| x.nav.view().scale);
        let mut key = 0xcbf2_9ce4_8422_2325u64;
        key = (key ^ duplicate as u64).wrapping_mul(0x100_0000_01b3);
        key = (key ^ printed.unwrap_or(false) as u64).wrapping_mul(0x100_0000_01b3);
        key = (key ^ self.voice as u64).wrapping_mul(0x100_0000_01b3);
        for v in [(scale * 10.0).round() as u64 + ((scale - 1.0).abs() < 0.005) as u64 * 1000, workspace.index() as u64, mode.map_or(0, |m| m as u64 + 1), slice_shown(state) as u64, state.map_or(0, |s| s.render_mode() as u64), state.is_some_and(|s| s.ui.arm_shown) as u64, playing as u64, first as u64, report.as_ref().map_or(0, |r| Arc::as_ptr(r) as u64), pieces as u64] {
            key = (key ^ v).wrapping_mul(0x100_0000_01b3);
        }
        if let Some(p) = state.filter(|_| mode == Some(PlateMode::Monitor)).map(|s| &s.print) {
            // a new snapshot of the printer is a new Arc
            for v in [p.printer.as_ref().map_or(0, |s| Arc::as_ptr(s) as u64), p.confirm_stop as u64, p.follow.on as u64] {
                key = (key ^ v).wrapping_mul(0x100_0000_01b3);
            }
        }
        if let Some(a) = state.filter(|_| workspace.shows(Family::Arm)).map(|s| &s.arm) {
            key = (key ^ a.running as u64).wrapping_mul(0x100_0000_01b3);
            for b in a.status.bytes().chain(a.stats.iter().take(4).flat_map(|(_, v)| v.bytes())) {
                key = (key ^ b as u64).wrapping_mul(0x100_0000_01b3);
            }
        }
        if let Some(f) = state.filter(|s| s.force_shown()).map(|s| &s.force) {
            for v in [f.field as u64, f.deform.to_bits(), f.loads.len() as u64, f.solving as u64] {
                key = (key ^ v).wrapping_mul(0x100_0000_01b3);
            }
            for b in f.status.bytes().chain(f.part.bytes()).chain(f.note.bytes()).chain(f.stats.iter().take(4).flat_map(|(_, v)| v.bytes())) {
                key = (key ^ b as u64).wrapping_mul(0x100_0000_01b3);
            }
        }
        if let Some(m) = mech.filter(|_| mode == Some(PlateMode::Mechanics)) {
            for v in [m.ready as u64, m.running as u64, m.shaking as u64, m.scene as u64, m.speed.to_bits(), m.piece as u64, m.tool as u64] {
                key = (key ^ v).wrapping_mul(0x100_0000_01b3);
            }
            for b in m.status.bytes().chain(m.verdict.bytes()) {
                key = (key ^ b as u64).wrapping_mul(0x100_0000_01b3);
            }
        }
        let plate = self.plate.as_ref();
        self.menu.panel.set(key, |rows| {
            // the workspaces offered (an experimental one while View › Experimental has it)
            for (k, w) in Workspace::ALL.iter().enumerate().filter(|(_, w)| state.is_none_or(|s| s.workspace_shown(**w))) {
                rows.row(Height::Fixed(44.0)).toggle(item::WORKSPACE + k as u32, w.label(), *w == workspace, Width::FILL);
            }
            // the wearer speaks to the chat from here (the chat itself is on the desktop)
            match self.voice {
                makepad_app_aichat::VoiceUi::Hidden => {}
                makepad_app_aichat::VoiceUi::Idle => {
                    rows.row(Height::Fixed(52.0)).button(item::VOICE, "Speak to the chat", Width::FILL);
                }
                makepad_app_aichat::VoiceUi::Listening => {
                    rows.row(Height::Fixed(52.0)).toggle(item::VOICE, "Listening: tap to stop", true, Width::FILL);
                }
                makepad_app_aichat::VoiceUi::Busy => {
                    rows.row(Height::Fixed(52.0)).button(item::VOICE, "Writing it down...", Width::FILL);
                }
            }
            let real = if (scale - 1.0).abs() < 0.005 { "Real size 1:1".to_string() } else { format!("Real size ({scale:.1}x)") };
            rows.row(Height::Fixed(52.0)).toggle(item::REAL_SIZE, real, (scale - 1.0).abs() < 0.005, Width::Flex(2.0)).button(item::UNDO, "Undo", Width::FILL).button(item::REDO, "Redo", Width::FILL);
            match (mode, workspace) {
                (Some(PlateMode::Mechanics), _) => mech_rows(rows, mech, plate),
                (Some(PlateMode::Monitor), _) => monitor_rows(rows, state),
                (_, w) if w.shows(Family::Arm) => arm_rows(rows, state),
                _ => {
                    if mode == Some(PlateMode::Design) {
                        // the plate view's render mode, as the desktop's switch has it
                        let mode = state.map(|s| s.render_mode());
                        let row = rows.row(Height::Fixed(52.0));
                        for (k, m) in fab::api::RenderMode::ALL.iter().enumerate() {
                            row.toggle(item::RENDER_MODE + k as u32, m.label(), mode == Some(*m), Width::FILL);
                        }
                    }
                    if mode == Some(PlateMode::Design) && mode_is_force(state) {
                        // the Force render mode: its display, detail, engine and results
                        force_rows(rows, state);
                    } else if mode == Some(PlateMode::Design) {
                        for chunk in [0..3usize, 3..ADD_PARTS.len()] {
                            let row = rows.row(Height::Fixed(52.0));
                            for k in chunk {
                                row.button(item::ADD_PART + k as u32, ADD_PARTS[k].1, Width::FILL);
                            }
                        }
                        let row = rows.row(Height::Fixed(52.0));
                        if duplicate {
                            row.button(item::DUPLICATE_LINKED, "Duplicate (linked)", Width::FILL)
                                .button(item::DUPLICATE_COPY, "Duplicate as copy", Width::FILL);
                            row.toggle(item::PRINT_PART, if printed == Some(false) { "Eye closed" } else { "Eye open" }, printed == Some(true), Width::FILL);
                        } else {
                            row.text("Duplicate (linked)", Tone::Meta, Width::FILL)
                                .text("Duplicate as copy", Tone::Meta, Width::FILL);
                        }
                    }
                    if slice_shown(state) {
                        rows.row(Height::Fixed(52.0)).toggle(item::PLAY, if playing { "Pause" } else { "Play" }, playing, Width::FILL);
                    }
                    let issues = report.as_ref().map(|r| r.issues.as_slice()).unwrap_or(&[]);
                    rows.row(Height::Fixed(52.0))
                        .button(item::PREV_ISSUE, "<", Width::Fixed(48.0))
                        .heading(format_args!("{} issue(s)", issues.len()), Tone::Meta, Width::FILL)
                        .button(item::NEXT_ISSUE, ">", Width::Fixed(48.0));
                    // what the Mechanics check found, as the desktop's list says it: it doesn't stop a print
                    if let Some(n) = state.map(|s| s.print.support_pieces.as_ref().map_or(0, |pieces| pieces.iter().filter(|p| p.light == fab::api::MechLight::Red).count())).filter(|n| *n > 0) {
                        rows.row(Height::Fixed(30.0)).text(&format!("Trapped supports: {n} (you can still print)"), Tone::Warning, Width::FILL);
                    }
                    for k in 0..ISSUE_ROWS {
                        rows.row(Height::Fill);
                        let id = item::ISSUE_ROW + k as u32;
                        match issues.get(first + k) {
                            Some(i) => {
                                let tone = match i.severity as u8 {
                                    0 => Tone::Error,
                                    1 => Tone::Warning,
                                    _ => Tone::Info,
                                };
                                match i.layer {
                                    Some(l) => rows.entry(id, format_args!("L{l} {}", i.message), tone, Width::FILL),
                                    None => rows.entry(id, format_args!("- {}", i.message), tone, Width::FILL),
                                };
                            }
                            None => {
                                rows.blank(id, Width::FILL);
                            }
                        }
                    }
                }
            }
        });
    }

    /// The issues the headset shows: the fresh slice's or the plate's, else (in Preview,
    /// without a fresh preview) the live slice's of the moved plate.
    fn report(&self, state: Option<&fab::AppState>) -> Option<Arc<fab_printability::Report>> {
        let live = self.live_shown.as_ref().and_then(|l| l.report.clone()).filter(|_| state.and_then(fresh_preview).is_none() && slice_shown(state));
        live.or_else(|| state.map(|s| s.print.report().clone()))
    }

    /// A pressed menu cell that goes to the sim or the document.
    fn duplicate_target<'a>(&'a self, state: Option<&'a fab::AppState>) -> Option<&'a str> {
        if let Some(object) = self.edit.part_target().and_then(|i| self.plate.as_ref()?.objects.get(i)) {
            return Some(object.as_str());
        }
        let state = state?;
        let active = state.scene_state.selection.active?;
        state.plate.parts.iter().find(|p| p.element == Some(active)).map(|p| p.object.as_str())
    }

    fn menu_action(&self, cx: &mut Cx, id: u32, state: Option<&fab::AppState>) {
        let mech = state.map(|s| &s.mech);
        match id {
            item::PRINT_PART => {
                if let Some(part) = self.duplicate_target(state).and_then(|o| state?.plate.parts.iter().find(|p| p.object == o))
                    .filter(|p| !p.insert && mode_of(state) == Some(PlateMode::Design) && !state.is_some_and(|s| s.slice_blocks_edits())) {
                    cx.action(fab::api::PartAction::SetPrint { objects: vec![part.object.clone()], print: !part.printed });
                }
            }
            item::DUPLICATE_LINKED | item::DUPLICATE_COPY => {
                if let Some(object) = self.duplicate_target(state).filter(|_| mode_of(state) == Some(PlateMode::Design)) {
                    cx.action(if id == item::DUPLICATE_COPY {
                        fab::api::PartAction::DuplicateCopy { object: object.to_string() }
                    } else {
                        fab::api::PartAction::DuplicateLinked { object: object.to_string() }
                    });
                }
            }
            item::VOICE => cx.action(crate::voice::VoiceAction::Toggle),
            item::MECH_PLAY => cx.action(if mech.is_some_and(|m| m.running) { MechAction::Pause } else { MechAction::Play }),
            item::MECH_RESET => cx.action(MechAction::Reset),
            item::ARM_PLAY => cx.action(if state.is_some_and(|s| s.arm.running) { ArmAction::Pause } else { ArmAction::Play }),
            item::ARM_RESET => cx.action(ArmAction::Reset),
            item::ARM_CHECKS => cx.action(ArmAction::RunChecks),
            item::FORCE_DEFORM => cx.action(ForceAction::Deform(if state.is_some_and(|s| s.force.deform > 0.0) { 0.0 } else { 0.5 })),
            item::FORCE_CLEAR => cx.action(ForceAction::Clear),
            item::FORCE_FLOOR => cx.action(ForceAction::FindFloor),
            _ if (item::FORCE_FIELD..item::FORCE_FIELD + ForceField::ALL.len() as u32).contains(&id) => {
                cx.action(ForceAction::Field(ForceField::ALL[(id - item::FORCE_FIELD) as usize]));
            }
            _ if (item::RENDER_MODE..item::RENDER_MODE + fab::api::RenderMode::ALL.len() as u32).contains(&id) => {
                cx.action(fab::api::ShellAction::SetRenderMode(fab::api::RenderMode::ALL[(id - item::RENDER_MODE) as usize]));
            }
            item::PRINTER_PAUSE => {
                let paused = state.and_then(|s| s.print.printer.as_ref()).is_some_and(|p| p.state.gcode_state.as_deref() == Some("PAUSE"));
                cx.action(PrintAction::PrinterControl(if paused { PrinterControl::Resume } else { PrinterControl::Pause }));
            }
            // Stop asks first, as the desktop's panel does
            item::PRINTER_STOP => cx.action(PrintAction::AskStop(true)),
            item::PRINTER_STOP_NO => cx.action(PrintAction::AskStop(false)),
            item::PRINTER_STOP_YES => cx.action(PrintAction::PrinterControl(PrinterControl::Stop)),
            item::FOLLOW_PRINTER => cx.action(PrintAction::FollowPrinter(!state.is_some_and(|s| s.print.follow.on))),
            item::MECH_SHAKE => cx.action(MechAction::Shake(!mech.is_some_and(|m| m.shaking))),
            item::MECH_PREV_PIECE | item::MECH_NEXT_PIECE => {
                let pieces = self.plate.as_ref().map_or(0, |p| p.ev.weaves.len());
                if pieces > 1 {
                    let at = mech.map_or(0, |m| m.piece).min(pieces - 1);
                    let piece = if id == item::MECH_NEXT_PIECE { (at + 1) % pieces } else { (at + pieces - 1) % pieces };
                    cx.action(MechAction::Piece(piece));
                }
            }
            _ => {
                if let Some(k) = id.checked_sub(item::MECH_TOOL).filter(|k| (*k as usize) < MECH_TOOLS.len()) {
                    cx.action(MechAction::Tool(MECH_TOOLS[k as usize].0));
                } else if let Some(k) = id.checked_sub(item::MECH_SCENE).filter(|k| (*k as usize) < MECH_SCENES.len()) {
                    cx.action(MechAction::Scene(MECH_SCENES[k as usize].0));
                } else if let Some(k) = id.checked_sub(item::MECH_SPEED).filter(|k| (*k as usize) < MECH_SPEEDS.len()) {
                    cx.action(MechAction::Speed(MECH_SPEEDS[k as usize].0));
                } else if let Some(k) = id.checked_sub(item::ADD_PART).filter(|k| (*k as usize) < ADD_PARTS.len()) {
                    cx.action(fab::api::PartAction::Add { shape: ADD_PARTS[k as usize].0.to_string() });
                }
            }
        }
    }

    /// A pressed menu cell as a command.
    fn menu_command(&self, id: u32, state: Option<&fab::AppState>) -> Option<Command> {
        if let Some(w) = id.checked_sub(item::WORKSPACE).and_then(|k| Workspace::ALL.get(k as usize)) {
            return Some(Command::SetWorkspace(*w));
        }
        Some(match id {
            item::REAL_SIZE => Command::RealSize,
            item::PLAY => Command::TogglePlay,
            item::UNDO => Command::Undo,
            item::REDO => Command::Redo,
            item::PREV_ISSUE => Command::PrevIssue,
            item::NEXT_ISSUE => Command::NextIssue,
            _ => {
                let k = id.checked_sub(item::ISSUE_ROW)? as usize;
                let count = self.report(state).map_or(0, |r| r.issues.len());
                let i = self.issue_first + k;
                return (k < ISSUE_ROWS && i < count).then_some(Command::Issue(i));
            }
        })
    }

    /// The chip's line while a shape handle is held: its value.
    fn fill_param_readout(&mut self, label: &str) {
        let mut key = 0x84222325cbf29ce4u64;
        for b in label.bytes() {
            key = (key ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
        self.chip.set(key, |rows| {
            rows.row(Height::Fixed(36.0)).text(label, Tone::Normal, Width::FILL);
        });
    }

    /// The rebuilding card over the part being rebuilt (its rows set once a rebuild starts).
    fn status_card(&mut self, state: Option<&fab::AppState>, plate: &Plate, transform: Mat4f, head: Vec3f) -> Option<Card> {
        let r = state.and_then(|s| s.params.rebuilding.as_ref())?;
        let i = plate.objects.iter().position(|o| *o == r.object)?;
        let b = plate.bounds[i];
        let top = [(b.min[0] + b.max[0]) * 0.5, (b.min[1] + b.max[1]) * 0.5, b.max[2]];
        let w = part_matrix(&self.edit, transform, i).transform_vec4(vec4((top[0] * 0.001) as f32, (top[2] * 0.001) as f32, (-top[1] * 0.001) as f32, 1.0));
        let at = vec3f(w.x, w.y, w.z) + vec3f(0.0, 0.05, 0.0);
        let key = r.since.to_bits();
        if self.status_shown != Some(key) {
            self.status_shown = Some(key);
            log!("fab xr status: Rebuilding… over {}", r.object);
        }
        self.status.set(0x5eb1_1d, |rows| {
            rows.row(Height::Fill).heading("Rebuilding…", Tone::Warning, Width::FILL);
        });
        Some(Card::facing(at, head, self.status.size()))
    }

    /// The layer readout's rows: (layer, layers, z mm).
    fn fill_readout(&mut self, state: Option<&fab::AppState>, (layer, total, z): (usize, usize, f32)) {
        let report = self.report(state);
        let mut key = 0xcbf2_9ce4_8422_2325u64;
        for v in [layer as u64, total as u64, z.to_bits() as u64, report.as_ref().map_or(0, |r| Arc::as_ptr(r) as u64)] {
            key = (key ^ v).wrapping_mul(0x100_0000_01b3);
        }
        self.readout.set(key, |rows| {
            let issues = report.as_ref().map(|r| r.issues.as_slice()).unwrap_or(&[]);
            let mut here = issues.iter().filter(|i| i.layer == Some(layer as u32));
            let count = here.clone().count();
            rows.row(Height::Fixed(52.0)).text(format_args!("Layer {} / {}   Z {:.2} mm", layer + 1, total, z), Tone::Normal, Width::FILL);
            rows.row(Height::Fixed(52.0));
            match here.next() {
                None => rows.text("no issues on this layer", Tone::Good, Width::FILL),
                Some(i) => rows.text(format_args!("{count} issue(s): {}", i.message), Tone::Warning, Width::FILL),
            };
        });
    }

    fn draw_xr(&mut self, cx: &mut Cx2d, state: Option<&fab::AppState>) {
        if self.xr.is_none() {
            return;
        }
        if let Some(plate) = self.pending.take() {
            self.install(cx.cx, plate, state);
        }
        let Some(plate) = self.plate.take() else {
            makepad_widgets::makepad_draw::xr_sink::xr_frame_empty(cx.cx, "waiting for plate");
            return;
        };
        self.draw_plate(cx, state, &plate);
        self.plate = Some(plate);
    }

    fn draw_plate(&mut self, cx: &mut Cx2d, state: Option<&fab::AppState>, plate: &Plate) {
        let bed = plate.bed;
        let xr = self.xr.as_mut().unwrap();
        let view = xr.nav.view();
        let transform = view.matrix();
        // where the plate is in the room, on change and at most once a second (a bad view, NaN
        // included, reads from the log)
        let room_bed = view.room_from_world(bed.center);
        let key = [view.scale, view.origin.x, view.origin.y, view.origin.z, view.yaw, room_bed.x, room_bed.y, room_bed.z];
        let now = Cx::time_now();
        if xr.logged_view.as_ref().is_none_or(|(k, at)| now - at >= 1.0 && k.iter().zip(&key).any(|(a, b)| !((a - b).abs() < 1.0e-3))) {
            xr.logged_view = Some((key, now));
            let h = xr.head;
            log!("fab xr: view scale={:.3} origin=({:.3}, {:.3}, {:.3}) yaw={:.3}, the bed's centre in the room ({:.3}, {:.3}, {:.3}), the head at ({:.3}, {:.3}, {:.3})", view.scale, view.origin.x, view.origin.y, view.origin.z, view.yaw, room_bed.x, room_bed.y, room_bed.z, h.x, h.y, h.z);
        }
        let workspace = workspace_of(state);
        let mode = mode_of(state);
        let preview = slice_shown(state);
        // Monitor shows the print it follows, else the model
        let slice_mode = preview || (mode == Some(PlateMode::Monitor) && state.is_some_and(|s| s.print.showing().is_some()));
        let in_mech = mode == Some(PlateMode::Mechanics);
        let fresh = state.and_then(fresh_preview).is_some();
        let grabbing = self.edit.grabbing() || self.edit.edits.any_waiting();
        let show_model = !plate.models.is_empty() && (!slice_mode || state.is_some_and(|s| s.print.play.show_model));
        // the live slice's newest result (the slice shows it while the fresh preview is not there)
        // a result let go of can hold the last of the plan's tiles: it is freed off the UI thread
        if plate.ev.plate.parts.is_empty() {
            if let Some(old) = self.live_shown.take() {
                crate::live_slice::let_go(old);
            }
        }
        if let Some(live) = self.live.as_ref().filter(|_| !plate.ev.plate.parts.is_empty()) {
            if let Some(r) = live.latest() {
                if self.live_shown.as_ref().is_none_or(|s| !Arc::ptr_eq(s, &r)) {
                    if r.stage == crate::live_slice::LiveStage::Refined {
                        log!("fab xr: live slice refined, revision {} cuts={} layers={} issues={}", r.revision, r.contours.all.count(), r.contours.top_layer + 1, r.report.as_ref().map_or(0, |x| x.issues.len()));
                    }
                    if let Some(old) = self.live_shown.replace(r) {
                        crate::live_slice::let_go(old);
                    }
                }
            }
        }
        // the parts drawn as fields (their meshes hide)
        let fields: Vec<String> = state.and_then(|s| s.sdf.as_ref()).filter(|_| show_model && cx.cx.xr_pass_eyes().is_some()).map_or(Vec::new(), |l| l.borrow().field_objects());
        let fields_key = fields.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, o| o.bytes().fold(h ^ 0xff, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3)));
        let moves_key = state.and_then(|s| s.scene_state.moves.as_ref()).map_or(0, |m| Arc::as_ptr(m) as *const u8 as usize);
        let poses_key = state.and_then(|s| s.poses_key(s.shared_poses()));
        let scene_key = state.map_or(0, |s| Arc::as_ptr(&s.scene) as *const u8 as usize);
        let key = (transform, show_model, self.parts_rev, fields_key, moves_key, poses_key, scene_key, in_mech);
        if self.placed != Some(key) {
            if in_mech {
                self.link_elements.follow(state, plate, scene_key);
            }
            let edit = &self.edit;
            let models = &plate.models;
            let objects = &plate.objects;
            let links = &self.link_elements;
            let mech_state = state.filter(|_| in_mech);
            let pose_state = state.filter(|_| in_mech || poses_key.is_some());
            let touch = self.touch;
            // a part or body a tip touches is drawn lighter
            let lit = |tint: Vec4f| vec4(tint.x * 1.3, tint.y * 1.3, tint.z * 1.3, tint.w);
            self.renderer.rebuild_models(|list| {
                let mut at = 0;
                let mut put = |id: &str, m: Mat4f, tint: Vec4f| {
                    if let Some(model) = list.get_mut(at) {
                        if model.model != id {
                            model.model.clear();
                            model.model.push_str(id);
                        }
                        model.transform = m;
                        model.tint = tint;
                    } else {
                        list.push(ModelInstance {
                            model: id.into(), custom_material: None,
                            layer_materials: Vec::new(),
                            transform: m,
                            tint, color_adjust: vec4(0., 1., 1., 0.),
                            dynamic: true, depth_order: 0., part_poses: Vec::new(), lamps: Default::default(),
                        });
                    }
                    at += 1;
                };
                if show_model {
                    let (yup, fab) = (yup_from_fab(), fab_from_yup());
                    for (i, id) in models.iter().enumerate() {
                        let excluded = i >= plate.ev.plate.parts.len();
                        if in_mech && excluded && poses_key.is_none() { continue; }
                        let room = part_matrix(edit, transform, i);
                        let selected = edit.selected.get(i).copied().unwrap_or(false);
                        // the sim moves each link: one instance of the link's model per link
                        if let (Some(s), Some(l)) = (mech_state, plate.links.get(i).and_then(|l| l.as_ref())) {
                            for (k, rest) in l.rest.iter().enumerate() {
                                let element = links.parts.get(i).and_then(|p| p.get(k).copied().flatten());
                                let world = match element.and_then(|e| s.shared_pose(e)) {
                                    Some(a) => Mat4f::mul(&pose_matrix(&a), rest),
                                    None => *rest,
                                };
                                let tint = l.tints.get(k).copied().unwrap_or(vec4(1., 1., 1., 1.));
                                let tint = if element.is_some() && touch.iter().any(|t| t.body == element) { lit(tint) } else { tint };
                                put(&l.model, Mat4f::mul(&room, &Mat4f::mul(&yup, &Mat4f::mul(&world, &fab))), tint);
                            }
                            continue;
                        }
                        // The headset and shell use placed order. While one plate is being replaced
                        // the other can still be on screen, so keep matching those parts by name.
                        let part = pose_state.and_then(|s| {
                            let k = if s.plate.parts.get(i).is_some_and(|p| p.object == objects[i]) { i }
                                else { s.plate.parts.iter().position(|p| p.object == objects[i])? };
                            Some((s, k))
                        });
                        let element = part.and_then(|(s, k)| s.plate.parts[k].element);
                        let pose = element.zip(pose_state).and_then(|(e, s)| s.shared_pose(e));
                        let assembled = poses_key.is_some() && pose.is_some();
                        if (in_mech && excluded && !assembled)
                            || (poses_key.is_some() && part.is_some_and(|(s, k)| s.plate.parts[k].hidden)) {
                            continue;
                        }
                        // a field part's mesh collapses (the field draws it), and so does a part shown
                        // only where the sim has it until the sim places it
                        let m = if fields.contains(&objects[i]) || (in_mech && part.is_some_and(|(s, k)| s.part_unposed(k))) {
                            Mat4f { v: [0.0; 16] }
                        } else {
                            match pose {
                                Some(a) => Mat4f::mul(&room, &Mat4f::mul(&yup, &Mat4f::mul(&pose_matrix(&a), &fab))),
                                None => room,
                            }
                        };
                        let tint = if excluded && !assembled { vec4(0.35, 0.35, 0.35, 0.18) } else if selected { vec4(1.25, 1.25, 1.05, 1.0) } else { vec4(1., 1., 1., 1.) };
                        let touched = touch.iter().any(|t| t.part == Some(i) || (element.is_some() && t.body == element));
                        put(id, m, if touched { lit(tint) } else { tint });
                    }
                    // the ball a cloth drapes over
                    if let Some((c, r)) = mech_state.and_then(|s| s.mech.ball) {
                        let (c, k) = (vec3f((c[0] * 0.001) as f32, (c[2] * 0.001) as f32, (-c[1] * 0.001) as f32), (r * 0.001) as f32);
                        let ball = Mat4f { v: [k, 0.0, 0.0, 0.0, 0.0, k, 0.0, 0.0, 0.0, 0.0, k, 0.0, c.x, c.y, c.z, 1.0] };
                        put(BALL_MODEL, Mat4f::mul(&transform, &ball), vec4(0.62, 0.64, 0.68, 1.0));
                    }
                }
                list.truncate(at);
            });
            self.placed = Some(key);
        }
        // while parts move, the slice shown is the live one (the preview is of where they were)
        let mut frame = if slice_mode && (fresh || !preview) && !grabbing { state.and_then(|s| PrintFrame::of(s, None)) } else { None };
        // the slice layer the hands are at: the preview's playhead, else the live slice's
        let readout = if slice_mode {
            match (&frame, &self.live_shown) {
                (Some(f), _) => {
                    let li = f.preview.layer_at(state.unwrap().print.play.time);
                    f.preview.layers.get(li).map(|l| (li, f.preview.layers.len(), l.z))
                }
                (None, Some(l)) => {
                    let layer = self.live_layer.min(l.contours.top_layer as usize);
                    Some((layer, l.contours.top_layer as usize + 1, (layer as f32 + 0.5) * l.layer_height))
                }
                _ => None,
            }
        } else {
            None
        };
        if let (Some(f), Some((li, _, _))) = (frame.as_mut(), readout) {
            // the current layer highlighted, the ones above it hidden
            let layer = f.preview.layers[li].layer;
            f.uniforms[1].1[1] = f.uniforms[1].1[1].min(layer as f32);
            f.uniforms[1].1[2] = layer as f32 + 1.0;
        }
        // the near sun cascade holds the build volume (not a ring around the eyes): the parts'
        // millimetre detail, a chain's 1.6 mm bars, keeps its shadows and their contacts
        {
            let h = plate.ev.machine.max_height as f32 * 0.001;
            let (c, half) = (bed.center, bed.size * 0.5);
            let (mut lo, mut hi) = (vec3f(f32::MAX, f32::MAX, f32::MAX), vec3f(f32::MIN, f32::MIN, f32::MIN));
            for k in 0..8 {
                let w = vec3f(c.x + if k & 1 == 0 { -half.x } else { half.x }, if k & 2 == 0 { -bed.size.y } else { h }, c.z + if k & 4 == 0 { -half.z } else { half.z });
                let r = transform.transform_vec4(vec4(w.x, w.y, w.z, 1.0)).to_vec3f();
                lo = vec3f(lo.x.min(r.x), lo.y.min(r.y), lo.z.min(r.z));
                hi = vec3f(hi.x.max(r.x), hi.y.max(r.y), hi.z.max(r.z));
            }
            self.renderer.set_csm_near_bounds(Some((lo, hi)));
        }
        // the eyes supply the cameras; this look only centres lighting on the moved bed
        let look = PreviewLook { target: view.room_from_world(bed.center), distance: 0.8, fov: 90.0, yaw: view.yaw, pitch: -0.5 };
        let rect = Rect { pos: dvec2(0., 0.), size: dvec2(1024., 1024.) };
        let Some(scene) = preview_scene_state(look, rect, Cx::time_now()) else { return };
        // the bed is Fab's ground plate: the stream panel offers its switch, and it is drawn only
        // with it on (off by default: the real table is the ground)
        let server = XrServer::get(cx.cx.cx);
        if let Some(s) = &server {
            s.set_ground_offered(true);
        }
        let ground = server.as_ref().is_some_and(|s| s.show_ground());
        // a held shape handle's value in a small chip beside it, else the layer grab's readout
        // by the hand
        let head = self.xr.as_ref().unwrap().head;
        let param_label = self.hout.label.clone();
        let chip_card = match &param_label {
            Some((at, text)) => {
                self.fill_param_readout(text);
                let em_deg = if cx.cx.cx.xr_display_kind() == XrDisplayKind::Handheld { CHIP_EM_DEG_HANDHELD } else { CHIP_EM_DEG_WORN };
                let size = self.chip.size();
                let aspect = size.x / size.y.max(1e-6);
                cx.cx.cx.xr_pass_eyes().map(|(cams, _)| chip_card(*at, &EyeFrame::of(&cams), aspect, em_deg, &mut self.chip_place))
            }
            None => {
                self.chip_place = ChipPlace::default();
                None
            }
        };
        let readout_card = match &param_label {
            Some(_) => None,
            None => readout.zip(self.out.readout_at.filter(|_| self.edit.layer_grab())).map(|(r, at)| {
                self.fill_readout(state, r);
                Card::beside(at, head, self.readout.size())
            }),
        };
        // over a part being rebuilt, from the value set until the full rebuild is in
        let status_card = self.status_card(state, plate, transform, head);
        let xr = self.xr.as_mut().unwrap();
        cx.begin_pass(&xr.pass, None);
        xr.root.begin_always(cx);
        // the pass's own root turtle: without it the pass's draws join the widget's turtle on
        // the desktop, whose alignment later shifts their rects by the widget's place there
        cx.begin_root_turtle_for_pass(Layout::flow_overlay());
        // the cards' textures: child passes of the XR pass (rendered before it), redrawn only
        // when what they show changed
        self.menu.render(cx);
        if readout_card.is_some() {
            self.readout.render(cx);
        }
        if chip_card.is_some() {
            self.chip.render(cx);
        }
        if status_card.is_some() {
            self.status.render(cx);
        }
        self.prof.step(cx, fields.len(), workspace);
        let xr = self.xr.as_mut().unwrap();
        self.shapes.clear();
        let cx3d = &mut Cx3d::new(cx.cx);
        let mut draws = SceneDraws {
            cube: Some(&mut self.draw_cube), alpha: Some(&mut self.draw_alpha), sky: &mut self.draw_sky,
            sky_analytic: None, terrain: Some(&mut self.draw_terrain), shadow: Some(&mut self.draw_shadow),
            shadow_sdf: None, firework: None, flare: None, water: None, screen: None,
            screen_instances: &[], view_model: None,
        };
        self.renderer.draw_preview(cx3d, &mut xr.draw_list, &mut draws, look, PreviewStage::empty(), scene, None, Some(&mut self.draw_models));
        self.shapes.light_dir = self.draw_cube.light_dir;
        // draw_preview closes its child list. Append here in the root, keeping its depth;
        // beginning that same model list again would erase the model's draw calls.
        // the parts' room boxes this frame: their contact shadows and the content's bounds
        let parts = part_boxes(&self.renderer, plate, &self.edit, transform, &fields);
        cx3d.begin_scene_3d(scene);
        self.draw_bed.transform = transform;
        self.draw_bed.cube_pos = bed.center - vec3f(0.0, bed.size.y * 0.5, 0.0);
        self.draw_bed.cube_size = bed.size;
        if !self.check.on {
            // the bed with the "Ground plate" switch on; else the real table under the parts,
            // and their contact shadows on it
            if ground {
                self.draw_bed.draw(cx3d);
            } else {
                self.contact.draw(cx3d, &mut self.draw_contact, &parts, transform);
            }
        }
        let place = PrintPlacement { origin: view.origin, scale: view.scale, yaw: view.yaw };
        // footprints under the parts the hands hold: green on the bed, red off it
        if self.edit.grabbing() {
            for (i, b) in plate.bounds.iter().enumerate() {
                if !self.edit.held(i) {
                    continue;
                }
                let m = xr_edit::moved_bounds(b, &self.edit.deltas[i]);
                let on_bed = m.min[0] >= plate.ev.machine.bed.min[0] && m.max[0] <= plate.ev.machine.bed.max[0] && m.min[1] >= plate.ev.machine.bed.min[1] && m.max[1] <= plate.ev.machine.bed.max[1];
                self.draw_mark.color = if on_bed { vec4(0.2, 0.75, 0.35, 1.0) } else { vec4(0.9, 0.2, 0.15, 1.0) };
                self.draw_mark.transform = transform;
                let c = [(m.min[0] + m.max[0]) * 0.5, (m.min[1] + m.max[1]) * 0.5];
                self.draw_mark.cube_pos = vec3f((c[0] * 0.001) as f32, 0.0008, (-c[1] * 0.001) as f32);
                self.draw_mark.cube_size = vec3f(((m.max[0] - m.min[0]) * 0.001) as f32, 0.0012, ((m.max[1] - m.min[1]) * 0.001) as f32);
                self.draw_mark.draw(cx3d);
            }
        }
        if mode == Some(PlateMode::Design) && !preview && !self.edit.grabbing() && !self.check.on {
            crate::xr_handles::draw_handles(&mut self.shapes, state, &self.edit, &self.handles, &self.hout, &view, transform, xr.head, Cx::time_now());
            if let Some(synth) = xr.handle_synth.as_mut() {
                let now = Cx::time_now();
                if now - synth.aligned_at >= 0.5 {
                    synth.aligned_at = now;
                    if let (Some(state), Some((cams, eye_px))) = (state, cx3d.cx.cx.xr_pass_eyes()) {
                        log_handle_alignment(plate, &self.edit, transform, state, &view, xr.head, &cams, eye_px);
                    }
                }
            }
        }
        let markers_on = state.is_none_or(|s| !s.print.hide_markers) && !self.check.on;
        if let Some(frame) = &frame {
            draw_print_placed(cx3d, frame, &mut self.print_palette, place, &xr_sdf_light(), (&mut self.draw_path, &mut self.draw_far, &mut self.far_tiers), &mut self.draw_head);
            self.grabs.print_frame(frame.preview.chunks.is_empty() && frame.cut.count() as usize >= frame.preview.beads.count());
            // The runs that changed tier, on the frames they did: none while the head holds still.
            if let Some((went, came, ranges)) = self.far_tiers.flips() {
                log!("fab xr far tier: {} runs went far, {} came near; ranges {} near, {} far", went, came, ranges[0], ranges[1]);
            }
            // One line per preview, and its first animated frame: evidence of the submitted
            // count without formatting or scanning the retained beads on subsequent frames.
            if (frame.cut.count() > 0 && xr.logged_preview != Some(frame.preview.id)) || (!frame.heads.is_empty() && xr.logged_heads != Some(frame.preview.id)) {
                log!("fab xr: print frame beads={} heads={} model={} time={:.3}s scale={:.3} origin=({:.3},{:.3},{:.3}) yaw={:.3}",
                    frame.cut.count(), frame.heads.len(), show_model, frame.uniforms[0].1[0], view.scale, view.origin.x, view.origin.y, view.origin.z, view.yaw);
                if frame.cut.count() > 0 { xr.logged_preview = Some(frame.preview.id); }
                if !frame.heads.is_empty() { xr.logged_heads = Some(frame.preview.id); }
            }
        } else if let (true, Some(live)) = (preview, &self.live_shown) {
            if self.edit.grabbing() && xr.logged_live != Some(self.parts_rev_grab) {
                xr.logged_live = Some(self.parts_rev_grab);
                log!("fab xr: live slice while the parts move: {} cuts, {} layers", live.contours.all.count(), live.contours.top_layer + 1);
            }
            // the live slice: its toolpath beads once released (else every layer's contour)
            // up to the current layer, the current one lit
            let lit = self.live_layer.min(live.contours.top_layer as usize) as f32;
            if let Some(pv) = &live.preview {
                let mode = state.map_or(0.0, |s| s.print.play.color.shader_mode());
                let frame = PrintFrame {
                    preview: pv.clone(),
                    cut: fab::print::RunCut::all(&pv.beads),
                    travel_count: 0,
                    uniforms: [
                        // its tiles whole, whenever their beads print
                        (live_id!(u_now), [1e9, 0.0, 0.0, 0.0]),
                        (live_id!(u_layers), [0.0, lit, lit + 1.0, 0.0]),
                        (live_id!(u_mode), [mode, pv.speed_range[0], pv.speed_range[1], pv.max_layer() as f32]),
                        (live_id!(u_live), [1e9, 0.0, 0.0, 0.0]),
                    ],
                    heads: Vec::new(),
                    show_markers: false,
                    hovered_marker: None,
                    compare_planar: false,
                    show_supports: state.is_none_or(|s| s.print.play.supports),
                    // The live slice is the newest one there is.
                    faded: fab::viewport::print::Faded::None,
                    parts: Vec::new(),
                    lit: None,
                    closed: Vec::new(),
                };
                draw_print_placed(cx3d, &frame, &mut self.print_palette, place, &xr_sdf_light(), (&mut self.draw_path, &mut self.draw_far, &mut self.far_tiers), &mut self.draw_head);
            } else {
                draw_overlay_placed(cx3d, &live.contours.all, &mut self.overlay_palette, place, [0.0, lit, lit + 1.0], None, &mut self.draw_overlay.draw_vars);
            }
            if markers_on {
                if let Some(m) = &live.markers {
                    draw_overlay_placed(cx3d, m, &mut self.overlay_palette, place, [0.0, lit, 0.0], None, &mut self.draw_overlay.draw_vars);
                }
            }
        }
        if !self.check.on {
            let now = Cx::time_now();
            if in_mech {
                use fab::handle_style::*;
                for (i, at) in self.mout.touch_at.iter().enumerate() {
                    let Some(at) = at else { continue };
                    let visual = if self.mech.held_hand() == Some(i) { HandleState::Active } else { HandleState::Hover };
                    let r = crate::xr_handles::ball_radius((*at - xr.head).length(), crate::xr_handles::REACH_M) * visual.scale();
                    self.shapes.ring(*at, (xr.head - *at).normalize(), r * 1.8, r * 0.18, crate::xr_handles::shape_style(GRAB_HALO, visual, now));
                    self.shapes.sphere(*at, r, crate::xr_handles::shape_style(GRAB_BALL, visual, now));
                }
            } else {
                self.edit.draw_grab_controls(&mut self.shapes, self.tip_at, self.touch.map(|t| t.part), state.map_or(fab::api::GizmoMode::Off, |s| s.ui.gizmo), xr.head, now);
            }
        }
        if !slice_mode && !in_mech && markers_on && !grabbing {
            // printability on the model: the plate's markers
            if let Some(m) = state.and_then(|s| s.print.markers()) {
                draw_overlay_placed(cx3d, m, &mut self.overlay_palette, place, [0.0, 1e9, 0.0], None, &mut self.draw_overlay.draw_vars);
            }
        } else if !slice_mode && !in_mech && markers_on && grabbing {
            // while parts move: the live slice's printability once it has it
            if let Some(m) = self.live_shown.as_ref().and_then(|l| l.markers.as_ref()) {
                draw_overlay_placed(cx3d, m, &mut self.overlay_palette, place, [0.0, 1e9, 0.0], None, &mut self.draw_overlay.draw_vars);
            }
        }
        if !self.check.on {
            self.tips.draw(cx3d, self.tip_at, self.touch.map(|t| t.any()));
            self.menu.draw(cx3d);
        }
        if let Some(card) = readout_card.as_ref().filter(|_| !self.check.on) {
            self.readout.draw(cx3d, card);
        }
        if let Some(card) = chip_card.as_ref().filter(|_| !self.check.on) {
            self.chip.draw(cx3d, card);
        }
        if let Some(card) = status_card.as_ref().filter(|_| !self.check.on) {
            self.status.draw(cx3d, card);
        }
        cx3d.end_scene_3d();
        // the field parts as tiles, depth-tested with the rest
        if let (true, Some((eyes, eye_px)), Some(layer)) = (!fields.is_empty(), cx.cx.xr_pass_eyes(), state.and_then(|s| s.sdf.clone())) {
            let world_from = |object: &str| plate_world(plate, &self.edit, transform, object);
            layer.borrow_mut().stereo_tiles(cx, &mut self.sdf_tile_lit, &xr_sdf_light(), &eyes, eye_px, &world_from, state.unwrap());
        }
        // Field shading above writes its real depth late. Both handle passes must follow it.
        let cx3d = &mut Cx3d::new(cx.cx);
        cx3d.begin_scene_3d(scene);
        self.shapes.draw(cx3d);
        cx3d.end_scene_3d();
        cx.end_pass_sized_turtle();
        // where Fab's content is, so a handheld's view can be narrowed to it (the rest of the
        // picture is transparent)
        if let Some(server) = &server {
            let menu = self.menu.card();
            let cards = [chip_card.as_ref(), readout_card.as_ref(), status_card.as_ref(), menu.as_ref()];
            server.set_content_bounds(Some(content_boxes(&parts, transform, ground.then_some((bed.center, bed.size)), self.shapes.bounds(), &cards)));
        }
        let xr = self.xr.as_mut().unwrap();
        xr.root.end(cx);
        // The framework's XR frame sink packs and sends these attachments.
        cx.end_pass(&xr.pass);
        let eye_px = cx.cx.xr_pass_eyes().map(|e| e.1);
        self.check.step(cx.cx, &xr._color, !fields.is_empty(), eye_px);
        if let Some(name) = self.check.picture.take() {
            self.grabs.ask(name);
        }
        let menu_probe = match (self.menu.card(), cx.cx.xr_pass_eyes()) {
            (Some(card), Some((cams, eye_px))) => self.menu.panel.lit_v().map(|v| CardProbe { card, cam: cams[0], eye_px, v }),
            _ => None,
        };
        // FAB_XR_PASS_STATS=1: what the pass drew, four times a second, as numbers (no picture
        // kept); no faster than they are counted
        if std::env::var_os("FAB_XR_PASS_STATS").is_some() && Cx::time_now() - self.grabs.stats_at >= 0.25 && !self.grabs.busy() {
            self.grabs.stats_at = Cx::time_now();
            self.grabs.want = Some("stats".to_string());
        }
        self.grabs.step(cx.cx, &xr._color, self.menu.panel.texture(), menu_probe);
        if show_model && !xr.logged_model {
            xr.logged_model = true;
            log!("fab xr: first model frame, bed {:.3} x {:.3} m, scale {:.3}, {} part(s)", bed.size.x, bed.size.z, view.scale, plate.models.len());
        }
    }
}

/// A finished grab goes to the model worker as one edit (one undo step): the app answers with
/// [`FabXr::sent`] once it has the job.
struct ModelWorker<'a>(&'a mut Cx);

impl EditSink<Vec<fab_ai::PartMove>> for ModelWorker<'_> {
    fn commit(&mut self, moves: Vec<fab_ai::PartMove>) {
        self.0.action(FabXrAction::Place(moves));
    }
}

/// `FAB_XR_HANDLE_SYNTH=1`: where the selected part's handles sit on the part as the headset
/// draws them. Per handle: its grip's distance to the part's surface (the nearest point of its
/// mesh, printer mm) and, per eye, how far apart (pixels) the grip as the handles draw it and that
/// surface point as the part draws it (its model, or its field through the field's own frame)
/// land on the eye's image.
#[allow(clippy::too_many_arguments)]
fn log_handle_alignment(plate: &Plate, edit: &XrEdit, transform: Mat4f, state: &fab::AppState, view: &XrNavView, head: Vec3f, cams: &[(Mat4f, Mat4f); 2], eye_px: DVec2) {
    use fab::params::*;
    let Some(i) = edit.selected.iter().position(|s| *s) else { return };
    let (Some(pp), Some(part)) = (state.params.part(i).map(|pp| state.params.shown_part(pp)), plate.ev.plate.placed_part(i)) else { return };
    let m = part_matrix(edit, transform, i);
    let world = |p: [f64; 3]| vec3f((p[0] * 0.001) as f32, (p[2] * 0.001) as f32, (-p[1] * 0.001) as f32);
    // the field as the layer draws it, against the evaluation the mesh is of
    let field = state.plate.parts.iter().find(|q| q.object == part.object).and_then(|q| q.sdf.as_ref()).zip(plate.ev.placed_field(i));
    let at_field = |c: [f64; 3]| -> [f64; 3] {
        let Some((drawn, ev)) = field else { return c };
        let shape = xr_edit::apply(&xr_edit::inverse(ev.distance().2), c);
        xr_edit::apply(&drawn.to_printer, shape)
    };
    let project = |cam: &(Mat4f, Mat4f), scene: Vec3f| -> Option<DVec2> {
        let c = cam.1.transform_vec4(cam.0.transform_vec4(vec4(scene.x, scene.y, scene.z, 1.0)));
        (c.w > 1e-6).then(|| dvec2((c.x / c.w * 0.5 + 0.5) as f64 * eye_px.x, (0.5 - c.y / c.w * 0.5) as f64 * eye_px.y))
    };
    let eye = xr_edit::printer_from_room(view, head);
    for p in fab::tools::handles::shown_params(state, &pp) {
        let Some(h) = &p.handle else { continue };
        if matches!(h, ParamHandle::Profile { .. } | ParamHandle::Count { .. }) {
            continue;
        }
        let g = grip_point(h, HandleGrip::default(), unit(sub(eye, grip_point(h, HandleGrip::default(), eye))));
        // a part drawn from its field has no mesh: its surface is its field's
        let (d, c) = match field {
            Some((drawn, _)) if part.mesh.triangles.is_empty() => fab::viewport::sdf::nearest_on_field(drawn, g),
            _ => closest_on_mesh(&part.mesh, g),
        };
        // a value set and not yet built: the part is still the one before it
        let live = state.params.dragging.as_ref().is_some_and(|(k, _)| *k == p.key) || state.params.pending.iter().any(|(k, _)| *k == p.key);
        let handle_at = m.transform_vec4(vec4(world(g).x, world(g).y, world(g).z, 1.0));
        let cf = if part.mesh.triangles.is_empty() { c } else { at_field(c) };
        let surface_at = m.transform_vec4(vec4(world(cf).x, world(cf).y, world(cf).z, 1.0));
        let px: Vec<String> = cams
            .iter()
            .map(|cam| match (project(cam, vec3f(handle_at.x, handle_at.y, handle_at.z)), project(cam, vec3f(surface_at.x, surface_at.y, surface_at.z))) {
                (Some(a), Some(b)) => format!("{:.1}", (a - b).length()),
                _ => "behind".into(),
            })
            .collect();
        log!("fab xr handle align: {} {} surface {:.2} mm field {} eyes [{}] px{}", p.key, format_value(p, &p.value), d, if field.is_some() { "yes" } else { "no" }, px.join(", "), if live { " (building)" } else { "" });
    }
}

/// The nearest point of a triangle mesh to `p`: (distance, point).
fn closest_on_mesh(mesh: &fab_plate::PartMesh, p: [f64; 3]) -> (f64, [f64; 3]) {
    use fab::params::{add, dot, length, mul, sub};
    let mut best = (f64::MAX, p);
    for t in &mesh.triangles {
        let [a, b, c] = t.map(|k| mesh.positions[k as usize]);
        // Ericson, Real-Time Collision Detection 5.1.5
        let (ab, ac, ap) = (sub(b, a), sub(c, a), sub(p, a));
        let (d1, d2) = (dot(ab, ap), dot(ac, ap));
        let q = if d1 <= 0.0 && d2 <= 0.0 {
            a
        } else {
            let bp = sub(p, b);
            let (d3, d4) = (dot(ab, bp), dot(ac, bp));
            let cp = sub(p, c);
            let (d5, d6) = (dot(ab, cp), dot(ac, cp));
            let (vc, vb, va) = (d1 * d4 - d3 * d2, d5 * d2 - d1 * d6, d3 * d6 - d5 * d4);
            if d3 >= 0.0 && d4 <= d3 {
                b
            } else if d6 >= 0.0 && d5 <= d6 {
                c
            } else if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
                add(a, mul(ab, d1 / (d1 - d3)))
            } else if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
                add(a, mul(ac, d2 / (d2 - d6)))
            } else if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
                add(b, mul(sub(c, b), (d4 - d3) / ((d4 - d3) + (d5 - d6))))
            } else {
                let den = 1.0 / (va + vb + vc);
                add(a, add(mul(ab, vb * den), mul(ac, vc * den)))
            }
        };
        let d = length(sub(p, q));
        if d < best.0 {
            best = (d, q);
        }
    }
    best
}

/// The preview when it is of the plate shown (not stale).
fn fresh_preview(s: &fab::AppState) -> Option<&Arc<fab::print::PrintPreview>> {
    if s.print.stale {
        return None;
    }
    s.print.preview.as_ref().filter(|p| p.has_beads())
}

/// The bed at real size `-TABLE[2]` in front of `head` along its level forward, its top
/// `RESET_DROP_M` below the eyes, turned to face them, the plate's `bed` at the anchor: where it
/// went, for the log.
/// The contact shadows' mesh, kept between frames: the geometry is uploaded again only when the
/// parts or the bed moved.
#[derive(Default)]
struct ContactShadows {
    mesh: ShadowMeshBuilder,
    geometry: Option<Geometry>,
    uploaded: Vec<f32>,
}

/// How far a part's contact shadow reaches past its footprint (times its half size), and how dark
/// it is at its middle: light, so the desk reads through it.
const CONTACT_SPREAD: f32 = 1.5;
const CONTACT_ALPHA: f32 = 0.45;
/// A part this far above the plane at real size casts none; nearer, a smaller and fainter one.
const CONTACT_REACH_M: f32 = 0.15;
/// Above the surface, in metres at real size: clear of the desk's depth, under any part.
const CONTACT_LIFT_M: f32 = 0.0004;

/// The room bounds of each part shown (placed, posed or held by the sim): its model's bounds
/// through the matrix just handed to the renderer. Faint parts (left off the plate) and
/// collapsed ones (drawn as fields) are left out.
fn shown_part_boxes(renderer: &Renderer) -> impl Iterator<Item = (Vec3f, Vec3f)> + '_ {
    renderer.placed_models().iter().filter(|m| m.tint.w >= 0.5).filter_map(|m| {
        let (lo, hi) = renderer.model_bounds(&m.model)?;
        let (mut min, mut max) = (vec3f(f32::MAX, f32::MAX, f32::MAX), vec3f(f32::MIN, f32::MIN, f32::MIN));
        for k in 0..8 {
            let p = vec3f(if k & 1 == 0 { lo.x } else { hi.x }, if k & 2 == 0 { lo.y } else { hi.y }, if k & 4 == 0 { lo.z } else { hi.z });
            let w = m.transform.transform_vec4(vec4(p.x, p.y, p.z, 1.0));
            min = vec3f(min.x.min(w.x), min.y.min(w.y), min.z.min(w.z));
            max = vec3f(max.x.max(w.x), max.y.max(w.y), max.z.max(w.z));
        }
        Some((min, max))
    })
}

/// The room boxes of the parts shown: the meshes' from the renderer (`shown_part_boxes`), the
/// parts drawn as fields (`fields`: their objects) from the plate's bounds through their
/// placement (the bed's and the hands' move).
fn part_boxes(renderer: &Renderer, plate: &Plate, edit: &XrEdit, transform: Mat4f, fields: &[String]) -> Vec<(Vec3f, Vec3f)> {
    let mut boxes: Vec<(Vec3f, Vec3f)> = shown_part_boxes(renderer).collect();
    for (i, (object, b)) in plate.objects.iter().zip(&plate.bounds).enumerate() {
        if !fields.contains(object) {
            continue;
        }
        let m = part_matrix(edit, transform, i);
        let (mut lo, mut hi) = (vec3f(f32::MAX, f32::MAX, f32::MAX), vec3f(f32::MIN, f32::MIN, f32::MIN));
        for k in 0..8 {
            // printer mm (z up) into the part's world (y up, metres)
            let p = [if k & 1 == 0 { b.min[0] } else { b.max[0] }, if k & 2 == 0 { b.min[1] } else { b.max[1] }, if k & 4 == 0 { b.min[2] } else { b.max[2] }];
            let w = m.transform_vec4(vec4((p[0] * 0.001) as f32, (p[2] * 0.001) as f32, (-p[1] * 0.001) as f32, 1.0));
            lo = vec3f(lo.x.min(w.x), lo.y.min(w.y), lo.z.min(w.z));
            hi = vec3f(hi.x.max(w.x), hi.y.max(w.y), hi.z.max(w.z));
        }
        boxes.push((lo, hi));
    }
    boxes
}

/// Where Fab's XR content lies (room boxes, min and max corners): each part shown (`parts`)
/// with its contact shadow's reach around its foot, the bed while it is drawn (`bed`: its
/// centre and size in the printer's space), the handles (`shapes`) and the cards. Nothing else
/// is drawn.
fn content_boxes(parts: &[(Vec3f, Vec3f)], transform: Mat4f, bed: Option<(Vec3f, Vec3f)>, shapes: Option<(Vec3f, Vec3f)>, cards: &[Option<&Card>]) -> Vec<[[f32; 3]; 2]> {
    let base_y = transform.transform_vec4(vec4(0.0, 0.0, 0.0, 1.0)).y;
    let mut boxes = Vec::new();
    let mut push = |pts: &[Vec3f]| {
        let (mut lo, mut hi) = (vec3f(f32::MAX, f32::MAX, f32::MAX), vec3f(f32::MIN, f32::MIN, f32::MIN));
        for p in pts {
            lo = vec3f(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
            hi = vec3f(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
        }
        boxes.push([[lo.x, lo.y, lo.z], [hi.x, hi.y, hi.z]]);
    };
    for &(min, max) in parts {
        // (the shadow reaches `CONTACT_SPREAD` times the part's half size from its middle)
        let (sx, sz) = ((max.x - min.x) * 0.5 * (CONTACT_SPREAD - 1.0), (max.z - min.z) * 0.5 * (CONTACT_SPREAD - 1.0));
        push(&[vec3f(min.x - sx, min.y.min(base_y), min.z - sz), vec3f(max.x + sx, max.y, max.z + sz)]);
    }
    if let Some((c, size)) = bed {
        // the drawn plate: its top at the bed's centre, its thickness below
        let corners: Vec<Vec3f> = (0..8)
            .map(|k| {
                let w = vec3f(c.x + if k & 1 == 0 { -0.5 } else { 0.5 } * size.x, c.y - if k & 2 == 0 { 0.0 } else { size.y }, c.z + if k & 4 == 0 { -0.5 } else { 0.5 } * size.z);
                transform.transform_vec4(vec4(w.x, w.y, w.z, 1.0)).to_vec3f()
            })
            .collect();
        push(&corners);
    }
    if let Some((lo, hi)) = shapes {
        push(&[lo, hi]);
    }
    for card in cards.iter().flatten() {
        let (r, u) = (card.right * (card.size.x * 0.5), card.up * (card.size.y * 0.5));
        push(&[card.center - r - u, card.center + r - u, card.center - r + u, card.center + r + u]);
    }
    boxes
}

impl ContactShadows {
    /// One soft blob per part shown (`parts`: their room boxes, `part_boxes`), under where it is
    /// drawn now, on the bed's plane. A part off the plane (lifted, falling) casts a smaller,
    /// fainter one; one well above casts none.
    fn draw(&mut self, cx3d: &mut Cx3d, draw: &mut DrawSceneShadow, parts: &[(Vec3f, Vec3f)], transform: Mat4f) {
        let up = |m: &Mat4f| vec3f(m.v[4], m.v[5], m.v[6]).length();
        let scale = up(&transform);
        let base_y = transform.transform_vec4(vec4(0.0, 0.0, 0.0, 1.0)).y;
        let sun = SunLight { shadow_alpha: CONTACT_ALPHA * 0.35 / BASE_SHADOW_ALPHA, ..SunLight::default() };
        let receiver = Receiver { base_y, terrain: None, statics: &[] };
        let mesh = &mut self.mesh;
        mesh.clear();
        for &(min, max) in parts {
            // the builder fades a blob over a game world's drop: the part's drop at real size
            // goes on that scale, so `CONTACT_REACH_M` up it is gone
            let drop = (min.y - base_y) / scale.max(1e-6) / CONTACT_REACH_M * MAX_SHADOW_DROP;
            let centre = vec3f((min.x + max.x) * 0.5, base_y + drop, (min.z + max.z) * 0.5);
            build_blob_shadow(centre, (max.x - min.x) * 0.5 * CONTACT_SPREAD, (max.z - min.z) * 0.5 * CONTACT_SPREAD, &sun, &receiver, mesh);
        }
        // on the plane itself, just above it (the builder's lift is a game world's)
        let lift = base_y + CONTACT_LIFT_M * scale;
        for v in mesh.vertices.chunks_exact_mut(SHADOW_VERTEX_FLOATS) {
            v[1] = lift;
        }
        if mesh.is_empty() {
            return;
        }
        let geometry = self.geometry.get_or_insert_with(|| Geometry::new(cx3d.cx));
        if self.uploaded != self.mesh.vertices {
            self.uploaded.clone_from(&self.mesh.vertices);
            geometry.update(cx3d.cx, std::mem::take(&mut self.mesh.indices), std::mem::take(&mut self.mesh.vertices));
        }
        draw.draw_vars.geometry_id = Some(geometry.geometry_id());
        if draw.draw_vars.can_instance() {
            let area = cx3d.add_instance(&mut draw.draw_vars);
            draw.draw_vars.area = cx3d.update_area_refs(draw.draw_vars.area, area);
        }
    }
}

/// Where the bed is set down at real size: in front of a worn display's head and below it; on a
/// handheld's view's centre ray; or, for a handheld's place request (`at`: a point on a real
/// surface the person tapped), on that point. Level and turned to face the head each time.
fn set_bed_down(xr: &mut XrPass, head: Pose, bed: Vec3f, handheld: bool, at: Option<Vec3f>) -> String {
    xr.real_size = None;
    xr.still_since = None;
    let forward = head.orientation.rotate_vec3(&vec3f(0.0, 0.0, -1.0));
    match at {
        // (from the point itself: the bed's centre there, turned as the head is)
        Some(at) => xr.nav.place_before(Pose::new(head.orientation, vec3f(at.x, head.position.y, at.z)), 0.0, at.y, 1.0, bed),
        None if handheld => {
            let level = (forward.x * forward.x + forward.z * forward.z).sqrt();
            xr.nav.place_before(head, HANDHELD_AHEAD_M * level, head.position.y + HANDHELD_AHEAD_M * forward.y, 1.0, bed);
        }
        None => xr.nav.place_before(head, -TABLE[2], head.position.y - RESET_DROP_M, 1.0, bed),
    }
    let (view, at) = (xr.nav.view(), head.position);
    let bed = view.room_from_world(bed);
    format!("the bed at ({:.3},{:.3},{:.3}) turned {:.3}, scale {:.3}; the head at ({:.3},{:.3},{:.3})", bed.x, bed.y, bed.z, view.yaw, view.scale, at.x, at.y, at.z)
}

impl Widget for FabXr {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // Quit or relaunch: the live slice worker ends (dropping it wakes and stops it).
        if let Event::Shutdown = event {
            self.live = None;
            return;
        }
        // A linking headset can deliver its first pose before the next desktop draw.
        if self.xr.is_none() && cx.xr_display() {
            self.activate(cx);
        }
        let (Some(xr), Event::XrUpdate(update)) = (self.xr.as_mut(), event) else { return };
        let now = Cx::time_now();
        let dt = xr.stepped_at.map_or(0.0, |t| (now - t) as f32);
        let bed = self.plate.as_ref().map_or(vec3f(0.0, 0.0, 0.0), |p| p.bed.center);
        // the menu held (the headset's long press): the bed back at real size in front of the
        // head and below it, facing it. A handheld's hold is its "Place here": the left aim's tip
        // is the point on a real surface the person tapped, and the bed goes there.
        let handheld = cx.xr_display_kind() == XrDisplayKind::Handheld;
        if update.held_menu() {
            let left = &update.state.left_controller;
            let at = (handheld && left.active()).then_some(left.aim_pose.position);
            log!("fab xr: view reset{}: {}", if at.is_some() { " on the tapped surface" } else { "" }, set_bed_down(xr, update.state.head_pose, bed, handheld, at));
            xr.placed = Some(update.state.space);
        }
        // and the same once per tracking space (a headset session, a recenter: its origin may
        // have moved, and the bed with it), from a settled head: one held still a moment, not
        // the first poses of a headset going on
        if xr.placed != Some(update.state.space) {
            let (s, l) = (&update.state, &update.last);
            let span = (s.time - l.time) as f32;
            let moved = (s.head_pose.position - l.head_pose.position).length();
            let turned = 2.0 * s.head_pose.orientation.dot(l.head_pose.orientation).abs().min(1.0).acos();
            let still = !update.space_changed() && l.time > 0.0 && span > 0.0 && moved < STILL_M_S * span && turned < STILL_RAD_S * span;
            if !still {
                xr.still_since = None;
            } else if xr.still_since.is_none() {
                xr.still_since = Some(l.time);
            }
            if xr.still_since.is_some_and(|t| s.time - t >= SETTLE_S) {
                log!("fab xr: the bed set down for tracking space {}: {}", s.space, set_bed_down(xr, s.head_pose, bed, handheld, None));
                xr.placed = Some(s.space);
            }
        }
        xr.stepped_at = Some(now);
        if dt > 0.0 {
            if std::env::var("FAB_XR_PROF").as_deref() == Ok("1") {
                self.prof.dts.push(dt);
            }
            if xr.frame_dt > 0.0 && dt > 2.0 * xr.frame_dt && dt > 0.02 {
                let ws = scope.data.get::<fab::AppState>().map_or("", |s| s.ui.kind().label());
                log!("fab xr: frame gap {:.0} ms ({:.1} ms frames) in {ws}", dt * 1000.0, xr.frame_dt * 1000.0);
            } else {
                xr.frame_dt = if xr.frame_dt > 0.0 { xr.frame_dt * 0.95 + dt.min(0.1) * 0.05 } else { dt.min(0.1) };
            }
        }
        let synth = xr.synth.then(|| {
            let mut state = (*update.state).clone();
            synth_controllers(&mut state, xr.synth_clock.at(now) as f32);
            state
        });
        let mut input = synth.as_ref().unwrap_or(&update.state);
        // (the scripted controllers start once the bed is set down: it moves then)
        let placed = xr.placed.is_some();
        let edit_synth = match (&mut xr.edit_synth, &self.plate) {
            (Some(script), Some(plate)) if placed && !plate.bounds.is_empty() => {
                let mut s = (*update.state).clone();
                script.step(&mut s, now, &xr.nav.view(), plate.bounds[0], &self.edit.deltas.first().copied().unwrap_or(xr_edit::IDENTITY));
                Some(s)
            }
            _ => None,
        };
        if let Some(s) = &edit_synth {
            input = s;
        }
        let synth_slice_row = xr.edit_synth.as_mut().is_some_and(|s| std::mem::take(&mut s.slice_row));
        let state = scope.data.get::<fab::AppState>();
        let handle_synth = match (&mut xr.handle_synth, &self.plate) {
            (Some(script), Some(plate)) if placed && script.part < plate.bounds.len() => {
                let mut s = (*update.state).clone();
                let k = script.part;
                let pp = state.and_then(|st| st.params.part(k).map(|pp| st.params.shown_part(pp)));
                script.step(&mut s, now, &xr.nav.view(), plate.bounds[k], self.edit.selected.get(k).copied().unwrap_or(false), pp.as_ref(), state);
                Some(s)
            }
            _ => None,
        };
        if let Some(s) = &handle_synth {
            input = s;
        }
        let mech_synth = match (&mut xr.mech_synth, &self.plate, state) {
            (Some(script), Some(plate), Some(state)) if placed && !plate.bounds.is_empty() => {
                let mut s = (*update.state).clone();
                let view = xr.nav.view();
                let held = self.mech_track.and_then(|(e, rest)| crate::xr_mech::now_point(state, e, rest)).map(|p| room_from_fab(&view, p));
                script.step(&mut s, now, &view, state, &self.menu, &mut self.grabs, held, self.tip_at);
                Some(s)
            }
            _ => None,
        };
        if let Some(s) = &mech_synth {
            input = s;
        }
        self.step(cx, input, dt, state);
        let interacting = self.buttons.any_held()
            || self.edit.grabbing() || self.handles.held_hand().is_some() || self.mech.held_hand().is_some() || self.mech.carrying_hand().is_some()
            || self.menu_step.slider.is_some_and(|drag| !drag.ended)
            || self.xr.as_ref().is_some_and(|xr| xr.nav.grips().into_iter().any(|held| held))
            || [&input.left_controller, &input.right_controller].into_iter()
                .any(|c| makepad_xr_nav::pad_stick(c) != Vec2f::default());
        makepad_xr_stream::XrDesktopFreeze::set_interaction(cx, interacting);
        if synth_slice_row {
            self.command(cx, Command::SliceView(true), state);
        }
        let xr = self.xr.as_mut().unwrap();
        if xr.synth && now - xr.logged_at >= 5.0 {
            xr.logged_at = now;
            let v = xr.nav.view();
            log!("fab xr: nav grips={:?} scale={:.3} origin=({:.3},{:.3},{:.3}) yaw={:.3}", xr.nav.grips(), v.scale, v.origin.x, v.origin.y, v.origin.z, v.yaw);
        }
        self.draw_tick.redraw(cx);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if self.xr.is_none() && cx.cx.xr_display() {
            self.activate(cx.cx);
        }
        // the headset's view only for its frames: a desktop frame never renders it again
        if self.xr.is_some() && cx.cx.drawing_xr_frame() {
            self.draw_xr(cx, scope.data.get_mut::<fab::AppState>().map(|s| &*s));
        }
        self.draw_tick.draw_walk(cx, walk);
        self.tick.0 = self.draw_tick.area();
        makepad_xr_stream::XrDesktopFreeze::bind(cx, self.draw_tick.area());
        DrawStep::done()
    }
}

/// The rows of the Mechanics panel: tools; pause, reset, shake; the scene; the speed; the chain part when
/// the plate has several; what the sim says.
fn mech_rows(rows: &mut makepad_xr_ui::Rows, mech: Option<&fab::api::MechView>, plate: Option<&Plate>) {
    let Some(m) = mech else { return };
    for (row_index, tools) in MECH_TOOLS.chunks(3).enumerate() {
        let row = rows.row(Height::Fixed(52.0));
        for (column, (tool, label)) in tools.iter().enumerate() {
            let k = row_index * 3 + column;
            row.toggle(item::MECH_TOOL + k as u32, label, *tool == m.tool, Width::FILL);
        }
    }
    rows.row(Height::Fixed(52.0))
        .toggle(item::MECH_PLAY, if m.running { "Pause" } else { "Play" }, m.running, Width::FILL)
        .button(item::MECH_RESET, "Reset all", Width::FILL)
        .toggle(item::MECH_SHAKE, "Shake", m.shaking, Width::FILL);
    let row = rows.row(Height::Fixed(52.0));
    for (k, (scene, label)) in MECH_SCENES.iter().enumerate() {
        row.toggle(item::MECH_SCENE + k as u32, label, *scene == m.scene, Width::FILL);
    }
    let row = rows.row(Height::Fixed(52.0));
    for (k, (speed, label)) in MECH_SPEEDS.iter().enumerate() {
        row.toggle(item::MECH_SPEED + k as u32, label, (m.speed - speed).abs() < 1e-3, Width::FILL);
    }
    if let Some(ev) = plate.map(|p| &p.ev).filter(|ev| ev.weaves.len() > 1) {
        let at = m.piece.min(ev.weaves.len() - 1);
        let name = ev.plate.parts.get(ev.weaves[at].part).map_or("", |p| p.name.as_str());
        rows.row(Height::Fixed(52.0))
            .button(item::MECH_PREV_PIECE, "<", Width::Fixed(48.0))
            .heading(name, Tone::Normal, Width::FILL)
            .button(item::MECH_NEXT_PIECE, ">", Width::Fixed(48.0));
    }
    if !m.status.is_empty() {
        rows.row(Height::Fixed(40.0)).text(&m.status, Tone::Meta, Width::FILL);
    }
    // the verdict a phrase a line (a row holds what fits)
    for phrase in m.verdict.split(" · ") {
        rows.row(Height::Fixed(30.0)).text(phrase, Tone::Meta, Width::FILL);
    }
    // the support pieces' answers, the worst first (a row each, as many as the wrist holds)
    let of = |light: MechLight| m.pieces.iter().filter(move |p| p.light == light);
    let worst_first = of(MechLight::Red).chain(of(MechLight::Amber)).chain(of(MechLight::Waiting)).chain(of(MechLight::Green));
    for p in worst_first.take(MECH_PIECE_ROWS) {
        let tone = match p.light {
            MechLight::Red => Tone::Error,
            MechLight::Amber => Tone::Warning,
            MechLight::Green => Tone::Good,
            MechLight::Waiting => Tone::Meta,
        };
        rows.row(Height::Fixed(30.0)).text(&format!("{}: {}", p.label, p.words), tone, Width::FILL);
    }
    if m.pieces.len() > MECH_PIECE_ROWS {
        rows.row(Height::Fixed(30.0)).text(&format!("and {} more", m.pieces.len() - MECH_PIECE_ROWS), Tone::Meta, Width::FILL);
    }
}

/// The Arm workspace's run: Run or Pause, Reset, the checks; its status and first figures.
fn arm_rows(rows: &mut makepad_xr_ui::Rows, state: Option<&fab::AppState>) {
    let Some(a) = state.map(|s| &s.arm) else { return };
    rows.row(Height::Fixed(52.0))
        .toggle(item::ARM_PLAY, if a.running { "Pause" } else { "Run" }, a.running, Width::FILL)
        .button(item::ARM_RESET, "Reset", Width::FILL)
        .button(item::ARM_CHECKS, "Run checks", Width::FILL);
    if !a.status.is_empty() {
        rows.row(Height::Fixed(40.0)).text(&a.status, Tone::Meta, Width::FILL);
    }
    for (name, value) in a.stats.iter().take(4) {
        rows.row(Height::Fixed(30.0)).text(format_args!("{name}: {value}"), Tone::Meta, Width::FILL);
    }
}

/// The Force study: the field the part is coloured by, the detail and engine, the deformed
/// shape, its status and first results. Fixtures and loads are picked on the desktop's view.
/// Design shows the Force render mode.
fn mode_is_force(state: Option<&fab::AppState>) -> bool {
    state.is_some_and(|s| s.force_shown())
}

fn force_rows(rows: &mut makepad_xr_ui::Rows, state: Option<&fab::AppState>) {
    let Some(f) = state.map(|s| &s.force) else { return };
    let title = if f.part.is_empty() { f.note.clone() } else { format!("{}: {} fixtures and loads", f.part, f.loads.len()) };
    rows.row(Height::Fixed(36.0)).text(&title, Tone::Meta, Width::FILL);
    let row = rows.row(Height::Fixed(52.0));
    for (k, x) in ForceField::ALL.iter().enumerate() {
        row.toggle(item::FORCE_FIELD + k as u32, x.label(), f.field == *x, Width::FILL);
    }
    rows.row(Height::Fixed(52.0)).toggle(item::FORCE_DEFORM, "Deformed", f.deform > 0.0, Width::FILL).button(item::FORCE_CLEAR, "Remove all loads", Width::FILL);
    if !f.status.is_empty() {
        rows.row(Height::Fixed(40.0)).text(&f.status, Tone::Meta, Width::FILL);
    }
    for (name, value) in f.stats.iter().take(4) {
        rows.row(Height::Fixed(30.0)).text(format_args!("{name}: {value}"), Tone::Meta, Width::FILL);
    }
    let floor = rows.row(Height::Fixed(40.0));
    match f.floor.running {
        true => floor.text(&f.floor.status, Tone::Meta, Width::FILL),
        false if f.floor.status.is_empty() => floor.button(item::FORCE_FLOOR, "Find the infill floor", Width::FILL),
        false => floor.text(&f.floor.status, Tone::Meta, Width::FILL).button(item::FORCE_FLOOR, "Again", Width::Fixed(96.0)),
    };
}

/// The value chip's text height on screen (degrees of the view, the same however near the
/// handle is): small UI text on a handheld, about a degree in a headset.
const CHIP_EM_DEG_HANDHELD: f32 = 0.8;
const CHIP_EM_DEG_WORN: f32 = 1.0;
/// The chip's height in its text's heights (its panel: 44 texels for a 22 font).
const CHIP_HEIGHT_EMS: f32 = 2.0;
/// The nearest the chip stands to the eyes (m): well beyond any near plane.
const CHIP_NEAR_M: f32 = 0.12;

/// Where the value chip went: the handle's last place on screen, its smoothed drag across the
/// screen and the way out from the handle the chip stands (`chip_card`).
#[derive(Clone, Copy, Default)]
struct ChipPlace {
    last: Option<Vec2f>,
    drift: Vec2f,
    dir: Vec2f,
}

/// The eyes as the scene has them: between the two, turned as the first, and the view's edges
/// as tangents (what both eyes see).
struct EyeFrame {
    pos: Vec3f,
    right: Vec3f,
    up: Vec3f,
    back: Vec3f,
    lo: Vec2f,
    hi: Vec2f,
}

impl EyeFrame {
    /// From the pass's cameras (view, projection per eye).
    fn of(cams: &[(Mat4f, Mat4f); 2]) -> EyeFrame {
        let axes = |m: &Mat4f| (vec3f(m.v[0], m.v[4], m.v[8]), vec3f(m.v[1], m.v[5], m.v[9]), vec3f(m.v[2], m.v[6], m.v[10]));
        let pos = |m: &Mat4f| {
            let (r, u, b) = axes(m);
            let s = r.length().max(1e-6);
            (r * m.v[12] + u * m.v[13] + b * m.v[14]) * (-1.0 / (s * s))
        };
        // x/-z = (ndc + p[2][0]) / p[0][0], at the edges ndc = -1 and 1 (y the same)
        let edges = |p: &Mat4f| (vec2f((p.v[8] - 1.0) / p.v[0], (p.v[9] - 1.0) / p.v[5]), vec2f((p.v[8] + 1.0) / p.v[0], (p.v[9] + 1.0) / p.v[5]));
        let (r, u, b) = axes(&cams[0].0);
        let ((lo0, hi0), (lo1, hi1)) = (edges(&cams[0].1), edges(&cams[1].1));
        EyeFrame {
            pos: (pos(&cams[0].0) + pos(&cams[1].0)) * 0.5,
            right: r.normalize(),
            up: u.normalize(),
            back: b.normalize(),
            lo: vec2f(lo0.x.max(lo1.x), lo0.y.max(lo1.y)),
            hi: vec2f(hi0.x.min(hi1.x), hi0.y.min(hi1.y)),
        }
    }
}

/// The value chip by a held handle at `at` (room = scene): its text `em_deg` high on screen
/// however near or far the handle is (never larger close up), beside the handle on the side
/// away from the view's centre and across the drag, about as deep as the handle and never
/// nearer than `CHIP_NEAR_M`, and kept on screen: at the nearest edge when the handle is off
/// it or too close. Facing the eyes, upright as they see it.
fn chip_card(at: Vec3f, eye: &EyeFrame, aspect: f32, em_deg: f32, place: &mut ChipPlace) -> Card {
    let chip_h = em_deg.to_radians().tan() * CHIP_HEIGHT_EMS;
    let chip = vec2f(chip_h * aspect, chip_h);
    let rel = at - eye.pos;
    let (x, y, depth) = (rel.dot(eye.right), rel.dot(eye.up), -rel.dot(eye.back));
    // the handle on the screen as tangents (behind the eyes: far off the edge it is toward)
    let s = if depth > CHIP_NEAR_M * 0.25 { vec2f(x / depth, y / depth) } else { vec2f(x, y).normalize() * 10.0 };
    // its drag across the screen, smoothed over a few frames
    if let Some(last) = place.last {
        place.drift = place.drift * 0.8 + (s - last) * 0.2;
    }
    place.last = Some(s);
    let out = if s.length() > 0.02 { s.normalize() } else { vec2f(1.0, 0.0) };
    let mut want = out;
    if place.drift.length() > 1e-4 {
        let across = vec2f(-place.drift.y, place.drift.x).normalize();
        let across = if across.x * out.x + across.y * out.y < 0.0 { -across } else { across };
        want = (across + out * 0.5).normalize();
    }
    // the side turns over a few frames, never jumps
    place.dir = if place.dir.length() < 0.5 { want } else { (place.dir * 0.85 + want * 0.15).normalize() };
    let dir = place.dir;
    // clear of the handle's ball, then its own half extent along the way out
    let d = (depth * 0.9).max(CHIP_NEAR_M);
    let ball = crate::xr_handles::ball_radius(rel.length(), crate::xr_handles::REACH_M) * 2.0 / depth.max(CHIP_NEAR_M);
    let reach = dir.x.abs() * chip.x * 0.5 + dir.y.abs() * chip.y * 0.5;
    let c = s + dir * (ball + chip_h * 0.5 + reach);
    let margin = chip_h * 0.5;
    let fit = |v: f32, lo: f32, hi: f32, half: f32| {
        let (a, b) = (lo + margin + half, hi - margin - half);
        if a <= b { v.clamp(a, b) } else { (lo + hi) * 0.5 }
    };
    let c = vec2f(fit(c.x, eye.lo.x, eye.hi.x, chip.x * 0.5), fit(c.y, eye.lo.y, eye.hi.y, chip.y * 0.5));
    Card { center: eye.pos + eye.right * (c.x * d) + eye.up * (c.y * d) - eye.back * d, right: eye.right, up: eye.up, normal: eye.back, size: chip * d }
}

/// The printer panel's job and status: what the printer reports, Pause and Stop while a job runs
/// (Stop asks first), following the job. Sending a print stays on the desktop.
fn monitor_rows(rows: &mut makepad_xr_ui::Rows, state: Option<&fab::AppState>) {
    let Some(p) = state.map(|s| &s.print) else { return };
    let Some(printer) = &p.printer else {
        rows.row(Height::Fixed(40.0)).text("No printer linked", Tone::Meta, Width::FILL);
        return;
    };
    let s = &printer.state;
    let (pill, status) = fab::print::panel::printer_status(printer);
    let tone = match pill as u8 {
        3 => Tone::Error,
        2 => Tone::Info,
        1 => Tone::Good,
        _ => Tone::Meta,
    };
    rows.row(Height::Fixed(40.0)).text(format_args!("{}: {status}", printer.name), tone, Width::FILL);
    let paused = s.gcode_state.as_deref() == Some("PAUSE");
    if s.busy() || paused || printer.job.is_some() {
        rows.row(Height::Fixed(40.0)).text(fab::print::panel::job_line(printer), Tone::Normal, Width::FILL);
        for line in fab::print::panel::job_detail(s) {
            rows.row(Height::Fixed(30.0)).text(line, Tone::Meta, Width::FILL);
        }
    }
    if fab_printer_link::printing_supported() && (s.busy() || paused) {
        if p.confirm_stop {
            rows.row(Height::Fixed(52.0))
                .text("Stop the print?", Tone::Warning, Width::FILL)
                .button(item::PRINTER_STOP_YES, "Stop", Width::Fixed(112.0))
                .button(item::PRINTER_STOP_NO, "Keep", Width::Fixed(112.0));
        } else {
            rows.row(Height::Fixed(52.0)).button(item::PRINTER_PAUSE, if paused { "Resume" } else { "Pause" }, Width::FILL).button(item::PRINTER_STOP, "Stop", Width::FILL);
        }
    }
    rows.row(Height::Fixed(52.0)).toggle(item::FOLLOW_PRINTER, "Follow the printer", p.follow.on, Width::FILL);
    // the temperatures, a line each (a row holds what fits)
    for line in fab::print::panel::printer_telemetry(printer).lines() {
        rows.row(Height::Fixed(30.0)).text(line, Tone::Meta, Width::FILL);
    }
}

impl LinkElements {
    /// Finds the links' elements in the scene shown, once per scene and plate.
    fn follow(&mut self, state: Option<&fab::AppState>, plate: &Plate, scene: usize) {
        let Some(state) = state else { return };
        let of = (scene, Arc::as_ptr(&plate.ev) as usize);
        if self.of == of {
            return;
        }
        self.of = of;
        let by_name: std::collections::HashMap<&str, ElementId> = state.scene.elements.iter().map(|e| (e.name.as_str(), e.id)).collect();
        self.parts = plate
            .links
            .iter()
            .enumerate()
            .map(|(i, l)| l.as_ref().map_or(Vec::new(), |l| (0..l.rest.len()).map(|k| by_name.get(crate::plate_scene::link_object(&plate.objects[i], k).as_str()).copied()).collect()))
            .collect();
    }
}

/// `FAB_XR_GRAB=<dir>`: pictures of the XR pass (both eyes side by side, over grey), written as
/// `<dir>/<name>.png`, and how much of each eye the picture covers.
#[derive(Default)]
struct XrGrabs {
    want: Option<String>,
    /// When `FAB_XR_PASS_STATS` last asked.
    stats_at: f64,
    pending: Vec<(String, Readback, Option<CardProbe>)>,
    /// The pictures read back, made on the pool one at a time.
    made: GrabQueue,
    /// `FAB_XR_GRAB_PRINT=<name>`: one picture of the print, as `<name>`, once its preview has
    /// drawn whole for `XrCheck::SETTLE` frames on end (for a still head); with the frames so
    /// far. Read at the first print frame.
    print: Option<Option<(String, u32)>>,
}

/// Where a card stood in the left eye when its picture was asked for, and the v (0 at its top)
/// of a cell drawn in the accent there: the picture says whether the card shows its panel upright.
#[derive(Clone, Copy)]
struct CardProbe {
    card: Card,
    cam: (Mat4f, Mat4f),
    eye_px: DVec2,
    v: f32,
}

impl CardProbe {
    fn project(&self, p: Vec3f) -> Option<DVec2> {
        let c = self.cam.1.transform_vec4(self.cam.0.transform_vec4(vec4(p.x, p.y, p.z, 1.0)));
        (c.w > 1e-6).then(|| dvec2((c.x / c.w * 0.5 + 0.5) as f64 * self.eye_px.x, (0.5 - c.y / c.w * 0.5) as f64 * self.eye_px.y))
    }

    /// The accent's v on the card in the picture (`w` x `h` BGRA, both eyes side by side).
    fn seen_v(&self, w: usize, h: usize, px: &[u8]) -> Option<f32> {
        let c = &self.card;
        let (hx, hy) = (c.right * (c.size.x * 0.5), c.up * (c.size.y * 0.5));
        let top = self.project(c.center + hy)?;
        let bottom = self.project(c.center - hy)?;
        let corners = [c.center + hx + hy, c.center - hx + hy, c.center + hx - hy, c.center - hx - hy].map(|p| self.project(p));
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for p in corners.iter().flatten() {
            (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
        }
        let ew = (w / 2).min(self.eye_px.x as usize);
        let axis = bottom - top;
        let dot = |a: DVec2, b: DVec2| a.x * b.x + a.y * b.y;
        let len2 = dot(axis, axis).max(1e-9);
        let (mut sum, mut n) = (0.0f64, 0usize);
        for y in (y0.max(0.0) as usize)..(y1.min(h as f64 - 1.0).max(0.0) as usize) {
            for x in (x0.max(0.0) as usize)..(x1.min(ew as f64 - 1.0).max(0.0) as usize) {
                let i = (y * w + x) * 4;
                let (r, g, b) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
                if b > 150 && b > r + 60 && g > r {
                    sum += dot(dvec2(x as f64, y as f64) - top, axis) / len2;
                    n += 1;
                }
            }
        }
        (n > 20).then(|| (sum / n as f64) as f32)
    }
}

impl XrGrabs {
    /// After a frame that drew the print: `whole`, all of its preview, every bead started.
    fn print_frame(&mut self, whole: bool) {
        let print = self.print.get_or_insert_with(|| std::env::var("FAB_XR_GRAB_PRINT").ok().map(|name| (name, 0)));
        let Some((name, frames)) = print else { return };
        *frames = if whole { *frames + 1 } else { 0 };
        if *frames == XrCheck::SETTLE {
            let name = name.clone();
            self.ask(&name);
        }
    }

    /// A picture is still read back or made.
    fn busy(&self) -> bool {
        !self.pending.is_empty() || self.made.busy()
    }

    fn ask(&mut self, name: &str) {
        match std::env::var_os("FAB_XR_GRAB") {
            Some(_) => self.want = Some(name.to_string()),
            None => log!("fab xr grab: {name}: FAB_XR_GRAB is not set"),
        }
    }

    /// After the XR pass: the pictures asked for, and the wrist menu's own (`<name>_menu`);
    /// `probe`: the menu's card in the picture, checked the right way up.
    fn step(&mut self, cx: &mut Cx, color: &Texture, menu: Option<&Texture>, probe: Option<CardProbe>) {
        let mut k = 0;
        while k < self.pending.len() {
            let Some(res) = self.pending[k].1.lock().unwrap().take() else {
                k += 1;
                continue;
            };
            let (name, _, probe) = self.pending.remove(k);
            match res {
                Ok((w, h, px)) => {
                    let dir = std::env::var("FAB_XR_GRAB").unwrap_or_default();
                    // on the pool: the picture's encode is no headset frame's
                    self.made.push(name.clone(), move || {
                        let px = xr_display_rgba(w, h, px);
                        if let Some(p) = probe {
                            match p.seen_v(w, h, &px) {
                                Some(seen) => log!("fab xr card check: {name}: the lit cells at v {:.2} on the card, drawn at v {seen:.2}, card {:.3} x {:.3} m", p.v, p.card.size.x, p.card.size.y),
                                None => log!("fab xr card check: {name}: no lit row seen on the card"),
                            }
                        }
                        write_grab(&dir, &name, w, h, &px)
                    });
                }
                Err(e) => log!("fab xr grab: {name}: {e}"),
            }
        }
        if self.made.busy() {
            self.made.turn(&cx.task_pool());
        }
        if let Some(name) = self.want.take() {
            // the stats are the eyes' numbers alone
            let menu = if name == "stats" { None } else { menu };
            let menu_name = format!("{name}_menu");
            for (name, texture, probe) in [(name, Some(color), probe), (menu_name, menu, None)] {
                let Some(texture) = texture else { continue };
                let rb: Readback = Arc::default();
                let out = rb.clone();
                match cx.read_render_texture(texture, move |r| *out.lock().unwrap() = Some(r.map_err(|e| format!("{e:?}")))) {
                    Ok(()) => self.pending.push((name, rb, probe)),
                    Err(e) => log!("fab xr grab: {name}: {e:?}"),
                }
            }
        }
    }
}

/// Pictures to make from the XR pass's read backs, made on the pool one at a time in the order
/// they came: a picture made more slowly than they are asked for leaves the rest waiting, none
/// dropped and never more than one worker on them.
#[derive(Default)]
struct GrabQueue {
    waiting: VecDeque<(String, Box<dyn FnOnce() + Send>)>,
    making: Option<(String, TaskHandle<()>)>,
    /// The pictures waiting behind the one being made were logged, once until they are all made.
    told: bool,
}

impl GrabQueue {
    fn push(&mut self, name: String, make: impl FnOnce() + Send + 'static) {
        self.waiting.push_back((name, Box::new(make)));
    }

    fn busy(&self) -> bool {
        self.making.is_some() || !self.waiting.is_empty()
    }

    /// Once a frame: the next picture to the pool once the one being made is done.
    fn turn(&mut self, pool: &TaskPool) {
        if let Some((name, task)) = &mut self.making {
            if !task.is_finished() {
                if !self.told && !self.waiting.is_empty() {
                    self.told = true;
                    log!("fab xr grab: pictures wait for {name} to be made, {} so far", self.waiting.len());
                }
                return;
            }
            match task.try_take() {
                Some(Ok(())) => {}
                Some(Err(e)) => log!("fab xr grab: {name}: {e:?}"),
                None => return,
            }
            self.making = None;
        }
        let Some((name, make)) = self.waiting.pop_front() else {
            self.told = false;
            return;
        };
        match pool.submit_named(Class::Background, "fab xr grab", make) {
            Ok(task) => self.making = Some((name, task)),
            Err(e) => log!("fab xr grab: {name}: {e}"),
        }
    }
}

/// Every half (by its bits) as display light: the linear value (at least 0) encoded at gamma 2.2.
fn half_encoded() -> &'static [f32] {
    use makepad_widgets::makepad_platform::makepad_math::f16_bits_to_f32;
    static TABLE: OnceLock<Box<[f32]>> = OnceLock::new();
    TABLE.get_or_init(|| (0..=u16::MAX).map(|h| f16_bits_to_f32(h).max(0.0).powf(1.0 / 2.2)).collect())
}

/// The XR pass's colour as read back made display RGBA8 (premultiplied), as the stream encodes
/// it: the eye target's RGBA16F linear light (`DrawPass::set_linear_light`) unpremultiplied,
/// encoded (gamma 2.2) and premultiplied again; 8-bit bytes (a picture of another pass, read
/// back as RGBA) as they are. An opaque texel's channels encode by their own bits, a partly
/// covered one's by the bits of their unpremultiplied values as halves (within a byte of
/// encoding the values themselves); values past the halves' range, or over an alpha above 1,
/// encode as they are.
fn xr_display_rgba(w: usize, h: usize, px: makepad_widgets::HostBuf) -> makepad_widgets::HostBuf {
    if px.len() != w * h * 8 {
        return px;
    }
    use makepad_widgets::makepad_platform::makepad_math::{f16_bits_to_f32, f32_to_f16_bits};
    const ONE: u16 = 0x3c00;
    let encoded = half_encoded();
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut out = vec![0u8; w * h * 4];
    for (texel, o) in px.chunks_exact(8).zip(out.chunks_exact_mut(4)) {
        let c = [0, 1, 2, 3].map(|k| u16::from_le_bytes([texel[2 * k], texel[2 * k + 1]]));
        let a = f16_bits_to_f32(c[3]);
        let partly = |h: u16| {
            let v = f16_bits_to_f32(h);
            if a > 0.0 && a < 1.0 && v < a * 65504.0 {
                encoded[f32_to_f16_bits(v / a) as usize] * a
            } else if a > 0.0 {
                (v / a).max(0.0).powf(1.0 / 2.2) * a
            } else {
                v
            }
        };
        let rgb = match c[3] {
            ONE => [0, 1, 2].map(|k| encoded[c[k] as usize]),
            _ => [0, 1, 2].map(|k| partly(c[k])),
        };
        o.copy_from_slice(&[byte(rgb[0]), byte(rgb[1]), byte(rgb[2]), byte(a)]);
    }
    out.into()
}

/// Whether pixel `i` of `texels` in the XR pass's colour as read back holds anything (alpha
/// above 0): RGBA16F halves (the linear-light eye target) or 8-bit RGBA bytes.
fn xr_covered(px: &[u8], texels: usize, i: usize) -> bool {
    if px.len() == texels * 8 {
        let a = u16::from_le_bytes([px[i * 8 + 6], px[i * 8 + 7]]);
        a != 0 && a & 0x8000 == 0
    } else {
        px[i * 4 + 3] > 0
    }
}

/// At half size past an image's limit (two eyes' width).
fn write_grab(dir: &str, name: &str, w: usize, h: usize, px: &[u8]) {
    if name == "stats" {
        // the left eye's solid pixels and how many of them are clearly coloured
        let (mut solid, mut coloured, mut half, mut between) = (0usize, 0usize, 0usize, 0usize);
        let mut half_sum = [0u64; 4];
        for y in 0..h {
            for x in 0..w / 2 {
                let i = (y * w + x) * 4;
                if px[i + 3] > 100 && px[i + 3] < 160 {
                    half += 1;
                    for k in 0..4 {
                        half_sum[k] += px[i + k] as u64;
                    }
                }
                between += (px[i + 3] >= 160 && px[i + 3] <= 200) as usize;
                if px[i + 3] > 200 {
                    let (hi, lo) = (px[i].max(px[i + 1]).max(px[i + 2]), px[i].min(px[i + 1]).min(px[i + 2]));
                    solid += 1;
                    coloured += ((hi - lo) as f64 > 0.25 * hi as f64) as usize;
                }
            }
        }
        return log!("fab xr pass: left eye {solid} solid px, {coloured} of them coloured, {half} half covered, {between} between, half mean rgba {:?}", half_sum.map(|v| v / half.max(1) as u64));
    }
    let limits = makepad_model::Limits::default();
    let k = if w > 2048 { 2 } else { 1 };
    let (iw, ih) = (w / k, h / k);
    let mut image = match makepad_model::RgbaImage::new(iw as u32, ih as u32, [64, 64, 64, 255], &limits) {
        Ok(image) => image,
        Err(e) => return log!("fab xr grab: {name}: {e:?}"),
    };
    let mut covered = [0usize; 2];
    for y in 0..ih {
        for x in 0..iw {
            let i = (y * k * w + x * k) * 4;
            let a = px[i + 3] as u32;
            if a > 0 {
                covered[(x * 2 / iw).min(1)] += 1;
            }
            let over = |c: u8| (c as u32 + 64 * (255 - a) / 255).min(255) as u8;
            let o = (y * iw + x) * 4;
            image.pixels[o..o + 4].copy_from_slice(&[over(px[i]), over(px[i + 1]), over(px[i + 2]), 255]);
        }
    }
    let _ = std::fs::create_dir_all(dir);
    let path = std::path::Path::new(dir).join(format!("{name}.png"));
    match image.to_png(&limits).map_err(|e| format!("{e:?}")).and_then(|png| std::fs::write(&path, png).map_err(|e| e.to_string())) {
        Ok(()) => log!("fab xr grab: {name}: {iw}x{ih}, covered left {:.2}% right {:.2}%", covered[0] as f64 * 200.0 / (iw * ih) as f64, covered[1] as f64 * 200.0 / (iw * ih) as f64),
        Err(e) => log!("fab xr grab: {name}: {e}"),
    }
}

/// One step of [`MechSynth`]'s script.
#[derive(Clone, Copy)]
enum SynthStep {
    /// Press the wrist menu's cell (the menu open), by aiming at it and pulling the trigger.
    Press(u32),
    /// Y: the menu opens or closes.
    Menu,
    /// Stay this long.
    Wait(f32),
    /// Until the sim is up.
    Ready,
    /// Log where things are, and a picture.
    Mark(&'static str),
    /// Take the sim's last body with the right grip, swing the hand fast and let go; the link's
    /// speed after against the hand's.
    Throw,
    /// Take the sim's last body, lift it and carry it across and back at a steady speed, then let
    /// go: how far the held point (as drawn: the sim's) trails the hand.
    Carry,
    /// Stay this long, the frames' intervals taken: their spread and the gaps.
    Pace(f32),
    /// The left hand takes a Supports sim's part, lifts and turns it; the right pulls a piece out
    /// of it and lets go; then the left lets go. What came of it, in the log.
    TwoHands,
}

/// The carry's speed across (room m/s).
const CARRY_MPS: f32 = 0.3;

/// A synthetic hand is where it was put once its tip (as the interaction filtered it) is this near (m).
const SYNTH_TIP_THERE_M: f32 = 0.0005;

/// Where a synthetic hand waiting to get there holds the script: this short of its close (s).
const SYNTH_TIP_WAIT_S: f32 = 0.001;

/// The frames a synthetic hand holds still after its take, however late they come: a view a step
/// behind shows where the sim had the part at the take on the first of them.
const SYNTH_TAKE_FRAMES: usize = 2;

/// `FAB_XR_MECH_SYNTH=1`: every workspace chosen from the wrist menu's rows with the right hand's
/// aim and trigger, as a person does, on synthetic controllers: Mechanics, Play, a carry, a
/// throw, the scenes, Reset, then a stretch of frames in each workspace for its pacing.
/// `FAB_XR_MECH_SYNTH=pace`: Mechanics alone (automatic start, the carry, the throw, then the pacing).
/// `FAB_XR_MECH_PACE_S` (default 10): how long Mechanics' pacing runs. `fab xr mech synth:` lines
/// say where it is and how far the bodies moved; `FAB_XR_GRAB` keeps pictures.
/// `FAB_XR_MECH_SYNTH=supports`: Mechanics' Supports row, then one hand on the part and one on a piece.
/// `FAB_XR_DRAG_SYNTH=1` extends the carry's held portion to 40 s for desktop-freeze counters.
#[derive(Default)]
struct MechSynth {
    /// Mechanics alone (`FAB_XR_MECH_SYNTH=pace`).
    pace_only: bool,
    /// The Supports row and two hands (`FAB_XR_MECH_SYNTH=supports`).
    supports: bool,
    /// [`SynthStep::TwoHands`]: where the part was and where the hands started (room).
    two_hands: Option<TwoHandsFrom>,
    /// The carry's lag samples (mm, along its motion) and its sideways error (mm).
    lags: Vec<f32>,
    side: f32,
    /// The pacing's frame intervals (s), and the last frame's time.
    dts: Vec<f32>,
    last: Option<f64>,
    /// The throw: the held point's last positions (time, room) after the release.
    flight: Vec<(f64, Vec3f)>,
    at: usize,
    clock: SynthClock,
    /// When the step began ([`SynthClock`] time).
    from: Option<f64>,
    /// The body the hand went for: where its point was taken (room).
    throw_from: Option<Vec3f>,
    menu_open: bool,
    /// The step's press had its frames.
    pressed: bool,
    done: bool,
}

/// Where [`SynthStep::TwoHands`] started.
#[derive(Clone, Copy)]
struct TwoHandsFrom {
    /// When it began, on the sim's clock.
    began: f64,
    part: ElementId,
    piece: ElementId,
    /// The part's pose then, and once the hand has it, where the sim had it at the take.
    pose: [[f32; 4]; 3],
    /// The left hand's point and, once it goes for the piece, the right's (room).
    left: Vec3f,
    right: Option<Vec3f>,
    /// The piece in the part's rest frame (printer metres).
    piece_in_part: Vec3f,
    /// The left hand's point on the bed (printer mm) and the view's scale then.
    start: [f64; 3],
    scale: f32,
    /// When the part first moved off its start (on the sim's clock), how far up, and the hand's lift then (mm).
    moved: Option<(f32, f32, f32)>,
    /// The log's line, with what the part did while held, and the hand's rise on the bed then.
    said: Option<(f32, f32, f32, f64)>,
    /// The hands' closes done (the left's, then the right's), and the sim time the script was put
    /// back by for closes that came late (s).
    closed: usize,
    late: f32,
    /// Taken, before the hand moves: the highest the part has been shown (printer metres), and the
    /// most it has come back down from there (mm). A view a step behind still shows it rising while
    /// the scene lifts it; a hand that took it there never puts it back down.
    top: f32,
    back: f32,
    /// The frame after the left hand's take, on the script's clock (s), and the hand's lift then
    /// (mm): a stall there has the hand on its way before the sim hears from it again.
    after_take: Option<(f32, f32)>,
    /// The frames since the left hand's take.
    since_take: usize,
}

impl MechSynth {
    fn script(pace_only: bool, supports: bool) -> Vec<SynthStep> {
        use SynthStep::*;
        let ws = |w: Workspace| item::WORKSPACE + w.index() as u32;
        let scene = |k: u32| item::MECH_SCENE + k;
        let render_mode = |m: fab::api::RenderMode| item::RENDER_MODE + fab::api::RenderMode::ALL.iter().position(|x| *x == m).unwrap_or(0) as u32;
        let pace = std::env::var("FAB_XR_MECH_PACE_S").ok().and_then(|v| v.parse().ok()).unwrap_or(10.0);
        if supports {
            return vec![
                Wait(1.0), Menu, Wait(0.3),
                Press(item::REAL_SIZE), Wait(0.5),
                Press(ws(Workspace::Mechanics)), Ready,
                Press(scene(3)), Wait(0.2), Ready, Menu, Wait(1.5), Mark("supports_rest"),
                TwoHands, Mark("supports_hands"),
            ];
        }
        if pace_only {
            return vec![
                Wait(1.0), Menu, Wait(0.3),
                Press(item::REAL_SIZE), Wait(0.5),
                Press(ws(Workspace::Mechanics)), Ready, Wait(0.5), Mark("mechanics_rest"),
                Menu, Wait(3.0), Mark("mechanics_drop"),
                Carry, Wait(0.5), Mark("mechanics_carried"),
                Throw, Wait(1.0), Mark("mechanics_thrown"),
                Pace(pace), Mark("mechanics_pace"),
            ];
        }
        vec![
            Wait(1.0), Menu, Wait(0.3),
            Press(item::REAL_SIZE), Wait(0.5),
            Press(ws(Workspace::Mechanics)), Ready, Wait(0.5), Mark("mechanics_rest"),
            Wait(3.0), Mark("mechanics_drop"),
            Carry, Wait(0.5), Mark("mechanics_carried"),
            Throw, Wait(1.0), Mark("mechanics_thrown"),
            Press(scene(1)), Wait(0.2), Ready, Wait(3.0), Mark("mechanics_drape"),
            Press(scene(2)), Wait(0.2), Ready, Wait(3.0), Mark("mechanics_hang"),
            Press(item::MECH_RESET), Wait(0.5), Mark("mechanics_reset"),
            Press(scene(0)), Wait(0.2), Ready, Menu, Pace(pace), Mark("mechanics_pace"), Menu, Wait(0.3),
            Press(ws(Workspace::Design)), Wait(6.0), Mark(Workspace::Design.label()),
            Press(render_mode(fab::api::RenderMode::Slice)), Wait(10.0), Mark("slice"),
            Press(render_mode(fab::api::RenderMode::Field)), Wait(1.0),
            Press(ws(Workspace::Monitor)), Wait(6.0), Mark(Workspace::Monitor.label()),
            Press(ws(Workspace::Design)), Wait(1.0), Menu,
        ]
    }

    /// One headset frame. `held`: where the sim has the point the hands last took (room). `tips`:
    /// the hands' tips as the interaction had them last frame (filtered, room).
    #[allow(clippy::too_many_arguments)]
    fn step(&mut self, s: &mut XrState, now: f64, view: &XrNavView, state: &fab::AppState, menu: &XrWristMenu, grabs: &mut XrGrabs, held: Option<Vec3f>, tips: [Option<Pose>; 2]) {
        let ident = Quat { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };
        let (l, r) = (&mut s.left_controller, &mut s.right_controller);
        for c in [&mut *l, &mut *r] {
            c.buttons = XrController::ACTIVE;
            c.grip = 0.0;
            c.trigger = 0.0;
            c.stick = Vec2f::default();
            c.grip_pose.orientation = ident;
            c.aim_pose.orientation = ident;
        }
        l.grip_pose.position = vec3f(-0.2, 1.0, -0.35);
        l.aim_pose = l.grip_pose;
        let rest = vec3f(0.25, 1.0, -0.3);
        r.aim_pose = r.grip_pose;
        input::place_tip(r, rest);
        if self.done {
            return;
        }
        let script = Self::script(self.pace_only, self.supports);
        let Some(step) = script.get(self.at).copied() else {
            self.done = true;
            log!("fab xr mech synth: done");
            return;
        };
        let at = self.clock.at(now);
        let t = (at - *self.from.get_or_insert(at)) as f32;
        let mut next = false;
        match step {
            SynthStep::Wait(secs) => next = t >= secs,
            SynthStep::Ready => next = state.mech.ready,
            SynthStep::Menu => {
                // pressed for a few frames, released for a few
                if t < 0.05 {
                    l.buttons |= XrController::CLICK_Y;
                }
                if t >= 0.2 {
                    self.menu_open = !self.menu_open;
                    next = true;
                }
            }
            SynthStep::Press(id) => {
                if let (Some(card), Some(uv)) = (menu.card(), menu.panel.cell_uv(id)) {
                    let on = card.center + card.right * (uv.x - 0.5) * card.size.x + card.up * (0.5 - uv.y) * card.size.y;
                    r.aim_pose.position = on + card.normal * 0.15;
                    r.aim_pose.orientation = aim_along(card.normal * -1.0);
                    // At least one frame of it, however late the frames come (a person holds
                    // a trigger for longer than a stall).
                    if t >= 0.1 && (t < 0.2 || !self.pressed) {
                        r.trigger = 1.0;
                        self.pressed = true;
                    }
                }
                if t >= 0.35 && r.trigger == 0.0 {
                    self.pressed = false;
                    next = true;
                }
            }
            SynthStep::Mark(name) => {
                let (bodies, moved) = bodies_moved(state);
                log!("fab xr mech synth: {name}: slice={} workspace {} ready={} running={} scene={:?} bodies={bodies} moved max {moved:.1} mm verdict {:?} status {:?}", state.print.play.active, state.ui.kind().label(), state.mech.ready, state.mech.running, state.mech.scene, state.mech.verdict, state.mech.status);
                grabs.ask(name);
                next = true;
            }
            SynthStep::Throw => {
                // over the body, in, a fast swing up and across, out at full speed
                let from = *self.throw_from.get_or_insert_with(|| last_body(state).map_or(rest, |p| room_from_fab(view, p)));
                let k = ((t - 0.3) / 0.3).clamp(0.0, 1.0);
                r.aim_pose = r.grip_pose;
                input::place_tip(r, from + vec3f(0.15 * k, 0.2 * k, 0.0));
                if (0.1..0.6).contains(&t) {
                    r.grip = 1.0;
                }
                // the link in flight just after the release
                if let (true, Some(p)) = ((0.62..0.72).contains(&t), held) {
                    self.flight.push((now, p));
                }
                if t >= 0.8 {
                    let hand = vec3f(0.15, 0.2, 0.0).length() / 0.3;
                    let link = match (self.flight.first(), self.flight.last()) {
                        (Some(a), Some(b)) if b.0 - a.0 > 0.02 => (b.1 - a.1).length() / (b.0 - a.0) as f32,
                        _ => 0.0,
                    };
                    log!("fab xr mech synth: throw: hand {hand:.2} m/s at the release, the link {link:.2} m/s after it ({} frames)", self.flight.len());
                    self.flight.clear();
                    self.throw_from = None;
                    next = true;
                }
            }
            SynthStep::Carry => {
                // onto the body, lift it 8 cm, across at a steady speed and back, still, let go
                let release = if std::env::var("FAB_XR_DRAG_SYNTH").as_deref() == Ok("1") { 40.0 } else { 1.95 };
                let from = *self.throw_from.get_or_insert_with(|| last_body(state).map_or(rest, |p| room_from_fab(view, p)));
                let up = vec3f(0.0, 0.08 * ((t - 0.15) / 0.4).clamp(0.0, 1.0), 0.0);
                let swing = 0.6 * CARRY_MPS;
                let x = if t < 0.55 {
                    0.0
                } else if t < 1.15 {
                    CARRY_MPS * (t - 0.55)
                } else {
                    (swing - CARRY_MPS * (t - 1.15)).max(0.0)
                };
                let hand = from + up + vec3f(x, 0.0, 0.0);
                r.aim_pose = r.grip_pose;
                input::place_tip(r, hand);
                if (0.1..release).contains(&t) {
                    r.grip = 1.0;
                }
                // steady across (0.1 s after each start), both ways
                let dir = if (0.65..1.15).contains(&t) {
                    Some(1.0)
                } else if (1.25..1.75).contains(&t) {
                    Some(-1.0)
                } else {
                    None
                };
                if let (Some(d), Some(p)) = (dir, held) {
                    let lag = hand - p;
                    self.lags.push(lag.x * d * 1000.0);
                    self.side = self.side.max(lag.z.abs() * 1000.0);
                }
                if t >= release + 0.1 {
                    let mut lags = std::mem::take(&mut self.lags);
                    lags.sort_by(f32::total_cmp);
                    let at = |q: f32| lags.get(((lags.len().max(1) - 1) as f32 * q) as usize).copied().unwrap_or(f32::NAN);
                    let ms = |mm: f32| mm / CARRY_MPS;
                    log!(
                        "fab xr mech synth: carry: {} frames at {CARRY_MPS} m/s, the held point (as drawn) trails the hand p50 {:.2} p99 {:.2} max {:.2} mm = latency p50 {:.1} p99 {:.1} ms, sideways max {:.2} mm",
                        lags.len(), at(0.5), at(0.99), at(1.0), ms(at(0.5)), ms(at(0.99)), self.side
                    );
                    self.side = 0.0;
                    self.throw_from = None;
                    next = true;
                }
            }
            SynthStep::TwoHands => {
                let centre = |e: ElementId| body_centre(state, e);
                let carried = state.mech.carried.first().copied();
                let piece = state.scene_state.moves.as_ref().and_then(|m| m.keys().filter(|e| Some(**e) != carried).min_by_key(|e| e.index()).copied());
                if self.two_hands.is_none() {
                    if let (Some(part), Some(piece), Some(at), Some(from), Some(pose)) = (carried, piece, carried.and_then(centre), piece.and_then(centre), carried.and_then(|e| state.scene_state.pose(e))) {
                        let piece_in_part = crate::xr_mech::to_rest(&pose, from);
                        let left = room_from_fab(view, at);
                        self.two_hands = Some(TwoHandsFrom { began: state.mech.time, part, piece, pose, left, right: None, piece_in_part, start: xr_edit::printer_from_room(view, left), scale: view.scale, moved: None, said: None, closed: 0, late: 0.0, top: f32::MIN, back: 0.0, after_take: None, since_take: 0 });
                    }
                }
                let Some(mut go) = self.two_hands else {
                    log!("fab xr mech synth: hands: no part and piece in the sim: {} carried", state.mech.carried.len());
                    self.at += 1;
                    self.from = None;
                    return;
                };
                // On the sim's own clock: a loaded box runs it in slow motion, and the hands go along.
                // A hand closes where it was put and only then moves, as a person's does: the sim runs on
                // through a stall of the frames, and a hand that went by its clock alone would close the
                // first frame after, already on its way (off the part: the view's to move then). A close
                // that comes late puts the rest of the script back by as much. A hand put somewhere glides
                // there through the interaction's filter, so it closes once its tip (as filtered) is
                // there: one closed on the way takes the part where the tip was, and the rest of the
                // glide carries the part along with it.
                let mut t = (state.mech.time - go.began) as f32 - go.late;
                let took = go.closed > 0;
                if let Some(&close) = [0.1f32, 0.9].get(go.closed).filter(|c| t >= **c) {
                    go.late += t - close;
                    let (hand, put) = if go.closed == 0 { (0, Some(go.left)) } else { (1, go.right) };
                    if put.zip(tips[hand]).is_some_and(|(put, tip)| (tip.position - put).length() <= SYNTH_TIP_THERE_M) {
                        go.closed += 1;
                        t = close;
                    } else {
                        t = close - SYNTH_TIP_WAIT_S;
                    }
                }
                // The hold ends at 0.2 s on the sim's clock and after `SYNTH_TAKE_FRAMES` frames since
                // the take. Through a stall of the frames the sim runs on past 0.2 s, and the part's
                // start would be read where the view showed it before the take (one bucket's frame
                // after the take came at 0.217 s: the part rose 88 mm with the hand's 80). The rest of
                // the script is put back by as much.
                let came = t;
                if took {
                    go.since_take += 1;
                    if go.since_take <= SYNTH_TAKE_FRAMES && t >= 0.2 - SYNTH_TIP_WAIT_S {
                        go.late += t - (0.2 - SYNTH_TIP_WAIT_S);
                        t = 0.2 - SYNTH_TIP_WAIT_S;
                    }
                }
                // the left: on the part, takes it at 0.1 s, 8 cm up and a sixth of a turn by 0.7 s, lets go at 2.6 s.
                // It holds still until 0.2 s, by when the view shows where the sim had the part when the
                // hand took it (the scene may be lifting it): what it does after is measured from there.
                let now_pose = state.scene_state.pose(go.part);
                if let (true, Some(pose), Some(p)) = (t < 0.2, now_pose, centre(go.piece)) {
                    if go.closed > 0 {
                        go.top = go.top.max(pose[2][3]);
                        go.back = go.back.max((go.top - pose[2][3]) * 1000.0);
                    }
                    go.pose = pose;
                    go.piece_in_part = crate::xr_mech::to_rest(&pose, p);
                }
                let k = ((t - 0.2) / 0.5).clamp(0.0, 1.0);
                if took && go.after_take.is_none() {
                    go.after_take = Some((came, 80.0 * k));
                }
                if let (None, false, Some(pose)) = (go.moved, t < 0.2, now_pose) {
                    let dz = (pose[2][3] - go.pose[2][3]) * 1000.0;
                    if dz.abs() > 0.5 {
                        go.moved = Some((t, dz, 80.0 * k));
                    }
                }
                let half = k * std::f32::consts::PI / 12.0;
                l.grip_pose.orientation = Quat { x: 0.0, y: half.sin(), z: 0.0, w: half.cos() };
                l.aim_pose = l.grip_pose;
                input::place_tip(l, go.left + vec3f(0.0, 0.08 * k, 0.0));
                if (0.1..2.6).contains(&t) {
                    l.grip = 1.0;
                }
                // the right: on the piece at 0.8 s, takes it at 0.9 s, 15 cm to the side by 1.7 s, lets go at 2.1 s
                if t >= 0.8 {
                    let from = *go.right.get_or_insert_with(|| centre(go.piece).map_or(rest, |p| room_from_fab(view, p)));
                    input::place_tip(r, from + vec3f(0.15 * ((t - 1.0) / 0.7).clamp(0.0, 1.0), 0.0, 0.0));
                    if (0.9..2.1).contains(&t) {
                        r.grip = 1.0;
                    }
                }
                if t >= 2.5 && go.said.is_none() {
                    if let (Some(pose), Some(p)) = (now_pose, centre(go.piece)) {
                        let d = crate::xr_mech::to_rest(&pose, p) - go.piece_in_part;
                        let hand = xr_edit::printer_from_room(view, go.left + vec3f(0.0, 0.08, 0.0))[2] - go.start[2];
                        go.said = Some(((pose[2][3] - go.pose[2][3]) * 1000.0, pose[0][0].clamp(-1.0, 1.0).acos().to_degrees(), d.length() * 1000.0, hand));
                    }
                }
                if t >= 3.2 {
                    let (rose, turned, away, hand) = go.said.unwrap_or_default();
                    let stays = now_pose.map_or(f32::NAN, |p| (p[2][3] - go.pose[2][3]) * 1000.0);
                    log!("fab xr mech synth: hands: part rose {rose:.1} mm turned {turned:.1} deg, piece {away:.1} mm from where it was in the part, part let go at {stays:.1} mm, went back {:.1} mm while the hand held still after the take; the hand rose {hand:.1} mm on the bed, view scale {:.3} (at the start {:.3}); the part first moved {:?} (s, mm up, the hand's mm up); the frame after the take {:?} (s, the hand's mm up)", go.back, view.scale, go.scale, go.moved, go.after_take);
                    next = true;
                }
                self.two_hands = Some(go);
            }
            SynthStep::Pace(secs) => {
                if let Some(l) = self.last {
                    self.dts.push((now - l) as f32);
                }
                self.last = Some(now);
                if t >= secs {
                    let mut d = std::mem::take(&mut self.dts);
                    d.sort_by(f32::total_cmp);
                    let at = |q: f32| d.get(((d.len().max(1) - 1) as f32 * q) as usize).copied().unwrap_or(f32::NAN) * 1000.0;
                    let p50 = at(0.5);
                    let gaps: Vec<String> = d.iter().rev().take_while(|x| **x * 1000.0 > (2.0 * p50).max(20.0)).map(|x| format!("{:.0}", x * 1000.0)).collect();
                    let (bodies, _) = bodies_moved(state);
                    log!(
                        "fab xr mech synth: pace: {secs:.0} s, {bodies} links, {} frames: interval p50 {p50:.1} p95 {:.1} p99 {:.1} max {:.1} ms, gaps {} [{}] ms",
                        d.len(), at(0.95), at(0.99), at(1.0), gaps.len(), gaps.join(" ")
                    );
                    self.last = None;
                    next = true;
                }
            }
        }
        if next {
            self.at += 1;
            self.from = None;
        }
    }
}

/// How many bodies the sim moves and how far the farthest is from where it rests (mm).
fn bodies_moved(state: &fab::AppState) -> (usize, f64) {
    let Some(moves) = state.scene_state.moves.as_ref() else { return (0, 0.0) };
    let mut far = 0.0f64;
    for (id, a) in moves.iter() {
        let Some(e) = state.scene.elements.get(id.index()) else { continue };
        let c = (e.bounds.min + e.bounds.max) * 0.5;
        let now = vec3f(a[0][0] * c.x + a[0][1] * c.y + a[0][2] * c.z + a[0][3], a[1][0] * c.x + a[1][1] * c.y + a[1][2] * c.z + a[1][3], a[2][0] * c.x + a[2][1] * c.y + a[2][2] * c.z + a[2][3]);
        far = far.max(((now - c).length() * 1000.0) as f64);
    }
    (moves.len(), far)
}

/// The sim's last body's centre now, Fab's scene frame.
fn last_body(state: &fab::AppState) -> Option<Vec3f> {
    let moves = state.scene_state.moves.as_ref()?;
    let (id, _) = moves.iter().max_by_key(|(id, _)| id.index())?;
    body_centre(state, *id)
}

/// A sim body's centre now, Fab's scene frame.
fn body_centre(state: &fab::AppState, id: ElementId) -> Option<Vec3f> {
    let a = state.scene_state.pose(id)?;
    let e = state.scene.elements.get(id.index())?;
    let c = (e.bounds.min + e.bounds.max) * 0.5;
    Some(vec3f(a[0][0] * c.x + a[0][1] * c.y + a[0][2] * c.z + a[0][3], a[1][0] * c.x + a[1][1] * c.y + a[1][2] * c.z + a[1][3], a[2][0] * c.x + a[2][1] * c.y + a[2][2] * c.z + a[2][3]))
}

fn room_from_fab(view: &XrNavView, p: Vec3f) -> Vec3f {
    xr_edit::room_from_printer(view, [p.x as f64 * 1000.0, p.y as f64 * 1000.0, p.z as f64 * 1000.0])
}

/// The orientation whose aim (-Z) points along `dir`.
fn aim_along(dir: Vec3f) -> Quat {
    let (a, b) = (vec3f(0.0, 0.0, -1.0), dir.normalize());
    let c = Vec3f::cross(a, b);
    let mut q = Quat { x: c.x, y: c.y, z: c.z, w: 1.0 + a.dot(b) };
    q.normalized()
}

/// A synthetic script's clock, in seconds of its frames: each frame takes it on by the time since
/// the frame before, at most [`SYNTH_STEP`]. A main thread that stalls (a loaded box, cold code)
/// slows the script down, every press, hold and release of it on a frame, where on the wall's
/// clock the frame after the stall came past them all and the script went on without them.
#[derive(Default)]
struct SynthClock {
    t: f64,
    last: Option<f64>,
}

/// The most a synthetic script moves on in one frame: its briefest phase (a press held 0.05 s)
/// gets a frame.
const SYNTH_STEP: f64 = 0.05;

impl SynthClock {
    /// The script's time at this frame (`now`, the app's clock); its first frame is 0.
    fn at(&mut self, now: f64) -> f64 {
        if let Some(last) = self.last {
            self.t += (now - last).clamp(0.0, SYNTH_STEP);
        }
        self.last = Some(now);
        self.t
    }

    /// Its first frame came.
    fn started(&self) -> bool {
        self.last.is_some()
    }

    /// The script goes on from `t`.
    fn set(&mut self, t: f64) {
        self.t = t;
    }
}

/// `FAB_XR_HANDLE_SYNTH=1`: a shape-handle drag on synthetic controllers, seconds from the
/// first frame its part has parameters. The right controller, 8 cm above the part and aiming
/// down, clicks its trigger at 1.2 s (the part is selected: its handles show); at 2 s the hand
/// goes to the grip of its first ring handle (else its first handle) as drawn, grips at 2.4 s,
/// carries it 10 mm outward (along the arrow for a linear handle) over 2.6..4.6 s and lets go at
/// 5 s. `FAB_XR_HANDLE_PART` picks the plate part (its index), `FAB_XR_HANDLE_KEY` the
/// parameter. While held it logs where the drawn grip is
/// against the hand (`fab xr handle synth:`); the draw logs where the handles sit on the part
/// in both eyes (`fab xr handle align:`).
#[derive(Default)]
struct HandleSynth {
    /// The plate part (index).
    part: usize,
    clock: SynthClock,
    /// The parameter, its grip when taken (printer mm) and the drag direction.
    target: Option<(String, [f64; 3], [f64; 3])>,
    logged_at: f64,
    aligned_at: f64,
    all_at: f64,
}

impl HandleSynth {
    #[allow(clippy::too_many_arguments)]
    fn step(&mut self, s: &mut XrState, now: f64, view: &XrNavView, part: fab_plate::Bounds, selected: bool, pp: Option<&fab::params::PartParamsView>, state: Option<&fab::AppState>) {
        use fab::params::*;
        // the clock starts once the part has its parameters; a click that did not select
        // (the plate was not drawn yet) is tried again
        if !self.clock.started() && pp.is_none_or(|pp| pp.params.is_empty()) {
            return;
        }
        let mut t = self.clock.at(now) as f32;
        if t >= 2.0 && !selected && self.target.is_none() {
            self.clock.set(0.9);
            t = 0.9;
        }
        let ident = Quat { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };
        let (l, r) = (&mut s.left_controller, &mut s.right_controller);
        for c in [&mut *l, &mut *r] {
            c.buttons = XrController::ACTIVE;
            c.grip = 0.0;
            c.trigger = 0.0;
            c.stick = Vec2f::default();
            c.grip_pose.orientation = ident;
            c.aim_pose.orientation = ident;
        }
        l.grip_pose.position = vec3f(-0.35, 1.0, -0.2);
        l.aim_pose = l.grip_pose;
        let rest = vec3f(0.25, 1.0, -0.3);
        let mut hand = rest;
        let eye = xr_edit::printer_from_room(view, s.head_pose.position);
        let c = part.center();
        if (1.0..1.8).contains(&t) {
            // above the part, aiming straight down (-Z turned onto -Y)
            hand = xr_edit::room_from_printer(view, [c[0], c[1], part.max[2] + 80.0]);
            r.aim_pose.orientation = Quat { x: -std::f32::consts::FRAC_1_SQRT_2, y: 0.0, z: 0.0, w: std::f32::consts::FRAC_1_SQRT_2 };
            if (1.2..1.35).contains(&t) {
                r.trigger = 1.0;
            }
        }
        if t >= 2.0 && self.target.is_none() && selected {
            let shown: Vec<&ParamView> = match (pp, state) {
                (Some(pp), Some(st)) => fab::tools::handles::shown_params(st, pp),
                _ => Vec::new(),
            };
            let want = std::env::var("FAB_XR_HANDLE_KEY").ok();
            let pick = shown
                .iter()
                .find(|p| want.as_deref() == Some(p.key.as_str()))
                .or_else(|| shown.iter().find(|p| want.is_none() && matches!(p.handle, Some(ParamHandle::Radius { .. }))))
                .or_else(|| shown.iter().find(|_| want.is_none()));
            if let Some(p) = pick {
                let h = p.handle.as_ref().unwrap();
                let at = grip_point(h, HandleGrip::default(), unit(sub(eye, grip_point(h, HandleGrip::default(), eye))));
                let dir = match h {
                    ParamHandle::Radius { center, axis, .. } => {
                        let d = sub(at, *center);
                        unit(sub(d, mul(unit(*axis), dot(d, unit(*axis)))))
                    }
                    ParamHandle::Linear { from, to, .. } => unit(sub(*to, *from)),
                    _ => [1.0, 0.0, 0.0],
                };
                log!("fab xr handle synth: taking {} at {:.1} {:.1} {:.1} mm, value {}", p.key, at[0], at[1], at[2], format_value(p, &p.value));
                self.target = Some((p.key.clone(), at, dir));
            }
        }
        if let Some((key, at, dir)) = self.target.clone() {
            if (2.0..5.0).contains(&t) {
                let k = ((t - 2.6) / 2.0).clamp(0.0, 1.0) as f64;
                let p = add(at, mul(dir, 10.0 * k));
                hand = xr_edit::room_from_printer(view, p);
                if t >= 2.4 {
                    r.grip = 1.0;
                }
                // the drawn grip against the hand (the handle follows it)
                if t >= 2.5 && now - self.logged_at >= 0.2 {
                    self.logged_at = now;
                    if let Some(q) = pp.and_then(|pp| pp.param(&key)) {
                        if let Some(h) = &q.handle {
                            let g = grip_point(h, HandleGrip::default(), unit(sub(eye, grip_point(h, HandleGrip::default(), eye))));
                            log!("fab xr handle synth: t={t:.2} {key} = {} drawn grip {:.2} mm from the hand", format_value(q, &q.value), length(sub(g, p)));
                        }
                    }
                }
            }
        }
        // every handle where it stands now, through the drag and once the commit is in
        if let (Some((key, ..)), Some(pp)) = (&self.target, pp) {
            if t >= 2.0 && (t < 5.0 || t >= 7.0) && now - self.all_at >= 0.1 && pp.param(key).is_some() && t < 7.5 {
                self.all_at = now;
                let mut line = format!("fab xr handle synth: all t={t:.2} value={}", pp.param(key).and_then(|p| p.value.num()).unwrap_or(0.0));
                for p in &pp.params {
                    let at = match &p.handle {
                        Some(ParamHandle::Linear { from, .. }) => *from,
                        Some(ParamHandle::Radius { center, .. }) | Some(ParamHandle::Angle { center, .. }) => *center,
                        Some(ParamHandle::Point { at, .. }) => *at,
                        _ => continue,
                    };
                    line += &format!(" | {} {:.3} {:.3} {:.3}", p.key, at[0], at[1], at[2]);
                }
                log!("{line}");
            }
        }
        input::place_tip(r, hand);
    }
}

/// `FAB_XR_EDIT_SYNTH=1`: a scripted session on synthetic controllers, seconds from the first
/// frame with a plate: the right grip takes the first part at 3 s, carries it 60 mm along the
/// bed's X and 40 mm up and lets go at 5 s (it drops and commits); the wrist menu's Preview row
/// at 7 s (taken as its command, no hand aims at it); in Preview
/// the grip takes it again at 9 s and carries it 30 mm back until 11 s; the right
/// thumbstick click held 12..15 s while the hand goes 25 mm down (layers) and 7 cm across;
/// Y at 16 s and 17 s (the menu opens and closes); B at 18 s (undo).
#[derive(Default)]
struct SynthEdit {
    clock: SynthClock,
    grab_at: Option<Vec3f>,
    regrab_at: Option<Vec3f>,
    layer_at: Option<Vec3f>,
    logged: u32,
    /// 7 s passed: the Preview row was asked for, and is it still to be taken.
    slice_asked: bool,
    slice_row: bool,
}

impl SynthEdit {
    fn step(&mut self, s: &mut XrState, now: f64, view: &XrNavView, part: fab_plate::Bounds, delta: &Affine) {
        let t = self.clock.at(now) as f32;
        let ident = Quat { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };
        let (l, r) = (&mut s.left_controller, &mut s.right_controller);
        for c in [&mut *l, &mut *r] {
            c.buttons = XrController::ACTIVE;
            c.grip = 0.0;
            c.trigger = 0.0;
            c.stick = Vec2f::default();
            c.grip_pose.orientation = ident;
            c.aim_pose.orientation = ident;
        }
        l.grip_pose.position = vec3f(-0.35, 1.0, -0.2);
        l.aim_pose = l.grip_pose;
        let top = {
            let c = part.center();
            xr_edit::apply(delta, [c[0], c[1], part.max[2]])
        };
        // the hand rests 25 cm to the right of the head until it reaches for the part
        let rest = vec3f(0.25, 1.0, -0.3);
        let mut hand = rest;
        if (2.5..5.0).contains(&t) {
            let at = *self.grab_at.get_or_insert_with(|| xr_edit::room_from_printer(view, top));
            let k = ((t - 3.2) / 1.3).clamp(0.0, 1.0);
            let m = 0.001 * view.scale;
            hand = at + vec3f(0.06 * m * 1000.0 * k, 0.04 * m * 1000.0 * k, 0.0);
            if t >= 3.0 {
                r.grip = 1.0;
            }
        }
        if (5.0..5.6).contains(&t) {
            hand = self.grab_at.unwrap_or(rest) + vec3f(0.06 * view.scale, 0.04 * view.scale, 0.0);
        }
        // in Preview: the same part again, 30 mm back (the live slice shows while it moves)
        if (8.5..11.0).contains(&t) {
            let at = *self.regrab_at.get_or_insert_with(|| xr_edit::room_from_printer(view, top));
            let k = ((t - 9.2) / 1.3).clamp(0.0, 1.0);
            hand = at + vec3f(-0.03 * view.scale * k, 0.0, 0.0);
            if t >= 9.0 {
                r.grip = 1.0;
            }
        }
        if (11.0..11.6).contains(&t) {
            hand = self.regrab_at.unwrap_or(rest) + vec3f(-0.03 * view.scale, 0.0, 0.0);
        }
        let press = |from: f32| (from..from + 0.05).contains(&t);
        if t >= 7.0 && !self.slice_asked {
            self.slice_asked = true;
            self.slice_row = true;
        }
        if (12.0..15.0).contains(&t) {
            let at = *self.layer_at.get_or_insert(rest);
            let up = ((t - 12.5) / 1.5).clamp(0.0, 1.0) * -0.025;
            let across = ((t - 14.0) / 0.5).clamp(0.0, 1.0) * -0.07;
            hand = at + vec3f(across, up, 0.0);
            r.buttons |= XrController::CLICK_THUMBSTICK;
        }
        if press(16.0) || press(17.0) {
            l.buttons |= XrController::CLICK_Y;
        }
        if press(18.0) {
            r.buttons |= XrController::CLICK_B;
        }
        r.aim_pose = r.grip_pose;
        input::place_tip(r, hand);
        let marks = [3.0, 5.0, 7.0, 9.0, 11.0, 12.0, 15.0, 16.0, 18.0, 20.0];
        if (self.logged as usize) < marks.len() && t >= marks[self.logged as usize] {
            log!("fab xr synth: t={:.1}s step {}", t, self.logged);
            self.logged += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Mutex};

    /// Pictures asked for faster than they are made wait their turn: one worker on them at a
    /// time however many wait, none dropped, made in the order asked. The test moves on when a
    /// picture's job ends, never by the clock: the pool is shared, and a busy one starts a job late.
    #[test]
    fn pictures_asked_faster_than_made_wait_for_one_worker() {
        /// Says a picture's job is over when dropped: at its end, a panic included.
        struct Over(mpsc::Sender<()>);
        impl Drop for Over {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let _cx = crate::cx_shared();
        let pool = Cx::new(Box::new(|_, _| {})).task_pool();
        let running = Arc::new(AtomicUsize::new(0));
        let most_running = Arc::new(AtomicUsize::new(0));
        let made = Arc::new(Mutex::new(Vec::new()));
        // the first picture is made only once all are asked for
        let (open, gate) = mpsc::channel::<()>();
        let gate = Arc::new(Mutex::new(gate));
        let (over, ended) = mpsc::channel::<()>();
        let mut queue = GrabQueue::default();
        let mut most_waiting = 0;
        for i in 0..8 {
            let (running, most_running, made, gate, over) = (running.clone(), most_running.clone(), made.clone(), gate.clone(), over.clone());
            queue.push(format!("picture {i}"), move || {
                let _over = Over(over);
                most_running.fetch_max(running.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                if i == 0 {
                    let _ = gate.lock().unwrap().recv();
                }
                made.lock().unwrap().push(i);
                running.fetch_sub(1, Ordering::SeqCst);
            });
            queue.turn(&pool);
            most_waiting = most_waiting.max(queue.waiting.len());
        }
        open.send(()).unwrap();
        // each job's end lets the next picture go to the pool
        for _ in 0..8 {
            ended.recv().unwrap();
            // its task is finished right after the job's body
            while queue.making.as_ref().is_some_and(|(_, task)| !task.is_finished()) {
                std::thread::yield_now();
            }
            queue.turn(&pool);
        }
        assert!(!queue.busy(), "pictures left after all eight were made: {} waiting", queue.waiting.len());
        assert_eq!(most_waiting, 7, "the pictures asked for did not outpace the one being made");
        assert_eq!(most_running.load(Ordering::SeqCst), 1, "more than one worker made pictures at once");
        assert_eq!(*made.lock().unwrap(), (0..8).collect::<Vec<_>>(), "pictures dropped or made out of order");
    }
}

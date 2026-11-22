//! The Board pane: the board's layers from retained batches, each layer drawn opaque into its
//! own offscreen target and composited at its opacity in stack order, the holes cut through
//! after, hover from the pick index, pan, zoom and a timed auto-pan for the bench.
//!
//! Board layers per frame (camera moves, hover, transitions): uniforms only. For each shown layer one
//! offscreen pass with at most three retained lists (polygon triangles, strokes, pads) plus one
//! composite quad, with the substrate before the layers and holes after: O(layers) CPU, never
//! O(items). The board's drawing is shared (`shared::BoardDraws`): built once on the task pool for
//! every view of the board, incrementally after an edit; the pane keeps drawing the previous
//! picture until the new one is in, and re-records only the streams whose publication changed.
//! The pane only reads drawings: its host hands boards to `BoardDraws` (`ElecHost` from each
//! snapshot, the bench itself) and points the pane at a unit (`set_unit`); without one it follows
//! the running bench's board, or the design's first board. It follows the theme's light or dark ground.
//! Check markers resolve when the findings, awaiting ids, preview awaiting a wave, drawn geometry
//! or unit change. The overlay culls and draws those cached marks through the current camera.
//!
//! Input is the canvas grammar (`canvas::pane`, ARCHITECTURE 6.3). Dragging a part or a via moves
//! it with its connections: the total delta snaps so the item's origin lands on the pane's grid
//! (`grid_mm`), and each new snapped delta goes to the host as a `ViewAction::Preview` of one
//! `BoardOp::Move` (anchored track ends follow inside the op); the drop commits it as one undo
//! step, Escape cancels it. In the same frame, without waiting for the host, the cursor holds the
//! item (`held::HeldDraw`), its ghost stays at the committed place, the picture's own copy of the
//! item is dimmed (the pane-state rows), and the ratsnest ends on it follow (`rats::RatsDraw`).
//! Cost of a drag: O(the item's size) at its start (logged), O(the moving rat lines) per grid step,
//! O(1) per pointer move otherwise.
//!
//! Keys act on what is under the pointer (`plan`), each one undo step committed at the press:
//! Delete or Backspace deletes the hovered item (a part goes back to the tray) and the picture
//! fades it out where it stood; F pours the net of the copper under the pointer (else the
//! circuit's ground) over the whole board on the active copper layer. A key that cannot act
//! says why beside the pointer (a refusal chip, for 3 s or until the hover moves); a key that
//! acts shows only its result, unless it also changed items the user did not point at: a pour
//! that raised other zones' priority marks them (emphasis, pane-state y) and says so in a note
//! chip for as long. Cost per key: one plan over the committed design (O(zones) for the pour's
//! checks) and, for a delete, the item's held copy (O(the item's size)).
//!
//! Layer targets: one premultiplied BGRA8 colour target per shown layer at the pane's device
//! size (4 bytes per device pixel each: 13.6 MB per layer at 2560 x 1328); a hidden layer
//! releases its target. Inside a target the layer's shapes blend with a per-channel max (the
//! `*Max` shaders), so overlaps and joints cover once; the targets then composite over each
//! other at their opacities. `set_composite(false)` draws every layer straight into the pane at
//! its opacity instead (no targets, over-blended; overlapping items of one layer then double
//! their alpha).

pub mod flags;

use super::batch::Templates;
use super::checks::{check_marks, wave_may_clear, CheckMark};
use super::held::{HeldDraw, HeldLook, HeldVars, PENDING_MIX};
use super::rats::RatsDraw;
use super::focus::{focus_box, focus_camera, Glide};
use super::draw::{
    DrawBoardGlow, DrawBoardLayer, DrawBoardPad, DrawBoardPadMax, DrawBoardStroke, DrawBoardStrokeMax, DrawBoardTri, DrawBoardTriMax,
};
use super::inst::{PadTemplate, StrokeTemplate, TriTemplate};
use super::lists::{BoardTextures, BoardUniforms, StreamList};
use super::heatkey::HeatKey;
use super::routeline::{cut_words, left_open, pushed, selected_unrouted, still_open, unpushed_words, why_open, LeftOpen, LineButton, LineKey, RouteLine};
use super::tools::{apply_strip, method_anchor, method_name, strip_presses, Armed, BoardStripAction, BoardTool, StripPress, StripShow, METHODS, STRIP_PARTS};
use crate::canvas::tools::{draw_bar, draw_strip, over_strip, ToolClicks, STRIP_INSET, STRIP_TOP};
use super::marks::MarkOverlay;
use crate::live::flags::ProbeFlags;
use super::nodes::{probe_names, BoardItems};
use super::plan;
use super::rail::RailLayer;
use super::scene::DrawScene;
use super::gesture::{classify, pour_cut, snap_target, track_widths, Press, Routing};
use super::shared::{BoardDraw, BoardDraws, BoardGeom};
use super::status::{BarPress, BoardStatus, BAR_PARTS};
use super::theme::BoardTheme;
use super::tray::{placement_op, tray_drag_part};
use crate::canvas::chips::{Chip, ChipAnchor, ChipKind, ChipLayer};
use crate::canvas::draw::{CanvasLive, DrawCanvasBox, DrawCanvasLine};
use crate::canvas::looks::{Tokens, STATE_SECONDS};
use crate::canvas::{pane::{SavedView2d, ViewOnArrival, BOARD_MAX_PPU, HIT_RADIUS}, Camera2d, CanvasInput, CanvasPane};
use crate::live::probe::{carries_probe, host_bench, probe_key, probe_on_click, probe_target, ProbeGesture};
use crate::snapshot::{next_tag, DesignSnapshot, ElecLive, RouteJobState, StateOf, ViewAction};
use crate::{ItemStates, StatesOf};
use fab_elec_board::{Anchor, Board, BoardOp, ConnItem, Connectivity, Field, Item, ItemKind};
use fab_elec_circuit::Violation;
use fab_elec_core::{to_mm, Angle, BoardId, CompId, FeatureRef, ItemId, LayerId, NetId, Nm, Sel, Side, P};
use fab_elec_design::{BenchOp, Design, DesignOp, Focus, FocusKind, FocusOrigin, PartRef, Place, ProbeOf};
use fab_elec_route::{GestureKind, JobPhase, Method, Mode, Posture, ProposalSource, Reason, RouteProposal, Tier};
use fab_elec_sim::heat::HeatField;
use makepad_widgets_core::{makepad_platform::a11y_tree, SemanticLookup, *};
use std::collections::VecDeque;
use std::sync::Arc;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.BoardPaneBase = #(BoardPane::register_widget(vm))
    mod.widgets.BoardPane = set_type_default() do mod.widgets.BoardPaneBase{
        width: Fill
        height: Fill
        ground: theme.color_surface_container
        grid_mm: 0.5
        draw_bg: mod.draw.DrawColor{}
        draw_stroke: mod.draw.DrawBoardStroke{}
        draw_pad: mod.draw.DrawBoardPad{}
        draw_glow: mod.draw.DrawBoardGlow{}
        draw_tri: mod.draw.DrawBoardTri{}
        draw_stroke_max: mod.draw.DrawBoardStrokeMax{}
        draw_pad_max: mod.draw.DrawBoardPadMax{}
        draw_tri_max: mod.draw.DrawBoardTriMax{}
        draw_layer: mod.draw.DrawBoardLayer{}
        marks: mod.widgets.BoardMarkOverlay{}
        flags: mod.widgets.ProbeFlags{}
        heat_key: mod.widgets.BoardHeatKey{}
        route_line: mod.widgets.BoardRouteLine{}
        // The tool strip (`tools`): the host gives it its look; none here, only the keys.
        tools: View{width: Fit height: Fit visible: false}
        status: View{visible: false}
        draw_line: mod.draw.DrawCanvasLine{}
        draw_box: mod.draw.DrawCanvasBox{}
        // The Measure ruler (`seg` from its start to its end), the Zone rectangle being dragged
        // (`zone` 1, `seg` its corners) or drawn copper's rubber line while its answer is late
        // (`zone` 2), in the pane's local points: one quad over the pane.
        draw_ruler: mod.draw.DrawQuad{
            seg: uniform(vec4(0.0))
            ink: uniform(vec4(1.0))
            zone: uniform(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                if self.zone > 1.5 {
                    sdf.move_to(self.seg.x, self.seg.y)
                    sdf.line_to(self.seg.z, self.seg.w)
                    sdf.stroke(vec4(self.ink.xyz, 0.6), 1.0)
                    return sdf.result
                }
                if self.zone > 0.5 {
                    sdf.rect(self.seg.x, self.seg.y, self.seg.z - self.seg.x, self.seg.w - self.seg.y)
                    sdf.fill_keep(vec4(self.ink.xyz, 0.16))
                    sdf.stroke(self.ink, 1.0)
                    return sdf.result
                }
                sdf.move_to(self.seg.x, self.seg.y)
                sdf.line_to(self.seg.z, self.seg.w)
                sdf.stroke(self.ink, 1.5)
                sdf.circle(self.seg.x, self.seg.y, 3.0)
                sdf.fill(self.ink)
                sdf.circle(self.seg.z, self.seg.w, 3.0)
                sdf.fill(self.ink)
                return sdf.result
            }
        }
        draw_text: mod.draw.DrawText{
            text_style: theme.font_regular{font_size: 9}
        }
    }
}

/// What the pane measured (the bench prints it once a second).
#[derive(Clone, Debug, Default)]
pub struct BoardStats {
    pub frames: u64,
    /// CPU time of the pane's draw, microseconds: the last, and the mean and max since the last
    /// `take_stats`.
    pub draw_us: f64,
    pub draw_us_mean: f64,
    pub draw_us_max: f64,
    /// GPU milliseconds per offscreen pass (mean over the window) and the window's own pass.
    pub gpu_ms: Vec<(String, f64)>,
    pub instances: usize,
    pub lists: usize,
    pub layers: usize,
    /// The current picture: the geometry job (scene, item table, pick index) and the picture job
    /// (diff, partial scene, rows, publications), worker milliseconds.
    pub scene_ms: f64,
    pub batch_ms: f64,
    /// The last edit: from the board handed in to the first frame drawing its picture, UI
    /// milliseconds; whether it was rebatched, and how many items it touched.
    pub edit_ms: f64,
    pub incremental: bool,
    pub edited: usize,
    /// Pictures drawn so far (one per board handed in, coalesced while a build runs).
    pub pictures: u64,
}

struct LayerSlot {
    layer: LayerId,
    tris: Option<StreamList>,
    strokes: Option<StreamList>,
    pads: Option<StreamList>,
    target: Option<RenderTarget>,
    gpu: Vec<f64>,
}

/// A hovered pour's edge light (`BoardUniforms::hover_at`) for a pass whose pixels start at
/// `origin` (absolute) and are `scale` per pane point: lit fully within 16 points of the pointer,
/// gone at 64. Far off without a pointer.
fn pour_light(pointer: Option<DVec2>, origin: DVec2, scale: f64) -> [f32; 4] {
    let at = pointer.map_or([-1e6, -1e6], |p| [((p.x - origin.x) * scale) as f32, ((p.y - origin.y) * scale) as f32]);
    [at[0], at[1], (16.0 * scale) as f32, (64.0 * scale) as f32]
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

/// The Board pane widget (module docs).
#[derive(Script, ScriptHook, Widget)]
pub struct BoardPane {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_stroke: DrawBoardStroke,
    #[live]
    draw_pad: DrawBoardPad,
    /// The part underlay between the substrate and the first copper layer.
    #[live]
    draw_glow: DrawBoardGlow,
    #[live]
    draw_tri: DrawBoardTri,
    // The same shaders max-blended, for the layer targets.
    #[live]
    draw_stroke_max: DrawBoardStrokeMax,
    #[live]
    draw_pad_max: DrawBoardPadMax,
    #[live]
    draw_tri_max: DrawBoardTriMax,
    #[live]
    draw_layer: DrawBoardLayer,
    #[live]
    marks: MarkOverlay,
    /// The probes' named connections drawn on the copper (`live::flags`, `flags`).
    #[live]
    flags: ProbeFlags,
    /// The heat field's legend (shown while a field is).
    #[live]
    heat_key: HeatKey,
    /// The route line (`routeline`): unrouted nets with Route all and Route selected.
    #[live]
    route_line: RouteLine,
    /// The tool strip along the left edge (`tools`), as the host styles it.
    #[find]
    #[live]
    tools: WidgetRef,
    /// The host's status row along the bottom edge.
    #[find]
    #[live]
    status: WidgetRef,
    /// The theme's panel ground: a light one gives the light board look (unless `set_light`).
    #[live]
    ground: Vec4f,
    /// The placement grid a dragged item's origin snaps to, mm.
    #[live]
    grid_mm: f64,
    /// The refusal chip's outline, box and text.
    #[live]
    draw_line: DrawCanvasLine,
    #[live]
    draw_box: DrawCanvasBox,
    #[live]
    draw_ruler: DrawQuad,
    #[live]
    draw_text: DrawText,
    #[rust]
    st: PaneState,
    #[rust(CanvasPane::with_max_ppu(BOARD_MAX_PPU))]
    pane: CanvasPane<Sel>,
    #[rust]
    opening_view: Option<ViewOnArrival<BoardGeom>>,
}

/// A part or via being dragged (B2).
struct Hold {
    unit: BoardId,
    item: ItemId,
    /// World mm where the drag started, and the item's origin there (nm, committed).
    from: [f64; 2],
    origin: P,
    /// The snapped total delta last sent to the host (nm).
    by: P,
    previewed: bool,
    /// The tag of the newest preview sent, None while none is live: the host names it on the
    /// drop's commit (`Committed::tag`) or refusal (`Refusal::sample`).
    tag: Option<u64>,
    /// The board the drag started on: the held copy's and the ratsnest ends' committed places.
    board: Arc<Board>,
    /// Dropped: the held copy stays at the drop until the drop is on screen.
    landing: Option<Landing>,
    /// The worker's stretch gesture, when the host answers gestures (`ElecLive`): its samples
    /// carry the motion and its answers are the preview and the commit (landing 4). Without one
    /// the drag previews the plain move.
    routing: Option<Routing>,
    /// The newest gesture answer this pane has read (`ElecLive::gesture`, by identity).
    seen: Option<Arc<RouteProposal>>,
    /// The chip's text for the tracks the shown answer cuts back, when it cuts any.
    chip: Option<String>,
}

/// Copper drawn from a pad or a track's end, or a track's segment moved (item 3). The route worker
/// answers each sample; the host files its answers as the live preview, so the picture shows them.
struct Route {
    routing: Routing,
    /// The picture at the press. Targets snap on it for the whole gesture, never on a picture of
    /// the preview: a reflow cannot move what the pointer aims at (the drag rule both editors keep).
    frozen: Arc<BoardDraw>,
    /// The newest gesture answer this pane has read (`ElecLive::gesture`, by identity).
    seen: Option<Arc<RouteProposal>>,
    /// Released: the release sample's answer, then the commit and its outcome.
    landing: Option<Landing>,
    /// Samples sent and not yet answered, with when (`Cx::monotonic_now`), oldest first.
    sent: VecDeque<(u64, f64)>,
    /// Milliseconds from each answered sample's send to its answer in the pane.
    waits: Vec<f64>,
    /// Why the shown head stops short, as its chip says it.
    blocked: Option<String>,
    /// The track a dragged segment could not push and walks round instead, as its chip says it.
    walked: Option<String>,
    /// Why the host refused the gesture's newest sample, until an answer follows (the chip says it,
    /// so a drag the router does not answer is never silent), and that refusal's `seq`.
    refused: Option<String>,
    refusal_seq: u64,
    /// What a finishing gesture's chip says: that the router is at it, or why it found no way.
    /// Nothing once it finished (no success chip).
    finished: Option<(ChipKind, String)>,
    /// What the shown preview cuts off other nets' pours (`pour_cut`), from the preview
    /// connectivity it was read from (by identity).
    cut: Option<(String, Vec<ItemId>)>,
    cut_seen: Option<Arc<Connectivity>>,
    /// The text of the chip the gesture shows.
    chip: Option<String>,
}

impl Route {
    /// A sample went to the host now: O(1).
    fn sent(&mut self, seq: u64) {
        self.sent.push_back((seq, Cx::monotonic_now()));
    }

    /// The answer to `seq` is in the pane now: its wait, and older samples it covers drop. O(covered).
    fn answered(&mut self, seq: u64) {
        let now = Cx::monotonic_now();
        while let Some(&(sent, at)) = self.sent.front().filter(|(sent, _)| *sent <= seq) {
            self.sent.pop_front();
            if sent == seq {
                self.waits.push((now - at) * 1e3);
            }
        }
    }

    /// One line per gesture: what its answers took, for the drum16 numbers. O(answers log answers).
    fn log_waits(&self) {
        let mut waits = self.waits.clone();
        if waits.is_empty() {
            log!("fab.board: gesture {}: {} samples, none answered in the pane", self.routing.id.0, self.routing.seq);
            return;
        }
        waits.sort_by(f64::total_cmp);
        let at = |q: f64| waits[((waits.len() - 1) as f64 * q).round() as usize];
        log!("fab.board: gesture {}: {} samples, {} answered; sample to answer in the pane median {:.1} ms, p95 {:.1} ms, max {:.1} ms",
            self.routing.id.0, self.routing.seq, waits.len(), at(0.5), at(0.95), at(1.0));
    }
}

/// A part dropped from the tray: its placement is committed, and the preview's picture stays until
/// the commit's picture or a refusal is in (the Hold's landing, without a held copy).
struct Dropped {
    unit: BoardId,
    landing: Landing,
}

/// The one item gesture in progress.
enum Drag {
    Hold(Hold),
    Route(Route),
    Dropped(Dropped),
}

/// A released gesture on its way to the screen. Everything shown at the release stays, and the pane
/// takes no new picture, until the host answers the commit's tag and, for a commit, a picture of the
/// committed board is in: then the copies leave in the frame that picture is shown. No timer: the
/// host owes one commit or refusal per `CommitPreview` it takes.
struct Landing {
    /// The tag the host names on the commit (`Committed::tag`) or the refusal (`Refusal::sample`).
    tag: u64,
    /// `CommitPreview` is sent (a route sends it once the release sample is answered).
    sent: bool,
    /// Monotonic seconds at release, for the held copy's pending tint and the release-to-screen log.
    since: f64,
}

/// A press made while a gesture lands: it starts on the landed picture if the button is still down.
struct Pending {
    unit: BoardId,
    from: [f64; 2],
    at: [f64; 2],
}

/// Key A's label plan, made on the task pool from `design`: it commits only onto that design.
struct Arranging {
    unit: BoardId,
    design: Arc<Design>,
    job: TaskHandle<Result<plan::Plan, String>>,
}

/// What a finished label plan does: commit onto the design it was made from, plan again on the
/// design an edit put in its place, or go when the pane shows another board now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ArrangeFate {
    Commit,
    Again,
    Drop,
}

fn arrange_fate(planned: &Arc<Design>, unit: BoardId, now: Option<&Arc<Design>>, shown: Option<BoardId>) -> ArrangeFate {
    match now {
        _ if shown != Some(unit) => ArrangeFate::Drop,
        Some(now) if Arc::ptr_eq(now, planned) => ArrangeFate::Commit,
        Some(_) => ArrangeFate::Again,
        None => ArrangeFate::Drop,
    }
}

/// A tray placement preview; only its position changes between grid steps. The host resolves the
/// `place` into the part and its labels (`place_ops`), for the preview and the commit alike.
struct Placing {
    unit: BoardId,
    comp: CompId,
    at: P,
    /// The committed design the part was checked against on entry, never a preview snapshot.
    design: Arc<Design>,
    place: Place,
    label: String,
    /// The newest preview's tag: the host names it on the drop's commit or refusal.
    tag: u64,
}

struct HeatSlot {
    /// The field shown until its replacement or the end of its fade-out.
    field: Arc<HeatField>,
    /// Premultiplied ramp and coverage bytes per cell, reused while size and format are unchanged.
    texture: Texture,
}

/// World mm to board nm.
fn nm(at: [f64; 2]) -> P {
    P::new((at[0] * 1e6).round() as Nm, (at[1] * 1e6).round() as Nm)
}

/// Whether `shown` has the dropped `item` at its new place `want`, or no longer has it: the end of a
/// drop for a host without a snapshot. O(1) through the board's item index.
fn shows_drop(item: ItemId, want: P, shown: &Board) -> bool {
    let at = match shown.locate(item) {
        Some((ItemKind::Placed, i)) => Some(shown.placed[i].at),
        Some((ItemKind::Via, i)) => Some(shown.vias[i].at),
        _ => None,
    };
    at.is_none_or(|p| p == want)
}

/// What the host's snapshot says about a released gesture's commit, named by its tag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    /// No answer yet: everything on screen stays as at the release.
    Waiting,
    /// Taken: a picture of the unit's board at this generation, or a later one, shows it.
    Committed(u64),
    /// Refused, whatever else is previewed: the picture is the truth.
    Over,
}

/// O(1). There is no third way out: the host owes exactly one commit or refusal per taken
/// `CommitPreview` (ELEC's H3), and the gap of a commit it cannot carry (its preview dropped, the
/// committed design not yet the new one) is `Waiting`, so the committed state without the edit is
/// never shown for that evaluation.
fn outcome(tag: u64, unit: BoardId, snap: &DesignSnapshot) -> Outcome {
    if snap.last_refusal.as_ref().is_some_and(|r| r.sample == Some(tag)) {
        return Outcome::Over;
    }
    if snap.last_commit.as_ref().is_some_and(|c| c.tag == Some(tag)) {
        return Outcome::Committed(snap.design.unit(unit).and_then(|u| u.board.as_ref()).map_or(0, |b| b.generation));
    }
    Outcome::Waiting
}

/// Whether a landing with `outcome` is over while `shown` is the picture's board.
fn landed(outcome: Outcome, shown: &Board) -> bool {
    match outcome {
        Outcome::Waiting => false,
        Outcome::Committed(generation) => shown.generation >= generation,
        Outcome::Over => true,
    }
}

fn heat_texels(field: &HeatField, current: Option<u64>) -> Option<Vec<u8>> {
    if !field.is_valid() || current.is_some_and(|current| current != field.generation) {
        return None;
    }
    Some(field.values.iter().flat_map(|&v| match field.ramp(v) {
        Some(k) => [(k * 255.0).round() as u8, 255],
        None => [0, 0],
    }).collect())
}

#[derive(Default)]
struct PaneState {
    unit: Option<BoardId>,
    /// Selection labels are resolved only on selection or geometry changes, never per frame.
    selected: Vec<Sel>,
    selection_summary: String,
    /// The shared drawing the lists below were synced from.
    drawn: Option<Arc<BoardDraw>>,
    layers: Vec<LayerSlot>,
    substrate: Option<StreamList>,
    /// Shared footprint rectangles, one per part with a live row.
    glows: Option<StreamList>,
    /// The heat field and its texture, kept until the fade-out ends.
    heat: Option<HeatSlot>,
    /// A valid field is set: fade toward visible, otherwise toward hidden.
    heat_want: bool,
    /// How far the heat is shown, 0..1.
    heat_mix: f32,
    /// Monotonic seconds of the last heat fade step.
    heat_at: f64,
    holes: Option<StreamList>,
    refit: bool,
    /// Edge coverage measured after the overlays drew, for the following placement.
    drawn_inset: [f64; 4],
    /// Where the tool strip was last drawn (empty while hidden): the flags and route line draw
    /// before it.
    strip_at: Rect,
    drag: Option<Drag>,
    pending: Option<Pending>,
    arranging: Option<Arranging>,
    placing: Option<Placing>,
    held: Option<HeldDraw>,
    /// During a hold, x dims the held item and y lifts the neighbours its newest shown answer pushes.
    dims: Option<ItemStates>,
    rats: RatsDraw,
    /// The connectivity a host without `ElecLive` hands in (`set_conn`).
    conn: Option<Arc<Connectivity>>,
    /// The committed design a host without `ElecLive` hands in (`set_design`).
    design: Option<Arc<Design>>,
    /// The bench new probes go to (`set_bench`); without one the pane offers no probe drop.
    bench: Option<String>,
    /// The bench's probes on this board, for their flags.
    flag_probes: flags::BoardProbes,
    /// A probe carried over copper: the flag its drop would make, drawn before the drop.
    flag_ghost: Option<flags::BoardGhost>,
    /// The camera glide toward a focus from another view; any camera input ends it.
    glide: Option<Glide>,
    /// The net under a probe: its probe name and its index on the drawn board (`None` when the
    /// name is on no net of this board). It wins over `host_lit` while a probe is dragged.
    lit: Option<(String, Option<NetId>)>,
    /// The net the host lights (`set_lit_net`: a focused or probed net).
    host_lit: Option<NetId>,
    /// The nets the last finished route job on this board left open, taken once its commit is on
    /// the board; the route line names those still unrouted while the board keeps that generation.
    left_open: Option<LeftOpen>,
    /// The nets of `left_open` the route line names now (`still_open`).
    still_open: Vec<NetId>,
    /// Findings from a host without `ElecLive`, keyed by calls to `set_violations`.
    violations: Option<Arc<Vec<Violation>>>,
    violations_awaiting: Arc<Vec<u64>>,
    violations_gen: u64,
    /// Resolved once per (findings generation, awaiting list, drawn geometry generation, unit,
    /// preview proposal awaiting a wave).
    marks: Vec<CheckMark>,
    marks_key: Option<(u64, usize, u64, BoardId, usize)>,
    /// `set_light`'s choice; `None` follows the theme's ground.
    light: Option<bool>,
    /// The host named the unit (`set_unit`); otherwise the pane follows the electronics host.
    pinned: bool,
    direct: bool,
    autopan: Option<f64>,
    next_frame: NextFrame,
    /// The host's emphasis this pane last drew (`SimLive::emphasis`).
    live_emphasis: u64,
    active: Option<LayerId>,
    /// The person's copper dimensions in nm. Each follows the rules until first set.
    track_w: Option<Nm>,
    /// Copper drawn by hand walks round the tracks in its way instead of pushing them: the
    /// strip's Shove switch off (S flips it). Kept while the pane lives.
    walks: bool,
    via_d: Option<Nm>,
    bar: BoardStatus,
    hidden: Vec<LayerId>,
    shown: Vec<LayerId>,
    solo: Option<LayerId>,
    hover: Option<ItemId>,
    /// The hovered item is a pour: its edge lights around the pointer, so moves redraw.
    hover_pour: bool,
    rect: Rect,
    stats: BoardStats,
    window: Vec<f64>,
    main_gpu: Vec<f64>,
    gpu_buf: Vec<(u64, f64)>,
    timing: bool,
    /// The UI time a newly synced picture's board was handed in, until its first frame draws.
    awaiting: Option<f64>,
    /// A deleted item's copy, faded out once the picture no longer has the item.
    gone: Option<Gone>,
    /// The chip of a key that could not act, or that changed items besides its target, made on
    /// the first one.
    chips: Option<Chips>,
    /// The strip's armed tool, the routing method its Route options chose, the last tool click
    /// (a second one on the same tool soon after keeps it), what the strip's buttons show, and
    /// whether the committed board has copper Clear routes would take, by (unit, generation).
    armed: Armed,
    method: Method,
    tool_clicks: ToolClicks<BoardTool>,
    strip_shown: Option<StripShow>,
    clearable: Option<(BoardId, u64, bool)>,
    /// The items that chip names, marked in the picture (pane-state y) while it shows.
    marked: Vec<ItemId>,
    /// The unit this pane last drew, as told to the Checks editor (`ElecLive::shown_unit`).
    told_unit: Option<BoardId>,
    /// Where the last press on the canvas went down, world mm: the Zone and Measure drags start
    /// there (a marquee knows only its box).
    press_at: Option<[f64; 2]>,
    /// The Measure tool's ruler (from, to), world mm, drawn until the next press or Escape.
    ruler: Option<[[f64; 2]; 2]>,
    /// The Zone tool's rectangle while it is dragged, world mm (min x, min y, max x, max y).
    zone_box: Option<[f64; 4]>,
    /// The strip's height as one column, measured while it stands so: past the pane's height
    /// it stands in two (its board actions beside the rest).
    strip_tall: f64,
}

impl PaneState {
    fn hold(&self) -> Option<&Hold> {
        match &self.drag {
            Some(Drag::Hold(hold)) => Some(hold),
            _ => None,
        }
    }
    fn hold_mut(&mut self) -> Option<&mut Hold> {
        match &mut self.drag {
            Some(Drag::Hold(hold)) => Some(hold),
            _ => None,
        }
    }
    fn route_mut(&mut self) -> Option<&mut Route> {
        match &mut self.drag {
            Some(Drag::Route(route)) => Some(route),
            _ => None,
        }
    }
    /// The released gesture's landing, while one lands.
    fn landing(&self) -> Option<&Landing> {
        match &self.drag {
            Some(Drag::Hold(hold)) => hold.landing.as_ref(),
            Some(Drag::Route(route)) => route.landing.as_ref(),
            Some(Drag::Dropped(dropped)) => Some(&dropped.landing),
            None => None,
        }
    }
}

/// A deleted item's held copy: drawn from the first frame whose picture lacks the item, its
/// opacity falling to zero over the state fade. A refused delete keeps it unseen until the next.
struct Gone {
    unit: BoardId,
    item: ItemId,
    held: HeldDraw,
    start: Option<f64>,
}

struct Chips {
    layer: ChipLayer,
    live: CanvasLive,
    shown: bool,
    timer: Timer,
}

/// How long a key's chip stays when the pointer does not move off what it names.
const NOTE_SECONDS: f64 = 3.0;
/// How long a drawn sample or a dropped item may wait before it takes the pending tint.
/// A waiting route also shows a rubber line to the cursor. Tinting shorter waits would flicker.
const STALE_SECONDS: f64 = 0.05;

impl BoardPane {
    pub fn held_view(&self) -> Option<SavedView2d> {
        self.opening_view.as_ref().map_or_else(|| self.pane.held_view(), |opening| opening.saved)
    }

    pub fn restore_view(&mut self, cx: &mut Cx, view: Option<SavedView2d>) {
        self.opening_view = ViewOnArrival::new(view, self.st.drawn.as_ref().map(|draw| &draw.geom));
        self.redraw(cx);
    }

    /// Fits the whole board at the size the pane draws at next, at once: a layout was applied
    /// (V9). A sash drag refits only while the camera keeps its fit (no pan or zoom since).
    /// A plate's saved view not shown yet places the board the first time instead (7b).
    pub fn fit_when_shown(&mut self, cx: &mut Cx) {
        if self.opening_view.as_ref().is_some_and(|opening| opening.saved.is_some()) {
            return;
        }
        self.pane.keeps_fit = true;
        self.st.refit = true;
        self.redraw(cx);
    }

    /// Free this pane's layer targets, heat and dimming textures, and probe registration.
    /// The holder calls this before dropping the pane because Drop has no Cx.
    /// Calling it twice is harmless. Host inputs and display settings stay; the empty
    /// picture and canvas rebuild on the next draw as on first use.
    pub fn release(&mut self, cx: &mut Cx) {
        self.end_drag(Some(&mut *cx));
        self.cancel_placing(cx);
        self.flags.release();
        if cx.has_global::<ProbeGesture>() {
            let gesture = cx.get_global::<ProbeGesture>();
            if gesture.hover.as_ref().is_some_and(|(_, from)| *from == self.uid) {
                gesture.hover = None;
            }
        }
        let mut st = std::mem::take(&mut self.st);
        self.st = PaneState {
            unit: st.unit,
            selected: std::mem::take(&mut st.selected),
            selection_summary: std::mem::take(&mut st.selection_summary),
            pinned: st.pinned,
            conn: st.conn.take(),
            design: st.design.take(),
            bench: st.bench.take(),
            host_lit: st.host_lit,
            violations: st.violations.take(),
            violations_awaiting: std::mem::take(&mut st.violations_awaiting),
            violations_gen: st.violations_gen,
            light: st.light,
            direct: st.direct,
            active: st.active,
            track_w: st.track_w,
            walks: st.walks,
            via_d: st.via_d,
            hidden: std::mem::take(&mut st.hidden),
            shown: std::mem::take(&mut st.shown),
            solo: st.solo,
            refit: true,
            ..PaneState::default()
        };
        self.pane = CanvasPane::with_max_ppu(BOARD_MAX_PPU);
        let mut textures = Vec::new();
        for slot in &mut st.layers {
            if let Some(t) = slot.target.as_mut() {
                textures.push(t.texture.clone());
                t.pass.withdraw(cx);
                t.release(cx);
            }
        }
        if let Some(dims) = st.dims.as_mut() {
            dims.release(cx);
        }
        if let Some(draw) = st.drawn.take() {
            // Copper rows belong to the shared drawing, not to any one pane.
            BoardDraws::retire(cx, draw);
        }
        if let Some(heat) = st.heat.take() {
            textures.push(heat.texture);
            BoardDraws::discard(cx, heat.field);
        }
        if !st.marks.is_empty() {
            BoardDraws::discard(cx, std::mem::take(&mut st.marks));
        }
        for vars in [
            &mut self.draw_stroke.draw_vars, &mut self.draw_pad.draw_vars,
            &mut self.draw_tri.draw_vars, &mut self.draw_stroke_max.draw_vars,
            &mut self.draw_pad_max.draw_vars, &mut self.draw_tri_max.draw_vars,
            &mut self.draw_layer.draw_vars, &mut self.draw_glow.draw_vars,
        ] {
            vars.texture_slots.fill(None);
        }
        // Composite quads can still retain the targets in the enclosing draw list.
        if !textures.is_empty() {
            let ids: Vec<_> = textures.iter().map(Texture::texture_id).collect();
            cx.draw_lists.release_bindings(&ids, &[]);
            for texture in textures {
                texture.release(cx);
            }
        }
        self.redraw(cx);
    }

    /// Show this board unit's shared drawing (its host hands the boards to `BoardDraws`); the
    /// old picture stays up until the new one is in. `None` follows the electronics host's design.
    pub fn set_unit(&mut self, cx: &mut Cx, unit: Option<BoardId>) {
        self.st.pinned = unit.is_some();
        if unit.is_some() {
            self.show(unit);
        }
        self.redraw(cx);
    }

    fn show(&mut self, unit: Option<BoardId>) {
        if self.st.unit != unit {
            self.st.unit = unit;
            self.st.selected.clear();
            self.st.selection_summary.clear();
            self.st.refit = true;
            self.end_drag(None);
        }
    }

    /// The board the bench runs on when it has one, else the first board of the design.
    fn follow(&mut self, cx: &mut Cx) {
        if self.st.pinned || !cx.has_global::<ElecLive>() {
            return;
        }
        let live = cx.get_global::<ElecLive>();
        let design = live.snap.live();
        let unit = live.run_bench().and_then(|b| design.unit(b.out.scope_id())).filter(|u| u.board.is_some())
            .or_else(|| design.units.iter().find(|u| u.board.is_some())).map(|u| u.id);
        self.show(unit);
    }

    /// The board's shared drawing as this pane last drew it (scene, items, pick, batch,
    /// publications), for overlays and tools that draw or query the same board.
    pub fn drawing(&self) -> Option<&Arc<BoardDraw>> {
        self.st.drawn.as_ref()
    }

    /// The board connectivity for the ratsnest, from a host without `ElecLive` (the bench); with
    /// one, the pane reads the snapshot's.
    pub fn set_conn(&mut self, cx: &mut Cx, conn: Option<Arc<Connectivity>>) {
        self.st.conn = conn;
        self.redraw(cx);
    }

    /// The committed design for tray placement from a host without `ElecLive`. The live
    /// snapshot takes precedence; replaced designs are freed on the pool.
    pub fn set_design(&mut self, cx: &mut Cx, design: Option<Arc<Design>>) {
        if let Some(old) = std::mem::replace(&mut self.st.design, design) {
            BoardDraws::discard(cx, old);
        }
        self.redraw(cx);
    }

    /// Set the bench the host's bench view shows; probes dropped here (or P over copper) go to
    /// it. With none, a probe over the board only lights the net under it.
    pub fn set_bench(&mut self, bench: Option<String>) {
        self.st.bench = bench;
    }

    /// Answer a focus from another view (a Checks row, the explorer, code): glide the camera so
    /// the focused item, part, pin or net is in view, without moving for what is already in plain
    /// sight. Own / foreign focuses update the selection summary but never move this camera.
    /// O(board items) once for a net, O(1) per glide frame.
    pub fn answer_focus(&mut self, cx: &mut Cx, focus: &Focus) {
        self.select_focus(cx, focus);
        if focus.origin == FocusOrigin::Board {
            return;
        }
        let Some(draw) = self.st.drawn.clone() else { return };
        let design = self.design(cx);
        let Some(bounds) = focus_box(&draw.board, design.as_deref(), &focus.sel) else { return };
        let to = focus_camera(&self.pane.cam, bounds);
        if to == self.pane.cam {
            return;
        }
        self.st.glide = Some(Glide::new(self.pane.cam, to, Cx::monotonic_now()));
        if !cx.next_frame_is_pending(self.st.next_frame) {
            self.st.next_frame = cx.new_next_frame();
        }
    }

    /// Set this pane's selection from a host's selection list. The existing Focus broadcast
    /// selects one item (nets are emphasis); hosts with a multi-selection use this seam.
    /// Count and at most three labels are cached here, so semantics stays O(1) per frame.
    pub fn set_selection(&mut self, cx: &mut Cx, selected: &[Sel]) {
        let selected: Vec<_> = selected.iter().filter(|sel| self.st.unit.is_some() && sel.unit() == self.st.unit).cloned().collect();
        if self.st.selected == selected { return; }
        self.st.selected = selected;
        self.selection_summary();
        self.redraw(cx);
    }

    fn select_focus(&mut self, cx: &mut Cx, focus: &Focus) {
        let selected = if focus.kind == FocusKind::Net { &[][..] } else { std::slice::from_ref(&focus.sel) };
        self.set_selection(cx, selected);
    }

    /// Refresh names once when the selection or its drawn geometry changes. Component/feature
    /// joins may visit placements here; drawing only reads the resulting bounded string.
    fn selection_summary(&mut self) {
        self.st.selection_summary.clear();
        if self.st.selected.is_empty() { return; }
        self.st.selection_summary = format!(", {} selected", self.st.selected.len());
        let Some(draw) = &self.st.drawn else { return };
        let board = &draw.geom.board;
        let mut labels = Vec::new();
        for sel in self.st.selected.iter().take(3) {
            if sel.unit() != Some(board.unit) { continue; }
            let part = |comp| board.placed_of(comp).map(|part| part.refdes.clone());
            let name = match sel {
                Sel::Item(_, item) | Sel::Feature(FeatureRef::Hole(_, item)) => draw.geom.names.name(*item).map(str::to_owned),
                Sel::Comp(c) | Sel::Feature(FeatureRef::Part(c) | FeatureRef::Pad(c, _) | FeatureRef::Body(c) | FeatureRef::Port(c, _)) => part(c.comp),
                Sel::Pin(p) => part(p.comp),
                Sel::Net(n) => board.nets.get(n.net.ix()).map(|net| net.name.clone()),
                Sel::Unit(_) | Sel::Feature(FeatureRef::Outline(_)) => Some(board.name.clone()),
                _ => None,
            };
            if let Some(name) = name { labels.push(name); }
        }
        if !labels.is_empty() {
            self.st.selection_summary.push_str(": ");
            self.st.selection_summary.push_str(&labels.join(", "));
        }
    }

    /// Light one net of this board in this pane (the host's focused or probed net): its copper
    /// lifts toward white and the rest steps back, from one uniform against each instance's
    /// board net. O(1); a probe dragged over the pane lights its own net meanwhile.
    pub fn set_lit_net(&mut self, cx: &mut Cx, net: Option<NetId>) {
        if self.st.host_lit != net {
            self.st.host_lit = net;
            self.redraw(cx);
        }
    }

    /// The heat field glowing over the board material (a board temperature map), or none. The
    /// producer is ESIM's board thermal solve, which comes after the co-sim; until then the board
    /// bench's `--heat-test` is the only producer. O(cells) once per field: the texture is rewritten
    /// in place when size and format are unchanged; it fades in and out over 0.25 s.
    pub fn set_heat(&mut self, cx: &mut Cx, field: Option<Arc<HeatField>>) {
        let current = if cx.has_global::<ElecLive>() { Some(cx.get_global::<ElecLive>().sim.index.generation) } else { None };
        if self.st.heat_want && field.as_ref().zip(self.st.heat.as_ref()).is_some_and(|(field, slot)| {
            Arc::ptr_eq(field, &slot.field) && current.map_or(true, |current| current == field.generation)
        }) {
            return;
        }
        let field = field.and_then(|field| {
            if let Some(texels) = heat_texels(&field, current) {
                Some((field, texels))
            } else {
                if !field.is_valid() {
                    log!("fab.board: heat field {}x{} with {} values is not drawn", field.w, field.h, field.values.len());
                } else if let Some(c) = current {
                    let g = field.generation;
                    log!("fab.board: heat field of generation {g} is stale (current {c}), not drawn");
                }
                BoardDraws::discard(cx, field);
                None
            }
        });
        let want = field.is_some();
        if let Some((field, texels)) = field {
            let (width, height) = (field.w as usize, field.h as usize);
            if let Some(slot) = self.st.heat.as_mut().filter(|slot| matches!(
                slot.texture.get_format(cx), TextureFormat::VecRGu8 { width: w, height: h, .. } if *w == width && *h == height
            )) {
                let mut data = slot.texture.take_vec_u8(cx);
                data.copy_from_slice(&texels);
                let rect = RectUsize { origin: PointUsize { x: 0, y: 0 }, size: SizeUsize { width, height } };
                slot.texture.put_back_vec_u8(cx, data, Some(rect));
                BoardDraws::discard(cx, std::mem::replace(&mut slot.field, field));
            } else {
                let format = TextureFormat::VecRGu8 { width, height, data: Some(texels), unpack_row_length: None, updated: TextureUpdated::Full };
                let texture = Texture::new_with_format(cx, format);
                if let Some(old) = self.st.heat.replace(HeatSlot { field, texture }) {
                    BoardDraws::discard(cx, old.field);
                }
            }
        }
        if self.st.heat_want != want {
            self.st.heat_at = Cx::monotonic_now();
        }
        self.st.heat_want = want;
        if !cx.next_frame_is_pending(self.st.next_frame) {
            self.st.next_frame = cx.new_next_frame();
        }
        self.redraw(cx);
    }

    /// Findings from a host without `ElecLive` (the bench). With `ElecLive`, the snapshot's
    /// findings take precedence. `awaiting` is the sorted ids of `v` that await a zone's refill,
    /// shown as pending; empty for none. Replaced findings and awaiting ids are freed on the pool.
    pub fn set_violations(&mut self, cx: &mut Cx, v: Option<Arc<Vec<Violation>>>, awaiting: Arc<Vec<u64>>) {
        if let Some(old) = std::mem::replace(&mut self.st.violations, v) {
            BoardDraws::discard(cx, old);
        }
        BoardDraws::discard(cx, std::mem::replace(&mut self.st.violations_awaiting, awaiting));
        self.st.violations_gen = self.st.violations_gen.wrapping_add(1);
        self.redraw(cx);
    }

    pub fn set_light(&mut self, cx: &mut Cx, light: bool) {
        self.st.light = Some(light);
        self.redraw(cx);
    }

    /// The built board's layers in topmost-first stack order, for the layer rail.
    pub fn layers(&self) -> Vec<RailLayer> {
        let Some(draw) = &self.st.drawn else { return Vec::new() };
        let scene = &draw.geom.scene;
        let theme = self.theme();
        let active = self.st.active.unwrap_or(LayerId::F_CU);
        BoardTheme::stack(scene.layers.iter().map(|(layer, _)| *layer)).into_iter().rev()
            .map(|layer| RailLayer {
                layer,
                name: layer.name(),
                ink: theme.ink(layer),
                visible: self.layer_shown(layer),
                active: layer == active,
                count: scene.layer(layer).map_or(0, |content| content.shapes.len() + content.polys.len()),
            }).collect()
    }

    pub fn set_active_layer(&mut self, cx: &mut Cx, layer: LayerId) {
        self.st.active = Some(layer);
        self.redraw(cx);
    }

    /// The placed via's pad diameter in nm, from the pane or the default net class.
    pub(crate) fn via_diameter(&self, cx: &mut Cx, unit: BoardId) -> Nm {
        self.st.via_d.unwrap_or_else(|| self.st.bar.default_via(cx, unit))
    }

    pub fn set_layer_visible(&mut self, cx: &mut Cx, layer: LayerId, visible: bool) {
        self.st.solo = None;
        self.st.hidden.retain(|id| *id != layer);
        self.st.shown.retain(|id| *id != layer);
        if visible {
            self.st.shown.push(layer);
        } else {
            self.st.hidden.push(layer);
        }
        self.redraw(cx);
    }

    /// Solo leaves the saved visibility choices intact; soloing the same layer restores them.
    pub fn solo_layer(&mut self, cx: &mut Cx, layer: LayerId) {
        self.st.solo = if self.st.solo == Some(layer) { None } else { Some(layer) };
        self.redraw(cx);
    }

    /// Sweeps the camera in a loop and redraws every frame, with GPU timing on.
    pub fn set_autopan(&mut self, cx: &mut Cx, on: bool) {
        self.st.autopan = if on { Some(Cx::monotonic_now()) } else { None };
        self.st.timing = on;
        if on {
            self.st.next_frame = cx.new_next_frame();
        }
        self.redraw(cx);
    }

    /// Per-layer offscreen targets (true, the default) or every layer straight into the pane.
    pub fn set_composite(&mut self, cx: &mut Cx, on: bool) {
        self.st.direct = !on;
        for slot in &mut self.st.layers {
            for l in [&mut slot.tris, &mut slot.strokes, &mut slot.pads].into_iter().flatten() {
                l.invalidate();
            }
        }
        self.redraw(cx);
    }

    pub fn stats(&self) -> BoardStats {
        self.st.stats.clone()
    }

    /// The stats, and a fresh window for the mean, the max and the GPU means.
    pub fn take_stats(&mut self) -> BoardStats {
        let mut s = self.st.stats.clone();
        s.draw_us_mean = mean(&self.st.window);
        s.draw_us_max = self.st.window.iter().copied().fold(0.0, f64::max);
        s.gpu_ms = self
            .st
            .layers
            .iter()
            .filter(|l| !l.gpu.is_empty())
            .map(|l| (l.layer.name(), mean(&l.gpu)))
            .collect();
        if !self.st.main_gpu.is_empty() {
            s.gpu_ms.push(("window".into(), mean(&self.st.main_gpu)));
        }
        self.st.window.clear();
        self.st.main_gpu.clear();
        for l in &mut self.st.layers {
            l.gpu.clear();
        }
        s
    }

    /// The scene drawn now, if any.
    pub fn scene(&self) -> Option<&Arc<DrawScene>> {
        self.st.drawn.as_ref().map(|d| &d.geom.scene)
    }

    fn light(&self) -> bool {
        // The theme has no dark/light flag (darkness is a property of the worn sheet, and a dark
        // sheet need not be another's dark variant), so the ground's Rec. 709 luminance decides.
        let g = self.ground;
        self.st.light.unwrap_or(0.2126 * g.x + 0.7152 * g.y + 0.0722 * g.z > 0.5)
    }

    fn theme(&self) -> BoardTheme {
        BoardTheme::for_light(self.light())
    }

    /// Takes the board's newest shared drawing: a stream whose publication is unchanged keeps
    /// its recorded list; a layer keeps its target across pictures. O(streams).
    fn sync(&mut self, cx: &mut Cx) {
        self.follow(cx);
        let Some(unit) = self.st.unit else { return };
        let Some(draw) = BoardDraws::current(cx, unit) else { return };
        if self.st.drawn.as_ref().is_some_and(|d| Arc::ptr_eq(d, &draw)) {
            return;
        }
        // While a gesture lands only the picture that ends the landing is taken: nothing on screen
        // goes back to the committed state for the evaluation of a commit the host cannot carry.
        if self.st.drawn.is_some() && !self.takes_picture(cx, &draw.board) {
            return;
        }
        let new_picture = self.st.drawn.as_ref().map_or(true, |d| d.gen != draw.gen || d.unit != draw.unit);
        let mut old = std::mem::take(&mut self.st.layers);
        let mut slots = Vec::with_capacity(draw.pubs.layers.len());
        for lp in &draw.pubs.layers {
            let mut slot = match old.iter().position(|s| s.layer == lp.layer) {
                Some(i) => old.swap_remove(i),
                None => LayerSlot { layer: lp.layer, tris: None, strokes: None, pads: None, target: None, gpu: Vec::new() },
            };
            StreamList::sync(&mut slot.tris, &lp.tris);
            StreamList::sync(&mut slot.strokes, &lp.strokes);
            StreamList::sync(&mut slot.pads, &lp.pads);
            slots.push(slot);
        }
        for mut gone in old {
            if let Some(t) = gone.target.as_mut() {
                t.release(cx);
            }
        }
        self.st.layers = slots;
        StreamList::sync(&mut self.st.substrate, &draw.pubs.substrate);
        StreamList::sync(&mut self.st.glows, &draw.pubs.glows);
        StreamList::sync(&mut self.st.holes, &draw.pubs.holes);
        self.st.stats.instances = draw.batch.instances() + draw.pubs.glows.as_ref().map_or(0, |p| p.count());
        self.st.stats.scene_ms = draw.geom.ms;
        self.st.stats.batch_ms = draw.timing.picture_ms;
        if new_picture {
            self.st.awaiting = Some(draw.timing.requested);
            self.st.stats.incremental = draw.timing.incremental;
            self.st.stats.edited = draw.timing.edited;
            log!(
                "fab.board: picture {} in: {} layers, {} instances, {} ({} items), worker {:.1} ms",
                draw.gen,
                self.st.layers.len(),
                self.st.stats.instances,
                if draw.timing.incremental { "rebatched" } else { "full build" },
                draw.timing.edited,
                draw.timing.picture_ms
            );
        }
        if let Some(old) = self.st.drawn.replace(draw) {
            BoardDraws::retire(cx, old);
        }
        self.selection_summary();
    }

    fn layer_shown(&self, l: LayerId) -> bool {
        if let Some(solo) = self.st.solo {
            return l == solo || l == LayerId::EDGE;
        }
        if self.st.shown.contains(&l) {
            return true;
        }
        !self.st.hidden.contains(&l) && BoardTheme::shown_by_default(l)
    }

    /// Visible layers, topmost first (the pick order).
    /// The probe name and channel label of the copper at world point `at` on the drawn board
    /// (pads, tracks and vias over pours, topmost visible layer first). O(hits at the point).
    /// `design` names a pad by its circuit pin; without it a pad is labelled by its net.
    fn probe_at(&self, design: Option<&Design>, at: [f64; 2]) -> Option<(String, String)> {
        let draw = self.st.drawn.as_ref()?;
        let order = self.pick_order();
        let geom = &draw.geom;
        let items = BoardItems { unit: geom.board.unit, board: &geom.board, scene: &geom.scene, pick: &geom.pick, items: &geom.items, order: &order };
        let hit = items.probe_at(at, HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12))?;
        let comps = design.and_then(|design| design.unit(geom.board.unit)).map_or(&[][..], |unit| &unit.circuit.comps[..]);
        probe_names(&geom.board, comps, hit)
    }

    /// The probe seam (live::probe). With a bench (the one a host names, else the one the
    /// electronics host runs, `host_bench`): a probe dragged over copper takes the copy
    /// and names the net to every pane; a drop, or P over copper with the key focus, adds the
    /// probe to the bench as one op. Without one: no copy, no drop, P is silent, and the net
    /// under a probe here only lights. Either way the net another pane names lights here too.
    fn handle_probe(&mut self, cx: &mut Cx, event: &Event) {
        let drag = match event {
            Event::Drag(drag) => carries_probe(&drag.items).then_some(drag.abs),
            Event::Drop(drop) => carries_probe(&drop.items).then_some(drop.abs),
            _ => None,
        };
        let key = matches!(event, Event::KeyDown(k) if k.key_code == KeyCode::KeyP);
        if drag.is_none() && !key && !matches!(event, Event::DragEnd) {
            return;
        }
        let mut local = None;
        let design = self.design(cx);
        // Probes go to the bench a host names, else to the one the electronics host runs.
        if let Some(bench) = self.st.bench.clone().or_else(|| host_bench(cx)) {
            let cam = self.pane.cam;
            let mut label = None;
            let mut over = None;
            let dropped = probe_target(cx, event, self.draw_bg.area(), self.uid, |abs| {
                if matches!(event, Event::Drag(_)) { over = Some(abs); }
                let (net, channel) = self.probe_at(design.as_deref(), cam.to_world(abs))?;
                label = Some(channel);
                Some(net)
            });
            // A probe carried over copper shows the flag its drop would make there.
            let ghost = over.and_then(|abs| self.board_ghost(design.as_deref(), abs));
            if ghost != self.st.flag_ghost {
                self.st.flag_ghost = ghost;
                self.redraw(cx);
            }
            let probe = match dropped {
                Some(net) => {
                    let label = label.unwrap_or_else(|| net.clone());
                    Some((net, label))
                }
                None if key => {
                    let under = self.pane.pointer_world().and_then(|at| self.probe_at(design.as_deref(), at));
                    probe_key(cx, event, self.draw_bg.area(), under.as_ref().map(|(net, _)| net.as_str()))
                        .and_then(|net| under.filter(|(name, _)| *name == net))
                }
                None => None,
            };
            if let Some((net, label)) = probe {
                log!("fab.board: probe {net} ({label}) to bench {bench}");
                cx.widget_action(self.uid, ViewAction::Ops {
                    place: vec![],
                    label: format!("Probe {label}"),
                    ops: vec![DesignOp::Bench { bench, op: BenchOp::AddProbe { of: ProbeOf::v(net), label } }],
                });
            }
        } else if let (Some(abs), Event::Drag(_)) = (drag, event) {
            // No bench: the copper under the probe lights as a plain highlight, nothing more.
            if self.draw_bg.area().rect(cx).contains(abs) {
                local = self.probe_at(design.as_deref(), self.pane.cam.to_world(abs)).map(|(net, _)| net);
            }
        }
        let name = local.as_deref().or_else(|| cx.global::<ProbeGesture>().hovered()).map(str::to_owned);
        self.light_net(cx, name);
    }

    /// Light the net named `name` (resolved against the drawn board once per change).
    fn light_net(&mut self, cx: &mut Cx, name: Option<String>) {
        if self.st.lit.as_ref().map(|(lit, _)| lit) == name.as_ref() {
            return;
        }
        self.st.lit = name.map(|name| {
            let net = self.st.drawn.as_ref().and_then(|draw| {
                draw.board.nets.iter().position(|net| net.name == name).map(|ix| NetId(ix as u32))
            });
            log!("fab.board: net {name} lit ({})", if net.is_some() { "on this board" } else { "not on this board" });
            (name, net)
        });
        self.redraw(cx);
    }

    fn pick_order(&self) -> Vec<LayerId> {
        let mut order: Vec<LayerId> = BoardTheme::stack(self.st.layers.iter().map(|s| s.layer)).into_iter().filter(|l| self.layer_shown(*l)).collect();
        order.reverse();
        order
    }

    /// Resolve a semantic part against the geometry actually drawn. Presses use the same input
    /// path as pointer clicks; scrolling replaces all camera motion in one frame.
    fn semantic_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let Some(draw) = self.st.drawn.clone() else { return ActionResult::Unhandled };
        let geom = &draw.geom;
        let Some(&item) = geom.names.keys.get(&key) else { return ActionResult::Unhandled };
        if request.action != Actions::PRESS && request.action != Actions::SCROLL_INTO_VIEW {
            return ActionResult::Unhandled;
        }
        let order = self.pick_order();
        let items = BoardItems { unit: geom.board.unit, board: &geom.board, scene: &geom.scene,
            pick: &geom.pick, items: &geom.items, order: &order };
        let bounds = items.bounds(item);
        let center = |b: [f64; 4]| [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5];
        if request.action == Actions::PRESS {
            let Some(at) = items.press_point(item, &self.pane.cam).or_else(|| bounds.map(center)) else {
                return ActionResult::Unhandled;
            };
            cx.set_key_focus(self.draw_bg.area());
            self.input(cx, CanvasInput::Click {
                key: Some(Sel::Item(geom.board.unit, item)), at, clicks: 1, shift: false,
            });
        } else {
            let Some(bounds) = bounds else { return ActionResult::Unhandled };
            let cam = self.pane.cam;
            let mut to = focus_camera(&cam, bounds);
            // A degenerate width gets one nm, so even a point has a finite zoom target.
            let width = (bounds[2] - bounds[0]).max(1e-6);
            if to == cam && width * cam.px_per_unit < 4.0 * HIT_RADIUS && items.press_point(item, &cam).is_none() {
                to.center = center(bounds);
                to.px_per_unit = 4.0 * HIT_RADIUS / width;
            }
            self.st.glide = None;
            self.st.autopan = None;
            self.pane.nav.stop();
            self.pane.cam = to;
            self.redraw(cx);
        }
        ActionResult::Handled
    }


    /// Over the layers: the ratsnest, then a drag's ghost and the held item at the cursor.
    fn draw_drag(&mut self, cx: &mut Cx2d, theme: &BoardTheme, active: LayerId, base: &BoardUniforms) {
        let Some(t) = BoardDraws::templates(cx) else { return };
        let conn = self.conn(cx);
        let held: Vec<ItemId> = self.st.hold().iter().map(|h| h.item).collect();
        let by = self.st.hold().map_or(P::new(0, 0), |h| h.by);
        let committed = self.st.hold().map(|h| h.board.clone());
        let at = |c: ConnItem| -> Option<P> {
            let board = committed.as_ref()?;
            match c {
                ConnItem::Pad { placed, pad } => board.anchor_pos(Anchor::Pad { placed, pad }),
                ConnItem::Via(via) => board.anchor_pos(Anchor::Via(via)),
                _ => None,
            }
        };
        let shown = self.st.drawn.as_ref().map(|d| d.board.clone());
        self.st.rats.sync(conn.as_ref(), shown.as_ref(), &held, &at, &t.stroke);
        let rats = BoardUniforms { thin_alpha: 1.0, ..*base };
        self.st.rats.draw(cx, &mut self.draw_stroke.draw_vars, &rats, theme.rats, by, &t.stroke);
        let cam = self.pane.cam;
        let visible: Vec<LayerId> = self.st.layers.iter().map(|s| s.layer).filter(|l| self.layer_shown(*l)).collect();
        let shown = |l: LayerId| visible.contains(&l);
        let landing = self.st.hold().and_then(|h| h.landing.as_ref());
        let settling = landing.is_some();
        let pending = landing.is_some_and(|landing| Cx::monotonic_now() - landing.since >= STALE_SECONDS);
        if settling && !pending && !cx.next_frame_is_pending(self.st.next_frame) {
            self.st.next_frame = cx.new_next_frame();
        }
        let Some(held) = self.st.held.as_mut() else { return };
        if !settling {
            let vars = HeldVars { stroke: &mut self.draw_stroke.draw_vars, pad: &mut self.draw_pad.draw_vars, tri: &mut self.draw_tri.draw_vars };
            held.draw_ghost(cx, vars, theme, active, &shown, base, &cam, HeldLook { alpha: 0.34, desaturate: 1.0, pending: false });
        }
        let vars = HeldVars { stroke: &mut self.draw_stroke.draw_vars, pad: &mut self.draw_pad.draw_vars, tri: &mut self.draw_tri.draw_vars };
        let look = HeldLook { alpha: if settling { 1.0 } else { 0.8 }, desaturate: 0.0, pending };
        held.draw_held(cx, vars, theme, active, &shown, base, &cam, by, look);
    }

    /// One canvas input (canvas::pane's grammar).
    fn input(&mut self, cx: &mut Cx, input: CanvasInput<Sel>) {
        match input {
            CanvasInput::Hover(sel) => {
                self.st.hover = match sel {
                    Some(Sel::Item(_, item)) => Some(item),
                    _ => None,
                };
                self.st.hover_pour = self.st.hover.zip(self.st.drawn.as_ref())
                    .is_some_and(|(item, draw)| matches!(draw.geom.board.locate(item), Some((ItemKind::Zone, _))));
                self.clear_note(cx);
                self.redraw(cx);
            }
            // Keys while copper is drawn adjust it (V, W, /); S flips Shove; the rest act on what
            // is hovered.
            CanvasInput::Key { code, modifiers, .. } if self.route_key(cx, code, modifiers) => {}
            CanvasInput::Key { code, modifiers, under } if !(modifiers.shift || modifiers.control || modifiers.alt || modifiers.logo) => match code {
                KeyCode::Delete | KeyCode::Backspace => self.key_delete(cx, under),
                KeyCode::KeyF => self.key_pour(cx),
                KeyCode::KeyA => self.key_labels(cx),
                KeyCode::KeyS => self.flip_shove(cx),
                // The strip's tools: X arms Route, V goes back to Select.
                KeyCode::KeyX => self.set_armed(cx, self.st.armed.key(BoardTool::Route)),
                KeyCode::KeyV => self.set_armed(cx, self.st.armed.key(BoardTool::Select)),
                _ => {}
            },
            // Escape with no drag running: away with the ruler, and out of the armed tool.
            CanvasInput::Escape if self.st.armed != Armed::default() || self.st.ruler.is_some() => {
                self.st.ruler = None;
                self.st.zone_box = None;
                self.set_armed(cx, self.st.armed.escape());
                self.redraw(cx);
            }
            // Via places where it is clicked; Zone and Measure act on drags only.
            CanvasInput::Click { at, .. } if self.st.armed.tool == BoardTool::Via => self.place_via(cx, at),
            CanvasInput::Click { .. } if matches!(self.st.armed.tool, BoardTool::Zone | BoardTool::Measure) => {}
            CanvasInput::DragStart { from, at, .. } if self.tool_drags() => {
                self.st.press_at = Some(from);
                self.tool_drag(cx, at, false);
            }
            CanvasInput::DragMove { at, .. } if self.tool_drags() => self.tool_drag(cx, at, false),
            CanvasInput::DragEnd { at, .. } if self.tool_drags() => self.tool_drag(cx, at, true),
            CanvasInput::Marquee { rect, done, .. } if self.tool_drags() => {
                // the marquee's box has the press at one corner: the drag's end is the opposite one
                let Some(from) = self.st.press_at else { return };
                let far = |v: f64, lo: f64, hi: f64| if (v - lo).abs() <= (v - hi).abs() { hi } else { lo };
                self.tool_drag(cx, [far(from[0], rect[0], rect[2]), far(from[1], rect[1], rect[3])], done);
            }
            CanvasInput::Click { key: Some(sel @ Sel::Item(unit, item)), at, .. } => {
                let placed = self.st.drawn.as_ref().and_then(|d| match d.board.locate(item) {
                    Some((ItemKind::Placed, i)) => d.board.placed.get(i).map(|p| p.comp),
                    _ => None,
                });
                let kind = if placed.is_some() { FocusKind::Part } else { FocusKind::Board };
                self.set_selection(cx, std::slice::from_ref(&sel));
                cx.widget_action(self.uid, ViewAction::Focus(Focus { kind, sel, origin: FocusOrigin::Board }));
                // While a run is made, an output jack clicked is heard (or no longer heard)
                if placed.is_some_and(|comp| crate::live::listen::click_jack(cx, fab_elec_core::CompRef { board: unit, comp })) {
                    self.redraw(cx);
                    return;
                }
                // While a run is live, copper clicked also brings its net's lane into view in the
                // Scope, the Board staying in front.
                if cx.get_global_ref::<ElecLive>().is_some_and(|live| live.sim.runs()) {
                    let design = self.design(cx);
                    if let Some((net, _)) = self.probe_at(design.as_deref(), at) {
                        probe_on_click(cx, &net);
                    }
                }
            }
            // A press while a gesture lands waits for the landed picture (it must not start on
            // geometry about to be replaced).
            CanvasInput::DragStart { key: Sel::Item(unit, _), from, at } if self.st.landing().is_some() => {
                self.st.pending = Some(Pending { unit, from, at });
            }
            CanvasInput::DragStart { key: Sel::Item(unit, _), from, at } => self.begin_drag(cx, unit, from, at),
            CanvasInput::DragMove { at, .. } => {
                if let Some(pending) = self.st.pending.as_mut() {
                    pending.at = at;
                    self.start_pending(cx);
                    return;
                }
                match self.st.drag {
                    Some(Drag::Route(_)) => self.move_route(cx, at, false),
                    _ => self.move_hold(cx, at),
                }
            }
            CanvasInput::DragEnd { at, .. } => {
                if self.st.pending.is_some() {
                    self.start_pending(cx);
                    // Released before the landing ended: the press never started.
                    if self.st.pending.take().is_some() {
                        return;
                    }
                }
                if matches!(self.st.drag, Some(Drag::Route(_))) {
                    self.move_route(cx, at, true);
                    self.set_armed(cx, self.st.armed.done());
                    return;
                }
                self.move_hold(cx, at);
                let landing = Self::release_landing();
                let Some(hold) = self.st.hold_mut() else { return };
                // The worker's drag commits its answer to the last sample (`hold_poll`).
                if let Some(routing) = hold.routing.as_mut() {
                    if routing.release_held() {
                        hold.landing = Some(Landing { tag: routing.id.tag(), ..landing });
                        self.hold_poll(cx);
                    } else {
                        self.end_drag(Some(cx));
                    }
                    return;
                }
                match (hold.by != P::new(0, 0), hold.tag) {
                    (true, Some(tag)) => {
                        // The labels the drop disturbs move in the same undo step: a last preview
                        // carries them, so the commit is the move and its labels at once.
                        let moved = [DesignOp::Board { unit: hold.unit, op: BoardOp::Move { item: hold.item, by: hold.by } }];
                        let labels = plan::label_ops(hold.unit, &hold.board, &moved, &[hold.item]);
                        let tag = if labels.is_empty() {
                            tag
                        } else {
                            let tag = next_tag();
                            hold.tag = Some(tag);
                            let ops = moved.into_iter().chain(labels).collect();
                            cx.widget_action(self.uid, ViewAction::Preview { notes: vec![], tag, place: vec![], ops, label: "Move".into() });
                            tag
                        };
                        // The held copy stays at the drop until the drop is on screen.
                        hold.landing = Some(Landing { tag, sent: true, ..landing });
                        cx.widget_action(self.uid, ViewAction::CommitPreview);
                    }
                    _ => self.end_drag(Some(cx)),
                }
            }
            // Escape while a gesture lands does nothing: its commit is sent, and undo answers it.
            CanvasInput::DragCancel { .. } if self.st.landing().is_some() => self.st.pending = None,
            CanvasInput::DragCancel { .. } => self.end_drag(Some(cx)),
            CanvasInput::Camera => {
                self.st.glide = None;
                self.redraw(cx);
            }
            _ => {}
        }
    }

    /// Delete or Backspace: delete exactly the hovered item, one undo step at the press, and fade
    /// it out once the picture lacks it; or say why not.
    fn key_delete(&mut self, cx: &mut Cx, under: Option<Sel>) {
        let Some(design) = self.design(cx) else { return };
        let target = match under {
            Some(Sel::Item(unit, item)) => Some((unit, item)),
            _ => None,
        };
        let planned = match target {
            Some((unit, item)) => plan::delete(&design, unit, item),
            None => Err("nothing to delete here".to_owned()),
        };
        match planned {
            Ok(plan) => {
                let (unit, item) = target.unwrap();
                self.clear_note(cx);
                self.st.gone = self.st.drawn.as_ref().filter(|draw| draw.unit == unit).zip(BoardDraws::templates(cx)).map(|(draw, t)| {
                    let held = HeldDraw::new(&draw.board, &draw.fills, &[item], &t);
                    Gone { unit, item, held, start: None }
                });
                log!("fab.board: {}", plan.label);
                cx.widget_action(self.uid, ViewAction::Ops { ops: plan.ops, place: vec![], label: plan.label });
            }
            Err(why) => self.note(cx, ChipKind::Refused, why, Vec::new()),
        }
    }

    /// A (arrange): every reference label beside its part, clear of pads, parts and other labels.
    /// Fab takes bare L for the sliced view. The plan is made on the task pool (a large board takes
    /// a while) and commits in `poll_arrange`; a press while one is being made adds nothing.
    fn key_labels(&mut self, cx: &mut Cx) {
        if self.st.arranging.is_some() {
            return;
        }
        let Some(design) = self.design(cx) else { return };
        let Some(unit) = self.st.drawn.as_ref().map(|d| d.unit) else { return };
        let read = design.clone();
        match cx.task_pool().submit(Class::Interactive, move || plan::arrange_labels(&read, unit)) {
            Ok(job) => self.st.arranging = Some(Arranging { unit, design, job }),
            Err(e) => log!("fab.board: the label plan was not queued ({e:?})"),
        }
    }

    /// A finished label plan commits only onto the design it was made from: after an edit it is
    /// made again on the new design, never applied to a changed board.
    fn poll_arrange(&mut self, cx: &mut Cx) {
        let Some(result) = self.st.arranging.as_mut().and_then(|a| a.job.try_take()) else { return };
        let Arranging { unit, design, .. } = self.st.arranging.take().expect("a job was taken");
        let now = self.design(cx);
        let fate = arrange_fate(&design, unit, now.as_ref(), self.st.drawn.as_ref().map(|d| d.unit));
        // The designs may hold a board's last references: freed on the pool.
        BoardDraws::discard(cx, (design, now));
        match (fate, result) {
            (ArrangeFate::Commit, Ok(Ok(plan))) => {
                log!("fab.board: {}", plan.label);
                cx.widget_action(self.uid, ViewAction::Ops { ops: plan.ops, place: vec![], label: plan.label });
                self.clear_note(cx);
            }
            (ArrangeFate::Commit, Ok(Err(text))) => self.note(cx, ChipKind::Info, text, Vec::new()),
            (ArrangeFate::Again, _) => {
                log!("fab.board: the board changed while its labels were planned; planning again");
                self.key_labels(cx);
            }
            (ArrangeFate::Drop, _) => {}
            (ArrangeFate::Commit, Err(e)) => log!("fab.board: the label plan failed: {e:?}"),
        }
    }

    /// F: pour the net under the pointer (else the circuit's ground) over the whole board on the
    /// active copper layer (top copper while a non-copper layer is active); or say why not.
    fn key_pour(&mut self, cx: &mut Cx) {
        let Some(draw) = self.st.drawn.clone() else { return };
        let net = self.pane.pointer_world().and_then(|at| self.net_at(&draw, at));
        self.pour(cx, net);
    }

    /// The net of the copper at world `at` on the drawn board, topmost first. O(hits).
    fn net_at(&self, draw: &BoardDraw, at: [f64; 2]) -> Option<NetId> {
        let order = self.pick_order();
        let geom = &draw.geom;
        let items = BoardItems { unit: draw.unit, board: &geom.board, scene: &geom.scene, pick: &geom.pick, items: &geom.items, order: &order };
        items.probe_at(at, HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12)).map(|hit| hit.net)
    }

    /// Pour `net` (else the circuit's ground) over the whole board on the active copper layer: F
    /// with the net under the pointer, the strip's Fill with none. One undo step, or a chip saying
    /// why not.
    fn pour(&mut self, cx: &mut Cx, net: Option<NetId>) {
        let Some(design) = self.design(cx) else { return };
        let Some(draw) = self.st.drawn.clone() else { return };
        let unit = draw.unit;
        let layer = self.st.active.filter(|l| draw.board.copper_set().has(*l)).unwrap_or(LayerId::F_CU);
        match plan::pour(&design, unit, net, layer) {
            Ok(plan) => {
                // Zones the pour raised: the user did not point at them, so the picture marks
                // them and a note says what happened. One undo step takes them back with the pour.
                let raised: Vec<ItemId> = plan.ops.iter().filter_map(|op| match op {
                    DesignOp::Board { op: BoardOp::SetField { item, field: Field::Priority(_) }, .. } => Some(*item),
                    _ => None,
                }).collect();
                log!("fab.board: {} ({} zones raised)", plan.label, raised.len());
                cx.widget_action(self.uid, ViewAction::Ops { ops: plan.ops, place: vec![], label: plan.label });
                match raised.len() {
                    0 => self.clear_note(cx),
                    1 => self.note(cx, ChipKind::Info, "Raised 1 zone's priority: the pour stays below it".to_owned(), raised),
                    n => self.note(cx, ChipKind::Info, format!("Raised {n} zones' priority: the pour stays below them"), raised),
                }
            }
            Err(why) => self.note(cx, ChipKind::Refused, why, Vec::new()),
        }
    }

    /// A key's chip beside the pointer until the hover moves or 3 s pass: why it could not act
    /// (`Refused`), or what it changed besides its target (`Info`), those `marked` items lifted in
    /// the picture meanwhile.
    fn note(&mut self, cx: &mut Cx, kind: ChipKind, text: String, marked: Vec<ItemId>) {
        self.clear_note(cx);
        if kind == ChipKind::Refused {
            log!("fab.board: refused: {text}");
        }
        let at = self.pane.pointer_world().unwrap_or(self.pane.cam.center);
        let chips = match &mut self.st.chips {
            Some(chips) => chips,
            None => self.st.chips.insert(Chips { layer: ChipLayer::default(), live: CanvasLive::off(cx), shown: false, timer: Timer::empty() }),
        };
        chips.layer.set(vec![Chip { id: 0, kind, text, anchor: ChipAnchor::World { at, dx: 14.0, dy: 14.0 }, align: [0.0, 0.0] }], cx.seconds_since_app_start());
        chips.shown = true;
        chips.timer = cx.start_timeout(NOTE_SECONDS);
        if !marked.is_empty() && self.st.hold().is_none() {
            if let Some(count) = self.st.drawn.as_ref().map(|draw| draw.board.next_item as usize) {
                let rows = marked.iter().map(|item| (item.0, [0.0, 1.0, 0.0, 0.0])).collect();
                self.st.dims.get_or_insert_with(ItemStates::new).write(count, rows);
                self.st.marked = marked;
            }
        }
        self.redraw(cx);
    }

    fn clear_note(&mut self, cx: &mut Cx) {
        if !self.st.marked.is_empty() {
            self.st.marked.clear();
            if self.st.hold().is_none() {
                if let (Some(dims), Some(count)) = (self.st.dims.as_mut(), self.st.drawn.as_ref().map(|draw| draw.board.next_item as usize)) {
                    dims.write(count, Vec::new());
                }
            }
        }
        let Some(chips) = self.st.chips.as_mut().filter(|chips| chips.shown) else { return };
        chips.shown = false;
        chips.layer.set(Vec::new(), cx.seconds_since_app_start());
        cx.stop_timer(chips.timer);
        self.redraw(cx);
    }

    /// The Measure ruler with its length beside its end, or the Zone rectangle being dragged: one
    /// quad over the pane, shaped by uniforms. O(1) per frame.
    /// Copper being drawn whose newest sample has waited `STALE_SECONDS` for its answer: a straight
    /// line from the head's start to the cursor, in mm. O(1).
    fn rubber(&self) -> Option<[[f64; 2]; 2]> {
        let route = self.st.drag.as_ref().and_then(|drag| match drag {
            Drag::Route(route) if route.landing.is_none() && route.routing.stale() => Some(route),
            _ => None,
        })?;
        let (_, since) = route.sent.front()?;
        if Cx::monotonic_now() - since < STALE_SECONDS {
            return None;
        }
        let mm = |p: P| [p.x as f64 / 1e6, p.y as f64 / 1e6];
        Some([mm(route.routing.head_start()), mm(route.routing.g.cursor)])
    }

    fn draw_tool_marks(&mut self, cx: &mut Cx2d, rect: Rect) {
        let cam = self.pane.cam;
        let local = |w: [f64; 2]| {
            let s = cam.to_screen(w) - rect.pos;
            [s.x as f32, s.y as f32]
        };
        let ink = Tokens::for_theme(!self.light()).hover;
        let (seg, zone, label) = if let Some([from, to]) = self.st.ruler {
            let (a, b) = (local(from), local(to));
            let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
            let label = format!("{:.3} mm   dx {:.3}   dy {:.3}", dx.hypot(dy), dx, dy);
            ([a[0], a[1], b[0], b[1]], 0.0, Some((dvec2(b[0] as f64 + 10.0, b[1] as f64 + 6.0), label)))
        } else if let Some(z) = self.st.zone_box {
            let (a, b) = (local([z[0], z[1]]), local([z[2], z[3]]));
            ([a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])], 1.0, None)
        } else if let Some([from, to]) = self.rubber() {
            let (a, b) = (local(from), local(to));
            ([a[0], a[1], b[0], b[1]], 2.0, None)
        } else {
            return;
        };
        let vars = &mut self.draw_ruler.draw_vars;
        vars.set_uniform(cx, id!(seg), &seg);
        vars.set_uniform(cx, id!(ink), &ink);
        vars.set_uniform(cx, id!(zone), &[zone]);
        self.draw_ruler.draw_abs(cx, rect);
        if let Some((at, label)) = label {
            let color = self.draw_text.color;
            self.draw_text.color = vec4(ink[0], ink[1], ink[2], ink[3]);
            self.draw_text.draw_abs(cx, rect.pos + at, &label);
            self.draw_text.color = color;
        }
    }

    /// The deleted item's fading copy and the refusal chip, over everything else.
    fn draw_keys(&mut self, cx: &mut Cx2d, theme: &BoardTheme, active: LayerId, base: &BoardUniforms, rect: Rect) {
        let now = cx.seconds_since_app_start();
        let mut animating = false;
        let lacks = |gone: &Gone, draw: &Option<Arc<BoardDraw>>| draw.as_ref().is_some_and(|d| d.unit == gone.unit && d.board.locate(gone.item).is_none());
        if self.st.gone.as_ref().is_some_and(|gone| gone.start.is_none() && lacks(gone, &self.st.drawn)) {
            self.st.gone.as_mut().unwrap().start = Some(now);
        }
        if let Some(start) = self.st.gone.as_ref().and_then(|gone| gone.start) {
            let fade = 1.0 - ((now - start) / STATE_SECONDS).clamp(0.0, 1.0);
            if fade <= 0.0 {
                self.st.gone = None;
            } else {
                let cam = self.pane.cam;
                let visible: Vec<LayerId> = self.st.layers.iter().map(|s| s.layer).filter(|l| self.layer_shown(*l)).collect();
                let shown = |l: LayerId| visible.contains(&l);
                let gone = self.st.gone.as_mut().unwrap();
                let vars = HeldVars { stroke: &mut self.draw_stroke.draw_vars, pad: &mut self.draw_pad.draw_vars, tri: &mut self.draw_tri.draw_vars };
                gone.held.draw_ghost(cx, vars, theme, active, &shown, base, &cam, HeldLook { alpha: fade as f32, desaturate: 0.0, pending: false });
                animating = true;
            }
        }
        let dark = !self.light();
        if let Some(chips) = self.st.chips.as_mut() {
            let clip = [rect.pos.x as f32, rect.pos.y as f32, (rect.pos.x + rect.size.x) as f32, (rect.pos.y + rect.size.y) as f32];
            chips.layer.draw(cx, &self.pane.cam, clip, Tokens::for_theme(dark), &chips.live, &mut self.draw_line, &mut self.draw_box, &mut self.draw_text, now);
            animating |= chips.layer.animating(now);
        }
        if animating && !cx.next_frame_is_pending(self.st.next_frame) {
            self.st.next_frame = cx.new_next_frame();
        }
    }

    /// A press on an item becomes a gesture. What it starts is decided once, from what the press
    /// takes on the picture on screen, in the pane's own pick order (`BoardItems::pressed`,
    /// `gesture::classify`): O(hits).
    fn begin_drag(&mut self, cx: &mut Cx, unit: BoardId, from: [f64; 2], at: [f64; 2]) {
        let Some(draw) = self.st.drawn.clone() else { return };
        let (p, tol) = (nm(from), self.pick_nm());
        let order = self.pick_order();
        let geom = &draw.geom;
        let items = BoardItems { unit, board: &geom.board, scene: &geom.scene, pick: &geom.pick, items: &geom.items, order: &order };
        let pressed = items.pressed(from, HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12));
        let active = self.st.active.unwrap_or(LayerId::F_CU);
        let routes = cx.has_global::<ElecLive>();
        // Armed Route draws copper from a pad or a track's end, and nothing else starts.
        let route_tool = self.st.armed.tool == BoardTool::Route;
        let press = classify(&geom.board, &pressed, p, tol, active, route_tool);
        if route_tool && !matches!(press, Press::Route { .. }) {
            return;
        }
        match press {
            Press::Hold(item) => {
                self.begin_hold(cx, unit, item, from);
                self.move_hold(cx, at);
            }
            // Otherwise a pad moves its part: a small part is mostly pads, and copper from a pad
            // is the Route tool's (as it is in a host that answers no gestures, no `ElecLive`).
            Press::Route { start: Anchor::Pad { placed, .. }, .. } if !(route_tool && routes) => {
                self.begin_hold(cx, unit, placed, from);
                self.move_hold(cx, at);
            }
            press @ (Press::Route { .. } | Press::Seg { .. }) if routes => self.begin_route(cx, unit, press, draw, at),
            _ => {}
        }
    }

    /// A press kept while a gesture landed starts once the landing is over, on the landed picture.
    fn start_pending(&mut self, cx: &mut Cx) {
        if self.st.landing().is_some() {
            return;
        }
        let Some(Pending { unit, from, at }) = self.st.pending.take() else { return };
        self.begin_drag(cx, unit, from, at);
    }

    /// The pick radius in nm at the current zoom: the canvas's own.
    fn pick_nm(&self) -> Nm {
        (HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12) * 1e6).round() as Nm
    }

    /// A landing starting now; its tag is set by the gesture that lands.
    fn release_landing() -> Landing {
        Landing { tag: 0, sent: false, since: Cx::monotonic_now() }
    }

    /// Copper drawing or a segment drag starts on the picture at the press, which the gesture keeps.
    fn begin_route(&mut self, cx: &mut Cx, unit: BoardId, press: Press, frozen: Arc<BoardDraw>, at: [f64; 2]) {
        self.end_drag(Some(&mut *cx));
        let Some(mut routing) = Routing::start(&press, &frozen.geom.board, unit) else { return };
        if let (Press::Route { .. }, Some(w)) = (press, self.st.track_w) {
            routing.set_width(w);
        }
        // Drawn copper pushes or walks as the switch says; a dragged segment pushes either way.
        if let Press::Route { .. } = press {
            routing.g.mode = self.route_mode();
        }
        log!("fab.board: gesture {} from {:?} on {:?}", routing.id.0, routing.g.start, routing.g.layer);
        self.st.drag = Some(Drag::Route(Route {
            routing, frozen, seen: None, landing: None, sent: VecDeque::new(), waits: Vec::new(), blocked: None, walked: None,
            refused: None, refusal_seq: 0, finished: None, cut: None, cut_seen: None, chip: None,
        }));
        self.move_route(cx, at, false);
    }

    /// The pointer moved while copper is drawn, or was released: snap to a same-net target on the
    /// frozen picture, then one sample to the route worker. O(hits + the trail since the last leg).
    fn move_route(&mut self, cx: &mut Cx, at: [f64; 2], release: bool) {
        let tol = self.pick_nm();
        // The trail keeps a point that bends by more than half a screen pixel.
        let trail = (0.5 / self.pane.cam.px_per_unit.max(1e-12) * 1e6).round() as Nm;
        let order = self.pick_order();
        let landing = release.then(Self::release_landing);
        let uid = self.uid;
        let Some(route) = self.st.route_mut().filter(|r| r.landing.is_none()) else { return };
        // Finishing: the router lays the rest from where the head was; the pointer no longer moves
        // it, and the release commits the Finish once it is in.
        if route.routing.g.finish {
            if let Some(landing) = landing {
                route.routing.release_held();
                route.landing = Some(Landing { tag: route.routing.id.tag(), ..landing });
            }
            return;
        }
        let p = nm(at);
        let geom = &route.frozen.geom;
        let target = if route.routing.draws() {
            snap_target(&geom.board, &geom.pick.pick_all(&geom.scene, p, tol, &order), &route.routing.g)
        } else {
            None
        };
        let sample = if release { route.routing.release(p, trail, target) } else { route.routing.moved(p, trail, target) };
        route.sent(sample.seq);
        cx.widget_action(uid, ViewAction::Gesture(sample));
        if let Some(landing) = landing {
            route.landing = Some(Landing { tag: route.routing.id.tag(), ..landing });
        }
        self.redraw(cx);
    }

    /// Reads the route worker's newest answer (O(1), by identity). Once the release sample is
    /// answered the host has filed that answer as the live preview, so the commit goes now; a
    /// release that drew nothing, or whose sample the host refused (`last_refusal.sample` is the
    /// gesture's tag), ends without one.
    fn route_poll(&mut self, cx: &mut Cx) {
        if !matches!(self.st.drag, Some(Drag::Route(_))) || !cx.has_global::<ElecLive>() {
            return;
        }
        let unit = self.st.unit;
        let live = cx.get_global::<ElecLive>();
        let answer = live.gesture.clone();
        let refusal = live.snap.last_refusal.clone();
        let refused = refusal.as_ref().and_then(|r| r.sample);
        // The shown preview's connectivity and the committed one, while the preview is a gesture's.
        let snap = &live.snap;
        let shown = snap.proposal.as_ref().and_then(|p| match p.source {
            ProposalSource::Gesture { id, .. } => Some(id),
            _ => None,
        });
        let conns = unit.and_then(|unit| {
            let of = |state| snap.conn.iter().find(|t| t.unit == Some(unit) && t.of == state).map(|t| t.value.clone());
            Some((of(StateOf::Committed)?, of(StateOf::Preview)?))
        });
        let uid = self.uid;
        let Some(route) = self.st.route_mut() else { return };
        // A refused sample of a released gesture ends it before any commit; its preview goes.
        if route.landing.as_ref().is_some_and(|l| !l.sent && refused == Some(l.tag)) {
            log!("fab.board: gesture {} refused before its commit", route.routing.id.0);
            self.end_drag(Some(cx));
            return;
        }
        let mut redraw = false;
        if let Some(answer) = answer.filter(|a| route.seen.as_ref().is_none_or(|seen| !Arc::ptr_eq(seen, a))) {
            // The hand part's ops, from the stretch answer the Finish follows.
            let hand = route.routing.answer.as_ref().map(|kept| kept.ops.len());
            if route.routing.offer(&answer) {
                if let ProposalSource::Gesture { seq, .. } = answer.source {
                    route.answered(seq);
                }
                // A head stopped short says why, and that F lets the router finish round it. A
                // finishing gesture says only how its Finish went. A dragged segment that could not
                // push a track names it: it walks round it, or stays where it is.
                let draws = route.routing.draws();
                let board = &route.frozen.geom.board;
                let unpushed = answer.head.as_ref().and_then(|head| head.unpushed.as_ref())
                    .map(|(net, reason)| unpushed_words(board, *net, reason));
                route.blocked = match route.routing.g.finish {
                    true => None,
                    false => answer.head.as_ref().and_then(|head| head.blocked.as_ref()).map(|b| match (&unpushed, draws) {
                        (Some(unpushed), _) => format!("{}; {unpushed}", b.summary()),
                        (None, true) => format!("{}; F finishes round it", b.summary()),
                        (None, false) => b.summary(),
                    }),
                };
                route.walked = unpushed.filter(|_| route.blocked.is_none()).map(|unpushed| format!("{unpushed}; walks round it"));
                // A Finish says why it found no way, and that it had nothing to lay when the net was
                // joined already: otherwise F would seem to do nothing. A dragged segment's Full
                // answer (its pushes) is no Finish.
                if route.routing.g.finish && matches!(answer.source, ProposalSource::Gesture { tier: Tier::Full, .. }) {
                    let net = route.routing.g.net;
                    let open = net.and_then(|net| answer.open.iter().find(|(open, _)| *open == net));
                    route.finished = match open {
                        Some((_, reason)) => Some((ChipKind::Refused, format!("Finish: {}", why_open(board, reason)))),
                        None if hand == Some(answer.ops.len()) => {
                            let name = net.and_then(|net| board.nets.get(net.ix())).map_or("the net", |n| n.name.as_str());
                            Some((ChipKind::Info, format!("Finish: {name} is joined already")))
                        }
                        None => None,
                    };
                }
                route.refused = None;
                redraw = true;
            }
            route.seen = Some(answer);
        }
        // A refusal read after the answers of this poll is taken as the newer: an answer clears it.
        let tag = route.routing.id.tag();
        if let Some(refusal) = refusal.filter(|r| r.sample == Some(tag) && r.seq != route.refusal_seq) {
            route.refusal_seq = refusal.seq;
            route.refused = Some(refusal.reason.clone());
        }
        // A cut pour is never silent (D7): read once per preview connectivity, by identity.
        match conns.filter(|_| shown == Some(route.routing.id)) {
            Some((committed, preview)) => {
                if route.cut_seen.as_ref().is_none_or(|seen| !Arc::ptr_eq(seen, &preview)) {
                    route.cut = pour_cut(&route.frozen.geom.board, &committed, &preview, route.routing.g.net);
                    route.cut_seen = Some(preview);
                }
            }
            None => {
                route.cut = None;
                route.cut_seen = None;
            }
        }
        // The chip follows when it changes: why the sample was refused, else why the head stops
        // short, else what it cuts, else the track a dragged segment walks round, else how a
        // Finish goes.
        let walked = route.walked.as_ref().map(|text| (ChipKind::Info, text.clone()));
        let want = match (route.refused.as_ref().or(route.blocked.as_ref()), &route.cut, walked.as_ref().or(route.finished.as_ref())) {
            (Some(why), _, _) => Some((ChipKind::Refused, why.clone(), Vec::new())),
            (None, Some((cut, marked)), _) => Some((ChipKind::Info, cut.clone(), marked.clone())),
            (None, None, Some((kind, text))) => Some((*kind, text.clone(), Vec::new())),
            (None, None, None) => None,
        };
        let text = want.as_ref().map(|(_, text, _)| text.clone());
        let chip = (text != route.chip).then(|| {
            route.chip = text;
            want
        });
        let ready = route.landing.as_ref().is_some_and(|l| !l.sent) && route.routing.commit_ready();
        if ready && route.routing.answer.as_ref().is_some_and(|a| a.ops.is_empty()) {
            self.end_drag(Some(cx));
            return;
        }
        if ready {
            log!("fab.board: gesture {} commits after {} samples", route.routing.id.0, route.routing.seq);
            cx.widget_action(uid, ViewAction::CommitPreview);
            cx.widget_action(uid, ViewAction::EndGesture(route.routing.id));
            if let Some(landing) = route.landing.as_mut() {
                landing.sent = true;
            }
            redraw = true;
        }
        match chip {
            Some(Some((kind, text, marked))) => self.note(cx, kind, text, marked),
            Some(None) => self.clear_note(cx),
            None => {}
        }
        if redraw {
            self.redraw(cx);
        }
    }

    /// Reads the worker's newest answer to a held part or via by identity. Each newly shown answer
    /// lifts the neighbours it pushes in the pane rows (`pushed`, O(ops)). Once the released position
    /// is answered, the commit goes with that answer as it is, or with the labels the drop
    /// disturbs in one last preview, so they move in the same undo step. An answer with no ops
    /// (back where it was), or a refused release, ends the drag without one.
    fn hold_poll(&mut self, cx: &mut Cx) {
        if !matches!(&self.st.drag, Some(Drag::Hold(Hold { routing: Some(_), .. }))) || !cx.has_global::<ElecLive>() {
            return;
        }
        let live = cx.get_global::<ElecLive>();
        let answer = live.gesture.clone();
        let refused = live.snap.last_refusal.as_ref().and_then(|r| r.sample);
        let uid = self.uid;
        let Some(hold) = self.st.hold_mut() else { return };
        if hold.landing.as_ref().is_some_and(|l| !l.sent && refused == Some(l.tag)) {
            log!("fab.board: the held item's drag refused before its commit");
            self.end_drag(Some(cx));
            return;
        }
        let Some(routing) = hold.routing.as_mut() else { return };
        // A track the drag cuts back says so (it opens to a ratsnest line), once per change.
        let mut chip = None;
        let mut dims = None;
        if let Some(answer) = answer.filter(|a| hold.seen.as_ref().is_none_or(|seen| !Arc::ptr_eq(seen, a))) {
            if routing.offer(&answer) {
                let neighbours = pushed(&hold.board, &answer.ops, &[hold.item]);
                let mut rows = Vec::with_capacity(neighbours.len() + 1);
                rows.push((hold.item.0, [1.0, 0.0, 0.0, 0.0]));
                let mut tracks = 0;
                for (id, kind) in neighbours {
                    if kind == ItemKind::Track {
                        tracks += 1;
                    }
                    if id != hold.item {
                        rows.push((id.0, [0.0, 1.0, 0.0, 0.0]));
                    }
                }
                rows.sort_unstable_by_key(|&(row, _)| row);
                rows.dedup_by_key(|(row, _)| *row);
                dims = Some((hold.board.next_item as usize, rows));
                let text = cut_words(&hold.board, &answer.open, tracks);
                if text != hold.chip {
                    hold.chip = text.clone();
                    chip = Some(text);
                }
            }
            hold.seen = Some(answer);
        }
        let ready = hold.landing.as_ref().is_some_and(|l| !l.sent) && routing.commit_ready();
        if let Some((count, rows)) = dims {
            self.st.dims.get_or_insert_with(ItemStates::new).write(count, rows);
            self.redraw(cx);
        }
        match chip {
            Some(Some(text)) => self.note(cx, ChipKind::Info, text, Vec::new()),
            Some(None) => self.clear_note(cx),
            None => {}
        }
        if !ready {
            return;
        }
        let Some(hold) = self.st.hold_mut() else { return };
        let Some(routing) = hold.routing.as_mut() else { return };
        let Some(answer) = routing.answer.as_ref().filter(|a| !a.ops.is_empty()) else {
            self.end_drag(Some(cx));
            return;
        };
        let ops: Vec<_> = answer.ops.iter().map(|op| DesignOp::Board { unit: hold.unit, op: op.clone() }).collect();
        let id = routing.id;
        let labels = plan::label_ops(hold.unit, &hold.board, &ops, &[hold.item]);
        let landing = hold.landing.as_mut().expect("a released hold has its landing");
        if labels.is_empty() {
            cx.widget_action(uid, ViewAction::CommitPreview);
            cx.widget_action(uid, ViewAction::EndGesture(id));
        } else {
            // The gesture ends first, so no late answer replaces the preview that commits.
            cx.widget_action(uid, ViewAction::EndGesture(id));
            let tag = next_tag();
            landing.tag = tag;
            hold.tag = Some(tag);
            let corners = answer.open.partition_point(|(_, why)| !matches!(why, Reason::SharpCorner { .. }));
            cx.widget_action(uid, ViewAction::Preview {
                tag, place: vec![], ops: ops.into_iter().chain(labels).collect(), label: "Move".into(),
                notes: answer.open[corners..].to_vec(),
            });
            cx.widget_action(uid, ViewAction::CommitPreview);
        }
        landing.sent = true;
        log!("fab.board: the held item's drag commits after {} samples", routing.seq);
        self.redraw(cx);
    }

    /// V (Shift+V), W (Shift+W), / and F while copper is drawn: a via to the next (previous)
    /// copper layer at the head's end, the next wider (narrower) track width, the head's bend, and
    /// Finish (the router lays the rest of the net from the head's end). Each sends one sample and
    /// the chip says what changed, or why not. False when no copper is being drawn.
    fn route_key(&mut self, cx: &mut Cx, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if modifiers.control || modifiers.alt || modifiers.logo
            || !matches!(code, KeyCode::KeyV | KeyCode::KeyW | KeyCode::Slash | KeyCode::KeyF)
        {
            return false;
        }
        if code == KeyCode::KeyF && modifiers.shift {
            return false;
        }
        if code == KeyCode::Slash && modifiers.shift {
            return false;
        }
        let widths = if code == KeyCode::KeyW { self.route_widths(cx) } else { None };
        let uid = self.uid;
        let Some(route) = self.st.route_mut().filter(|r| r.landing.is_none() && matches!(r.routing.g.kind, GestureKind::Route)) else { return false };
        let said = match code {
            // The rest of the drawn copper is the router's now: no key changes it.
            _ if route.routing.g.finish => Err("the router is finishing this track"),
            KeyCode::KeyF => route.routing.finish().map(|()| {
                let text = "Finish: the router lays the rest".to_string();
                route.blocked = None;
                route.finished = Some((ChipKind::Value, text.clone()));
                route.chip = Some(text.clone());
                text
            }),
            KeyCode::KeyV => route.routing.via(&route.frozen.geom.board.stackup.copper(), !modifiers.shift).map(|l| format!("via to {}", l.name())),
            KeyCode::KeyW => match &widths {
                Some((widths, rule_w)) => route.routing.width(widths, *rule_w, !modifiers.shift).map(|w| format!("{:.2} mm", w as f64 / 1e6)),
                None => Err("no track rules yet"),
            },
            _ => Ok(match route.routing.posture() {
                Posture::Auto => "bend: as drawn",
                Posture::StraightFirst => "bend: straight first",
                Posture::DiagonalFirst => "bend: diagonal first",
            }.to_string()),
        };
        if said.is_ok() {
            let sample = route.routing.sample();
            route.sent(sample.seq);
            cx.widget_action(uid, ViewAction::Gesture(sample));
        }
        match said {
            Ok(text) => self.note(cx, ChipKind::Value, text, Vec::new()),
            Err(why) => self.note(cx, ChipKind::Refused, why.to_string(), Vec::new()),
        }
        true
    }

    /// The board's track widths (the base and every class's, sorted) and the drawn net's own, from
    /// the host's rule matrix. O(classes log classes), once per W press.
    fn route_widths(&self, cx: &mut Cx) -> Option<(Vec<Nm>, Nm)> {
        let Some(Drag::Route(route)) = &self.st.drag else { return None };
        if !cx.has_global::<ElecLive>() {
            return None;
        }
        let snap = cx.get_global::<ElecLive>().snap.clone();
        let rules = &snap.of(&snap.rules, route.routing.g.unit)?.value;
        let board = &route.frozen.geom.board;
        let rule_w = rules.track_min(board.nets.get(route.routing.g.net?.ix())?.class);
        Some((track_widths(board, |class| rules.track_min(class), rule_w), rule_w))
    }

    /// A drag of a part or a via starts: its held copy (O(the item's size), logged) and its dim row.
    fn begin_hold(&mut self, cx: &mut Cx, unit: BoardId, item: ItemId, from: [f64; 2]) {
        self.end_drag(Some(&mut *cx));
        let Some(draw) = self.st.drawn.clone() else { return };
        let board = draw.board.clone();
        let origin = match board.locate(item) {
            Some((ItemKind::Placed, i)) => board.placed[i].at,
            Some((ItemKind::Via, i)) => board.vias[i].at,
            _ => return,
        };
        let Some(t) = BoardDraws::templates(cx) else { return };
        let held = HeldDraw::new(&board, &draw.fills, &[item], &t);
        log!("fab.board: holding {item:?}: {} instances in {:.2} ms", held.instances, held.build_ms);
        self.st.held = Some(held);
        self.st.marked.clear();
        let dims = self.st.dims.get_or_insert_with(ItemStates::new);
        dims.write(board.next_item as usize, vec![(item.0, [1.0, 0.0, 0.0, 0.0])]);
        let routing = if cx.has_global::<ElecLive>() { Routing::hold(&board, unit, item, origin) } else { None };
        self.st.drag = Some(Drag::Hold(Hold {
            unit, item, from, origin, by: P::new(0, 0), previewed: false, tag: None, board, landing: None, routing, seen: None,
            chip: None,
        }));
        self.redraw(cx);
    }

    /// The pointer moved during a drag: snap so the origin lands on the grid; a new delta goes to
    /// the host as a preview of one move (O(1); the held copy and the rats follow in the frame).
    fn move_hold(&mut self, cx: &mut Cx, at: [f64; 2]) {
        let grid = ((self.grid_mm.max(0.001)) * 1e6).round() as i64;
        let Some(hold) = self.st.hold_mut().filter(|h| h.landing.is_none()) else { return };
        let raw = [((at[0] - hold.from[0]) * 1e6).round() as i64, ((at[1] - hold.from[1]) * 1e6).round() as i64];
        let snap = |o: Nm, d: i64| -> Nm { (((o as i64 + d) as f64 / grid as f64).round() as i64 * grid - o as i64) as Nm };
        let by = P::new(snap(hold.origin.x, raw[0]), snap(hold.origin.y, raw[1]));
        if by == hold.by {
            return;
        }
        hold.by = by;
        if let Some(routing) = hold.routing.as_mut() {
            // The worker answers every position, the press's too: back there its answer is no change.
            let sample = routing.held(hold.item, by);
            hold.previewed = true;
            hold.tag = Some(routing.id.tag());
            cx.widget_action(self.uid, ViewAction::Gesture(sample));
        } else if by == P::new(0, 0) {
            if hold.previewed {
                hold.previewed = false;
                hold.tag = None;
                cx.widget_action(self.uid, ViewAction::CancelPreview);
            }
        } else {
            hold.previewed = true;
            let tag = next_tag();
            hold.tag = Some(tag);
            let op = DesignOp::Board { unit: hold.unit, op: BoardOp::Move { item: hold.item, by } };
            cx.widget_action(self.uid, ViewAction::Preview { notes: vec![], tag, place: vec![], ops: vec![op], label: "Move".into() });
        }
        self.redraw(cx);
    }

    /// The gesture is over (cancelled, a no-op, or landed): the copies go, the dim row clears. A
    /// live preview is cancelled and a route's gesture ended unless its commit was sent.
    fn end_drag(&mut self, cx: Option<&mut Cx>) {
        let Some(drag) = self.st.drag.take() else { return };
        self.st.held = None;
        let (previewed, sent) = match &drag {
            Drag::Hold(hold) => {
                if let Some(dims) = self.st.dims.as_mut() {
                    dims.write(hold.board.next_item as usize, Vec::new());
                }
                (hold.previewed, hold.landing.as_ref().is_some_and(|l| l.sent))
            }
            Drag::Route(route) => (true, route.landing.as_ref().is_some_and(|l| l.sent)),
            Drag::Dropped(dropped) => (true, dropped.landing.sent),
        };
        if let Some(cx) = cx {
            if previewed && !sent {
                cx.widget_action(self.uid, ViewAction::CancelPreview);
            }
            if let Drag::Route(route) = &drag {
                route.log_waits();
                if !sent {
                    cx.widget_action(self.uid, ViewAction::EndGesture(route.routing.id));
                }
            }
            if let Drag::Hold(Hold { routing: Some(routing), .. }) = &drag {
                if !sent {
                    cx.widget_action(self.uid, ViewAction::EndGesture(routing.id));
                }
            }
            self.redraw(cx);
        }
    }

    /// The landing's outcome from the host's snapshot now; `Waiting` until the commit is sent.
    /// O(1).
    fn outcome(&self, cx: &mut Cx) -> Option<Outcome> {
        let landing = self.st.landing()?;
        let unit = match self.st.drag.as_ref()? {
            Drag::Hold(hold) => hold.unit,
            Drag::Route(route) => route.routing.g.unit,
            Drag::Dropped(dropped) => dropped.unit,
        };
        if !landing.sent || !cx.has_global::<ElecLive>() {
            return Some(Outcome::Waiting);
        }
        Some(outcome(landing.tag, unit, &cx.get_global::<ElecLive>().snap))
    }

    /// Whether the pane may show `shown` now: always, except while a gesture lands, when only the
    /// picture that ends the landing is taken (the drop's commit, or anything after a refusal).
    fn takes_picture(&self, cx: &mut Cx, shown: &Board) -> bool {
        match self.outcome(cx) {
            None => true,
            Some(outcome) => match self.st.hold() {
                // A host without a snapshot: the drop is on screen when the picture has it there.
                Some(hold) if !cx.has_global::<ElecLive>() => {
                    shows_drop(hold.item, hold.origin.add(hold.by), shown)
                }
                _ => landed(outcome, shown),
            },
        }
    }

    /// Why the host refused the landing's commit, when it did: the chip says it as the gesture goes
    /// back. O(1).
    fn landing_refusal(&self, cx: &mut Cx) -> Option<String> {
        let tag = self.st.landing()?.tag;
        if self.outcome(cx)? != Outcome::Over {
            return None;
        }
        let snap = &cx.get_global::<ElecLive>().snap;
        snap.last_refusal.as_ref().filter(|refusal| refusal.sample == Some(tag)).map(|refusal| refusal.reason.clone())
    }

    /// The landing is over with the picture on screen.
    fn drag_over(&self, cx: &mut Cx) -> bool {
        let Some(draw) = self.st.drawn.clone() else { return false };
        self.st.landing().is_some() && self.takes_picture(cx, &draw.board)
    }

    /// The connectivity the ratsnest draws: the host snapshot's for this unit, or `set_conn`'s.
    fn conn(&self, cx: &mut Cx) -> Option<Arc<Connectivity>> {
        let unit = self.st.unit?;
        if cx.has_global::<ElecLive>() {
            let snap = &cx.get_global::<ElecLive>().snap;
            return snap.of(&snap.conn, unit).map(|t| t.value.clone());
        }
        self.st.conn.clone()
    }

    /// The committed board's connectivity (never a preview's), when it has come.
    fn committed_conn(&self, cx: &mut Cx) -> Option<Arc<Connectivity>> {
        let unit = self.st.unit?;
        if cx.has_global::<ElecLive>() {
            let snap = &cx.get_global::<ElecLive>().snap;
            return snap.conn.iter().find(|t| t.unit == Some(unit) && t.of == StateOf::Committed).map(|t| t.value.clone());
        }
        self.st.conn.clone()
    }

    /// Placement ops always start from the committed design, even while a preview is visible.
    fn design(&self, cx: &mut Cx) -> Option<Arc<Design>> {
        if cx.has_global::<ElecLive>() {
            return Some(cx.get_global::<ElecLive>().snap.design.clone());
        }
        self.st.design.clone()
    }

    /// The first item id the shown picture has that its committed board has not issued (a
    /// preview's new tracks, vias or part); `None` while the committed board is shown. Ids are
    /// never reused, so this one comparison finds every new item.
    fn fresh_from(&self, cx: &mut Cx) -> Option<u32> {
        let draw = BoardDraws::current(cx, self.st.unit?)?;
        let design = self.design(cx)?;
        let committed = design.unit(draw.unit)?.board.as_ref()?;
        (draw.board.next_item > committed.next_item).then_some(committed.next_item)
    }

    /// The route job on this pane's board, and whether the shown picture is its running partial
    /// (its new copper then draws pending).
    fn route_job(&self, cx: &Cx) -> Option<(Arc<RouteJobState>, bool)> {
        let unit = self.st.unit?;
        let live = cx.get_global_ref::<ElecLive>()?;
        let job = live.snap.job.clone().filter(|job| job.status.unit == unit)?;
        let partial = job.end.is_none()
            && live.snap.proposal.as_ref().is_some_and(|p| matches!(p.source, ProposalSource::Job { job: id, .. } if id == job.status.job));
        Some((job, partial))
    }

    /// What the route line shows: a route job running on this board, else the unrouted nets of
    /// the shown board, how many the selection holds, and the finished job whose open nets it
    /// names; None without a host or before a picture.
    fn route_line_key(&mut self, cx: &mut Cx) -> Option<LineKey> {
        let job = self.route_job(cx);
        if let Some((job, _)) = job.as_ref().filter(|(job, _)| job.end.is_none()) {
            return Some(LineKey::Job { status: job.status, organic: job.method == Method::Organic });
        }
        let unit = self.st.unit?;
        let (committed, preview_shown) = {
            let live = cx.get_global_ref::<ElecLive>()?;
            (live.snap.design.unit(unit).and_then(|u| u.board.clone()), live.snap.preview.is_some())
        };
        let conn = self.conn(cx)?;
        // A finished job's open nets are taken once, when its commit is on the board and the
        // committed board's own connectivity has caught up with it (a preview's may carry the same
        // generation); those still unrouted in the connectivity the count comes from are named
        // while the board keeps that generation.
        if let (Some((job, _)), Some(board)) = (&job, &committed) {
            if self.st.left_open.as_ref().is_none_or(|left| left.job != job.status.job) {
                if let Some(left) = self.committed_conn(cx).and_then(|committed| left_open(job, preview_shown, board, &committed)) {
                    self.st.left_open = Some(left);
                }
            }
        }
        self.st.still_open = match (&self.st.left_open, &committed) {
            (Some(left), Some(board)) => still_open(left, board.generation, &conn),
            _ => Vec::new(),
        };
        let left = self.st.left_open.as_ref().filter(|_| !self.st.still_open.is_empty()).map(|left| left.job);
        let board = &self.st.drawn.as_ref()?.board;
        let selected = selected_unrouted(board, &self.st.selected, &conn.unrouted).len();
        Some(LineKey::Idle { unrouted: conn.unrouted.len(), selected, left })
    }

    /// A press on the route line: route the board's unrouted nets, or the selection's, or re-route
    /// the nets the last route left open.
    fn route_pressed(&mut self, cx: &mut Cx, button: LineButton) {
        let Some(unit) = self.st.unit else { return };
        if let (LineButton::Stop | LineButton::Discard, Some(LineKey::Job { status, .. })) = (button, self.route_line.key()) {
            log!("fab.board: {:?} route job {}", button, status.job.0);
            let action = if button == LineButton::Stop { ViewAction::StopRoute(status.job) } else { ViewAction::DiscardRoute(status.job) };
            cx.widget_action(self.uid, action);
            return;
        }
        let nets = match button {
            LineButton::Stop | LineButton::Discard => return,
            LineButton::RouteAll => Vec::new(),
            LineButton::Reroute if self.st.still_open.is_empty() => return,
            LineButton::Reroute => self.st.still_open.clone(),
            LineButton::RouteSelected => {
                let (Some(conn), Some(draw)) = (self.conn(cx), self.st.drawn.as_ref()) else { return };
                let nets = selected_unrouted(&draw.board, &self.st.selected, &conn.unrouted);
                if nets.is_empty() {
                    return;
                }
                nets
            }
        };
        let method = self.st.method;
        let reroute = button == LineButton::Reroute;
        log!("fab.board: {} {} on unit {} ({})", if reroute { "re-route" } else { "route" },
            if nets.is_empty() { "all unrouted nets".to_owned() } else { format!("{} nets", nets.len()) }, unit.0, super::tools::method_name(method));
        cx.widget_action(self.uid, ViewAction::Route { unit, nets, method, reroute });
    }

    /// Whether the armed tool takes drags (Zone, Measure).
    fn tool_drags(&self) -> bool {
        matches!(self.st.armed.tool, BoardTool::Zone | BoardTool::Measure)
    }

    /// A Zone or Measure drag from where its press went down to world `to`; `done` on release:
    /// the ruler stays until the next press, the zone is added. O(1) per move.
    fn tool_drag(&mut self, cx: &mut Cx, to: [f64; 2], done: bool) {
        let Some(from) = self.st.press_at else { return };
        match self.st.armed.tool {
            BoardTool::Measure => {
                self.st.ruler = Some([from, to]);
                if done {
                    self.set_armed(cx, self.st.armed.done());
                }
            }
            BoardTool::Zone => {
                let rect = [from[0].min(to[0]), from[1].min(to[1]), from[0].max(to[0]), from[1].max(to[1])];
                self.st.zone_box = (!done).then_some(rect);
                if done {
                    self.add_zone(cx, from, rect);
                    self.set_armed(cx, self.st.armed.done());
                }
            }
            _ => {}
        }
        self.redraw(cx);
    }

    /// A world point snapped to the pane's grid, in nm.
    fn snapped(&self, at: [f64; 2]) -> P {
        let grid = (self.grid_mm.max(0.001) * 1e6).round() as i64;
        let snap = |mm: f64| ((mm * 1e6 / grid as f64).round() as i64 * grid) as Nm;
        P::new(snap(at[0]), snap(at[1]))
    }

    /// The Via tool's click at world `at`: a via there on the net of the copper under it, the Via
    /// field's diameter and its net class's drill (`plan::via`), one undo step; or a chip saying
    /// why not.
    fn place_via(&mut self, cx: &mut Cx, at: [f64; 2]) {
        let Some(design) = self.design(cx) else { return };
        let Some(draw) = self.st.drawn.clone() else { return };
        let unit = draw.unit;
        let net = self.net_at(&draw, at);
        let pad = self.via_diameter(cx, unit);
        let drill = self.via_drill(cx, &draw.board, unit, net);
        match plan::via(&design, unit, nm(at), net, pad, drill) {
            Ok(plan) => {
                log!("fab.board: {}", plan.label);
                self.clear_note(cx);
                cx.widget_action(self.uid, ViewAction::Ops { ops: plan.ops, place: vec![], label: plan.label });
                self.set_armed(cx, self.st.armed.done());
            }
            Err(why) => self.note(cx, ChipKind::Refused, why, Vec::new()),
        }
    }

    /// The drill of a via on `net`: its net class's via drill from the rules, else 0.3 mm.
    fn via_drill(&self, cx: &mut Cx, board: &Board, unit: BoardId, net: Option<NetId>) -> Nm {
        let class = net.and_then(|net| board.nets.get(net.ix())).map(|net| net.class);
        let rules = cx.get_global_ref::<ElecLive>().and_then(|live| live.snap.of(&live.snap.rules, unit).map(|r| r.value.clone()));
        match (rules, class) {
            (Some(rules), Some(class)) => rules.via(class).drill,
            _ => 300_000,
        }
    }

    /// The Zone tool's release: a zone over the grid-snapped rectangle on the active copper layer,
    /// of the net of the copper where the drag began, else ground (`plan::zone`); one undo step.
    fn add_zone(&mut self, cx: &mut Cx, from: [f64; 2], rect: [f64; 4]) {
        let Some(design) = self.design(cx) else { return };
        let Some(draw) = self.st.drawn.clone() else { return };
        let net = self.net_at(&draw, from);
        let layer = self.st.active.filter(|l| draw.board.copper_set().has(*l)).unwrap_or(LayerId::F_CU);
        let area = fab_elec_core::BBox::new(self.snapped([rect[0], rect[1]]), self.snapped([rect[2], rect[3]]));
        match plan::zone(&design, draw.unit, area, net, layer) {
            Ok(plan) => {
                log!("fab.board: {}", plan.label);
                self.clear_note(cx);
                cx.widget_action(self.uid, ViewAction::Ops { ops: plan.ops, place: vec![], label: plan.label });
            }
            Err(why) => self.note(cx, ChipKind::Refused, why, Vec::new()),
        }
    }

    /// Update from the circuit: the circuit's parts the board does not place yet, placed under
    /// the board (`plan::place_unplaced`), one undo step; or a chip saying there are none.
    fn place_from_circuit(&mut self, cx: &mut Cx) {
        let (Some(unit), Some(design)) = (self.st.unit, self.design(cx)) else { return };
        match plan::place_unplaced(&design, unit) {
            Ok((place, label)) => {
                log!("fab.board: {label}");
                self.clear_note(cx);
                cx.widget_action(self.uid, ViewAction::Ops { ops: vec![], place, label });
            }
            Err(why) => self.note(cx, ChipKind::Info, why, Vec::new()),
        }
    }

    /// The route method picked in the strip's flyout.
    fn set_method(&mut self, cx: &mut Cx, method: Method) {
        if self.st.method != method {
            log!("fab.board: route method {}", method_name(method));
            self.st.method = method;
            self.redraw(cx);
        }
    }

    /// How copper drawn by hand meets the tracks in its way: the strip's Shove switch.
    fn route_mode(&self) -> Mode {
        if self.st.walks { Mode::Walk } else { Mode::Shove }
    }

    /// The Shove switch flipped (its button, or S): copper drawn from now on, and the copper being
    /// drawn, pushes the tracks in its way or walks round them. The strip shows it at the next draw;
    /// while copper is drawn its chip says it too, and its next answer is made the new way.
    fn flip_shove(&mut self, cx: &mut Cx) {
        self.st.walks = !self.st.walks;
        let mode = self.route_mode();
        log!("fab.board: drawn copper {}", if mode == Mode::Shove { "shoves" } else { "walks" });
        let uid = self.uid;
        let drawing = self.st.route_mut()
            .filter(|r| r.landing.is_none() && matches!(r.routing.g.kind, GestureKind::Route) && !r.routing.g.finish);
        if let Some(route) = drawing {
            route.routing.g.mode = mode;
            let sample = route.routing.sample();
            route.sent(sample.seq);
            cx.widget_action(uid, ViewAction::Gesture(sample));
            let text = match mode {
                Mode::Shove => "shove: pushes the tracks in its way",
                _ => "walk: round the tracks in its way",
            };
            self.note(cx, ChipKind::Value, text.to_string(), Vec::new());
        }
        self.redraw(cx);
    }

    /// The part of the pane right of the strip, as last drawn: the route line centres in it, so
    /// the two never overlap in a narrow pane.
    fn right_of_strip(&self, rect: Rect) -> Rect {
        let strip = self.st.strip_at;
        let right = if strip.size.x > 0.0 { rect.pos.x.max(strip.pos.x + strip.size.x + 8.0) } else { rect.pos.x };
        let end = rect.pos.x + rect.size.x;
        Rect { pos: dvec2(right.min(end), rect.pos.y), size: dvec2((end - right).max(0.0), rect.size.y) }
    }

    /// One column while the strip fits the pane's height, else two: its board actions stand
    /// beside the rest. What stands is read from the drawn strip (a style reload puts its one
    /// column back), and its height is measured as drawn, so a flip shows from the next frame;
    /// it happens only when the pane's height crosses it.
    fn fit_strip(&mut self, cx: &mut Cx2d, above: Rect) {
        let mut strip = self.tools.widget(cx, ids!(strip));
        let drawn = strip.area().rect(cx);
        let column = self.tools.widget(cx, ids!(col_a)).area().rect(cx).size.x;
        let two = column > 0.0 && drawn.size.x > column * 1.5;
        if !two {
            self.st.strip_tall = drawn.size.y;
        }
        let cols = self.st.strip_tall > above.size.y - STRIP_TOP - STRIP_INSET;
        if cols == two {
            return;
        }
        if cols {
            script_apply_eval!(cx, strip, { flow: mod.turtle.Right spacing: 4 });
        } else {
            script_apply_eval!(cx, strip, { flow: mod.turtle.Down spacing: 1 });
        }
        self.tools.widget(cx, ids!(sep_b)).set_visible(cx, !cols);
        self.redraw(cx);
    }

    /// A press on the tool strip.
    fn strip_pressed(&mut self, cx: &mut Cx, press: StripPress) {
        match press {
            StripPress::Tool(tool) => {
                let double = self.st.tool_clicks.click(tool, Cx::monotonic_now());
                self.set_armed(cx, self.st.armed.click(tool, double));
            }
            StripPress::Method => {
                if let Some(anchor) = method_anchor(cx, &self.tools) {
                    let rows = METHODS.iter().map(|m| (method_name(*m), *m == self.st.method)).collect();
                    cx.action(BoardStripAction::MethodMenu { rows, anchor });
                }
            }
            StripPress::Shove => self.flip_shove(cx),
            StripPress::Fill => self.pour(cx, None),
            // the host's Checks editor lists the findings
            StripPress::Check => cx.action(crate::ElecPanel::Checks),
            StripPress::Update => self.place_from_circuit(cx),
            StripPress::Export => {
                if let Some(unit) = self.st.unit {
                    cx.action(BoardStripAction::Export(unit));
                }
            }
            StripPress::RouteAll | StripPress::RouteSelected => {
                self.route_pressed(cx, if press == StripPress::RouteAll { LineButton::RouteAll } else { LineButton::RouteSelected });
                self.set_armed(cx, self.st.armed.done());
            }
            StripPress::ClearRoutes => {
                self.clear_routes(cx);
                self.set_armed(cx, self.st.armed.done());
            }
        }
    }

    /// Arm a tool (or go back to Select); the strip shows it at the next draw.
    fn set_armed(&mut self, cx: &mut Cx, armed: Armed) {
        if self.st.armed != armed {
            log!("fab.board: tool {:?}{}", armed.tool, if armed.kept { " (kept)" } else { "" });
            self.st.armed = armed;
            self.redraw(cx);
        }
    }

    /// Clear routes: the board's unlocked copper of its own, one undo step (`plan::clear_routes`).
    fn clear_routes(&mut self, cx: &mut Cx) {
        let (Some(unit), Some(design)) = (self.st.unit, self.design(cx)) else { return };
        match plan::clear_routes(&design, unit, None) {
            Ok(plan) => {
                log!("fab.board: {} ({} items) on unit {}", plan.label, plan.ops.len(), unit.0);
                cx.widget_action(self.uid, ViewAction::Ops { ops: plan.ops, place: vec![], label: plan.label });
            }
            Err(why) => self.note(cx, ChipKind::Refused, why, Vec::new()),
        }
    }

    /// What the strip's buttons show for the route line's `key` (None: no host or no picture).
    /// Whether there is copper to clear is read once per committed board: O(tracks + vias) then.
    fn strip_show(&mut self, cx: &mut Cx, key: Option<LineKey>) -> StripShow {
        let (running, unrouted, selected) = match key {
            Some(LineKey::Job { .. }) => (true, 0, 0),
            Some(LineKey::Idle { unrouted, selected, .. }) => (false, unrouted, selected),
            None => (false, 0, 0),
        };
        let board = self.st.unit.zip(self.design(cx)).and_then(|(unit, design)| design.unit(unit).and_then(|u| u.board.clone()));
        let clearable = match board {
            Some(board) => match self.st.clearable {
                Some((unit, generation, clearable)) if unit == board.unit && generation == board.generation => clearable,
                _ => {
                    let clearable = plan::has_clearable_routes(&board);
                    self.st.clearable = Some((board.unit, board.generation, clearable));
                    clearable
                }
            },
            None => false,
        };
        StripShow {
            armed: self.st.armed,
            route_all: !running && unrouted > 0,
            route_selected: !running && selected > 0,
            clear_routes: !running && clearable,
            shove: !self.st.walks,
        }
    }

    /// One placement template per board entry/design revision, then one op per grid step.
    fn place_part(&mut self, cx: &mut Cx, unit: BoardId, comp: CompId, abs: Vec2d) {
        let Some(design) = self.design(cx) else { return };
        let world = self.pane.cam.to_world(abs);
        let grid = (self.grid_mm.max(0.001) * 1e6).round() as i64;
        let snap = |mm: f64| ((mm * 1e6 / grid as f64).round() as i64 * grid) as Nm;
        let at = P::new(snap(world[0]), snap(world[1]));
        let cached = self.st.placing.as_ref().is_some_and(|p| {
            p.unit == unit && p.comp == comp && Arc::ptr_eq(&p.design, &design)
        });
        if cached {
            let placing = self.st.placing.as_mut().unwrap();
            if placing.at == at {
                return;
            }
            placing.at = at;
            placing.place.at = at;
        } else {
            self.cancel_placing(cx);
            // Checked once on entry (no board, no footprint, already placed: nothing to preview).
            let Some(op) = placement_op(&design, unit, comp, at, Angle::ZERO, Side::Top) else { return };
            let DesignOp::Board { op: BoardOp::Add(Item::Placed(placed)), .. } = &op else { return };
            let label = format!("Place {}", placed.refdes);
            let place = Place { unit, part: PartRef::Path(placed.path.clone()), at, rot: None, side: None };
            self.st.placing = Some(Placing { unit, comp, at, design, place, label, tag: 0 });
        }
        let placing = self.st.placing.as_mut().unwrap();
        placing.tag = next_tag();
        cx.widget_action(self.uid, ViewAction::Preview {
            notes: vec![], tag: placing.tag, place: vec![placing.place.clone()], ops: vec![], label: placing.label.clone(),
        });
        self.redraw(cx);
    }

    fn cancel_placing(&mut self, cx: &mut Cx) {
        if let Some(placing) = self.st.placing.take() {
            cx.widget_action(self.uid, ViewAction::CancelPreview);
            BoardDraws::discard(cx, placing);
            self.redraw(cx);
        }
    }

    /// All units' findings, awaiting ids and generation, preferring the host's live snapshot.
    fn violations(&self, cx: &mut Cx) -> Option<(u64, Arc<Vec<Violation>>, Arc<Vec<u64>>)> {
        if cx.has_global::<ElecLive>() {
            let snap = &cx.get_global::<ElecLive>().snap;
            return Some((snap.violations.generation, snap.violations.value.clone(), snap.awaiting.clone()));
        }
        self.st.violations.as_ref().map(|v| (self.st.violations_gen, v.clone(), self.st.violations_awaiting.clone()))
    }

    /// Resolve marks against the drawn geometry. The snapshot supplies the proposal checked for
    /// these findings because `routing.answer` updates separately and can already be a later tier.
    /// Free the old marks on the pool because each owns its shape list and label.
    fn sync_marks(&mut self, cx: &mut Cx) {
        let violations = self.violations(cx);
        let drawn = self.st.drawn.as_ref();
        let routing = match &self.st.drag {
            Some(Drag::Hold(hold)) => hold.routing.as_ref(),
            Some(Drag::Route(route)) => Some(&route.routing),
            _ => None,
        };
        let wave = routing.and_then(|routing| {
            let snap = &cx.get_global_ref::<ElecLive>()?.snap;
            let p = snap.proposal.as_ref()?;
            let follows = matches!(p.source, ProposalSource::Gesture { id, tier: Tier::Stretch, .. } if id == routing.id)
                && !p.last;
            follows.then_some((p, snap.live()))
        });
        let key = drawn.zip(violations.as_ref()).map(|(draw, (generation, _, awaiting))| {
            let proposal = wave.map_or(0, |(p, _)| Arc::as_ptr(p) as usize);
            (*generation, Arc::as_ptr(awaiting) as usize, draw.geom_gen, draw.unit, proposal)
        });
        if key == self.st.marks_key {
            return;
        }
        let t0 = Cx::monotonic_now();
        let marks = match (drawn, violations.as_ref()) {
            (Some(draw), Some((_, v, awaiting))) => {
                let mut ids = wave.and_then(|(p, design)| {
                    let board = design.unit(draw.unit)?.board.as_deref()?;
                    Some(wave_may_clear(v, &p.collisions, board))
                }).unwrap_or_default();
                if !ids.is_empty() && !awaiting.is_empty() {
                    ids.extend_from_slice(awaiting);
                    ids.sort_unstable();
                    ids.dedup();
                }
                let pending = if ids.is_empty() { awaiting.as_slice() } else { &ids };
                check_marks(v, pending, draw.unit, &draw.geom.board, &draw.geom.checks)
            }
            _ => Vec::new(),
        };
        let ms = (Cx::monotonic_now() - t0) * 1e3;
        if ms > 2.0 {
            log!("fab.board: check marks took {ms:.2} ms on the UI thread ({} marks)", marks.len());
        }
        self.st.marks_key = key;
        let old = std::mem::replace(&mut self.st.marks, marks);
        if !old.is_empty() {
            BoardDraws::discard(cx, old);
        }
    }

    fn step_autopan(&mut self, now: f64) {
        let (Some(t0), Some(draw), cam) = (self.st.autopan, self.st.drawn.as_ref(), &mut self.pane.cam) else { return };
        let b = draw.geom.scene.bbox;
        let (x0, y0, x1, y1) = (b.min.x as f64 / 1e6, b.min.y as f64 / 1e6, b.max.x as f64 / 1e6, b.max.y as f64 / 1e6);
        let fit = Camera2d::fit(cam.view, cam.dpi, [x0, y0, x1, y1], 24.0);
        let t = now - t0;
        // a 12 s loop: zoom from the whole board to 16x and back while circling the board
        let z = 0.5 - 0.5 * (t * std::f64::consts::TAU / 12.0).cos();
        cam.px_per_unit = fit.px_per_unit * 16f64.powf(z);
        let a = t * std::f64::consts::TAU / 7.0;
        cam.center = [(x0 + x1) * 0.5 + (x1 - x0) * 0.3 * a.cos() * z, (y0 + y1) * 0.5 + (y1 - y0) * 0.3 * a.sin() * z];
    }

    fn draw_board(&mut self, cx: &mut Cx2d, rect: Rect) {
        // A drop settles before the layers read the dim rows, so the frame that retires the held
        // copy also shows the picture's copy undimmed: the item is never missing for a frame.
        if self.drag_over(cx) {
            if let Some(landing) = self.st.landing() {
                log!("fab.board: release on screen {:.1} ms after it", (Cx::monotonic_now() - landing.since) * 1e3);
            }
            let refused = self.landing_refusal(cx);
            self.end_drag(None);
            if let Some(text) = refused {
                self.note(cx, ChipKind::Refused, text, Vec::new());
            }
        }
        let theme = self.theme();
        let dpi = cx.current_dpi_factor();
        let cam = self.pane.cam;
        let main = cam.uniforms();
        let active = self.st.active.unwrap_or(LayerId::F_CU);
        let hover_item = match self.st.hover {
            Some(item) if item.0 < (1 << 24) => item.0 as f32,
            _ => -1.0,
        };
        // A preview's own items breathe in the proposal ink until it commits, so the pane keeps
        // drawing while they are on screen (uniforms only: nothing re-records). A running route
        // job's partial is steady instead (faint, bright edges): a long job draws only on change.
        // Drawn copper whose newest sample waits for its answer shows its old head the same way
        // until the answer is in; a wait still under `STALE_SECONDS` asks for the frame it ends in.
        let fresh = self.fresh_from(cx).filter(|first| *first < (1 << 24));
        let job = self.route_job(cx);
        let waiting = self.st.drag.as_ref().and_then(|drag| match drag {
            Drag::Route(route) if route.landing.is_none() && route.routing.stale() => route.sent.front().map(|(_, since)| *since),
            _ => None,
        });
        let stale = waiting.is_some_and(|since| Cx::monotonic_now() - since >= STALE_SECONDS);
        if waiting.is_some() && !stale && !cx.next_frame_is_pending(self.st.next_frame) {
            self.st.next_frame = cx.new_next_frame();
        }
        let pending = fresh.is_some() && (stale || job.as_ref().is_some_and(|(_, partial)| *partial));
        if fresh.is_some() && !pending && !cx.next_frame_is_pending(self.st.next_frame) {
            self.st.next_frame = cx.new_next_frame();
        }
        let breath = (Cx::monotonic_now() * std::f64::consts::TAU / 1.6).sin() as f32;
        // A probe's or the host's net wins; else a running job lights the net it routes, softer.
        let routing = job.as_ref()
            .filter(|(job, _)| job.end.is_none() && !matches!(job.status.phase, JobPhase::Relax { .. }))
            .and_then(|(job, _)| job.status.current);
        let (lit, lit_mix) = match (&self.st.lit, self.st.host_lit) {
            (Some((_, net)), _) => (*net, 1.0),
            (None, Some(net)) => (Some(net), 1.0),
            (None, None) => (routing, 0.6),
        };
        let lit = lit.filter(|net| net.0 < (1 << 24));
        let mut base = BoardUniforms {
            cam: main,
            live_light: if self.light() { 1.0 } else { 0.0 },
            hover_item,
            hover_mix: self.pane.hover_mix(),
            hover_at: pour_light(self.pane.pointer(), dvec2(0.0, 0.0), 1.0),
            fresh_item: fresh.map_or(-1.0, |first| first as f32),
            fresh_mix: if pending { PENDING_MIX } else if fresh.is_some() { 0.75 + 0.15 * breath } else { 0.0 },
            fresh_pending: if pending { 1.0 } else { 0.0 },
            lit_net: lit.map_or(-1.0, |net| net.0 as f32),
            lit_mix: if lit.is_some() { lit_mix } else { 0.0 },
            thin_alpha: theme.thin_alpha,
            clip: [rect.pos.x as f32, rect.pos.y as f32, (rect.pos.x + rect.size.x) as f32, (rect.pos.y + rect.size.y) as f32],
            ..BoardUniforms::default()
        };
        let mut tex = if cx.has_global::<ElecLive>() {
            let live = cx.get_global::<ElecLive>();
            let sim = &live.sim;
            // The rows are in once the host has drawn its first frame.
            base.live_on = if sim.nets.is_some() { 1.0 } else { 0.0 };
            base.live_time = sim.time;
            let playing = sim.playing;
            let states = self.st.unit.and_then(|unit| live.item_states(StatesOf::Board(unit)).cloned());
            base.item_states_on = if states.is_some() { 1.0 } else { 0.0 };
            let tex = BoardTextures { slots: [sim.nets.clone(), sim.segs.clone(), sim.parts.clone(), states, None, None] };
            if playing && !cx.next_frame_is_pending(self.st.next_frame) {
                self.st.next_frame = cx.new_next_frame();
            }
            tex
        } else {
            BoardTextures::default()
        };
        // The pane's rows stay bound once made (zero rows after a drag), so lists re-record once.
        if let Some(dims) = self.st.dims.as_mut() {
            tex.slots[4] = Some(dims.texture(cx));
            base.pane_states_on = 1.0;
        }
        let size = ((rect.size.x * dpi).ceil().max(1.0) as u32, (rect.size.y * dpi).ceil().max(1.0) as u32);
        let target_cam = [((main[0] as f64 - rect.pos.x) * dpi) as f32, ((main[1] as f64 - rect.pos.y) * dpi) as f32, main[2] * dpi as f32, 1.0];
        let order = BoardTheme::stack(self.st.layers.iter().map(|s| s.layer));
        let direct = self.st.direct;
        let timing = self.st.timing;
        let mut lists = 0;
        let mut shown = 0;
        if let Some(substrate) = self.st.substrate.as_mut() {
            let mut u = BoardUniforms { layer_ink: theme.substrate, hover_item: -1.0, ..base };
            let mut tex = tex.clone();
            if let Some(slot) = &self.st.heat {
                tex.slots[5] = Some(slot.texture.clone());
                u.heat_on = 1.0;
                u.heat_box = slot.field.bounds_mm.map(|v| v as f32);
                u.heat_mix = self.st.heat_mix;
            }
            substrate.draw(cx, &mut self.draw_tri.draw_vars, &tex, &u);
            lists += 1;
        }
        if tex.slots[2].is_some() && self.st.drawn.as_ref().and_then(|d| d.index.as_ref())
            .is_some_and(|index| Arc::ptr_eq(index, &cx.get_global::<ElecLive>().sim.index))
        {
            let (item, at) = self.st.hold().filter(|h| h.item.0 < (1 << 24)).map_or((-1.0, [0.0; 2]), |h| {
                let at = h.origin.add(h.by);
                (h.item.0 as f32, [fab_elec_core::to_mm(at.x) as f32, fab_elec_core::to_mm(at.y) as f32])
            });
            if let Some(glows) = self.st.glows.as_mut() {
                let u = BoardUniforms { live_on: 1.0, ..base };
                glows.draw_with(cx, &mut self.draw_glow.draw_vars, &tex, &u, &[
                    (live_id!(glow_drag_item), &[item]), (live_id!(glow_drag_at), &at),
                ]);
                lists += 1;
            }
        }
        for layer in order {
            let visible = self.layer_shown(layer);
            let Some(i) = self.st.layers.iter().position(|s| s.layer == layer) else { continue };
            let alpha = theme.opacity(layer, active);
            let ink = theme.ink(layer);
            let slot = &mut self.st.layers[i];
            if !visible || alpha < 0.004 {
                if let Some(t) = slot.target.as_mut() {
                    t.release(cx);
                }
                continue;
            }
            shown += 1;
            if direct {
                let u = BoardUniforms { layer_alpha: alpha, layer_ink: ink, ..base };
                if let Some(l) = slot.tris.as_mut() {
                    l.draw(cx, &mut self.draw_tri.draw_vars, &tex, &u);
                    lists += 1;
                }
                if let Some(l) = slot.strokes.as_mut() {
                    l.draw(cx, &mut self.draw_stroke.draw_vars, &tex, &u);
                    lists += 1;
                }
                if let Some(l) = slot.pads.as_mut() {
                    l.draw(cx, &mut self.draw_pad.draw_vars, &tex, &u);
                    lists += 1;
                }
                continue;
            }
            let target = slot.target.get_or_insert_with(|| RenderTarget::new(cx, "fab_board_layer"));
            target.ensure(cx, size, RenderTargetFormat::Bgra8);
            if timing {
                target.pass.set_gpu_timing_enabled(cx, true);
                slot.gpu.extend(target.pass.take_gpu_times_ms(cx));
            }
            target.begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
            let u = BoardUniforms {
                cam: target_cam, layer_ink: ink, clip: [0.0, 0.0, size.0 as f32, size.1 as f32],
                hover_at: pour_light(self.pane.pointer(), rect.pos, dpi), ..base
            };
            if let Some(l) = slot.tris.as_mut() {
                l.draw(cx, &mut self.draw_tri_max.draw_vars, &tex, &u);
                lists += 1;
            }
            if let Some(l) = slot.strokes.as_mut() {
                l.draw(cx, &mut self.draw_stroke_max.draw_vars, &tex, &u);
                lists += 1;
            }
            if let Some(l) = slot.pads.as_mut() {
                l.draw(cx, &mut self.draw_pad_max.draw_vars, &tex, &u);
                lists += 1;
            }
            target.end(cx);
            self.draw_layer.draw_vars.set_texture(0, &target.texture);
            self.draw_layer.opacity = alpha;
            self.draw_layer.draw_abs(cx, rect);
        }
        if let Some(holes) = self.st.holes.as_mut() {
            let u = BoardUniforms { layer_ink: theme.hole, hover_item: -1.0, fresh_item: -1.0, ..base };
            holes.draw(cx, &mut self.draw_pad.draw_vars, &tex, &u);
            lists += 1;
        }
        self.sync_marks(cx);
        let mut pending_ink = theme.label;
        pending_ink[3] *= 0.55;
        self.marks.draw(cx, &self.st.marks, &cam, theme.marks, pending_ink, self.st.hover);
        self.draw_flags(cx, rect);
        if let Some(slot) = &self.st.heat {
            let light = self.light();
            self.heat_key.draw(cx, rect, &slot.field, self.st.heat_mix, light);
        }
        let key = self.route_line_key(cx);
        let show = self.strip_show(cx, key);
        if self.st.strip_shown != Some(show) {
            apply_strip(cx, &self.tools, &show);
            self.st.strip_shown = Some(show);
        }
        let board = self.st.drawn.as_ref().map(|draw| draw.board.clone());
        let name_of = |net: NetId| board.as_ref().and_then(|b| b.nets.get(net.ix())).map(|n| n.name.clone());
        let why = self.st.left_open.as_ref().map_or(&[][..], |left| &left.why[..]);
        let line_rect = self.right_of_strip(rect);
        self.route_line.draw(cx, line_rect, key, &self.st.still_open, why, name_of, &theme);
        self.draw_drag(cx, &theme, active, &base);
        self.draw_keys(cx, &theme, active, &base, rect);
        self.draw_tool_marks(cx, rect);
        self.st.stats.lists = lists;
        self.st.stats.layers = shown;
        if timing {
            let pass = cx.current_pass_id();
            cx.passes[pass].set_gpu_timing_enabled(true);
            self.st.gpu_buf.clear();
            cx.passes[pass].drain_gpu_time_samples(&mut self.st.gpu_buf);
            self.st.main_gpu.extend(self.st.gpu_buf.iter().map(|s| s.1));
        }
    }
}

impl Widget for BoardPane {
    /// One node for the pane. Counts are Vec lengths and selection text is cached on change.
    fn semantics(&self, cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.role_description, "board");
        let board = self.st.drawn.as_ref().map(|draw| &draw.geom.board);
        let design = cx.get_global_ref::<ElecLive>().map(|live| &live.snap.design).or(self.st.design.as_ref());
        let name = board.and_then(|board| design.and_then(|design| design.unit(board.unit)))
            .map(|unit| unit.name.as_str())
            .or_else(|| board.map(|board| board.name.as_str())).filter(|name| !name.is_empty()).unwrap_or("Board");
        makepad_platform::a11y::set_string(&mut node.name, name);
        let (parts, tracks, vias) = board.map_or((0, 0, 0), |board| (board.placed.len(), board.tracks.len(), board.vias.len()));
        makepad_platform::a11y::set_string(&mut node.value,
            &format!("{parts} parts, {tracks} tracks, {vias} vias{}", self.st.selection_summary));
    }

    /// Item nodes exist only for this lookup; the frame publishes no semantic parts.
    fn semantics_lookup(&self, cx: &Cx, lookup: &SemanticLookup, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let Some(draw) = &self.st.drawn else { return };
        let order = self.pick_order();
        let geom = &draw.geom;
        let items = BoardItems {
            unit: geom.board.unit,
            board: &geom.board,
            scene: &geom.scene,
            pick: &geom.pick,
            items: &geom.items,
            order: &order,
        };
        let id_key = match lookup {
            SemanticLookup::Terms(q) => q.id_term()
                .filter(|id| cx.a11y.action_owner(*id).uid() == self.uid.0)
                .and_then(|id| cx.a11y.action_part_key(id)),
            SemanticLookup::Key(_) => None,
        };
        out.extend(items.lookup(&geom.names, &self.pane.cam, lookup, id_key)
            .into_iter().map(|(key, node)| (key, None, node)));
    }

    /// Actions on this pane's lookup answers; the widget layer routes them here by owner.
    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        self.semantic_action(cx, request)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.route_poll(cx);
        self.hold_poll(cx);
        if self.status.visible() {
            if matches!(event, Event::Signal) {
                if let Some(draw) = &self.st.drawn {
                    let supplied = self.st.conn.as_ref().filter(|_| self.st.unit == Some(draw.unit));
                    if self.st.bar.sync_sources(cx, &draw.board, supplied) {
                        self.redraw(cx);
                    }
                }
            }
            let actions = cx.capture_actions(|cx| self.status.handle_event(cx, event, scope));
            for press in self.st.bar.bar_presses(cx, &self.status, &actions) {
                match press {
                    BarPress::Layer(layer) => self.set_active_layer(cx, layer),
                    BarPress::Track(w) => self.st.track_w = Some(w),
                    BarPress::Via(d) => self.st.via_d = Some(d),
                    BarPress::Grid(mm) => self.grid_mm = mm,
                }
                self.redraw(cx);
            }
            if over_strip(cx, &self.status, BAR_PARTS, event, self.pane.pressed()) {
                return;
            }
        }
        // The method the strip's flyout picked (the host draws the flyout).
        if let Event::Actions(actions) = event {
            let picked = actions.iter().filter_map(|a| a.downcast_ref::<BoardStripAction>())
                .filter_map(|a| match a { BoardStripAction::Method(i) => METHODS.get(*i).copied(), _ => None }).last();
            if let Some(method) = picked {
                self.set_method(cx, method);
            }
        }
        // The tool strip is over the canvas: its presses, hovers and scrolls are its own.
        let actions = cx.capture_actions(|cx| self.tools.handle_event(cx, event, scope));
        for press in strip_presses(cx, &self.tools, &actions) {
            self.strip_pressed(cx, press);
        }
        if over_strip(cx, &self.tools, STRIP_PARTS, event, self.pane.pressed()) {
            return;
        }
        // A press on the canvas: where the Zone and Measure drags start, and away with the ruler.
        if let Event::MouseDown(e) = event {
            if self.st.rect.contains(e.abs) {
                self.st.press_at = Some(self.pane.cam.to_world(e.abs));
                if self.st.ruler.take().is_some() {
                    self.redraw(cx);
                }
            }
        }
        // Before everything that hit-tests the pane: a press on the route line is the line's.
        let hit = self.route_line.handle_event(cx, event);
        if hit.redraw {
            self.redraw(cx);
        }
        if let Some(button) = hit.pressed {
            self.route_pressed(cx, button);
        }
        // Then the probe flags over the copper.
        if self.flag_event(cx, event) {
            return;
        }
        if let Event::Actions(actions) = event {
            for action in actions {
                let action = action.as_widget_action().and_then(|widget| widget.action.downcast_ref::<ViewAction>())
                    .or_else(|| action.downcast_ref::<ViewAction>());
                match action {
                    Some(ViewAction::Focus(focus)) => self.select_focus(cx, focus),
                    Some(ViewAction::Unfocus) => self.set_selection(cx, &[]),
                    _ => {}
                }
            }
        }
        if let Event::Signal = event {
            // Another view may have polled first: compare with what this pane shows.
            BoardDraws::poll(cx);
            self.poll_arrange(cx);
            self.follow(cx);
            // A reopened plate can reuse both a unit id and its generation numbers.
            let shown = self.st.drawn.as_ref().map(|d| (d.unit, d.gen, Arc::as_ptr(&d.geom)));
            let current = self.st.unit.and_then(|u| BoardDraws::current(cx, u)).map(|d| (d.unit, d.gen, Arc::as_ptr(&d.geom)));
            // New findings matter only over a drawn board (the marks resolve against it).
            let findings = self.violations(cx).map(|(generation, _, awaiting)| (generation, Arc::as_ptr(&awaiting) as usize));
            // The route line hides while a job runs on this board and returns when it ends.
            let line = shown.is_some() && self.route_line_key(cx) != self.route_line.key();
            if current.is_some() && current != shown
                || shown.is_some() && findings != self.st.marks_key.map(|key| (key.0, key.1))
                || self.st.landing().is_some()
                || line
            {
                self.redraw(cx);
            }
        }
        // A hover elsewhere (the Firmware pane's pins) lit other rows: draw them, playing or not.
        if matches!(event, Event::NextFrame(_)) && cx.has_global::<ElecLive>() {
            let emphasis = cx.get_global::<ElecLive>().sim.emphasis;
            if emphasis != self.st.live_emphasis {
                self.st.live_emphasis = emphasis;
                self.redraw(cx);
            }
        }
        if self.st.chips.as_ref().is_some_and(|chips| chips.timer.is_event(event).is_some()) {
            self.clear_note(cx);
        }
        if self.st.next_frame.is_event(event).is_some() {
            if self.st.autopan.is_some() {
                self.step_autopan(Cx::monotonic_now());
                self.st.next_frame = cx.new_next_frame();
            }
            if let Some(glide) = &self.st.glide {
                let now = Cx::monotonic_now();
                self.pane.cam = glide.at(now);
                if glide.done(now) {
                    self.st.glide = None;
                } else {
                    self.st.next_frame = cx.new_next_frame();
                }
            }
            let target = if self.st.heat_want { 1.0 } else { 0.0 };
            if self.st.heat_mix != target {
                let now = Cx::monotonic_now();
                let step = ((now - self.st.heat_at) / 0.25).max(0.0) as f32;
                self.st.heat_mix = if self.st.heat_want {
                    (self.st.heat_mix + step).min(1.0)
                } else {
                    (self.st.heat_mix - step).max(0.0)
                };
                self.st.heat_at = now;
                if self.st.heat_mix != target && !cx.next_frame_is_pending(self.st.next_frame) {
                    self.st.next_frame = cx.new_next_frame();
                }
            }
            if !self.st.heat_want && self.st.heat_mix == 0.0 {
                if let Some(slot) = self.st.heat.take() {
                    BoardDraws::discard(cx, slot.field);
                }
            }
            self.redraw(cx);
        }
        self.handle_probe(cx, event);
        // Only a tray drag is hit-tested here: `drag_hits` claims the event inside the pane, and
        // other drags (a probe) belong to their own handlers.
        let tray = match event {
            Event::Drag(drag) => tray_drag_part(&drag.items),
            Event::Drop(drop) => tray_drag_part(&drop.items),
            _ => None,
        };
        if let Some((unit, comp)) = tray {
            match event.drag_hits(cx, self.draw_bg.area()) {
                DragHit::Drag(hit) => match hit.state {
                    DragState::In | DragState::Over if self.st.unit == Some(unit) => {
                        *hit.response.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = DragResponse::Copy;
                        self.place_part(cx, unit, comp, hit.abs);
                    }
                    _ => self.cancel_placing(cx),
                },
                DragHit::Drop(_) => {
                    if self.st.unit == Some(unit)
                        && self.st.placing.as_ref().is_some_and(|p| p.unit == unit && p.comp == comp)
                    {
                        // The live preview is the drop's: the host places the part and its labels
                        // in one undo step, and names the preview's tag on the commit or refusal.
                        let placing = self.st.placing.take().unwrap();
                        cx.widget_action(self.uid, ViewAction::CommitPreview);
                        log!("fab.board: placed {:?} at {:?}", placing.comp, placing.at);
                        if cx.has_global::<ElecLive>() {
                            self.end_drag(Some(&mut *cx));
                            let landing = Landing { tag: placing.tag, sent: true, ..Self::release_landing() };
                            self.st.drag = Some(Drag::Dropped(Dropped { unit, landing }));
                        }
                        BoardDraws::discard(cx, placing);
                        self.redraw(cx);
                    }
                }
                _ => {}
            }
        }
        if matches!(event, Event::DragEnd) {
            self.cancel_placing(cx);
        }
        let inputs = {
            let Some(draw) = self.st.drawn.clone() else { return };
            let order = self.pick_order();
            let geom = &draw.geom;
            let items = BoardItems { unit: geom.board.unit, board: &geom.board, scene: &geom.scene, pick: &geom.pick, items: &geom.items, order: &order };
            self.pane.handle_event(cx, event, self.draw_bg.area(), &items)
        };
        for input in inputs {
            self.input(cx, input);
        }
        // A hovered pour's lit edge follows the pointer (uniforms only).
        if self.st.hover_pour && matches!(event, Event::MouseMove(_)) {
            self.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let started = Cx::monotonic_now();
        if self.st.heat_want && cx.has_global::<ElecLive>() {
            let c = cx.get_global::<ElecLive>().sim.index.generation;
            if let Some(slot) = self.st.heat.as_ref().filter(|slot| slot.field.generation != c) {
                let g = slot.field.generation;
                log!("fab.board: heat field of generation {g} is stale (current {c}), not drawn");
                self.set_heat(cx, None);
            }
        }
        let dpi = cx.current_dpi_factor();
        let rect = cx.walk_turtle(walk);
        // on whole device pixels, so a layer's texels land on the pane's pixels
        let rect = Rect { pos: dvec2((rect.pos.x * dpi).round() / dpi, (rect.pos.y * dpi).round() / dpi), size: rect.size };
        self.st.rect = rect;
        let theme = self.theme();
        self.draw_bg.color = vec4(theme.canvas[0], theme.canvas[1], theme.canvas[2], theme.canvas[3]);
        self.draw_bg.draw_abs(cx, rect);
        if !BoardDraws::has_templates(cx) {
            if let (Some(stroke), Some(pad), Some(tri)) = (
                StrokeTemplate::capture(cx, &self.draw_stroke.draw_vars),
                PadTemplate::capture(cx, &self.draw_pad.draw_vars),
                TriTemplate::capture(cx, &self.draw_tri.draw_vars),
            ) {
                BoardDraws::set_templates(cx, Templates { stroke, pad, tri });
            }
        }
        self.sync(cx);
        let Some(draw) = self.st.drawn.as_ref() else { return DrawStep::done() };
        let b = draw.geom.scene.bbox;
        let bounds = [b.min.x as f64 / 1e6, b.min.y as f64 / 1e6, b.max.x as f64 / 1e6, b.max.y as f64 / 1e6];
        if self.st.refit {
            self.pane.placed = false;
            self.st.refit = false;
        }
        self.pane.set_covered_inset(self.st.drawn_inset);
        if self.opening_view.as_ref().is_some_and(|opening| opening.prepare(&mut self.pane, &draw.geom, rect.size, bounds)) {
            self.opening_view = None;
            self.st.glide = None;
        }
        self.pane.place(cx, rect, Some(bounds));
        if self.status.visible() {
            let active = self.st.active.unwrap_or(LayerId::F_CU);
            let supplied = self.st.conn.as_ref().filter(|_| self.st.unit == Some(draw.unit));
            self.st.bar.sync_sources(cx, &draw.board, supplied);
            let track = self.st.track_w.unwrap_or(self.st.bar.default_track);
            let via = self.via_diameter(cx, draw.unit);
            // the hovered copper's voltage at the play head: one probe while the pointer is on an item
            let hover = self.st.hover.and_then(|_| self.pane.pointer_world()).and_then(|at| self.net_at(&draw, at))
                .and_then(|net| draw.board.nets.get(net.ix()).map(|n| (net, n.name.clone())))
                .and_then(|(net, name)| crate::live::flags::net_value_line(cx, fab_elec_core::NetRef { board: draw.unit, net }, &name));
            self.st.bar.sync_status(cx, &self.status, &draw.board, active, theme.ink(active), [to_mm(track), to_mm(via), self.grid_mm], hover);
        }
        self.draw_board(cx, rect);
        // The status row along the bottom; the strip over the board above it, the Check list and
        // the chip beside the strip.
        let bar = draw_bar(cx, scope, self.uid, &self.status, rect);
        let above = Rect { pos: rect.pos, size: dvec2(rect.size.x, (rect.size.y - bar).max(0.0)) };
        let strip = draw_strip(cx, scope, self.uid, &self.tools, above);
        if self.st.strip_at != strip {
            self.st.strip_at = strip;
            // The flags and route line drew before the strip and need its new rectangle.
            self.redraw(cx);
        }
        let left = if strip.size.x > 0.0 {
            (strip.pos.x + strip.size.x - rect.pos.x).max(0.0)
        } else { 0.0 };
        let inset = [left, 0.0, 0.0, bar];
        if self.st.drawn_inset != inset {
            self.st.drawn_inset = inset;
            if self.pane.keeps_fit && !cx.next_frame_is_pending(self.st.next_frame) {
                self.st.next_frame = cx.new_next_frame();
            }
        }
        self.fit_strip(cx, above);
        // the Checks editor lists the unit this pane shows: told once per change, which redraws
        // the window so the editor follows wherever it drew
        let unit = self.st.drawn.as_ref().map(|draw| draw.unit);
        if unit != self.st.told_unit && cx.has_global::<ElecLive>() {
            self.st.told_unit = unit;
            cx.get_global::<ElecLive>().shown_unit = unit;
            cx.redraw_all();
        }
        let us = (Cx::monotonic_now() - started) * 1e6;
        self.st.stats.frames += 1;
        self.st.stats.draw_us = us;
        if let Some(requested) = self.st.awaiting.take() {
            // the frame that draws a new picture: the edit is on screen once this frame presents
            self.st.stats.edit_ms = (Cx::monotonic_now() - requested) * 1e3;
            self.st.stats.pictures += 1;
            log!(
                "fab.board: picture on screen {:.1} ms after its board was handed in ({}, {} items)",
                self.st.stats.edit_ms,
                if self.st.stats.incremental { "rebatched" } else { "full build" },
                self.st.stats.edited
            );
        }
        self.st.window.push(us);
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::{arrange_fate, heat_texels, landed, outcome, shows_drop, ArrangeFate, HeatField, Outcome};
    use crate::snapshot::{Committed, DesignSnapshot, Refusal, StageFlags, StateOf, Tagged};
    use fab_elec_design::{By, Design};
    use fab_elec_core::{BoardId, P};
    use std::sync::Arc;

    fn snapshot(design: Arc<fab_elec_design::Design>) -> DesignSnapshot {
        DesignSnapshot {
            design, preview: None, preview_tag: None, preview_by: None, created: vec![], last_commit: None,
            sheets: vec![], proposal: None, job: None, conn: vec![], rules: vec![], fills: vec![], sysnets: None, export: vec![], benches: Arc::default(),
            violations: Tagged { unit: None, of: StateOf::Committed, generation: 0, value: Arc::new(vec![]) },
            awaiting: Arc::new(vec![]), last_refusal: None, stale: StageFlags::default(), head: String::new(),
            source_map: None, docs: None, catalog: None,
        }
    }

    #[test]
    fn a_label_plan_commits_only_onto_the_design_it_was_made_from() {
        let planned = Arc::new(Design::default());
        let unit = BoardId(0);
        let edited = Arc::new((*planned).clone());
        assert_eq!(arrange_fate(&planned, unit, Some(&planned), Some(unit)), ArrangeFate::Commit,
            "the design the plan read is still the committed one: it commits");
        assert_eq!(arrange_fate(&planned, unit, Some(&edited), Some(unit)), ArrangeFate::Again,
            "an edit landed while it was made, even one leaving equal content: planned again, never applied");
        assert_eq!(arrange_fate(&planned, unit, Some(&planned), Some(BoardId(1))), ArrangeFate::Drop,
            "the pane shows another board now: the plan goes");
    }

    #[test]
    fn a_landing_ends_on_its_commit_on_screen_or_its_refusal() {
        let design = Arc::new(fab_elec_fixtures::ledctl());
        let unit = design.units.iter().find(|u| u.board.is_some()).expect("ledctl has a board unit");
        let board = unit.board.clone().expect("the unit's board");
        let (item, from) = (board.placed[0].id, board.placed[0].at);
        let want = from.add(P::new(1_000_000, 0));
        let tag = 41;
        let mut moved = (*board).clone();
        moved.placed[0].at = want;
        moved.generation = board.generation + 1;
        assert!(shows_drop(item, want, &moved), "without a host, the picture with the item at its new place ends the drop");
        assert!(!shows_drop(item, want, &board), "without a host, the drop waits for that picture");

        let mut live = snapshot(design.clone());
        live.preview = Some(design.clone());
        live.preview_tag = Some(tag);
        assert_eq!(outcome(tag, unit.id, &live), Outcome::Waiting, "the gesture's own preview still live: it waits");
        assert_eq!(outcome(tag, unit.id, &snapshot(design.clone())), Outcome::Waiting,
            "no preview and no answer: the gap of a commit the host cannot carry, not a refusal");

        let mut after = (*design).clone();
        after.units.iter_mut().find(|u| u.id == unit.id).unwrap().board = Some(Arc::new(moved.clone()));
        let after = Arc::new(after);
        let mut committed = snapshot(after);
        committed.last_commit = Some(Arc::new(Committed { seq: 1, tag: Some(tag), by: By::Person, created: vec![] }));
        let taken = outcome(tag, unit.id, &committed);
        assert_eq!(taken, Outcome::Committed(moved.generation), "a commit of the tag names its board's generation");
        assert!(!landed(taken, &board), "a taken commit waits for the picture of its board");
        assert!(landed(taken, &moved), "the picture of the committed board ends the landing");

        let mut refused = live.clone();
        refused.preview_tag = Some(77);
        refused.last_refusal = Some(Arc::new(Refusal { seq: 1, label: "Move".into(), op: "board", text: "refused".into(), reason: "The item cannot be moved.".into(), site: None, sample: Some(tag) }));
        assert_eq!(outcome(tag, unit.id, &refused), Outcome::Over, "a refusal of the tag ends it, whatever else is previewed");
        refused.last_refusal = Some(Arc::new(Refusal { seq: 2, label: "Move".into(), op: "board", text: "refused".into(), reason: "The item cannot be moved.".into(), site: None, sample: Some(77) }));
        assert_eq!(outcome(tag, unit.id, &refused), Outcome::Waiting, "another sample's refusal does not end it");
        assert!(landed(Outcome::Over, &board) && !landed(Outcome::Waiting, &moved), "Over takes any picture, Waiting none");
    }

    #[test]
    fn heat_generation_and_coverage() {
        let field = HeatField {
            w: 2, h: 1, bounds_mm: [0.0, 0.0, 2.0, 1.0], generation: 7,
            values: vec![f32::NAN, 30.0], range: [20.0, 40.0],
            ..HeatField::default()
        };
        assert!(heat_texels(&field, Some(8)).is_none(), "a stale generation must not produce texels");
        assert!(heat_texels(&field, None).is_some(), "a host without a generation must accept the field");
        let Some(texels) = heat_texels(&field, Some(7)) else { panic!("a matching generation must produce texels") };
        assert_eq!(&texels[..2], &[0, 0], "a NaN cell must have zero ramp and coverage");
        assert_eq!(texels[2], 128, "a data cell's ramp must be rounded to the nearest byte");
        assert_eq!(texels[3], 255, "a data cell must have full coverage");
    }
}

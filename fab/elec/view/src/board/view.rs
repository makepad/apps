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
//! A running place job marks existing parts at their proposed poses in pane-state z.
//! `PlacePreview::sync` keys the marks and the retained displacement hairlines to the proposal
//! and drawn board Arcs. A new pair scans the ops, sorts the pose-op ids and looks up each
//! distinct placement on both boards. Slot 4 merges the sorted marks with its ordinary rows.
//! Between proposals the CPU submits at most one retained stroke list with camera uniforms.
//! Idle panes need no placement texture, uploads, line submissions or animation frames.
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
mod settings_panel;

use super::batch::Templates;
use super::checks::{check_marks, wave_may_clear, CheckMark};
use super::held::{HeldDraw, HeldLook, HeldVars, PENDING_MIX};
use super::rats::{self, RatsDraw};
use super::focus::{focus_box, focus_camera, Glide};
use super::draw::{
    DrawBoardGlow, DrawBoardLayer, DrawBoardPad, DrawBoardPadMax, DrawBoardStroke, DrawBoardStrokeMax, DrawBoardTri, DrawBoardTriMax,
};
use super::inst::{PadTemplate, StrokeTemplate, TriTemplate};
use super::lists::{BoardTextures, BoardUniforms, StreamList};
use super::heatkey::HeatKey;
use super::routeline::{cut_words, left_open, pushed, selected_unrouted, still_open, unpushed_words, why_open, HandLine, LeftOpen, LineButton, LineKey, RouteLine};
use super::tools::{apply_strip, flyout_anchor, method_name, strip_presses, Armed, BoardStripAction, BoardTool, PartEdit, StripPress, StripShow, Tidying, ARRANGEMENTS, METHODS, STRIP_PARTS};
use crate::canvas::tools::{draw_bar, draw_strip, over_strip, ToolClicks, STRIP_INSET, STRIP_TOP};
use super::marks::MarkOverlay;
use crate::live::flags::ProbeFlags;
use super::nodes::{probe_names, BoardItems, PressItems};
use super::plan;
use super::rail::RailLayer;
use super::scene::DrawScene;
use super::gesture::{classify, pour_cut, snap_target, track_widths, tune_hit, Press, Routing};
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
use fab_elec_board::{Anchor, Board, BoardOp, ConnItem, Connectivity, Field, Item, ItemKind, Placed};
use fab_elec_circuit::Violation;
use fab_elec_core::{to_mm, Angle, BBox, BlockId, BoardId, CompId, FeatureRef, ItemId, LayerId, NetId, Nm, Sel, Side, P};
use fab_elec_design::{BenchOp, Design, DesignOp, Focus, FocusKind, FocusOrigin, PartRef, Place, ProbeOf};
use fab_elec_route::{GestureKind, JobId, JobPhase, Method, Mode, Posture, ProposalSource, Reason, RouteProposal, Tier};
use fab_elec_sim::heat::HeatField;
use makepad_widgets_core::{makepad_platform::a11y_tree, SemanticLookup, *};
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Weak};

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
        settings: View{visible: false}
        status: View{visible: false}
        draw_line: mod.draw.DrawCanvasLine{}
        draw_box: mod.draw.DrawCanvasBox{}
        draw_polygon: mod.draw.DrawVector{}
        // The Measure ruler (`seg` from its start to its end), the Zone rectangle being dragged
        // (`zone` 1, `seg` its corners), drawn copper's rubber line (`zone` 2), or the Select
        // marquee (3 for a window, 4 for a crossing), in the pane's local points.
        draw_ruler: mod.draw.DrawQuad{
            seg: uniform(vec4(0.0))
            ink: uniform(vec4(1.0))
            zone: uniform(0.0)
            pixel: fn() {
                let p = self.pos * self.rect_size
                let sdf = Sdf2d.viewport(p)
                if self.zone > 2.5 {
                    sdf.rect(self.seg.x, self.seg.y, max(1.0, self.seg.z - self.seg.x), max(1.0, self.seg.w - self.seg.y))
                    sdf.fill_keep(vec4(self.ink.xyz, 0.08))
                    let dash = if self.zone > 3.5 {step(2.5, mod(p.x + p.y, 7.0))} else {1.0}
                    sdf.stroke(vec4(self.ink.rgb, self.ink.a * dash), 1.0)
                    return sdf.result
                }
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

/// The request tag returns the catalog outcome to the pane that issued it.
#[derive(Clone, Debug)]
pub struct StitchPress {
    pub tag: u64,
    pub args: Vec<(String, String)>,
}

impl StitchPress {
    pub const ACTION: &'static str = "elec.zone.stitch";
}

#[derive(Clone, Debug)]
pub struct BoardEditPress {
    pub tag: u64,
    pub action: &'static str,
    pub args: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct BoardEditDone {
    pub tag: u64,
    pub refusal: Option<String>,
}

/// A catalog action started or finished waiting for this board's committed pours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FillWaitNotice {
    pub unit: BoardId,
    pub action: &'static str,
    pub label: &'static str,
    pub waiting: bool,
}

struct PendingStitch {
    tag: u64,
    unit: BoardId,
    net: String,
    anchor: ChipAnchor,
    layer: ChipLayer,
    live: CanvasLive,
}

impl PendingStitch {
    fn status(&mut self, cx: &Cx, waiting: bool) {
        let text = if waiting { format!("waiting for the {} pour to refill", self.net) } else { format!("Stitching {}…", self.net) };
        self.layer.set(vec![Chip { id: 0, kind: ChipKind::Info, text, anchor: self.anchor, align: [0.0, 0.5] }],
            cx.seconds_since_app_start());
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
    #[find]
    #[live]
    settings: WidgetRef,
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
    draw_polygon: DrawVector,
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
    /// Reports for preview pictures still on screen or being built on the drawing worker.
    readout: RouteReadout,
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

struct ReadoutLabel {
    board: Weak<Board>,
    id: fab_elec_route::ProposalId,
    conn: Option<Weak<Connectivity>>,
    text: String,
}

#[derive(Default)]
struct RouteReadout {
    labels: Vec<ReadoutLabel>,
}

impl RouteReadout {
    fn cache(&mut self, board: &Arc<Board>, id: fab_elec_route::ProposalId, conn: Option<&Arc<Connectivity>>, text: impl FnOnce() -> String) -> bool {
        if self.labels.last().is_some_and(|old| old.id == id && old.board.as_ptr() == Arc::as_ptr(board)
            && old.conn.as_ref().map(Weak::as_ptr) == conn.map(Arc::as_ptr)) {
            return false;
        }
        self.labels.retain(|old| old.board.strong_count() != 0 && old.board.as_ptr() != Arc::as_ptr(board));
        self.labels.push(ReadoutLabel { board: Arc::downgrade(board), id, conn: conn.map(Arc::downgrade), text: text() });
        true
    }

    fn remember(&mut self, board: &Arc<Board>, id: fab_elec_route::ProposalId, report: &fab_elec_route::meander::Report) -> bool {
        self.cache(board, id, None, || tune_text(report))
    }

    fn remember_pair(&mut self, snap: &DesignSnapshot, net: NetId, mate: NetId) -> bool {
        let Some(p) = &snap.proposal else { return false };
        let Some(report) = &p.pair else { return false };
        let Some(board) = snap.preview.as_ref().and_then(|d| d.unit(p.unit)).and_then(|u| u.board.as_ref()) else { return false };
        // A board-first load can publish the preview while still holding the committed
        // connectivity. Only a preview result for this board can qualify either leg.
        let conn = if snap.checking(p.unit) { None } else {
            snap.conn.iter().find(|c| c.of == StateOf::Preview && c.unit == Some(p.unit)
                && c.generation == board.generation).map(|c| &c.value)
        };
        self.cache(board, p.id, conn, || {
            use fab_elec_board::length::{difference_text, net_lengths, Measurement};
            let pair = board.pairs.iter().find(|p| (p.p == net && p.n == mate) || (p.p == mate && p.n == net));
            let Some(pair) = pair else { return "the pair declaration has changed".to_string() };
            let nets = [pair.p, pair.n];
            let lengths = conn.map(|c| net_lengths(board, c, &nets));
            let leg = |name: &str, net, nm: i64| {
                let mut text = format!("{name} {:.3} mm", nm as f64 / 1e6);
                if let Some(length) = lengths.as_ref().and_then(|l| l.get(&net)) {
                    if length.partial { text.push_str(" (partial)"); }
                    if length.measurement == Measurement::NoStackup { text.push_str(" (vias not counted: no stackup)"); }
                }
                text
            };
            let mut text = format!("{}   {}", leg("P", pair.p, report.p_length), leg("N", pair.n, report.n_length));
            let tol = board.length_matches.iter().filter(|m| matches!(m.nets.as_slice(), [a, b] if [*a, *b] == nets || [*b, *a] == nets)).map(|m| m.tol).min();
            if let Some(tol) = tol {
                let (skew, allowed) = difference_text(report.skew, tol);
                text.push_str(&format!("\nskew {skew}   tolerance {allowed}"));
            } else {
                text.push_str(&format!("\nskew {:.3} mm", report.skew as f64 / 1e6));
            }
            if conn.is_none() { text.push_str("   checking connectivity"); }
            if let Some(block) = &report.blocked {
                let leg = if block.leg == pair.p { "P" } else { "N" };
                text.push_str(&format!("\n{leg} blocked at ({:.3}, {:.3}) mm by {}\nneeds {:.3} mm",
                    to_mm(block.at.x), to_mm(block.at.y), block.source.reason(), block.need as f64 / 1e6));
            }
            text
        })
    }

    fn shown(&self, board: &Arc<Board>) -> Option<&str> {
        self.labels.iter().rev().find(|label| label.board.as_ptr() == Arc::as_ptr(board)).map(|label| label.text.as_str())
    }
}

fn tune_text(report: &fab_elec_route::meander::Report) -> String {
    let mm = |nm: i64| nm as f64 / 1e6;
    let mut text = format!("reached {:.3} of {:.3} mm, {:.3} mm short", mm(report.reached), mm(report.target), mm(report.short_by));
    if report.partial { text.push_str(" (partial)"); }
    if report.no_stackup { text.push_str(" (vias not counted: no stackup)"); }
    text
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

/// Resolve a selection once when it or the geometry changes. Component selections from
/// other panes can name an unplaced part; keep its id so the key can explain the refusal.
fn part_target<'a>(board: &'a Board, sel: &Sel) -> Option<(CompId, Option<&'a Placed>)> {
    if sel.unit() != Some(board.unit) { return None; }
    let comp = match sel {
        Sel::Item(_, item) => {
            let p = board.placed_item(*item)?;
            return Some((p.comp, Some(p)));
        }
        Sel::Comp(c) | Sel::Feature(FeatureRef::Part(c) | FeatureRef::Body(c) | FeatureRef::Pad(c, _) | FeatureRef::Port(c, _)) => c.comp,
        Sel::Pin(p) => p.comp,
        _ => return None,
    };
    Some((comp, board.placed_of(comp)))
}

#[derive(Default)]
struct PartSelection {
    ids: Vec<CompId>,
    items: Vec<ItemId>,
    transform: bool,
    arrange: bool,
}

impl PartSelection {
    fn new(board: &Board, selected: &[Sel]) -> Self {
        let mut parts = Self::default();
        let mut seen = HashSet::new();
        let mut editable = true;
        let mut first_box = None;
        for (comp, placed) in selected.iter().filter_map(|s| part_target(board, s)) {
            if !seen.insert(comp) { continue; }
            parts.ids.push(comp);
            if let Some(p) = placed {
                parts.items.push(p.id);
                editable &= plan::part_refusal(p).is_none();
                if selected.len() >= 2 && !parts.arrange {
                    let bounds = plan::courtyard_box(p);
                    parts.arrange = *first_box.get_or_insert(bounds) != bounds;
                }
            } else {
                editable = false;
            }
        }
        parts.transform = editable && !parts.ids.is_empty();
        parts.arrange &= editable;
        parts
    }
}

/// The overlay on the drawn board. Weak keys keep pointer identity without owning an old board.
struct PlacePreview {
    job: JobId,
    proposal: Weak<RouteProposal>,
    board: Weak<Board>,
    moved: Vec<ItemId>,
    lines: Option<StreamList>,
}

impl PlacePreview {
    /// Returns whether the pane needs to rewrite its rows. A newer snapshot can arrive before
    /// its picture. Keep the old picture's overlay while that same job still owns the preview.
    fn sync(slot: &mut Option<Self>, drawn: &Arc<Board>, source: Option<(JobId, &Arc<RouteProposal>, &DesignSnapshot)>,
        stroke: impl FnOnce() -> Option<StrokeTemplate>) -> bool {
        let Some((job, p, snap)) = source else {
            return slot.take().is_some_and(|old| !old.moved.is_empty());
        };
        let same_board = p.unit == drawn.unit && slot.as_ref().is_some_and(|old| old.board.as_ptr() == Arc::as_ptr(drawn) && old.job == job);
        if same_board && slot.as_ref().is_some_and(|old| old.proposal.as_ptr() == Arc::as_ptr(p)) {
            return false;
        }
        let preview = snap.preview.as_ref().and_then(|d| d.unit(p.unit)?.board.as_ref());
        let old = slot.get_or_insert_with(|| Self {
            job, proposal: Weak::new(), board: Weak::new(), moved: Vec::new(), lines: None,
        });
        let mut moved = Vec::new();
        let mut lines = None;
        if let Some(base) = preview.filter(|b| Arc::ptr_eq(b, drawn))
            .and_then(|_| snap.design.unit(p.unit)?.board.as_deref())
        {
            let Some(t) = stroke() else { return false };
            (moved, lines) = Self::moves(base, drawn, &p.ops, &t);
        } else if same_board && preview.is_some() {
            old.proposal = Arc::downgrade(p);
            return false;
        }
        let changed = old.moved != moved;
        *old = Self { job, proposal: Arc::downgrade(p), board: Arc::downgrade(drawn), moved, lines };
        changed
    }

    fn moves(base: &Board, drawn: &Board, ops: &[BoardOp], stroke: &StrokeTemplate) -> (Vec<ItemId>, Option<StreamList>) {
        let mut moved: Vec<_> = ops.iter().filter_map(|op| match op {
            BoardOp::SetField { item, field: Field::At(_) | Field::Rot(_) | Field::Side(_) }
                if item.0 < base.next_item => Some(*item),
            _ => None,
        }).collect();
        moved.sort_unstable();
        moved.dedup();
        let mut data = Vec::new();
        moved.retain(|&id| {
            let Some((from, to)) = base.placed_item(id).zip(drawn.placed_item(id)) else { return false };
            if from.at != to.at {
                rats::append_line(&mut data, from.at, to.at, stroke);
                return true;
            }
            from.rot != to.rot || from.side != to.side
        });
        (moved, rats::list(data, stroke))
    }
}

/// The Select marquee being dragged: its box (world mm, the press at one corner) and the
/// selection at its press, which Shift adds to and a cancelled press puts back.
struct BoardMarquee {
    rect: [f64; 4],
    crossing: bool,
    before: Vec<Sel>,
}

#[derive(Default)]
struct PaneState {
    unit: Option<BoardId>,
    stitch_target: Option<(NetId, Option<ItemId>)>,
    stitch: Option<PendingStitch>,
    settings: settings_panel::SettingsPanel,
    /// Selection labels are resolved only on selection or geometry changes, never per frame.
    selected: Vec<Sel>,
    parts: PartSelection,
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
    /// The ordinary slot-4 rows, before the place overlay. None until another feature needs slot 4.
    ordinary_rows: Option<Vec<(u32, [f32; 4])>>,
    place_preview: Option<PlacePreview>,
    /// A completed picture that a newer place proposal has already superseded.
    skipped_place: Option<(Weak<RouteProposal>, Weak<Board>)>,
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
    /// preview proposal awaiting a wave, whether the unit waits for its whole-board checks).
    marks: Vec<CheckMark>,
    marks_key: Option<(u64, usize, u64, BoardId, usize, bool)>,
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
    /// Logical pins when the Tune press landed, before the drag threshold.
    tune_press: Option<(BoardId, Arc<fab_elec_circuit::Circuit>)>,
    /// Both starting terminals are picked on this picture, even if a new board arrives before the drag threshold.
    pair_press: Option<(Arc<BoardDraw>, Arc<fab_elec_board::RuleMatrix>, LayerId)>,
    /// The Measure tool's ruler (from, to), world mm, drawn until the next press or Escape.
    ruler: Option<[[f64; 2]; 2]>,
    /// The shape being drawn, local to this pane and unit until a close or drag release.
    shape: ShapeGesture,
    /// The Select marquee while it is dragged.
    marquee: Option<BoardMarquee>,
    /// The strip's height as one column, measured while it stands so: past the pane's height
    /// it stands in two (its board actions beside the rest).
    strip_tall: f64,
}

/// The raw first press selects a Zone's net. The vertices themselves lie on the board grid.
#[derive(Debug)]
struct DrawnShape {
    from: [f64; 2],
    points: Vec<P>,
}

#[derive(Default)]
enum ShapeGesture {
    #[default]
    Idle,
    Polygon(DrawnShape),
    Rectangle { from: [f64; 2], to: [f64; 2] },
}

fn snapped(at: [f64; 2], grid_mm: f64) -> P {
    let grid = (grid_mm.max(0.001) * 1e6).round() as i64;
    let snap = |mm: f64| ((mm * 1e6 / grid as f64).round() as i64 * grid) as Nm;
    P::new(snap(at[0]), snap(at[1]))
}

const CLOSE_RADIUS: f64 = 6.0; // logical screen points, independent of zoom

/// Build the press from the selection on the displayed board. A direct transform never
/// requests the catalog's optional move to the nearest legal placement.
pub fn part_gesture(design: &Design, unit: BoardId, parts: &[CompId], edit: PartEdit) -> ViewAction {
    use crate::snapshot::Invocation;
    let (name, mut args, planned) = match edit {
        PartEdit::Rotate(by) => {
            let degrees = if by > Angle::HALF { by.degrees() - 360.0 } else { by.degrees() };
            ("elec.part.rotate", vec![("by", degrees.to_string())], plan::rotate(design, unit, parts, by))
        }
        PartEdit::Flip => ("elec.part.flip", vec![], plan::flip(design, unit, parts)),
        PartEdit::Arrange(plan::Arrangement::Align(edge)) => ("elec.part.align", vec![("edge", edge.name().into())], plan::align(design, unit, parts, edge)),
        PartEdit::Arrange(plan::Arrangement::Distribute(axis)) => ("elec.part.distribute", vec![("axis", axis.name().into())], plan::distribute(design, unit, parts, axis)),
    };
    let action = match planned {
        Ok(edit) => ViewAction::Ops { ops: edit.ops, place: edit.place, label: edit.label },
        Err(why) => ViewAction::PlanRefused(why),
    };
    let Some(unit) = design.unit(unit) else { return action };
    let Some(names) = parts.iter().map(|&id| Invocation::part(unit, id)).collect::<Option<Vec<_>>>() else { return action };
    args.push(("parts", names.join(", ")));
    args.push(("legalise", "off".into()));
    Invocation::new(name, &unit.name, args).attach(action)
}

fn keepout_layer(board: &Board, active: Option<LayerId>) -> LayerId {
    let active = active.unwrap_or(LayerId::F_CU);
    if board.copper_set().has(active) { active }
    else if active.side() == fab_elec_core::LayerSide::Back { LayerId::B_CU }
    else { LayerId::F_CU }
}

impl DrawnShape {
    fn near_start(&self, at: [f64; 2], px_per_mm: f64) -> bool {
        let first = self.points[0];
        (at[0] - to_mm(first.x)).hypot(at[1] - to_mm(first.y)) * px_per_mm <= CLOSE_RADIUS
    }

    fn plan(&self, tool: BoardTool, design: &Design, board: &Board, active: Option<LayerId>, net: Option<NetId>) -> Result<plan::Plan, String> {
        match tool {
            BoardTool::Zone => {
                let layer = active.filter(|l| board.copper_set().has(*l)).unwrap_or(LayerId::F_CU);
                plan::zone(design, board.unit, &self.points, net, layer)
            }
            BoardTool::Outline => plan::outline(design, board.unit, &self.points),
            BoardTool::Keepout => {
                let layer = keepout_layer(board, active);
                plan::keepout(design, board.unit, &self.points, fab_elec_core::LayerSet::one(layer), fab_elec_parts::Forbid::ALL, "Keep clear")
            }
            BoardTool::Cutout => plan::cutout(design, board.unit, &self.points, "Cutout"),
            _ => unreachable!("only shape tools finish a drawing"),
        }
    }
}

/// Complete a polygon or drag rectangle using the snapped integer vertices of the drawing.
pub fn shape_gesture(tool: BoardTool, design: &Design, board: &Board, active: Option<LayerId>, net: Option<NetId>, points: Vec<P>) -> ViewAction {
    let shape = DrawnShape { from: [0.0; 2], points };
    let action = match shape.plan(tool, design, board, active, net) {
        Ok(edit) => ViewAction::Ops { ops: edit.ops, place: vec![], label: edit.label },
        Err(why) => ViewAction::PlanRefused(why),
    };
    let mut fields = Vec::new();
    let name = match tool {
        BoardTool::Outline => "elec.board.outline",
        BoardTool::Cutout => "elec.cutout.add",
        BoardTool::Keepout => {
            fields.push(("layers", keepout_layer(board, active).name()));
            fields.push(("reason", "Keep clear".into()));
            "elec.keepout.add"
        }
        _ => return action,
    };
    let Some(unit) = design.unit(board.unit) else { return action };
    fields.push(("points", shape.points.iter().map(|p| format!("{} {}", to_mm(p.x), to_mm(p.y))).collect::<Vec<_>>().join(", ")));
    crate::snapshot::Invocation::new(name, &unit.name, fields).attach(action)
}

impl ShapeGesture {
    /// The canvas sends a release for each tap of a double-click. Its first tap has already
    /// supplied the last vertex. A second tap after first-point closure starts nothing.
    fn click(&mut self, at: [f64; 2], grid_mm: f64, px_per_mm: f64, clicks: u32) -> bool {
        match self {
            Self::Polygon(shape) => {
                if clicks >= 2 || shape.near_start(at, px_per_mm) { return true; }
                shape.points.push(snapped(at, grid_mm));
            }
            Self::Idle if clicks == 1 => {
                *self = Self::Polygon(DrawnShape { from: at, points: vec![snapped(at, grid_mm)] });
            }
            _ => {}
        }
        false
    }

    fn backspace(&mut self) {
        if let Self::Polygon(shape) = self {
            shape.points.pop();
            if shape.points.is_empty() { self.cancel(); }
        }
    }

    fn drag(&mut self, from: [f64; 2], to: [f64; 2]) {
        *self = Self::Rectangle { from, to };
    }

    fn cancel(&mut self) { *self = Self::Idle; }

    fn finish(&mut self, grid_mm: f64) -> Option<DrawnShape> {
        match std::mem::take(self) {
            Self::Idle => None,
            Self::Polygon(shape) => Some(shape),
            Self::Rectangle { from, to } => {
                let bounds = fab_elec_core::BBox::new(snapped(from, grid_mm), snapped(to, grid_mm));
                Some(DrawnShape { from, points: plan::rect_points(bounds).to_vec() })
            }
        }
    }
}

impl PaneState {
    /// A moved-only partial has no new ids. A threshold above every drawable id enables just
    /// the marked rows. The holes pass disables both with its usual negative threshold.
    fn place_fresh_from(&self, fresh: Option<u32>) -> Option<u32> {
        fresh.or_else(|| self.place_preview.as_ref().filter(|p| !p.moved.is_empty()).map(|_| 1 << 24))
    }

    /// Return a texture owed solely to the finished overlay for release by its owner.
    fn sync_place(&mut self, drawn: &Arc<Board>, source: Option<(JobId, &Arc<RouteProposal>, &DesignSnapshot)>,
        stroke: impl FnOnce() -> Option<StrokeTemplate>) -> Option<ItemStates> {
        if PlacePreview::sync(&mut self.place_preview, drawn, source, stroke) {
            if self.ordinary_rows.is_none() && self.place_preview.as_ref().is_none_or(|p| p.moved.is_empty()) {
                return self.dims.take();
            }
            self.write_pane_rows(drawn.next_item as usize);
        }
        None
    }

    fn set_ordinary_rows(&mut self, count: usize, rows: Vec<(u32, [f32; 4])>) {
        self.ordinary_rows = Some(rows);
        self.write_pane_rows(count);
    }

    fn write_pane_rows(&mut self, count: usize) {
        let ordinary = self.ordinary_rows.as_deref().unwrap_or_default();
        let moved = self.place_preview.as_ref().map_or(&[][..], |p| p.moved.as_slice());
        let mut rows = Vec::with_capacity(ordinary.len() + moved.len());
        let mut rest = ordinary.iter().copied().peekable();
        for &ItemId(id) in moved {
            while let Some(row) = rest.next_if(|row| row.0 < id) { rows.push(row); }
            let (_, mut texel) = rest.next_if(|row| row.0 == id).unwrap_or((id, [0.0; 4]));
            texel[2] = 1.0;
            rows.push((id, texel));
        }
        rows.extend(rest);
        self.dims.get_or_insert_with(ItemStates::new).write(count, rows);
    }

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
    /// "Place them anyway", while the chips offer it: the scope its press places with the panel
    /// free. It stays through hovers until pressed, a click elsewhere, Escape or another note.
    offer: Option<(BoardId, Vec<CompId>)>,
}

/// The chip of an offer that its press acts on.
const OFFER_CHIP: u32 = 1;

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
        self.cancel_shape();
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
            self.cancel_shape();
            self.st.settings = Default::default();
            self.st.unit = unit;
            self.st.selected.clear();
            self.st.parts = PartSelection::default();
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
        self.sync_stitch_target(cx);
        self.redraw(cx);
    }

    fn select_focus(&mut self, cx: &mut Cx, focus: &Focus) {
        let selected = if focus.kind == FocusKind::Net { &[][..] } else { std::slice::from_ref(&focus.sel) };
        self.set_selection(cx, selected);
    }

    /// Refresh names once when the selection or its drawn geometry changes. Component/feature
    /// joins may visit placements here; drawing only reads the resulting bounded string.
    fn selection_summary(&mut self) {
        self.st.parts = self.st.drawn.as_ref().filter(|d| Some(d.unit) == self.st.unit)
            .map_or_else(PartSelection::default, |d| PartSelection::new(&d.board, &self.st.selected));
        self.selection_marks();
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

    /// The selected parts and any chip marks share the pane's emphasis rows. A drag has
    /// its own rows until it ends. Drawing reads the texture without visiting the selection.
    fn selection_marks(&mut self) {
        if self.st.hold().is_some() { return; }
        let Some(draw) = &self.st.drawn else { return };
        if self.st.parts.items.is_empty() && self.st.marked.is_empty() && self.st.ordinary_rows.is_none() { return; }
        let mut rows: Vec<_> = self.st.parts.items.iter().chain(&self.st.marked)
            .map(|id| (id.0, [0.0, 1.0, 0.0, 0.0])).collect();
        rows.sort_unstable_by_key(|row| row.0);
        rows.dedup_by_key(|row| row.0);
        self.st.set_ordinary_rows(draw.board.next_item as usize, rows);
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
        // An already superseded picture may finish building. Keep the displayed round until
        // the board for the new round is ready, so its marks and geometry change together.
        if let Some((_, Some(p))) = self.route_job(cx) {
            if p.metrics.place.is_some() {
                if self.st.skipped_place.as_ref().is_some_and(|(proposal, board)|
                    proposal.as_ptr() == Arc::as_ptr(&p) && board.as_ptr() == Arc::as_ptr(&draw.board))
                {
                    return;
                }
                let snap = &cx.get_global::<ElecLive>().snap;
                let is_drawn = |d: &Design| d.unit(unit).and_then(|u| u.board.as_ref()).is_some_and(|b| Arc::ptr_eq(b, &draw.board));
                if !is_drawn(&snap.design) && !snap.preview.as_ref().is_some_and(|d| is_drawn(d)) {
                    self.st.skipped_place = Some((Arc::downgrade(&p), Arc::downgrade(&draw.board)));
                    return;
                }
            }
        }
        self.st.skipped_place = None;
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
        self.sync_stitch_target(cx);
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
        if let Some(lines) = self.st.place_preview.as_mut().and_then(|p| p.lines.as_mut()) {
            let stroke = &mut self.draw_stroke.draw_vars;
            let mut ink = [0.0; 4];
            stroke.get_uniform(cx, live_id!(fresh_ink), &mut ink);
            let u = BoardUniforms { cam: base.cam, clip: base.clip, thin_alpha: 1.0, layer_ink: ink, ..BoardUniforms::default() };
            lines.draw(cx, stroke, &BoardTextures::default(), &u);
        }
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
        match &input {
            CanvasInput::Click { at, .. } if self.click_offer(cx, *at) => return,
            // Escape takes the offer away first: one step out.
            CanvasInput::Escape if matches!(self.st.shape, ShapeGesture::Idle) && self.st.chips.as_ref().is_some_and(|chips| chips.offer.is_some()) => {
                if let Some(chips) = self.st.chips.as_mut() {
                    chips.offer = None;
                }
                self.clear_note(cx);
                return;
            }
            _ => {}
        }
        match input {
            CanvasInput::Press { key: Sel::Item(unit, _), down: true } if self.st.armed.tool == BoardTool::Tune => {
                self.st.tune_press = self.design(cx).and_then(|d| Some((unit, d.unit(unit)?.circuit.clone())));
            }
            CanvasInput::Press { key: Sel::Item(unit, _), down: true } if self.st.armed.tool == BoardTool::RoutePair => {
                self.st.pair_press = self.st.drawn.as_ref().filter(|d| d.unit == unit).and_then(|d| {
                    let snap = &cx.get_global_ref::<ElecLive>()?.snap;
                    let rules = snap.of(&snap.rules, unit)?;
                    Some((d.clone(), rules.value.clone(), self.st.active.unwrap_or(LayerId::F_CU)))
                });
            }
            CanvasInput::Press { down: false, .. } => {
                self.st.tune_press = None;
                self.st.pair_press = None;
            }
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
            CanvasInput::Key { code: KeyCode::ReturnKey | KeyCode::NumpadEnter, .. }
                if matches!(self.st.shape, ShapeGesture::Polygon(_)) => self.finish_shape(cx),
            CanvasInput::Key { code: KeyCode::Backspace, .. } if self.st.armed.tool.draws_shape() => {
                self.st.shape.backspace();
                self.redraw(cx);
            }
            CanvasInput::Key { code, modifiers, .. } if self.route_key(cx, code, modifiers) => {}
            CanvasInput::Key { code: KeyCode::KeyX, modifiers, .. }
                if modifiers.shift && !(modifiers.control || modifiers.alt || modifiers.logo) => self.set_armed(cx, self.st.armed.key(BoardTool::RoutePair)),
            CanvasInput::Key { code: KeyCode::KeyR, modifiers, under }
                if self.st.armed.tool == BoardTool::Select && !(modifiers.control || modifiers.alt || modifiers.logo) => {
                let by = if modifiers.shift { Angle::QUARTER.neg() } else { Angle::QUARTER };
                self.edit_parts(cx, PartEdit::Rotate(by), under.as_ref());
            }
            CanvasInput::Key { code, modifiers, under } if !(modifiers.shift || modifiers.control || modifiers.alt || modifiers.logo) => match code {
                KeyCode::Delete | KeyCode::Backspace => self.key_delete(cx, under),
                KeyCode::KeyF => self.key_pour(cx),
                KeyCode::KeyB if self.st.armed.tool == BoardTool::Select => self.edit_parts(cx, PartEdit::Flip, under.as_ref()),
                KeyCode::KeyA => self.key_labels(cx),
                KeyCode::KeyS => self.flip_shove(cx),
                // The strip's tools: X arms Route, V goes back to Select.
                KeyCode::KeyT => self.set_armed(cx, self.st.armed.key(BoardTool::Tune)),
                KeyCode::KeyX => self.set_armed(cx, self.st.armed.key(BoardTool::Route)),
                KeyCode::KeyV => self.set_armed(cx, self.st.armed.key(BoardTool::Select)),
                _ => {}
            },
            // Escape with no drag running: away with the ruler, and out of the armed tool.
            CanvasInput::Escape if self.st.armed != Armed::default() || self.st.ruler.is_some() => {
                self.st.ruler = None;
                self.cancel_shape();
                self.set_armed(cx, self.st.armed.escape());
                self.redraw(cx);
            }
            // A shape starts on a click release; a press that passed the drag threshold has no click.
            CanvasInput::Click { at, .. } if self.st.armed.tool == BoardTool::Via => self.place_via(cx, at),
            CanvasInput::Click { at, clicks, .. } if self.st.armed.tool.draws_shape() => {
                if self.st.drawn.as_ref().map(|d| d.unit) != self.st.unit { return; }
                if self.st.shape.click(at, self.grid_mm, self.pane.cam.px_per_unit, clicks) {
                    self.finish_shape(cx);
                } else {
                    self.redraw(cx);
                }
            }
            CanvasInput::Click { .. } if self.st.armed.tool == BoardTool::Measure => {}
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
            CanvasInput::Marquee { rect, done, shift, crossing } if self.st.armed.tool == BoardTool::Select =>
                self.marquee(cx, rect, done, shift, crossing),
            CanvasInput::Click { key: Some(Sel::Item(_, _)), at, shift: true, .. }
                if self.st.armed.tool == BoardTool::Select => {
                let Some(draw) = self.st.drawn.clone() else { return };
                // On a dense board the topmost item under the press is often copper or a pour:
                // shift takes the first part under it.
                let Some((sel, comp)) = self.part_under(&draw, at) else { return };
                let mut selected = self.st.selected.clone();
                if self.st.parts.ids.contains(&comp) {
                    selected.retain(|s| !part_target(&draw.board, s).is_some_and(|(id, _)| id == comp));
                } else {
                    selected.push(sel);
                }
                self.set_selection(cx, &selected);
                self.publish_selection(cx);
            }
            CanvasInput::Click { key: None, shift: false, .. } | CanvasInput::Escape
                if self.st.armed.tool == BoardTool::Select && !self.st.selected.is_empty() => {
                self.set_selection(cx, &[]);
                cx.widget_action(self.uid, ViewAction::Unfocus);
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

    /// One plan on the committed board per press. The strip acts on the selection;
    /// only a key without selected parts falls back to the pointer.
    fn edit_parts(&mut self, cx: &mut Cx, edit: PartEdit, under: Option<&Sel>) {
        let (Some(unit), Some(design)) = (self.st.unit, self.design(cx)) else { return };
        let Some(board) = design.unit(unit).and_then(|u| u.board.as_deref()) else { return };
        let hovered = under.and_then(|sel| part_target(board, sel)).map(|(comp, _)| comp);
        let parts = if self.st.parts.ids.is_empty() { hovered.as_slice() } else { &self.st.parts.ids };
        self.send_part_or_shape(cx, part_gesture(&design, unit, parts, edit));
    }

    fn send_part_or_shape(&mut self, cx: &mut Cx, action: ViewAction) {
        let inner = if let ViewAction::Planned { action, .. } = &action { action.as_ref() } else { &action };
        match inner {
            ViewAction::Ops { .. } => self.clear_note(cx),
            ViewAction::PlanRefused(why) => self.note(cx, ChipKind::Refused, why.clone(), Vec::new()),
            _ => return,
        }
        crate::snapshot::Invocation::send(cx, self.uid, action);
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

    /// Left to right takes parts wholly inside the window. Right to left takes parts whose
    /// visible shape boxes meet the band, excluding footprint text. Shift adds to the press
    /// selection. Both preview locally and publish on release. Both pay for the same layer-index
    /// query, O(h log h) to sort h hits, item lookups and the selection update. A window also
    /// checks each candidate part's full visible box.
    fn marquee(&mut self, cx: &mut Cx, rect: [f64; 4], done: bool, shift: bool, crossing: bool) {
        let Some(draw) = self.st.drawn.clone() else { return };
        let Some(unit) = self.st.unit.filter(|unit| *unit == draw.unit) else { return };
        let before = match self.st.marquee.take() {
            Some(marquee) => marquee.before,
            None => self.st.selected.clone(),
        };
        let order = self.pick_order();
        let geom = &draw.geom;
        let items = BoardItems { unit: geom.board.unit, board: &geom.board, scene: &geom.scene, pick: &geom.pick, items: &geom.items, order: &order };
        let mut selected = if shift { before.clone() } else { Vec::new() };
        let mut held: HashSet<CompId> = selected.iter().filter_map(|sel| part_target(&draw.board, sel).map(|(comp, _)| comp)).collect();
        for (_, comp) in items.parts_in_band(rect, crossing) {
            if held.insert(comp) {
                selected.push(Sel::Comp(fab_elec_core::CompRef { board: unit, comp }));
            }
        }
        self.set_selection(cx, &selected);
        if done {
            self.publish_selection(cx);
        } else {
            self.st.marquee = Some(BoardMarquee { rect, crossing, before });
        }
        self.redraw(cx);
    }

    /// A Select marquee whose press ended without its release (Escape, a cancelled touch): the
    /// selection at its press comes back. O(selected).
    fn end_marquee(&mut self, cx: &mut Cx) {
        if self.pane.pressed() { return; }
        let Some(marquee) = self.st.marquee.take() else { return };
        self.set_selection(cx, &marquee.before);
        self.redraw(cx);
    }

    /// Tell the other views what this pane selected: nothing, one item (its focus), or a group
    /// (the views' shared selection, as the schematic's marquee sends it). O(selected).
    fn publish_selection(&self, cx: &mut Cx) {
        match self.st.selected.as_slice() {
            [] => cx.widget_action(self.uid, ViewAction::Unfocus),
            [sel] => {
                let part = self.st.drawn.as_ref().is_some_and(|draw| part_target(&draw.board, sel).is_some());
                let kind = if part { FocusKind::Part } else { FocusKind::Board };
                cx.widget_action(self.uid, ViewAction::Focus(Focus { kind, sel: sel.clone(), origin: FocusOrigin::Board }));
            }
            group => cx.widget_action(self.uid, crate::ViewState { selected: group.to_vec(), emphasis: Vec::new() }),
        }
    }

    /// The first part under world `at` on the drawn board (`BoardItems::part_at`), as a selection
    /// and its component: what a shift-click adds or takes away. O(hits).
    fn part_under(&self, draw: &BoardDraw, at: [f64; 2]) -> Option<(Sel, CompId)> {
        let order = self.pick_order();
        let geom = &draw.geom;
        let items = BoardItems { unit: draw.unit, board: &geom.board, scene: &geom.scene, pick: &geom.pick, items: &geom.items, order: &order };
        items.part_at(at, HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12)).map(|(item, comp)| (Sel::Item(draw.unit, item), comp))
    }

    /// The net of the copper at world `at` on the drawn board, topmost first. O(hits).
    fn net_at(&self, draw: &BoardDraw, at: [f64; 2]) -> Option<NetId> {
        let order = self.pick_order();
        let geom = &draw.geom;
        let items = BoardItems { unit: draw.unit, board: &geom.board, scene: &geom.scene, pick: &geom.pick, items: &geom.items, order: &order };
        items.probe_at(at, HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12)).map(|hit| hit.net)
    }

    fn sync_stitch_target(&mut self, cx: &mut Cx) {
        let target = self.st.drawn.as_ref().filter(|draw| Some(draw.unit) == self.st.unit).and_then(|draw| {
            if let [Sel::Item(_, id)] = self.st.selected.as_slice() {
                if let Some((ItemKind::Zone, i)) = draw.board.locate(*id) {
                    return draw.board.zones[i].net.map(|net| (net, Some(*id)));
                }
            }
            let net = self.net_at(draw, self.pane.pointer_world()?)?;
            draw.board.zones.iter().any(|z| z.net == Some(net)).then_some((net, None))
        });
        if self.st.stitch_target != target {
            self.st.stitch_target = target;
            self.tools.widget(cx, ids!(stitch_options)).set_visible(cx, target.is_some());
            self.redraw(cx);
        }
    }

    fn stitch_pressed(&mut self, cx: &mut Cx) {
        if self.st.stitch.is_some() { return }
        let (Some((net, zone)), Some(draw)) = (self.st.stitch_target, self.st.drawn.clone()) else { return };
        let Some(design) = self.design(cx) else { return };
        let Some(unit) = design.unit(draw.unit).filter(|unit| Some(unit.id) == self.st.unit) else { return };
        let Some(network) = draw.board.nets.get(net.ix()) else { return };
        let pitch = self.tools.widget(cx, ids!(stitch_pitch)).text();
        let Some(mm) = super::status::parse_mm(&pitch, 0.0, to_mm(Nm::MAX)).filter(|m| fab_elec_core::mm(*m) > 0) else {
            return self.note(cx, ChipKind::Refused, "enter a positive stitch pitch in millimetres".into(), Vec::new());
        };
        let pad = self.via_diameter(cx, unit.id);
        let drill = self.via_drill(cx, &draw.board, unit.id, Some(net));
        let mut args = vec![("unit".into(), unit.name.clone()), ("net".into(), network.name.clone()),
            ("pitch".into(), mm.to_string()), ("pad".into(), to_mm(pad).to_string()), ("drill".into(), to_mm(drill).to_string())];
        if let Some(id) = zone { args.push(("zone".into(), id.0.to_string())); }
        self.clear_note(cx);
        let tag = next_tag();
        let button = self.tools.widget(cx, ids!(a_stitch)).area().rect(cx);
        let anchor = ChipAnchor::Screen { at: [self.st.strip_at.pos.x + self.st.strip_at.size.x + 8.0, button.pos.y + button.size.y * 0.5] };
        let mut pending = PendingStitch { tag, unit: unit.id, net: network.name.clone(), anchor, layer: ChipLayer::default(), live: CanvasLive::off(cx) };
        pending.status(cx, false);
        self.st.stitch = Some(pending);
        self.redraw(cx);
        cx.action(StitchPress { tag, args });
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
            None => self.st.chips.insert(Chips { layer: ChipLayer::default(), live: CanvasLive::off(cx), shown: false, timer: Timer::empty(), offer: None }),
        };
        chips.offer = None;
        chips.layer.set(vec![Chip { id: 0, kind, text, anchor: ChipAnchor::World { at, dx: 14.0, dy: 14.0 }, align: [0.0, 0.0] }], cx.seconds_since_app_start());
        chips.shown = true;
        chips.timer = cx.start_timeout(NOTE_SECONDS);
        if !marked.is_empty() {
            self.st.marked = marked;
            self.selection_marks();
        }
        self.redraw(cx);
    }

    fn clear_note(&mut self, cx: &mut Cx) {
        if !self.st.marked.is_empty() {
            self.st.marked.clear();
            self.selection_marks();
        }
        let Some(chips) = self.st.chips.as_mut().filter(|chips| chips.shown && chips.offer.is_none()) else { return };
        chips.shown = false;
        chips.layer.set(Vec::new(), cx.seconds_since_app_start());
        cx.stop_timer(chips.timer);
        self.redraw(cx);
    }

    /// The strip's Place held standing panel parts (`plan::PlacePress`): two chips over them say
    /// why, and offer "Place them anyway", which places the whole scope with the panel free. The
    /// offer stays until it is pressed, a click lands elsewhere, Escape, or another note.
    fn offer_place(&mut self, cx: &mut Cx, unit: BoardId, press: plan::PlacePress) {
        let Some(design) = self.design(cx) else { return };
        let Some(board) = design.unit(unit).and_then(|found| found.board.as_deref()) else { return };
        let bbox = press.held.iter().filter_map(|&comp| board.placed_of(comp)).fold(BBox::EMPTY, |b, placed| b.union(placed.bbox()));
        if bbox.is_empty() {
            return;
        }
        let center = bbox.center();
        let at = [center.x as f64 / 1e6, center.y as f64 / 1e6];
        let why = match press.held.len() {
            1 => "1 panel part stays where it stands: the case is built round it".to_owned(),
            n => format!("{n} panel parts stay where they stand: the case is built round them"),
        };
        log!("fab.board: place held {} panel parts on unit {}", press.held.len(), unit.0);
        self.clear_note(cx);
        let chips = match &mut self.st.chips {
            Some(chips) => chips,
            None => self.st.chips.insert(Chips { layer: ChipLayer::default(), live: CanvasLive::off(cx), shown: false, timer: Timer::empty(), offer: None }),
        };
        cx.stop_timer(chips.timer);
        chips.layer.set(vec![
            Chip { id: 0, kind: ChipKind::Info, text: why, anchor: ChipAnchor::World { at, dx: 14.0, dy: 14.0 }, align: [0.0, 0.0] },
            Chip { id: OFFER_CHIP, kind: ChipKind::Value, text: "Place them anyway".into(), anchor: ChipAnchor::World { at, dx: 14.0, dy: 34.0 }, align: [0.0, 0.0] },
        ], cx.seconds_since_app_start());
        chips.shown = true;
        chips.offer = Some((unit, press.parts));
        self.redraw(cx);
    }

    /// A click while "Place them anyway" is offered: on its chip, the whole scope is placed with
    /// the panel free; elsewhere the offer goes. True when the click was the offer's.
    fn click_offer(&mut self, cx: &mut Cx, at: [f64; 2]) -> bool {
        let screen = self.pane.cam.to_screen(at);
        let Some(chips) = self.st.chips.as_mut().filter(|chips| chips.offer.is_some()) else { return false };
        let pressed = chips.layer.hit([screen.x, screen.y]) == Some(OFFER_CHIP);
        let (unit, parts) = chips.offer.take().expect("an offer");
        if pressed {
            log!("fab.board: place {} parts with the panel free on unit {}", parts.len(), unit.0);
            cx.widget_action(self.uid, ViewAction::Place { unit, parts, all: false, free_panel: true });
        }
        self.clear_note(cx);
        pressed
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

    /// The same hover ink, translucent fill and hairline as a dragged rectangle. Only the
    /// confirmed edges and the edge to the pointer are stroked until the ring closes.
    fn draw_polygon_preview(&mut self, cx: &mut Cx2d, rect: Rect, ink: [f32; 4]) {
        let ShapeGesture::Polygon(shape) = &self.st.shape else { return };
        let pointer = self.pane.pointer_world();
        let closing = pointer.is_some_and(|p| shape.near_start(p, self.pane.cam.px_per_unit));
        let start = shape.points[0];
        let tip = pointer.map(|p| if closing { start } else { snapped(p, self.grid_mm) });
        let screen = |p: P| {
            let at = self.pane.cam.to_screen([to_mm(p.x), to_mm(p.y)]);
            (at.x as f32, at.y as f32)
        };
        let (x, y) = screen(start);
        let path = |draw: &mut DrawVector| {
            draw.move_to(x, y);
            for p in shape.points[1..].iter().copied().chain(tip) {
                let (x, y) = screen(p);
                draw.line_to(x, y);
            }
        };
        cx.begin_turtle(Walk { abs_pos: Some(rect.pos), width: Size::Fixed(rect.size.x), height: Size::Fixed(rect.size.y),
            ..Default::default() }, Layout { clip_x: true, clip_y: true, ..Default::default() });
        let draw = &mut self.draw_polygon;
        draw.begin();
        if shape.points.len() + usize::from(tip.is_some()) >= 3 {
            draw.set_color(ink[0], ink[1], ink[2], 0.16);
            path(draw);
            draw.close();
            draw.fill();
        }
        draw.set_color(ink[0], ink[1], ink[2], ink[3]);
        path(draw);
        draw.stroke(1.0);
        draw.circle(x, y, 2.5);
        draw.fill();
        if closing {
            draw.circle(x, y, CLOSE_RADIUS as f32);
            draw.stroke(1.5);
        }
        draw.end(cx);
        cx.end_turtle();
    }

    fn draw_tool_marks(&mut self, cx: &mut Cx2d, rect: Rect) {
        let cam = self.pane.cam;
        let local = |w: [f64; 2]| {
            let s = cam.to_screen(w) - rect.pos;
            [s.x as f32, s.y as f32]
        };
        let tokens = Tokens::for_theme(!self.light());
        if let Some(Drag::Route(route)) = &self.st.drag {
            if matches!(route.routing.g.kind, GestureKind::Tune { .. } | GestureKind::RoutePair { .. }) {
                let label = route.refused.as_deref().or_else(|| route.readout.shown(&self.st.drawn.as_ref()?.board));
                if let Some(label) = label {
                    let p = route.routing.g.cursor;
                    let at = cam.to_screen([to_mm(p.x), to_mm(p.y)]) + dvec2(10.0, 6.0);
                    let ink = if route.refused.is_some() { tokens.warning } else { tokens.hover };
                    let color = self.draw_text.color;
                    self.draw_text.color = vec4(ink[0], ink[1], ink[2], ink[3]);
                    self.draw_text.draw_abs(cx, at, label);
                    self.draw_text.color = color;
                }
                return;
            }
        }
        let mut ink = tokens.hover;
        let (seg, zone, label) = if let Some(m) = &self.st.marquee {
            // the Select marquee, in the selection's colour as the schematic draws its own
            ink = tokens.selection;
            let (a, b) = (local([m.rect[0], m.rect[1]]), local([m.rect[2], m.rect[3]]));
            let zone = if m.crossing { 4.0 } else { 3.0 };
            ([a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])], zone, None)
        } else if let Some([from, to]) = self.st.ruler {
            let (a, b) = (local(from), local(to));
            let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
            let label = format!("{:.3} mm   dx {:.3}   dy {:.3}", dx.hypot(dy), dx, dy);
            ([a[0], a[1], b[0], b[1]], 0.0, Some((dvec2(b[0] as f64 + 10.0, b[1] as f64 + 6.0), label)))
        } else if let ShapeGesture::Rectangle { from, to } = &self.st.shape {
            let (a, b) = (local(*from), local(*to));
            ([a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]), a[1].max(b[1])], 1.0, None)
        } else if matches!(self.st.shape, ShapeGesture::Polygon(_)) {
            self.draw_polygon_preview(cx, rect, ink);
            return;
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
        let clip = [rect.pos.x as f32, rect.pos.y as f32, (rect.pos.x + rect.size.x) as f32, (rect.pos.y + rect.size.y) as f32];
        if let Some(stitch) = self.st.stitch.as_mut().filter(|s| Some(s.unit) == self.st.unit) {
            stitch.layer.draw(cx, &self.pane.cam, clip, Tokens::for_theme(dark), &stitch.live, &mut self.draw_line, &mut self.draw_box, &mut self.draw_text, now);
            animating |= stitch.layer.animating(now);
        }
        if let Some(chips) = self.st.chips.as_mut() {
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
        let pair_press = self.st.pair_press.take().filter(|(draw, _, _)| draw.unit == unit);
        let draw = if self.st.armed.tool == BoardTool::RoutePair {
            pair_press.as_ref().map(|(d, _, _)| d.clone())
        } else { self.st.drawn.clone() };
        let Some(draw) = draw else { return };
        let (p, tol) = (nm(from), self.pick_nm());
        let order = self.pick_order();
        let geom = &draw.geom;
        let items = BoardItems { unit, board: &geom.board, scene: &geom.scene, pick: &geom.pick, items: &geom.items, order: &order };
        let pressed = items.pressed(from, HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12));
        let active = self.st.active.unwrap_or(LayerId::F_CU);
        let routes = cx.has_global::<ElecLive>();
        // Armed Route draws copper from a pad or a track's end, and nothing else starts.
        let route_tool = self.st.armed.tool == BoardTool::Route;
        if self.st.armed.tool == BoardTool::Tune {
            if let Some((_, circuit)) = self.st.tune_press.take().filter(|(id, _)| routes && *id == unit) {
                if let Some(routing) = tune_hit(&geom.board, &pressed, p).and_then(|hit| Routing::tune(&geom.board, hit, circuit)) {
                    self.begin_route(cx, routing, draw, at);
                }
            }
            return;
        }
        if self.st.armed.tool == BoardTool::RoutePair {
            if let Some((_, rules, active)) = pair_press {
                let pressed = classify(&geom.board, &pressed, p, tol, active, false);
                match Routing::pair(&geom.board, &rules, pressed) {
                    Ok(Some(routing)) => self.begin_route(cx, routing, draw, at),
                    Ok(None) => {}
                    Err(why) => self.note(cx, ChipKind::Refused, why, Vec::new()),
                }
            }
            return;
        }
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
            press @ (Press::Route { .. } | Press::Seg { .. }) if routes => {
                if let Some(routing) = Routing::start(&press, &geom.board, unit) { self.begin_route(cx, routing, draw, at); }
            }
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
    fn begin_route(&mut self, cx: &mut Cx, mut routing: Routing, frozen: Arc<BoardDraw>, at: [f64; 2]) {
        self.end_drag(Some(&mut *cx));
        if matches!(routing.g.kind, GestureKind::Route) {
            if let Some(w) = self.st.track_w { routing.set_width(w); }
            routing.g.mode = self.route_mode();
        }
        log!("fab.board: gesture {} from {:?} on {:?}", routing.id.0, routing.g.start, routing.g.layer);
        self.st.drag = Some(Drag::Route(Route {
            routing, frozen, seen: None, landing: None, sent: VecDeque::new(), waits: Vec::new(), blocked: None, walked: None,
            readout: RouteReadout::default(), refused: None, refusal_seq: 0, finished: None, cut: None, cut_seen: None, chip: None,
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
        if release && matches!(route.routing.g.kind, GestureKind::Tune { .. } | GestureKind::RoutePair { .. }) && cx.has_global::<ElecLive>() {
            if let Some(prior) = cx.get_global::<ElecLive>().snap.last_refusal.as_ref().filter(|r| r.sample == Some(route.routing.id.tag())) {
                // The release sends a fresh sample. Its answer may be legal even if the last one wasn't.
                route.refusal_seq = prior.seq;
            }
        }
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
        route.routing.set_span_end(&geom.board, p);
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
        let mut redraw = false;
        if shown == Some(route.routing.id) {
            if let Some((p, report)) = snap.proposal.as_ref().and_then(|p| Some((p, p.metrics.tune.as_ref()?.as_ref().ok()?))) {
                if let Some(board) = snap.live().unit(p.unit).and_then(|u| u.board.as_ref()) {
                    redraw |= route.readout.remember(board, p.id, report);
                }
            }
            if let (Some(net), GestureKind::RoutePair { mate, .. }) = (route.routing.g.net, &route.routing.g.kind) {
                redraw |= route.readout.remember_pair(snap, net, *mate);
            }
        }
        // A refused sample of a released gesture ends it before any commit; its preview goes.
        let measured = matches!(route.routing.g.kind, GestureKind::Tune { .. } | GestureKind::RoutePair { .. });
        if route.landing.as_ref().is_some_and(|l| !l.sent && refused == Some(l.tag))
            && (!measured || refusal.as_ref().is_some_and(|r| r.seq != route.refusal_seq)) {
            log!("fab.board: gesture {} refused before its commit", route.routing.id.0);
            let why = refusal.as_ref().filter(|_| measured).map(|r| r.reason.clone());
            self.end_drag(Some(cx));
            if let Some(why) = why { self.note(cx, ChipKind::Refused, why, Vec::new()); }
            return;
        }
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
            self.st.set_ordinary_rows(count, rows);
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
        if self.st.route_mut().is_some_and(|r| r.landing.is_none() && matches!(r.routing.g.kind, GestureKind::RoutePair { .. })) {
            let why = match code {
                KeyCode::KeyV => "pair vias come next",
                KeyCode::KeyW => "pair track widths come from the net classes",
                KeyCode::KeyF => "automatic finish is unavailable for pairs",
                _ => "the pointer trail sets the pair bends",
            };
            self.note(cx, ChipKind::Refused, why.into(), Vec::new());
            return true;
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
        self.st.set_ordinary_rows(board.next_item as usize, vec![(item.0, [1.0, 0.0, 0.0, 0.0])]);
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
                self.selection_marks();
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

    /// The connectivity the ratsnest, the route line and Route selected read: the host snapshot's
    /// for this unit, or `set_conn`'s. None while the unit waits for its whole-board checks: its
    /// connectivity so far is the board's without its pours, so no line or count is drawn from it.
    fn conn(&self, cx: &mut Cx) -> Option<Arc<Connectivity>> {
        let unit = self.st.unit?;
        if cx.has_global::<ElecLive>() {
            let snap = &cx.get_global::<ElecLive>().snap;
            if snap.checking(unit) {
                return None;
            }
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
        let draw = self.st.drawn.as_ref()?;
        let design = self.design(cx)?;
        let committed = design.unit(draw.unit)?.board.as_ref()?;
        (draw.board.next_item > committed.next_item).then_some(committed.next_item)
    }

    /// The job on this pane's board and the snapshot proposal that belongs to that running job.
    fn route_job(&self, cx: &Cx) -> Option<(Arc<RouteJobState>, Option<Arc<RouteProposal>>)> {
        let unit = self.st.unit?;
        let live = cx.get_global_ref::<ElecLive>()?;
        let job = live.snap.job.clone().filter(|job| job.status.unit == unit)?;
        let partial = live.snap.proposal.as_ref().filter(|p| job.end.is_none() && p.unit == unit
            && matches!(p.source, ProposalSource::Job { job: id, .. } if id == job.status.job)).cloned();
        Some((job, partial))
    }

    /// What the route line shows: a route job running on this board, else the unrouted nets of
    /// the shown board, how many the selection holds, and the finished job whose open nets it
    /// names; None without a host or before a picture.
    fn route_line_key(&mut self, cx: &mut Cx) -> Option<LineKey> {
        let job = self.route_job(cx);
        if let Some((job, partial)) = job.as_ref().filter(|(job, _)| job.end.is_none()) {
            let hand = partial.as_ref().and_then(|p| p.metrics.place.as_ref()).map_or_else(HandLine::default, |m| HandLine::of(&m.hand));
            return Some(LineKey::Job { status: job.status, organic: job.method == Method::Organic, hand });
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

    /// Whether the armed tool takes a shape or ruler drag.
    fn tool_drags(&self) -> bool {
        self.st.armed.tool.draws_shape() || self.st.armed.tool == BoardTool::Measure
    }

    /// A rectangle or Measure drag from where its press went down to world `to`; `done` on release:
    /// the ruler stays until the next press, a rectangle commits one edit. O(1) per move.
    fn tool_drag(&mut self, cx: &mut Cx, to: [f64; 2], done: bool) {
        let Some(from) = self.st.press_at else { return };
        match self.st.armed.tool {
            BoardTool::Measure => {
                self.st.ruler = Some([from, to]);
                if done {
                    self.set_armed(cx, self.st.armed.done());
                }
            }
            tool if tool.draws_shape() => {
                self.st.shape.drag(from, to);
                if done { self.finish_shape(cx); }
            }
            _ => {}
        }
        self.redraw(cx);
    }

    fn cancel_shape(&mut self) {
        self.st.shape.cancel();
        self.st.press_at = None;
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

    /// Both gestures commit through the same shape plan in a single history step.
    fn finish_shape(&mut self, cx: &mut Cx) {
        let Some(shape) = self.st.shape.finish(self.grid_mm) else { return };
        self.redraw(cx);
        let Some(design) = self.design(cx) else { return };
        let Some(draw) = self.st.drawn.clone().filter(|d| Some(d.unit) == self.st.unit) else { return };
        let tool = self.st.armed.tool;
        let net = if tool == BoardTool::Zone { self.net_at(&draw, shape.from) } else { None };
        self.send_part_or_shape(cx, shape_gesture(tool, &design, &draw.board, self.st.active, net, shape.points));
        self.set_armed(cx, self.st.armed.done());
    }

    /// Update from the circuit: the circuit's parts the board does not place yet, placed by the
    /// placer as a job (`ViewAction::Place`), round the parts that stand; a part it finds no room
    /// for goes in the row under the board (`plan::place_unplaced`). Or a chip saying there are none.
    fn place_from_circuit(&mut self, cx: &mut Cx) {
        let (Some(unit), Some(design)) = (self.st.unit, self.design(cx)) else { return };
        match plan::place_unplaced(&design, unit) {
            Ok((place, _)) => {
                log!("fab.board: place {} parts from the circuit on unit {}", place.len(), unit.0);
                self.clear_note(cx);
                cx.widget_action(self.uid, ViewAction::Place { unit, parts: Vec::new(), all: false, free_panel: false });
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
        let a = self.tools.widget(cx, ids!(col_a)).area().rect(cx);
        let b = self.tools.widget(cx, ids!(col_b)).area().rect(cx);
        let two = a.size.x > 0.0 && b.pos.x >= a.pos.x + a.size.x;
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
                if let Some(anchor) = flyout_anchor(cx, &self.tools, ids!(a_method)) {
                    let rows = METHODS.iter().map(|m| (method_name(*m), *m == self.st.method)).collect();
                    cx.action(BoardStripAction::MethodMenu { rows, anchor });
                }
            }
            StripPress::Part(edit) => self.edit_parts(cx, edit, None),
            StripPress::ArrangeMenu => {
                let Some(draw) = self.st.drawn.as_ref().filter(|d| Some(d.unit) == self.st.unit) else { return };
                if let Some(anchor) = flyout_anchor(cx, &self.tools, ids!(a_arrange)) {
                    let parts: Vec<_> = self.st.parts.items.iter().filter_map(|id| draw.board.placed_item(*id)).collect();
                    let enabled = ARRANGEMENTS.map(|(_, action)| self.st.parts.transform && action.available(&parts));
                    cx.action(BoardStripAction::ArrangeMenu { pane: self.uid, unit: draw.unit, enabled, anchor });
                }
            }
            StripPress::Shove => self.flip_shove(cx),
            StripPress::Place => {
                let (Some(unit), Some(design)) = (self.st.unit, self.design(cx)) else { return };
                match plan::PlacePress::of(&design, unit, &self.st.parts.ids) {
                    Ok(press) => {
                        let parts = &press.parts;
                        log!("fab.board: place {} on unit {}", if parts.is_empty() { "the parts not on the board".to_owned() } else { format!("{} parts", parts.len()) }, unit.0);
                        self.clear_note(cx);
                        if press.send() {
                            cx.widget_action(self.uid, ViewAction::Place { unit, parts: parts.clone(), all: false, free_panel: false });
                        }
                        if !press.held.is_empty() {
                            self.offer_place(cx, unit, press);
                        }
                    }
                    Err(why) => self.note(cx, ChipKind::Info, why, Vec::new()),
                }
            }
            StripPress::Tidy => {
                if let Some(unit) = self.st.unit {
                    cx.action(BoardStripAction::Tidy { unit, parts: self.st.parts.ids.clone() });
                }
            }
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
            if self.st.armed.tool != armed.tool { self.cancel_shape(); self.st.tune_press = None; self.st.pair_press = None; }
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
            transform: self.st.parts.transform,
            arrange: self.st.parts.arrange,
            tidy_busy: self.st.unit.zip(cx.get_global_ref::<Tidying>()).is_some_and(|(unit, busy)| busy.boards.contains(&unit)),
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
        // The checks' merge keeps the findings' generation: the flag alone tells it apart.
        let checking = |unit| cx.get_global_ref::<ElecLive>().is_some_and(|live| live.snap.checking(unit));
        let key = drawn.zip(violations.as_ref()).map(|(draw, (generation, _, awaiting))| {
            let proposal = wave.map_or(0, |(p, _)| Arc::as_ptr(p) as usize);
            (*generation, Arc::as_ptr(awaiting) as usize, draw.geom_gen, draw.unit, proposal, checking(draw.unit))
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
        let mut fresh = self.fresh_from(cx).filter(|first| *first < (1 << 24));
        let job = self.route_job(cx);
        let place = job.as_ref().and_then(|(job, partial)| {
            partial.as_ref().filter(|p| p.metrics.place.is_some()).map(|p| (job.status.job, p))
        });
        if place.is_some() || self.st.place_preview.is_some() {
            if let Some(board) = self.st.drawn.as_ref().map(|draw| draw.board.clone()) {
                let snap = cx.get_global_ref::<ElecLive>().map(|live| live.snap.clone());
                let source = place.zip(snap.as_deref()).map(|((job, p), snap)| (job, p, snap));
                if let Some(mut texture) = self.st.sync_place(&board, source, || BoardDraws::templates(cx).map(|t| t.stroke)) {
                    texture.release(cx);
                }
                fresh = self.st.place_fresh_from(fresh);
            }
        }
        let waiting = self.st.drag.as_ref().and_then(|drag| match drag {
            Drag::Route(route) if route.landing.is_none() && route.routing.stale() => route.sent.front().map(|(_, since)| *since),
            _ => None,
        });
        let stale = waiting.is_some_and(|since| Cx::monotonic_now() - since >= STALE_SECONDS);
        if waiting.is_some() && !stale && !cx.next_frame_is_pending(self.st.next_frame) {
            self.st.next_frame = cx.new_next_frame();
        }
        let pending = fresh.is_some() && (stale || job.as_ref().is_some_and(|(_, partial)| partial.is_some()));
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
        let part_of = |comp: CompId| board.as_ref().and_then(|b| b.placed_of(comp)).map(|p| p.refdes.clone());
        let design = self.design(cx);
        let block_of = |block: BlockId| {
            let unit = design.as_ref()?.unit(board.as_ref()?.unit)?;
            unit.circuit.tree.get(block.ix()).map(|b| b.path.clone())
        };
        let why = self.st.left_open.as_ref().map_or(&[][..], |left| &left.why[..]);
        let line_rect = self.right_of_strip(rect);
        self.route_line.draw(cx, line_rect, key, &self.st.still_open, why, name_of, part_of, block_of, &theme);
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
        if !matches!(self.st.shape, ShapeGesture::Idle)
            && (matches!(event, Event::KeyFocus(f) if f.prev == self.draw_bg.area() && f.focus != f.prev)
                || matches!(event, Event::MouseDown(e) if !self.st.rect.contains(e.abs))
                || matches!(event, Event::WindowLostFocus(_))) {
            self.cancel_shape();
            self.redraw(cx);
        }
        if self.settings_event(cx, event, scope) { return; }
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
            for action in actions {
                if let Some(wait) = action.downcast_ref::<FillWaitNotice>().filter(|wait| wait.action == StitchPress::ACTION) {
                    if let Some(stitch) = self.st.stitch.as_mut().filter(|s| s.unit == wait.unit) {
                        stitch.status(cx, wait.waiting);
                        self.redraw(cx);
                    }
                }
                if let Some(done) = action.downcast_ref::<BoardEditDone>() {
                    if let Some(stitch) = self.st.stitch.take_if(|s| s.tag == done.tag) {
                        if let Some(why) = done.refusal.as_ref().filter(|_| Some(stitch.unit) == self.st.unit) {
                            self.note(cx, ChipKind::Refused, why.clone(), Vec::new());
                        }
                        self.redraw(cx);
                    }
                }
            }
            for action in actions.iter().filter_map(|a| a.downcast_ref::<BoardStripAction>()) {
                match action {
                    BoardStripAction::Method(i) => {
                        if let Some(&method) = METHODS.get(*i) { self.set_method(cx, method); }
                    }
                    BoardStripAction::Arrange { pane, unit, command } if *pane == self.uid && Some(*unit) == self.st.unit => {
                        self.edit_parts(cx, PartEdit::Arrange(*command), None);
                    }
                    _ => {}
                }
            }
        }
        // The tool strip is over the canvas: its presses, hovers and scrolls are its own.
        let actions = cx.capture_actions(|cx| self.tools.handle_event(cx, event, scope));
        let board_button = self.tools.button(cx, ids!(a_board));
        if board_button.clicked(&actions) || matches!(event, Event::Actions(a) if board_button.clicked(a)) {
            self.toggle_settings(cx);
        }
        if self.tools.button(cx, ids!(a_stitch)).clicked(&actions) {
            self.stitch_pressed(cx);
        }
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
            let checking = self.st.unit.is_some_and(|unit| cx.get_global_ref::<ElecLive>().is_some_and(|live| live.snap.checking(unit)));
            let findings = self.violations(cx).map(|(generation, _, awaiting)| (generation, Arc::as_ptr(&awaiting) as usize, checking));
            // The route line hides while a job runs on this board and returns when it ends.
            let line = shown.is_some() && self.route_line_key(cx) != self.route_line.key();
            if current.is_some() && current != shown
                || shown.is_some() && findings != self.st.marks_key.map(|key| (key.0, key.1, key.5))
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
        // During a press the canvas consumes Escape to release the pointer. Cancel the
        // board's draft too, including a rectangle over empty space with no drag target.
        let escape_press = self.pane.pressed() && !matches!(self.st.shape, ShapeGesture::Idle)
            && matches!(event, Event::KeyDown(k) if k.key_code == KeyCode::Escape);
        let inputs = {
            let Some(draw) = self.st.drawn.clone() else { return };
            let order = self.pick_order();
            let geom = &draw.geom;
            let base = BoardItems { unit: geom.board.unit, board: &geom.board, scene: &geom.scene, pick: &geom.pick, items: &geom.items, order: &order };
            let items = PressItems { base, select: self.st.armed.tool == BoardTool::Select, selected: &self.st.selected };
            self.pane.handle_event(cx, event, self.draw_bg.area(), &items)
        };
        for input in inputs {
            self.input(cx, input);
        }
        if escape_press && !self.pane.pressed() {
            self.input(cx, CanvasInput::Escape);
        }
        if matches!(event, Event::MouseMove(_)) {
            self.sync_stitch_target(cx);
        }
        if matches!(self.st.armed.tool, BoardTool::Tune | BoardTool::RoutePair) && !self.pane.pressed() {
            let pointer = match event { Event::MouseMove(e) => Some(e.abs), Event::MouseUp(e) => Some(e.abs), _ => None };
            if let Some(p) = pointer.filter(|p| self.st.rect.contains(*p)) {
                let at = self.pane.cam.to_world(p);
                let order = self.pick_order();
                if let Some(draw) = &self.st.drawn {
                    let geom = &draw.geom;
                    let items = BoardItems { unit: draw.unit, board: &geom.board, scene: &geom.scene, pick: &geom.pick, items: &geom.items, order: &order };
                    let hits = items.pressed(at, HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12));
                    let hand = if self.st.armed.tool == BoardTool::RoutePair {
                        let press = classify(&geom.board, &hits, nm(at), self.pick_nm(), self.st.active.unwrap_or(LayerId::F_CU), true);
                        matches!(press, Press::Route { start: Anchor::Pad { .. }, net, .. } if geom.board.pairs.iter().any(|p| p.p == net || p.n == net))
                    } else {
                        tune_hit(&geom.board, &hits, nm(at)).is_some()
                    };
                    cx.set_cursor(if hand { MouseCursor::Hand } else { MouseCursor::Default });
                }
            }
        }
        if self.st.armed.tool.draws_shape() {
            let pointer = match event {
                Event::MouseMove(e) => Some(e.abs),
                Event::MouseUp(e) => Some(e.abs),
                _ => None,
            };
            if pointer.is_some_and(|p| self.st.rect.contains(p))
                && (!self.pane.pressed() || matches!(self.st.shape, ShapeGesture::Rectangle { .. })) {
                cx.set_cursor(MouseCursor::Crosshair);
            }
            if matches!(self.st.shape, ShapeGesture::Polygon(_)) && matches!(event, Event::MouseMove(_)) {
                self.redraw(cx);
            }
        }
        self.end_marquee(cx);
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
        self.draw_settings(cx, scope, above);
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
            profiles: Arc::default(), profile_diags: Arc::default(),
            design, preview: None, preview_tag: None, preview_by: None, created: vec![], last_commit: None,
            sheets: vec![], proposal: None, job: None, conn: vec![], rules: vec![], fills: vec![], sysnets: None, export: vec![], benches: Arc::default(),
            violations: Tagged { unit: None, of: StateOf::Committed, generation: 0, value: Arc::new(vec![]) },
            awaiting: Arc::new(vec![]), cases: Arc::default(), checking: vec![], last_refusal: None, stale: StageFlags::default(), head: String::new(),
            source_map: None, docs: None, catalog: None,
        }
    }

    #[test]
    fn tune_readout_keeps_the_reported_lengths_and_measurement_qualifiers() {
        use super::RouteReadout;
        use fab_elec_board::Board;
        use fab_elec_core::BoardId;
        use fab_elec_route::ProposalId;
        use std::sync::Arc;
        let mut report = fab_elec_route::meander::Report {
            reached: 45_000_000, target: 45_000_000, short_by: 0, uncoupled_added: None,
            partial: false, no_stackup: false,
        };
        let values = |s: &str| s.split_whitespace().filter_map(|v| v.trim_end_matches(',').parse::<f64>().ok()).collect::<Vec<_>>();
        let a = Arc::new(Board::new(BoardId(0), "A"));
        let b = Arc::new((*a).clone());
        let mut labels = RouteReadout::default();
        labels.remember(&a, ProposalId(1), &report);
        report.reached = 41_234_000;
        report.short_by = 3_766_000;
        report.partial = true;
        report.no_stackup = true;
        labels.remember(&b, ProposalId(2), &report);
        assert_eq!(values(labels.shown(&a).unwrap()), [45., 45., 0.], "the earlier picture still shows its own report");
        let text = labels.shown(&b).unwrap();
        assert_eq!(values(text), [41.234, 45., 3.766]);
        assert!(text.contains("short") && text.contains("partial") && text.contains("vias not counted: no stackup"));
    }

    #[test]
    fn pair_release_joins_both_pads_in_one_step_and_waits_for_its_preview_connectivity() {
        use super::{classify, snap_target, Press, RouteReadout, Routing};
        use super::super::{nodes::Pressed, pick::Hit, scene::{Role, Tag}};
        use fab_elec_board::{connect, resolve_rules, Anchor};
        use fab_elec_core::{Canon, LayerId, PadIx};
        use fab_elec_design::DesignOp;
        use fab_elec_route::{head, Base, GestureKind, ProposalId, RBase};
        let mut sources = fab_elec_lang::Sources::new();
        sources.add("pair.ckt.splash", r#"use mod.elec.*
let e = mod.elec
let p = e.net({name: "P"})
let n = e.net({name: "N"})
let a = e.r(1kohm, p, n)
let b = e.r(1kohm, p, n)
let c = e.r(1kohm, e.net({name: "other"}), e.net({name: "return"}))
e.pair(p, n, {gap: 0.6mm skew: 0.1mm})
"#).unwrap();
        sources.add("pair.pcb.splash", r#"board("pair")
outline([[[0mm 0mm] [40mm 0mm] [40mm 30mm] [0mm 30mm] [0mm 0mm]]])
place("a", [10mm 5mm])
place("b", [25mm 20mm], {rot: -90deg})
place("c", [35mm 5mm])
"#).unwrap();
        let design = Arc::new(fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).unwrap().design);
        let unit = &design.units[0];
        let board = unit.board.as_ref().unwrap();
        let pair = board.pairs[0];
        let layer = LayerId::F_CU;
        let rules = Arc::new(resolve_rules(board, None));
        let pad = |part: &str, net| {
            let placed = board.placed_of(unit.circuit.comp(part).unwrap().id).unwrap();
            let i = placed.pad_nets.iter().position(|v| *v == Some(net)).unwrap();
            let a = Anchor::Pad { placed: placed.id, pad: PadIx(i as u16) };
            let p = board.anchor_pos(a).unwrap();
            let hit = Hit { layer, tag: Tag { item: placed.id, sub: i as u32, role: Role::Pad }, net: Some(net), index: 0, poly: false, dist: 0 };
            (a, p, hit)
        };
        let press = |at, hit| classify(board, &[Pressed::Copper(hit)], at, 0, layer, true);
        let (a, at, hit) = pad("a", pair.p);
        let ordinary = press(at, hit);
        assert!(matches!(Routing::start(&ordinary, board, unit.id).unwrap().g.kind, GestureKind::Route));
        let mut drag = Routing::pair(board, &rules, ordinary).unwrap().unwrap();
        let (mate, mate_at, _) = pad("a", pair.n);
        assert_eq!(drag.g.start, a);
        assert!(matches!(drag.g.kind, GestureKind::RoutePair { mate: n, mate_start, mate_start_at }
            if n == pair.n && mate_start == mate && mate_start_at == mate_at));
        let (_, at, hit) = pad("c", board.net("other").unwrap());
        assert!(Routing::pair(board, &rules, press(at, hit)).unwrap().is_none());
        assert!(Routing::pair(board, &rules, Press::Nothing).unwrap().is_none());

        let base_conn = Arc::new(connect(board, &[]));
        assert!(base_conn.unrouted.contains(&pair.p) && base_conn.unrouted.contains(&pair.n));
        let base = Base::new(board.clone(), rules, base_conn.clone(), Arc::default());
        drag.moved(P::mm(10., 20.), 0, None);
        let (arrival, at, hit) = pad("b", pair.n);
        let target = snap_target(board, &[hit], &drag.g).unwrap();
        assert_eq!(target.0, arrival);
        let sample = drag.release(at, 0, Some(target));
        let answer = Arc::new(head::answer(&base, &RBase::build(board), &sample, &mut head::GestureState::default(), ProposalId(1)).proposal().unwrap());
        assert!(answer.verified, "{:?}", answer.metrics.pair_refusal);
        assert!(drag.offer(&answer) && drag.commit_ready());
        let report = answer.pair.as_ref().unwrap();
        assert!(report.blocked.is_none());
        let ops: Vec<_> = answer.ops.iter().cloned().map(|op| DesignOp::Board { unit: unit.id, op }).collect();
        let mut preview = (*design).clone();
        let applied = preview.apply_all(&ops, false).unwrap();
        let [step] = applied.as_slice() else { panic!("both legs form one undo step") };
        let drawn = preview.units[0].board.as_ref().unwrap().clone();
        assert_eq!(drawn.tracks.len(), 2);
        assert!(drawn.tracks.iter().flat_map(|t| t.ends).all(|a| matches!(a, Anchor::Pad { .. })));
        let mut snap = snapshot(design.clone());
        snap.preview = Some(Arc::new(preview.clone()));
        snap.proposal = Some(answer.clone());
        let tag = |of, generation, value| Tagged { unit: Some(unit.id), of, generation, value };
        snap.conn.push(tag(StateOf::Committed, board.generation, base_conn.clone()));
        snap.checking.push(unit.id);
        // Even a preview-tagged result is provisional while the unit owes its checks.
        snap.conn.push(tag(StateOf::Preview, drawn.generation, base_conn.clone()));
        let mut labels = RouteReadout::default();
        assert!(labels.remember_pair(&snap, pair.p, pair.n));
        let pending = labels.shown(&drawn).unwrap();
        assert!(pending.contains("checking connectivity") && !pending.contains("partial"));
        let values = |s: &str| s.split_whitespace().filter_map(|v| v.parse::<f64>().ok()).collect::<Vec<_>>();
        let nums = values(pending);
        assert_eq!(nums.len(), 4);
        assert_eq!(pending.split_whitespace().filter(|word| *word == "mm").count(), nums.len());
        assert!((nums[0] * 1e6 - report.p_length as f64).abs() <= 500.);
        assert!((nums[1] * 1e6 - report.n_length as f64).abs() <= 500.);
        assert!((nums[2] * 1e6 - report.skew as f64).abs() <= 5000.);
        assert_eq!(nums[3], 0.1);
        // When checking clears, an older generation still cannot qualify the legs.
        snap.conn.pop();
        snap.conn.push(tag(StateOf::Preview, board.generation, base_conn));
        snap.checking.clear();
        assert!(!labels.remember_pair(&snap, pair.p, pair.n));
        let joined = Arc::new(connect(&drawn, &[]));
        assert!(!joined.unrouted.contains(&pair.p) && !joined.unrouted.contains(&pair.n));
        snap.conn.push(tag(StateOf::Preview, drawn.generation, joined));
        assert!(labels.remember_pair(&snap, pair.p, pair.n));
        let done = labels.shown(&drawn).unwrap();
        assert!(!done.contains("partial") && !done.contains("checking connectivity"));
        assert_eq!(values(done), nums);
        assert!(!labels.remember_pair(&snap, pair.p, pair.n));
        preview.apply_all(&step.inverse(), true).unwrap();
        assert_eq!(preview.units[0].board.as_ref().unwrap().content_hash(), board.content_hash());
    }

    #[test]
    fn polygon_clicks_and_rectangle_drags_commit_the_drawn_points_once() {
        use super::{BoardTool, ShapeGesture};
        use fab_elec_board::{Area, BoardOp, Item};
        use fab_elec_core::{BBox, Canon, LayerId, PolySet};
        use fab_elec_design::DesignOp;
        let grid = 0.5;
        let zoom = 10.0;
        let clicks = [[2.12, 3.12], [8.1, 3.2], [8.2, 9.1], [5.1, 6.1], [2.2, 9.22]];
        let polygon = [P::mm(2.0, 3.0), P::mm(8.0, 3.0), P::mm(8.0, 9.0), P::mm(5.0, 6.0), P::mm(2.0, 9.0)];
        let mut gesture = ShapeGesture::Idle;
        for tool in [BoardTool::Zone, BoardTool::Outline, BoardTool::Keepout, BoardTool::Cutout] {
            let mut design = fab_elec_fixtures::ledctl();
            let board = design.units[0].board.clone().unwrap();
            let before = board.content_hash();
            for at in clicks { assert!(!gesture.click(at, grid, zoom, 1)); }
            gesture.backspace();
            assert!(!gesture.click(clicks[4], grid, zoom, 1));
            match tool {
                BoardTool::Zone => {
                    // Five screen points from the first vertex: close without appending it.
                    assert!(gesture.click([2.5, 3.0], grid, zoom, 1));
                }
                BoardTool::Outline => {
                    // The second tap of the last double-click can land on a different grid point.
                    assert!(gesture.click([2.3, 9.2], grid, zoom, 2));
                }
                // Enter finishes the existing vertices.
                BoardTool::Keepout | BoardTool::Cutout => {}
                _ => unreachable!(),
            }
            let shape = gesture.finish(grid).unwrap();
            assert_eq!(shape.from, clicks[0], "the unsnapped start picks a zone's net");
            assert_eq!(shape.points, polygon);
            let plan = shape.plan(tool, &design, &board, Some(LayerId::B_CU), board.net("GND")).unwrap();
            let [DesignOp::Board { op, .. }] = plan.ops.as_slice() else { panic!("one op for {tool:?}") };
            let area = PolySet::from_ring_nonzero(polygon.to_vec());
            match op {
                BoardOp::Add(Item::Zone(z)) => assert_eq!(z.area, Area::Poly(area)),
                BoardOp::Add(Item::Keepout(k)) => assert_eq!(k.area, area),
                BoardOp::SetOutline(contours) => assert_eq!(PolySet::from_ring_nonzero(contours[0].to_ring(1)), area),
                BoardOp::Add(Item::Cutout(c)) => assert_eq!(PolySet::from_ring_nonzero(c.contour.to_ring(1)), area),
                _ => panic!("unexpected {tool:?} op"),
            }
            let steps = design.apply_all(&plan.ops, false).unwrap();
            let [step] = steps.as_slice() else { panic!("one history step for {tool:?}") };
            design.apply_all(&step.inverse(), true).unwrap();
            assert_eq!(design.units[0].board.as_ref().unwrap().content_hash(), before);
            assert!(gesture.finish(grid).is_none());
        }

        // Escape and Backspace on the first vertex both leave nothing to commit.
        gesture.click(clicks[0], grid, zoom, 1);
        gesture.cancel();
        assert!(gesture.finish(grid).is_none());
        gesture.click(clicks[0], grid, zoom, 1);
        gesture.backspace();
        assert!(gesture.finish(grid).is_none());

        // A drag replaces any open path. Reversed corners still give the old zone rectangle.
        gesture.click(clicks[0], grid, zoom, 1);
        gesture.drag(clicks[2], clicks[0]);
        let rect = gesture.finish(grid).unwrap();
        let design = fab_elec_fixtures::ledctl();
        let board = design.units[0].board.as_ref().unwrap();
        let plan = rect.plan(BoardTool::Zone, &design, board, Some(LayerId::F_CU), None).unwrap();
        let [DesignOp::Board { op: BoardOp::Add(Item::Zone(z)), .. }] = plan.ops.as_slice() else { panic!("one zone") };
        assert_eq!(rect.from, clicks[2]);
        assert_eq!(z.area, Area::Poly(PolySet::rect(BBox::new(polygon[0], polygon[2]))));
    }

    #[test]
    fn place_preview_follows_the_drawn_round_and_restores_pane_rows() {
        use super::{rats, BoardOp, Field, Item, JobId, PaneState, ProposalSource, RouteProposal, StrokeTemplate};
        use fab_elec_core::{Angle, CompId, ItemId};
        use fab_elec_parts::FpTextKind;

        let mut snap = snapshot(Arc::new(fab_elec_fixtures::ledctl()));
        let unit = snap.design.units.iter().position(|u| u.board.is_some()).unwrap();
        let base = snap.design.units[unit].board.clone().unwrap();
        let (a, b, label) = (&base.placed[0], &base.placed[1], &base.placed[2]);
        let to = a.at.add(P::mm(2.0, 1.0));
        let job = JobId(1);
        let round = |step, ops: Vec<BoardOp>| {
            let mut board = (*base).clone();
            board.apply_all(&ops).unwrap();
            let proposal = Arc::new(RouteProposal {
                pair: None,
                id: Default::default(), source: ProposalSource::Job { job, step }, unit: board.unit,
                base_generation: base.generation, base_hash: Default::default(), ops, checked: None,
                metrics: fab_elec_route::RouteMetrics { place: Some(Default::default()), ..Default::default() },
                verified: true, collisions: vec![], open: vec![], head: None, solve_us: 0, last: false, label: String::new(),
            });
            (proposal, Arc::new(board))
        };
        let label_pose = fab_elec_board::LabelPose { at: P::mm(2.0, 3.0), rot: Angle::QUARTER, pinned: false };
        let (first, shown) = round(0, vec![
            BoardOp::SetField { item: a.id, field: Field::At(to) },
            BoardOp::SetField { item: a.id, field: Field::Rot(a.rot.add(Angle::QUARTER)) },
            BoardOp::SetField { item: a.id, field: Field::Side(a.side.flipped()) },
            BoardOp::SetField { item: b.id, field: Field::Rot(b.rot.add(Angle::QUARTER)) },
            BoardOp::SetField { item: label.id, field: Field::Label(FpTextKind::Reference, Some(label_pose)) },
        ]);
        let preview = |board| {
            let mut d = (*snap.design).clone();
            d.units[unit].board = Some(board);
            Arc::new(d)
        };
        let template = || Some(StrokeTemplate::packed());
        let cached = || panic!("an unchanged or unmatched picture must reuse its lines");
        let mut pane = PaneState::default();
        snap.preview = Some(preview(shown.clone()));
        pane.sync_place(&base, Some((job, &first, &snap)), cached);
        assert!(pane.dims.is_none(), "the committed picture waiting for the first round stays settled");
        pane.sync_place(&shown, Some((job, &first, &snap)), template);
        assert_eq!(base.next_item, shown.next_item, "the first round only edits existing ids");
        assert_eq!(pane.place_fresh_from(None), Some(1 << 24), "a moved-only round enables the pending shader");
        let rows = pane.dims.as_ref().unwrap();
        assert_eq!(rows.row(a.id.ix())[2], 1.0, "a translated placement is pending");
        assert_eq!(rows.row(b.id.ix())[2], 1.0, "a rotation about an unchanged origin is pending");
        assert_eq!(rows.row(label.id.ix())[2], 0.0, "a label edit doesn't ghost the placement");
        let lines = pane.place_preview.as_ref().unwrap().lines.as_ref().unwrap().publication();
        let mut expected = Vec::new();
        rats::append_line(&mut expected, a.at, to, &StrokeTemplate::packed());
        assert_eq!(lines.segments()[0].data(), expected, "one hairline, from the committed to the shown origin");
        pane.sync_place(&shown, Some((job, &first, &snap)), cached);

        let mut brought = a.clone();
        brought.id = ItemId(base.next_item);
        brought.comp = CompId(base.placed.iter().map(|p| p.comp.0).max().unwrap() + 1);
        let (next, new_board) = round(1, vec![
            BoardOp::Add(Item::Placed(brought)),
            BoardOp::SetField { item: label.id, field: Field::Side(label.side.flipped()) },
        ]);
        snap.preview = Some(preview(new_board.clone()));
        pane.sync_place(&shown, Some((job, &next, &snap)), cached);
        assert_eq!(pane.dims.as_ref().unwrap().row(a.id.ix())[2], 1.0, "the old visible round keeps its marks");
        pane.sync_place(&new_board, Some((job, &next, &snap)), template);
        let rows = pane.dims.as_ref().unwrap();
        assert_eq!(rows.row(a.id.ix())[2], 0.0, "replacement clears old marks");
        assert_eq!(rows.row(label.id.ix())[2], 1.0, "the replacement marks a flip in place");
        assert_eq!(rows.row(base.next_item as usize)[2], 0.0, "a brought part uses only the fresh-id threshold");
        assert_eq!(pane.place_fresh_from(Some(base.next_item)), Some(base.next_item), "brought parts retain their threshold");
        assert!(pane.place_preview.as_ref().unwrap().lines.is_none(), "a flip in place and a brought part get no line");
        assert!(pane.sync_place(&new_board, None, cached).is_some() && pane.dims.is_none(), "removal restores an absent binding");
        assert!(pane.place_fresh_from(None).is_none(), "ending the job disables the moved-only treatment");

        pane.set_ordinary_rows(new_board.next_item as usize, vec![(label.id.0, [1.0, 1.0, 0.0, 0.0])]);
        pane.sync_place(&new_board, Some((job, &next, &snap)), template);
        assert_eq!(pane.dims.as_ref().unwrap().row(label.id.ix()), [1.0, 1.0, 1.0, 0.0], "dimming and emphasis coexist with the move");
        pane.sync_place(&new_board, None, cached);
        assert_eq!(pane.dims.as_ref().unwrap().row(label.id.ix()), [1.0, 1.0, 0.0, 0.0], "ending the job preserves ordinary rows");
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

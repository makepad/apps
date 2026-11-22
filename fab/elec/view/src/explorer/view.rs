//! The retained circuit explorer: a click selects a part in place and opens its inspector card
//! (nothing on the sheet moves); a double-click opens the projected view around it (the reveal),
//! Escape returns; T/A/F for theme, assembly and fit. The pane owns pan, zoom, hover and semantic
//! presses.
//! `item` is a SheetRows row; selection comes from the host's item-state texture
//! (`electronics/shared/item-state.md`), shared by the sheet and its reveal.
//! A host feeds it (`set_design`, `update_design`, `set_live`), or it follows the electronics
//! host's `ElecLive` on its own (`follow`).

use super::{
    drag::{unpin_ops, Drag, Finish},
    glyphs,
    gesture::{landed, moved_parts, Answer, ByPath, ChipLook, EditGesture, Frozen, LivePress, Phase, Source, Step},
    layout_job::{Dropped, Keep, KeepHints, Kept, Laid, LayoutJobs, Result as LayoutResult, Revealed},
    liveix::{mini_nets, SceneLive, SheetPieces, REVEAL_SHEET},
    reveal_view::{ExplorerItems, Reveal},
    scene::{text_points, Flight, FlyFrom, Leg, Scene, SceneState, Text, TextFade, FLOOR_MAX_SCALE,
        NO_ITEM, TEXT_FLOOR_POINTS, TEXT_MIN_POINTS},
    scrub::Scrub,
    targets,
    value_edit::{queue_field_key, NameEdit, NetNameEdit, ValueEdit, WireName},
};
use crate::canvas::{
    draw::{
        ArcInst, ArcTemplate, BoxInst, BoxTemplate, BridgeInst, BridgeTemplate, CanvasLayer, CanvasLive, CanvasUniforms, Common, DrawCanvasArc,
        DrawCanvasBox, DrawCanvasBridge, DrawCanvasGrid, DrawCanvasLine, DrawCanvasMini, DrawCanvasPin, LineInst, LineTemplate, MiniInst, MiniTemplate,
        PinInst, PinTemplate,
    },
    pane::{part_key, SavedView2d, ViewOnArrival, DRAG_THRESHOLD, FIT_MARGIN, HIT_RADIUS},
    Camera2d, CanvasInput, CanvasItems, CanvasNode, CanvasPane, Look, Tokens,
};
use crate::live::mini::MiniSubs;
use crate::live::sim::SimAction;
use crate::live::flags::ProbeFlags;
use crate::live::probe::{host_bench, probe_label, probe_on_click, probe_target, ProbeGesture};
use crate::canvas::tools::{draw_bar, draw_float, draw_strip, over_strip, Armed, ToolClicks};
use crate::board::findings::reason_line;
use super::tools::{annotate_pressed, apply_strip, bar_presses, check_pressed, counts, hint, place_pressed, power_anchor, power_pressed, strip_presses, BarPress, Carrying, CircuitTool, StripAction, BAR_PARTS, LIBRARY_PARTS, STEP_MM, STRIP_PARTS};
use crate::live::segs::{Prepared, TreeOwner};
use crate::{ElecLive, ElecPanel, ViewAction};
use crate::library::{library_drag_entry, EntryKey, LibraryAction};
use crate::snapshot::next_tag;
use fab_elec_bench::ValueIndex;
// Hosts can supply sheet rows and inspect view actions without another dependency.
pub use fab_elec_circuit::{CircuitOp, End};
pub use fab_elec_core::Sel;
pub use fab_elec_explore::rows::{RowIndex, SheetKey, SheetRows};
use fab_elec_circuit::{Circuit, NetKind, SheetHints};
use fab_elec_core::{BoardId, CompId, CompRef, NetId, NetRef};
use fab_elec_design::{BenchOp, Design, DesignOp, Focus, FocusKind, FocusOrigin, ProbeOf, Unit};
use fab_elec_explore::{edit::Target, legible::legible_labels, plan::{delete, delete_parts, label_net, mirror_part, plan, turn_part, Gesture, Planned}, sheet, KeepBase, Opts, SheetLayout};
use fab_elec_explore::plan_drop::{Template, TemplateKind};
use fab_elec_explore::plan_paste::{plan_paste, CopiedPart};
use fab_elec_lang::{CatalogEntry, CatalogKind};
use fab_elec_parts::PartKind;
use makepad_widgets_core::makepad_platform::a11y_tree;
use makepad_widgets_core::event::TouchState;
use makepad_widgets_core::SemanticLookup;
use makepad_widgets_core::*;
use makepad_widgets_pickers::combo_box::ComboBoxWidgetRefExt;
use std::sync::Arc;

mod card;
pub use card::ShowModelSources;
mod follow;
mod paste;
mod probe_flags;
use card::{starts_value, Card};

#[derive(Default)]
enum Marquee {
    #[default]
    Idle,
    Pressed,
    Drawing([f64; 4]),
}

/// Give pin-line presses an item key even where ordinary scene picking sees empty canvas.
/// A pin press can start a wire; the other tools keep the ordinary click target.
struct WireItems<'a> {
    base: ExplorerItems<'a>,
    sheet: &'a SheetLayout,
    unit: Option<BoardId>,
    tool: CircuitTool,
    /// A click to finish a wire or a move needs no item under the pointer.
    click_pending: bool,
    /// The ordinary click target when a pin line needs a synthetic item key.
    click: std::cell::Cell<Option<Option<Sel>>>,
    /// Pin presses can start a wire even when the ordinary hit already names the part.
    pin: std::cell::Cell<Option<[f64; 2]>>,
    /// On a Select press, record whether both the ordinary and edit picks found empty sheet.
    empty: Option<&'a std::cell::Cell<bool>>,
}

impl CanvasItems for WireItems<'_> {
    type Key = Sel;

    fn hit(&self, at: [f64; 2], radius: f64) -> Vec<Sel> {
        if self.click_pending { return Vec::new(); }
        let scene = self.base.sheet;
        let resolve = |scene: &Scene, sheet: &SheetLayout|
            targets::resolve(&scene.hits, sheet, at, self.base.cam.px_per_unit, &Target::None);
        let mut main_target = None;
        if let Some(board) = self.unit.filter(|_| self.base.reveal.is_none()
            && (self.tool == CircuitTool::Wire || scene.value_at_time(at, radius, self.base.now).is_none()))
        {
            let source = resolve(scene, self.sheet);
            main_target = Some(source);
            if let Target::Pin { comp, .. } | Target::WireEnd { comp, .. } = source {
                if matches!(source, Target::Pin { .. }) { self.pin.set(Some(at)); }
                if !scene.body_contains(comp.0, at, 0.0, self.base.now) {
                    self.click.set(Some(self.base.hit(at, radius).into_iter().next()));
                    return vec![Sel::Comp(CompRef { board, comp })];
                }
            }
        }
        let hits = self.base.hit(at, radius);
        if let Some(empty) = self.empty.filter(|_| hits.is_empty()) {
            let canvas = |target| matches!(target, Target::Canvas { .. } | Target::None);
            empty.set(canvas(main_target.unwrap_or_else(|| resolve(scene, self.sheet)))
                && self.base.reveal.is_none_or(|reveal| canvas(resolve(&reveal.scene, reveal.sheet()))));
        }
        hits
    }

    fn nodes(&self, cam: &Camera2d) -> Vec<CanvasNode<Sel>> { self.base.nodes(cam) }
}

script_mod! {
    use mod.prelude.widgets.*

    mod.widgets.ExplorerView = set_type_default() do #(ExplorerView::register_widget(vm)) {
        width: Fill
        height: Fill
        flags: mod.widgets.ProbeFlags{}
        // The host's tool strip (`explorer::tools` names its parts); none by default.
        tools: View{width: Fit height: Fit visible: false}
        // The host's Library float (`explorer::tools` names its parts); none by default.
        library: View{width: Fit height: Fit visible: false}
        draw_text +: {
            text_style: theme.font_regular{font_size: 11.0}
        }
        draw_marquee +: {
            pixel: fn() {
                let px = 1.0 / self.draw_pass.dpi_factor
                let sdf = Sdf2d.viewport_px(self.pos * self.rect_size, self.draw_pass.dpi_factor)
                sdf.rect(px * 0.5, px * 0.5, max(0.0, self.rect_size.x - px), max(0.0, self.rect_size.y - px))
                sdf.fill_keep(vec4(self.color.rgb, 0.08))
                sdf.stroke(self.color, px * 0.5)
                return sdf.result
            }
        }
        // The inspector card and its field boxes: rounded, a hairline rim.
        draw_card +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, 6.0)
                sdf.fill_keep(self.color)
                sdf.stroke(vec4(mix(self.color.rgb, vec3(0.55), 0.35), 1.0), 1.0)
                return sdf.result
            }
        }
        draw_card_field +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, 4.0)
                sdf.fill_keep(self.color)
                sdf.stroke(vec4(mix(self.color.rgb, vec3(0.6), 0.3), 1.0), 1.0)
                return sdf.result
            }
        }
        value_field +: {
            is_multiline: false
            keep_focus_on_submit: true
            margin: 0.
            padding: Inset{left: 4., right: 4., top: 2., bottom: 2.}
            draw_text +: {text_style: theme.font_regular{font_size: 11.0}}
        }
        value_combo: mod.widgets.ComboBox{
            accepts_typed: true match_case: true margin: 0.
            padding: Inset{left: 4. right: 20.}
            a11y: {label: "Part value"}
            input +: {
                is_multiline: false keep_focus_on_submit: true
                draw_text +: {text_style: theme.font_regular{font_size: 12.0}}
            }
        }
    }
}

pub(super) struct Templates {
    pub(super) line: LineTemplate,
    pub(super) boxes: BoxTemplate,
    pub(super) arc: ArcTemplate,
    pub(super) bridge: BridgeTemplate,
    /// Captured with the other shaders, before any mini strips are shown.
    pub(super) mini: MiniTemplate,
    /// Captured before the host supplies firmware rows.
    pub(super) pin: PinTemplate,
}

impl Templates {
    fn capture(cx: &Cx, line: &DrawCanvasLine, boxes: &DrawCanvasBox, arc: &DrawCanvasArc,
        bridge: &DrawCanvasBridge, mini: &DrawCanvasMini, pin: &DrawCanvasPin) -> Option<Self>
    {
        Some(Self {
            line: LineTemplate::capture(cx, line)?,
            boxes: BoxTemplate::capture(cx, boxes)?,
            arc: ArcTemplate::capture(cx, arc)?,
            bridge: BridgeTemplate::capture(cx, bridge)?,
            mini: MiniTemplate::capture(cx, mini)?,
            pin: PinTemplate::capture(cx, pin)?,
        })
    }
}

struct ValueScrub {
    scrub: Scrub,
    unit: BoardId,
    from: [f64; 2],
    text: Text,
    previewed: bool,
}

/// Wire input cost and the newest sample's first host acknowledgement, without per-frame work.
#[derive(Default)]
struct WireMeasure {
    samples: u64,
    ui_sum: f64,
    ui_max: f64,
    layouts: u64,
    sent: Option<(u64, f64)>,
    shown: u64,
    shown_sum: f64,
    shown_max: f64,
    outcome: Option<&'static str>,
}

impl WireMeasure {
    /// Time only the machine's pointer sample, including resolution and a changed candidate's plan.
    fn sample(&mut self, gesture: &mut EditGesture, at: [f64; 2], scale: f64) -> Step {
        let start = Cx::monotonic_now();
        let step = gesture.sample(at, scale);
        let us = (Cx::monotonic_now() - start) * 1e6;
        self.samples += 1;
        self.ui_sum += us;
        self.ui_max = self.ui_max.max(us);
        step
    }

    /// Count a shown sample once; older previews cannot acknowledge the newest send.
    fn shown(&mut self, tag: u64) {
        if let Some((_, start)) = self.sent.filter(|(sent, _)| *sent == tag) {
            self.sent = None;
            let ms = (Cx::monotonic_now() - start) * 1e3;
            self.shown += 1;
            self.shown_sum += ms;
            self.shown_max = self.shown_max.max(ms);
        }
    }

    /// Emit one summary when the gesture ends, with no retained sample history.
    fn report(&self, chip: &str, outcome: &str) {
        log!("fab.explorer: wire {}: {} samples, ui mean {:.2} us max {:.2} us, {} layouts, shown mean {:.2} ms max {:.2} ms, {}",
            chip, self.samples, self.ui_sum / self.samples.max(1) as f64, self.ui_max,
            self.layouts, self.shown_sum / self.shown.max(1) as f64, self.shown_max, outcome);
    }
}

/// A catalog entry as a drop plan takes it: a passive whose part is a capacitor decouples.
fn template_of(entry: &CatalogEntry) -> Template {
    let kind = match entry.kind {
        CatalogKind::Passive if entry.part.as_ref().is_some_and(|part| part.kind == PartKind::Capacitor) => TemplateKind::Capacitor,
        CatalogKind::Passive => TemplateKind::Passive,
        CatalogKind::Part => TemplateKind::Part,
        CatalogKind::Block => TemplateKind::Block,
    };
    Template {
        stmt: entry.stmt.clone(), stem: entry.stem.clone(), kind,
        pins: entry.pins.iter().map(|pin| (pin.name.clone(), pin.kind)).collect(),
    }
}

/// The current catalog's entry for `key`, carried as a part under its title.
fn library_template(cx: &mut Cx, key: &EntryKey) -> Option<Carried> {
    if !cx.has_global::<ElecLive>() { return None; }
    let catalog = cx.get_global::<ElecLive>().snap.catalog.clone()?;
    let entry = catalog.entries.iter().find(|entry| entry.lib == key.lib && entry.name == key.name)?;
    Some(Carried::Part(Arc::new(template_of(entry)), entry.title.clone()))
}

/// Whether the open Library would take more than two fifths of the sheet.
fn library_is_narrow(open_width: f64, sheet_width: f64) -> bool {
    open_width > 0.4 * sheet_width
}

/// The fold to show for the saved choice and this sheet's current width and open request.
fn library_folded(folded: bool, narrow: bool, opened_while_narrow: bool) -> bool {
    folded || (narrow && !opened_while_narrow)
}

/// Find the first power flag in the current catalog when the Power flyout opens or picks it.
fn power_flag_entry(cx: &Cx) -> Option<&CatalogEntry> {
    let catalog = cx.get_global_ref::<ElecLive>()?.snap.catalog.as_ref()?;
    catalog.entries.iter().find(|entry| entry.part.as_ref().is_some_and(|part| part.kind == PartKind::PowerFlag))
}

/// What a carry holds, with the name its chip shows: a Library entry to place, or a power rail
/// (from the Power flyout) to tie pins to.
#[derive(Clone)]
enum Carried {
    Part(Arc<Template>, String),
    Rail(NetId, sheet::TagKind, String),
}

/// The name a bench probe takes for net `net` of unit `unit` (`Design::probe_net`).
fn probe_name(design: &Design, unit: usize, net: u32) -> Option<String> {
    design.probe_net(NetRef { board: design.units.get(unit)?.id, net: NetId(net) }).map(str::to_owned)
}

/// Subscribe once per geometry publication, preserving first label/member occurrence order.
fn subscribe_minis(minis: Option<&mut MiniSubs>, owner: TreeOwner, circuit: &Circuit, sheet: &SheetLayout) -> Vec<f32> {
    let start = Cx::monotonic_now();
    let mut slots = vec![-1.0; circuit.nets.len()];
    let mut count = 0usize;
    if let Some(minis) = minis {
        let TreeOwner::Sheet(board, _) = owner else { return slots };
        let mut nets = Vec::new();
        for net in sheet.labels.iter().flat_map(|label| mini_nets(circuit, label.owner)) {
            count += 1;
            if slots[net as usize] == -1.0 {
                // Pending slots mark first occurrence without a second net-sized table.
                slots[net as usize] = -2.0;
                nets.push(NetRef { board, net: NetId(net) });
            }
        }
        for (net, slot) in nets.iter().zip(minis.set(owner, &nets)) {
            slots[net.net.0 as usize] = slot as f32;
        }
    }
    let reveal = if matches!(owner, TreeOwner::Sheet(_, REVEAL_SHEET)) { " (reveal)" } else { "" };
    log!("fab.explorer: {count} mini traces{reveal}, subscriptions on the UI thread: {:.2} ms",
        (Cx::monotonic_now() - start) * 1e3);
    slots
}

#[derive(Script, ScriptHook, Widget)]
pub struct ExplorerView {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawCanvasGrid,
    #[live]
    draw_line: DrawCanvasLine,
    #[live]
    draw_box: DrawCanvasBox,
    /// Mini trace shader, registered and captured alongside the other canvas shapes.
    #[live]
    draw_mini: DrawCanvasMini,
    /// Firmware pin marks share the part glyphs' pose and state.
    #[live]
    draw_pin: DrawCanvasPin,
    #[live]
    draw_arc: DrawCanvasArc,
    #[live]
    draw_bridge: DrawCanvasBridge,
    #[live]
    draw_text: DrawText,
    #[live]
    value_field: TextInput,
    /// Retained selection strokes of the nets named by the wire-end field.
    #[rust]
    name_lines: CanvasLayer,
    #[find]
    #[live]
    value_combo: WidgetRef,
    /// The draw lists the sheet's vertical texts turn up in: the sheet's, the overlay's, a drag's.
    #[rust]
    up_lists: [Option<DrawList2d>; 3],
    #[rust]
    design: Option<Arc<Design>>,
    /// A host feeds this view (`set_design`); until one does, it follows `ElecLive`.
    #[rust]
    pinned: bool,
    /// The button a pointer holds down in the playing run (a live press), released with the
    /// pointer; the click that ends it does not pick the part.
    #[rust]
    live_press: LivePress,
    /// Pane rows project the host's held set onto stable sheet item ids (sheet and reveal).
    #[rust]
    press_states: Option<crate::ItemStates>,
    #[rust]
    press_from: Option<Arc<Vec<CompRef>>>,
    /// What this view follows in `ElecLive` (its unit and the circuit and rows it laid out).
    #[rust]
    followed: Option<follow::Followed>,
    /// Rows of `design`, belonging to the latest requested layout generation.
    #[rust]
    rows: Arc<RowIndex>,
    #[rust]
    sheet_design: Option<Arc<Design>>,
    /// The bench shown by the host's bench view; new probes go to it.
    #[rust]
    bench: Option<String>,
    /// Last global probe net and its sheet item, resolved only when the name or scene changes.
    #[rust]
    probe_lit: Option<(String, Option<u32>)>,
    /// The probes' named connections drawn on the sheet (`live::flags`, `probe_flags`).
    #[live]
    flags: ProbeFlags,
    /// The tool strip the host supplies (`explorer::tools`), drawn over the sheet's top left.
    #[find]
    #[live]
    tools: WidgetRef,
    /// The Library float the host supplies (`explorer::tools`), over the sheet's top right.
    #[find]
    #[live]
    library: WidgetRef,
    /// The Library's saved side and fold choices.
    #[rust]
    library_left: bool,
    #[rust]
    library_folded: bool,
    /// Whether the open Library would take more than 40% of the sheet, as last drawn.
    #[rust]
    library_narrow: bool,
    /// An explicit open in a narrow sheet, cleared on fold or when the sheet widens enough.
    #[rust]
    opened_while_narrow: bool,
    /// The side and effective fold last drawn.
    #[rust]
    library_shown: Option<(bool, bool)>,
    /// Where the Library was last drawn (empty while hidden), for the card and Check list.
    #[rust]
    library_at: Rect,
    /// Where the tool strip was last drawn (empty while hidden): the card draws before it.
    #[rust]
    strip_at: Rect,
    #[rust]
    library_search_pending: bool,
    /// The status bar the host supplies (`explorer::tools`), drawn along the sheet's bottom.
    #[find]
    #[live]
    status: WidgetRef,
    /// The grid cell under the pointer the bar shows, None off the sheet.
    #[rust]
    status_cell: Option<[i32; 2]>,
    /// The bar's counts and the circuit they count (its address), rebuilt when it changes.
    #[rust]
    status_counts: Option<(usize, String)>,
    /// The bar's words as last written: position, hint, counts.
    #[rust]
    status_shown: [String; 3],
    /// The reason the bar shows for the hovered pin, part or net, for that hover and that
    /// generation of findings; looked up again only when either changes.
    #[rust]
    status_why: Option<(Option<Sel>, u64, Option<String>)>,
    /// The unit this sheet last drew, as told to the Checks editor (`ElecLive::shown_unit`).
    #[rust]
    told_unit: Option<BoardId>,
    /// The strip's armed tool, the last click on a tool's button (a second one soon after keeps
    /// the tool), and the armed tool the strip's buttons show.
    #[rust]
    armed: Armed<CircuitTool>,
    #[rust]
    tool_clicks: ToolClicks<CircuitTool>,
    #[rust]
    strip_shown: Option<Armed<CircuitTool>>,
    #[rust]
    sheet_probes: probe_flags::SheetProbes,
    /// A probe carried over the sheet: the flag its drop would make, drawn before the drop.
    #[rust]
    flag_ghost: Option<probe_flags::ProbeGhost>,
    /// Rows of `sheet_design`, retained until the matching next layout arrives.
    #[rust]
    sheet_rows: Arc<RowIndex>,
    #[rust]
    unit: usize,
    #[rust]
    sheet: Arc<SheetLayout>,
    #[rust]
    layout_jobs: LayoutJobs,
    #[rust]
    gen: u64,
    /// Results before this generation belong to a previous set_design call.
    #[rust]
    design_gen: u64,
    #[rust]
    applied_gen: u64,
    #[rust]
    reveal_gen: u64,
    #[rust]
    reveal_focus: Option<u32>,
    #[rust]
    reveal: Option<Reveal>,
    #[rust]
    pending_reveal: Option<Revealed>,
    #[rust]
    fit_after_home: bool,
    /// A layout was applied: the next draw fits the whole sheet at the pane's new size.
    #[rust]
    refit: bool,
    /// Edge coverage measured after the overlays drew, for the following placement.
    #[rust]
    drawn_inset: [f64; 4],
    #[rust(CanvasPane::with_max_ppu(SHEET_MAX_PPU))]
    pane: CanvasPane<Sel>,
    #[rust]
    opening_view: Option<ViewOnArrival<SheetLayout>>,
    /// The pane's width over height at its last draw (zero before one); layouts pack a sheet's
    /// unconnected parts to it (`Opts::aspect`).
    #[rust]
    pane_aspect: f64,
    /// The aspect the shown sheet was laid out for.
    #[rust]
    laid_aspect: f64,
    /// A relayout for a new pane shape fits the camera to it when it lands.
    #[rust]
    refit_after_layout: bool,
    #[rust]
    scene: Scene,
    #[rust(1.0)]
    legible: f64,
    /// The first relabel of this rebuilt scene also reports its cost.
    #[rust(true)]
    legible_pending: bool,
    #[rust]
    state: SceneState,
    #[rust]
    lines: CanvasLayer,
    /// Pin warnings for the current sheet and findings generation.
    #[rust]
    erc_marks: CanvasLayer,
    #[rust]
    erc_generation: Option<u64>,
    /// Retained sheet strips, published with the other scene layers.
    #[rust]
    minis: CanvasLayer,
    /// Static pin indices; firmware publications only update their texture.
    #[rust]
    pins: CanvasLayer,
    /// Moving copies, republished at the same gesture boundaries as the pin stubs.
    #[rust]
    drag_pins: CanvasLayer,
    /// The drag's glyph-copy item, retained through its return flight.
    #[rust]
    drag_pin_item: Option<u32>,
    /// One transparent off-screen instance, submitted only on the first captured frame.
    #[rust]
    mini_warm: CanvasLayer,
    #[rust]
    bridge_warm: CanvasLayer,
    #[rust]
    pin_warm: CanvasLayer,
    #[rust]
    boxes: CanvasLayer,
    /// Block backgrounds move below the light, including when their whole subtree is dragged.
    #[rust]
    frame_rows: Vec<BoxInst>,
    #[rust]
    frame_fills: CanvasLayer,
    /// The one retained glow row per simulated placement, updated at layout and drag boundaries.
    #[rust]
    glow_rows: Vec<BoxInst>,
    #[rust]
    glows: CanvasLayer,
    #[rust]
    arcs: CanvasLayer,
    #[rust]
    bridges: CanvasLayer,
    #[rust]
    leaders: CanvasLayer,
    #[rust]
    chip: CanvasLayer,
    #[rust]
    chip_lines: CanvasLayer,
    /// Frozen target attachment, independent of the live sheet's motion.
    #[rust]
    chip_arcs: CanvasLayer,
    #[rust]
    scrub: Option<ValueScrub>,
    #[rust]
    value_edit: Option<ValueEdit>,
    /// The shared text input's target while editing a net name or a part reference.
    #[rust]
    name_edit: Option<NameEdit>,
    /// Keys received before the shared text input's first focused frame.
    #[rust]
    name_keys: Vec<Event>,
    /// The name the last Global label tied, offered again on the next pin.
    #[rust]
    last_global: String,
    /// An input has no drawn area on its first opening; focus it after drawing that area.
    #[rust]
    value_focus_pending: bool,
    /// Selected parts in pick order. Paths keep the indices current across source edits.
    #[rust]
    selected: Vec<(u32, String)>,
    #[rust]
    marquee: Marquee,
    #[live]
    draw_marquee: DrawColor,
    /// The last part copied from this sheet, retained across edits and repeated pastes.
    #[rust]
    copied: Option<CopiedPart>,
    /// The inspector card of `selected`, as last drawn.
    #[rust]
    card: Option<Card>,
    /// A press on the card's value: where (absolute), and whether it became a scrub.
    #[rust]
    card_press: Option<(DVec2, bool)>,
    #[live]
    draw_card: DrawColor,
    #[live]
    draw_card_field: DrawColor,
    #[rust]
    note: Option<(Text, bool)>,
    /// The newest key edit's label, its pointer point and the host's refusal count when it was
    /// sent: a later refusal of that label is shown there.
    #[rust]
    key_sent: Option<(String, [f64; 2], u64)>,
    /// The layout generation when the person's latest sheet edit (a paste, a key at rest) was
    /// sent: the first layout after it keeps the camera where they work, as a drag's does.
    #[rust]
    own_edit: Option<u64>,
    #[rust]
    drag: Drag,
    /// Wire input owns only its frozen references and overlays.
    #[rust]
    gesture: Option<EditGesture>,
    /// The frozen committed scene, moved aside before the first preview scene replaces it.
    #[rust]
    ghost_src: Option<Scene>,
    /// Capture on the next scene build, after the first replacement layout arrives.
    #[rust]
    ghost_take: bool,
    /// Frozen part bodies and their hit geometry, below the live sheet.
    #[rust]
    ghost_boxes: CanvasLayer,
    #[rust]
    ghost_lines: CanvasLayer,
    #[rust]
    ghost_arcs: CanvasLayer,
    #[rust]
    ghost_pins: CanvasLayer,
    /// A move commit keeps its displayed sheet until this tag or its base is answered.
    #[rust]
    hold: Option<(u64, BoardId, Arc<Circuit>)>,
    /// A paste or a Library drop waiting to commit with the hints that keep the sheet.
    #[rust]
    keep_edit: Option<paste::KeepEdit>,
    /// The keep result for the Library preview drawn in `sheet_design`.
    #[rust]
    shown_keep: Option<KeepHints>,
    /// Animate the first layout after a gesture or held commit ends.
    #[rust]
    wire_settle: bool,
    /// Input and preview latency, reset at each wire press.
    #[rust]
    wire_measure: WireMeasure,
    /// Cached overlay text; only candidate changes copy its string.
    #[rust]
    gesture_text: Option<Text>,
    /// A press held across Landing, with its latest pointer sample.
    #[rust]
    wire_press: Option<([f64; 2], [f64; 2])>,
    /// A Library entry a click armed (`LibraryAction::Arm`) and its title: the carry begins at the
    /// pointer's next move over the sheet, on the committed sheet.
    #[rust]
    carry_armed: Option<Carried>,
    /// The entry being carried and its title, armed again after a Shift-click lands or when the
    /// committed design moves under a click-armed carry.
    #[rust]
    carry_last: Option<Carried>,
    /// The carry follows a Library drag: its drop commits it, and leaving the sheet cancels it.
    #[rust]
    carry_drag: bool,
    /// Shift was held on the click that placed the carried part: carry it again once it lands.
    #[rust]
    carry_again: bool,
    /// Preserve the ordinary click when a tool leaves a pin or wire end to the sheet.
    #[rust]
    wire_click: Option<Option<Sel>>,
    /// A held button during a click-to-finish gesture crossed the threshold, even if it moved back.
    #[rust]
    click_button_dragged: bool,
    /// Pin-line hover, resolved only on pointer events so signals cannot reset its hand cursor.
    #[rust]
    hover_wire: bool,
    #[rust]
    hover_value_box: Option<[f64; 4]>,
    #[rust]
    pending_sheet: Option<Laid>,
    /// How far the displayed layout was moved from its own origin (`Keep`); later layouts start
    /// from it, so an edit never moves the drawing under the camera.
    #[rust]
    shift: [i32; 2],
    #[rust]
    templates: Option<Templates>,
    /// The host's live row textures; live off until a host sets them.
    #[rust]
    live: Option<CanvasLive>,
    /// Latest worker-built trees, prepared for the host's allocator and waiting for it.
    #[rust]
    live_trees: Option<Prepared>,
    /// Pieces of the displayed sheet, indexed like its runs and entries.
    #[rust]
    live_pieces: Arc<SheetPieces>,
    /// Host rows parallel to the registered trees' segments.
    #[rust]
    live_rows: Vec<Vec<u32>>,
    /// Sheet strip slots indexed by graph net; -1 for nets with no subscribed strip.
    #[rust]
    mini_slots: Vec<f32>,
    /// Lowering whose net and part numbering the instances currently carry.
    #[rust]
    live_index: Option<Arc<ValueIndex>>,
    /// Static live indices for the displayed sheet.
    #[rust]
    scene_live: Arc<SceneLive>,
    /// Sheet geometry registered with the simulation host.
    #[rust]
    live_owner: Option<TreeOwner>,
    /// Pieces of the displayed reveal, indexed like its runs and entries.
    #[rust]
    reveal_pieces: Arc<SheetPieces>,
    /// Host rows parallel to the reveal's registered tree segments.
    #[rust]
    reveal_rows: Vec<Vec<u32>>,
    /// Reveal strip slots indexed by graph net; -1 for nets with no subscribed strip.
    #[rust]
    reveal_mini_slots: Vec<f32>,
    /// Reveal geometry registered separately from the sheet with the simulation host.
    #[rust]
    reveal_owner: Option<TreeOwner>,
    #[rust]
    capture_attempted: bool,
    #[rust(true)]
    dark: bool,
    #[rust(true)]
    dirty: bool,
    #[rust]
    assemble_pending: bool,
    #[rust]
    frame: NextFrame,
    /// The live bench's publication this view last drew, and the live clock as of it.
    #[rust]
    live_mark: LiveMark,
    #[rust]
    live_clock: f32,
}

/// What marks a new publication of the live bench (`SimLive`): new values (their simulated time),
/// new trace points (the record's end), a new numbering, a scrub instant, play or pause, rows
/// appearing, a new emphasis. A view at rest draws again only when this changes, never per display
/// frame.
#[derive(Clone, Copy, Default, PartialEq)]
struct LiveMark {
    t: u64,
    end: u64,
    index: usize,
    scrub: Option<u64>,
    playing: bool,
    rows: bool,
    emphasis: u64,
    held: usize,
}

/// The gesture chip's text height and its offset from the pointer, in logical pixels.
const CHIP_PX: f64 = 12.0;

impl ExplorerView {
    /// Whether the live bench published since this view last looked; if so, takes its mark and
    /// the live clock with it, so flow dashes step once per publication. The host's clock moves
    /// back when it re-bases its epoch (the flow rows move with it): that counts as a change too.
    fn live_moved(&mut self, cx: &Cx) -> bool {
        let Some(live) = cx.get_global_ref::<ElecLive>() else { return false };
        let sim = &live.sim;
        let mark = LiveMark {
            t: sim.t.to_bits(),
            end: sim.record.1.to_bits(),
            index: Arc::as_ptr(&sim.index) as usize,
            scrub: sim.scrub.map(f64::to_bits),
            playing: sim.playing,
            rows: sim.nets.is_some(),
            emphasis: sim.emphasis,
            held: Arc::as_ptr(&sim.held) as usize,
        };
        if mark == self.live_mark && sim.time >= self.live_clock {
            return false;
        }
        self.live_mark = mark;
        self.live_clock = sim.time;
        true
    }

    /// Only a held-set or row-mapping change writes the small pane texture. These rows can
    /// show simultaneous holds from different views.
    fn bind_pressed(&mut self, cx: &mut Cx, live: &mut CanvasLive) {
        let held = cx.get_global_ref::<ElecLive>().map(|live| live.sim.held.clone());
        let same = match (&self.press_from, &held) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            let board = self.design.as_ref().and_then(|d| d.units.get(self.unit)).map(|u| u.id);
            let mut rows: Vec<_> = held.as_deref().into_iter().flatten()
                .filter(|comp| Some(comp.board) == board)
                .filter_map(|comp| self.sheet_rows.comp(comp.comp.0).map(|row| (row, [0.0, 1.0, 0.0, 0.0])))
                .collect();
            rows.sort_unstable_by_key(|(row, _)| *row);
            let count = rows.last().map_or(0, |(row, _)| *row as usize + 1);
            self.press_states.get_or_insert_with(crate::ItemStates::new).write(count, rows);
            self.press_from = held;
        }
        if let Some(states) = &mut self.press_states {
            live.pane = states.texture(cx);
            live.pane_on = true;
        }
    }

    /// Free this view's sheet/reveal wiring and mini subscriptions and their cached indices.
    /// The holder calls this before dropping the view because Drop has no Cx.
    /// Calling it twice is harmless.
    pub fn release(&mut self, cx: &mut Cx) {
        if let Some(cmd) = self.live_press.up() { cx.action(SimAction::Cmd(cmd)); }
        if let Some(mut states) = self.press_states.take() { states.release(cx); }
        self.press_from = None;
        if let Some(owner) = self.live_owner.take() {
            if cx.has_global::<ElecLive>() {
                let sim = &mut cx.get_global::<ElecLive>().sim;
                sim.trees.remove(owner);
                sim.minis.remove(owner);
            }
        }
        self.drop_reveal_live(cx);
        self.flags.release();
        self.flag_ghost = None;
        self.live_trees = None;
        self.live_pieces = Arc::default();
        self.live_rows.clear();
        self.mini_slots.clear();
        self.live_index = None;
        self.scene_live = Arc::default();
    }

    /// Layout runs on a worker; the first frame after its arrival starts assembly.
    /// `rows` must index `design.units[unit].circuit`. The host feeds the view from now on.
    pub fn set_design(&mut self, cx: &mut Cx, design: Arc<Design>, unit: usize, rows: RowIndex) {
        self.pinned = true;
        self.load(cx, design, unit, rows);
    }

    fn load(&mut self, cx: &mut Cx, design: Arc<Design>, unit: usize, rows: RowIndex) {
        self.close_value_edit(cx);
        self.copied = None;
        let previous = self.design.as_ref().and_then(|design| design.units.get(self.unit)).map(|unit| (unit.id, &unit.name));
        let next = design.units.get(unit).map(|unit| (unit.id, &unit.name));
        if previous != next {
            self.release(cx);
            self.selected.clear();
            self.card = None;
            self.card_press = None;
        }
        // Before the first layout: the layout keeps clear the boxes this font's texts cover.
        super::font_metrics::measure_once(cx, &mut self.draw_text);
        self.gen += 1;
        self.design_gen = self.gen;
        self.design = Some(design.clone());
        self.rows = Arc::new(rows);
        self.unit = unit;
        self.rebind_selection(cx);
        self.sheet_design = None;
        self.probe_lit = None;
        self.sheet_rows = Arc::default();
        self.sheet = Arc::new(SheetLayout::default());
        self.reveal_gen += 1;
        self.reveal_focus = None;
        self.reveal = None;
        self.drop_reveal_live(cx);
        self.pending_reveal = None;
        self.fit_after_home = false;
        self.pane = CanvasPane::with_max_ppu(SHEET_MAX_PPU);
        self.marquee = Marquee::Idle;
        self.scene = Scene::default();
        self.legible = 1.0;
        self.legible_pending = true;
        self.state = SceneState::default();
        self.scrub = None;
        self.note = None;
        self.drag = Drag::default();
        if let Some(gesture) = &self.gesture {
            if let Some((chip, _)) = gesture.chip() { self.wire_measure.report(chip, "cancelled"); }
        }
        self.gesture = None;
        self.clear_ghosts();
        self.hold = None;
        self.keep_edit = None;
        self.shown_keep = None;
        self.wire_settle = false;
        self.wire_measure = WireMeasure::default();
        self.gesture_text = None;
        self.wire_press = None;
        self.wire_click = None;
        self.hover_wire = false;
        self.chip.set(1, Vec::new());
        self.chip_lines.set(1, Vec::new());
        self.chip_arcs.set(1, Vec::new());
        self.hover_value_box = None;
        self.pending_sheet = None;
        self.dirty = true;
        self.assemble_pending = false;
        self.frame = NextFrame::default();
        self.shift = [0, 0];
        self.refit_after_layout = false;
        let opts = self.layout_opts();
        self.layout_jobs.request(cx, design, unit, SheetHints::default(), None, None, Keep::default(), self.gen, opts);
        self.draw_bg.redraw(cx);
    }

    /// Set the bench shown by the host's bench view; new probes go to it.
    pub fn set_bench(&mut self, bench: Option<String>) {
        self.bench = bench;
    }

    /// Re-layout without resetting navigation, focus, hover or the assembly clock.
    /// `rows` belongs to this design's displayed unit, not the preceding sheet.
    pub fn update_design(&mut self, cx: &mut Cx, design: Arc<Design>, rows: RowIndex) {
        if self.value_edit.as_ref().is_some_and(|edit| {
            let previous = self.design.as_ref().and_then(|d| d.units.get(self.unit))
                .filter(|unit| unit.id == edit.unit).and_then(|unit| unit.circuit.comps.get(edit.comp.0 as usize));
            let next = design.units.get(self.unit).filter(|unit| unit.id == edit.unit)
                .and_then(|unit| unit.circuit.comps.get(edit.comp.0 as usize));
            !matches!((previous, next), (Some(a), Some(b)) if b.id == edit.comp && a.path == b.path
                && b.value.as_ref() == Some(&edit.current) && b.part.kind == edit.kind)
        }) || self.name_edit.as_ref().is_some_and(|edit| {
            let source = match edit {
                NameEdit::Net(NetNameEdit { wire_end: Some(_), .. }) =>
                    cx.get_global_ref::<ElecLive>().map(|host| host.snap.design.as_ref()),
                _ => Some(design.as_ref()),
            };
            !source.and_then(|d| d.units.get(self.unit)).is_some_and(|unit| edit.matches(&unit.circuit))
        })
        {
            self.close_value_edit(cx);
        }
        if self.scrub.is_some() {
            // Keep the sheet rows and chip anchor stable throughout the gesture. The
            // host's preview text is current underneath; publish the new layout on release.
            self.scene.update_values(&design, self.unit);
            if let Some(reveal) = &mut self.reveal {
                reveal.scene.update_values(&design, self.unit);
            }
            self.hover_value_box = self.state.hover.and_then(|item| self.value_box(item, cx.seconds_since_app_start()));
        }
        self.design = Some(design.clone());
        self.rows = Arc::new(rows);
        self.rebind_selection(cx);
        self.gen += 1;
        self.drag.design_updated(self.gen);
        self.reveal_gen += 1;
        self.pending_reveal = None;
        let kept = self.carry_layout(&design);
        let keep = Keep { shift: self.shift, block: self.drag.anchor() };
        let opts = self.layout_opts();
        self.layout_jobs.request(cx, design, self.unit, SheetHints::default(), self.drag.lead(), kept, keep, self.gen, opts);
        self.draw_bg.redraw(cx);
    }

    /// While a Library carry is under way: the block its drop lands in keeps its origin on the
    /// committed sheet (`Drag::keep`, so the cancel's and the commit's layouts keep it too), and the
    /// single part the preview adds is laid out against the carry's frozen sheet.
    /// Cost: one pass over the parts' paths per preview and an Arc clone.
    fn carry_layout(&mut self, design: &Design) -> Option<(Arc<KeepBase>, String)> {
        let gesture = self.gesture.as_ref().filter(|gesture| gesture.carries())?;
        let frozen = gesture.frozen();
        let block = match gesture.target() {
            Target::Canvas { block, .. } => block,
            Target::Pin { comp, .. } | Target::WireEnd { comp, .. } => frozen.circuit.comps.get(comp.ix()).map_or(0, |part| part.block.0),
            _ => 0,
        };
        if let Some(origin) = sheet::block_origin(&frozen.sheet, block) { self.drag.keep((block, origin)); }
        let base = gesture.keep_base()?;
        let known: std::collections::HashSet<&str> = frozen.circuit.comps.iter().map(|part| part.path.as_str()).collect();
        let mut added = design.units.get(frozen.unit)?.circuit.comps.iter().filter(|part| !known.contains(part.path.as_str()));
        let first = added.next()?;
        added.next().is_none().then(|| (base.clone(), first.path.clone()))
    }

    /// Layout options for the pane as last drawn.
    fn layout_opts(&self) -> Opts {
        let opts = Opts::default();
        Opts { aspect: if self.pane_aspect > 0.0 { self.pane_aspect } else { opts.aspect }, ..opts }
    }

    /// A sheet whose unconnected parts were packed for another pane shape is laid out again for
    /// this one (a fifth off either way), once nothing else is moving it; the camera refits when
    /// it lands. Other sheets do not depend on the pane's shape and never relayout for it.
    fn relayout_for_pane(&mut self, cx: &mut Cx) {
        // A re-pack refits, so a camera the person moved keeps the packing it shows (a sash
        // drag never fits); the next fit (a layout applied, F) re-packs for the new shape.
        if !self.sheet.packed || !self.pane.keeps_fit || self.laid_aspect <= 0.0 || self.pane_aspect <= 0.0 { return; }
        let ratio = self.pane_aspect / self.laid_aspect;
        if (1.0 / 1.2..=1.2).contains(&ratio) { return; }
        if self.applied_gen != self.gen || self.layout_jobs.busy() || self.busy() || self.holding()
            || self.scrub.is_some() || self.field_open() || self.reveal.is_some()
        {
            return;
        }
        let Some(design) = self.design.clone() else { return };
        self.gen += 1;
        self.drag.design_updated(self.gen);
        self.reveal_gen += 1;
        self.pending_reveal = None;
        self.refit_after_layout = self.pane.placed;
        let keep = Keep { shift: self.shift, block: None };
        let opts = self.layout_opts();
        self.layout_jobs.request(cx, design, self.unit, SheetHints::default(), None, None, keep, self.gen, opts);
    }

    fn apply_layout(&mut self, cx: &mut Cx, laid: Laid) {
        self.drag.worker(laid.ms);
        if laid.gen != self.gen || laid.gen <= self.applied_gen
            || self.pending_sheet.as_ref().is_some_and(|pending| laid.gen <= pending.gen)
        {
            return;
        }
        if self.holding() || self.scrub.is_some() || self.field_holds_sheet() {
            self.pending_sheet = Some(laid);
            return;
        }
        let Laid { gen, sheet, kept, trees, pieces, shift, ms, aspect } = laid;
        self.laid_aspect = aspect;
        let first = self.applied_gen < self.design_gen;
        self.applied_gen = gen;
        self.shift = shift;
        let now = cx.seconds_since_app_start();
        if self.gesture.is_some() { self.wire_measure.layouts += 1; }
        // The person's own edit is under way: the camera stays where they work.
        let editing = self.drag.reflow() || self.gesture.is_some() || self.own_edit.take_if(|sent| gen > *sent).is_some();
        if self.drag.reflow() || self.gesture.is_some() || self.wire_settle {
            // The previous sheet's part at the same path: positions shift when a part is added.
            let circuit = self.design.as_ref().and_then(|design| design.units.get(self.unit));
            let previous = circuit.and_then(|unit| self.sheet_design.as_ref()?.unit(unit.id));
            let mut by_path = previous.map(|unit| ByPath::new(&unit.circuit));
            let parts = sheet.parts.iter().enumerate().map(|(n, part)| {
                if !part.placed || self.drag.member(n as u32) { return None; }
                let path = &circuit?.circuit.comps.get(n)?.path;
                let previous = by_path.as_mut()?.find(n, path)?;
                let current = self.scene.position(previous as u32, now)?;
                Some(Flight {
                    from: [(current[0] - part.at[0] as f64) as f32,
                        (current[1] - part.at[1] as f64) as f32, 0.0, 1.0],
                    to: [0.0, 0.0, 0.0, 1.0], span: [now as f32, 0.12],
                })
            }).collect();
            self.state.assemble_at = None;
            self.assemble_pending = false;
            self.state.fly = Some(FlyFrom { parts, detail: Flight {
                from: [0.0, 0.0, 0.0, 1.0], to: [0.0, 0.0, 0.0, 1.0], span: [0.0; 2],
            }, leg: Leg::Settle, start: now, continued: false, text_from: Vec::new() });
            if let Some(unit) = self.design.as_ref().and_then(|design| design.units.get(self.unit)) {
                self.drag.layout(gen, &sheet, &unit.circuit, now);
            }
        }
        self.wire_settle = false;
        // A new sheet may have moved or unpinned the hovered part: its pin chip goes until the next hover.
        self.drag.release_pinned(now);
        if self.ghost_src.is_none() && self.gesture.as_ref()
            .is_some_and(|gesture| Arc::ptr_eq(&gesture.frozen().sheet, &self.sheet))
        {
            self.ghost_take = true;
        }
        let bounds_before = self.sheet.bounds;
        self.sheet = Arc::new(sheet);
        self.sheet_design = self.design.clone();
        self.shown_keep = kept;
        self.sheet_rows = self.rows.clone();
        self.live_trees = Some(trees);
        self.live_pieces = Arc::new(pieces);
        self.sync_live(cx);
        if !cx.has_global::<ElecLive>() {
            if let Some(unit) = self.sheet_design.as_ref().and_then(|design| design.units.get(self.unit)) {
                self.mini_slots = subscribe_minis(None, TreeOwner::Sheet(unit.id, 0), &unit.circuit, &self.sheet);
            }
            self.scene_live = Arc::new(SceneLive { mini: self.mini_slots.clone(), ..SceneLive::default() });
        }
        self.dirty = true;
        log!("fab.explorer: layout {} parts in {:.2} ms (worker)",
            self.sheet.parts.iter().filter(|p| p.placed).count(), ms);
        if first {
            self.replay_assemble(cx);
        }
        // A relayout for the pane's shape refits; so does any new layout that moves the sheet's
        // bounds while the camera keeps its fit (a design update after the first fit), unless the
        // person's own edit is under way or just landed (a paste stays under the pointer).
        let follow = self.pane.keeps_fit && self.sheet.bounds != bounds_before && !editing;
        if (self.refit_after_layout || follow) && gen == self.gen && !self.field_open() {
            self.refit_after_layout = false;
            if self.opening_view.is_none() {
                self.fit(cx);
            }
        }
        if gen == self.gen {
            if let Some(focus) = self.reveal_focus {
                self.request_reveal(cx, focus);
            }
        }
        self.draw_bg.redraw(cx);
    }

    /// Register worker-built wiring and refresh indices only when geometry or lowering changes.
    fn sync_live(&mut self, cx: &mut Cx) {
        if !cx.has_global::<ElecLive>() { return; }
        let sim = &mut cx.get_global::<ElecLive>().sim;
        let mut changed = false;
        let start = Cx::monotonic_now();
        let mut registered = None;
        if let Some(unit) = self.sheet_design.as_ref().and_then(|design| design.units.get(self.unit)) {
            if let Some(trees) = self.live_trees.take() {
                let owner = TreeOwner::Sheet(unit.id, 0);
                self.live_rows = sim.trees.set_prepared(owner, trees);
                registered = Some((Cx::monotonic_now() - start) * 1e3);
                self.mini_slots = subscribe_minis(Some(&mut sim.minis), owner, &unit.circuit, &self.sheet);
                self.live_owner = Some(owner);
                changed = true;
            }
        }
        if !self.live_index.as_ref().is_some_and(|index| Arc::ptr_eq(index, &sim.index)) {
            self.live_index = Some(sim.index.clone());
            changed = true;
        }
        if !changed { return; }
        self.scene_live = Arc::new(self.sheet_design.as_ref().map_or_else(SceneLive::default, |design| {
            SceneLive::new(&sim.index, design, self.unit, Some((&self.live_pieces, &self.live_rows)), &self.mini_slots)
        }));
        // Once per layout or lowering, never per frame: the UI thread's share of live wiring.
        log!("fab.explorer: live wiring on the UI thread: rows {}, indices {:.2} ms",
            registered.map_or_else(|| "kept".to_owned(), |ms| format!("{ms:.2} ms")), (Cx::monotonic_now() - start) * 1e3);
        if self.reveal.is_some() {
            let live = self.reveal_live();
            if let (Some(reveal), Some(templates)) = (&mut self.reveal, &self.templates) {
                reveal.set_live(live, Tokens::for_theme(self.dark), templates);
            }
        }
        self.dirty = true;
    }

    /// Resolve the reveal's net and part numbering and its own wire segment rows.
    fn reveal_live(&self) -> Arc<SceneLive> {
        Arc::new(match (&self.live_index, &self.sheet_design) {
            (Some(index), Some(design)) => SceneLive::new(index, design, self.unit,
                Some((&self.reveal_pieces, &self.reveal_rows)), &self.reveal_mini_slots),
            _ => SceneLive::default(),
        })
    }

    /// Release the reveal's segment rows and mini slots when its geometry is no longer drawn.
    fn drop_reveal_live(&mut self, cx: &mut Cx) {
        if let Some(owner) = self.reveal_owner.take() {
            if cx.has_global::<ElecLive>() {
                let sim = &mut cx.get_global::<ElecLive>().sim;
                sim.trees.remove(owner);
                sim.minis.remove(owner);
            }
        }
        self.reveal_rows.clear();
        self.reveal_mini_slots.clear();
        self.reveal_pieces = Arc::default();
    }

    /// Bind the host's live and item-state row textures. The host writes them once a
    /// frame; the view writes nothing live. Set again only when a texture is replaced.
    pub fn set_live(&mut self, cx: &mut Cx, live: CanvasLive) {
        self.live = Some(live);
        self.draw_bg.redraw(cx);
    }

    /// The selected part, else the revealed one. Sel has no empty selection: a view that clears
    /// it sends `ViewAction::Unfocus`.
    pub fn focus(&self) -> Option<Sel> {
        let node = self.selected_node().or(self.reveal_focus)?;
        let unit = self.design.as_ref()?.units.get(self.unit)?;
        let comp = unit.circuit.comps.get(node as usize)?;
        Some(Sel::Comp(CompRef { board: unit.id, comp: comp.id }))
    }

    pub fn view_actions(&self, actions: &Actions) -> Vec<ViewAction> {
        actions.filter_widget_actions(self.uid)
            .filter_map(|action| action.action.downcast_ref::<ViewAction>().cloned())
            .collect()
    }

    pub fn set_dark(&mut self, cx: &mut Cx, dark: bool) {
        if self.dark != dark {
            self.dark = dark;
            self.dirty = true;
            let tokens = Tokens::for_theme(dark);
            self.rebuild_chip(tokens);
            if let (Some(reveal), Some(templates)) = (&mut self.reveal, &self.templates) {
                reveal.rebuild(tokens, templates);
            }
            self.draw_bg.redraw(cx);
        }
    }

    pub fn replay_assemble(&mut self, cx: &mut Cx) {
        if self.design.is_some() && self.applied_gen >= self.design_gen {
            self.assemble_pending = true;
            self.request_frame(cx);
            self.draw_bg.redraw(cx);
        }
    }

    pub fn held_view(&self) -> Option<SavedView2d> {
        self.opening_view.as_ref().map_or_else(|| self.pane.held_view(), |opening| opening.saved)
    }

    pub fn restore_view(&mut self, cx: &mut Cx, view: Option<SavedView2d>) {
        self.opening_view = ViewOnArrival::new(view, Some(&self.sheet));
        self.draw_bg.redraw(cx);
    }

    /// Fits the whole sheet at the size the pane draws at next, at once: a layout was applied
    /// (V9). A sash drag refits only while the camera keeps its fit (no pan or zoom since).
    /// A plate's saved view not shown yet places the sheet the first time instead (7b).
    pub fn fit_when_shown(&mut self, cx: &mut Cx) {
        if self.opening_view.as_ref().is_some_and(|opening| opening.saved.is_some()) {
            return;
        }
        self.pane.keeps_fit = true;
        self.refit = true;
        self.draw_bg.redraw(cx);
    }

    pub fn fit(&mut self, cx: &mut Cx) {
        if self.pane.placed {
            let now = cx.seconds_since_app_start();
            self.pane.fit_whole(self.sheet.bounds.map(f64::from), FIT_MARGIN, now);
            self.pane.animate(cx);
        }
        self.draw_bg.redraw(cx);
    }

    fn request_frame(&mut self, cx: &mut Cx) {
        if self.frame == NextFrame::default() {
            self.frame = cx.new_next_frame();
        }
    }

    /// The projected view around part `focus` (a double-click): the sheet laid out again around
    /// it on a worker, the camera flown to it. Layout refreshes keep the current selection.
    fn request_reveal(&mut self, cx: &mut Cx, focus: u32) {
        let Some(design) = self.design.clone() else { return };
        if !design.units.get(self.unit).is_some_and(|unit| (focus as usize) < unit.circuit.comps.len()) {
            self.leave_reveal(cx, false);
            return;
        }
        self.reveal_focus = Some(focus);
        self.reveal_gen += 1;
        self.pending_reveal = None;
        self.fit_after_home = false;
        // An edit's graph and sheet must have the same indices and generation.
        if self.applied_gen != self.gen || !self.sheet.parts.get(focus as usize).is_some_and(|p| p.placed) {
            return;
        }
        self.layout_jobs.reveal(cx, design, self.unit, self.sheet.clone(), focus, self.reveal_gen);
    }

    fn apply_reveal(&mut self, cx: &mut Cx, revealed: Revealed) {
        if revealed.gen != self.reveal_gen || self.reveal_focus != Some(revealed.reveal.focus) || self.applied_gen != self.gen {
            return;
        }
        if self.scrub.is_some() || self.field_open() || self.busy() || self.templates.is_none() {
            self.pending_reveal = Some(revealed);
            return;
        }
        let (Some(design), Some(templates)) = (&self.sheet_design, &self.templates) else { return };
        let Revealed { reveal: data, trees, pieces, .. } = revealed;
        let now = cx.seconds_since_app_start();
        let frame = data.frame.map(f64::from);
        let start = Cx::monotonic_now();
        if let Some(unit) = design.units.get(self.unit) {
            let owner = TreeOwner::Sheet(unit.id, REVEAL_SHEET);
            if cx.has_global::<ElecLive>() {
                let sim = &mut cx.get_global::<ElecLive>().sim;
                self.reveal_rows = sim.trees.set_prepared(owner, trees);
                self.reveal_mini_slots = subscribe_minis(Some(&mut sim.minis), owner, &unit.circuit, &data.sheet);
                self.reveal_owner = Some(owner);
            } else {
                self.reveal_mini_slots = subscribe_minis(None, owner, &unit.circuit, &data.sheet);
            }
        }
        self.reveal_pieces = Arc::new(pieces);
        let live = self.reveal_live();
        let ms = (Cx::monotonic_now() - start) * 1e3;
        log!("fab.explorer: reveal wiring on the UI thread: {ms:.2} ms");
        self.reveal = Some(Reveal::new(data, self.reveal.take(), &self.sheet, &self.scene,
            design, self.unit, &self.sheet_rows, live, Tokens::for_theme(self.dark), templates, now));
        self.pane.nav.fly_to_box(&self.pane.cam, frame, 40.0, now, None);
        self.pane.moved_by_hand();
        self.pane.animate(cx);
        self.request_frame(cx);
        self.draw_bg.redraw(cx);
    }

    fn leave_reveal(&mut self, cx: &mut Cx, fit: bool) {
        // Sel::Unit selects a unit; it is not an empty selection. The host reads focus().
        self.reveal_focus = None;
        self.reveal_gen += 1;
        self.pending_reveal = None;
        self.fit_after_home |= fit;
        if let (Some(reveal), Some(templates)) = (&mut self.reveal, &self.templates) {
            reveal.home(&self.sheet, Tokens::for_theme(self.dark), templates, cx.seconds_since_app_start());
            self.request_frame(cx);
        } else if fit {
            self.fit_after_home = false;
            self.fit(cx);
        }
    }

    /// Probe the reveal first, then the sheet, naming the hit from the displayed design.
    fn probe_at(&self, abs: DVec2, now: f64) -> Option<(String, String)> {
        self.probe_at_world(self.pane.cam.to_world(abs), now)
    }

    /// A wire clicked: its lane in the Scope comes into view there, the tab staying in front.
    fn click_net(&mut self, cx: &mut Cx, net: NetRef) {
        let Some(name) = self.sheet_design.as_ref().and_then(|design| design.probe_net(net)).map(str::to_owned) else { return };
        probe_on_click(cx, &name);
    }

    /// [`Self::probe_at`] at a world point: the probed net's name and the channel's label.
    fn probe_at_world(&self, at: [f64; 2], now: f64) -> Option<(String, String)> {
        let design = self.sheet_design.as_ref()?;
        let radius = HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12);
        let (net, pin, node) = self.reveal.as_ref().and_then(|reveal| reveal.scene.probe_hit(at, radius, now))
            .or_else(|| self.scene.probe_hit(at, radius, now))?;
        let name = probe_name(design, self.unit, net)?;
        let label = match pin {
            Some(pin) => probe_label(design.units.get(self.unit)?.circuit.comps.get(node as usize)?, pin)?,
            None => name.clone(),
        };
        Some((name, label))
    }

    /// Resolve shared probe lighting once per name change, including probes from other panes.
    fn sync_probe_lit(&mut self, cx: &mut Cx) {
        let hovered = cx.global::<ProbeGesture>().hovered();
        if self.probe_lit.as_ref().map(|(name, _)| name.as_str()) == hovered { return; }
        self.probe_lit = hovered.map(|name| {
            let item = self.sheet_design.as_ref().and_then(|design| {
                let unit = design.units.get(self.unit)?;
                let net = (0..unit.circuit.nets.len()).find(|&net| probe_name(design, self.unit, net as u32).as_deref() == Some(name))?;
                self.scene.item_of(&Sel::Net(NetRef { board: unit.id, net: NetId(net as u32) }))
            });
            (name.to_owned(), item)
        });
        self.draw_bg.redraw(cx);
    }

    fn value_scene_at(&self, at: [f64; 2], radius: f64, now: f64) -> Option<(&Scene, u32)> {
        if let Some(reveal) = &self.reveal {
            if let Some(node) = reveal.scene.value_at_time(at, radius, now) {
                return Some((&reveal.scene, node));
            }
        }
        self.scene.value_at_time(at, radius, now).map(|node| (&self.scene, node))
    }

    fn value_box(&self, item: u32, now: f64) -> Option<[f64; 4]> {
        if let Some(reveal) = &self.reveal {
            if let Some(node) = reveal.scene.part_node(item).filter(|&node| reveal.scene.has_part(node)) {
                return reveal.scene.value_text(node).filter(|text| text.alpha(now) > 0.0)
                    .and_then(|_| reveal.scene.value_box_at(node, now));
            }
        }
        self.scene.value_box_at(self.scene.part_node(item)?, now)
    }

    fn field_open(&self) -> bool {
        self.value_edit.is_some() || self.name_edit.is_some()
    }

    /// A field keeps its displayed layout, except when a wire's cancelled hover preview is
    /// giving way to the committed sheet that the field names. O(1) per arriving layout.
    fn field_holds_sheet(&self) -> bool {
        if let Some(NameEdit::Net(NetNameEdit { wire_end: Some(end), .. })) = &self.name_edit {
            let on_base = |design: &Arc<Design>| design.units.get(self.unit)
                .is_some_and(|unit| Arc::ptr_eq(&unit.circuit, &end.circuit));
            return self.sheet_design.as_ref().is_some_and(on_base) || !self.design.as_ref().is_some_and(on_base);
        }
        self.field_open()
    }

    /// Open on a picked label target or the frozen start of a wire released at `at`.
    /// Opening from a wire only inspects its pin's connections.
    fn open_net_name(&mut self, cx: &mut Cx, at: [f64; 2], wire: Option<(Target, &Arc<Circuit>)>) {
        if self.field_open() || self.busy() || self.scrub.is_some() { return; }
        self.clear_note();
        let (target, circuit) = match wire {
            Some((target, base)) => {
                if !self.pane.cam.view.contains(self.pane.cam.to_screen(at)) { return; }
                if !cx.get_global_ref::<ElecLive>().and_then(|host| host.snap.design.unit(base.unit))
                    .is_some_and(|unit| Arc::ptr_eq(&unit.circuit, base)) { return; }
                (target, base)
            }
            None => {
                let Some((target, shown)) = self.edit_target(at) else { return };
                (target, &shown.circuit)
            }
        };
        let on = label_net(circuit, target);
        let (net, text) = if wire.is_some() {
            let net = on.ok().map(|net| (net.id, net.name.clone()));
            let text = net.as_ref().map(|(_, name)| name.clone()).unwrap_or_default();
            (net, text)
        } else if self.armed.tool == CircuitTool::Global {
            if !matches!(target, Target::Pin { .. } | Target::WireEnd { .. }) { return; }
            (None, on.map_or_else(|_| self.last_global.clone(), |net| net.name.clone()))
        } else {
            match on {
                Ok(net) => (Some((net.id, net.name.clone())), net.name.clone()),
                Err("") => return,
                Err(chip) => {
                    self.show_note(cx, chip.into(), at, false);
                    return;
                }
            }
        };
        let pane = self.pane.cam.view;
        let screen = pane.contain(Rect {
            pos: self.pane.cam.to_screen(at), size: dvec2(pane.size.x.min(160.0), pane.size.y.min(24.0)),
        });
        let wire_end = wire.map(|(_, base)| WireName { circuit: base.clone(), other: None });
        let edit = NetNameEdit { target, board: circuit.unit, net, wire_end, at, screen };
        if wire.is_none() && !self.design.as_ref().and_then(|d| d.units.get(self.unit))
            .is_some_and(|unit| edit.matches(&unit.circuit)) { return; }
        self.focus_name_field(cx, &text);
        self.name_edit = Some(NameEdit::Net(edit));
    }

    /// Select the initial text and focus the shared input as soon as its target is drawn.
    fn focus_name_field(&mut self, cx: &mut Cx, text: &str) {
        self.value_field.set_font_size(cx, 12.0);
        self.value_field.set_text(cx, text);
        self.value_field.select_all(cx);
        self.value_focus_pending = true;
        self.pane.nav.stop();
        self.draw_bg.redraw(cx);
    }

    /// Draw the text input at its net or card field in screen points.
    fn draw_name_field(&mut self, cx: &mut Cx2d, scope: &mut Scope, rect: Rect) {
        let walk = Walk { abs_pos: Some(rect.pos), width: Size::Fixed(rect.size.x), height: Size::Fixed(rect.size.y), ..Walk::default() };
        let _ = self.value_field.draw_walk(cx, scope, walk);
        if self.value_focus_pending {
            self.value_focus_pending = false;
            cx.set_key_focus_from_user(self.value_field.area());
        }
    }

    /// Route a single input event. Only submission can write the source.
    fn dispatch_name_field(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        for action in cx.capture_actions(|cx| self.value_field.handle_event(cx, event, scope)) {
            match action.as_widget_action().cast() {
                TextInputAction::Returned(text, _) => self.commit_field_text(cx, &text),
                TextInputAction::Changed(text) => { self.wire_name_text(cx, &text, false); }
                TextInputAction::KeyFocusLost | TextInputAction::Escaped if self.name_edit.is_some() => self.close_value_edit(cx),
                _ => {}
            }
        }
    }

    /// Preview or submit the wire-end field against the same committed source. Each text change
    /// plans once; only a different matching net needs new highlight strokes.
    fn wire_name_text(&mut self, cx: &mut Cx, text: &str, submit: bool) -> bool {
        let Some(NameEdit::Net(edit)) = &mut self.name_edit else { return false };
        let Some(end) = &mut edit.wire_end else { return false };
        if !cx.get_global_ref::<ElecLive>().and_then(|host| host.snap.design.unit(edit.board))
            .is_some_and(|unit| Arc::ptr_eq(&unit.circuit, &end.circuit)) {
            self.close_value_edit(cx);
            return true;
        }
        let at = edit.at;
        let planned = plan(&end.circuit, &Gesture::NameEnd { from: edit.target, name: text.into() });
        if submit && !matches!(planned, Planned::Refused { .. }) {
            self.close_value_edit(cx);
            if matches!(planned, Planned::Edit(_)) { self.send_sheet_plan(cx, planned, at); }
            return true;
        }
        let other = end.circuit.net(text.trim()).map(|net| net.id)
            .filter(|id| !edit.net.as_ref().is_some_and(|(start, _)| start == id));
        if end.other != other {
            end.other = other;
            self.publish_name_lines();
        }
        match planned {
            Planned::Edit(r) => self.show_note(cx, format!("Enter: {}", r.label), at, false),
            Planned::Refused { chip, .. } => self.show_note(cx, chip, at, true),
            Planned::Nothing { chip } if !chip.is_empty() => self.show_note(cx, chip, at, false),
            _ => { self.clear_note(); self.draw_bg.redraw(cx); }
        }
        true
    }

    /// Draw the selection rim on the matching wires, flags, rails and pin stubs. Scan the
    /// displayed strokes and pins once per matching pair or scene change. Draws reuse the rows.
    fn publish_name_lines(&mut self) {
        let Some(NameEdit::Net(edit)) = &self.name_edit else { return };
        let Some(end) = &edit.wire_end else { return };
        let Some(dest) = end.other else { self.name_lines.set(1, Vec::new()); return };
        let Some(shown) = self.sheet_design.as_ref().and_then(|d| d.unit(edit.board)) else { return };
        let Some(templates) = &self.templates else { return };
        // A last hover preview can still be on screen. Resolve by name in that scene's rows.
        let nets = [edit.net.as_ref().map(|(id, _)| *id), Some(dest)].map(|id| id.and_then(|id| {
            if Arc::ptr_eq(&shown.circuit, &end.circuit) { Some(id.0) }
            else { shown.circuit.net(&end.circuit.nets.get(id.ix())?.name).map(|n| n.id.0) }
        }));
        let items = nets.map(|n| n.and_then(|n| self.sheet_rows.net(n)).map(|row| row as f32));
        let mut rows = Vec::new();
        for line in self.scene.lines.iter().filter(|line| items.contains(&Some(line.c.item))) {
            let c = Common { look: Look::Focus.code(), ..line.c };
            templates.line.append(&mut rows, &LineInst { c, ..*line });
        }
        // A connection drawn directly between pins can have no routed stroke of its own.
        let c = Common { look: Look::Focus.code(), ..Common::at([0.0; 3], Tokens::for_theme(self.dark).ink) };
        for pin in self.scene.hits.pins.iter().filter(|pin| pin.net.is_some() && nets.contains(&pin.net)) {
            let [x1, y1] = pin.inner.map(|v| v as f32);
            let [x2, y2] = pin.outer.map(|v| v as f32);
            templates.line.append(&mut rows, &LineInst { c, seg: [x1, y1, x2, y2], width: [glyphs::WIRE_W, 0.0], ..LineInst::default() });
        }
        self.name_lines.set(templates.line.slots(), rows);
    }

    /// Enter submits the active reference or sends a nonblank net name through the planner once.
    fn commit_field_text(&mut self, cx: &mut Cx, text: &str) {
        if self.wire_name_text(cx, text, true) { return; }
        let edit = match &self.name_edit {
            Some(NameEdit::Reference(edit)) => {
                if let Some((label, op)) = edit.submission(text) {
                    let op = DesignOp::Circuit { unit: edit.part.board, op };
                    self.send_card_edit(cx, label, op);
                }
                self.close_value_edit(cx);
                return;
            }
            Some(NameEdit::Net(edit)) => edit,
            None => return,
        };
        let at = edit.at;
        if text.trim().is_empty() {
            self.show_note(cx, "enter a net name".into(), at, true);
            return;
        }
        let Some(unit) = self.design.as_ref().and_then(|d| d.units.get(self.unit)) else { return };
        let gesture = match edit.net {
            Some(_) => Gesture::NameNet { target: edit.target, to: text.into() },
            None => Gesture::Global { target: edit.target, name: text.into() },
        };
        if edit.net.is_none() { self.last_global = text.trim().to_string(); }
        let planned = plan(&unit.circuit, &gesture);
        self.close_value_edit(cx);
        self.send_sheet_plan(cx, planned, at);
    }

    fn close_value_edit(&mut self, cx: &mut Cx) {
        if !self.field_open() { return; }
        if let Some(edit) = self.value_edit.take() {
            let combo = self.value_combo.as_combo_box();
            combo.set_text(cx, super::value_edit::edit_text(&edit.current));
            combo.close_list(cx);
            let input = combo.input_area();
            if !input.is_empty() && cx.has_key_focus(input) { cx.set_key_focus(self.draw_bg.area()); }
        }
        if let Some(edit) = self.name_edit.take() {
            self.name_keys.clear();
            if matches!(&edit, NameEdit::Net(NetNameEdit { wire_end: Some(_), .. })) {
                self.name_lines.set(1, Vec::new());
            }
            if matches!(edit, NameEdit::Net(_)) {
                self.clear_note();
                if self.armed.tool.names_nets() { self.set_armed(cx, self.armed.done()); }
            }
            if cx.has_key_focus(self.value_field.area()) { cx.set_key_focus(self.draw_bg.area()); }
        }
        self.value_focus_pending = false;
        if let Some(layout) = self.pending_sheet.take() {
            self.apply_layout(cx, layout);
        }
        if let Some(revealed) = self.pending_reveal.take() {
            self.apply_reveal(cx, revealed);
        }
        self.draw_bg.redraw(cx);
    }

    fn clear_note(&mut self) {
        if self.note.take().is_some() {
            self.chip.set(1, Vec::new());
            self.chip_lines.set(1, Vec::new());
            self.chip_arcs.set(1, Vec::new());
        }
    }

    /// Release frozen draw data and its layers together when the gesture ends.
    fn clear_ghosts(&mut self) {
        self.ghost_src = None;
        self.ghost_take = false;
        self.ghost_boxes.set(1, Vec::new());
        self.ghost_lines.set(1, Vec::new());
        self.ghost_arcs.set(1, Vec::new());
        self.ghost_pins.set(1, Vec::new());
    }

    /// Both kinds of released edit hold the display until the host answers.
    fn holding(&self) -> bool {
        self.hold.is_some() || self.gesture.as_ref().is_some_and(|gesture| gesture.phase().landing())
    }

    /// Move and wire gestures exclude other sheet edits and reveals.
    fn busy(&self) -> bool { self.drag.busy() || self.gesture.is_some() || self.hold.is_some() }

    /// A first pin click owns the sheet pointer until it lands or is cancelled.
    fn click_wire(&self) -> bool { self.gesture.as_ref().is_some_and(|g| g.phase() == Phase::ClickWire) }

    /// Sample the frozen sheet once. On release the host receives any new preview before its commit.
    fn wire_pointer(&mut self, cx: &mut Cx, at: [f64; 2], release: bool) {
        let Some(gesture) = &mut self.gesture else { return };
        let sample = self.wire_measure.sample(gesture, at, self.pane.cam.px_per_unit);
        if release {
            let step = gesture.release(self.pane.cam.px_per_unit);
            if let Some(action) = sample.action { self.wire_send(cx, action); }
            self.wire_step(cx, step);
        } else {
            self.wire_step(cx, sample);
        }
    }

    /// Cancel the active input and report whether the gesture took Escape.
    fn escape_gesture(&mut self, cx: &mut Cx) -> bool {
        let Some(gesture) = &mut self.gesture else { return false };
        if !gesture.phase().landing() { self.wire_measure.outcome = Some("cancelled"); }
        let step = gesture.escape();
        self.wire_step(cx, step);
        true
    }

    /// Cancel the active click-to-finish gesture when another tool or a secondary click takes the sheet.
    fn cancel_click(&mut self, cx: &mut Cx) {
        if self.drag.click_to_drop() {
            let now = cx.seconds_since_app_start();
            self.drag.cancel(cx, self.uid, now);
            self.draw_bg.redraw(cx);
        } else if self.gesture.as_ref().is_some_and(|g| matches!(g.phase(), Phase::ClickWire | Phase::ClickLanding)) {
            self.escape_gesture(cx);
        }
    }

    /// Sample a move or a wire at the pointer, including the last point when the click lands.
    fn click_pointer(&mut self, cx: &mut Cx, at: [f64; 2], release: bool) {
        if self.drag.click_to_drop() {
            let now = cx.seconds_since_app_start();
            if release { self.finish_drag(cx, at, now); }
            else { self.drag.sample(cx, self.uid, at, &self.sheet, now); }
            self.draw_bg.redraw(cx);
        } else {
            self.wire_pointer(cx, at, release);
        }
    }

    /// The wire or keyboard move takes primary clicks, including Space-left. Secondary and middle drags
    /// go through the pane's pan handling. Only a secondary click cancels on release.
    fn click_button(&mut self, cx: &mut Cx, event: &Event) -> bool {
        let area = self.draw_bg.area();
        match event {
            Event::MouseDown(_) | Event::MouseUp(_) | Event::TouchUpdate(_) | Event::FingerCancel(_) => {}
            Event::MouseMove(_) if cx.fingers.is_area_captured(area) => {}
            _ => return false,
        }
        match event.hits(cx, area) {
            Hit::FingerDown(e) => {
                self.click_button_dragged = false;
                if e.is_primary_hit() {
                    cx.set_key_focus(area);
                    self.pane.nav.stop();
                    self.click_pointer(cx, self.pane.cam.to_world(e.abs), false);
                    return true;
                }
            }
            Hit::FingerMove(e) => {
                self.click_button_dragged |= e.move_distance() > DRAG_THRESHOLD;
                if e.is_primary_hit() && !self.pane.pressed() {
                    self.click_pointer(cx, self.pane.cam.to_world(e.abs), false);
                    return true;
                }
            }
            Hit::FingerUp(e) => {
                let click = e.is_over && !e.cancelled && !self.click_button_dragged
                    && (e.abs - e.abs_start).length() <= DRAG_THRESHOLD;
                if e.is_primary_hit() && !self.pane.pressed() {
                    if click { self.click_pointer(cx, self.pane.cam.to_world(e.abs), true); }
                    return true;
                }
                if click && e.mouse_button().is_some_and(|b| b.contains(MouseButton::SECONDARY)) {
                    self.cancel_click(cx);
                }
            }
            _ => {}
        }
        false
    }

    /// Take the grip from the selection in pick order, or hit-test the sheet once on the key.
    fn key_move(&mut self, cx: &mut Cx) {
        if self.armed.tool != CircuitTool::Select || self.busy() || self.pane.pressed() || self.field_open()
            || self.carry_armed.is_some() || self.scrub.is_some() || self.reveal.is_some() || self.reveal_focus.is_some() { return; }
        let now = cx.seconds_since_app_start();
        let pointer = self.pane.pointer_world().filter(|_| cx.fingers.is_area_hovered(self.draw_bg.area()));
        let radius = HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12);
        let node = match self.selected_node() {
            Some(first) => {
                let under = pointer.and_then(|at| self.selected.iter().find(|(node, _)| self.scene.body_contains(*node, at, radius, now)));
                Some(under.map_or(first, |(node, _)| *node))
            }
            None => pointer.and_then(|at| self.scene.body_at(at, radius, now)),
        };
        let Some(node) = node else {
            self.show_note(cx, "select a part to move".into(), pointer.unwrap_or(self.pane.cam.center), false);
            return;
        };
        let Some(from) = pointer.or_else(|| {
            let [x0, y0, x1, y1] = self.scene.part_box(node)?;
            let [dx, dy] = self.scene.offset(node, now);
            Some([(x0 + x1) * 0.5 + dx, (y0 + y1) * 0.5 + dy])
        }) else { return };
        self.clear_note();
        self.pane.nav.stop();
        self.begin_part_drag(node, from, true, now);
    }

    /// Build the retained copy and publish its connection rows once for either kind of move.
    fn begin_part_drag(&mut self, node: u32, from: [f64; 2], click_to_drop: bool, now: f64) {
        let (Some(unit), Some(templates)) = (self.sheet_design.as_ref().and_then(|d| d.units.get(self.unit)), &self.templates) else { return };
        let selected = if click_to_drop || (self.selected.len() > 1 && self.selected.iter().any(|(n, _)| *n == node)) {
            self.selected.as_slice()
        } else { &[] };
        if let Some(item) = self.drag.begin(node, selected, from, click_to_drop, &unit.circuit, unit.id, &self.sheet, &mut self.scene,
            templates, Tokens::for_theme(self.dark), now)
        {
            self.drag_pin_item = Some(item);
            self.publish_connections();
        }
    }

    /// Sample the release and hand the drop's hints to the same layout job for a click or a drag.
    fn finish_drag(&mut self, cx: &mut Cx, at: [f64; 2], now: f64) {
        self.drag.sample(cx, self.uid, at, &self.sheet, now);
        match self.drag.finish(cx, self.uid, &mut self.scene, &self.sheet, now) {
            Finish::None => {}
            Finish::Done => self.publish_connections(),
            Finish::Drop { path, hint, token } => match self.design.clone() {
                Some(design) => {
                    let opts = self.layout_opts();
                    self.layout_jobs.drop_hints(cx, design, self.unit, path, hint, token, opts)
                }
                None => if let Some(tag) = self.drag.dropped(cx, self.uid, token, Vec::new(), &mut self.scene) {
                    self.hold_drag(cx, tag);
                },
            },
        }
    }

    /// Retain the committed base of a move's tag and publish its released connection rows.
    fn hold_drag(&mut self, cx: &mut Cx, tag: u64) {
        if !self.pinned && cx.has_global::<ElecLive>() {
            let snap = &cx.get_global::<ElecLive>().snap;
            self.hold = self.sheet_design.as_ref().and_then(|design| design.units.get(self.unit))
                .and_then(|unit| snap.design.unit(unit.id))
                .map(|unit| (tag, unit.id, unit.circuit.clone()));
        }
        self.publish_connections();
    }

    /// What a press at `at` would carry: a wire end, or a pin only on its stub, outward of the
    /// body edge. The part's body box includes its pins, so a press inside the glyph moves the part.
    fn wire_source(&self, at: [f64; 2]) -> Option<Target> {
        match targets::resolve(&self.scene.hits, &self.sheet, at, self.pane.cam.px_per_unit, &Target::None) {
            source @ Target::Pin { comp, pin, .. } => {
                let hit = self.scene.hits.pins.iter().find(|hit| hit.node == comp.0 && hit.pin == u32::from(pin.0))?;
                let out = [hit.outer[0] - hit.inner[0], hit.outer[1] - hit.inner[1]];
                ((at[0] - hit.inner[0]) * out[0] + (at[1] - hit.inner[1]) * out[1] >= 0.0).then_some(source)
            }
            source @ Target::WireEnd { .. } => Some(source),
            _ => None,
        }
    }

    /// Start only from the committed sheet whose positional ids still name the displayed part.
    fn begin_wire(&mut self, cx: &mut Cx, from: [f64; 2], at: [f64; 2]) -> bool {
        if self.pinned || self.busy() || self.reveal.is_some() || self.reveal_focus.is_some() || !cx.has_global::<ElecLive>() { return false; }
        let Some(design) = &self.sheet_design else { return false };
        let Some(shown) = design.units.get(self.unit) else { return false };
        let snap = &cx.get_global::<ElecLive>().snap;
        if !snap.design.unit(shown.id).is_some_and(|unit| Arc::ptr_eq(&unit.circuit, &shown.circuit)) { return false; }
        let Some(source) = self.wire_source(from) else { return false };
        let (Target::Pin { comp, .. } | Target::WireEnd { comp, .. }) = source else { return false };
        let Some(unit) = self.design.as_ref().and_then(|design| design.units.get(self.unit)) else { return false };
        let Some(current) = unit.circuit.comps.get(comp.ix()) else { return false };
        if shown.id != unit.id || !shown.circuit.comps.get(comp.ix()).is_some_and(|shown| shown.path == current.path) { return false; }
        // Arc clones at the press only. All later hits and plans read these frozen objects (V1).
        let frozen = Frozen {
            design: design.clone(), unit: self.unit, circuit: shown.circuit.clone(),
            sheet: self.sheet.clone(), hits: self.scene.hits.clone(),
        };
        let Some(mut gesture) = EditGesture::begin(frozen, Source::Wire(source), from) else { return false };
        self.wire_measure = WireMeasure::default();
        let step = self.wire_measure.sample(&mut gesture, at, self.pane.cam.px_per_unit);
        self.gesture = Some(gesture);
        self.clear_note();
        self.drag.release_pinned(cx.seconds_since_app_start());
        self.wire_step(cx, step);
        true
    }

    /// The status bar's words, each written only when it changes (V7): the pointer's grid cell in
    /// mm, what the tool or the gesture does, and the shown circuit's counts (rebuilt when the
    /// circuit does, O(pins); otherwise a few short strings per draw).
    fn sync_status(&mut self, cx: &mut Cx) {
        let tool = self.armed.tool;
        let instruction = match &self.gesture {
            Some(gesture) if gesture.carries() => match self.carry_last {
                Some(Carried::Rail(..)) => hint(tool, Carrying::Rail),
                _ => "click to place · R turns · X mirrors · Y flips · Shift-click places another · Esc puts it back",
            },
            Some(gesture) if gesture.phase() == Phase::ClickWire => "click a pin, wire or tag to connect · click empty sheet to name · Esc cancels",
            Some(_) => hint(tool, Carrying::Wire),
            None if tool == CircuitTool::Wire => "click or drag from a pin to connect · finish on empty sheet to name · Esc",
            None if tool == CircuitTool::Select && self.selected.len() > 1 && !self.drag.busy() =>
                "G move · R / Shift-R turn · X / Y mirror · Shift-click toggles a part · Shift-drag adds parts · Delete removes selected parts · Esc clears",
            None if tool == CircuitTool::Select && self.selected.len() > 1 =>
                "G move · Shift-click toggles a part · Shift-drag adds parts · Delete removes selected parts · Esc clears",
            None if tool == CircuitTool::Select && self.selected.len() == 1 =>
                "G move · R / Shift-R turn · X mirrors left-right · Y flips top-bottom · Q on a pin toggles no-connect",
            None if tool == CircuitTool::Select => "G move or drag a part · drag empty sheet to select · Shift-click toggles · click or drag a pin to wire · A place · P power · W wire · Q no-connect · B probe · F fit",
            None => hint(tool, Carrying::Nothing),
        };
        let shown = self.sheet_design.as_ref().and_then(|design| design.units.get(self.unit)).map(|unit| unit.circuit.clone());
        let key = shown.as_ref().map_or(0, |circuit| Arc::as_ptr(circuit) as usize);
        if self.status_counts.as_ref().is_none_or(|(at, _)| *at != key) {
            self.status_counts = Some((key, shown.as_deref().map(counts).unwrap_or_default()));
        }
        let pos = self.status_cell.map(|[x, y]| format!("X {:.2}  Y {:.2} mm", x as f64 * STEP_MM, y as f64 * STEP_MM)).unwrap_or_default();
        let why = self.hover_why(cx);
        // a hovered wire's voltage at the play head, or a hovered block's whole path (its frame's
        // title shows only its own segment), unless a check has something to say
        let value = match (self.pane.hovered(), shown.as_deref()) {
            (Some(Sel::Net(net)), Some(circuit)) => circuit.nets.get(net.net.ix())
                .and_then(|n| crate::live::flags::net_value_line(cx, *net, &n.name)),
            (Some(Sel::Block(_, block)), Some(circuit)) => circuit.tree.get(block.0 as usize).filter(|b| !b.path.is_empty())
                .map(|b| match b.def.and_then(|d| circuit.block_defs.get(d.0 as usize)) {
                    Some(def) => format!("{} \u{b7} {}", b.path, def.name),
                    None => b.path.clone(),
                }),
            _ => None,
        };
        let words = [pos, why.or(value).unwrap_or_else(|| instruction.to_owned()),
            self.status_counts.as_ref().map(|(_, text)| text.clone()).unwrap_or_default()];
        for ((id, shown), word) in [ids!(s_pos), ids!(s_hint), ids!(s_counts)].into_iter().zip(self.status_shown.iter_mut()).zip(words) {
            if *shown != word {
                self.status.label(cx, id).set_text(cx, &word);
                *shown = word;
            }
        }
    }

    /// What a check says about the hovered pin, part or net (a part answers for its pins too), the
    /// most severe finding first; None when it has none. O(findings) when the hover or the
    /// findings change, O(1) otherwise.
    fn hover_why(&mut self, cx: &mut Cx) -> Option<String> {
        let hovered = self.pane.hovered().cloned();
        if !cx.has_global::<ElecLive>() {
            return None;
        }
        let violations = &cx.get_global::<ElecLive>().snap.violations;
        if let Some((at, generation, why)) = &self.status_why {
            if *at == hovered && *generation == violations.generation {
                return why.clone();
            }
        }
        let unit = self.design.as_ref().and_then(|d| d.units.get(self.unit)).map(|unit| unit.id);
        let why = hovered.as_ref().zip(unit).and_then(|(hovered, unit)| {
            let names = |item: &Sel| item == hovered || matches!((item, hovered),
                (Sel::Pin(pin), Sel::Comp(comp)) if pin.board == comp.board && pin.comp == comp.comp);
            violations.value.iter()
                .filter(|v| v.unit == Some(unit) && v.at.is_none() && v.items.iter().any(names))
                .max_by_key(|v| v.severity)
                .map(reason_line)
        });
        self.status_why = Some((hovered, violations.generation, why.clone()));
        why
    }

    /// Annotate (the strip's button): every part whose reference the source does not hold gets
    /// one, numbered by its place on the sheet, in one undoable edit written to the source.
    fn annotate_now(&mut self, cx: &mut Cx) {
        if self.busy() {
            return;
        }
        let Some(unit) = self.design.as_ref().and_then(|d| d.units.get(self.unit)) else { return };
        match fab_elec_explore::annotate::annotate(&unit.circuit, &self.sheet) {
            Planned::Edit(r) => cx.widget_action(self.uid, ViewAction::Ops { ops: r.ops, place: r.place, label: r.label }),
            Planned::Nothing { chip } | Planned::Refused { chip, .. } if !chip.is_empty() => {
                let at = self.pane.cam.center;
                self.show_note(cx, chip, at, false);
            }
            _ => {}
        }
    }

    /// Restores the Library float's side and fold state and requests one redraw.
    pub fn set_library_place(&mut self, cx: &mut Cx, left: bool, folded: bool) {
        self.library_left = left;
        self.library_folded = folded;
        self.draw_bg.redraw(cx);
    }

    /// The host shows the Library elsewhere (an area of its own): the float stays hidden, and
    /// Place part asks the host for its Library. A redraw only when it changes.
    pub fn set_library_hidden(&mut self, cx: &mut Cx, hidden: bool) {
        if self.library.visible() == hidden {
            self.library.set_visible(cx, !hidden);
            self.draw_bg.redraw(cx);
        }
    }

    fn choose_library_place(&mut self, cx: &mut Cx, left: bool, folded: bool) {
        if (left, folded) != (self.library_left, self.library_folded) {
            cx.action(LibraryAction::Place { left, folded });
        }
        self.set_library_place(cx, left, folded);
    }

    fn choose_library_fold(&mut self, cx: &mut Cx, folded: bool) {
        self.opened_while_narrow = self.library_narrow && !folded;
        self.choose_library_place(cx, self.library_left, folded);
    }

    fn library_is_folded(&self) -> bool {
        library_folded(self.library_folded, self.library_narrow, self.opened_while_narrow)
    }

    /// Place part (A, or the strip's button): the Library float opens and its search takes the
    /// keyboard once it has drawn. A host that shows the Library elsewhere hides the float: then
    /// the host is asked for its Library ([`ElecPanel::Library`]).
    fn open_library_search(&mut self, cx: &mut Cx) {
        self.cancel_click(cx);
        if !self.library.visible() {
            cx.action(ElecPanel::Library);
            return;
        }
        self.library_search_pending = true;
        self.choose_library_fold(cx, false);
    }

    /// How far the strip and open Library, as last drawn, reach from the sheet's two sides. O(1).
    fn covered_sides(&self, rect: Rect) -> (f64, f64) {
        let (mut left, mut right) = (0.0_f64, 0.0_f64);
        let strip = self.strip_at;
        if strip.size.x > 0.0 { left = left.max(strip.pos.x + strip.size.x - rect.pos.x); }
        let library = self.library_at;
        if !self.library_is_folded() && library.size.x > 0.0 {
            if self.library_left {
                left = left.max(library.pos.x + library.size.x - rect.pos.x);
            } else {
                right = (rect.pos.x + rect.size.x - library.pos.x).max(0.0);
            }
        }
        (left, right)
    }

    /// Where the inspector card and Check list fit between the strip and the open Library.
    fn card_room(&self, above: Rect) -> Rect {
        let (left, right) = self.covered_sides(above);
        Rect { pos: above.pos + dvec2(left, 0.0), size: dvec2((above.size.x - left - right).max(0.0), above.size.y) }
    }

    /// Places the Library or its tab in its chosen corner. One width comparison per draw
    /// decides whether to fold it for room.
    fn draw_library(&mut self, cx: &mut Cx2d, scope: &mut Scope, above: Rect) {
        const INSET: f64 = 8.0;
        self.library_at = Rect::default();
        if !self.library.visible() {
            return;
        }
        let room = dvec2((above.size.x - 2.0 * INSET).max(0.0), (above.size.y - 2.0 * INSET).max(0.0));
        let open = self.library.widget(cx, ids!(lib_open));
        let open_walk = open.walk(cx);
        self.library_narrow = library_is_narrow(open_walk.width.to_fixed().unwrap_or(room.x), above.size.x);
        if !self.library_narrow {
            self.opened_while_narrow = false;
        }
        let folded = self.library_is_folded();
        let place = (self.library_left, folded);
        if self.library_shown != Some(place) {
            open.set_visible(cx, !folded);
            self.library.widget(cx, ids!(lib_tab)).set_visible(cx, folded);
            self.library_shown = Some(place);
            // The inspector drew before the Library and needs the new rectangle on the next draw.
            self.draw_bg.redraw(cx);
        }
        let walk = if folded { self.library.widget(cx, ids!(lib_tab)).walk(cx) } else { open_walk };
        let fixed = |size: Size, room: f64| match size { Size::Fixed(v) => v.min(room), _ => room };
        let size = dvec2(fixed(walk.width, room.x), fixed(walk.height, room.y));
        let x = if self.library_left {
            let strip = self.strip_at;
            (strip.pos.x + strip.size.x + INSET).max(above.pos.x + INSET)
        } else {
            above.pos.x + above.size.x - INSET - size.x
        };
        self.library_at = Rect { pos: dvec2(x, above.pos.y + INSET), size };
        draw_float(cx, scope, self.uid, live_id!(library), &self.library, self.library_at);
        if !folded && std::mem::take(&mut self.library_search_pending) {
            self.library.widget(cx, ids!(library.search)).set_key_focus(cx);
        }
    }

    /// Glide to a finding's part (a pin's part, or the first part on a net) and select it, its
    /// card showing. A small part is framed with room around it, not blown up to fill the sheet.
    fn go_to(&mut self, cx: &mut Cx, sel: &Sel) {
        let Some(unit) = self.sheet_design.as_ref().and_then(|d| d.units.get(self.unit)) else { return };
        let comp = match sel {
            Sel::Comp(comp) if comp.board == unit.id => Some(comp.comp),
            Sel::Pin(pin) if pin.board == unit.id => Some(pin.comp),
            Sel::Net(net) if net.board == unit.id => unit.circuit.comps.iter()
                .find(|c| c.conns.iter().any(|(_, n)| *n == net.net)).map(|c| c.id),
            _ => None,
        };
        let Some(comp) = comp else { return };
        let Some(path) = unit.circuit.comps.get(comp.ix()).map(|part| part.path.clone()) else { return };
        let Some(current) = self.design.as_ref().and_then(|design| design.unit(unit.id)) else { return };
        let Some(node) = ByPath::new(&current.circuit).find(comp.ix(), &path) else { return };
        let Some(b) = self.scene.part_box(comp.0) else { return };
        let middle = [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5];
        let half = [((b[2] - b[0]) * 0.5).max(20.0), ((b[3] - b[1]) * 0.5).max(14.0)];
        let frame = [middle[0] - half[0], middle[1] - half[1], middle[0] + half[0], middle[1] + half[1]];
        self.pane.nav.fly_to_box(&self.pane.cam, frame, 40.0, cx.seconds_since_app_start(), None);
        self.pane.moved_by_hand();
        self.pane.animate(cx);
        self.close_value_edit(cx);
        self.set_selection(cx, vec![(node as u32, path)]);
        self.draw_bg.redraw(cx);
    }

    /// The Power flyout (P, or the strip's Power button): the shown circuit's ground and power
    /// nets, ground first, then by volts from high to low, for the host to show beside the button.
    /// Opening it scans the nets, sorts the rails and looks up the optional flag in the catalog.
    fn open_power(&mut self, cx: &mut Cx) {
        let Some(anchor) = power_anchor(cx, &self.tools) else { return };
        self.cancel_click(cx);
        let Some(unit) = self.sheet_design.as_ref().and_then(|design| design.units.get(self.unit)) else { return };
        let mut rails: Vec<_> = unit.circuit.nets.iter().filter(|net| matches!(net.kind, NetKind::Power | NetKind::Ground)).collect();
        rails.sort_by(|a, b| (a.kind != NetKind::Ground).cmp(&(b.kind != NetKind::Ground))
            .then(b.volts.map_or(0.0, |v| v.max).total_cmp(&a.volts.map_or(0.0, |v| v.max)))
            .then(a.name.cmp(&b.name)));
        rails.dedup_by(|a, b| a.name == b.name);
        let rails = rails.iter().map(|net| {
            let note = match net.volts {
                Some(v) if v.min == v.max => format!("{} V", v.max),
                Some(v) => format!("{} to {} V", v.min, v.max),
                None => String::new(),
            };
            (net.name.clone(), note)
        }).collect();
        let has_power_flag = power_flag_entry(cx).is_some();
        cx.action(StripAction::PowerMenu { rails, has_power_flag, anchor });
    }

    /// The shown circuit's rail named `name`, carried: its tag kind follows the net's kind.
    fn rail(&self, name: &str) -> Option<Carried> {
        let unit = self.sheet_design.as_ref()?.units.get(self.unit)?;
        let net = unit.circuit.nets.iter().find(|net| net.name == name && matches!(net.kind, NetKind::Power | NetKind::Ground))?;
        let kind = if net.kind == NetKind::Ground { sheet::TagKind::Ground } else { sheet::TagKind::Power };
        Some(Carried::Rail(net.id, kind, net.name.clone()))
    }

    /// Begin carrying a Library entry at world `at`, on the committed sheet only, as a wire
    /// begins. The chip rides the pointer; each grid cell, pin or tag under it is a drop plan.
    fn begin_carry(&mut self, cx: &mut Cx, carried: Carried, at: [f64; 2]) -> bool {
        if self.pinned || self.busy() || self.field_open() || self.reveal.is_some() || self.reveal_focus.is_some()
            || !cx.has_global::<ElecLive>() { return false; }
        let Some(design) = &self.sheet_design else { return false };
        let Some(shown) = design.units.get(self.unit) else { return false };
        let snap = &cx.get_global::<ElecLive>().snap;
        if !snap.design.unit(shown.id).is_some_and(|unit| Arc::ptr_eq(&unit.circuit, &shown.circuit)) { return false; }
        let frozen = Frozen {
            design: design.clone(), unit: self.unit, circuit: shown.circuit.clone(),
            sheet: self.sheet.clone(), hits: self.scene.hits.clone(),
        };
        self.carry_last = Some(carried.clone());
        let mut gesture = match carried {
            Carried::Part(template, title) => EditGesture::carry(frozen, template, &title, at),
            Carried::Rail(net, kind, name) => EditGesture::rail(frozen, net, kind, &name, at),
        };
        self.wire_measure = WireMeasure::default();
        let step = self.wire_measure.sample(&mut gesture, at, self.pane.cam.px_per_unit);
        self.gesture = Some(gesture);
        self.clear_note();
        self.drag.release_pinned(cx.seconds_since_app_start());
        self.wire_step(cx, step);
        true
    }

    /// One pointer sample of the carry at world `at`; false when nothing is carried.
    fn carry_sample(&mut self, cx: &mut Cx, at: [f64; 2]) -> bool {
        let Some(gesture) = self.gesture.as_mut().filter(|gesture| gesture.carries()) else { return false };
        let step = self.wire_measure.sample(gesture, at, self.pane.cam.px_per_unit);
        self.wire_step(cx, step);
        true
    }

    /// Place the carried entry where the click or drop at world `at` lands; `again` (Shift) carries
    /// it on once the placement has landed.
    fn carry_place(&mut self, cx: &mut Cx, at: [f64; 2], again: bool) {
        let Some(gesture) = self.gesture.as_mut().filter(|gesture| gesture.carries()) else { return };
        let sample = self.wire_measure.sample(gesture, at, self.pane.cam.px_per_unit);
        let step = gesture.release(self.pane.cam.px_per_unit);
        if let Some(action) = sample.action { self.wire_send(cx, action); }
        if step.phase == Phase::Landing { self.carry_again = again; }
        self.wire_step(cx, step);
    }

    /// End a carry that was not placed: Esc, a drag leaving the sheet, another entry armed.
    fn end_carry(&mut self, cx: &mut Cx) {
        self.carry_armed = None;
        self.carry_again = false;
        if let Some(gesture) = self.gesture.as_mut().filter(|gesture| gesture.carries() && gesture.phase() == Phase::Dragging) {
            self.wire_measure.outcome = Some("cancelled");
            let step = gesture.escape();
            self.wire_step(cx, step);
        }
    }

    /// The Library's input to the sheet: a click on an entry arms it (it rides the pointer from
    /// its next move over the sheet), and an entry dragged over the sheet is carried until it is
    /// dropped (placed) or leaves (cancelled). True when the event was the carry's alone.
    /// Cost: a catalog lookup when a carry begins; per pointer event one target resolve, and a
    /// plan only when the cell, pin or tag under the pointer changes.
    fn carry_event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        match event {
            Event::Actions(actions) => {
                let armed = actions.iter().filter_map(|action| action.downcast_ref::<LibraryAction>())
                    .filter_map(|action| match action { LibraryAction::Arm(key) => Some(key.clone()), _ => None }).last();
                let carried = armed.and_then(|key| library_template(cx, &key)).or_else(|| {
                    actions.iter().rev().find_map(|action| match action.downcast_ref::<StripAction>()? {
                        StripAction::Rail(name) => self.rail(name),
                        StripAction::PowerFlag => power_flag_entry(cx)
                            .map(|entry| Carried::Part(Arc::new(template_of(entry)), "Power flag".into())),
                        StripAction::PowerMenu { .. } => None,
                    })
                });
                if let Some(carried) = carried {
                    self.cancel_click(cx);
                    self.end_carry(cx);
                    self.carry_armed = Some(carried);
                }
                false
            }
            Event::Drag(drag) => {
                let Some(key) = library_drag_entry(&drag.items) else { return false };
                let DragHit::Drag(hit) = event.drag_hits(cx, self.draw_bg.area()) else { return true };
                if hit.state != DragState::Out { self.cancel_click(cx); }
                let at = self.pane.cam.to_world(hit.abs);
                let carrying = self.gesture.as_ref().is_some_and(|gesture| gesture.carries());
                let carried = match hit.state {
                    DragState::Out => {
                        if std::mem::take(&mut self.carry_drag) { self.end_carry(cx); }
                        false
                    }
                    _ if carrying => self.carry_drag && self.carry_sample(cx, at),
                    _ => match library_template(cx, &key) {
                        Some(carried) if self.gesture.is_none() => {
                            self.carry_armed = None;
                            self.carry_drag = self.begin_carry(cx, carried, at);
                            self.carry_drag
                        }
                        _ => false,
                    },
                };
                let lands = carried && self.gesture.as_ref()
                    .is_some_and(|gesture| gesture.tag().is_some() && gesture.refusal().is_none());
                *hit.response.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    if lands { DragResponse::Copy } else { DragResponse::None };
                true
            }
            Event::Drop(drop) => {
                if library_drag_entry(&drop.items).is_none() { return false; }
                if let DragHit::Drop(hit) = event.drag_hits(cx, self.draw_bg.area()) {
                    if std::mem::take(&mut self.carry_drag) {
                        let at = self.pane.cam.to_world(hit.abs);
                        self.carry_place(cx, at, false);
                    }
                }
                true
            }
            Event::DragEnd => {
                if std::mem::take(&mut self.carry_drag) { self.end_carry(cx); }
                false
            }
            Event::MouseMove(moved) if !self.carry_drag => {
                if self.carry_armed.is_none() && !self.gesture.as_ref().is_some_and(|gesture| gesture.carries()) { return false; }
                if !self.draw_bg.area().rect(cx).contains(moved.abs) { return false; }
                let at = self.pane.cam.to_world(moved.abs);
                if !self.carry_sample(cx, at) {
                    if let Some(carried) = self.carry_armed.clone() {
                        if self.begin_carry(cx, carried, at) { self.carry_armed = None; }
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Track only the newest preview's send time; pointer-only samples send and log nothing.
    fn wire_send(&mut self, cx: &mut Cx, action: ViewAction) {
        if let ViewAction::Preview { tag, label, .. } = &action {
            log!("fab.explorer: wire preview t{tag}: {label}");
            self.wire_measure.sent = Some((*tag, Cx::monotonic_now()));
            self.wire_measure.outcome = None;
        }
        cx.widget_action(self.uid, action);
    }

    /// Send one machine result and retain only useful refusal/no-op notes after it ends.
    fn wire_step(&mut self, cx: &mut Cx, step: Step) {
        if let Some(action) = step.action {
            if action != ViewAction::CommitPreview || !self.keep_carry(cx) { self.wire_send(cx, action); }
        }
        if matches!(step.phase, Phase::Idle | Phase::FlyBack) {
            let Some(mut gesture) = self.gesture.take() else { return };
            if gesture.carries() {
                // Shift-click carries the entry on once it has landed; a click-armed carry whose
                // committed design moved under it is armed again on the new sheet.
                let again = match self.wire_measure.outcome {
                    Some("committed") => self.carry_again,
                    Some("base moved") => !self.carry_drag,
                    _ => false,
                };
                self.carry_again = false;
                if again { self.carry_armed = self.carry_last.clone(); }
            }
            if let Some((chip, look)) = gesture.chip() {
                self.wire_measure.report(chip, self.wire_measure.outcome
                    .unwrap_or(if look == ChipLook::Refused { "refused" } else { "cancelled" }));
            }
            self.wire_measure = WireMeasure::default();
            self.wire_settle = true;
            self.clear_ghosts();
            self.clear_note();
            self.wire_press = gesture.take_deferred().filter(|_| self.pane.dragging().is_some())
                .map(|from| (from, self.wire_press.map_or(from, |(_, at)| at)));
            self.gesture_text = None;
            self.chip.set(1, Vec::new());
            self.chip_lines.set(1, Vec::new());
            self.chip_arcs.set(1, Vec::new());
            // A wire landed or let go is the Wire tool's one gesture.
            if self.armed.tool == CircuitTool::Wire {
                self.set_armed(cx, self.armed.done());
            }
            if step.phase == Phase::FlyBack {
                if let Some((chip, look)) = gesture.note() {
                    self.show_note(cx, chip.to_owned(), gesture.end_point(), look == ChipLook::Refused);
                }
            } else if let Some((from, at)) = step.name_end {
                self.open_net_name(cx, at, Some((from, &gesture.frozen().circuit)));
            }
        } else {
            self.rebuild_chip(Tokens::for_theme(self.dark));
        }
        self.draw_bg.redraw(cx);
    }

    /// Read the host before following it, so the Landing gate opens on its tagged answer.
    fn wire_snapshot(&mut self, cx: &mut Cx) {
        if !cx.has_global::<ElecLive>() { return; }
        let snap = cx.get_global::<ElecLive>().snap.clone();
        if self.hold.as_ref().is_some_and(|(tag, unit, circuit)| landed(&snap, *tag, *unit, circuit).is_some()) {
            self.hold = None;
            self.wire_settle = true;
            self.draw_bg.redraw(cx);
        }
        let Some(gesture) = &mut self.gesture else { return };
        let refused = gesture.refusal().is_some();
        let (step, answer) = gesture.snapshot(&snap);
        match answer {
            Some(Answer::Shown(tag)) => self.wire_measure.shown(tag),
            Some(Answer::Committed { .. }) => self.wire_measure.outcome = Some("committed"),
            Some(Answer::BaseMoved) => self.wire_measure.outcome = Some("base moved"),
            Some(Answer::Refused) if step.phase == Phase::FlyBack => self.wire_measure.outcome = Some("refused"),
            _ => {}
        }
        // Repeated snapshots at rest change no overlay rows.
        let changed = refused != gesture.refusal().is_some() || gesture.chip().is_some_and(|(chip, _)|
            self.gesture_text.as_ref().is_none_or(|text| text.text != chip));
        if changed || step.action.is_some() || matches!(step.phase, Phase::Idle | Phase::FlyBack) {
            self.wire_step(cx, step);
        }
    }

    /// Restart a deferred press only after the committed sheet and its scene are in hand.
    fn resume_wire(&mut self, cx: &mut Cx) {
        if self.gesture.is_some() || self.hold.is_some() { return; }
        let Some((from, at)) = self.wire_press else { return };
        if self.pane.dragging().is_none() { self.wire_press = None; return; }
        if self.dirty || !cx.has_global::<ElecLive>() { return; }
        let Some(shown) = self.sheet_design.as_ref().and_then(|design| design.units.get(self.unit)) else { return };
        if !cx.get_global::<ElecLive>().snap.design.unit(shown.id)
            .is_some_and(|unit| Arc::ptr_eq(&unit.circuit, &shown.circuit)) { return; }
        let items = WireItems {
            base: ExplorerItems { sheet: &self.scene, reveal: self.reveal.as_ref(), cam: &self.pane.cam,
                now: cx.seconds_since_app_start() },
            sheet: &self.sheet, unit: Some(shown.id), tool: self.armed.tool, click_pending: false,
            click: Default::default(), pin: Default::default(), empty: None,
        };
        let key = items.hit(from, HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12)).into_iter().next();
        self.wire_press = None;
        if let Some(key) = key { self.input(cx, CanvasInput::DragStart { key, from, at }); }
    }

    /// Resolve one sheet click or key against the geometry and circuit currently on screen.
    fn edit_target(&self, at: [f64; 2]) -> Option<(Target, &Unit)> {
        let (scene, sheet, design) = match &self.reveal {
            Some(reveal) => (&reveal.scene, reveal.sheet(), reveal.design()),
            None => (&self.scene, self.sheet.as_ref(), self.sheet_design.as_ref()?),
        };
        let target = targets::resolve(&scene.hits, sheet, at, self.pane.cam.px_per_unit, &Target::None);
        Some((target, design.units.get(self.unit)?))
    }

    /// The path of the body drawn for a pick, including the projected sheet. O(1).
    fn drawn_path(&self, picked: CompRef) -> Option<&str> {
        let design = self.reveal.as_ref().filter(|reveal| reveal.scene.has_part(picked.comp.0))
            .map(|reveal| reveal.design()).or(self.sheet_design.as_ref())?;
        let unit = design.units.get(self.unit).filter(|unit| unit.id == picked.board)?;
        Some(&unit.circuit.comps.get(picked.comp.ix())?.path)
    }

    /// Resolve one drawn part in the current source, by path if indices moved.
    fn picked_node(&self, picked: CompRef) -> Option<u32> {
        if self.dirty { return None; }
        let unit = self.design.as_ref()?.units.get(self.unit).filter(|unit| unit.id == picked.board)?;
        ByPath::new(&unit.circuit).find(picked.comp.ix(), self.drawn_path(picked)?).map(|n| n as u32)
    }

    /// Select the visible bodies wholly inside the released band. O(parts), including a
    /// path index if a newer circuit is waiting for layout. Shift keeps the old pick order.
    fn finish_marquee(&mut self, cx: &mut Cx, rect: [f64; 4], shift: bool) {
        if self.dirty { return; }
        let Some(unit) = self.design.as_ref().and_then(|d| d.units.get(self.unit)) else { return };
        let cam = &self.pane.cam;
        let band = cam.rect_to_screen(rect);
        let items = ExplorerItems { sheet: &self.scene, reveal: self.reveal.as_ref(), cam, now: cx.seconds_since_app_start() };
        let mut selected = if shift { self.selected.clone() } else { Vec::new() };
        let mut seen: std::collections::HashSet<_> = selected.iter().map(|(n, _)| *n).collect();
        let mut paths = ByPath::new(&unit.circuit);
        for item in items.nodes(cam) {
            let Sel::Comp(picked) = item.sel else { continue };
            if !item.rect.is_inside_of(band) { continue; }
            let Some(path) = self.drawn_path(picked) else { continue };
            let Some(n) = paths.find(picked.comp.ix(), path) else { continue };
            if seen.insert(n as u32) { selected.push((n as u32, path.to_owned())); }
        }
        self.select_parts(cx, selected);
    }

    /// Resolve the pointer once per edit. Q arms its tool when the hit misses both pins and wire ends.
    fn key_edit(&mut self, cx: &mut Cx, gesture: impl FnOnce(Target) -> Gesture, arm_on_miss: Option<CircuitTool>) {
        let pointer = self.pane.pointer_world();
        let hit = pointer.and_then(|at| self.edit_target(at));
        if let Some(tool) = arm_on_miss {
            if !matches!(hit, Some((Target::Pin { .. } | Target::WireEnd { .. }, _))) {
                self.set_armed(cx, self.armed.key(tool));
                return;
            }
        }
        if self.busy() || self.scrub.is_some() || self.field_open() || !matches!(self.marquee, Marquee::Idle) { return; }
        let Some(unit) = self.design.as_ref().and_then(|d| d.units.get(self.unit)) else { return };
        let (target, shown) = match hit {
            Some(hit) => hit,
            None if pointer.is_none() => (Target::None, unit),
            None => return,
        };
        if shown.id != unit.id { return; }
        let gesture = gesture(target);
        if matches!(gesture, Gesture::Delete(_)) && self.selected.len() > 1 {
            let parts: Vec<_> = self.selected.iter().map(|(node, _)| CompId(*node)).collect();
            let planned = delete_parts(&unit.circuit, &parts);
            self.send_sheet_plan(cx, planned, pointer.unwrap_or(self.pane.cam.center));
            return;
        }
        if let Target::Pin { comp, .. } | Target::WireEnd { comp, .. } = target {
            let Some(current) = unit.circuit.comps.get(comp.ix()) else { return };
            // The visible id must still name the same component in the latest design.
            if !shown.circuit.comps.get(comp.ix()).is_some_and(|shown| shown.path == current.path) { return; }
        }
        let planned = match gesture {
            Gesture::Delete(target) => delete(&unit.circuit, target, self.selected_node().map(CompId)),
            gesture => plan(&shown.circuit, &gesture),
        };
        self.send_sheet_plan(cx, planned, pointer.unwrap_or(self.pane.cam.center));
    }

    /// Copy the selection in pick order. Only a copy press parses its statements and reads
    /// the displayed group positions. Clipboard replacement waits for every template.
    fn copy_part(&mut self, cx: &mut Cx) {
        let at = self.pane.pointer_world().unwrap_or(self.pane.cam.center);
        let Some(node) = self.selected_node() else { return };
        let Some(unit) = self.design.as_ref().and_then(|d| d.units.get(self.unit)) else { return };
        let Some(snap) = cx.get_global_ref::<ElecLive>().map(|live| &live.snap) else { return };
        let Some(current) = snap.design.unit(unit.id).filter(|u| Arc::ptr_eq(&u.circuit, &unit.circuit)) else { return };
        let Some(part) = current.circuit.comps.get(node as usize) else { return };
        if self.selected.len() > 1 && self.selected.iter().any(|(n, _)| {
            current.circuit.comps.get(*n as usize).is_some_and(|p| p.block != part.block)
        }) {
            self.show_note(cx, "copy parts of one block".into(), at, false);
            return;
        }
        let (Some(map), Some(docs)) = (&snap.source_map, &snap.docs) else { return };
        if docs.units.iter().any(|(id, _, state)| *id == unit.id && *state != fab_elec_lang::UnitState::Built) {
            self.show_note(cx, "fix the source errors before copying this part".into(), at, true);
            return;
        }
        let template = |part: &fab_elec_circuit::Component| fab_elec_lang::part_templates(map, &unit.circuit, part,
            |id| docs.texts.iter().find(|doc| doc.doc == id).map(|doc| doc.text.as_ref()));
        let (copy, chip) = if self.selected.len() == 1 {
            match template(part) {
                Ok(t) => (CopiedPart::new(&unit.circuit, part, t.stmt, t.detached, &t.pins), format!("Copied {}", part.refdes)),
                Err(error) => { self.show_note(cx, error.message, at, true); return; }
            }
        } else {
            let Some(shown) = self.sheet_design.as_ref().and_then(|d| d.unit(unit.id)) else { return };
            let mut paths = ByPath::new(&shown.circuit);
            let mut templates = Vec::with_capacity(self.selected.len());
            for (node, path) in &self.selected {
                let Some(p) = current.circuit.comps.get(*node as usize).filter(|p| &p.path == path) else { return };
                let Some(n) = paths.find(*node as usize, path) else { return };
                let place = self.reveal.as_ref().and_then(|r| r.sheet().parts.get(n)).filter(|p| p.placed)
                    .or_else(|| self.sheet.parts.get(n).filter(|p| p.placed));
                let Some(place) = place else { return };
                let t = match template(p) {
                    Ok(template) => template,
                    Err(error) => { self.show_note(cx, error.message, at, true); return; }
                };
                let Some(source) = map.locate(&Sel::Comp(CompRef { board: unit.id, comp: p.id })) else { return };
                templates.push(fab_elec_explore::plan_paste::CopyTemplate {
                    part: p, stmt: t.stmt, detached: t.detached, pins: t.pins, at: place.at, source_start: source.span.start,
                });
            }
            match CopiedPart::group(&unit.circuit, templates) {
                Ok(copy) => (copy, format!("Copied {} parts", self.selected.len())),
                Err(note) => { self.show_note(cx, note.into(), at, false); return; }
            }
        };
        cx.copy_to_clipboard(&copy.stmt);
        self.copied = Some(copy);
        self.show_note(cx, chip, at, false);
    }

    /// Paste the copied statements in one edit at the pointer. An overlay may have taken the latest hover
    /// while the pane still remembers its preceding world point.
    fn paste_part(&mut self, cx: &mut Cx) {
        let Some(copy) = &self.copied else { return };
        if !cx.fingers.is_area_hovered(self.draw_bg.area()) { return; }
        let Some(at) = self.pane.pointer_world() else { return };
        let Some(shown) = self.sheet_design.as_ref().and_then(|d| d.units.get(self.unit)) else { return };
        let Some(unit) = cx.get_global_ref::<ElecLive>().and_then(|live| live.snap.design.unit(shown.id)) else { return };
        let block = targets::canvas_block(&self.sheet, at);
        match plan_paste(&unit.circuit, &shown.circuit, &self.sheet, copy, block, at) {
            Planned::Edit(edit) => self.paste_kept(cx, edit, at),
            planned => self.send_sheet_plan(cx, planned, at),
        }
    }

    /// Orient the selection from the latest source hints. While layout catches up, check
    /// only the selected parts and their movers against the two snapshots.
    fn orient_selected(&mut self, cx: &mut Cx, alternate: bool, single: fn(&Circuit, &SheetLayout, CompId, bool) -> Planned,
        group: fn(&Circuit, &SheetLayout, &[CompId], bool) -> Planned) {
        if self.armed.tool != CircuitTool::Select || self.busy() || self.pane.pressed()
            || self.carry_armed.is_some() || self.scrub.is_some() || self.field_open() { return; }
        let at = self.pane.pointer_world().unwrap_or(self.pane.cam.center);
        let Some(node) = self.selected_node() else { return };
        let Some(shown) = self.sheet_design.as_ref().and_then(|d| d.units.get(self.unit)) else { return };
        let Some(unit) = self.design.as_ref().and_then(|d| d.units.get(self.unit)) else { return };
        if shown.id != unit.id { return; }
        if !Arc::ptr_eq(&shown.circuit, &unit.circuit) {
            let same_part = |n: u32| shown.circuit.comps.get(n as usize).zip(unit.circuit.comps.get(n as usize))
                .is_some_and(|(shown, current)| shown.path == current.path);
            for &(n, _) in &self.selected {
                let mover = if self.selected.len() > 1 {
                    let Some(head) = fab_elec_explore::plan::group_mover(&self.sheet, CompId(n)) else { return };
                    head.0
                } else {
                    match self.sheet.movers.get(n as usize) {
                        Some(&sheet::Mover::Head(head)) | Some(&sheet::Mover::Rides { host: head, .. }) => head,
                        _ => n,
                    }
                };
                if !same_part(n) || !same_part(mover) { return; }
            }
        }
        let planned = if self.selected.len() == 1 {
            single(&unit.circuit, &self.sheet, CompId(node), alternate)
        } else {
            let parts: Vec<_> = self.selected.iter().map(|(n, _)| CompId(*n)).collect();
            group(&unit.circuit, &self.sheet, &parts, alternate)
        };
        self.send_sheet_plan(cx, planned, at);
    }

    /// Publish this edit once and remember its click or key position for a host refusal.
    fn send_sheet_plan(&mut self, cx: &mut Cx, planned: Planned, at: [f64; 2]) {
        if self.note.is_some() {
            self.clear_note();
            self.draw_bg.redraw(cx);
        }
        let (chip, refused) = match planned {
            Planned::Edit(r) => {
                if r.ops.iter().any(|op| matches!(op, DesignOp::Circuit { op: CircuitOp::Remove(_), .. })) {
                    self.clear_selection(cx);
                    if self.reveal_focus.is_some() { self.leave_reveal(cx, false); }
                }
                let seen = cx.has_global::<ElecLive>().then(|| cx.get_global::<ElecLive>().snap.last_refusal
                    .as_ref().map_or(0, |refusal| refusal.seq)).unwrap_or(0);
                self.key_sent = Some((r.label.clone(), at, seen));
                self.own_edit = Some(self.gen);
                cx.widget_action(self.uid, ViewAction::Ops { ops: r.ops, place: r.place, label: r.label });
                return;
            }
            Planned::Nothing { chip } if !chip.is_empty() => (chip, false),
            Planned::Refused { chip, .. } => (chip, true),
            _ => return,
        };
        self.show_note(cx, chip, at, refused);
    }

    /// Arm a tool (or go back to Select); the strip shows it at the next draw.
    fn set_armed(&mut self, cx: &mut Cx, armed: Armed<CircuitTool>) {
        if self.armed.tool != armed.tool || armed.tool == CircuitTool::Select { self.cancel_click(cx); }
        if self.armed != armed {
            self.marquee = Marquee::Idle;
            log!("fab.circuit: tool {:?}{}", armed.tool, if armed.kept { " (kept)" } else { "" });
            self.armed = armed;
            self.draw_bg.redraw(cx);
        }
    }

    /// What a probe at the pointer would read, as (net, label): a pin names it "<refdes>.<pin>",
    /// as a drop there does; a wire or another hovered net, the net's name.
    fn probe_under(&self, now: f64) -> Option<(String, String)> {
        self.pane.pointer_world().and_then(|at| self.probe_at_world(at, now)).or_else(|| self.pane.hovered().and_then(|sel| match sel {
            Sel::Net(NetRef { net, .. }) => probe_name(self.sheet_design.as_ref()?, self.unit, net.0),
            _ => None,
        }).map(|name| (name.clone(), name)))
    }

    /// The Probe tool's click: a probe on the net under the pointer, to the bench a host names or
    /// the one the electronics host runs, as P does.
    fn probe_here(&mut self, cx: &mut Cx) {
        let now = cx.seconds_since_app_start();
        let Some(bench) = self.bench.clone().or_else(|| host_bench(cx)) else { return };
        if let Some((net, label)) = self.probe_under(now) {
            self.add_probe(cx, bench, net, label);
        }
    }

    fn add_probe(&self, cx: &mut Cx, bench: String, net: String, label: String) {
        cx.widget_action(self.uid, ViewAction::Ops {
            place: vec![],
            label: format!("Probe {}", label.replace('\n', " ")),
            ops: vec![DesignOp::Bench { bench, op: BenchOp::AddProbe { of: ProbeOf::v(net), label } }],
        });
    }

    /// The host's answer to the newest key edit: its refusal, when one with that label came after it,
    /// is shown where the key was pressed. O(1) per signal.
    fn key_refusal(&mut self, cx: &mut Cx) {
        let Some((label, _, seen)) = &self.key_sent else { return };
        if !cx.has_global::<ElecLive>() { return; }
        let Some(refusal) = cx.get_global::<ElecLive>().snap.last_refusal.clone() else { return };
        if refusal.seq <= *seen { return; }
        let matched = refusal.label == *label;
        let Some((_, at, _)) = self.key_sent.take() else { return };
        if matched { self.own_edit = None; self.show_note(cx, refusal.reason.clone(), at, true); }
    }

    /// A chip at world point `at`: a refusal, or why a key changed nothing.
    fn show_note(&mut self, cx: &mut Cx, chip: String, at: [f64; 2], refused: bool) {
        let tokens = Tokens::for_theme(self.dark);
        // A message to the person: never smaller on screen than the gesture's chip (a fitted big
        // sheet drew it as a red stroke). The note goes when the camera moves, so it is sized once.
        let px = 1.0 / self.pane.cam.px_per_unit.max(1e-6);
        let off = (CHIP_PX * px).max(0.6);
        let (at, scale) = if let Some(NameEdit::Net(edit @ NetNameEdit { wire_end: Some(_), .. })) = &self.name_edit {
            // The field is clamped to the pane. Keep the chip beside its actual rectangle, above
            // it when there is room and below it at the top edge.
            let pane = self.pane.cam.view;
            let width = sheet::text_w(&chip, CHIP_PX);
            let x = edit.screen.pos.x.clamp(pane.pos.x + 4.0, (pane.pos.x + pane.size.x - width - 4.0).max(pane.pos.x + 4.0));
            let y = if edit.screen.pos.y - pane.pos.y >= CHIP_PX * 2.0 { edit.screen.pos.y - CHIP_PX }
                else { edit.screen.pos.y + edit.screen.size.y + CHIP_PX };
            (self.pane.cam.to_world(dvec2(x, y)), CHIP_PX * px / sheet::TEXT_VALUE)
        } else {
            ([at[0] + off, at[1] + off], (CHIP_PX * px / sheet::TEXT_VALUE).max(1.0))
        };
        self.note = Some((Text {
            text: chip, at, size: sheet::TEXT_VALUE, scale, hidden: false,
            align: glyphs::Align::Left, vertical: false,
            color: if refused { tokens.violation } else { tokens.ink },
            item: NO_ITEM, owner: None, is_value: false, fade: TextFade::REST,
        }, refused));
        self.rebuild_chip(tokens);
        self.draw_bg.redraw(cx);
    }

    fn start_scrub(&mut self, cx: &mut Cx, key: &Sel, from: [f64; 2], radius: f64, now: f64) -> bool {
        let Some((scene, node)) = self.value_scene_at(from, radius, now) else { return false };
        if scene.part_item(node).and_then(|item| scene.sel_of(item)).as_ref() != Some(key) { return true; }
        let Some(unit) = self.design.as_ref().and_then(|design| design.units.get(self.unit)) else { return true };
        let Some(value) = unit.circuit.comps.get(node as usize).and_then(|comp| comp.value.as_ref()) else { return true };
        let Some(scrub) = Scrub::begin(CompId(node), value) else { return true };
        let Some(mut text) = scene.value_text(node).cloned() else { return true };
        let offset = scene.offset(node, now);
        text.at[0] += offset[0];
        text.at[1] += offset[1];
        text.text = scrub.value().text;
        text.fade = TextFade::REST;
        self.scrub = Some(ValueScrub { scrub, unit: unit.id, from, text, previewed: false });
        self.rebuild_chip(Tokens::for_theme(self.dark));
        self.draw_bg.redraw(cx);
        true
    }

    fn drag_scrub(&mut self, cx: &mut Cx, at: [f64; 2]) -> bool {
        let Some(active) = &mut self.scrub else { return false };
        // Camera2d's scale is logical points/world unit, independent of device DPI.
        let dx = (at[0] - active.from[0]) * self.pane.cam.px_per_unit;
        if !active.scrub.drag(dx) {
            return false;
        }
        let value = active.scrub.value();
        active.text.text.clone_from(&value.text);
        let op = CircuitOp::SetValue { comp: active.scrub.comp, value };
        cx.widget_action(self.uid, ViewAction::Preview {
            notes: vec![], tag: next_tag(), place: vec![],
            ops: vec![DesignOp::Circuit { unit: active.unit, op }],
            label: "Set value".into(),
        });
        active.previewed = true;
        self.rebuild_chip(Tokens::for_theme(self.dark));
        true
    }

    fn end_scrub(&mut self, cx: &mut Cx, commit: bool) {
        let Some(active) = self.scrub.take() else { return };
        if commit && active.scrub.finish().is_some() {
            cx.widget_action(self.uid, ViewAction::CommitPreview);
        } else if active.previewed {
            cx.widget_action(self.uid, ViewAction::CancelPreview);
        }
        self.chip.set(1, Vec::new());
        self.chip_lines.set(1, Vec::new());
        self.chip_arcs.set(1, Vec::new());
        if let Some(layout) = self.pending_sheet.take() {
            self.apply_layout(cx, layout);
        }
        if let Some(revealed) = self.pending_reveal.take() {
            self.apply_reveal(cx, revealed);
        }
    }

    /// A pin-mounted part explains itself on press, before the pane's drag threshold.
    /// Observe the pane's claimed event without capturing or dispatching it a second time.
    fn rides_press(&mut self, cx: &mut Cx, event: &Event, now: f64) {
        if self.busy() || self.wire_click.is_some() || self.scrub.is_some() || self.field_open() || self.armed.tool.names_nets()
            || self.reveal.is_some() || self.reveal_focus.is_some() {
            return;
        }
        let area = self.draw_bg.area();
        let screen = match event {
            Event::MouseDown(e) if e.handled.get() == area && e.button.contains(MouseButton::PRIMARY) => Some(e.abs),
            Event::TouchUpdate(e) if e.touches.len() == 1 => e.touches.iter()
                .find(|t| t.state == TouchState::Start && t.handled.get() == area).map(|t| t.abs),
            _ => None,
        };
        let Some(screen) = screen else { return };
        if cx.keyboard.is_key_down(KeyCode::Space) { return; }
        let at = self.pane.cam.to_world(screen);
        let radius = HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12);
        if self.value_scene_at(at, radius, now).is_some() { return; }
        let Some(node) = self.scene.body_at(at, radius, now) else { return };
        let Some(&sheet::Mover::Rides { host, port }) = self.sheet.movers.get(node as usize) else { return };
        let Some(unit) = self.sheet_design.as_ref().and_then(|d| d.units.get(self.unit)) else { return };
        self.drag.show_rides(node, host, port, &unit.circuit, &self.scene, now);
        self.draw_bg.redraw(cx);
    }

    fn value_cursor(&self, cx: &mut Cx) {
        let cursor = if self.gesture.as_ref().is_some_and(|gesture| gesture.phase().sampling()) {
            MouseCursor::Grabbing
        } else if self.drag.rides() {
            MouseCursor::Default
        } else if self.drag.held() {
            MouseCursor::Grabbing
        } else if self.scrub.is_some() {
            MouseCursor::EwResize
        } else if matches!(cx.mouse_cursor(), MouseCursor::Hand | MouseCursor::Default | MouseCursor::EwResize) {
            let Some(at) = self.pane.pointer_world() else { return };
            let radius = HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12);
            let over_value = self.hover_value_box.is_some_and(|b| {
                at[0] >= b[0] - radius && at[0] <= b[2] + radius
                    && at[1] >= b[1] - radius && at[1] <= b[3] + radius
            });
            if self.hover_wire && !self.busy() { MouseCursor::Hand }
                else if over_value { MouseCursor::EwResize }
                else if self.reveal.is_none() && self.reveal_focus.is_none()
                    && (self.state.hover.and_then(|item| self.scene.part_node(item)).is_some_and(|node| {
                        !matches!(self.sheet.movers.get(node as usize), Some(sheet::Mover::Rides { .. }))
                            && self.scene.body_contains(node, at, radius, cx.seconds_since_app_start())
                    }) || self.state.hover.and_then(|item| self.scene.block_node(item))
                        .is_some_and(|block| self.sheet.block_movable.get(block as usize) == Some(&true))) { MouseCursor::Hand }
                else { MouseCursor::Default }
        } else {
            return;
        };
        if cx.mouse_cursor() != cursor {
            cx.set_cursor(cursor);
        }
    }

    fn input(&mut self, cx: &mut Cx, input: CanvasInput<Sel>) {
        // The click that ends a live press is the press's: it neither selects nor reveals.
        match &input {
            CanvasInput::Click { key, .. } => {
                if self.live_press.takes_click(key.as_ref()) {
                    return;
                }
            }
            CanvasInput::Press { .. } => {}
            _ => self.live_press.other(),
        }
        match input {
            // While the run plays, a pointer on a button presses it live until the pointer comes up.
            CanvasInput::Press { key: Sel::Comp(comp), down: true } => {
                if self.armed.tool.names_nets() || self.gesture.as_ref().is_some_and(|g| g.phase() == Phase::Armed) { return; }
                let live = cx.get_global_ref::<ElecLive>().is_some_and(|live| live.sim.playing && live.sim.index.momentary().contains(&comp));
                if let Some(cmd) = self.live_press.down(comp, live) {
                    cx.action(SimAction::Cmd(cmd));
                    self.draw_bg.redraw(cx);
                }
                return;
            }
            CanvasInput::Press { down: false, .. } => {
                if let Some(cmd) = self.live_press.up() {
                    cx.action(SimAction::Cmd(cmd));
                    self.draw_bg.redraw(cx);
                }
                return;
            }
            CanvasInput::Click { at, .. } if self.drag.click_to_drop() => {
                self.click_pointer(cx, at, true);
                return;
            }
            CanvasInput::Click { at, .. } if self.gesture.as_ref().is_some_and(|g| g.phase() == Phase::Armed) => {
                self.wire_pointer(cx, at, true);
                return;
            }
            // A carried Library entry is placed where the click lands; Shift-click places another.
            // A click before the pointer moved over the sheet begins the carry there.
            CanvasInput::Click { at, shift, .. } if !self.carry_drag
                && (self.carry_armed.is_some() || self.gesture.as_ref().is_some_and(|gesture| gesture.carries())) => {
                if !self.gesture.as_ref().is_some_and(|gesture| gesture.carries()) {
                    let Some(carried) = self.carry_armed.clone() else { return };
                    if !self.begin_carry(cx, carried, at) { return; }
                    self.carry_armed = None;
                }
                self.carry_place(cx, at, shift);
                return;
            }
            CanvasInput::Click { at, .. } if self.armed.tool.names_nets() => {
                self.open_net_name(cx, at, None);
            }
            // Under No-connect or Probe a click is the tool's one gesture, on what is under it.
            CanvasInput::Click { .. } if matches!(self.armed.tool, CircuitTool::NoConnect | CircuitTool::Probe) => {
                if self.field_open() { return; }
                match self.armed.tool {
                    CircuitTool::NoConnect => self.key_edit(cx, Gesture::NoConnect, None),
                    _ => self.probe_here(cx),
                }
                self.set_armed(cx, self.armed.done());
            }
            // A double-click on a part opens the projected view around it (user, 11-18: "possible
            // to open but in a more explicit way"); its first click already selected it.
            CanvasInput::Click { key, clicks: 2, shift, .. } if !shift || self.armed.tool != CircuitTool::Select => {
                if self.busy() || self.scrub.is_some() || self.field_open() { return; }
                self.clear_note();
                if let Some(Sel::Comp(comp)) = key {
                    let Some(node) = self.picked_node(comp) else { return };
                    self.select_part(cx, node, false);
                    self.request_reveal(cx, node);
                }
            }
            CanvasInput::Click { key, shift, .. } => {
                if self.field_open() || self.gesture.is_some() { return; }
                self.clear_note();
                if self.busy() && !self.drag.rides() { return; }
                self.drag.release_rides(cx.seconds_since_app_start());
                // Trail picks and semantic presses carry the same component selection; while a run
                // is made, an output jack clicked is also heard (or no longer heard).
                // A click selects the part in place and opens its card: nothing on the sheet moves.
                if let Some(Sel::Comp(comp)) = key.as_ref() {
                    let toggle = shift && self.armed.tool == CircuitTool::Select;
                    if !toggle { crate::live::listen::click_jack(cx, *comp); }
                    if let Some(node) = self.picked_node(*comp) { self.select_part(cx, node, toggle); }
                } else if let Some(&Sel::Net(net)) = key.as_ref() {
                    // A wire or a pin clicked lights its whole net in every view, as a part
                    // clicked focuses the part (the Board lights it, the Scope's selection lane
                    // follows it).
                    self.set_selection(cx, Vec::new());
                    cx.widget_action(self.uid, ViewAction::Focus(Focus { kind: FocusKind::Net, sel: Sel::Net(net), origin: FocusOrigin::Explorer }));
                    self.click_net(cx, net);
                } else if key.is_none() && (!shift || self.armed.tool != CircuitTool::Select) {
                    self.clear_selection(cx);
                    self.leave_reveal(cx, false);
                }
            }
            CanvasInput::Escape => {
                self.wire_press = None;
                if self.gesture.is_none() && self.carry_armed.take().is_some() {
                    self.carry_again = false;
                    return;
                }
                if self.escape_gesture(cx) { return; }
                if self.hold.is_some() { return; }
                self.clear_note();
                if self.drag.busy() {
                    let now = cx.seconds_since_app_start();
                    self.drag.cancel(cx, self.uid, now);
                } else if self.selected.len() > 1 {
                    self.clear_selection(cx);
                } else if self.armed.armed() {
                    self.set_armed(cx, self.armed.escape());
                } else if self.reveal.is_none() && self.reveal_focus.is_none() && !self.selected.is_empty() {
                    self.clear_selection(cx);
                } else { self.leave_reveal(cx, true); }
            }
            CanvasInput::Key { code: KeyCode::Tab, modifiers, .. }
                if !modifiers.control && !modifiers.alt && !modifiers.logo => {
                if let Some(gesture) = self.gesture.as_mut().filter(|gesture| gesture.phase() == Phase::Dragging) {
                    let step = gesture.tab(modifiers.shift);
                    self.wire_step(cx, step);
                }
            }
            // With a part selected, Enter opens its value in the card (a typed digit does too, as
            // text: `typed_value`).
            CanvasInput::Key { code: KeyCode::ReturnKey | KeyCode::NumpadEnter, modifiers, .. }
                if self.selected.len() == 1 && !modifiers.control && !modifiers.alt && !modifiers.logo => {
                self.open_card_field(cx, None);
            }
            CanvasInput::Key { code: code @ (KeyCode::KeyX | KeyCode::KeyY), modifiers, .. }
                if !modifiers.shift && !modifiers.control && !modifiers.alt && !modifiers.logo => {
                let vertical = code == KeyCode::KeyY;
                if let Some(gesture) = self.gesture.as_mut().filter(|gesture| gesture.carries()) {
                    let step = gesture.mirror(vertical);
                    self.wire_step(cx, step);
                } else if self.drag.held() {
                    let now = cx.seconds_since_app_start();
                    self.drag.mirror(cx, self.uid, vertical, &self.sheet, now);
                } else {
                    self.orient_selected(cx, vertical, mirror_part, fab_elec_explore::plan::mirror_parts);
                }
            }
            CanvasInput::Key { code: KeyCode::KeyG | KeyCode::KeyM, modifiers, .. }
                if !modifiers.shift && !modifiers.control && !modifiers.alt && !modifiers.logo => {
                self.key_move(cx);
            }
            CanvasInput::Key { code: code @ (KeyCode::KeyC | KeyCode::KeyV), modifiers, .. }
                if modifiers.is_primary() && !modifiers.alt && !modifiers.shift && self.armed.tool == CircuitTool::Select => {
                if self.field_open() || self.busy()
                    || self.pane.pressed() || self.scrub.is_some() || self.carry_armed.is_some() { return; }
                if (self.reveal.is_some() || self.reveal_focus.is_some()) && !(code == KeyCode::KeyC && self.selected.len() > 1) { return; }
                if code == KeyCode::KeyC { self.copy_part(cx); } else { self.paste_part(cx); }
            }
            // The tools' keys arm them; pressed again while armed, they keep them.
            CanvasInput::Key { code: code @ (KeyCode::KeyV | KeyCode::KeyW | KeyCode::KeyL | KeyCode::KeyB), modifiers, .. }
                if (!modifiers.shift || code == KeyCode::KeyL) && !modifiers.control && !modifiers.alt && !modifiers.logo => {
                let tool = match code {
                    KeyCode::KeyW => CircuitTool::Wire,
                    KeyCode::KeyL if modifiers.shift => CircuitTool::Global,
                    KeyCode::KeyL => CircuitTool::Label,
                    KeyCode::KeyB => CircuitTool::Probe,
                    _ => CircuitTool::Select,
                };
                self.set_armed(cx, self.armed.key(tool));
            }
            CanvasInput::Key { code: KeyCode::KeyQ, modifiers, .. }
                if !modifiers.shift && !modifiers.control && !modifiers.alt && !modifiers.logo => {
                self.key_edit(cx, Gesture::NoConnect, Some(CircuitTool::NoConnect));
            }
            CanvasInput::Key { code: KeyCode::Delete | KeyCode::Backspace, modifiers, .. }
                if !modifiers.shift && !modifiers.control && !modifiers.alt && !modifiers.logo => {
                self.key_edit(cx, Gesture::Delete, None);
            }
            CanvasInput::Key { code: KeyCode::KeyU, modifiers, under: Some(Sel::Comp(comp)) } => {
                if self.busy() || self.scrub.is_some() || self.reveal.is_some() || self.reveal_focus.is_some() { return; }
                let Some(unit) = self.sheet_design.as_ref().and_then(|d| d.units.get(self.unit)) else { return };
                if comp.board != unit.id { return; }
                let node = match self.sheet.movers.get(comp.comp.0 as usize).copied().unwrap_or_default() {
                    sheet::Mover::Itself => comp.comp.0,
                    sheet::Mover::Head(head) => head,
                    sheet::Mover::Rides { .. } => return,
                };
                let circuit = &unit.circuit;
                let ops = unpin_ops(circuit, unit.id, node, modifiers.shift);
                if ops.is_empty() { return; }
                let comp = &circuit.comps[node as usize];
                let name = if modifiers.shift {
                    let path = circuit.tree.get(comp.block.0 as usize).map_or("", |b| b.path.as_str());
                    if path.is_empty() { circuit.name.as_str() } else { path.rsplit('.').next().unwrap_or(path) }
                } else { comp.refdes.as_str() };
                cx.widget_action(self.uid, ViewAction::Ops { place: vec![], ops, label: format!("Auto-place {name}") });
            }
            CanvasInput::Key { code: KeyCode::KeyU, modifiers, under: Some(Sel::Block(board, block)) } => {
                if self.busy() || self.scrub.is_some() || self.reveal.is_some() || self.reveal_focus.is_some() { return; }
                let Some(unit) = self.sheet_design.as_ref().and_then(|d| d.units.get(self.unit)) else { return };
                if board != unit.id { return; }
                let circuit = &unit.circuit;
                let ops = super::drag::unpin_block_ops(circuit, unit.id, block.0, modifiers.shift);
                if ops.is_empty() { return; }
                let path = &circuit.tree[block.0 as usize].path;
                let name = path.rsplit('.').next().unwrap_or(path);
                let label = if modifiers.shift { format!("Auto-place all in {name}") } else { format!("Auto-place {name}") };
                cx.widget_action(self.uid, ViewAction::Ops { place: vec![], ops, label });
            }
            CanvasInput::Key { code: KeyCode::KeyR, modifiers, .. } => {
                if let Some(gesture) = self.gesture.as_mut().filter(|gesture| gesture.carries()) {
                    let step = gesture.turn(modifiers.shift);
                    self.wire_step(cx, step);
                    return;
                }
                if self.drag.held() {
                    let now = cx.seconds_since_app_start();
                    self.drag.turn(cx, self.uid, modifiers.shift, &self.sheet, now);
                    return;
                }
                if !modifiers.control && !modifiers.alt && !modifiers.logo {
                    self.orient_selected(cx, modifiers.shift, turn_part, fab_elec_explore::plan::turn_parts);
                }
            }
            CanvasInput::Key { code: KeyCode::KeyT, .. } => self.set_dark(cx, !self.dark),
            // Power (P): the flyout of the circuit's rails beside the strip's Power button.
            CanvasInput::Key { code: KeyCode::KeyP, modifiers, .. }
                if !modifiers.shift && !modifiers.control && !modifiers.alt && !modifiers.logo => self.open_power(cx),
            // Place part (A): the Library opens and its search takes the keyboard. Shift-A replays
            // the sheet's assembly.
            CanvasInput::Key { code: KeyCode::KeyA, modifiers, .. } if !modifiers.control && !modifiers.alt && !modifiers.logo => {
                if modifiers.shift { self.replay_assemble(cx) } else { self.open_library_search(cx) }
            }
            CanvasInput::Key { code: KeyCode::KeyF, .. } => self.fit(cx),
            CanvasInput::Hover(sel) => {
                self.state.hover = sel.as_ref().and_then(|sel| self.scene.item_of(sel));
                self.hover_value_box = self.state.hover.and_then(|item| self.value_box(item, cx.seconds_since_app_start()));
                let now = cx.seconds_since_app_start();
                let target = if !self.busy() && self.reveal.is_none() && self.reveal_focus.is_none() {
                    self.sheet_design.as_ref().and_then(|d| d.units.get(self.unit)).and_then(|unit| {
                        match sel.as_ref()? {
                            Sel::Comp(comp) => {
                                let part = unit.circuit.comps.get(comp.comp.0 as usize)?;
                                if comp.board != unit.id || !super::scene::pinned(&unit.circuit, &part.path) { return None; }
                                let body = self.scene.part_box(comp.comp.0)?;
                                let offset = self.scene.offset(comp.comp.0, now);
                                Some([body[2] + offset[0] + 1.0, body[3] + offset[1] + 1.0])
                            }
                            Sel::Block(board, b) => {
                                if *board != unit.id || b.0 == 0 { return None; }
                                let frame = self.sheet.frames.get(b.0 as usize).filter(|frame| frame.placed)?;
                                let block = unit.circuit.tree.get(b.0 as usize)?;
                                if !super::scene::pinned(&unit.circuit, &block.path) { return None; }
                                let rect = frame.rect.map(f64::from);
                                Some([rect[2] + 1.0, rect[3] + 1.0])
                            }
                            _ => None,
                        }
                    })
                } else { None };
                match target {
                    Some(anchor) => self.drag.show_pinned(anchor, now),
                    None => self.drag.release_pinned(now),
                }
            }
            CanvasInput::Marquee { rect, done, shift } if !matches!(self.marquee, Marquee::Idle) => {
                if matches!(self.marquee, Marquee::Pressed) { self.clear_note(); }
                if done {
                    self.marquee = Marquee::Idle;
                    self.finish_marquee(cx, rect, shift);
                } else {
                    self.marquee = Marquee::Drawing(rect);
                }
            }
            CanvasInput::DragStart { key, from, at } => {
                if self.holding() {
                    if let Some(gesture) = &mut self.gesture { gesture.defer(from); }
                    self.wire_press = Some((from, at));
                    return;
                }
                if self.gesture.is_some() || self.carry_armed.is_some() { return; }
                if self.field_open() { return; }
                self.clear_note();
                // An armed tool takes the drag: Wire only from a pin or a wire's end, the click
                // tools none (no part moves under them).
                match self.armed.tool {
                    CircuitTool::Select => {}
                    CircuitTool::Wire => {
                        if !self.busy() { self.begin_wire(cx, from, at); }
                        return;
                    }
                    CircuitTool::Label | CircuitTool::Global | CircuitTool::NoConnect | CircuitTool::Probe => return,
                }
                let radius = HIT_RADIUS / self.pane.cam.px_per_unit.max(1e-12);
                let screen = self.pane.cam.to_screen(from);
                if self.reveal.as_ref().is_some_and(|reveal| reveal.trail_hit([screen.x, screen.y]).is_some()) { return; }
                let now = cx.seconds_since_app_start();
                let node = self.scene.item_of(&key).and_then(|item| self.scene.part_node(item));
                if self.drag.rides() && self.selected.len() > 1 && node.is_some_and(|node| self.selected.iter().any(|(n, _)| *n == node)) {
                    self.drag.release_rides(now);
                }
                if self.busy() || self.start_scrub(cx, &key, from, radius, now) { return; }
                if self.begin_wire(cx, from, at) { return; }
                if let Some(node) = node {
                    if self.reveal.is_some() || self.reveal_focus.is_some() {
                        self.drag.inside_reveal();
                        return;
                    }
                    if !self.scene.body_contains(node, from, radius, now) { return; }
                    self.begin_part_drag(node, from, false, now);
                } else if let Sel::Block(board, block) = key {
                    let (Some(unit), Some(templates)) = (self.sheet_design.as_ref().and_then(|d| d.units.get(self.unit)), &self.templates) else { return };
                    if board != unit.id { return; }
                    if self.reveal.is_some() || self.reveal_focus.is_some() {
                        self.drag.inside_reveal();
                        return;
                    }
                    if self.drag.begin_block(block.0, from, &unit.circuit, unit.id, &self.sheet, &mut self.scene,
                        templates, Tokens::for_theme(self.dark), now)
                    {
                        self.drag_pin_item = self.scene.item_of(&Sel::Block(board, block));
                        self.publish_connections();
                    }
                } else { return; }
            }
            CanvasInput::DragMove { at, .. } => {
                if let Some((_, latest)) = &mut self.wire_press { *latest = at; }
                if self.gesture.is_some() {
                    self.wire_pointer(cx, at, false);
                    return;
                }
                if self.wire_press.is_some() { self.resume_wire(cx); return; }
                if self.scrub.is_some() {
                    if !self.drag_scrub(cx, at) { return; }
                } else {
                    let now = cx.seconds_since_app_start();
                    self.drag.sample(cx, self.uid, at, &self.sheet, now);
                }
            }
            CanvasInput::DragEnd { at, .. } => {
                self.wire_press = None;
                if self.click_wire() { return; }
                if self.gesture.is_some() {
                    self.wire_pointer(cx, at, true);
                    return;
                }
                if self.scrub.is_some() {
                    self.drag_scrub(cx, at);
                    self.end_scrub(cx, true);
                } else {
                    let now = cx.seconds_since_app_start();
                    self.finish_drag(cx, at, now);
                }
            }
            CanvasInput::DragCancel { .. } => {
                self.wire_press = None;
                if self.escape_gesture(cx) { return; }
                if self.scrub.is_some() { self.end_scrub(cx, false); }
                else {
                    let now = cx.seconds_since_app_start();
                    self.drag.cancel(cx, self.uid, now);
                }
            }
            CanvasInput::Camera => {}
            _ => return,
        }
        self.pane.animate(cx);
        self.draw_bg.redraw(cx);
    }

    /// Wire samples rebuild the chip, rubber line and frozen target ring, never the scene.
    /// Other chips republish only on an opening, a new step, a key or theme change.
    fn rebuild_chip(&mut self, tokens: &Tokens) {
        let Some(templates) = &self.templates else { return };
        self.chip_arcs.set(templates.arc.slots(), Vec::new());
        // The gesture's chip rides the pointer at one screen size at every zoom (world units per
        // logical pixel); the scrub's label belongs to its sheet item and stays in it; the note
        // is at least chip-sized (`show_note`).
        let px = 1.0 / self.pane.cam.px_per_unit.max(1e-6);
        let mut pad = 0.3;
        let (text, chevrons, look) = if let Some(gesture) = &self.gesture {
            let Some((chip, look)) = gesture.chip() else { return };
            let color = match look {
                ChipLook::Plain => tokens.ink, ChipLook::Edit => tokens.selection, ChipLook::Refused => tokens.violation,
            };
            let text = self.gesture_text.get_or_insert_with(|| Text {
                text: String::new(), at: [0.0; 2], size: sheet::TEXT_VALUE,
                scale: 1.0, hidden: false,
                align: glyphs::Align::Left, vertical: false, color, item: NO_ITEM,
                owner: None, is_value: false, fade: TextFade::REST,
            });
            if text.text != chip { text.text.clear(); text.text.push_str(chip); }
            let end = gesture.end_point();
            text.scale = CHIP_PX * px / sheet::TEXT_VALUE;
            text.at = [end[0] + CHIP_PX * px, end[1] + CHIP_PX * px];
            text.color = color;
            pad = 4.0 * px;
            let look = match look { ChipLook::Plain => Look::Committed, ChipLook::Edit => Look::Preview, ChipLook::Refused => Look::Violation };
            (text, false, look)
        } else if let Some(active) = &mut self.scrub {
            active.text.color = tokens.selection;
            (&mut active.text, true, Look::Preview)
        } else if let Some((text, refused)) = &mut self.note {
            text.color = if *refused { tokens.violation } else { tokens.ink };
            pad = pad.max(4.0 * px);
            (text, false, if *refused { Look::Violation } else { Look::Committed })
        } else { return };
        let size = text.size * text.scale;
        let width = sheet::text_w(&text.text, size);
        let left = text.at[0] - pad;
        let right = text.at[0] + width + pad;
        let y = text.at[1];
        let c = Common {
            item: if text.item == NO_ITEM { -1.0 } else { text.item as f32 },
            look: look.code(),
            ..Common::at([0.0, 0.0, 0.0], text.color)
        };
        let mut boxes = Vec::with_capacity(templates.boxes.slots());
        templates.boxes.append(&mut boxes, &BoxInst {
            c,
            rect: [((left + right) * 0.5) as f32, y as f32,
                ((right - left) * 0.5) as f32, (size * 0.5 + pad) as f32],
            corner: (pad * 7.0 / 6.0) as f32,
            stroke: [1.0, 0.0],
            fill: tokens.part_fill,
        });
        if !chevrons {
            self.chip.set(templates.boxes.slots(), boxes);
            let mut lines = Vec::new();
            if let Some(gesture) = &self.gesture {
                let from = gesture.from_point();
                let end = gesture.end_point();
                if !gesture.carries() {
                    templates.line.append(&mut lines, &LineInst {
                        c, seg: [from[0] as f32, from[1] as f32, end[0] as f32, end[1] as f32],
                        width: [1.0, 0.0], ..LineInst::default()
                    });
                }
                if let Some(at) = gesture.aim_point().filter(|_| gesture.phase().sampling()) {
                    let mut arcs = Vec::with_capacity(templates.arc.slots());
                    templates.arc.append(&mut arcs, &ArcInst {
                        c, circle: [at[0] as f32, at[1] as f32, 0.45, 0.0],
                        sweep: [0.0, std::f32::consts::TAU], stroke: [1.5, 0.0],
                        ..ArcInst::default()
                    });
                    self.chip_arcs.set(templates.arc.slots(), arcs);
                }
            }
            self.chip_lines.set(templates.line.slots(), lines);
            return;
        }
        let mut lines = Vec::with_capacity(4 * templates.line.slots());
        let d = 0.4 * std::f64::consts::FRAC_1_SQRT_2;
        for (edge, direction) in [(left - 0.3, -1.0), (right + 0.3, 1.0)] {
            for dy in [-d, d] {
                templates.line.append(&mut lines, &LineInst {
                    c,
                    seg: [(edge + direction * d) as f32, y as f32, edge as f32, (y + dy) as f32],
                    width: [1.0, 0.0],
                    ..LineInst::default()
                });
            }
        }
        self.chip.set(templates.boxes.slots(), boxes);
        self.chip_lines.set(templates.line.slots(), lines);
    }

    /// Publish following connections, pin copies and part underlays at drag boundaries.
    fn publish_connections(&mut self) {
        let Some(templates) = &self.templates else { return };
        let mut lines = Vec::with_capacity(self.scene.lines.len() * templates.line.slots());
        for row in &self.scene.lines { templates.line.append(&mut lines, row); }
        self.lines.set(templates.line.slots(), lines);
        let mut minis = Vec::with_capacity(self.scene.minis.len() * templates.mini.slots());
        for row in &self.scene.minis { templates.mini.append(&mut minis, row); }
        self.minis.set(templates.mini.slots(), minis);
        self.publish_drag_pins();
        self.publish_underlay();
    }

    /// Publish backgrounds and glows at rest or attached to the current grip. The
    /// drag's foreground copies can then draw without covering the moving parts' light.
    fn publish_underlay(&mut self) {
        let Some(templates) = &self.templates else { return };
        let grip = self.drag_pin_item.filter(|_| self.drag.busy() && !self.drag.rides());
        let block = grip.and_then(|item| self.scene.block_node(item));
        let mut fills = Vec::with_capacity(self.frame_rows.len() * templates.boxes.slots());
        let tree = self.sheet_design.as_ref().and_then(|d| d.units.get(self.unit)).map(|u| &u.circuit.tree);
        for frame in &self.frame_rows {
            let mut frame = *frame;
            if let (Some(grip), Some(root), Some(tree), Some(node)) =
                (grip, block, tree, self.scene.block_node(frame.c.item as u32)) {
                let parents = std::iter::successors(Some(node), |&n| tree.get(n as usize)?.parent.map(|p| p.0));
                if frame.c.item >= 0.0 && parents.take(tree.len()).any(|n| n == root) {
                    // Leave the original ghost at rest, like the dragged frame's border.
                    let mut ghost = frame;
                    ghost.c.item = -1.0;
                    ghost.c.look = Look::Ghost.code();
                    templates.boxes.append(&mut fills, &ghost);
                    frame.c.item = grip as f32;
                    frame.c.look = Look::Preview.code();
                }
            }
            templates.boxes.append(&mut fills, &frame);
        }
        self.frame_fills.set(templates.boxes.slots(), fills);
        let mut rows = Vec::with_capacity(self.glow_rows.len() * templates.boxes.slots());
        for glow in &self.glow_rows {
            let mut glow = *glow;
            if glow.c.item >= 0.0 && self.scene.part_node(glow.c.item as u32).is_some_and(|n| self.drag.member(n)) {
                glow.c.item = self.drag_pin_item.map_or(glow.c.item, |item| item as f32);
                glow.c.pose_a = [0.0, 0.0, 0.0, 1.0];
                glow.c.pose_b = glow.c.pose_a;
                glow.c.span = [0.0; 2];
            }
            templates.boxes.append(&mut rows, &glow);
        }
        self.glows.set(templates.boxes.slots(), rows);
    }

    fn publish_drag_pins(&mut self) {
        let Some(templates) = &self.templates else { return };
        let mut pins = Vec::new();
        for pin in &self.scene.pins {
            if pin.c.item >= 0.0 && self.scene.part_node(pin.c.item as u32).is_some_and(|node| self.drag.member(node)) {
                // Like the drag's glyph copy: resting pose, preview state, offset supplied at draw.
                let c = Common {
                    pose_a: [0.0, 0.0, 0.0, 1.0], pose_b: [0.0, 0.0, 0.0, 1.0], span: [0.0; 2],
                    item: self.drag_pin_item.map_or(pin.c.item, |item| item as f32), look: Look::Preview.code(), ..pin.c
                };
                templates.pin.append(&mut pins, &PinInst { c, ..*pin });
            }
        }
        self.drag_pins.set(templates.pin.slots(), pins);
    }

    /// Only design, static live indices, theme and assembly endpoints publish instance rows.
    fn rebuild(&mut self, tokens: &Tokens) {
        self.press_from = None;
        let Some(templates) = &self.templates else { return };
        if self.ghost_take && self.gesture.is_some() {
            self.ghost_src = Some(std::mem::take(&mut self.scene));
        }
        self.ghost_take = false;
        self.scene = match &self.sheet_design {
            Some(design) => Scene::build(design, self.unit, &self.sheet, &self.sheet_rows, tokens, &self.state, &self.scene_live),
            None => Scene::default(),
        };
        self.erc_generation = None;
        self.legible = 1.0;
        self.legible_pending = true;
        self.probe_lit = None;
        if self.scrub.is_some() {
            if let Some(design) = &self.design {
                self.scene.update_values(design, self.unit);
            }
        }
        // Keep reveal sheets' ordered box stream intact. On the main sheet the glow gets
        // its own retained list, below even the gesture's frozen geometry.
        let grounds = self.scene.glow_range.start;
        self.glow_rows = self.scene.boxes.drain(self.scene.glow_range.clone()).collect();
        self.frame_rows = self.scene.boxes.drain(..grounds).collect();
        self.scene.glow_range = 0..0;
        self.drag.rebuild(&mut self.scene, templates, tokens);
        let mut lines = Vec::with_capacity(self.scene.lines.len() * templates.line.slots());
        let mut boxes = Vec::with_capacity(self.scene.boxes.len() * templates.boxes.slots());
        let mut arcs = Vec::with_capacity(self.scene.arcs.len() * templates.arc.slots());
        let mut bridges = Vec::with_capacity(self.scene.bridges.len() * templates.bridge.slots());
        let mut leaders = Vec::with_capacity(self.scene.leaders.len() * templates.line.slots());
        let mut minis = Vec::with_capacity(self.scene.minis.len() * templates.mini.slots());
        let mut pins = Vec::with_capacity(self.scene.pins.len() * templates.pin.slots());
        for row in &self.scene.lines {
            templates.line.append(&mut lines, row);
        }
        for row in &self.scene.boxes { templates.boxes.append(&mut boxes, row); }
        for row in &self.scene.arcs {
            templates.arc.append(&mut arcs, row);
        }
        for row in &self.scene.bridges { templates.bridge.append(&mut bridges, row); }
        for row in &self.scene.leaders {
            templates.line.append(&mut leaders, row);
        }
        for row in &self.scene.minis { templates.mini.append(&mut minis, row); }
        for row in &self.scene.pins { templates.pin.append(&mut pins, row); }
        self.lines.set(templates.line.slots(), lines);
        self.boxes.set(templates.boxes.slots(), boxes);
        self.arcs.set(templates.arc.slots(), arcs);
        self.bridges.set(templates.bridge.slots(), bridges);
        self.leaders.set(templates.line.slots(), leaders);
        self.minis.set(templates.mini.slots(), minis);
        self.pins.set(templates.pin.slots(), pins);
        self.publish_drag_pins();
        self.publish_underlay();
        self.hover_value_box = self.state.hover.and_then(|item| self.scene.part_node(item))
            .and_then(|node| self.scene.value_box(node));
        let templates = self.templates.as_ref().expect("rebuild captured its templates");
        let mut boxes = Vec::new();
        let mut lines = Vec::new();
        let mut arcs = Vec::new();
        let mut pins = Vec::new();
        if let (Some(gesture), Some(source), Some(design)) = (&self.gesture, &self.ghost_src, &self.sheet_design) {
            let frozen = gesture.frozen();
            if let Some(shown) = design.unit(frozen.design.units[frozen.unit].id) {
                let moved = moved_parts(&frozen.circuit, &frozen.sheet, &shown.circuit, &self.sheet);
                let rows = source.ghost_rows(&moved);
                for row in &rows.boxes { templates.boxes.append(&mut boxes, row); }
                for row in &rows.lines { templates.line.append(&mut lines, row); }
                for row in &rows.arcs { templates.arc.append(&mut arcs, row); }
                for row in &rows.pins { templates.pin.append(&mut pins, row); }
            }
        }
        self.ghost_boxes.set(templates.boxes.slots(), boxes);
        self.ghost_lines.set(templates.line.slots(), lines);
        self.ghost_arcs.set(templates.arc.slots(), arcs);
        self.ghost_pins.set(templates.pin.slots(), pins);
        self.publish_name_lines();
        self.dirty = false;
    }

    fn texts(&mut self, cx: &mut Cx2d, now: f64, overlay: bool) {
        if !overlay {
            let raw = TEXT_FLOOR_POINTS / text_points(sheet::TEXT_REFDES, self.pane.cam.px_per_unit);
            // Steps of 2^(1/8) (about 9%), rounded up: a zoom always gets the same labels whatever
            // the way there (a fit's flight relabels on its way), never under the floor, and a
            // continuous zoom relabels once a step.
            let want = if raw <= 1.0 || raw > FLOOR_MAX_SCALE { 1.0 } else { ((raw.log2() * 8.0).ceil() / 8.0).exp2() };
            if self.legible_pending || want != self.legible {
                let start = Cx::monotonic_now();
                let labels = legible_labels(&self.sheet, want);
                self.scene.apply_legible(&labels, want);
                self.legible = want;
                if self.legible_pending {
                    let ms = (Cx::monotonic_now() - start) * 1e3;
                    log!("fab.explorer: legible labels at {want:.2}x: {ms:.2} ms");
                    self.legible_pending = false;
                }
                self.hover_value_box = self.state.hover.and_then(|item| self.value_box(item, now));
            }
        }
        let scene = if overlay {
            self.reveal.as_ref().map(|r| &r.scene)
        } else { Some(&self.scene) };
        let layer_alpha = if overlay { 1.0 } else {
            self.reveal.as_ref().map_or(1.0, |r| r.sheet_alpha(now))
        };
        let scrub_item = self.scrub.as_ref().map(|active| active.text.item);
        // Two passes over the same texts: the horizontal ones, then the vertical ones turned up in
        // their own list. O(texts) each.
        let turn = UpTurn::of(self.pane.cam.view);
        let list = &mut self.up_lists[overlay as usize];
        for up in [None, Some(turn)] {
            match up {
                None => cx.push_clip_rect(self.pane.cam.view),
                Some(turn) => turn.begin(cx, list),
            }
            if let Some(scene) = scene {
                for text in scene.texts.iter().filter(|text| !text.is_value || Some(text.item) != scrub_item) {
                    let offset = text.owner.map_or([0.0; 2], |node| scene.offset(node, now));
                    let ghost = if !overlay && text.owner.is_some_and(|node| self.drag.member(node)) { 0.33 } else { 1.0 };
                    draw_text(cx, &mut self.draw_text, &self.pane.cam, text,
                        [text.at[0] + offset[0], text.at[1] + offset[1]], text.alpha(now) * layer_alpha * ghost, up);
                }
            }
            if overlay {
                if let Some(text) = self.gesture_text.as_ref().or(self.scrub.as_ref().map(|active| &active.text))
                    .or(self.note.as_ref().map(|(text, _)| text)) {
                    draw_text(cx, &mut self.draw_text, &self.pane.cam, text, text.at, 1.0, up);
                }
            }
            match up {
                None => cx.pop_clip_rect(),
                Some(turn) => turn.end(cx, list),
            }
        }
    }
}

impl Widget for ExplorerView {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Signal) {
            self.key_refusal(cx);
            self.card_refusal(cx);
            self.wire_snapshot(cx);
            self.keep_snapshot(cx);
        }
        // Unfed, the view follows the electronics host: its results arrive with signals.
        if matches!(event, Event::Signal) || self.design.is_none() {
            self.follow(cx);
            if !self.holding() && self.scrub.is_none() && !self.field_holds_sheet() {
                if let Some(laid) = self.pending_sheet.take() { self.apply_layout(cx, laid); }
            }
        }
        if let Event::Actions(actions) = event {
            self.follow_focus(cx, actions);
            self.take_focus(cx, actions);
        }
        // The value's popup is above the card, strips and sheet.
        if self.value_combo_event(cx, event, scope) { return; }
        // The tool strip is over the sheet: its presses, hovers and scrolls are its own.
        let actions = cx.capture_actions(|cx| self.tools.handle_event(cx, event, scope));
        for tool in strip_presses(cx, &self.tools, &actions) {
            if self.name_edit.is_some() { self.close_value_edit(cx); }
            let double = self.tool_clicks.click(tool, cx.seconds_since_app_start());
            self.set_armed(cx, self.armed.click(tool, double));
        }
        if place_pressed(cx, &self.tools, &actions) {
            self.open_library_search(cx);
        }
        if power_pressed(cx, &self.tools, &actions) {
            self.open_power(cx);
        }
        if annotate_pressed(cx, &self.tools, &actions) {
            self.annotate_now(cx);
        }
        // the strip's Check button: the host's Checks editor lists the findings
        if check_pressed(cx, &self.tools, &actions) {
            cx.action(ElecPanel::Checks);
        }
        // The strip's other buttons (undo and redo) are the host's: their actions go on to it.
        cx.extend_actions(actions);
        if over_strip(cx, &self.tools, STRIP_PARTS, event, self.pane.pressed()) {
            return;
        }
        // The Library float: its fold, open and swap buttons are the pane's; its rows' actions
        // go on to the host, and come back here as Arm and drags.
        if self.library.visible() {
            let actions = cx.capture_actions(|cx| self.library.handle_event(cx, event, scope));
            let fold = self.library.button(cx, ids!(lib_fold)).clicked(&actions);
            if fold || self.library.button(cx, ids!(lib_unfold)).clicked(&actions) {
                self.choose_library_fold(cx, fold);
            }
            if self.library.button(cx, ids!(lib_side)).clicked(&actions) {
                self.choose_library_place(cx, !self.library_left, self.library_folded);
            }
            cx.extend_actions(actions);
            if over_strip(cx, &self.library, LIBRARY_PARTS, event, self.pane.pressed()) {
                return;
            }
        }
        // The status bar is along the sheet's bottom: its zoom buttons, and its pointer events.
        let actions = cx.capture_actions(|cx| self.status.handle_event(cx, event, scope));
        for press in bar_presses(cx, &self.status, &actions) {
            match press {
                BarPress::Zoom(factor) => {
                    self.pane.zoom_by(factor, cx.seconds_since_app_start());
                    self.pane.animate(cx);
                    self.draw_bg.redraw(cx);
                }
                BarPress::Fit => self.fit(cx),
            }
        }
        if over_strip(cx, &self.status, BAR_PARTS, event, self.pane.pressed()) {
            return;
        }
        // A click on a row of the Checks editor: the sheet goes to its finding (the other views
        // answer the same focus their own way). O(actions).
        if let Event::Actions(actions) = event {
            let picked: Vec<Sel> = actions.iter()
                .filter_map(|a| a.as_widget_action().and_then(|w| w.action.downcast_ref::<ViewAction>()).or_else(|| a.downcast_ref::<ViewAction>()))
                .filter_map(|a| match a { ViewAction::Focus(focus) if focus.origin == FocusOrigin::Checks => Some(focus.sel.clone()), _ => None })
                .collect();
            for sel in picked {
                self.go_to(cx, &sel);
            }
        }
        let editing = self.name_edit.is_some();
        let input = self.value_field.area();
        let focused = !self.value_focus_pending && !input.is_empty() && cx.has_key_focus(input);
        if editing && !focused && queue_field_key(&mut self.name_keys, event) { return; }
        let gained = matches!(event, Event::KeyFocus(e) if !input.is_empty() && e.focus == input);
        let lost = matches!(event, Event::KeyFocus(e) if !input.is_empty() && e.prev == input);
        if editing && gained { self.dispatch_name_field(cx, event, scope); }
        if editing && focused && !self.name_keys.is_empty() {
            for key in std::mem::take(&mut self.name_keys) {
                if self.name_edit.is_none() { break; }
                self.dispatch_name_field(cx, &key, scope);
            }
        }
        // The hidden input still takes its last focus loss to stop its caret timer.
        if !gained && (self.name_edit.is_some() || lost) { self.dispatch_name_field(cx, event, scope); }
        // In particular, Enter/Escape must not fall through after they close the field.
        // P is handled before the pane too, so all explorer key handling stays below this gate.
        if editing && matches!(event, Event::KeyDown(_) | Event::KeyUp(_) | Event::TextInput(_)) {
            return;
        }
        // A Library entry carried over the sheet: dragged from the panel, or armed by a click on it.
        if self.carry_event(cx, event) {
            return;
        }
        if matches!(event, Event::Signal) && self.layout_jobs.busy() {
            match self.layout_jobs.poll(cx) {
                Some(LayoutResult::Layout(laid)) => self.apply_layout(cx, laid),
                Some(LayoutResult::Reveal(revealed)) => self.apply_reveal(cx, revealed),
                Some(LayoutResult::Drop(Dropped { token, drop })) => {
                    if let Some(tag) = self.drag.dropped(cx, self.uid, token, drop, &mut self.scene) {
                        self.hold_drag(cx, tag);
                    }
                }
                Some(LayoutResult::Kept(Kept { token, hints })) => self.kept(cx, token, hints),
                None => {}
            }
        }
        // Whoever runs the bench drives its frames; the explorer only looks, and draws again
        // when the bench has published (never per display frame while it plays).
        if matches!(event, Event::NextFrame(_) | Event::Signal | Event::Actions(_)) && self.live_moved(cx) {
            self.draw_bg.redraw(cx);
        }
        if let Some(frame) = self.frame.is_event(event) {
            self.frame = NextFrame::default();
            if self.assemble_pending {
                self.assemble_pending = false;
                self.state.assemble_at = Some(frame.time);
                self.state.fly = None;
                self.dirty = true;
            }
            self.draw_bg.redraw(cx);
        }
        let now = cx.seconds_since_app_start();
        // The inspector card over the sheet takes its pointer events first, then the probe flags,
        // then the canvas.
        if !self.pane.pressed() && !matches!(self.name_edit, Some(NameEdit::Net(_))) && self.card_event(cx, event) {
            return;
        }
        // A digit typed over the sheet with a part selected starts its new value in the card
        // (user: "type 22k"); the field takes the keys after it.
        if !self.field_open() && self.selected.len() == 1 {
            if let Hit::TextInput(typed) = event.hits(cx, self.draw_bg.area()) {
                if !typed.was_paste && starts_value(&typed.input) {
                    self.open_card_field(cx, Some(typed));
                    if self.value_edit.is_some() { return; }
                }
            }
        }
        let click_pending = self.click_wire() || self.drag.click_to_drop();
        if click_pending && self.click_button(cx, event) { return; }
        if !click_pending && !self.pane.pressed() && self.flag_event(cx, event, now) {
            return;
        }
        // Probes go to the bench a host names, else to the one the electronics host runs.
        let probing = matches!(event, Event::Drag(_) | Event::Drop(_) | Event::DragEnd | Event::KeyDown(_));
        let bench = if probing { self.bench.clone().or_else(|| host_bench(cx)) } else { None };
        if let Some(bench) = &bench {
            let mut dropped_label = None;
            let mut over = None;
            let dropped = probe_target(cx, event, self.draw_bg.area(), self.uid, |abs| {
                if matches!(event, Event::Drag(_)) { over = Some(abs); }
                let (net, label) = self.probe_at(abs, now)?;
                if matches!(event, Event::Drop(_)) { dropped_label = Some(label); }
                Some(net)
            });
            // A probe carried over a net shows the flag its drop would make there.
            if matches!(event, Event::Drag(_) | Event::Drop(_) | Event::DragEnd) {
                let ghost = over.and_then(|abs| self.probe_ghost(abs, now));
                if ghost != self.flag_ghost {
                    self.flag_ghost = ghost;
                    self.draw_bg.redraw(cx);
                }
            }
            // P is the Power flyout on the sheet (V7); B arms the Probe tool.
            let probe = dropped.and_then(|net| dropped_label.map(|label| (net, label)));
            if let Some((net, label)) = probe {
                self.add_probe(cx, bench.clone(), net, label);
            }
        }
        if matches!(event, Event::Drag(_) | Event::Drop(_) | Event::DragEnd) {
            self.sync_probe_lit(cx);
        }
        let cam = self.pane.cam;
        let press = matches!(event, Event::MouseDown(_)) || matches!(event, Event::TouchUpdate(e)
            if e.touches.iter().any(|touch| touch.state == TouchState::Start));
        let wire_unit = (press && !click_pending && cx.has_global::<ElecLive>() && self.reveal_focus.is_none())
            .then(|| self.sheet_design.as_ref().and_then(|design| design.units.get(self.unit)).map(|unit| unit.id)).flatten();
        let empty = std::cell::Cell::new(false);
        let primary_shift = match event {
            Event::MouseDown(e) if e.button.is_primary() => Some(e.modifiers.shift),
            Event::TouchUpdate(e) if press => Some(e.modifiers.shift),
            _ => None,
        };
        let select_press = primary_shift.is_some() && self.armed.tool == CircuitTool::Select && !self.busy()
            && !self.field_open() && self.carry_armed.is_none() && self.scrub.is_none();
        let items = WireItems {
            base: ExplorerItems { sheet: &self.scene, reveal: self.reveal.as_ref(), cam: &cam, now },
            sheet: &self.sheet, unit: wire_unit, tool: self.armed.tool, click_pending, click: Default::default(), pin: Default::default(),
            empty: select_press.then_some(&empty),
        };
        let pressed = self.pane.pressed();
        let inputs = self.pane.handle_event(cx, event, self.draw_bg.area(), &items);
        let original = items.click.take();
        let toggle_part = select_press && primary_shift == Some(true) && match &original {
            Some(key) => matches!(key, Some(Sel::Comp(_))),
            None => inputs.iter().any(|input| matches!(input, CanvasInput::Press { key: Sel::Comp(_), down: true })),
        };
        if press {
            self.marquee = if empty.get() { Marquee::Pressed } else { Marquee::Idle };
            self.wire_click = original;
            if let Some(from) = items.pin.get() {
                if matches!(self.armed.tool, CircuitTool::Select | CircuitTool::Wire)
                    && !toggle_part && !self.field_open() && self.carry_armed.is_none() && self.scrub.is_none() {
                    self.begin_wire(cx, from, from);
                }
            }
        }
        if click_pending {
            if let Event::MouseMove(e) = event {
                if e.handled.get() == self.draw_bg.area() || self.pane.pressed() {
                    self.click_pointer(cx, self.pane.cam.to_world(e.abs), false);
                }
            }
        }
        // CanvasInput::Camera also accompanies hover fades; only actual motion invalidates
        // the editor's saved screen rectangle.
        if matches!(self.name_edit, Some(NameEdit::Net(_))) && self.pane.cam != cam {
            self.close_value_edit(cx);
        }
        if self.note.is_some() && self.pane.cam != cam {
            self.clear_note();
        }
        // A zoom under a gesture keeps its chip at one screen size.
        if self.gesture.is_some() && self.pane.cam.px_per_unit != cam.px_per_unit {
            self.rebuild_chip(Tokens::for_theme(self.dark));
        }
        self.rides_press(cx, event, now);
        // Escape before the drag threshold consumes the pane's press without an input.
        // The explanatory chip still belongs to that press and must end with it.
        let release_rides = matches!(event, Event::MouseUp(_) | Event::FingerCancel(_))
            || matches!(event, Event::KeyDown(key) if key.key_code == KeyCode::Escape)
            || matches!(event, Event::KeyUp(key) if !self.pane.pressed() && matches!(key.key_code, KeyCode::KeyG | KeyCode::KeyM))
            || matches!(event, Event::TouchUpdate(e) if e.touches.iter()
                .any(|t| matches!(t.state, TouchState::Stop | TouchState::Cancel)));
        if self.drag.rides() && release_rides {
            self.drag.release_rides(now);
            self.draw_bg.redraw(cx);
        }
        // The pane consumes Escape below its threshold. A cancelled touch can also arrive as
        // a release. Neither ends the first pin press with a click.
        let interrupted = pressed && !self.pane.pressed() && (matches!(event, Event::FingerCancel(_))
            || matches!(event, Event::KeyDown(key) if key.key_code == KeyCode::Escape)
            || matches!(event, Event::TouchUpdate(e) if e.touches.iter().any(|t| t.state == TouchState::Cancel)));
        if interrupted && !matches!(self.marquee, Marquee::Idle) {
            self.marquee = Marquee::Idle;
            self.draw_bg.redraw(cx);
            return;
        }
        if interrupted && self.drag.click_to_drop() {
            self.cancel_click(cx);
            return;
        }
        if interrupted && self.gesture.as_ref().is_some_and(|g| !g.carries() && (g.phase() == Phase::Armed || g.phase().sampling())) {
            self.wire_click = None;
            self.escape_gesture(cx);
            return;
        }
        for mut input in inputs {
            if toggle_part && matches!(input, CanvasInput::Press { down: true, .. }) { continue; }
            if let CanvasInput::Click { key, .. } = &mut input {
                if let Some(original) = self.wire_click.take() { *key = original; }
            }
            self.input(cx, input);
        }
        if pressed && !self.pane.pressed() { self.marquee = Marquee::Idle; }
        // A flying value changes bounds even when its hovered identity is unchanged.
        self.hover_value_box = self.state.hover.and_then(|item| self.value_box(item, now));
        // The pane has set its cursor. Hover resolves once and never previews or lays out (V3).
        // Wires start only on the main sheet with a host (begin_wire), so only there is a pin line a handle.
        if matches!(event, Event::MouseMove(_) | Event::MouseUp(_)) && !self.busy() && self.scrub.is_none() {
            self.hover_wire = !self.pinned && self.reveal.is_none() && self.reveal_focus.is_none()
                && cx.has_global::<ElecLive>() && self.pane.pointer_world().is_some_and(|at| self.wire_source(at).is_some());
        }
        // The bar's position follows the pointer by grid cell.
        let cell = self.pane.pointer_world().map(|[x, y]| [x.round() as i32, y.round() as i32]);
        if cell != self.status_cell {
            self.status_cell = cell;
            self.draw_bg.redraw(cx);
        }
        self.value_cursor(cx);
        self.resume_wire(cx);
    }

    /// One node for the pane (a schematic publishes no item nodes): its unit and an O(1)
    /// summary. Parts are reached through `semantics_lookup`.
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.role_description, "schematic");
        let unit = self.design.as_ref().and_then(|design| design.units.get(self.unit));
        let name = unit.map(|unit| unit.name.as_str()).filter(|name| !name.is_empty()).unwrap_or("Schematic");
        makepad_platform::a11y::set_string(&mut node.name, name);
        let (parts, nets) = unit.map_or((0, 0), |unit| (unit.circuit.comps.len(), unit.circuit.nets.len()));
        let focus = self.reveal_focus.and_then(|n| unit?.circuit.comps.get(n as usize)).map(|comp| comp.refdes.as_str());
        let selected = self.selected_node().and_then(|n| unit?.circuit.comps.get(n as usize)).map(|comp| match &comp.value {
            Some(value) => format!("{} {}", comp.refdes, value.text),
            None => comp.refdes.clone(),
        });
        let value = match (focus, selected) {
            _ if self.selected.len() > 1 => format!("{parts} parts, {nets} nets, {} selected", self.selected.len()),
            (Some(refdes), _) => format!("{parts} parts, {nets} nets, showing {refdes}'s relations"),
            (None, Some(part)) => format!("{parts} parts, {nets} nets, {part} selected"),
            (None, None) => format!("{parts} parts, {nets} nets"),
        };
        makepad_platform::a11y::set_string(&mut node.value, &value);
    }

    /// The parts a query names, built only on a frame that asks; the pane's frames carry none.
    fn semantics_lookup(&self, cx: &Cx, lookup: &SemanticLookup, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let (query, key) = match lookup {
            SemanticLookup::Terms(q) => (Some(*q), q.id_term()
                .filter(|id| cx.a11y.action_owner(*id).uid() == self.uid.0)
                .and_then(|id| cx.a11y.action_part_key(id))),
            SemanticLookup::Key(key) => (None, Some(*key)),
        };
        let items = ExplorerItems { sheet: &self.scene, reveal: self.reveal.as_ref(), cam: &self.pane.cam,
            now: cx.seconds_since_app_start() };
        out.extend(items.lookup(query, key).into_iter().map(|(key, node)| (key, None, node)));
    }

    /// A lookup answer's actions, routed here by owner: a press is a click on that part by its
    /// key (exact at any zoom); scroll-into-view brings it into plain sight in one frame.
    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        if request.action != Actions::PRESS && request.action != Actions::SCROLL_INTO_VIEW {
            return ActionResult::Unhandled;
        }
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let cam = self.pane.cam;
        let items = ExplorerItems { sheet: &self.scene, reveal: self.reveal.as_ref(), cam: &cam,
            now: cx.seconds_since_app_start() };
        let Some(part) = items.lookup_parts().into_iter().find(|part| part_key(&part.sel) == key) else {
            return ActionResult::Unhandled;
        };
        let Some(body) = part.body else { return ActionResult::Unhandled };
        let centre = [(body[0] + body[2]) * 0.5, (body[1] + body[3]) * 0.5];
        if request.action == Actions::PRESS {
            let at = items.press_rect(&part.sel, body).map_or(centre, |rect| {
                let (x, y) = rect.center();
                cam.to_world(dvec2(f64::from(x), f64::from(y)))
            });
            cx.set_key_focus(self.draw_bg.area());
            self.input(cx, CanvasInput::Click { key: Some(part.sel), at, clicks: 1, shift: false });
        } else {
            let shown = cam.visible();
            let inside = shown[0] <= body[0] && shown[1] <= body[1] && body[2] <= shown[2] && body[3] <= shown[3];
            let size = (body[2] - body[0]).min(body[3] - body[1]).max(1e-6);
            let small = size * cam.px_per_unit < 2.0 * HIT_RADIUS;
            if inside && !small {
                return ActionResult::Handled;
            }
            let mut to = cam;
            to.center = centre;
            if small {
                to.px_per_unit = 4.0 * HIT_RADIUS / size;
            }
            self.close_value_edit(cx);
            self.pane.nav.stop();
            self.pane.cam = to;
            self.draw_bg.redraw(cx);
        }
        ActionResult::Handled
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.follow(cx);
        self.sync_live(cx);
        let rect = cx.walk_turtle(walk);
        if rect.size.x > 0.0 && rect.size.y > 0.0 {
            self.pane_aspect = rect.size.x / rect.size.y;
        }
        let tokens = Tokens::for_theme(self.dark);
        if !self.capture_attempted {
            self.capture_attempted = true;
            self.templates = Templates::capture(cx, &self.draw_line, &self.draw_box, &self.draw_arc, &self.draw_bridge, &self.draw_mini, &self.draw_pin);
            if let Some(templates) = &self.templates {
                // I21: submit the shader even while loading. Alpha zero would skip the draw;
                // this one transparent instance is outside the pane's clip at alpha one.
                let mut rows = Vec::with_capacity(templates.mini.slots());
                templates.mini.append(&mut rows, &MiniInst {
                    c: Common::at([0.0; 3], [0.0; 4]),
                    rect: [0.0, 0.0, 0.5, 0.5], slot: -1.0,
                    stroke: [0.0; 2], fill: [0.0; 4],
                });
                self.mini_warm.set(templates.mini.slots(), rows);
                let mut rows = Vec::with_capacity(templates.bridge.slots());
                templates.bridge.append(&mut rows, &BridgeInst {
                    c: Common::at([0.0; 3], [0.0; 4]),
                    ends: [0.0, 0.0, 1.0, 0.0], bend: super::scene::BRIDGE_BEND,
                    stroke: [1.0, 0.0], fade: [0.0, 0.0, 1e9, 0.0],
                    ..BridgeInst::default()
                });
                self.bridge_warm.set(templates.bridge.slots(), rows);
                let mut rows = Vec::with_capacity(templates.pin.slots());
                templates.pin.append(&mut rows, &PinInst {
                    c: Common::at([0.0; 3], [0.0; 4]),
                    at: [0.0; 2], dir: [1.0, 0.0], size: 0.22, live_pin: -1.0,
                });
                self.pin_warm.set(templates.pin.slots(), rows);
                let live = self.live.get_or_insert_with(|| CanvasLive::off(cx));
                let warm = CanvasUniforms {
                    cam: [rect.pos.x as f32 - 16.0, rect.pos.y as f32 - 16.0, 1.0, 1.0],
                    motion_t: 0.0, hover_item: -1.0, hover_mix: 0.0,
                    drag_item: -1.0, drag_offset: [0.0; 2], layer_alpha: 1.0,
                    time: 0.0, live_time: 0.0, light: !self.dark,
                    clip: [rect.pos.x as f32, rect.pos.y as f32,
                        (rect.pos.x + rect.size.x) as f32, (rect.pos.y + rect.size.y) as f32],
                };
                draw_mini_layer(cx, &mut self.mini_warm, &mut self.draw_mini, &warm, tokens, live, -1.0);
                warm.with(tokens, |u| self.bridge_warm.draw(cx, &mut self.draw_bridge.draw_vars, u, live));
                warm.with(tokens, |u| self.pin_warm.draw(cx, &mut self.draw_pin.draw_vars, u, live));
            }
        }
        if self.design.is_none() || self.applied_gen < self.design_gen {
            self.draw_bg.draw_grid(cx, rect, None, tokens);
            return DrawStep::done();
        }
        // The first camera fit must use the arriving sheet, not empty loading bounds.
        if self.field_open() && self.pane.cam.view != rect {
            self.close_value_edit(cx);
        }
        if std::mem::take(&mut self.refit) {
            self.pane.placed = false;
        }
        self.pane.set_covered_inset(self.drawn_inset);
        let bounds = self.sheet.bounds.map(f64::from);
        if self.opening_view.as_ref().is_some_and(|opening| opening.prepare(&mut self.pane, &self.sheet, rect.size, bounds)) {
            self.opening_view = None;
            self.refit_after_layout = false;
        }
        self.pane.place(cx, rect, Some(bounds));
        self.draw_bg.draw_grid(cx, rect, Some(&self.pane.cam), tokens);
        self.relayout_for_pane(cx);
        if self.templates.is_none() || self.assemble_pending {
            return DrawStep::done();
        }
        let now = cx.seconds_since_app_start();
        if self.dirty {
            self.rebuild(tokens);
        }
        self.resume_wire(cx);
        if self.state.assemble_at.is_some() && now >= self.scene.settle_at() {
            self.state.assemble_at = None;
            self.dirty = true;
            self.rebuild(tokens);
        }
        if self.drag.settle(&mut self.scene, now) { self.publish_connections(); }
        if self.scrub.is_none() && !self.field_open() && !self.busy() {
            if let Some(revealed) = self.pending_reveal.take() {
                self.apply_reveal(cx, revealed);
            }
        }
        if let (Some(reveal), Some(templates)) = (&mut self.reveal, &self.templates) {
            reveal.settle(tokens, templates, now);
        }
        if self.reveal.as_ref().is_some_and(|reveal| reveal.finished(now)) {
            self.reveal = None;
            self.drop_reveal_live(cx);
            if self.fit_after_home {
                self.fit_after_home = false;
                self.fit(cx);
            }
        }
        self.sync_probe_lit(cx);
        let hover_item = match &self.probe_lit {
            Some((_, item)) => *item,
            None => self.pane.hovered().and_then(|sel| self.scene.item_of(sel)),
        };
        let hover_net = hover_item.and_then(|item| self.scene.sel_of(item)).and_then(|sel| match sel {
            Sel::Net(net) => Some(net.net.0 as usize),
            _ => None,
        });
        let mini_hover = hover_net.and_then(|net| self.mini_slots.get(net)).copied().unwrap_or(-1.0);
        let reveal_mini_hover = hover_net.and_then(|net| self.reveal_mini_slots.get(net)).copied().unwrap_or(-1.0);
        let mut uniforms = CanvasUniforms {
            cam: self.pane.cam.uniforms(),
            motion_t: now as f32,
            hover_item: hover_item.map_or(-1.0, |item| item as f32),
            hover_mix: if self.probe_lit.is_some() { 1.0 } else { self.pane.hover_mix() },
            drag_item: -1.0,
            drag_offset: [0.0; 2],
            layer_alpha: 1.0,
            time: now as f32,
            live_time: now as f32,
            light: !self.dark,
            clip: [rect.pos.x as f32, rect.pos.y as f32,
                (rect.pos.x + rect.size.x) as f32, (rect.pos.y + rect.size.y) as f32],
        };
        let mut live = match &self.live {
            Some(live) => live.clone(),
            None => self.live.insert(CanvasLive::off(cx)).clone(),
        };
        self.live_moved(cx);
        self.bind_pressed(cx, &mut live);
        if cx.has_global::<ElecLive>() {
            let host = cx.get_global::<ElecLive>();
            let violations = &host.snap.violations;
            if let Some(line) = self.templates.as_ref().map(|t| &t.line).filter(|_| self.erc_generation != Some(violations.generation)) {
                let unit = self.sheet_design.as_ref().and_then(|design| design.units.get(self.unit));
                let marks = unit.map_or_else(Vec::new, |unit|
                    super::erc_marks::erc_marks(&self.scene.hits.pins, self.scene.part_bodies(),
                        self.scene.hits.wires.iter().map(|wire| (wire.net, wire.a, wire.b)), &violations.value, unit.id, tokens));
                let mut rows = Vec::with_capacity(marks.len() * line.slots());
                for mark in &marks { line.append(&mut rows, mark); }
                self.erc_marks.set(line.slots(), rows);
                self.erc_generation = Some(violations.generation);
            }
            let sim = &host.sim;
            if let Some(mini) = &sim.mini { live.mini = mini.clone(); }
            if let Some(pins) = &sim.pins { live.pins = pins.clone(); }
            if let Some(nets) = &sim.nets {
                live.nets = nets.clone();
                if let Some(segs) = &sim.segs { live.segs = segs.clone(); }
                if let Some(parts) = &sim.parts { live.parts = parts.clone(); }
                uniforms.live_time = self.live_clock;
            }
        } else if self.erc_generation.take().is_some() || self.erc_marks.count() != 0 {
            self.erc_marks.set(1, Vec::new());
        }
        if let Some(items) = self.followed_items(cx) {
            live.items = items;
            live.items_on = true;
        }
        let sheet_uniforms = CanvasUniforms {
            drag_offset: self.drag.offset(now),
            layer_alpha: self.reveal.as_ref().map_or(1.0, |reveal| reveal.sheet_alpha(now)),
            ..uniforms
        };
        let glow_uniforms = CanvasUniforms {
            drag_item: self.drag_pin_item.map_or(-1.0, |item| item as f32),
            ..sheet_uniforms
        };
        glow_uniforms.with(tokens, |u| {
            self.frame_fills.draw(cx, &mut self.draw_box.draw_vars, u, &live);
            self.glows.draw(cx, &mut self.draw_box.draw_vars, u, &live);
        });
        // The probes' tags behind the symbols and wires; their names come with the drawing's text.
        self.draw_flags(cx, rect, now);
        sheet_uniforms.with(tokens, |u| {
            self.ghost_boxes.draw(cx, &mut self.draw_box.draw_vars, u, &live);
            self.ghost_lines.draw(cx, &mut self.draw_line.draw_vars, u, &live);
            self.ghost_arcs.draw(cx, &mut self.draw_arc.draw_vars, u, &live);
            self.ghost_pins.draw(cx, &mut self.draw_pin.draw_vars, u, &live);
            self.boxes.draw(cx, &mut self.draw_box.draw_vars, u, &live);
            self.lines.draw(cx, &mut self.draw_line.draw_vars, u, &live);
            self.name_lines.draw(cx, &mut self.draw_line.draw_vars, u, &live);
            self.arcs.draw(cx, &mut self.draw_arc.draw_vars, u, &live);
            self.bridges.draw(cx, &mut self.draw_bridge.draw_vars, u, &live);
            self.pins.draw(cx, &mut self.draw_pin.draw_vars, u, &live);
            // Re-placed (enlarged) labels left their layout spots, so the layout's leaders would point at nothing.
            if self.legible == 1.0 && text_points(sheet::TEXT_NET, self.pane.cam.px_per_unit) >= TEXT_MIN_POINTS {
                self.leaders.draw(cx, &mut self.draw_line.draw_vars, u, &live);
            }
            self.erc_marks.draw(cx, &mut self.draw_line.draw_vars, u, &live);
        });
        let mini_uniforms = CanvasUniforms {
            layer_alpha: sheet_uniforms.layer_alpha * mini_alpha(self.pane.cam.px_per_unit),
            ..sheet_uniforms
        };
        draw_mini_layer(cx, &mut self.minis, &mut self.draw_mini, &mini_uniforms, tokens, &live, mini_hover);
        self.texts(cx, now, false);
        if let Some(reveal) = &mut self.reveal {
            let reveal_uniforms = CanvasUniforms {
                hover_item: if self.probe_lit.is_some() {
                    hover_item.and_then(|item| self.scene.sel_of(item)).and_then(|sel| reveal.scene.item_of(&sel))
                        .map_or(-1.0, |item| item as f32)
                } else { uniforms.hover_item },
                ..uniforms
            };
            reveal.draw_layers(cx, &reveal_uniforms, &self.pane.cam, tokens, &live,
                &mut self.draw_box, &mut self.draw_line, &mut self.draw_arc, &mut self.draw_bridge, &mut self.draw_mini, &mut self.draw_pin, reveal_mini_hover);
        }
        uniforms.with(tokens, |u| {
            self.chip.draw(cx, &mut self.draw_box.draw_vars, u, &live);
            self.chip_lines.draw(cx, &mut self.draw_line.draw_vars, u, &live);
            self.chip_arcs.draw(cx, &mut self.draw_arc.draw_vars, u, &live);
        });
        self.texts(cx, now, true);
        if let Some(reveal) = &mut self.reveal {
            reveal.draw_trail(cx, &self.pane.cam, &uniforms, tokens, &live,
                &mut self.draw_box, &mut self.draw_line, &mut self.draw_text, now);
        }
        self.drag.draw(cx, &uniforms, &self.pane.cam, tokens, &live,
            &mut self.draw_box, &mut self.draw_line, &mut self.draw_arc, &mut self.draw_text,
            &mut self.up_lists[2], now);
        let pin_uniforms = CanvasUniforms {
            drag_item: self.drag_pin_item.map_or(-1.0, |item| item as f32), drag_offset: self.drag.offset(now), ..uniforms
        };
        pin_uniforms.with(tokens, |u| self.drag_pins.draw(cx, &mut self.draw_pin.draw_vars, u, &live));
        self.draw_flag_labels(cx, rect);
        if let Marquee::Drawing(band) = self.marquee {
            let [r, g, b, a] = tokens.selection;
            self.draw_marquee.color = vec4(r, g, b, a);
            let mut band = self.pane.cam.rect_to_screen(band);
            let px = 1.0 / self.pane.cam.dpi;
            band.size.x = band.size.x.max(px);
            band.size.y = band.size.y.max(px);
            cx.push_clip_rect(rect);
            self.draw_marquee.draw_abs(cx, band);
            cx.pop_clip_rect();
        }
        if let Some(NameEdit::Net(edit)) = &self.name_edit {
            let rect = edit.screen;
            self.draw_name_field(cx, scope, rect);
        }
        // The strip over the sheet, at the pane's top left.
        if self.strip_shown != Some(self.armed) {
            apply_strip(cx, &self.tools, self.armed);
            self.strip_shown = Some(self.armed);
        }
        // The status bar along the bottom, the strip above it.
        self.sync_status(cx);
        let bar = draw_bar(cx, scope, self.uid, &self.status, rect);
        let above = Rect { pos: rect.pos, size: dvec2(rect.size.x, (rect.size.y - bar).max(0.0)) };
        let room = self.card_room(above);
        self.draw_inspector(cx, scope, room, tokens, now);
        let strip = draw_strip(cx, scope, self.uid, &self.tools, above);
        if self.strip_at != strip {
            self.strip_at = strip;
            // The card drew before the strip and needs its new rectangle on the next draw.
            self.draw_bg.redraw(cx);
        }
        self.draw_library(cx, scope, above);
        let (left, right) = self.covered_sides(rect);
        let inset = [left, 0.0, right, bar];
        if self.drawn_inset != inset {
            self.drawn_inset = inset;
            if self.pane.keeps_fit { self.request_frame(cx); }
        }
        // the Checks editor lists the unit this sheet shows: told once per change, which redraws
        // the window so the editor follows in the same frame wherever it drew
        let unit = self.design.as_ref().and_then(|d| d.units.get(self.unit)).map(|unit| unit.id);
        if unit != self.told_unit && cx.has_global::<ElecLive>() {
            self.told_unit = unit;
            cx.get_global::<ElecLive>().shown_unit = unit;
            cx.redraw_all();
        }
        self.pane.animate(cx);
        if now < self.scene.settle_at() || self.reveal.as_ref().is_some_and(|reveal| reveal.animating(now))
            || self.drag.animating(now) {
            self.request_frame(cx);
        }
        DrawStep::done()
    }
}

/// Strips enter over two screen points at the same zoom rung as net names.
pub(super) fn mini_alpha(px_per_unit: f64) -> f32 {
    ((text_points(sheet::TEXT_NET, px_per_unit) - TEXT_MIN_POINTS) / 2.0).clamp(0.0, 1.0) as f32
}

/// Add the strip-only hover uniform on the stack; frames never allocate instance rows.
pub(super) fn draw_mini_layer(cx: &mut Cx2d, layer: &mut CanvasLayer, draw: &mut DrawCanvasMini,
    uniforms: &CanvasUniforms, tokens: &Tokens, live: &CanvasLive, hover: f32)
{
    if uniforms.layer_alpha <= 0.0 { return; }
    uniforms.with(tokens, |u| {
        // The canvas uniforms, then mini_hover.
        let mut values = [(live_id!(mini_hover), std::slice::from_ref(&hover)); CanvasUniforms::COUNT + 1];
        values[..CanvasUniforms::COUNT].copy_from_slice(u);
        layer.draw(cx, &mut draw.draw_vars, &values, live);
    });
}

/// The sheet's deepest useful zoom (world units are grid steps): its smallest text, a net name or
/// a value, 1000 points tall. Nothing on the sheet is smaller.
const SHEET_MAX_PPU: f64 = 1000.0 / sheet::TEXT_NET;

/// A quarter turn counter-clockwise about the pane's bottom left (`o`): a draw list with this view
/// transform draws its text running up the screen. List x runs up, list y to the right:
/// screen = (o.x + q.y, o.y - q.x). The sheet's vertical texts draw through it as ordinary (Slug)
/// text, so any zoom costs what a horizontal label does and no glyph is rasterized at its size.
#[derive(Clone, Copy)]
pub(super) struct UpTurn {
    o: DVec2,
    view: Rect,
}

impl UpTurn {
    pub(super) fn of(view: Rect) -> UpTurn {
        UpTurn { o: dvec2(view.pos.x, view.pos.y + view.size.y), view }
    }

    fn matrix(self) -> Mat4f {
        let (x, y) = (self.o.x as f32, self.o.y as f32);
        Mat4f { v: [0.0, -1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, x, y, 0.0, 1.0] }
    }

    /// A screen point in the turned list's coordinates.
    fn to_list(self, s: DVec2) -> DVec2 {
        dvec2(self.o.y - s.y, s.x - self.o.x)
    }

    /// Begins the turned texts in `list` (one list per place that draws them in a frame), clipped
    /// to the pane: a quarter turn keeps it a rectangle, (0, 0) to (height, width) in the list. The
    /// texts sit on a root turtle, which clips them on its own: the turtles around the pane clip
    /// in screen space, which is not the list's.
    pub(super) fn begin(self, cx: &mut Cx2d, list: &mut Option<DrawList2d>) {
        let list = list.get_or_insert_with(|| DrawList2d::new(cx));
        list.begin_always(cx);
        // Text reads its list's transform while it draws, so it is set before as well as after.
        list.set_view_transform_self_only(cx, &self.matrix());
        cx.begin_root_turtle(dvec2(self.view.size.y, self.view.size.x), Layout::default());
    }

    pub(super) fn end(self, cx: &mut Cx2d, list: &mut Option<DrawList2d>) {
        cx.end_pass_sized_turtle();
        if let Some(list) = list {
            list.end(cx);
            list.set_view_transform(cx, &self.matrix());
        }
    }
}

/// Draws a sheet text: in the plain pass (`up` None) the horizontal ones, in the turned pass
/// (between `UpTurn::begin` and `end`) the vertical ones. Both as Slug text: no glyph raster.
pub(super) fn draw_text(cx: &mut Cx2d, draw: &mut DrawText, cam: &crate::canvas::Camera2d, text: &Text,
    at: [f64; 2], alpha: f32, up: Option<UpTurn>)
{
    let points = text_points(text.size * text.scale, cam.px_per_unit);
    if text.hidden || text.vertical != up.is_some() || points < TEXT_MIN_POINTS || alpha <= 0.0 { return; }
    let color = text.color;
    draw.color = vec4(color[0], color[1], color[2], color[3] * alpha);
    draw.text_style.font_size = points as f32;
    let size = draw.layout(cx, 0.0, 0.0, None, false, Align::default(), &text.text).size_in_lpxs;
    let x = match text.align {
        glyphs::Align::Left => 0.0,
        glyphs::Align::Right => size.width as f64,
        glyphs::Align::Center => size.width as f64 * 0.5,
    };
    let anchor = cam.to_screen(at);
    let anchor = up.map_or(anchor, |turn| turn.to_list(anchor));
    draw.draw_abs(cx, anchor - dvec2(x, size.height as f64 * 0.5), &text.text);
}


//! The board and its local interaction overlay. Fold events materialize text and shape handles alongside
//! the spatial view. Drawing only borrows this projection and the camera.
use super::gesture::{scroll_nav, AlignTo, BoardTool, Gesture, Handle, NewItem, Placement, Preview, ScrollNav, Transform, DRAG_SLOP};
use super::live::{Incoming, Outbox};
use super::look::{self, Body, Lift, Look, Place, Surface, STEPS};
use super::tree::step_kind;
use super::view::{Present, Step, Target};
use super::{BoardItem, BoardView, Rect as Bounds, RectIndex};
use crate::tweak::overlay::{self, Selections};
use crate::model::{Action as ModelAction, ItemStyle, RestackTo};
use makepad_canvas::{
    remap_event, sync_handled, Camera, CameraState, CanvasLayer, DrawCanvasCard, DrawCanvasGrid,
    DrawCanvasShadow, MAX_ZOOM, MIN_ZOOM,
};
use makepad_town_core::entity::{field, kind};
use makepad_town_core::event::{Id, Key, Value};
use makepad_town_core::fold::State;
use makepad_widgets::{
    drag_visual::CxDragVisualExt,
    event::{EditModifier, ScrollPhase, TouchState, TAP_COUNT_TIME},
    text::{font_family::FontFamilyId, layouter::LaidoutText},
    *,
};
use std::{collections::{BTreeSet, HashMap}, ops::Range, rc::Rc};

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*
    let ZoomButton = PanelButton{
        height: 25 padding: Inset{left: 8 right: 8} align: Align{x: 0.5 y: 0.5} grab_key_focus: false
        draw_bg +: {color: palette.panel border_size: 0}
    }
    mod.widgets.BoardCanvas = #(BoardCanvas::register_widget(vm)){
        width: Fill height: Fill
        // The board's colours, under the names of Town's skin tokens so a skin sets them: items
        // without a colour of their own, the selection, frames and shadows. Inks are worked out
        // against the ground (the grid's `color_a`).
        sticky_1: #xF6DC86
        shape_fill: #x5AA2E0
        shape_line: #x487FAE
        section_fill: #x8FA9BD24
        section_line: #x8FA9BD66
        board_ink: palette.text
        board_selection: palette.accent
        frame_color: palette.panel
        frame_line: palette.line
        caption_color: palette.muted
        shadow_color: #x000000
        shadow_strength: 1.0
        shadow_softness: 1.0
        title +: {text_style: theme.font_bold{font_size: 10.5}}
        grid +: {
            color_a: palette.bg color_b: palette.line
            pixel: fn() {
                let p = self.rect_pos + self.pos * self.rect_size - self.origin
                let dist = length(fract(p / self.cell) - vec2(0.5)) * self.cell
                return Pal.premul(mix(self.color_a, self.color_b, 1.0 - smoothstep(0.5, 1.4, dist)))
            }
        }
        outline +: {color: palette.accent}
        tool_chip: DragChip{}
        editor: TextInputFlat{
            visible: false is_multiline: true submit_on_enter: false
            empty_text: "" padding: 0 margin: 0 align: Align{x: 0 y: 0}
            a11y: {label: "Board item text"}
            draw_bg +: {pixel: fn() { return vec4(0.0) }}
            draw_text +: {get_color: fn() { return self.color } text_style: theme.font_regular{font_size: 10.5}}
            draw_cursor +: {color: palette.accent}
        }
        // Shadows draw in their own pass under each layer, so cards cast none of their own.
        card +: {border_size: 0.0 shadow_radius: 0.0}
        share_label +: {text_style: theme.font_regular{font_size: 9}}
        label +: {text_style: theme.font_regular{font_size: 10.5}}
        nav: RoundedView{
            width: Fit height: Fit flow: Right spacing: 2 padding: 3
            draw_bg +: {color: palette.panel border_size: 1 border_color: palette.line border_radius: 5}
            zoom_out := ZoomButton{text: "−"}
            zoom_level := ZoomButton{text: "100 %" width: 54}
            zoom_in := ZoomButton{text: "+"}
        }
    }
}

/// The zoom cluster's distance from the board's corner.
const NAV_MARGIN: f64 = 11.0;
/// How far the cluster's buttons and the zoom keys step.
const ZOOM_STEP: f64 = 2.0;
/// New text fits a board draw lays out; the rest waits for the next frames, without text.
const FITS_PER_DRAW: usize = 96;

fn rect(b: Bounds) -> Rect {
    Rect { pos: dvec2(b.x0, b.y0), size: dvec2(b.x1 - b.x0, b.y1 - b.y0) }
}

fn total_bounds() -> Bounds {
    Bounds::new(f64::MIN, f64::MIN, f64::MAX, f64::MAX)
}

fn painted(item: &BoardItem) -> Bounds {
    let b = item.rect;
    // A layer's shadows draw beneath all its cards, so a shadow never splits a layer. Only a
    // stroke's edge and a frame's caption widen what an item paints.
    let pad = if item.kind == kind::SHAPE { 1.0 } else { 0.0 };
    let top = if body_of(&item.kind, "").caption_above() { look::caption_reach(b.x1 - b.x0) } else { pad };
    Bounds::new(b.x0 - pad, b.y0 - top, b.x1 + pad, b.y1 + pad)
}

fn painter_layers(view: &BoardView, ids: &[Id], preview: Option<&Preview>) -> Vec<Range<usize>> {
    let mut result = Vec::new();
    let mut hits = Vec::new();
    let footprint = |id: &Id| {
        let item = view.get(id).unwrap();
        let mut bounds = painted(item);
        if let Some(p) = preview {
            let r = match p.transform {
                Transform::Move { .. } => p.items[id].rect,
                Transform::Resize { .. } => p.rect(id).unwrap(),
            };
            bounds = Bounds::new(
                r.x0 + (bounds.x0 - item.rect.x0), r.y0 + (bounds.y0 - item.rect.y0),
                r.x1 + (bounds.x1 - item.rect.x1), r.y1 + (bounds.y1 - item.rect.y1),
            );
        }
        bounds
    };
    for range in view.layers(ids) {
        let mut start = range.start;
        let mut index = RectIndex::new();
        for i in range.clone() {
            let b = footprint(&ids[i]);
            index.query_rect(b, &mut hits);
            if hits.iter().any(|&j| b.overlaps(footprint(&ids[j as usize]))) {
                result.push(start..i);
                start = i;
                index = RectIndex::new();
            }
            index.insert(i as u32, b);
        }
        result.push(start..range.end);
    }
    result
}

#[derive(Default, Clone, Copy)]
struct Detail {
    body: bool,
    text: bool,
    decoration: bool,
}

/// The detail pressure after a board draw that cost `load` frame budgets, the last one having cost
/// `last_load`, and whether to draw once more.
/// - One slow draw (a first draw's compiles, a page fault, a busy GPU) is a hiccup, not a heavy
///   board, so the pressure rises only by the cooler of the last two draws.
/// - Under pressure, a cool draw asks for another frame. A board that stopped changing then
///   restores its detail by itself and sleeps; without this, its text stayed hidden until an edit.
/// - A hot draw asks for nothing, so a board too heavy for its detail never keeps redrawing.
fn pressure_after(pressure: f64, load: f64, last_load: f64) -> (f64, bool) {
    let next = load.min(last_load).sqrt().max(pressure * 0.94).max(1.);
    (next, next > 1. && load.sqrt() < next)
}

fn readable(size: f64, threshold: f64, was: bool) -> bool {
    size >= threshold * if was { 0.9 } else { 1.1 }
}

#[derive(PartialEq)]
struct MetricsKey {
    width: f32,
    font: FontFamilyId,
    size: f32,
    spacing: f32,
    centre: bool,
    family_generation: u64,
}

/// What a fitted text size depends on.
#[derive(PartialEq)]
struct FitKey {
    inner: (f32, f32),
    cap: f32,
    fixed: bool,
    centre: bool,
    font: FontFamilyId,
    family_generation: u64,
}

#[derive(Default)]
struct Face {
    text: String,
    shape: String,
    // Validate a literal for display only when its register changes, never on a paint or pan.
    open_writers: HashMap<String, Id>,
    layout: Option<(MetricsKey, Rc<LaidoutText>)>,
    detail: Detail,
    /// The font size, in points, the text fits its item at, and what it was fitted to.
    fit: Option<(FitKey, f32)>,
}

impl Face {
    fn update(&mut self, item: &BoardItem, state: &State) -> bool {
        let e = &state.entities[&item.text_handle()];
        self.open_writers.retain(|name, _| e.fields.contains_key(&Key::Name(name.clone())));
        for (name, register) in e.open_fields() {
            if self.open_writers.get(name) == Some(&register.writer) { continue }
            self.open_writers.insert(name.to_owned(), register.writer);
            if let Value::Literal(src) = &register.value {
                if let Err(errors) = makepad_splash_syntax::read_value(src) {
                    for (offset, reason) in errors {
                        eprintln!("board item {} property {name} at byte {offset}: {reason}; drawing default",
                            makepad_town_core::hex(&e.id));
                    }
                }
            }
        }
        let mut text = e.text(field::TEXT);
        if text.is_empty() && e.kind == kind::SECTION {
            text = e.text(field::TITLE);
        }
        let fallback;
        if item.kind == kind::DESIGN {
            fallback = super::design::label(e);
            text = &fallback;
        } else if item.kind == kind::APP {
            fallback = crate::tweak::app_caption(e, |_, _| None);
            text = &fallback;
        } else if let Key::Name(name) = &item.kind {
            fallback = format!("unknown kind {name}");
            text = &fallback;
        }
        let changed = self.text != text || self.shape != e.text(field::SHAPE);
        if self.text != text {
            text.clone_into(&mut self.text);
            self.layout = None;
            self.fit = None;
        }
        let shape = e.text(field::SHAPE);
        if self.shape != shape {
            shape.clone_into(&mut self.shape);
        }
        changed
    }

    fn layout(&mut self, cx: &mut Cx2d, label: &DrawText, width: f32, centre: bool) -> &LaidoutText {
        let style = &label.text_style;
        let key = MetricsKey {
            width,
            font: style.font_family_id(),
            size: style.font_size,
            spacing: style.line_spacing,
            centre,
            family_generation: cx.fonts.borrow().font_family_generation(),
        };
        if self.layout.as_ref().is_none_or(|(old, _)| old != &key) {
            let align = Align { x: if centre { 0.5 } else { 0. }, y: 0. };
            let layout = label.layout(cx, 0., 0., Some(width), true, align, &self.text);
            self.layout = Some((key, layout));
        }
        &self.layout.as_ref().unwrap().1
    }

    /// The font size, in points, the text takes in `place`: the cap's step when the size is
    /// fixed, else the largest step up to the cap whose layout fits the box (the smallest step
    /// when none does). It is laid out again only when the text, the box or the fonts change,
    /// never on a pan or zoom. `None` when a new fit is due and this draw's `budget` is spent.
    fn fit(&mut self, cx: &mut Cx2d, label: &mut DrawText, place: &Place, budget: &mut usize) -> Option<f32> {
        let key = FitKey {
            inner: (place.inner.size.x as f32, place.inner.size.y as f32),
            cap: place.cap as f32,
            fixed: place.fixed,
            centre: place.centre_x,
            font: label.text_style.font_family_id(),
            family_generation: cx.fonts.borrow().font_family_generation(),
        };
        if let Some((old, size)) = &self.fit {
            if *old == key {
                return Some(*size);
            }
        }
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let top = look::step_at_most(place.cap);
        let size = if place.fixed || self.text.is_empty() {
            STEPS[top]
        } else {
            let (w, h) = (key.inner.0 + 0.5, key.inner.1 + 0.5);
            let (mut lo, mut hi) = (0, top);
            while lo < hi {
                let mid = (lo + hi + 1) / 2;
                label.text_style.font_size = STEPS[mid];
                let size = self.layout(cx, label, key.inner.0, place.centre_x).size_in_lpxs;
                if size.width <= w && size.height <= h { lo = mid } else { hi = mid - 1 }
            }
            STEPS[lo]
        };
        self.fit = Some((key, size));
        Some(size)
    }

    fn fitted(&self) -> Option<f32> {
        self.fit.as_ref().map(|(_, size)| *size)
    }
}

struct Scene {
    id: Id,
    view: BoardView,
    /// The board's presenting path, refreshed when a step or a step's target changes.
    path: Vec<Step>,
    faces: HashMap<Id, Face>,
    query: Option<Bounds>,
    visible: Vec<Id>,
    footprints: Vec<Bounds>,
    layers: Vec<Range<usize>>,
    lifted: Vec<Id>,
    lifted_layers: Vec<Range<usize>>,
    lifted_transform: Option<Transform>,
}

impl Scene {
    fn new(state: &State, id: Id) -> Self {
        let view = BoardView::build(state, id);
        let mut scene = Self {
            id,
            path: view.path(state),
            view,
            faces: HashMap::new(),
            query: None,
            visible: vec![],
            footprints: vec![],
            layers: vec![],
            lifted: vec![],
            lifted_layers: vec![],
            lifted_transform: None,
        };
        scene.update_faces(state, scene.view.visible(total_bounds()));
        scene
    }

    fn update_faces(&mut self, state: &State, ids: impl IntoIterator<Item = Id>) {
        for id in ids {
            if let Some(item) = self.view.get(&id) {
                self.faces.entry(id).or_default().update(item, state);
            } else {
                self.faces.remove(&id);
            }
        }
    }

    fn apply(&mut self, state: &State, touched: &[Id], gesture: &mut Gesture) -> bool {
        let changed = self.view.apply(state, touched);
        if changed.is_empty() {
            return false;
        }
        gesture.fold_changed(&self.view, &changed);
        self.update_faces(state, changed);
        self.query = None;
        true
    }

    fn lift(&mut self, preview: Option<&Preview>) {
        self.query = None;
        self.lifted.clear();
        self.lifted_layers.clear();
        self.lifted_transform = preview.map(|p| p.transform);
        if let Some(p) = preview {
            self.lifted = self.view.visible(total_bounds());
            self.lifted.retain(|id| p.items.contains_key(id));
            self.lifted_layers = painter_layers(&self.view, &self.lifted, Some(p));
        }
    }

    fn pick(&self, p: DVec2) -> Option<Id> {
        let top = self.view.hit((p.x, p.y), 0.)?;
        let contains = |id: &Id| {
            let item = self.view.get(id).unwrap();
            if item.kind != kind::SHAPE {
                return true;
            }
            let r = rect(item.rect);
            if r.size.x == 0. || r.size.y == 0. {
                return false;
            }
            let x = (p.x - r.pos.x) / r.size.x * 2. - 1.;
            let y = (p.y - r.pos.y) / r.size.y * 2. - 1.;
            match self.faces[id].shape.as_str() {
                "ellipse" => x * x + y * y <= 1.,
                "circle" => dvec2(x * r.size.x, y * r.size.y).length() <= r.size.x.min(r.size.y),
                "diamond" => x.abs() + y.abs() <= 1.,
                "triangle" => x.abs() <= (y + 1.) * 0.5,
                _ => true,
            }
        };
        if contains(&top) {
            return Some(top);
        }
        self.view.visible(Bounds::new(p.x, p.y, p.x, p.y)).into_iter().rev().find(contains)
    }

    fn cull(&mut self, bounds: Bounds, preview: Option<&Preview>) {
        if self.query == Some(bounds) {
            return;
        }
        self.query = Some(bounds);
        let mut ids = self.view.visible(bounds);
        if let Some(p) = preview {
            ids.retain(|id| !p.items.contains_key(id));
        }
        if self.visible == ids
            && ids.iter().zip(&self.footprints).all(|(id, b)| painted(self.view.get(id).unwrap()) == *b)
        {
            return;
        }
        self.layers = painter_layers(&self.view, &ids, None);
        self.footprints = ids.iter().map(|id| painted(self.view.get(id).unwrap())).collect();
        self.visible = ids;
    }

    fn bounds(&self) -> Option<Rect> {
        self.faces
            .keys()
            .filter_map(|id| self.view.get(id))
            .map(painted)
            .reduce(|a, b| Bounds::new(a.x0.min(b.x0), a.y0.min(b.y0), a.x1.max(b.x1), a.y1.max(b.y1)))
            .map(rect)
    }

    /// What Present shows at each step of the path. A path with nothing to show presents one
    /// step: the selection, else the whole board.
    fn present_targets(&self, selection: &BTreeSet<Id>) -> Vec<(Id, Option<Bounds>)> {
        let targets = self.view.present_targets(&self.path);
        if targets.iter().any(|(_, rect)| rect.is_some()) {
            return targets;
        }
        let union = |a: Bounds, b: Bounds| Bounds::new(a.x0.min(b.x0), a.y0.min(b.y0), a.x1.max(b.x1), a.y1.max(b.y1));
        let chosen = selection.iter().filter_map(|id| self.view.get(id)).map(|item| item.rect).reduce(union)
            .or_else(|| self.view.present_targets(&[Step { id: self.id, target: Target::Board, title: String::new() }])[0].1);
        vec![(self.id, chosen)]
    }

    /// Whether `id` is a step of this path or what one leads to.
    fn on_path(&self, id: Id) -> bool {
        self.path.iter().any(|step| step.id == id || match step.target {
            Target::Item(t) | Target::Gone(t) | Target::Unknown(Some(t)) => t == id,
            Target::Board | Target::Unknown(None) => false,
        })
    }
}

#[derive(Clone, Debug)]
pub enum BoardCanvasAction {
    Act(ModelAction),
    Place(NewItem),
    CommitText { board: Id, action: ModelAction },
    ToolChanged,
    SelectionChanged,
    Undo,
    Redo,
}

struct TextEdit {
    item: Id,
    initial: String,
}

/// A click completed on a live App picture. The window routes the pick to this item's session.
#[derive(Clone, Debug)]
pub struct AppPick { pub item: Id, pub at: crate::tweak::At }

#[derive(Script, ScriptHook, Widget)]
pub struct BoardCanvas {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[live]
    grid: DrawCanvasGrid,
    #[live]
    card: DrawCanvasCard,
    #[live]
    shadow: DrawCanvasShadow,
    #[live]
    outline: DrawColor,
    #[live]
    label: DrawText,
    #[live]
    share_label: DrawText,
    /// Section titles, in bold.
    #[live]
    title: DrawText,
    #[live]
    sticky_1: Vec4f,
    #[live]
    shape_fill: Vec4f,
    #[live]
    shape_line: Vec4f,
    #[live]
    section_fill: Vec4f,
    #[live]
    section_line: Vec4f,
    #[live]
    board_ink: Vec4f,
    #[live]
    board_selection: Vec4f,
    #[live]
    frame_color: Vec4f,
    #[live]
    frame_line: Vec4f,
    #[live]
    caption_color: Vec4f,
    #[live]
    shadow_color: Vec4f,
    #[live]
    shadow_strength: f32,
    #[live]
    shadow_softness: f32,
    #[redraw]
    #[rust]
    area: Area,
    #[rust]
    local_area: Area,
    #[rust]
    recording: Option<DrawList2d>,
    #[rust]
    layer: Option<CanvasLayer>,
    #[rust]
    camera: Camera,
    #[rust]
    scene: Option<Scene>,
    #[rust]
    app_presence: HashMap<Id, Selections>,
    #[rust]
    live_in: Incoming,
    #[rust]
    live_out: Outbox,
    #[rust]
    live_frame: NextFrame,
    #[rust]
    drag_pointer: Option<(DVec2, KeyModifiers)>,
    #[rust]
    fit_pending: bool,
    #[rust]
    gesture: Gesture,
    #[rust]
    pub tool: BoardTool,
    #[rust]
    placement_start: Option<DVec2>,
    // Set once the press has moved past the slop. Until then nothing is drawn, and a release
    // is a click that places the default size.
    #[rust]
    placement_dragged: bool,
    #[rust]
    ghost: Option<Placement>,
    #[live]
    tool_chip: ScriptObjectRef,
    #[find]
    #[live]
    editor: WidgetRef,
    /// The zoom cluster in the board's corner: out, the level (a press fits the board), in.
    #[find]
    #[live]
    nav: WidgetRef,
    /// Whether the cluster was drawn on the last frame, so it takes the pointer before the board.
    #[rust]
    nav_shown: bool,
    #[rust]
    editing: Option<TextEdit>,
    #[rust]
    last_click: Option<Id>,
    #[rust]
    writable: bool,
    #[rust]
    toolbar_drag: Option<(BoardTool, WidgetRef)>,
    #[rust]
    overlay: Option<DrawList2d>,
    #[rust]
    preview_layer: Option<CanvasLayer>,
    #[rust]
    overlay_pending: bool,
    #[rust]
    repaint_pending: bool,
    #[rust]
    pressure: f64,
    /// The last board draw's cost in frame budgets; 0 before the first draw.
    #[rust]
    last_load: f64,
    /// Presenting along the board's path, for this view only; the board is read-only meanwhile.
    #[rust]
    present: Option<Present>,
    #[rust]
    next_frame: NextFrame,
}

impl BoardCanvas {
    pub fn is_empty(&self) -> bool {
        self.scene.as_ref().is_none_or(|s| s.faces.is_empty())
    }

    fn request(&mut self, cx: &mut Cx) {
        if !self.repaint_pending {
            self.repaint_pending = true;
            self.area.redraw(cx);
        }
    }

    pub fn select_board(&mut self, cx: &mut Cx, state: &State, selected: Option<Id>, shown: bool) {
        if self.scene.as_ref().map(|s| s.id) == selected {
            return;
        }
        self.end_live(cx, false);
        self.finish_text(cx, true);
        self.last_click = None;
        self.scene = selected.map(|id| Scene::new(state, id));
        self.present = None;
        self.fit_pending = true;
        self.placement_start = None;
        self.ghost = None;
        self.toolbar_drag = None;
        self.tool = BoardTool::Select;
        self.gesture = Gesture::default();
        self.pressure = 1.;
        self.last_load = 0.;
        if shown {
            self.request(cx);
        } else {
            self.repaint_pending = true;
        }
    }

    pub fn applied(&mut self, cx: &mut Cx, state: &State, ids: &[Id], shown: bool) {
        if self.live_in.reconcile(state, self.board_id()) { self.request_overlay(cx); }
        let Some(scene) = &mut self.scene else { return };
        let path_changed = ids.iter().any(|&id| scene.on_path(id) || state.entities.get(&id).is_some_and(|e| step_kind(&e.kind)));
        if path_changed {
            scene.path = scene.view.path(state);
        }
        let was_empty = scene.faces.is_empty();
        if !scene.apply(state, ids, &mut self.gesture) {
            // A presented step's target can change without any face changing.
            if path_changed && self.present.is_some() {
                if shown { self.request(cx) } else { self.repaint_pending = true }
            }
        } else {
            scene.lift(self.gesture.preview.as_ref());
            let lost_item = self.editing.as_ref().is_some_and(|e| scene.view.get(&e.item).is_none_or(|i| i.locked));
            self.fit_pending |= was_empty && !scene.faces.is_empty();
            if lost_item { self.finish_text(cx, false); }
            if shown {
                self.request(cx);
            } else {
                self.repaint_pending = true;
            }
        }
        self.update_live(cx, false);
    }

    // The link currently publishes one refolded State for a batch, without event ids.
    // Reconcile only when that batch arrives. The input and draw paths keep their index.
    pub fn peer_snapshot(&mut self, cx: &mut Cx, state: &State, shown: bool) {
        if self.live_in.reconcile(state, self.board_id()) { self.request_overlay(cx); }
        let Some(scene) = &mut self.scene else { return };
        let next = BoardView::build(state, scene.id);
        let ids = next.visible(total_bounds());
        let previous = scene.view.visible(total_bounds());
        let mut changed = std::collections::HashSet::new();
        scene.faces.retain(|id, _| {
            let keep = next.get(id).is_some();
            if !keep {
                changed.insert(*id);
            }
            keep
        });
        for id in &ids {
            let item = next.get(id).unwrap();
            let face_changed = scene.faces.entry(*id).or_default().update(item, state);
            if face_changed || scene.view.get(id) != Some(item) {
                changed.insert(*id);
            }
        }
        scene.view = next;
        let path = scene.view.path(state);
        let path_changed = path != scene.path;
        scene.path = path;
        if changed.is_empty() && ids == previous && !(path_changed && self.present.is_some()) {
            return;
        }
        self.fit_pending |= previous.is_empty() && !ids.is_empty();
        self.gesture.fold_changed(&scene.view, &changed);
        scene.lift(self.gesture.preview.as_ref());
        if self.editing.as_ref().is_some_and(|e| scene.view.get(&e.item).is_none_or(|i| i.locked)) {
            self.finish_text(cx, false);
        }
        if shown {
            self.request(cx);
        } else {
            self.repaint_pending = true;
        }
        self.update_live(cx, false);
    }

    fn fit(&self, view: Rect) -> Option<Camera> {
        let mut world = self.scene.as_ref()?.bounds()?;
        world.size = dvec2(world.size.x.max(1.), world.size.y.max(1.));
        let insets = Inset { left: 16., right: 16., top: 56., bottom: 16. };
        let fitted = Camera::fit(view, world, insets)?;
        fitted
            .zoom_about(fitted.local_to_screen(fitted.world_to_local(world.center())), fitted.scale.min(1.))
    }

    fn fit_board(&mut self, cx: &mut Cx) {
        if let Some(fit) = self.fit(self.camera.view) {
            self.change_camera(cx, fit);
        }
    }

    /// A zoom about the board's centre, as the cluster's buttons and the keys step it.
    fn zoom_step(&mut self, cx: &mut Cx, by: f64) {
        self.zoom(cx, self.camera.view.center(), self.camera.scale * by);
    }

    fn change_camera(&mut self, cx: &mut Cx, next: Camera) {
        if next.scale == self.camera.scale && next.pan == self.camera.pan {
            return;
        }
        self.camera = next;
        self.refresh_drag();
        self.update_live(cx, false);
        self.fit_pending = false;
        self.request(cx);
    }

    fn refresh_drag(&mut self) {
        let Some(scene) = &self.scene else { return };
        let camera = self.camera;
        self.gesture.set_viewport(&scene.view, camera_bounds(camera));
        if let Some((screen, mods)) = self.drag_pointer {
            if self.gesture.moving().is_some() {
                self.gesture.motion(&scene.view, camera.world_at(screen), screen, camera.scale, mods.alt, mods.shift);
            }
        }
    }

    fn zoom(&mut self, cx: &mut Cx, anchor: DVec2, scale: f64) {
        if let Some(next) = self.camera.zoom_about(anchor, scale.clamp(MIN_ZOOM.min(self.camera.scale), MAX_ZOOM))
        {
            self.change_camera(cx, next);
        }
    }

    fn pan(&mut self, cx: &mut Cx, pan: DVec2) {
        let next =
            Camera::new(self.camera.view, CameraState { pan_x: pan.x, pan_y: pan.y, zoom: self.camera.scale });
        self.change_camera(cx, next);
    }
}

fn body_of(kind: &Key, shape: &str) -> Body {
    if *kind == kind::APP { Body::Frame } else { Body::of(kind, shape) }
}

fn item_colour(item: &BoardItem, look: &Look) -> Vec4f {
    if item.kind == kind::DESIGN {
        return look.frame;
    }
    match item.colour {
        Some(n) => Vec4f::from_u32(if n > 0xffffff { n } else { (n << 8) | 255 }),
        None => match item.kind {
            Key::Number(kind::STICKY) => look.sticky,
            Key::Number(kind::TEXT_BLOCK) => look.ink,
            Key::Number(kind::SECTION) => look.section,
            _ => look.shape,
        },
    }
}

/// The solid chip a section's title sits on: its colour, or the skin's section line.
fn section_chip(item: &BoardItem, look: &Look) -> Vec4f {
    look::opaque(if item.colour.is_some() { item_colour(item, look) } else { look.section_line })
}

/// The ink of an item's text: a text block's own colour made readable on the board, a frame's
/// caption colour, a section title's ink on its chip, else an ink that reads on the item's fill.
fn text_ink(item: &BoardItem, look: &Look) -> Vec4f {
    let colour = item_colour(item, look);
    match item.kind {
        Key::Number(kind::TEXT_BLOCK) => look::readable_on(colour, look.ground),
        Key::Number(kind::DESIGN | kind::APP) => look.caption,
        Key::Number(kind::SECTION) => look::ink_on(section_chip(item, look), look.ground),
        _ => look::ink_on(colour, look.ground),
    }
}

/// An item's surface. Shapes and sections without a colour of their own take the skin's lines.
fn item_surface(item: &BoardItem, body: Body, colour: Vec4f, size: DVec2, look: &Look) -> Surface {
    let preset = if body == Body::Frame { super::design::preset_for((item.w, item.h)) } else { "" };
    let mut s = look::surface(body, colour, size, preset, look);
    match body {
        _ if item.colour.is_some() => {}
        Body::Shape(_) => s.border_color = look.shape_line,
        Body::Section => s.border_color = look.section_line,
        _ => {}
    }
    s
}

fn camera_bounds(camera: Camera) -> Bounds {
    let a = camera.world_at(camera.view.pos);
    let b = camera.world_at(camera.view.pos + camera.view.size);
    Bounds::new(a.x, a.y, b.x, b.y)
}

fn local_rect(camera: &Camera, b: Bounds) -> Rect {
    let r = rect(b);
    Rect { pos: camera.world_to_local(r.pos), ..r }
}

fn supported(kind: &Key) -> bool {
    matches!(kind, Key::Name(_) | Key::Number(kind::STICKY | kind::SHAPE | kind::SECTION | kind::TEXT_BLOCK | kind::DESIGN | kind::APP))
}

#[derive(Default)]
struct Tint {
    count: usize,
    sum: Vec4f,
}

impl Tint {
    fn add(&mut self, c: Vec4f) {
        self.count += 1;
        self.sum = self.sum + c;
    }
    fn colour(&self) -> Vec4f {
        self.sum * (1. / self.count as f32)
    }
}

fn card(draw: &mut DrawCanvasCard, cx: &mut Cx2d, r: Rect, colour: Vec4f, s: &Surface) {
    draw.color = colour;
    draw.shape = s.shape.code();
    draw.border_radius = s.radius;
    draw.shade = s.shade;
    draw.rim = s.rim;
    draw.border_size = s.border;
    draw.border_color = s.border_color;
    draw.draw_abs(cx, r);
}

/// The shadow of a card with surface `s` in `r`, cut away from `hole` pixels inside its outline.
fn cast(draw: &mut DrawCanvasShadow, cx: &mut Cx2d, r: Rect, s: &Surface, l: Lift, look: &Look, hole: f32) {
    draw.color = vec4(look.shadow.x, look.shadow.y, look.shadow.z, l.alpha * look.shadow_strength);
    draw.shape = s.shape.code();
    draw.radius = s.radius;
    draw.blur = (l.blur * look.shadow_softness).max(0.75);
    draw.offset = vec2(0., l.offset);
    draw.contact = l.contact * look.shadow_strength;
    draw.contact_blur = (l.contact_blur * look.shadow_softness).max(0.75);
    draw.hole = hole;
    draw.draw_abs(cx, r);
}

impl BoardCanvas {
    fn look(&self) -> Look {
        Look {
            sticky: self.sticky_1,
            shape: self.shape_fill,
            shape_line: self.shape_line,
            section: self.section_fill,
            section_line: self.section_line,
            ink: self.board_ink,
            ground: self.grid.color_a,
            frame: self.frame_color,
            frame_line: self.frame_line,
            caption: self.caption_color,
            shadow: self.shadow_color,
            shadow_strength: self.shadow_strength,
            shadow_softness: self.shadow_softness.clamp(0., 1.),
        }
    }

    /// Draws the board, or its lifted items. Whether some text still waits for its fit.
    fn draw_board(&mut self, cx: &mut Cx2d, camera: Camera, clip: Rect, lifted: bool) -> bool {
        let a = camera.world_at(clip.pos);
        let b = camera.world_at(clip.pos + clip.size);
        let local = local_rect(&camera, Bounds::new(a.x, a.y, b.x, b.y));
        if !lifted {
            cx.add_aligned_rect_area(&mut self.local_area, local);
        }
        // Items just off screen still cast their shadows into it.
        let reach = Lift::REACH;
        let bounds = Bounds::new(a.x - reach, a.y - reach, b.x + reach, b.y + reach);
        let look = self.look();
        self.outline.color = self.board_selection;
        let Some(scene) = &mut self.scene else { return false };
        let preview = self.gesture.preview.as_ref();
        if !lifted {
            scene.cull(bounds, preview);
        }
        let geometry = |item: &BoardItem| {
            if lifted {
                preview.and_then(|p| p.rect(&item.id)).unwrap_or(item.rect)
            } else {
                item.rect
            }
        };
        if let Some(p) = preview.filter(|p| lifted && matches!(p.transform, Transform::Resize { .. })) {
            if scene.lifted_transform != Some(p.transform) {
                scene.lifted_layers = painter_layers(&scene.view, &scene.lifted, Some(p));
                scene.lifted_transform = Some(p.transform);
            }
        }
        let (ids, layers) = if lifted {
            (&scene.lifted, &scene.lifted_layers)
        } else {
            (&scene.visible, &scene.layers)
        };
        let pixels = camera.scale * cx.current_dpi_factor();
        let pressure = self.pressure.max(1.);
        let mut sections = HashMap::<Id, Tint>::new();
        let mut cells = HashMap::<(i64, i64), Tint>::new();
        let cell_size = 8. / pixels;
        let mut budget = FITS_PER_DRAW;
        let mut waiting = false;
        for id in ids {
            let item = scene.view.get(id).unwrap();
            if !supported(&item.kind) {
                continue;
            }
            if lifted && !geometry(item).intersects(bounds) {
                continue;
            }
            let r = rect(geometry(item));
            let face = scene.faces.get_mut(id).unwrap();
            let body = body_of(&item.kind, &face.shape);
            // At the font's native pixel size a person's words must remain readable
            // after the editor closes. In an overview, cards can still lose detail.
            let label_is_content = item.kind == kind::TEXT_BLOCK || matches!(item.kind, Key::Name(_))
                || (pixels >= 1. && !face.text.is_empty());
            let detail_pressure = if label_is_content { 1. } else { pressure };
            let shown = readable(r.size.x.max(r.size.y) * pixels, 3. * detail_pressure.sqrt(), face.detail.body);
            // Text is fitted only where its largest size could be read; the fit decides.
            let place = look::place(body, r);
            let px = |pt: f32| pt as f64 * look::PX_PER_PT * pixels;
            let text_threshold = 4. * detail_pressure;
            let could_read = shown && !face.text.is_empty()
                && readable(px(STEPS[look::step_at_most(place.cap)]), text_threshold, face.detail.text);
            let label = if place.bold { &mut self.title } else { &mut self.label };
            let text = could_read && match face.fit(cx, label, &place, &mut budget) {
                Some(pt) => readable(px(pt), text_threshold, face.detail.text),
                None => {
                    waiting = true;
                    false
                }
            };
            let d = &mut face.detail;
            d.body = shown;
            d.text = text;
            d.decoration = shown && readable(r.size.x.min(r.size.y) * pixels, 24. * pressure, d.decoration);
        }
        for id in ids {
            let item = scene.view.get(id).unwrap();
            if item.kind == kind::APP || !supported(&item.kind) || scene.faces[id].detail.body || !geometry(item).intersects(bounds) {
                continue;
            }
            if let Some(section) = item.parent_section.filter(|p| {
                scene.faces.get(p).is_some_and(|f| f.detail.body)
                    && scene.view.get(p).is_some_and(|parent| parent.rect.intersects(bounds))
            }) {
                sections.entry(section).or_default().add(item_colour(item, &look));
            } else {
                let p = rect(geometry(item)).center();
                cells
                    .entry(((p.x / cell_size).floor() as i64, (p.y / cell_size).floor() as i64))
                    .or_default()
                    .add(item_colour(item, &look));
            }
        }
        for ((x, y), tint) in cells {
            let mut c = tint.colour();
            c.w = (0.28 + 0.12 * (tint.count as f32).ln_1p()).min(0.9);
            let pos = camera.world_to_local(dvec2(x as f64 * cell_size, y as f64 * cell_size));
            card(&mut self.card, cx, Rect { pos, size: dvec2(cell_size, cell_size) }, c, &Surface::FLAT);
        }
        // Lifted items float higher: their shadows spread and drop further.
        let (blur, drop) = if lifted { (2.2, 2.4) } else { (1., 1.) };
        let mut locks = Vec::new();
        for layer in layers {
            // The draw append search cannot cross a different parent base. Quad
            // and text use the enclosing (penultimate) parent for their lanes.
            cx.push_draw_call_parent();
            cx.push_draw_call_parent();
            // A layer's shadows all lie beneath its cards. Its items never overlap, so they
            // sit side by side at one height, and a neighbour's shadow falls on the board.
            for id in &ids[layer.clone()] {
                let item = scene.view.get(id).unwrap();
                let face = &scene.faces[id];
                if item.kind == kind::APP || !face.detail.decoration || !supported(&item.kind) || !geometry(item).intersects(bounds) {
                    continue;
                }
                let body = body_of(&item.kind, &face.shape);
                let Some(l) = look::lift(body) else { continue };
                let r = local_rect(&camera, geometry(item));
                let s = item_surface(item, body, item_colour(item, &look), r.size, &look);
                cast(&mut self.shadow, cx, r, &s, l.raised(blur, drop), &look, 1.);
            }
            for id in &ids[layer.clone()] {
                let item = scene.view.get(id).unwrap();
                let face = scene.faces.get_mut(id).unwrap();
                if item.kind == kind::APP || !face.detail.body || !supported(&item.kind) || !geometry(item).intersects(bounds) {
                    continue;
                }
                let r = local_rect(&camera, geometry(item));
                let body = body_of(&item.kind, &face.shape);
                let c = item_colour(item, &look);
                let s = item_surface(item, body, c, r.size, &look);
                match body {
                    Body::Section => {
                        let fill = match sections.get(id) {
                            // Its members are too small to draw: the zone takes on their colour.
                            Some(tint) => {
                                let t = look::mix(look::opaque(c), tint.colour(), 0.5);
                                vec4(t.x, t.y, t.z, 0.5)
                            }
                            None if item.colour.is_some() => vec4(c.x, c.y, c.z, c.w * 0.14),
                            None => c,
                        };
                        card(&mut self.card, cx, r, fill, &s);
                        if let (true, Some(pt)) = (face.detail.text, face.fitted()) {
                            // The title's chip, as wide as the title. The text pass reuses its layout.
                            let px = pt as f64 * look::PX_PER_PT;
                            self.title.text_style.font_size = pt;
                            let width = look::place(body, r).inner.size.x as f32;
                            let title = face.layout(cx, &self.title, width, false);
                            let town = r.size - dvec2(2. * look::CHIP_INSET, 2. * look::CHIP_INSET);
                            let size = dvec2(title.size_in_lpxs.width as f64 + 2. * look::CHIP_PAD.0 * px,
                                title.size_in_lpxs.height as f64 + 2. * look::CHIP_PAD.1 * px);
                            let chip = Rect { pos: r.pos + dvec2(look::CHIP_INSET, look::CHIP_INSET),
                                size: dvec2(size.x.min(town.x), size.y.min(town.y)) };
                            let pill = Surface { radius: (chip.size.y * 0.5) as f32, ..Surface::FLAT };
                            card(&mut self.card, cx, chip, section_chip(item, &look), &pill);
                        }
                    }
                    Body::Text if face.detail.text => {}
                    Body::Text => {
                        // Too small to read: a greeked line per line of text, in its ink.
                        let ink = text_ink(item, &look);
                        let pt = face.fitted().unwrap_or(STEPS[look::step_at_most(look::place(body, r).cap)]);
                        let line = (pt as f64 * look::PX_PER_PT).min(r.size.y);
                        let fits = ((r.size.y / (line * 1.3)).floor() as usize).max(1);
                        let rows = match &face.layout {
                            Some((_, l)) if face.fitted().is_some() => l.rows.len(),
                            _ => (face.text.chars().count() as f64 * line * 0.5 / r.size.x.max(1.)).ceil() as usize,
                        }.clamp(1, fits);
                        let top = r.pos.y + (r.size.y - rows as f64 * line * 1.3) * 0.5 + line * 0.3;
                        let bar = Surface { radius: (line * 0.3) as f32, ..Surface::FLAT };
                        for row in 0..rows {
                            let width = if row + 1 == rows && rows > 1 { 0.6 } else { 0.9 };
                            let pos = dvec2(r.pos.x, top + row as f64 * line * 1.3);
                            card(&mut self.card, cx, Rect { pos, size: dvec2(r.size.x * width, line * 0.6) },
                                vec4(ink.x, ink.y, ink.z, ink.w * 0.3), &bar);
                        }
                    }
                    _ => card(&mut self.card, cx, r, c, &s),
                }
                if item.locked && face.detail.decoration {
                    locks.push(camera.local_to_screen(dvec2(r.pos.x + r.size.x, r.pos.y)));
                }
            }
            for corner in locks.drain(..) {
                draw_lock(cx, &mut self.outline, self.grid.color_a, corner, Some(&camera));
            }
            self.label.begin_deferred_slug_flush();
            self.title.begin_deferred_slug_flush();
            for id in &ids[layer.clone()] {
                let item = scene.view.get(id).unwrap();
                let face = scene.faces.get_mut(id).unwrap();
                if self.editing.as_ref().is_some_and(|edit| edit.item == *id) { continue }
                if !supported(&item.kind) || !face.detail.text || face.text.is_empty() || !geometry(item).intersects(bounds) {
                    continue;
                }
                let Some(pt) = face.fitted() else { continue };
                let r = local_rect(&camera, geometry(item));
                let place = look::place(body_of(&item.kind, &face.shape), r);
                if place.inner.size.x <= 0. || place.inner.size.y <= 0. {
                    continue;
                }
                let label = if place.bold { &mut self.title } else { &mut self.label };
                label.color = text_ink(item, &look);
                label.text_style.font_size = pt;
                let laidout = face.layout(cx, label, place.inner.size.x as f32, place.centre_x);
                let height = laidout.size_in_lpxs.height as f64;
                let mut at = place.inner;
                if place.centre_y && height < at.size.y {
                    at.pos.y += (at.size.y - height) * 0.5;
                }
                cx.push_clip_rect(place.inner);
                label.draw_walk_laidout(cx, Walk::abs_rect(at), laidout);
                cx.pop_clip_rect();
            }
            self.title.end_deferred_slug_flush(cx);
            self.label.end_deferred_slug_flush(cx);
            cx.pop_draw_call_parent();
            cx.pop_draw_call_parent();
        }
        waiting
    }
}

fn snap_to_pixel(point: DVec2, dpi: f64) -> DVec2 {
    dvec2((point.x * dpi).round(), (point.y * dpi).round()) / dpi
}

// Draw the same screen-size badge in the board recording and in selection overlays.
fn draw_lock(cx: &mut Cx2d, draw: &mut DrawColor, background: Vec4f, corner: DVec2, layer: Option<&Camera>) {
    let dpi = cx.current_dpi_factor();
    let pixel = dpi.round().max(1.) / dpi;
    let origin = snap_to_pixel(corner, dpi) + dvec2(-16., 2.) * pixel;
    let (origin, pixel) = match layer {
        Some(camera) => (camera.screen_to_local(origin), pixel / camera.scale),
        None => (origin, pixel),
    };
    let bar = |draw: &mut DrawColor, cx: &mut Cx2d, x, y, w, h| {
        draw.draw_abs(cx, Rect { pos: origin + dvec2(x, y) * pixel, size: dvec2(w, h) * pixel });
    };
    let accent = draw.color;
    draw.color = background;
    bar(draw, cx, 0., 0., 14., 15.);
    draw.color = accent;
    // The two shackle uprights meet the top bar of the body.
    bar(draw, cx, 4., 2., 6., 1.);
    bar(draw, cx, 4., 3., 1., 4.);
    bar(draw, cx, 9., 3., 1., 4.);
    bar(draw, cx, 2., 7., 10., 1.);
    bar(draw, cx, 2., 8., 1., 4.);
    bar(draw, cx, 11., 8., 1., 4.);
    bar(draw, cx, 2., 12., 10., 1.);
}

impl BoardCanvas {
    /// A local placement already happened under the pointer in this camera.
    /// Opening a board and the explicit fit command can still fit its content.
    pub fn placed(&mut self, cx: &mut Cx, id: Id) {
        self.fit_pending = false;
        let Some(item) = self.scene.as_ref().and_then(|s| s.view.get(&id)) else { return };
        self.gesture.selection.ids = [id].into();
        self.gesture.selection.anchor = Some(id);
        if matches!(item.kind, Key::Number(kind::STICKY | kind::TEXT_BLOCK)) {
            self.open_text(cx, id, item.kind == kind::TEXT_BLOCK);
        }
        self.request_overlay(cx);
        cx.widget_action(self.widget_uid(), BoardCanvasAction::SelectionChanged);
    }

    pub fn edit_context(&mut self, cx: &mut Cx, shown: bool, writable: bool) {
        self.writable = writable;
        if !shown || !writable {
            self.last_click = None;
            self.finish_text(cx, writable);
        }
    }

    fn open_text(&mut self, cx: &mut Cx, id: Id, select_all: bool) {
        if !self.writable || self.editing.is_some() { return }
        let Some(scene) = &self.scene else { return };
        let Some(item) = scene.view.get(&id) else { return };
        if item.locked || !matches!(item.kind, Key::Number(kind::STICKY | kind::TEXT_BLOCK | kind::SHAPE | kind::SECTION)) { return }
        let Some(face) = scene.faces.get(&id) else { return };
        // The editor sets its text where the item shows it.
        let place = look::place(body_of(&item.kind, &face.shape), rect(item.rect));
        let (ax, ay) = (if place.centre_x { 0.5 } else { 0. }, if place.centre_y { 0.5 } else { 0. });
        let mut editor = self.editor.clone();
        script_apply_eval!(cx, editor, {label_align: Align{x: #(ax) y: 0.0} align: Align{x: 0.0 y: #(ay)}});
        self.last_click = None;
        self.editor.set_visible(cx, true);
        self.editor.set_text(cx, &face.text);
        if let Some(mut input) = self.editor.borrow_mut::<TextInput>() {
            if select_all {
                input.select_all(cx);
            } else {
                let cursor = text::selection::Cursor { index: input.text().len(), prefer_next_row: false };
                input.set_cursor(cx, cursor, false);
            }
            input.take_key_focus_from_user(cx);
        }
        self.editing = Some(TextEdit { item: id, initial: self.editor.text() });
        self.request(cx);
    }

    fn finish_text(&mut self, cx: &mut Cx, commit: bool) {
        let Some(edit) = self.editing.take() else { return };
        let text = self.editor.text();
        if commit && self.writable && text != edit.initial {
            if let Some(scene) = self.scene.as_ref().filter(|s| s.view.get(&edit.item).is_some_and(|i| !i.locked)) {
                let action = ModelAction::EditItem { item: edit.item, style: ItemStyle { text: Some(text), ..Default::default() },
                    w: None, h: None, locked: None };
                cx.widget_action(self.widget_uid(), BoardCanvasAction::CommitText { board: scene.id, action });
            }
        }
        let area = self.editor.area();
        if !area.is_empty() && cx.has_key_focus(area) {
            // Release the IME and the caret timer before the field becomes hidden.
            self.editor.handle_event(cx, &Event::KeyFocus(KeyFocusEvent { prev: area, focus: Area::Empty }), &mut Scope::empty());
        }
        self.editor.set_visible(cx, false);
        self.editor.set_text(cx, "");
        self.request(cx);
    }

    fn item_screen_rect(&self, item: &BoardItem) -> Rect {
        let r = rect(item.rect);
        Rect { pos: self.camera.local_to_screen(self.camera.world_to_local(r.pos)), size: r.size * self.camera.scale }
    }

    fn draw_editor(&mut self, cx: &mut Cx2d, scope: &mut Scope, clip: Rect) {
        let Some(edit) = &self.editing else {
            // Give the hidden field a focus target, without drawing it. A placement can
            // then accept the next keystroke even before the item has had its first frame.
            if let Some(mut input) = self.editor.borrow_mut::<TextInput>() {
                cx.add_aligned_rect_area(&mut input.draw_bg.draw_vars.area, Rect::default());
            }
            return
        };
        let Some(scene) = self.scene.as_ref() else { return };
        let (Some(item), Some(face)) = (scene.view.get(&edit.item), scene.faces.get(&edit.item)) else { return };
        // The text box and size the item draws its text at.
        let place = look::place(body_of(&item.kind, &face.shape), rect(item.rect));
        let pt = face.fitted().unwrap_or(STEPS[look::step_at_most(place.cap)]);
        let scale = self.camera.scale;
        let r = Rect {
            pos: self.camera.local_to_screen(self.camera.world_to_local(place.inner.pos)),
            size: dvec2((place.inner.size.x * scale).max(1.), (place.inner.size.y * scale).max(1.)),
        };
        let ink = text_ink(item, &self.look());
        if let Some(mut input) = self.editor.borrow_mut::<TextInput>() {
            input.set_font_size(cx, pt * scale as f32);
            // The caret takes the item's ink, so it shows on light and dark notes alike.
            input.draw_text.color = ink;
            input.draw_cursor.draw_vars.set_uniform(cx, live_id!(color), &[ink.x, ink.y, ink.z, ink.w]);
        }
        cx.push_clip_rect(clip);
        self.editor.draw_walk_all(cx, scope, Walk::abs_rect(r));
        cx.pop_clip_rect();
    }

    /// The zoom cluster in the bottom-right corner, over a board with something on it; Present
    /// shows the board alone.
    fn draw_nav(&mut self, cx: &mut Cx2d, scope: &mut Scope, clip: Rect) {
        self.nav_shown = self.present.is_none() && !self.is_empty();
        if !self.nav_shown { return }
        self.nav.button(cx, ids!(zoom_level)).set_text(cx, &format!("{:.0} %", self.camera.scale * 100.));
        let layout = Layout { align: Align { x: 1., y: 1. }, padding: Inset { left: NAV_MARGIN, right: NAV_MARGIN,
            top: NAV_MARGIN, bottom: NAV_MARGIN }, ..Layout::flow_down() };
        cx.begin_turtle(Walk::abs_rect(clip), layout);
        self.nav.draw_all(cx, scope);
        cx.end_turtle();
    }

    /// The cluster takes the pointer events over it, and its buttons step or fit the zoom.
    /// Whether the event was the cluster's.
    fn nav_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) -> bool {
        if !self.nav_shown { return false }
        let actions = cx.capture_actions(|cx| self.nav.handle_event(cx, event, scope));
        let pressed = |path| self.nav.button(cx, path).clicked(&actions);
        let step = if pressed(ids!(zoom_in)) {
            Some(Some(ZOOM_STEP))
        } else if pressed(ids!(zoom_out)) {
            Some(Some(1. / ZOOM_STEP))
        } else if pressed(ids!(zoom_level)) {
            Some(None)
        } else {
            None
        };
        if let Some(step) = step {
            match step {
                Some(by) => self.zoom_step(cx, by),
                None => self.fit_board(cx),
            }
            // The board keeps the keys: Space, H and the zoom keys go on working.
            cx.set_key_focus_from_user(self.area);
        }
        let at = match event {
            Event::MouseDown(e) => e.abs,
            Event::MouseMove(e) => e.abs,
            Event::Scroll(e) => e.abs,
            _ => return false,
        };
        // A drag the board holds goes on across the cluster.
        self.nav.area().clipped_rect(cx).contains(at) && !cx.fingers.is_area_captured(self.local_area)
    }

    fn editor_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) -> bool {
        let Some(edit) = &self.editing else { return false };
        let outside = |point| self.scene.as_ref().and_then(|s| s.view.get(&edit.item))
            .is_none_or(|item| !self.item_screen_rect(item).contains(point) || !self.area.clipped_rect(cx).contains(point));
        let press_outside = match event {
            Event::MouseDown(d) => outside(d.abs),
            Event::TouchUpdate(e) => e.touches.iter().any(|t| t.state == TouchState::Start && outside(t.abs)),
            _ => false,
        };
        if press_outside || matches!(event, Event::WindowLostFocus(_)) {
            self.finish_text(cx, true);
            return false
        }
        if matches!(event, Event::KeyDown(k) if k.key_code == KeyCode::Escape
            || (matches!(k.key_code, KeyCode::ReturnKey | KeyCode::NumpadEnter) && k.modifiers.is_edit_primary())) {
            self.finish_text(cx, true);
            cx.set_key_focus_from_user(self.area);
            return true
        }
        self.editor.handle_event(cx, event, scope);
        if matches!(event, Event::KeyFocus(e) if e.prev == self.editor.area() && e.focus != e.prev) {
            self.finish_text(cx, true);
        }
        // Wheel and pinch may still move the camera under the field.
        !matches!(event, Event::Scroll(_) | Event::Pinch(_))
    }

    pub fn show_invitation(&self) -> bool {
        self.is_empty() && !self.tool.places() && self.toolbar_drag.is_none()
    }

    pub fn drag_tool(&mut self, cx: &mut Cx, tool: BoardTool, toolbar: WidgetRef) {
        if self.toolbar_drag.is_some() || !tool.places() { return }
        let chip = cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.tool_chip.as_object().into()));
        chip.label(cx, ids!(label)).set_text(cx, tool.label());
        let payload = DragItem::String { value: tool.label().into(), internal_id: Some(live_id!(board_creation_tool)) };
        self.toolbar_drag = Some((tool, toolbar));
        cx.start_dragging_with(vec![payload], chip, dvec2(14., 10.));
        cx.widget_action(self.widget_uid(), BoardCanvasAction::ToolChanged);
    }

    fn drag_event(&mut self, cx: &mut Cx, event: &Event) {
        let Some((tool, toolbar)) = &self.toolbar_drag else { return };
        let tool = *tool;
        let over_toolbar = toolbar.area().clipped_rect(cx);
        if matches!(event, Event::KeyDown(k) if k.key_code == KeyCode::Escape) {
            if let Some(chip) = cx.drag_visual() { chip.set_visible(cx, false); }
            self.toolbar_drag = None;
            self.ghost = None;
            self.request_overlay(cx);
            cx.widget_action(self.widget_uid(), BoardCanvasAction::ToolChanged);
            return
        }
        match event {
            Event::WindowLostFocus(_) | Event::DragEnd => {
                self.toolbar_drag = None;
                self.ghost = None;
                self.request_overlay(cx);
                cx.widget_action(self.widget_uid(), BoardCanvasAction::ToolChanged);
                return
            }
            Event::Drag(e) if e.items.iter().any(|i| matches!(i, DragItem::String { internal_id: Some(id), .. }
                if *id == live_id!(board_creation_tool))) => {}
            Event::Drop(e) if e.items.iter().any(|i| matches!(i, DragItem::String { internal_id: Some(id), .. }
                if *id == live_id!(board_creation_tool))) => {}
            _ => return,
        }
        match event.drag_hits(cx, self.area) {
            DragHit::Drag(d) if d.state != DragState::Out && !over_toolbar.contains(d.abs) => {
                self.place_at(tool, d.abs, d.modifiers.alt);
                *d.response.lock().unwrap_or_else(|e| e.into_inner()) = DragResponse::Copy;
            }
            DragHit::Drop(d) if !over_toolbar.contains(d.abs) => {
                self.place_at(tool, d.abs, d.modifiers.alt);
                self.finish_placement(cx);
                self.toolbar_drag = None;
            }
            _ => { self.ghost = None; }
        }
        self.request_overlay(cx);
    }

    pub fn arm(&mut self, cx: &mut Cx, tool: BoardTool) {
        if self.gesture.preview.is_some() { return }
        self.tool = tool;
        self.placement_start = None;
        self.ghost = None;
        self.gesture.hover = None;
        self.gesture.handle = None;
        cx.set_key_focus_from_user(self.area);
        self.request_overlay(cx);
        cx.widget_action(self.widget_uid(), BoardCanvasAction::ToolChanged);
    }

    fn place_at(&mut self, tool: BoardTool, point: DVec2, alt: bool) {
        let start = self.placement_start.filter(|_| self.placement_dragged);
        self.ghost = tool.preview(self.scene.as_ref().map(|s| &s.view), start,
            self.camera.world_at(point), self.camera.scale, alt);
    }

    fn finish_placement(&mut self, cx: &mut Cx) {
        self.placement_start = None;
        if let Some(ghost) = self.ghost.take() {
            self.arm(cx, BoardTool::Select);
            cx.widget_action(self.widget_uid(), BoardCanvasAction::Place(ghost.item));
        }
    }

    pub fn can_align(&self) -> bool { self.gesture.selection.ids.len() >= 2 }

    pub fn alignment(&self, choice: AlignTo) -> Option<ModelAction> {
        self.gesture.align(&self.scene.as_ref()?.view, choice)
    }

    pub fn board_id(&self) -> Option<Id> {
        self.scene.as_ref().map(|s| s.id)
    }

    fn request_overlay(&mut self, cx: &mut Cx) {
        if !self.overlay_pending {
            self.overlay_pending = true;
            if let Some(list) = &self.overlay {
                list.redraw(cx);
            } else {
                self.area.redraw(cx);
            }
        }
    }

    fn schedule_live(&mut self, cx: &mut Cx) {
        if self.live_frame.0 == 0 { self.live_frame = cx.new_next_frame(); }
    }

    fn end_live(&mut self, cx: &mut Cx, dropped: bool) {
        if let Some(board) = self.board_id() {
            if self.live_out.end(cx, board, dropped) { self.schedule_live(cx); }
        }
        self.drag_pointer = None;
    }

    fn update_live(&mut self, cx: &mut Cx, dropped: bool) {
        let Some((screen, _)) = self.drag_pointer else { return };
        let pointer = self.camera.world_at(screen);
        if let (Some(board), Some(preview)) = (self.board_id(), self.gesture.moving()) {
            if self.live_out.moving(cx, board, preview, pointer) { self.schedule_live(cx); }
        } else {
            self.end_live(cx, dropped);
        }
    }

    pub(super) fn poll_live(&mut self, cx: &mut Cx, event: &Event, model: &crate::model::Model,
        relay: Option<&mut impl crate::tweak::Relay>, messages: Vec<(crate::tweak::Seat, Vec<u8>)>) {
        let board = self.board_id();
        if self.live_out.beat(event, board) { self.schedule_live(cx); }
        let mut redraw = self.live_in.expire(cx, event, board);
        for (seat, body) in messages {
            redraw |= self.live_in.receive(cx, &model.state, seat, &body, board);
        }
        if redraw { self.request_overlay(cx); }
        if matches!(event, Event::NextFrame(frame) if frame.set.contains(&self.live_frame)) {
            self.live_frame = NextFrame::default();
            self.live_out.frame(model, relay);
        }
    }

    fn draw_live(&mut self, cx: &mut Cx2d, clip: Rect) {
        if self.live_in.ghosts.is_empty() { return }
        let Some(scene) = &self.scene else { return };
        let camera = self.camera;
        let screen = |p| camera.local_to_screen(camera.world_to_local(p));
        let previous = (self.outline.color, self.share_label.color);
        cx.push_clip_rect(clip);
        for ((_, board), ghost) in &mut self.live_in.ghosts {
            if *board != scene.id { continue }
            let start = makepad_platform::monotonic_seconds();
            let ring = Surface { border: 2., border_color: ghost.colour, ..Surface::FLAT };
            for id in &ghost.motion.ids {
                let Some(item) = scene.view.get(id) else { continue };
                let r = rect(ghost.motion.transform.item_rect(item));
                let at = Rect { pos: screen(r.pos), size: r.size * camera.scale };
                if at.intersects(clip) { card(&mut self.card, cx, at, vec4(0., 0., 0., 0.), &ring); }
            }
            let point = screen(ghost.motion.pointer);
            if clip.contains(point) {
                self.outline.color = ghost.colour;
                // A small crosshair stays the same size as the camera zooms.
                for (offset, size) in [(dvec2(-5., -1.), dvec2(11., 2.)), (dvec2(-1., -5.), dvec2(2., 11.))] {
                    self.outline.draw_abs(cx, Rect { pos: point + offset, size });
                }
                let text = self.share_label.layout(cx, 0., 0., None, false, Align::default(), &ghost.name);
                let size = dvec2(text.size_in_lpxs.width as f64, text.size_in_lpxs.height as f64)
                    * self.share_label.font_scale as f64 + dvec2(6., 2.);
                let pos = dvec2((point.x + 7.).min(clip.pos.x + clip.size.x - size.x).max(clip.pos.x),
                    (point.y + 7.).min(clip.pos.y + clip.size.y - size.y).max(clip.pos.y));
                self.outline.draw_abs(cx, Rect { pos, size });
                let c = ghost.colour;
                self.share_label.color = if 0.2126*c.x + 0.7152*c.y + 0.0722*c.z > 0.52 {
                    vec4(0., 0., 0., 1.)
                } else { vec4(1., 1., 1., 1.) };
                self.share_label.draw_walk_laidout(cx, Walk::abs_rect(Rect { pos: pos + dvec2(3., 1.), size }), &text);
            }
            ghost.drew(start);
        }
        cx.pop_clip_rect();
        (self.outline.color, self.share_label.color) = previous;
    }

    /// Refresh on a presence/window change. The overlay can repaint without rebuilding the board recording.
    pub fn set_app_presence(&mut self, cx: &mut Cx, item: Id, window: usize, size: [f64; 2], view: &crate::tweak::SeatView, state: &State) {
        let next = Selections::new(window, size, view, state);
        if self.app_presence.get(&item) != Some(&next) {
            self.app_presence.insert(item, next);
            self.request_overlay(cx);
        }
    }

    pub fn clear_app_presence(&mut self, cx: &mut Cx, item: Id) {
        if self.app_presence.remove(&item).is_some() { self.request_overlay(cx); }
    }

    /// A click on the displayed share uses the same letterbox transform as the selection overlay.
    pub fn app_pick_at(&self, item: Id, screen: DVec2) -> Option<crate::tweak::At> {
        let presence = self.app_presence.get(&item)?;
        let point = overlay::board_window_point(self.app_view_rect(item)?, presence.window_size, screen)?;
        overlay::pick_at(presence.window, point, presence.window_size)
    }

    fn app_view_rect(&self, id: Id) -> Option<Rect> {
        let item = self.scene.as_ref()?.view.get(&id)?;
        let r = rect(self.gesture.preview.as_ref().and_then(|p| p.rect(&id)).unwrap_or(item.rect));
        Some(Rect { pos: self.camera.local_to_screen(self.camera.world_to_local(r.pos)), size: r.size * self.camera.scale })
    }

    fn draw_app_selections(&mut self, cx: &mut Cx2d, clip: Rect) {
        cx.push_clip_rect(clip);
        for (id, presence) in &self.app_presence {
            if let Some(view) = self.app_view_rect(*id).filter(|r| r.intersects(clip)) {
                overlay::draw_selections(cx, view, presence, &mut self.outline, &mut self.share_label);
            }
        }
        cx.pop_clip_rect();
    }

    fn draw_outlines(&mut self, cx: &mut Cx2d, clip: Rect) {
        let look = self.look();
        let accent = self.board_selection;
        self.outline.color = accent;
        let scene = self.scene.as_ref();
        let g = &self.gesture;
        let camera = self.camera;
        let dpi = cx.current_dpi_factor();
        cx.push_clip_rect(clip);
        // A rounded ring a little outside each selected or hovered item, and under the pointer an
        // object rises: a wider shadow outside its outline. Neither moves anything.
        for &id in g.selection.ids.iter().chain(g.hover.iter().filter(|id| !g.selection.ids.contains(*id))) {
            let Some(scene) = scene else { break };
            let (Some(item), Some(face)) = (scene.view.get(&id), scene.faces.get(&id)) else { continue };
            let selected = g.selection.ids.contains(&id);
            let r = rect(g.preview.as_ref().and_then(|p| p.rect(&id)).unwrap_or(item.rect));
            let body = body_of(&item.kind, &face.shape);
            let s = item_surface(item, body, item_colour(item, &look), r.size, &look);
            let k = camera.scale as f32;
            let at = Rect { pos: camera.local_to_screen(camera.world_to_local(r.pos)), size: r.size * camera.scale };
            if g.preview.is_none() && g.hover.iter().any(|h| *h == id) {
                if let Some(l) = look::lift(body) {
                    let up = l.raised(1.6, 1.6);
                    let up = Lift { blur: up.blur * k, offset: up.offset * k, alpha: up.alpha * 0.6, contact: 0., ..up };
                    cast(&mut self.shadow, cx, at, &Surface { radius: s.radius * k, ..s }, up, &look, 0.);
                }
            }
            let pen = if selected { 2. } else { 1.5 };
            let reach = 3. + pen;
            let ring = Surface {
                radius: s.radius * k + reach as f32,
                border: pen as f32,
                border_color: if selected { accent } else { vec4(accent.x, accent.y, accent.z, accent.w * 0.55) },
                ..Surface::FLAT
            };
            let pos = snap_to_pixel(at.pos - dvec2(reach, reach), dpi);
            card(&mut self.card, cx, Rect { pos, size: at.size + dvec2(2. * reach, 2. * reach) }, vec4(0., 0., 0., 0.), &ring);
            if item.locked {
                draw_lock(cx, &mut self.outline, self.grid.color_a, dvec2(at.pos.x + at.size.x, at.pos.y), None);
            }
        }
        let mut outline = |r: Bounds, weight: f64, locked: bool, marquee: bool| {
            let point = |x, y| snap_to_pixel(camera.local_to_screen(camera.world_to_local(dvec2(x, y))), dpi);
            let top_left = point(r.x0, r.y0);
            let bottom_right = point(r.x1, r.y1);
            let pen = if marquee { 1. / dpi } else { (weight * dpi).round().max(1.) / dpi };
            let inset = if marquee { 0. } else { (2. * dpi).round() / dpi + pen };
            let a = top_left - dvec2(inset, inset);
            let b = bottom_right + dvec2(inset, inset);
            if b.x < clip.pos.x || b.y < clip.pos.y || a.x > clip.pos.x + clip.size.x || a.y > clip.pos.y + clip.size.y {
                return;
            }
            if marquee {
                let colour = self.outline.color;
                self.outline.color.w *= 0.12;
                self.outline.draw_abs(cx, Rect { pos: a, size: b - a });
                self.outline.color = colour;
            }
            let mut bar = |x, y, w, h| {
                self.outline.draw_abs(cx, Rect { pos: dvec2(x, y), size: dvec2(w, h) });
            };
            bar(a.x, a.y, b.x - a.x, pen);
            bar(a.x, b.y - pen, b.x - a.x, pen);
            bar(a.x, a.y, pen, b.y - a.y);
            bar(b.x - pen, a.y, pen, b.y - a.y);
            if locked {
                draw_lock(cx, &mut self.outline, self.grid.color_a, dvec2(bottom_right.x, top_left.y), None);
            }
        };
        if let Some(preview) = &g.preview {
            for id in preview.pinned.difference(&g.selection.ids) {
                if let Some(item) = scene.and_then(|s| s.view.get(id)) {
                    outline(item.rect, 1., true, false);
                }
            }
        }
        let bounds = scene.and_then(|s| g.handles(&s.view));
        if g.selection.ids.len() >= 2 {
            if let Some(r) = bounds { outline(r, 1., false, false); }
        }
        if let Some(r) = g.marquee() {
            outline(r, 1., false, true);
        }
        if let Some(ghost) = &self.ghost {
            if let Some(section) = ghost.item.parent.and_then(|id| scene?.view.get(&id)) {
                outline(section.rect, 2., false, false);
            }
            outline(ghost.rect, 1., false, true);
        }
        let screen = |p| snap_to_pixel(camera.local_to_screen(camera.world_to_local(p)), dpi);
        let mut draw_segment = |(a, b)| {
            let pos = screen(a);
            let size = screen(b) - pos;
            self.outline.draw_abs(cx, Rect { pos, size: dvec2(size.x.max(1.), size.y.max(1.)) });
        };
        if let Some(ghost) = &self.ghost {
            for (axis, guide) in ghost.guides.iter().enumerate() {
                if let Some(g) = guide { draw_segment(g.segment(axis)); }
            }
        } else {
            g.snap_segments().for_each(draw_segment);
        }
        if let Some(b) = bounds {
            let knob = Surface { radius: 2.5, border: 1.5, border_color: accent, ..Surface::FLAT };
            for handle in Handle::ALL {
                let pos = screen(handle.point(b, camera.scale)) - dvec2(4.5, 4.5);
                card(&mut self.card, cx, Rect { pos, size: dvec2(9., 9.) }, look.ink, &knob);
            }
        }
        cx.pop_clip_rect();
    }

    /// Whether the board is being presented.
    pub fn presenting(&self) -> bool {
        self.present.is_some()
    }

    /// Present the board along its path, starting from the first step that can be shown. With
    /// no such step it presents the selection, else the whole board. Esc ends it and puts the
    /// camera back. Returns whether it is presenting.
    pub fn present(&mut self, cx: &mut Cx) -> bool {
        if self.present.is_some() {
            return true;
        }
        let Some(scene) = &self.scene else { return false };
        let targets = scene.present_targets(&self.gesture.selection.ids);
        let now = makepad_platform::monotonic_seconds();
        let mut present = Present::start(self.camera);
        if !present.next(&targets, now) {
            return false;
        }
        self.finish_text(cx, true);
        self.placement_start = None;
        self.ghost = None;
        self.toolbar_drag = None;
        self.gesture.cancel();
        self.end_live(cx, false);
        self.present = Some(present);
        self.overlay_pending = true;
        cx.set_key_focus_from_user(self.area);
        self.glide_frame(cx, now);
        true
    }

    /// Stop presenting: the camera goes back to where it was before.
    pub fn end_present(&mut self, cx: &mut Cx) {
        if let Some(present) = self.present.take() {
            let view = self.camera.view;
            self.camera = present.end();
            self.camera.view = view;
            self.overlay_pending = true;
            self.request(cx);
        }
    }

    fn present_move(&mut self, cx: &mut Cx, forward: bool) {
        let (Some(scene), Some(present)) = (&self.scene, &mut self.present) else { return };
        let targets = scene.present_targets(&self.gesture.selection.ids);
        let now = makepad_platform::monotonic_seconds();
        if if forward { present.next(&targets, now) } else { present.previous(&targets, now) } {
            self.glide_frame(cx, now);
        }
    }

    // The presented camera at `now`, with frames requested while it glides.
    fn glide_frame(&mut self, cx: &mut Cx, now: f64) {
        let Some(present) = &self.present else { return };
        self.camera = present.camera(now);
        if present.gliding(now) {
            self.next_frame = cx.new_next_frame();
        }
        self.request(cx);
    }

    // While presenting, the board takes only the keys and clicks that move along the path.
    fn present_input(&mut self, cx: &mut Cx, event: &Event) {
        match event.hits(cx, self.area) {
            Hit::KeyDown(k) if !k.modifiers.logo && !k.modifiers.control && !k.modifiers.alt => match k.key_code {
                KeyCode::ArrowRight | KeyCode::ArrowDown | KeyCode::PageDown | KeyCode::Space | KeyCode::ReturnKey => {
                    self.present_move(cx, true)
                }
                KeyCode::ArrowLeft | KeyCode::ArrowUp | KeyCode::PageUp | KeyCode::Backspace => self.present_move(cx, false),
                KeyCode::Escape => self.end_present(cx),
                _ => {}
            },
            Hit::FingerDown(d) if d.is_primary_hit() => self.present_move(cx, true),
            _ => {}
        }
    }

    fn input(&mut self, cx: &mut Cx, event: &Event) {
        // Space lifts the hand wherever the key focus is, so a later drag never pans by mistake.
        if matches!(event, Event::KeyUp(k) if k.key_code == KeyCode::Space) && self.gesture.space {
            self.gesture.space = false;
            cx.set_cursor(MouseCursor::Default);
        }
        let camera = self.camera;
        self.drag_event(cx, event);
        if camera.scale <= 0. {
            return;
        }
        let could_align = self.can_align();
        let had_preview = self.gesture.preview.is_some();
        let mut action = None;
        let mut redraw = false;
        let mut cursor = false;
        if let Some(mapped) = remap_event(event, &camera) {
            // A release elsewhere breaks the pair too. The platform supplies the
            // click count and timing; the gesture tells us which item was clicked.
            let previous_click = match event {
                Event::MouseUp(_) => self.last_click.take(),
                Event::TouchUpdate(e) if e.touches.iter().any(|t| matches!(t.state, TouchState::Stop | TouchState::Cancel)) => {
                    self.last_click.take()
                }
                _ => None,
            };
            let hit = mapped.hits(cx, self.local_area);
            sync_handled(event, &mapped, &camera);
            let scene = self.scene.as_ref();
            let world = |point| camera.world_at(camera.local_to_screen(point));
            match hit {
                Hit::FingerDown(d) if d.is_primary_hit() || d.mouse_button().is_some_and(|b| b.is_middle() || b.is_secondary()) => {
                    cx.set_key_focus_from_user(self.area);
                    let space = cx.keyboard.is_key_down(KeyCode::Space) || self.gesture.space;
                    if self.tool.places() && !space && d.is_primary_hit() {
                        let screen = camera.local_to_screen(d.abs);
                        // Nothing is drawn on press: click or drag is decided by the first
                        // move past the slop, or by the release.
                        self.placement_start = Some(camera.world_at(screen));
                        self.placement_dragged = false;
                        self.ghost = None;
                        redraw = true;
                    } else if let Some(scene) = scene {
                        let p = world(d.abs);
                        self.gesture.set_viewport(&scene.view, camera_bounds(camera));
                        let id = scene.pick(p);
                        let interior = id.and_then(|id| scene.view.get(&id)).is_some_and(|i| {
                            let b = i.rect;
                            let edge = 5. / camera.scale;
                            i.kind == kind::SECTION && p.x > b.x0 + edge && p.x < b.x1 - edge
                                && p.y > b.y0 + 20. / camera.scale && p.y < b.y1 - edge
                        });
                        let pan = space || self.tool == BoardTool::Hand
                            || d.mouse_button().is_some_and(|b| b.is_middle() || b.is_secondary());
                        let screen = camera.local_to_screen(d.abs);
                        let handle = self.gesture.hit_handle(&scene.view, p, camera.scale);
                        if let Some(handle) = handle.filter(|_| !pan) {
                            self.gesture.press_handle(&scene.view, handle, p);
                            self.drag_pointer = Some((screen, d.modifiers));
                        } else {
                            self.gesture.handle = None;
                            self.gesture.press(&scene.view, id, p, screen, d.modifiers.shift, pan.then_some(camera.pan), interior);
                        }
                        redraw = true;
                        cursor = true;
                    }
                }
                Hit::FingerMove(m) if self.placement_start.is_some() => {
                    let screen = camera.local_to_screen(m.abs);
                    if self.placement_start.is_some_and(|a| (camera.world_at(screen) - a).length() * camera.scale >= DRAG_SLOP) {
                        self.placement_dragged = true;
                    }
                    if self.placement_dragged { self.place_at(self.tool, screen, m.modifiers.alt); }
                    redraw = true;
                }
                Hit::FingerUp(u) if self.placement_start.is_some() => {
                    if !u.cancelled && !u.is_sweep && u.is_over {
                        self.place_at(self.tool, camera.local_to_screen(u.abs), u.modifiers.alt);
                        self.finish_placement(cx);
                    }
                    self.placement_start = None;
                    self.ghost = None;
                    redraw = true;
                }
                Hit::FingerMove(m) if self.gesture.pointer_active() => {
                    if let Some(scene) = scene {
                        let screen = camera.local_to_screen(m.abs);
                        self.drag_pointer = Some((screen, m.modifiers));
                        let pan = self.gesture.motion(&scene.view, world(m.abs), screen,
                            camera.scale, m.modifiers.alt, m.modifiers.shift);
                        if let Some(pan) = pan {
                            self.pan(cx, pan);
                        }
                        redraw = true;
                        cursor = true;
                    }
                }
                Hit::FingerUp(u) if self.gesture.pointer_active() => {
                    if u.cancelled || u.is_sweep {
                        self.gesture.cancel();
                    } else if let Some(scene) = scene {
                        let pan = self.gesture.motion(&scene.view, world(u.abs), camera.local_to_screen(u.abs),
                            camera.scale, u.modifiers.alt, u.modifiers.shift);
                        // motion already checked the drag distance in screen pixels.
                        let clicked = self.gesture.clicked_item()
                            .filter(|_| !u.has_long_press_occurred && u.time - u.capture_time < TAP_COUNT_TIME);
                        // A release may be the event that first crosses the drag threshold.
                        if let Some(preview) = self.gesture.moving() {
                            self.live_out.moving(cx, scene.id, preview, world(u.abs));
                            self.drag_pointer = Some((camera.local_to_screen(u.abs), u.modifiers));
                        }
                        action = self.gesture.release(&scene.view).map(BoardCanvasAction::Act);
                        self.gesture.handle = if u.is_over {
                            self.gesture.hit_handle(&scene.view, world(u.abs), camera.scale)
                        } else { None };
                        self.gesture.hover = if u.is_over { scene.pick(world(u.abs)) } else { None };
                        if let Some(id) = clicked.filter(|id| self.gesture.hover == Some(*id)) {
                            if let Some(at) = self.app_pick_at(id, camera.local_to_screen(u.abs)) {
                                cx.widget_action(self.widget_uid(), AppPick { item: id, at });
                            } else if u.tap_count >= 2 && previous_click == Some(id) {
                                self.open_text(cx, id, false);
                            } else {
                                self.last_click = Some(id);
                            }
                        }
                        if let Some(pan) = pan {
                            self.pan(cx, pan);
                        }
                    }
                    cursor = true;
                    redraw = true;
                }
                Hit::FingerHoverIn(h) | Hit::FingerHoverOver(h) => {
                    let id = scene.and_then(|s| s.pick(world(h.abs)));
                    let handle = scene.and_then(|s| self.gesture.hit_handle(&s.view, world(h.abs), camera.scale));
                    redraw = self.gesture.hover != id || self.gesture.handle != handle;
                    self.gesture.handle = handle;
                    self.gesture.hover = id;
                    self.gesture.space = cx.keyboard.is_key_down(KeyCode::Space);
                    cursor = true;
                }
                Hit::FingerHoverOut(_) => {
                    self.gesture.handle = None;
                    redraw = self.gesture.hover.take().is_some();
                }
                Hit::FingerScroll(s) => {
                    if let Some((_, mods)) = &mut self.drag_pointer { *mods = s.modifiers; }
                    // A wheel reports neither a precise delta nor a gesture phase.
                    let wheel = match event {
                        Event::Scroll(e) => e.is_mouse || e.phase == ScrollPhase::None,
                        _ => true,
                    };
                    match scroll_nav(s.scroll * camera.scale, wheel, &s.modifiers) {
                        ScrollNav::Zoom(by) => self.zoom(cx, camera.local_to_screen(s.abs), camera.scale * by),
                        ScrollNav::Pan(by) => self.pan(cx, camera.pan + by),
                    }
                    if let Event::Scroll(e) = &mapped {
                        e.handled_x.set(true);
                        e.handled_y.set(true);
                    }
                    sync_handled(event, &mapped, &camera);
                }
                _ => {}
            }
        } else if self.placement_start.is_some() && matches!(event, Event::KeyDown(k) if k.key_code == KeyCode::Escape) {
            self.arm(cx, BoardTool::Select);
        } else if matches!(event, Event::WindowLostFocus(_)) {
            if let Some(scene) = &self.scene {
                redraw = self.gesture.pointer_active() || had_preview;
                action = self.gesture.focus_lost(&scene.view).map(BoardCanvasAction::Act);
            }
        } else {
            match event.hits(cx, self.area) {
                Hit::FingerPinch(p) => self.zoom(cx, p.abs, camera.scale * p.scale),
                Hit::KeyFocusLost(_) => {
                    if self.placement_start.take().is_some() {
                        self.ghost = None;
                        redraw = true;
                    }
                    if let Some(s) = &self.scene {
                        redraw = self.gesture.pointer_active() || had_preview;
                        action = self.gesture.focus_lost(&s.view).map(BoardCanvasAction::Act);
                    }
                }
                Hit::KeyUp(k) => {
                    if let Some(scene) = &self.scene {
                        action = self.gesture.key_up(&scene.view, k.key_code).map(BoardCanvasAction::Act);
                    }
                    if k.key_code == KeyCode::Space {
                        self.gesture.space = false;
                        cursor = true;
                    }
                }
                Hit::KeyDown(k) => {
                    let command = k.modifiers.logo || k.modifiers.control;
                    match k.key_code {
                        KeyCode::ReturnKey | KeyCode::NumpadEnter if !k.is_repeat && !k.modifiers.any()
                            && !had_preview && !self.gesture.pointer_active() && self.placement_start.is_none()
                            && self.gesture.selection.ids.len() == 1 => {
                            if let Some(&id) = self.gesture.selection.ids.first() { self.open_text(cx, id, false); }
                        }
                        KeyCode::Space => {
                            self.gesture.space = true;
                            cursor = true;
                        }
                        KeyCode::KeyH if !k.modifiers.any() && !k.is_repeat => {
                            self.arm(cx, if self.tool == BoardTool::Hand { BoardTool::Select } else { BoardTool::Hand });
                            cursor = true;
                        }
                        KeyCode::KeyF if !k.modifiers.any() => self.fit_board(cx),
                        KeyCode::Key1 if k.modifiers.shift && !command => self.fit_board(cx),
                        KeyCode::F5 if !k.is_repeat && !had_preview && !self.gesture.pointer_active() => {
                            self.present(cx);
                        }
                        KeyCode::Key0 if command => self.zoom(cx, camera.view.center(), 1.),
                        KeyCode::Key1 if !k.modifiers.any() => self.zoom(cx, camera.view.center(), 1.),
                        KeyCode::Equals | KeyCode::NumpadAdd => self.zoom_step(cx, ZOOM_STEP),
                        KeyCode::Minus | KeyCode::NumpadSubtract => self.zoom_step(cx, 1. / ZOOM_STEP),
                        KeyCode::KeyZ if command && !k.is_repeat && !had_preview => {
                            action = Some(if k.modifiers.shift { BoardCanvasAction::Redo } else { BoardCanvasAction::Undo });
                        }
                        KeyCode::Escape if self.tool != BoardTool::Select => self.arm(cx, BoardTool::Select),
                        KeyCode::Escape => {
                            self.gesture.cancel();
                            redraw = true;
                            cursor = true;
                        }
                        key => {
                            if let Some(scene) = &self.scene {
                                let g = &mut self.gesture;
                                match key {
                                    KeyCode::ArrowUp | KeyCode::ArrowDown | KeyCode::ArrowLeft | KeyCode::ArrowRight => {
                                        g.nudge(&scene.view, key, k.modifiers.shift);
                                        redraw = true;
                                    }
                                    KeyCode::LBracket | KeyCode::RBracket if command && !had_preview => {
                                        let to = match (key, k.modifiers.shift) {
                                            (KeyCode::LBracket, false) => RestackTo::Backward,
                                            (KeyCode::LBracket, true) => RestackTo::Back,
                                            (_, false) => RestackTo::Forward,
                                            (_, true) => RestackTo::Front,
                                        };
                                        if !g.selection.ids.is_empty() {
                                            let items = g.selection.ids.iter().copied().collect();
                                            action = Some(BoardCanvasAction::Act(ModelAction::Restack { items, to }));
                                        }
                                    }
                                    key if k.modifiers.alt && !command && !had_preview => {
                                        let to = match key {
                                            KeyCode::KeyA => Some(AlignTo::Left), KeyCode::KeyH => Some(AlignTo::Centre),
                                            KeyCode::KeyD => Some(AlignTo::Right), KeyCode::KeyW => Some(AlignTo::Top),
                                            KeyCode::KeyV => Some(AlignTo::Middle), KeyCode::KeyS => Some(AlignTo::Bottom),
                                            _ => None,
                                        };
                                        action = to.and_then(|to| g.align(&scene.view, to)).map(BoardCanvasAction::Act);
                                    }
                                    KeyCode::KeyA if command && !g.pointer_active() => {
                                        g.select_all(&scene.view);
                                        redraw = true;
                                    }
                                    KeyCode::KeyL if command && k.modifiers.shift && !k.is_repeat && !had_preview => {
                                        action = g.lock(&scene.view).map(BoardCanvasAction::Act);
                                    }
                                    KeyCode::Delete | KeyCode::Backspace if !k.is_repeat && !had_preview => {
                                        action = g.delete(&scene.view).map(BoardCanvasAction::Act);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if let Event::KeyDown(k) | Event::KeyUp(k) = event {
            if let Some((_, mods)) = &mut self.drag_pointer {
                if mods.alt != k.modifiers.alt || mods.shift != k.modifiers.shift {
                    *mods = k.modifiers;
                    self.refresh_drag();
                    redraw = true;
                }
            }
        }
        if self.placement_start.is_some() && (!cx.fingers.is_area_captured(self.local_area)
            || matches!(event, Event::WindowLostFocus(_))) {
            self.placement_start = None;
            self.ghost = None;
            redraw = true;
        }
        if self.gesture.pointer_active() && !cx.fingers.is_area_captured(self.local_area) {
            self.gesture.cancel();
            redraw = true;
            cursor = true;
        }
        if cursor && self.tool.places() && !self.gesture.space {
            cx.set_cursor(MouseCursor::Crosshair);
        } else if cursor {
            if let Some(scene) = &self.scene {
                cx.set_cursor(self.gesture.cursor(&scene.view, self.tool == BoardTool::Hand));
            }
        }
        self.update_live(cx, matches!(action, Some(BoardCanvasAction::Act(_))));
        if had_preview != self.gesture.preview.is_some() {
            if let Some(scene) = &mut self.scene {
                scene.lift(self.gesture.preview.as_ref());
            }
            self.request(cx);
        } else if redraw {
            self.request_overlay(cx);
        }
        if could_align != self.can_align() {
            cx.widget_action(self.widget_uid(), BoardCanvasAction::SelectionChanged);
        }
        if let Some(action) = action {
            cx.widget_action(self.widget_uid(), action);
        }
    }
}

impl Widget for BoardCanvas {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let started = makepad_platform::monotonic_seconds();
        // An unrelated change can redraw the shell containing our hit area.
        // Reattach the board recording unless its geometry, data or glyphs changed.
        let recording = self.recording.get_or_insert_with(|| DrawList2d::new(cx));
        let changed = cx.will_redraw(recording, walk) || self.repaint_pending;
        let view = cx.walk_turtle_with_area(&mut self.area, walk);
        let clip = self.area.clipped_rect(cx);
        if clip.size.x <= 0. || clip.size.y <= 0. {
            return DrawStep::done();
        }
        if !recording.begin_maybe(cx, changed).is_not_redrawing() {
            self.repaint_pending = false;
            let previous = self.camera;
            self.camera.view = view;
            if let Some(present) = &mut self.present {
                // The window may have resized, or the shown step's target moved.
                let now = makepad_platform::monotonic_seconds();
                let targets = self.scene.as_ref().map(|s| s.present_targets(&self.gesture.selection.ids)).unwrap_or_default();
                present.refit(&targets, view, now);
                self.camera = present.camera(now);
                if present.gliding(now) {
                    self.next_frame = cx.new_next_frame();
                }
            } else if self.camera.scale == 0. || self.fit_pending {
                self.camera = self
                    .fit(view)
                    .unwrap_or_else(|| Camera::new(view, CameraState { pan_x: 0., pan_y: 0., zoom: 1. }));
                self.fit_pending = false;
            }
            let camera = self.camera;
            if previous.view != camera.view || previous.scale != camera.scale || previous.pan != camera.pan {
                self.refresh_drag();
                self.update_live(cx, false);
            }
            // Keep the dots legible as the camera crosses a zoom octave.
            let grid_cell = 18. * 2f64.powf((1. / camera.scale).log2().round()) * camera.scale;
            self.grid.cell = grid_cell as f32;
            self.grid.origin = vec2(
                ((view.pos.x + camera.pan.x).rem_euclid(grid_cell)) as f32,
                ((view.pos.y + camera.pan.y).rem_euclid(grid_cell)) as f32,
            );
            self.grid.draw_abs(cx, clip);
            let mut layer = self.layer.take().unwrap_or_else(|| CanvasLayer::new(cx));
            let scope = layer.begin(cx, &camera, clip);
            let waiting = self.draw_board(cx, camera, clip, false);
            scope.end(cx);
            self.layer = Some(layer);
            self.recording.as_mut().unwrap().end(cx);
            let elapsed = makepad_platform::monotonic_seconds() - started;
            let load = elapsed.max(cx.window_frame_gpu_time().unwrap_or_default()) / cx.display_frame_interval();
            // The cost applies from the next frame on.
            let (next, again) = pressure_after(self.pressure, load, self.last_load);
            if next > self.pressure.max(1.) {
                log!("board draw exceeded its frame budget; detail threshold scale {:.2}", next);
            }
            self.pressure = next;
            self.last_load = load;
            // Text still waiting for its fit gets it over the next frames.
            if again || waiting {
                self.repaint_pending = true;
                cx.redraw_area_in_draw(self.area);
            }
        }
        let overlay = self.overlay.get_or_insert_with(|| DrawList2d::new(cx));
        if !overlay.begin_maybe(cx, changed || self.overlay_pending).is_not_redrawing() {
            self.overlay_pending = false;
            if self.gesture.preview.is_some() {
                let mut layer = self.preview_layer.take().unwrap_or_else(|| CanvasLayer::new(cx));
                let scope = layer.begin(cx, &self.camera, clip);
                if self.draw_board(cx, self.camera, clip, true) {
                    self.overlay_pending = true;
                    cx.redraw_area_in_draw(self.area);
                }
                scope.end(cx);
                self.preview_layer = Some(layer);
            }
            self.draw_app_selections(cx, clip);
            self.draw_live(cx, clip);
            if self.present.is_none() {
                self.draw_outlines(cx, clip);
            }
            self.overlay.as_mut().unwrap().end(cx);
        }
        self.draw_editor(cx, scope, clip);
        self.draw_nav(cx, scope, clip);
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.next_frame.is_event(event).is_some() {
            self.glide_frame(cx, makepad_platform::monotonic_seconds());
        }
        if self.present.is_some() {
            self.present_input(cx, event);
        } else if !self.nav_event(cx, event, scope) && !self.editor_event(cx, event, scope) {
            self.input(cx, event);
        }
    }
}

#[cfg(test)]
#[path = "canvas_tests.rs"]
mod tests;

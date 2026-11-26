//! The board and its local interaction overlay. Fold events materialize text and shape handles alongside
//! the spatial view. Drawing only borrows this projection and the camera.
use super::gesture::{AlignTo, BoardTool, Gesture, Handle, NewItem, Placement, Preview, Transform, DRAG_SLOP};
use super::{BoardItem, BoardView, Rect as Bounds, RectIndex};
use crate::model::{Action as ModelAction, ItemStyle, RestackTo};
use makepad_canvas::{
    remap_event, sync_handled, Camera, CameraState, CanvasLayer, DrawCanvasCard, DrawCanvasGrid, MAX_ZOOM,
    MIN_ZOOM,
};
use makepad_work_core::entity::{field, kind};
use makepad_work_core::event::{Id, Key, Value};
use makepad_work_core::fold::State;
use makepad_widgets::{
    drag_visual::CxDragVisualExt,
    event::{EditModifier, TouchState, TAP_COUNT_TIME},
    text::{font_family::FontFamilyId, layouter::LaidoutText},
    *,
};
use std::{collections::HashMap, ops::Range, rc::Rc};

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*
    mod.widgets.BoardCanvas = #(BoardCanvas::register_widget(vm)){
        width: Fill height: Fill
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
        card +: {border_size: 0.0 shadow_offset: vec2(0.0, 0.0)}
        label +: {text_style: theme.font_regular{font_size: 10.5}}
    }
}

const SHADOW: f64 = 4.0;
const PADDING: f64 = 4.0;

fn rect(b: Bounds) -> Rect {
    Rect { pos: dvec2(b.x0, b.y0), size: dvec2(b.x1 - b.x0, b.y1 - b.y0) }
}

fn total_bounds() -> Bounds {
    Bounds::new(f64::MIN, f64::MIN, f64::MAX, f64::MAX)
}

fn painted(item: &BoardItem) -> Bounds {
    let b = item.rect;
    let pad = match item.kind {
        Key::Number(kind::STICKY) => SHADOW,
        Key::Number(kind::SHAPE) => 1.0,
        _ => 0.0,
    };
    Bounds::new(b.x0 - pad, b.y0 - pad, b.x1 + pad, b.y1 + pad)
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
                            makepad_work_core::hex(&e.id));
                    }
                }
            }
        }
        let mut text = e.text(field::TEXT);
        if text.is_empty() && e.kind == kind::SECTION {
            text = e.text(field::TITLE);
        }
        let fallback;
        if let Key::Name(name) = &item.kind {
            fallback = format!("unknown kind {name}");
            text = &fallback;
        }
        let changed = self.text != text || self.shape != e.text(field::SHAPE);
        if self.text != text {
            text.clone_into(&mut self.text);
            self.layout = None;
        }
        let shape = e.text(field::SHAPE);
        if self.shape != shape {
            shape.clone_into(&mut self.shape);
        }
        changed
    }

    fn layout(&mut self, cx: &mut Cx2d, label: &DrawText, width: f32) -> &LaidoutText {
        let style = &label.text_style;
        let key = MetricsKey {
            width,
            font: style.font_family_id(),
            size: style.font_size,
            spacing: style.line_spacing,
            family_generation: cx.fonts.borrow().font_family_generation(),
        };
        if self.layout.as_ref().is_none_or(|(old, _)| old != &key) {
            let layout = label.layout(cx, 0., 0., Some(width), true, Align::default(), &self.text);
            self.layout = Some((key, layout));
        }
        &self.layout.as_ref().unwrap().1
    }
}

struct Scene {
    id: Id,
    view: BoardView,
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
                "ellipse" | "circle" => x * x + y * y <= 1.,
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
    outline: DrawColor,
    #[live]
    vector: DrawVector,
    #[live]
    label: DrawText,
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
        self.finish_text(cx, true);
        self.last_click = None;
        self.scene = selected.map(|id| Scene::new(state, id));
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
        let Some(scene) = &mut self.scene else { return };
        let was_empty = scene.faces.is_empty();
        if scene.apply(state, ids, &mut self.gesture) {
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
    }

    // The link currently publishes one refolded State for a batch, without event ids.
    // Reconcile only when that batch arrives. The input and draw paths keep their index.
    pub fn peer_snapshot(&mut self, cx: &mut Cx, state: &State, shown: bool) {
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
        if changed.is_empty() && ids == previous {
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
    }

    fn fit(&self, view: Rect) -> Option<Camera> {
        let mut world = self.scene.as_ref()?.bounds()?;
        world.size = dvec2(world.size.x.max(1.), world.size.y.max(1.));
        let insets = Inset { left: 16., right: 16., top: 56., bottom: 16. };
        let fitted = Camera::fit(view, world, insets)?;
        fitted
            .zoom_about(fitted.local_to_screen(fitted.world_to_local(world.center())), fitted.scale.min(1.))
    }

    fn change_camera(&mut self, cx: &mut Cx, next: Camera) {
        if next.scale == self.camera.scale && next.pan == self.camera.pan {
            return;
        }
        self.camera = next;
        self.fit_pending = false;
        self.request(cx);
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

fn item_colour(item: &BoardItem) -> Vec4f {
    let n = item.colour.unwrap_or(match item.kind {
        Key::Number(kind::STICKY) => 0xf1d996,
        Key::Number(kind::TEXT_BLOCK) => 0xe3e5e8,
        Key::Number(kind::SECTION) => 0x435966,
        _ => 0x93b6c9,
    });
    Vec4f::from_u32(if n > 0xffffff { n } else { (n << 8) | 255 })
}

fn text_ink(item: &BoardItem) -> Vec4f {
    let colour = item_colour(item);
    if item.kind == kind::TEXT_BLOCK {
        colour
    } else if item.kind == kind::SECTION
        || colour.x * 0.299 + colour.y * 0.587 + colour.z * 0.114 < 0.48
    {
        vec4(1., 1., 1., 1.)
    } else {
        vec4(0.09, 0.1, 0.12, 1.)
    }
}

fn local_rect(camera: &Camera, b: Bounds) -> Rect {
    let r = rect(b);
    Rect { pos: camera.world_to_local(r.pos), ..r }
}

fn supported(kind: &Key) -> bool {
    matches!(kind, Key::Name(_) | Key::Number(kind::STICKY | kind::SHAPE | kind::SECTION | kind::TEXT_BLOCK))
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

fn card(draw: &mut DrawCanvasCard, cx: &mut Cx2d, r: Rect, colour: Vec4f, shadow: bool) {
    draw.color = colour;
    draw.border_radius = if shadow { 3. } else { 0. };
    draw.shadow_radius = if shadow { SHADOW as f32 } else { 0. };
    draw.shadow_color = vec4(0., 0., 0., if shadow { 0.32 } else { 0. });
    draw.draw_abs(cx, r);
}

impl BoardCanvas {
    fn draw_board(&mut self, cx: &mut Cx2d, camera: Camera, clip: Rect, lifted: bool) {
        let a = camera.world_at(clip.pos);
        let b = camera.world_at(clip.pos + clip.size);
        let bounds = Bounds::new(a.x, a.y, b.x, b.y);
        let local = local_rect(&camera, bounds);
        if !lifted {
            cx.add_aligned_rect_area(&mut self.local_area, local);
        }
        let Some(scene) = &mut self.scene else { return };
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
        for id in ids {
            let item = scene.view.get(id).unwrap();
            if !supported(&item.kind) {
                continue;
            }
            if lifted && !geometry(item).intersects(bounds) {
                continue;
            }
            let size = rect(geometry(item)).size;
            let face = scene.faces.get_mut(id).unwrap();
            let d = &mut face.detail;
            // At the font's native pixel size a person's words must remain readable
            // after the editor closes. In an overview, cards can still lose detail.
            let label_is_content = item.kind == kind::TEXT_BLOCK || matches!(item.kind, Key::Name(_))
                || (pixels >= 1. && !face.text.is_empty());
            let detail_pressure = if label_is_content { 1. } else { pressure };
            d.body = readable(size.x.max(size.y) * pixels, 3. * detail_pressure.sqrt(), d.body);
            d.text = d.body
                && readable(self.label.text_style.font_size as f64 * 96. / 72. * pixels, 4. * detail_pressure, d.text);
            d.decoration = d.body && readable(size.x.min(size.y) * pixels, 24. * pressure, d.decoration);
        }
        for id in ids {
            let item = scene.view.get(id).unwrap();
            if !supported(&item.kind) || scene.faces[id].detail.body || !geometry(item).intersects(bounds) {
                continue;
            }
            if let Some(section) = item.parent_section.filter(|p| {
                scene.faces.get(p).is_some_and(|f| f.detail.body)
                    && scene.view.get(p).is_some_and(|parent| parent.rect.intersects(bounds))
            }) {
                sections.entry(section).or_default().add(item_colour(item));
            } else {
                let p = rect(geometry(item)).center();
                cells
                    .entry(((p.x / cell_size).floor() as i64, (p.y / cell_size).floor() as i64))
                    .or_default()
                    .add(item_colour(item));
            }
        }
        for ((x, y), tint) in cells {
            let mut c = tint.colour();
            c.w = (0.28 + 0.12 * (tint.count as f32).ln_1p()).min(0.9);
            let pos = camera.world_to_local(dvec2(x as f64 * cell_size, y as f64 * cell_size));
            card(&mut self.card, cx, Rect { pos, size: dvec2(cell_size, cell_size) }, c, false);
        }
        let mut locks = Vec::new();
        for layer in layers {
            // The draw append search cannot cross a different parent base. Quad
            // and text use the enclosing (penultimate) parent for their lanes.
            cx.push_draw_call_parent();
            cx.push_draw_call_parent();
            self.vector.begin();
            for id in &ids[layer.clone()] {
                let item = scene.view.get(id).unwrap();
                let face = &scene.faces[id];
                if !face.detail.body || !supported(&item.kind) || !geometry(item).intersects(bounds) {
                    continue;
                }
                let r = local_rect(&camera, geometry(item));
                let mut c = item_colour(item);
                let shape = if item.kind == kind::SHAPE { face.shape.as_str() } else { "" };
                if item.kind == kind::SECTION {
                    if let Some(tint) = sections.get(id) {
                        c = c * 0.6 + tint.colour() * 0.4;
                    }
                    c.w *= 0.45;
                }
                match shape {
                    "ellipse" | "circle" | "diamond" | "triangle" => {
                        let (x, y) = (r.pos.x as f32, r.pos.y as f32);
                        let (w, h) = (r.size.x as f32, r.size.y as f32);
                        self.vector.clear();
                        self.vector.set_color(c.x, c.y, c.z, c.w);
                        if matches!(shape, "ellipse" | "circle") {
                            self.vector.ellipse(x + w * 0.5, y + h * 0.5, w * 0.5, h * 0.5);
                        } else {
                            self.vector.move_to(x + w * 0.5, y);
                            if shape == "diamond" {
                                self.vector.line_to(x + w, y + h * 0.5);
                                self.vector.line_to(x + w * 0.5, y + h);
                                self.vector.line_to(x, y + h * 0.5);
                            } else {
                                self.vector.line_to(x + w, y + h);
                                self.vector.line_to(x, y + h);
                            }
                            self.vector.close();
                        }
                        self.vector.fill();
                    }
                    _ if item.kind != kind::TEXT_BLOCK || !face.detail.text => {
                        card(&mut self.card, cx, r, c, face.detail.decoration && item.kind == kind::STICKY);
                    }
                    _ => {}
                }
                if item.locked && face.detail.decoration {
                    locks.push(camera.local_to_screen(dvec2(r.pos.x + r.size.x, r.pos.y)));
                }
            }
            self.vector.end(cx);
            for corner in locks.drain(..) {
                draw_lock(cx, &mut self.outline, self.grid.color_a, corner, Some(&camera));
            }
            self.label.begin_deferred_slug_flush();
            for id in &ids[layer.clone()] {
                let item = scene.view.get(id).unwrap();
                let face = scene.faces.get_mut(id).unwrap();
                if self.editing.as_ref().is_some_and(|edit| edit.item == *id) { continue }
                if !supported(&item.kind) || !face.detail.text || face.text.is_empty() || !geometry(item).intersects(bounds) {
                    continue;
                }
                let r = local_rect(&camera, geometry(item));
                let padding = if item.kind == kind::TEXT_BLOCK { 0. } else { PADDING };
                let size = r.size - dvec2(2. * padding, 2. * padding);
                if size.x <= 0. || size.y <= 0. {
                    continue;
                }
                let inner = Rect { pos: r.pos + dvec2(padding, padding), size };
                self.label.color = text_ink(item);
                let laidout = face.layout(cx, &self.label, size.x as f32);
                cx.push_clip_rect(inner);
                self.label.draw_walk_laidout(cx, Walk::abs_rect(inner), laidout);
                cx.pop_clip_rect();
            }
            self.label.end_deferred_slug_flush(cx);
            cx.pop_draw_call_parent();
            cx.pop_draw_call_parent();
        }
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
        let Some(item) = self.scene.as_ref().and_then(|s| s.view.get(&edit.item)) else { return };
        let mut r = self.item_screen_rect(item);
        let pad = if item.kind == kind::TEXT_BLOCK { 0. } else { PADDING * self.camera.scale };
        r.pos += dvec2(pad, pad);
        r.size = dvec2((r.size.x - 2. * pad).max(1.), (r.size.y - 2. * pad).max(1.));
        if let Some(mut input) = self.editor.borrow_mut::<TextInput>() {
            input.set_font_size(cx, self.label.text_style.font_size * self.camera.scale as f32);
            // The caret takes the item's ink, so it shows on light and dark notes alike.
            let ink = text_ink(item);
            input.draw_text.color = ink;
            input.draw_cursor.draw_vars.set_uniform(cx, live_id!(color), &[ink.x, ink.y, ink.z, ink.w]);
        }
        cx.push_clip_rect(clip);
        self.editor.draw_walk_all(cx, scope, Walk::abs_rect(r));
        cx.pop_clip_rect();
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
        self.is_empty() && self.tool == BoardTool::Select && self.toolbar_drag.is_none()
    }

    pub fn drag_tool(&mut self, cx: &mut Cx, tool: BoardTool, toolbar: WidgetRef) {
        if self.toolbar_drag.is_some() || tool == BoardTool::Select { return }
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

    fn draw_outlines(&mut self, cx: &mut Cx2d, clip: Rect) {
        let scene = self.scene.as_ref();
        let g = &self.gesture;
        let camera = self.camera;
        let dpi = cx.current_dpi_factor();
        cx.push_clip_rect(clip);
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
        for &id in g.selection.ids.iter().chain(g.hover.iter().filter(|id| !g.selection.ids.contains(*id))) {
            if let Some(item) = scene.and_then(|s| s.view.get(&id)) {
                let r = g.preview.as_ref().and_then(|p| p.rect(&id)).unwrap_or(item.rect);
                outline(r, if g.selection.ids.contains(&id) { 2. } else { 1. }, item.locked, false);
            }
        }
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
        for (axis, guide) in self.ghost.as_ref().map_or(&g.guides, |p| &p.guides).iter().enumerate() {
            if let Some(g) = guide {
                let (a, b) = if axis == 0 {
                    (dvec2(g.line, g.start), dvec2(g.line, g.end))
                } else {
                    (dvec2(g.start, g.line), dvec2(g.end, g.line))
                };
                let pos = screen(a);
                let size = screen(b) - pos;
                self.outline.draw_abs(cx, Rect { pos, size: dvec2(size.x.max(1.), size.y.max(1.)) });
            }
        }
        if let Some(b) = bounds {
            let accent = self.outline.color;
            for handle in Handle::ALL {
                let pos = screen(handle.point(b, camera.scale)) - dvec2(4., 4.);
                self.outline.draw_abs(cx, Rect { pos, size: dvec2(8., 8.) });
                self.outline.color = self.grid.color_a;
                self.outline.draw_abs(cx, Rect { pos: pos + dvec2(2., 2.), size: dvec2(4., 4.) });
                self.outline.color = accent;
            }
        }
        cx.pop_clip_rect();
    }

    fn input(&mut self, cx: &mut Cx, event: &Event) {
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
                Hit::FingerDown(d) if d.is_primary_hit() || d.mouse_button().is_some_and(|b| b.is_middle()) => {
                    cx.set_key_focus_from_user(self.area);
                    let space = cx.keyboard.is_key_down(KeyCode::Space) || self.gesture.space;
                    if self.tool != BoardTool::Select && !space && d.is_primary_hit() {
                        let screen = camera.local_to_screen(d.abs);
                        // Nothing is drawn on press: click or drag is decided by the first
                        // move past the slop, or by the release.
                        self.placement_start = Some(camera.world_at(screen));
                        self.placement_dragged = false;
                        self.ghost = None;
                        redraw = true;
                    } else if let Some(scene) = scene {
                        let p = world(d.abs);
                        let id = scene.pick(p);
                        let interior = id.and_then(|id| scene.view.get(&id)).is_some_and(|i| {
                            let b = i.rect;
                            let edge = 5. / camera.scale;
                            i.kind == kind::SECTION && p.x > b.x0 + edge && p.x < b.x1 - edge
                                && p.y > b.y0 + 20. / camera.scale && p.y < b.y1 - edge
                        });
                        let pan = space || d.mouse_button().is_some_and(|b| b.is_middle());
                        let screen = camera.local_to_screen(d.abs);
                        let handle = self.gesture.hit_handle(&scene.view, p, camera.scale);
                        if let Some(handle) = handle.filter(|_| !pan) {
                            self.gesture.press_handle(&scene.view, handle, screen);
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
                        let pan = self.gesture.motion(&scene.view, world(m.abs), camera.local_to_screen(m.abs),
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
                        action = self.gesture.release(&scene.view).map(BoardCanvasAction::Act);
                        self.gesture.handle = if u.is_over {
                            self.gesture.hit_handle(&scene.view, world(u.abs), camera.scale)
                        } else { None };
                        self.gesture.hover = if u.is_over { scene.pick(world(u.abs)) } else { None };
                        if let Some(id) = clicked.filter(|id| self.gesture.hover == Some(*id)) {
                            if u.tap_count >= 2 && previous_click == Some(id) {
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
                    let delta = s.scroll * camera.scale;
                    if s.modifiers.control || s.modifiers.logo {
                        self.zoom(cx, camera.local_to_screen(s.abs), camera.scale * (-delta.y * 0.012).exp());
                    } else {
                        self.pan(cx, camera.pan - delta);
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
                        KeyCode::KeyF | KeyCode::Key0 => {
                            if let Some(fit) = self.fit(camera.view) {
                                self.change_camera(cx, fit);
                            }
                        }
                        KeyCode::Key1 => self.zoom(cx, camera.view.center(), 1.),
                        KeyCode::Equals => self.zoom(cx, camera.view.center(), camera.scale * 2.),
                        KeyCode::Minus => self.zoom(cx, camera.view.center(), camera.scale * 0.5),
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
        if cursor && self.tool != BoardTool::Select && !self.gesture.space {
            cx.set_cursor(MouseCursor::Crosshair);
        } else if cursor {
            if let Some(scene) = &self.scene {
                cx.set_cursor(self.gesture.cursor(&scene.view));
            }
        }
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
            self.camera.view = view;
            if self.camera.scale == 0. || self.fit_pending {
                self.camera = self
                    .fit(view)
                    .unwrap_or_else(|| Camera::new(view, CameraState { pan_x: 0., pan_y: 0., zoom: 1. }));
                self.fit_pending = false;
            }
            let camera = self.camera;
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
            self.draw_board(cx, camera, clip, false);
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
            if again {
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
                self.draw_board(cx, self.camera, clip, true);
                scope.end(cx);
                self.preview_layer = Some(layer);
            }
            self.draw_outlines(cx, clip);
            self.overlay.as_mut().unwrap().end(cx);
        }
        self.draw_editor(cx, scope, clip);
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.editor_event(cx, event, scope) { self.input(cx, event); }
    }
}

#[cfg(test)]
#[path = "canvas_tests.rs"]
mod tests;

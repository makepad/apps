//! An opened graphic's viewer: the document's frame at the playhead, fitted
//! in the pane at its own aspect, with the layers' boxes over it. A layer's
//! box is what it draws (`MotionDraw::item_bounds`: text as laid out,
//! paths by their outline, groups as their children), everything a layer
//! draws internally (a kinetic text's letters, a Repeat's copies) counted
//! as the layer's. A click selects the layer under the pointer (the
//! topmost), a drag moves the selected layer live (a key at the playhead
//! when its position is animated, else its value; one undo step), and a
//! double click on a text layer edits its words in place, live (Enter is a
//! new line; Esc, cmd-Enter or clicking away ends it). A layer a layout
//! places (a Repeat's copy, a sequencer's word) is selected with a dashed
//! box and no handles, says what places it, and does not drag.
//!
//! The picture is the document's frame drawn by `MotionDraw` (the same
//! renderer the edit's preview and export use) at the viewer's pixel size;
//! until its first frame has painted, the shapes and text are sketched so
//! the layout is readable at once.

use makepad_library_browser::draw::DrawEditPicture;
use makepad_library_browser::draw::DrawEditBlock;
use makepad_motion_draw::MotionDraw;
use makepad_motion::MotionFrame;
use makepad_widgets::badge::advance;
use makepad_widgets::*;

/// A layer as the viewer shows it: its box's corners in document units,
/// top-left, top-right, bottom-right, bottom-left.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewItem {
    pub path: String,
    pub quad: [[f64; 2]; 4],
    pub label: String,
    /// Its text, for a Text layer (edited in place).
    pub text: Option<String>,
    pub locked: bool,
    /// For the sketch (no rendered picture yet): the fill (or stroke, or
    /// text colour), whether it is round, the text size in document units,
    /// and the layer's opacity.
    pub ink: Option<[f64; 4]>,
    pub round: bool,
    pub text_size: f64,
    pub opacity: f64,
    /// Placed by a layout: the layer that places it (selected from the
    /// hint) and the hint.
    pub placed: Option<(Option<String>, String)>,
    /// Its transform at the playhead, for the handles: `scale` (x, y),
    /// `rotate` (degrees, clockwise), `anchor` (a fraction of its box).
    pub scale: [f64; 2],
    pub rotate: f64,
    pub anchor: [f64; 2],
    /// A text layer's line width (`wrap`, document units), when it sets one.
    pub wrap: Option<f64>,
    /// Where its origin (`at`) goes over its span (document units), and
    /// its keys there: `(key index, point)` when `at` is keyed.
    pub motion: Vec<[f64; 2]>,
    pub keys: Vec<(usize, [f64; 2])>,
    /// A container (a canvas, a full-frame shader, a group): a click picks
    /// what it draws, not it.
    pub container: bool,
}

/// What a handle of the selected layer does while dragged.
#[derive(Clone, Debug)]
enum Handle {
    /// A corner: the scale, about the pivot (its anchor on screen).
    Scale { pivot: Vec2d, from: f64, base: [f64; 2] },
    /// The knob above the box: the rotation about the pivot.
    Rotate { pivot: Vec2d, from: f64, base: f64 },
    /// The anchor dot: where the box turns and scales about.
    Anchor { quad: [Vec2d; 4] },
    /// The right edge of a text box: its line width.
    Wrap { base: f64, from: f64 },
    /// A key of its motion path: that key's position.
    Key { index: usize, base: [f64; 2], from: Vec2d },
}

/// The point at fractions `f` of a quad (tl, tr, br, bl).
fn at_fraction(quad: &[Vec2d; 4], f: [f64; 2]) -> Vec2d {
    let top = quad[0] + (quad[1] - quad[0]) * f[0];
    let bottom = quad[3] + (quad[2] - quad[3]) * f[0];
    top + (bottom - top) * f[1]
}

/// The fractions of a quad (tl, tr, br, bl) a point is at (its axes taken
/// as the box's edges from tl).
fn fraction_of(quad: &[Vec2d; 4], p: Vec2d) -> [f64; 2] {
    let (u, v) = (quad[1] - quad[0], quad[3] - quad[0]);
    let d = p - quad[0];
    let fu = (d.x * u.x + d.y * u.y) / (u.x * u.x + u.y * u.y).max(1e-9);
    let fv = (d.x * v.x + d.y * v.y) / (v.x * v.x + v.y * v.y).max(1e-9);
    [fu, fv]
}

/// The rotate knob: a little above the middle of the box's top edge.
fn rotate_knob(quad: &[Vec2d; 4]) -> Vec2d {
    let mid = (quad[0] + quad[1]) * 0.5;
    let down = at_fraction(quad, [0.5, 0.5]) - mid;
    let len = down.length().max(1e-9);
    mid - down * (22.0 / len)
}

#[derive(Clone, Debug, Default)]
pub enum ViewerAction {
    #[default]
    None,
    Select(Option<String>),
    /// The selected layer dragged by `delta` document units so far (live,
    /// while dragging) and at its release (`Move`: the end of the step).
    MoveLive { path: String, delta: [f64; 2] },
    Move { path: String, delta: [f64; 2] },
    /// A text layer's words as typed in place (live), and at the end.
    TextLive { path: String, text: String },
    Text { path: String, text: String },
    /// A handle dragged: `scale`, `rotate`, `anchor` or `wrap` set (live,
    /// then at the release).
    TransformLive { path: String, prop: String, values: Vec<f64> },
    Transform { path: String, prop: String, values: Vec<f64> },
    /// A key of the motion path dragged: key `index` of `at` to `at`.
    PathKeyLive { path: String, index: usize, at: [f64; 2] },
    PathKey { path: String, index: usize, at: [f64; 2] },
    /// The pointer over the picture (document units): hovering, or a click
    /// (`alt`: the container, not what it draws).
    Point { at: [f64; 2], click: bool, alt: bool, shift: bool },
}

/// Each layer's drawn box, `[x0, y0, x1, y1]` in document units.
type Bounds = std::collections::HashMap<String, [f64; 4]>;
/// A box's corners on screen.
type Quad = [Vec2d; 4];

/// The layer a drawn item belongs to: the longest of `layers` that is the
/// item's path or leads it (`layers[6]` owns `layers[6].layer.layers[1]#7`
/// and `layers[6]#2`, not `layers[60]`).
pub fn owner_of<'a>(layers: &'a [String], item: &str) -> Option<&'a str> {
    layers.iter().filter(|layer| {
        item == layer.as_str() || (item.starts_with(layer.as_str()) && matches!(item.as_bytes()[layer.len()], b'.' | b'#'))
    }).max_by_key(|layer| layer.len()).map(String::as_str)
}

/// `[x0, y0, x1, y1]` of both.
fn union(a: Option<[f64; 4]>, b: [f64; 4]) -> [f64; 4] {
    match a {
        Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
        None => b,
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct MotionViewer {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_rect: DrawColor,
    #[live]
    draw_picture: DrawEditPicture,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_sketch_text: DrawText,
    #[live]
    draw_round: DrawEditBlock,
    #[live]
    draw_chip: DrawText,
    #[live]
    editor: TextInput,
    #[rust]
    size: [f64; 2],
    #[rust]
    background: Option<[f64; 4]>,
    #[rust]
    items: Vec<ViewItem>,
    #[rust]
    selected: Option<String>,
    #[rust]
    picture: Option<Texture>,
    #[rust]
    motion: Option<MotionDraw>,
    #[rust]
    motion_frame: Option<MotionFrame>,
    /// The shown frame's hash (what it draws).
    #[rust]
    frame_key: Option<u64>,
    /// What the last recording was of (frame hash and pixel size), and
    /// whether it has painted.
    #[rust]
    recorded: Option<u64>,
    #[rust]
    shown: bool,
    #[rust]
    note: String,
    #[rust]
    rect: Rect,
    /// The document's frame in the pane.
    #[rust]
    frame: Rect,
    #[rust]
    hover: Option<Vec2d>,
    #[rust]
    focus_pending: bool,
    #[rust]
    focused_once: bool,
    /// A drag of the selected layer: its origin and the offset so far.
    #[rust]
    drag: Option<(Vec2d, Vec2d)>,
    /// Each layer's drawn box in document units, and the frame they are of.
    #[rust]
    bounds: Bounds,
    #[rust]
    bounds_of: Option<u64>,
    /// The selected layer's box when its drag began (the picture moves
    /// live under it; the box follows the pointer).
    #[rust]
    drag_quad: Option<Quad>,
    /// The hint of a placed layer: its box, and the layer it selects.
    #[rust]
    hint: Option<(Rect, String)>,
    #[rust]
    typing: Option<String>,
    #[rust]
    last_down: Option<(f64, Vec2d)>,
    /// A handle of the selected layer being dragged, and its live values.
    #[rust]
    handle: Option<(Handle, Vec<f64>)>,
    /// What the code selected draws (document units), and its label; what
    /// the pointer would pick.
    #[rust]
    highlight: Vec<[[f64; 2]; 4]>,
    #[rust]
    highlight_label: String,
    #[rust]
    hover_pick: Option<[[f64; 2]; 4]>,
    #[rust]
    last_point: Option<Vec2d>,
}

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.MotionViewerBase = #(MotionViewer::register_widget(vm))
    mod.widgets.MotionViewer = set_type_default() do mod.widgets.MotionViewerBase{
        width: Fill height: Fill
        draw_bg +: {color: stage.video}
        draw_rect +: {color: stage.glass_rim}
        draw_text +: {color: stage.glass_ink text_style: theme.font_regular{font_size: 9}}
        draw_sketch_text +: {color: #fff text_style: theme.font_bold{font_size: 12}}
        draw_chip +: {color: stage.glass_ink text_style: theme.font_bold{font_size: 7}}
        editor: TextInput{width: 200 height: Fit margin: 0 padding: Inset{left: 6 right: 6 top: 4 bottom: 4} empty_text: ""
            is_multiline: true
            draw_bg +: {color: #000000c0 color_hover: #000000c0 color_focus: #000000c0 border_color: stage.accent border_size: 1.5}
            draw_text +: {color: #fff text_style: theme.font_regular{font_size: 12}}}
    }
}

fn color(value: u32, alpha: f32) -> Vec4f {
    let mut color = Vec4f::from_u32(value);
    color.w *= alpha;
    color
}

/// Whether `point` is inside the quad (convex, either winding).
fn inside(quad: &[Vec2d; 4], point: Vec2d) -> bool {
    let mut sign = 0.0;
    for i in 0..4 {
        let (a, b) = (quad[i], quad[(i + 1) % 4]);
        let cross = (b.x - a.x) * (point.y - a.y) - (b.y - a.y) * (point.x - a.x);
        if cross.abs() < 1e-9 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

impl MotionViewer {
    /// The picture as it shows (painted), and the document's size it is
    /// of: what a chat turn reads back to send.
    pub fn shown_picture(&self) -> Option<(Texture, [f64; 2])> {
        self.picture.clone().filter(|_| self.shown).map(|t| (t, self.size))
    }

    /// Ask the renderer what drew each pixel of the picture on screen
    /// (`Cx::request_pick`); `None` before a picture is drawn.
    pub fn request_pick(&self, cx: &mut Cx) -> Option<PickTicket> {
        let picture = self.picture.as_ref()?;
        Some(cx.request_pick(picture))
    }

    /// What a pick id of this picture names: the item's path and its
    /// Draw2D op.
    pub fn pick_name(&self, id: u32) -> Option<(String, Option<usize>)> {
        self.motion.as_ref()?.pick_name(id).cloned()
    }

    pub fn set_frame(&mut self, cx: &mut Cx, size: [f64; 2], background: Option<[f64; 4]>, items: Vec<ViewItem>, selected: Option<String>) {
        self.size = size;
        self.background = background;
        if self.drag.is_none() {
            if self.items.iter().map(|item| &item.path).ne(items.iter().map(|item| &item.path)) {
                self.bounds_of = None;
            }
            self.items = items;
        }
        self.selected = selected;
        self.redraw(cx);
    }

    /// The frame to draw (the document at the playhead), or none.
    /// The frame to show, and what it is (`key`: equal keys, equal frames;
    /// the host's, so nothing hashes what a frame draws).
    pub fn set_motion_frame(&mut self, cx: &mut Cx, frame: Option<MotionFrame>, key: u64) {
        let key = frame.as_ref().map(|_| key);
        if key.is_some() && key == self.frame_key && self.motion_frame.is_some() {
            return;
        }
        self.frame_key = key;
        self.motion_frame = frame;
        self.redraw(cx);
    }

    /// What the frame needs from the library that the renderer does not
    /// have yet (images and SVGs the document shows).
    pub fn missing(&self) -> Vec<makepad_motion::Media> {
        let (Some(motion), Some(frame)) = (&self.motion, &self.motion_frame) else { return Vec::new() };
        motion.missing(frame).into_iter().filter_map(|need| match need {
            makepad_motion_draw::Need::Image(media) | makepad_motion_draw::Need::Svg(media) => Some(media),
            _ => None,
        }).collect()
    }

    /// An image or SVG the document shows, fetched from the library.
    pub fn set_media(&mut self, cx: &mut Cx, media: makepad_motion::Media, bytes: &[u8], kind: &str) -> Result<(), String> {
        let motion = self.motion.get_or_insert_with(|| MotionDraw::new(cx));
        if kind == "svg" {
            motion.set_svg(cx, media, &String::from_utf8_lossy(bytes));
        } else {
            let (pixels, width, height) = makepad_motion_draw::decode_image(bytes)?;
            motion.set_image(cx, media, pixels, width, height);
        }
        self.recorded = None;
        self.redraw(cx);
        Ok(())
    }

    /// Problems the renderer found (shader compiles, effects), each once.
    pub fn draw_errors(&self) -> Vec<String> {
        self.motion.as_ref().map(MotionDraw::errors).unwrap_or_default()
    }

    /// A line over the frame (the document's problems, the render's state).
    pub fn set_note(&mut self, cx: &mut Cx, note: &str) {
        if self.note != note {
            self.note = note.to_string();
            self.redraw(cx);
        }
    }

    /// The drawing of the selected code (its boxes, document units).
    pub fn set_highlight(&mut self, cx: &mut Cx, quads: Vec<[[f64; 2]; 4]>, label: String) {
        self.highlight = quads;
        self.highlight_label = label;
        self.redraw(cx);
    }

    /// What a click here would pick.
    pub fn set_hover_pick(&mut self, cx: &mut Cx, quad: Option<[[f64; 2]; 4]>) {
        if self.hover_pick != quad {
            self.hover_pick = quad;
            self.redraw(cx);
        }
    }

    /// A screen point in document units.
    fn to_doc(&self, p: Vec2d) -> [f64; 2] {
        let s = self.size[0].max(1.0) / self.frame.size.x.max(1.0);
        [(p.x - self.frame.pos.x) * s, (p.y - self.frame.pos.y) * s]
    }

    fn to_screen(&self, p: [f64; 2]) -> Vec2d {
        let s = self.frame.size.x / self.size[0].max(1.0);
        dvec2(self.frame.pos.x + p[0] * s, self.frame.pos.y + p[1] * s)
    }

    /// A layer's box on screen: what it draws, once measured; the model's
    /// estimate before.
    fn quad(&self, item: &ViewItem) -> [Vec2d; 4] {
        match self.bounds.get(&item.path) {
            Some(b) => [[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]]].map(|p| self.to_screen(p)),
            None => item.quad.map(|p| self.to_screen(p)),
        }
    }

    /// Every layer's drawn box for `frame` (see [`owner_of`]).
    fn measure(&mut self, cx: &mut Cx2d, frame: &MotionFrame) {
        let key = self.frame_key.unwrap_or(0);
        // Until the picture has shown, fonts may still be arriving.
        if self.bounds_of == Some(key) && self.shown {
            return;
        }
        let Some(motion) = self.motion.as_mut() else { return };
        let layers: Vec<String> = self.items.iter().map(|item| item.path.clone()).collect();
        let mut bounds: std::collections::HashMap<String, [f64; 4]> = Default::default();
        for (_, item) in frame.walk() {
            let Some(owner) = owner_of(&layers, &item.path) else { continue };
            if let Some(b) = motion.item_bounds(cx, item) {
                let joined = union(bounds.get(owner).copied(), b);
                bounds.insert(owner.to_string(), joined);
            }
        }
        self.bounds = bounds;
        self.bounds_of = Some(key);
    }

    /// The name chip over a box: above it, or inside its top when there is
    /// no room above; kept inside the frame.
    fn chip_rect(&self, cx: &mut Cx2d, chip: &str, quad: [Vec2d; 4]) -> Rect {
        let w = advance(&self.draw_chip, cx, chip) + 12.0;
        let top = quad.iter().map(|q| q.y).fold(f64::MAX, f64::min);
        let left = quad.iter().map(|q| q.x).fold(f64::MAX, f64::min);
        let right_edge = self.frame.pos.x + self.frame.size.x;
        let x = left.min(right_edge - w).max(self.frame.pos.x);
        let y = if top - 18.0 < self.frame.pos.y { top.max(self.frame.pos.y) + 2.0 } else { top - 18.0 };
        Rect { pos: dvec2(x, y.min(self.frame.pos.y + self.frame.size.y - 17.0)), size: dvec2(w, 15.0) }
    }

    /// The drag so far in document units (whole units: the pointer's
    /// delta scaled from the screen to the document).
    fn drag_delta(&self) -> Option<[f64; 2]> {
        let (_, offset) = self.drag?;
        let s = self.size[0].max(1.0) / self.frame.size.x.max(1.0);
        Some([(offset.x * s).round(), (offset.y * s).round()])
    }

    /// The handle of the selected layer under `abs`.
    fn handle_at(&self, abs: Vec2d) -> Option<Handle> {
        let item = self.selected_item()?;
        if item.locked || item.placed.is_some() {
            return None;
        }
        let quad = self.quad(item);
        let near = |p: Vec2d, r: f64| (p - abs).length() <= r;
        let s = self.frame.size.x / self.size[0].max(1.0);
        // The motion path's keys first (they may sit inside the box).
        for (index, point) in &item.keys {
            if near(self.to_screen(*point), 6.0) {
                return Some(Handle::Key { index: *index, base: *point, from: abs });
            }
        }
        let pivot = at_fraction(&quad, item.anchor);
        if near(pivot, 6.0) {
            return Some(Handle::Anchor { quad });
        }
        if near(rotate_knob(&quad), 7.0) {
            let a = (abs.y - pivot.y).atan2(abs.x - pivot.x);
            return Some(Handle::Rotate { pivot, from: a, base: item.rotate });
        }
        if quad.iter().any(|corner| near(*corner, 6.0)) {
            return Some(Handle::Scale { pivot, from: (abs - pivot).length().max(1.0), base: item.scale });
        }
        if item.text.is_some() {
            let right = (quad[1] + quad[2]) * 0.5;
            if near(right, 6.0) {
                let left = (quad[0] + quad[3]) * 0.5;
                let width = item.wrap.unwrap_or((right - left).length() / s);
                return Some(Handle::Wrap { base: width, from: abs.x });
            }
        }
        None
    }

    /// A handle dragged to `abs`: the values it sets.
    fn handle_values(&self, handle: &Handle, abs: Vec2d, shift: bool) -> (String, Vec<f64>) {
        let s = self.frame.size.x / self.size[0].max(1.0);
        match handle {
            Handle::Scale { pivot, from, base } => {
                let k = (abs - *pivot).length() / from;
                let k = if shift { (k * 20.0).round() / 20.0 } else { (k * 1000.0).round() / 1000.0 };
                ("scale".into(), vec![base[0] * k, base[1] * k])
            }
            Handle::Rotate { pivot, from, base } => {
                let a = (abs.y - pivot.y).atan2(abs.x - pivot.x);
                let mut deg = base + (a - from).to_degrees();
                deg = if shift { (deg / 15.0).round() * 15.0 } else { (deg * 10.0).round() / 10.0 };
                ("rotate".into(), vec![deg])
            }
            Handle::Anchor { quad } => {
                let f = fraction_of(quad, abs);
                let snap = |v: f64| if shift { (v * 2.0).round() / 2.0 } else { (v * 100.0).round() / 100.0 };
                ("anchor".into(), vec![snap(f[0]), snap(f[1])])
            }
            Handle::Wrap { base, from } => ("wrap".into(), vec![(base + (abs.x - from) / s).max(10.0).round()]),
            Handle::Key { base, from, .. } => {
                let d = (abs - *from) * (1.0 / s);
                ("at".into(), vec![(base[0] + d.x).round(), (base[1] + d.y).round()])
            }
        }
    }

    fn selected_item(&self) -> Option<&ViewItem> {
        self.selected.as_ref().and_then(|path| self.items.iter().find(|item| item.path == *path))
    }

    fn item_at(&self, abs: Vec2d) -> Option<&ViewItem> {
        // Topmost first; the selected layer wins where it overlaps.
        if let Some(selected) = self.selected.as_ref().and_then(|path| self.items.iter().find(|item| item.path == *path)) {
            if inside(&self.quad(selected), abs) {
                return Some(selected);
            }
        }
        self.items.iter().rev().find(|item| !item.container && inside(&self.quad(item), abs))
    }

    fn line(&mut self, cx: &mut Cx2d, a: Vec2d, b: Vec2d, ink: Vec4f, width: f64) {
        // Axis-aligned boxes are the common case; a turned box draws its
        // edges as short steps.
        let d = b - a;
        let len = d.length();
        if len < 0.5 {
            return;
        }
        self.draw_rect.color = ink;
        if d.x.abs() < 0.01 || d.y.abs() < 0.01 {
            let r = Rect { pos: dvec2(a.x.min(b.x) - width * 0.5, a.y.min(b.y) - width * 0.5), size: dvec2(d.x.abs() + width, d.y.abs() + width) };
            self.draw_rect.draw_abs(cx, r);
            return;
        }
        let steps = (len / 2.0).ceil() as usize;
        for i in 0..steps {
            let p = a + d * (i as f64 / steps as f64);
            self.draw_rect.draw_abs(cx, Rect { pos: dvec2(p.x - width * 0.5, p.y - width * 0.5), size: dvec2(width, width) });
        }
    }

    /// A round dot of radius `r`: `fill`, with an `edge`.
    fn dot(&mut self, cx: &mut Cx2d, at: Vec2d, r: f64, fill: Vec4f, edge: Vec4f) {
        self.draw_round.fill = fill;
        self.draw_round.edge = edge;
        self.draw_round.stroke = 1.5;
        self.draw_round.radius = 0.0;
        self.draw_round.shape = 3.0;
        self.draw_round.draw_abs(cx, Rect { pos: dvec2(at.x - r, at.y - r), size: dvec2(2.0 * r, 2.0 * r) });
    }

    fn outline(&mut self, cx: &mut Cx2d, quad: [Vec2d; 4], ink: Vec4f, width: f64) {
        for i in 0..4 {
            self.line(cx, quad[i], quad[(i + 1) % 4], ink, width);
        }
    }

    /// The end of typing in place: the words so far are already live;
    /// this closes the edit as one undo step.
    fn end_typing(&mut self, cx: &mut Cx) {
        let Some(path) = self.typing.take() else { return };
        self.focused_once = false;
        let text = self.editor.text();
        cx.widget_action(self.widget_uid(), ViewerAction::Text { path, text });
        self.redraw(cx);
    }
}

impl Widget for MotionViewer {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let uid = self.widget_uid();
        if let Some(motion) = &mut self.motion {
            motion.handle_event(cx, event);
        }
        if self.typing.is_some() {
            let editor_uid = self.editor.widget_uid();
            self.editor.handle_event(cx, event, scope);
            if let Event::Actions(actions) = event {
                for action in actions.filter_widget_actions(editor_uid) {
                    match action.cast::<TextInputAction>() {
                        TextInputAction::Changed(text) => {
                            if let Some(path) = self.typing.clone() {
                                cx.widget_action(uid, ViewerAction::TextLive { path, text });
                            }
                            // The box grows with its lines.
                            self.redraw(cx);
                        }
                        // Plain Enter is a new line (the input's own).
                        TextInputAction::Returned(..) | TextInputAction::Escaped => self.end_typing(cx),
                        TextInputAction::KeyFocusLost if self.focused_once && !self.focus_pending => self.end_typing(cx),
                        _ => {}
                    }
                }
            }
        }
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                self.hover = Some(fe.abs);
                if self.drag.is_none() && self.handle.is_none() && self.frame.contains(fe.abs) && self.last_point.is_none_or(|p| (p - fe.abs).length() > 2.0) {
                    self.last_point = Some(fe.abs);
                    cx.widget_action(uid, ViewerAction::Point { at: self.to_doc(fe.abs), click: false, alt: false, shift: false });
                }
                let cursor = if self.hint.as_ref().is_some_and(|(r, _)| r.contains(fe.abs)) {
                    MouseCursor::Hand
                } else if let Some(handle) = self.handle_at(fe.abs) {
                    match handle {
                        Handle::Scale { .. } => MouseCursor::NwseResize,
                        Handle::Wrap { .. } => MouseCursor::EwResize,
                        Handle::Rotate { .. } | Handle::Anchor { .. } | Handle::Key { .. } => MouseCursor::Crosshair,
                    }
                } else {
                    match self.selected_item().filter(|item| inside(&self.quad(item), fe.abs)) {
                        Some(item) if item.placed.is_some() || item.locked => MouseCursor::NotAllowed,
                        Some(_) => MouseCursor::Move,
                        None => MouseCursor::Default,
                    }
                };
                cx.set_cursor(cursor);
                self.redraw(cx);
            }
            Hit::FingerHoverOut(_) => {
                self.hover = None;
                self.hover_pick = None;
                self.redraw(cx);
            }
            Hit::FingerDown(fe) if fe.is_primary_hit() => {
                if let Some(path) = self.typing.clone() {
                    let in_box = self.items.iter().find(|item| item.path == path).is_some_and(|item| inside(&self.quad(item), fe.abs));
                    if in_box {
                        // In the words being typed: the input's own.
                        return;
                    }
                    self.end_typing(cx);
                }
                // The hint of a placed layer selects what places it.
                if let Some((r, owner)) = self.hint.clone() {
                    if r.contains(fe.abs) {
                        self.selected = Some(owner.clone());
                        cx.widget_action(uid, ViewerAction::Select(Some(owner)));
                        self.redraw(cx);
                        return;
                    }
                }
                let double = self.last_down.is_some_and(|(time, at)| fe.time - time < 0.35 && (fe.abs - at).length() < 4.0);
                self.last_down = Some((fe.time, fe.abs));
                // The selected layer's handles: scale, rotate, anchor, its
                // text box's width, its motion path's keys.
                if let Some(handle) = self.handle_at(fe.abs) {
                    self.handle = Some((handle, Vec::new()));
                    self.redraw(cx);
                    return;
                }
                // A click off the picture picks nothing: the pick clears.
                if !double {
                    cx.widget_action(uid, ViewerAction::Point { at: self.to_doc(fe.abs), click: true, alt: fe.modifiers.alt, shift: fe.modifiers.shift });
                }
                let hit = self.item_at(fe.abs).cloned();
                match hit {
                    Some(item) => {
                        if double && item.text.is_some() && !item.locked {
                            self.editor.set_text(cx, item.text.as_deref().unwrap_or(""));
                            self.typing = Some(item.path.clone());
                        } else {
                            if self.selected.as_deref() != Some(item.path.as_str()) {
                                self.selected = Some(item.path.clone());
                                cx.widget_action(uid, ViewerAction::Select(Some(item.path.clone())));
                            }
                            // A layer a layout places does not drag.
                            if !item.locked && item.placed.is_none() {
                                self.drag = Some((fe.abs, dvec2(0.0, 0.0)));
                                self.drag_quad = Some(self.quad(&item));
                            }
                        }
                    }
                    None => {
                        self.selected = None;
                        cx.widget_action(uid, ViewerAction::Select(None));
                    }
                }
                self.redraw(cx);
            }
            Hit::FingerMove(fe) => {
                if let Some((handle, _)) = self.handle.clone() {
                    let (prop, values) = self.handle_values(&handle, fe.abs, fe.modifiers.shift);
                    self.handle = Some((handle.clone(), values.clone()));
                    if let Some(path) = self.selected.clone() {
                        match handle {
                            Handle::Key { index, .. } => cx.widget_action(uid, ViewerAction::PathKeyLive { path, index, at: [values[0], values[1]] }),
                            _ => cx.widget_action(uid, ViewerAction::TransformLive { path, prop, values }),
                        }
                    }
                    self.redraw(cx);
                    return;
                }
                if let Some((origin, _)) = self.drag {
                    let mut offset = fe.abs - origin;
                    // Shift keeps the move on one axis.
                    if fe.modifiers.shift {
                        if offset.x.abs() > offset.y.abs() { offset.y = 0.0 } else { offset.x = 0.0 }
                    }
                    let moved = self.drag.map(|(_, before)| before) != Some(offset);
                    self.drag = Some((origin, offset));
                    if moved {
                        // Live: the picture follows (one undo step, closed
                        // on release).
                        if let (Some(path), Some(delta)) = (self.selected.clone(), self.drag_delta()) {
                            cx.widget_action(uid, ViewerAction::MoveLive { path, delta });
                        }
                    }
                    self.redraw(cx);
                }
            }
            Hit::FingerUp(fe) if self.handle.is_some() => {
                let (handle, values) = self.handle.take().unwrap();
                if let (Some(path), false) = (self.selected.clone(), values.is_empty()) {
                    let _ = fe;
                    match handle {
                        Handle::Key { index, .. } => cx.widget_action(uid, ViewerAction::PathKey { path, index, at: [values[0], values[1]] }),
                        _ => {
                            let prop = self.handle_values(&handle, fe.abs, fe.modifiers.shift).0;
                            cx.widget_action(uid, ViewerAction::Transform { path, prop, values });
                        }
                    }
                }
                self.bounds_of = None;
                self.redraw(cx);
            }
            Hit::FingerUp(_) => {
                // The field takes the keys once the click that opened it is
                // released (a release outside a focused field drops focus).
                if self.typing.is_some() && !self.focused_once {
                    self.focused_once = true;
                    self.focus_pending = true;
                    self.redraw(cx);
                }
                if let Some(delta) = self.drag_delta() {
                    self.drag = None;
                    self.drag_quad = None;
                    if let Some(path) = self.selected.clone() {
                        cx.widget_action(uid, ViewerAction::Move { path, delta });
                    }
                    // The box is measured again from the moved frame.
                    self.bounds_of = None;
                    self.redraw(cx);
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let step_t0 = makepad_value_pickers::panel::host::timing().then(Cx::time_now);
        let step = self.draw_walk_timed(cx, scope, walk);
        if let Some(t0) = step_t0 {
            makepad_value_pickers::panel::host::time_add("draw.viewer", Cx::time_now() - t0);
        }
        step
    }
}

impl MotionViewer {
    fn draw_walk_timed(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.rect = rect;
        let p = makepad_look::palette::get(cx);
        self.draw_bg.color = color(p.video, 1.0);
        self.draw_bg.draw_abs(cx, rect);
        if self.size[0] <= 0.0 || self.size[1] <= 0.0 {
            return DrawStep::done();
        }
        // The frame at the document's aspect, inset a little.
        let inset = 16.0;
        let (aw, ah) = ((rect.size.x - 2.0 * inset).max(1.0), (rect.size.y - 2.0 * inset).max(1.0));
        let scale = (aw / self.size[0]).min(ah / self.size[1]);
        let (fw, fh) = (self.size[0] * scale, self.size[1] * scale);
        self.frame = Rect { pos: dvec2(rect.pos.x + (rect.size.x - fw) * 0.5, rect.pos.y + (rect.size.y - fh) * 0.5), size: dvec2(fw, fh) };
        cx.push_clip_rect(rect);
        match self.background {
            Some(bg) => {
                self.draw_rect.color = vec4(bg[0] as f32, bg[1] as f32, bg[2] as f32, bg[3] as f32);
                self.draw_rect.draw_abs(cx, self.frame);
            }
            None => {
                // Transparent: a quiet checkerboard.
                let cell = 16.0;
                let (nx, ny) = ((fw / cell).ceil() as usize, (fh / cell).ceil() as usize);
                for j in 0..ny {
                    for i in 0..nx {
                        let light = (i + j) % 2 == 0;
                        self.draw_rect.color = color(if light { 0x1c2027ff } else { 0x15181dff }, 1.0);
                        let r = Rect { pos: dvec2(self.frame.pos.x + i as f64 * cell, self.frame.pos.y + j as f64 * cell), size: dvec2(cell.min(fw - i as f64 * cell), cell.min(fh - j as f64 * cell)) };
                        self.draw_rect.draw_abs(cx, r);
                    }
                }
            }
        }
        // The rendered frame: recorded when it changed (or was not whole),
        // kept attached until it has painted.
        // (Taken, not copied, while it is drawn: a frame can be large.)
        let shown_frame = self.motion_frame.take();
        if let Some(frame) = &shown_frame {
            let dpi = cx.current_dpi_factor();
            let pixels = (((fw * dpi).round() as u32).max(2), ((fh * dpi).round() as u32).max(2));
            let key = self.frame_key.unwrap_or(0) ^ ((pixels.0 as u64) << 40) ^ ((pixels.1 as u64) << 20);
            let motion = self.motion.get_or_insert_with(|| MotionDraw::new(cx));
            // (This runs in the draw: a look again next frame is asked for
            // in the draw's own way; a plain redraw here is dropped, and the
            // new frame would wait for the next unrelated redraw.)
            // Its draws carry pick ids (a click asks the renderer what is
            // under it).
            let picks = cx.pick_variants();
            cx.set_pick_ids(picks);
            if self.recorded != Some(key) || !motion.exact(cx) {
                if motion.ready() || self.recorded.is_none() {
                    let drawn = motion.draw(cx, frame, pixels);
                    self.picture = Some(drawn.texture);
                    self.recorded = Some(key);
                    // Glyphs drawn for the first time arrive next frame.
                    if !drawn.exact {
                        cx.redraw_area_in_draw(self.draw_bg.area());
                    }
                } else {
                    // The renderer is busy with the last frame: this one
                    // is drawn when it is free.
                    cx.redraw_area_in_draw(self.draw_bg.area());
                }
            } else if motion.painted(cx) {
                self.shown = true;
            } else {
                motion.reattach(cx);
                cx.redraw_area_in_draw(self.draw_bg.area());
            }
            cx.set_pick_ids(false);
        } else {
            self.picture = None;
            self.recorded = None;
        }
        if let Some(frame) = &shown_frame {
            self.measure(cx, frame);
        }
        self.motion_frame = shown_frame;
        let wireframe = !self.shown || self.picture.is_none();
        if let (Some(texture), false) = (self.picture.clone(), wireframe) {
            self.draw_picture.draw_vars.set_texture(0, &texture);
            self.draw_picture.opacity = 1.0;
            self.draw_picture.uv = vec4(0.0, 0.0, 1.0, 1.0);
            self.draw_picture.draw_abs(cx, self.frame);
        }
        let drag_offset = self.drag.map(|(_, offset)| offset).unwrap_or_default();
        self.hint = None;
        let items = self.items.clone();
        for item in &items {
            let selected = self.selected.as_deref() == Some(item.path.as_str());
            let mut quad = self.quad(item);
            if selected {
                if let Some(start) = self.drag_quad {
                    // Dragging: the box as it was, carried by the pointer.
                    quad = start.map(|corner| corner + drag_offset);
                }
            }
            let hovered = self.hover.is_some_and(|hover| inside(&quad, hover));
            if wireframe {
                // The sketch: shapes in their colour, text at its size.
                let (x0, y0) = (quad.iter().map(|q| q.x).fold(f64::MAX, f64::min), quad.iter().map(|q| q.y).fold(f64::MAX, f64::min));
                let (x1, y1) = (quad.iter().map(|q| q.x).fold(f64::MIN, f64::max), quad.iter().map(|q| q.y).fold(f64::MIN, f64::max));
                match (&item.text, item.ink) {
                    (Some(text), ink) => {
                        let ink = ink.unwrap_or([1.0, 1.0, 1.0, 1.0]);
                        let scale = self.frame.size.x / self.size[0].max(1.0);
                        self.draw_sketch_text.text_style.font_size = ((item.text_size * scale * 0.75) as f32).max(4.0);
                        self.draw_sketch_text.color = vec4(ink[0] as f32, ink[1] as f32, ink[2] as f32, (ink[3] * item.opacity) as f32);
                        let mut y = y0;
                        for line in text.lines() {
                            self.draw_sketch_text.draw_abs(cx, dvec2(x0, y), line);
                            y += item.text_size * scale * 1.2;
                        }
                    }
                    (None, Some(ink)) if x1 - x0 >= 1.0 && y1 - y0 >= 1.0 => {
                        let fill = vec4(ink[0] as f32, ink[1] as f32, ink[2] as f32, (ink[3] * item.opacity) as f32);
                        self.draw_round.fill = fill;
                        self.draw_round.edge = fill;
                        self.draw_round.stroke = 0.0;
                        self.draw_round.radius = 0.0;
                        self.draw_round.shape = if item.round { 3.0 } else { 0.0 };
                        self.draw_round.draw_abs(cx, Rect { pos: dvec2(x0, y0), size: dvec2(x1 - x0, y1 - y0) });
                    }
                    _ => self.outline(cx, quad, color(p.glass_ink, 0.35), 1.0),
                }
            }
            if selected && item.placed.is_some() {
                // Placed by a layout: a dashed box, no handles, and what
                // places it (a click on the hint selects that).
                let (x0, y0) = (quad.iter().map(|q| q.x).fold(f64::MAX, f64::min), quad.iter().map(|q| q.y).fold(f64::MAX, f64::min));
                let (x1, y1) = (quad.iter().map(|q| q.x).fold(f64::MIN, f64::max), quad.iter().map(|q| q.y).fold(f64::MIN, f64::max));
                self.draw_round.fill = color(0, 0.0);
                self.draw_round.edge = color(p.accent, 1.0);
                self.draw_round.stroke = 1.5;
                self.draw_round.radius = 1.0;
                self.draw_round.shape = 5.0;
                self.draw_round.draw_abs(cx, Rect { pos: dvec2(x0 - 1.0, y0 - 1.0), size: dvec2(x1 - x0 + 2.0, y1 - y0 + 2.0) });
                let (owner, hint) = item.placed.clone().unwrap_or_default();
                let chip = format!("{}  ·  {hint}{}", item.label, if owner.is_some() { "  ›" } else { "" });
                let r = self.chip_rect(cx, &chip, quad);
                self.draw_rect.color = color(p.raised, 0.95);
                self.draw_rect.draw_abs(cx, r);
                self.draw_rect.color = color(p.accent, 1.0);
                self.draw_rect.draw_abs(cx, Rect { pos: r.pos, size: dvec2(2.0, r.size.y) });
                self.draw_chip.color = color(p.glass_ink, 1.0);
                self.draw_chip.draw_abs(cx, dvec2(r.pos.x + 6.0, r.pos.y + 2.5), &chip);
                self.hint = owner.map(|owner| (r, owner));
            } else if selected {
                // Its motion path: where its origin goes, keys as dots.
                if item.motion.len() > 1 {
                    let points: Vec<Vec2d> = item.motion.iter().map(|p| self.to_screen(*p)).collect();
                    for pair in points.windows(2) {
                        self.line(cx, pair[0], pair[1], color(p.accent, 0.45), 1.0);
                    }
                    let held = match &self.handle {
                        Some((Handle::Key { index, from, base }, values)) if values.len() == 2 => Some((*index, self.to_screen([values[0], values[1]]), *from, *base)),
                        _ => None,
                    };
                    for (index, point) in &item.keys {
                        let at = match held {
                            Some((i, moved, _, _)) if i == *index => moved,
                            _ => self.to_screen(*point),
                        };
                        self.dot(cx, at, 4.5, color(p.video, 1.0), color(p.accent, 1.0));
                    }
                }
                self.outline(cx, quad, color(p.accent, 0.75), 1.0);
                if !item.locked {
                    for corner in quad {
                        self.draw_rect.color = color(p.accent, 1.0);
                        self.draw_rect.draw_abs(cx, Rect { pos: dvec2(corner.x - 3.5, corner.y - 3.5), size: dvec2(7.0, 7.0) });
                        self.draw_rect.color = color(p.video, 1.0);
                        self.draw_rect.draw_abs(cx, Rect { pos: dvec2(corner.x - 2.0, corner.y - 2.0), size: dvec2(4.0, 4.0) });
                    }
                    // The rotate knob on its stalk, the anchor, the text
                    // box's width.
                    let knob = rotate_knob(&quad);
                    self.line(cx, (quad[0] + quad[1]) * 0.5, knob, color(p.accent, 0.8), 1.0);
                    self.dot(cx, knob, 4.5, color(p.video, 1.0), color(p.accent, 1.0));
                    let pivot = at_fraction(&quad, item.anchor);
                    self.line(cx, pivot - dvec2(6.0, 0.0), pivot + dvec2(6.0, 0.0), color(p.accent, 1.0), 1.0);
                    self.line(cx, pivot - dvec2(0.0, 6.0), pivot + dvec2(0.0, 6.0), color(p.accent, 1.0), 1.0);
                    self.dot(cx, pivot, 3.0, color(0, 0.0), color(p.accent, 1.0));
                    if item.text.is_some() {
                        let right = (quad[1] + quad[2]) * 0.5;
                        self.draw_rect.color = color(p.accent, 1.0);
                        self.draw_rect.draw_abs(cx, Rect { pos: dvec2(right.x - 2.0, right.y - 7.0), size: dvec2(4.0, 14.0) });
                    }
                }
                // The name above the box.
                let chip = if let Some((handle, values)) = self.handle.clone().filter(|(_, v)| !v.is_empty()) {
                    let shown = values.iter().map(|v| makepad_motion::patch::fmt_num((v * 100.0).round() / 100.0)).collect::<Vec<_>>().join(", ");
                    let name = match handle {
                        Handle::Scale { .. } => "scale",
                        Handle::Rotate { .. } => "rotate",
                        Handle::Anchor { .. } => "anchor",
                        Handle::Wrap { .. } => "wrap",
                        Handle::Key { .. } => "key",
                    };
                    format!("{}  {name} {shown}", item.label)
                } else if self.drag.is_some() {
                    let s = self.size[0].max(1.0) / self.frame.size.x.max(1.0);
                    format!("{}  Δ {:.0}, {:.0}", item.label, drag_offset.x * s, drag_offset.y * s)
                } else {
                    item.label.clone()
                };
                let r = self.chip_rect(cx, &chip, quad);
                self.draw_rect.color = color(p.accent, 1.0);
                self.draw_rect.draw_abs(cx, r);
                self.draw_chip.color = color(p.on_accent, 1.0);
                self.draw_chip.draw_abs(cx, dvec2(r.pos.x + 5.0, r.pos.y + 2.5), &chip);
            } else if hovered && self.drag.is_none() {
                self.outline(cx, quad, color(p.glass_ink, 0.3), 1.0);
            }
        }
        // What the selected code draws: hugged by an outline, washed.
        let highlight = self.highlight.clone();
        for quad in &highlight {
            let q = quad.map(|p| self.to_screen(p));
            self.outline(cx, q, color(p.accent, 0.55), 1.0);
        }
        if let Some(first) = highlight.first() {
            if !self.highlight_label.is_empty() {
                let q = first.map(|p| self.to_screen(p));
                let label = self.highlight_label.clone();
                let r = self.chip_rect(cx, &label, q);
                self.draw_rect.color = color(p.accent, 1.0);
                self.draw_rect.draw_abs(cx, r);
                self.draw_chip.color = color(p.on_accent, 1.0);
                self.draw_chip.draw_abs(cx, dvec2(r.pos.x + 5.0, r.pos.y + 2.5), &label);
            }
        }
        if let Some(quad) = self.hover_pick {
            let q = quad.map(|p| self.to_screen(p));
            self.outline(cx, q, color(p.glass_ink, 0.3), 1.0);
        }
        // The frame's edge.
        self.outline(cx, [self.frame.pos, dvec2(self.frame.pos.x + fw, self.frame.pos.y), dvec2(self.frame.pos.x + fw, self.frame.pos.y + fh), dvec2(self.frame.pos.x, self.frame.pos.y + fh)], color(p.glass_rim, 1.0), 1.0);
        if !self.note.is_empty() {
            let w = advance(&self.draw_chip, cx, &self.note) + 12.0;
            let r = Rect { pos: dvec2(rect.pos.x + 8.0, rect.pos.y + 8.0), size: dvec2(w.min(rect.size.x - 16.0), 17.0) };
            self.draw_rect.color = color(p.raised, 0.95);
            self.draw_rect.draw_abs(cx, r);
            self.draw_chip.color = color(p.glass_ink, 1.0);
            self.draw_chip.draw_abs(cx, dvec2(r.pos.x + 6.0, r.pos.y + 3.5), &self.note);
        }
        // Text typed in place, over its layer.
        if let Some(path) = self.typing.clone() {
            if let Some(item) = self.items.iter().find(|item| item.path == path) {
                let quad = self.quad(item);
                let left = quad.iter().map(|q| q.x).fold(f64::MAX, f64::min);
                let top = quad.iter().map(|q| q.y).fold(f64::MAX, f64::min);
                let right = quad.iter().map(|q| q.x).fold(f64::MIN, f64::max);
                let bottom = quad.iter().map(|q| q.y).fold(f64::MIN, f64::max);
                // Every line shows; past six the box scrolls.
                let lines = self.editor.text().split('\n').count().clamp(1, 6) as f64;
                let walk = Walk { abs_pos: Some(dvec2(left, top)), width: Size::Fixed((right - left).max(160.0)), height: Size::Fixed((bottom - top).max(lines * 17.0 + 10.0).min(6.0 * 17.0 + 10.0).max(28.0)), ..Walk::default() };
                self.editor.draw_walk(cx, scope, walk).ok();
                if std::mem::take(&mut self.focus_pending) {
                    // Focus once it has an area: it was just drawn.
                    self.editor.take_key_focus(cx);
                    self.editor.select_all(cx);
                }
            }
        }
        cx.pop_clip_rect();
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawn_items_belong_to_their_layer() {
        let layers: Vec<String> = ["layers[0]", "layers[6]", "layers[6].layers[1]", "layers[60]", "layers[2].layer"].map(String::from).to_vec();
        // A kinetic text's internal letter copy is its layer's.
        assert_eq!(owner_of(&layers, "layers[6].layer.layers[1]#7"), Some("layers[6]"));
        assert_eq!(owner_of(&layers, "layers[6].layers[1]"), Some("layers[6].layers[1]"));
        assert_eq!(owner_of(&layers, "layers[6].layers[1]#2"), Some("layers[6].layers[1]"));
        assert_eq!(owner_of(&layers, "layers[60]"), Some("layers[60]"));
        assert_eq!(owner_of(&layers, "layers[2].layer#3"), Some("layers[2].layer"));
        assert_eq!(owner_of(&layers, "layers[3]"), None);
        assert_eq!(union(Some([0.0, 0.0, 10.0, 10.0]), [5.0, -2.0, 12.0, 8.0]), [0.0, -2.0, 12.0, 10.0]);
    }
}

//! A canvas pane's input and per-pane state, embedded by every 2D view widget (explorer, board,
//! Part, proof): it turns Makepad events into the modeless grammar of ARCHITECTURE 6.3 and keeps
//! the camera, the hover (with its 120 ms fade) and the semantic nodes.
//!
//! Grammar: click focuses (empty canvas: `Click { key: None }`); a drag of an item past four
//! points is that item's drag; a drag of empty canvas, or from a background item's inside
//! (`CanvasItems::marquee_over`: a pour), is a marquee; the pan key (space), Alt
//! held at the press, the middle or secondary button, or two fingers pan; the wheel, a pinch, or Ctrl/Cmd with two
//! fingers zoom; a key acts on what is under the pointer; Escape cancels a drag first, else
//! steps out. The pointer is a hand over anything draggable and grabs while dragging.
//!
//! Keys go to the canvas under the pointer without a click first (6.3: a key acts on what is
//! under the pointer): the hovered pane takes the key (and the focus) unless the key focus takes
//! typed text. Typing in the chat with the pointer resting over a canvas types in the chat.
//! The focused pane takes its keys while the pointer is over no canvas.
//!
//! Cost per event: one `CanvasItems::hit` on pointer moves and presses (an index query in the
//! view), O(1) otherwise. Per frame: `Nav::step`, the hover fade, nothing else; frames are asked
//! only while the camera or the fade moves. Semantic nodes are gathered at draw time from
//! `CanvasItems::nodes` for what the camera shows, so their count follows the view, not the design.
use super::camera::{Camera2d, CanvasItems, CanvasNode};
use super::looks::STATE_SECONDS;
use super::nav::Nav;
use makepad_widgets_core::makepad_micro_serde::*;
use makepad_widgets_core::makepad_platform::a11y_tree;
use makepad_widgets_core::*;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

/// A press farther than this many points from where it started is a drag.
pub const DRAG_THRESHOLD: f64 = 4.0;
/// Items within this many points of the pointer are under it.
pub const HIT_RADIUS: f64 = 4.0;

/// What the pane's input means, for the view to act on.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasInput<K> {
    /// The item under the pointer changed (None: nothing).
    Hover(Option<K>),
    /// A press and release without a drag; `key` None on empty canvas. `clicks` counts taps.
    Click { key: Option<K>, at: [f64; 2], clicks: u32, shift: bool },
    /// The pointer went down on an item (`down`) or came up after such a press, dragged or not,
    /// before that release's `Click` or `DragEnd`: what a momentary control (a live button) reads.
    Press { key: K, down: bool },
    /// An item's drag passed the threshold.
    DragStart { key: K, from: [f64; 2], at: [f64; 2] },
    DragMove { key: K, at: [f64; 2] },
    DragEnd { key: K, at: [f64; 2] },
    /// Escape during an item drag: the view restores the committed state.
    DragCancel { key: K },
    /// A marquee's world box, with `done` on release. Left to right takes parts wholly inside.
    /// `crossing` is true left of the press and takes parts the band touches. O(1) to emit either.
    Marquee { rect: [f64; 4], done: bool, shift: bool, crossing: bool },
    /// A key, with the item under the pointer it acts on.
    Key { code: KeyCode, modifiers: KeyModifiers, under: Option<K> },
    /// Escape with no drag running: one step out (leave the reveal, then clear the selection).
    Escape,
    /// The camera moved; redraw.
    Camera,
}

#[derive(Clone, Debug)]
struct Press<K> {
    key: Option<K>,
    start: Vec2d,
    last: Vec2d,
    pan: bool,
    dragging: bool,
    clicks: u32,
    shift: bool,
    /// The press went down on a background item's inside: a drag draws the marquee.
    marquee: bool,
}

impl<K> Press<K> {
    /// The item this press drags, if it drags one (not a marquee from a background item).
    fn drags(&self) -> Option<&K> {
        self.key.as_ref().filter(|_| !self.marquee)
    }
}

/// The canvas pane under the pointer, across all panes in the window. A Cx global of a pane id,
/// kept current on events (area copies go stale on redraw). The hover entry is cleared when the
/// pointer leaves or the pane is no longer drawn (its area is no longer valid).
#[derive(Default)]
struct CanvasKeys {
    hovered: Option<u64>,
}

/// Who holds the key focus, as one pane sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KeyFocus {
    Mine,
    /// Nothing, another canvas pane, a button, Fab's viewport: no widget that takes typed text.
    Elsewhere,
    /// A widget that takes typed text (`Cx::key_focus_takes_text`): a text field, the code editor.
    Text,
}

/// Whether pane `me` takes a key: the pane under the pointer takes it unless the key focus takes
/// typed text; the focused pane takes it while the pointer is over no canvas. At most one pane
/// takes any key.
fn takes_key(me: u64, hovered: Option<u64>, focus: KeyFocus) -> bool {
    match focus {
        KeyFocus::Mine => hovered.is_none_or(|h| h == me),
        KeyFocus::Elsewhere => hovered == Some(me),
        KeyFocus::Text => false,
    }
}

/// Points round the content when a canvas fits it whole.
pub const FIT_MARGIN: f64 = 24.0;

/// Seconds a zoom button's step takes.
const ZOOM_STEP_S: f64 = 0.15;

/// The deepest useful zoom of a board pane (world units are millimetres): 0.1 mm, the finest
/// trace a board house makes, 1000 points across. Deeper shows nothing new.
pub const BOARD_MAX_PPU: f64 = 1000.0 / 0.1;

static NEXT_PANE: AtomicU64 = AtomicU64::new(1);

/// The world point and zoom a person left on a plate, independent of the pane's size and DPI.
#[derive(Clone, Copy, Debug, PartialEq, SerRon, DeRon)]
pub struct SavedView2d {
    pub center: [f64; 2],
    pub px_per_unit: f64,
}

fn can_place(view_size: Vec2d, bounds: [f64; 4]) -> bool {
    view_size.x > 0.0 && view_size.y > 0.0 && bounds[0] < bounds[2] && bounds[1] < bounds[3]
}

impl SavedView2d {
    pub fn shows(&self, view_size: Vec2d, bounds: [f64; 4]) -> bool {
        if !can_place(view_size, bounds) || !(self.px_per_unit.is_finite() && self.px_per_unit > 0.0) {
            return false;
        }
        let [x, y] = self.center;
        let half = view_size * (0.5 / self.px_per_unit);
        x - half.x < bounds[2] && x + half.x > bounds[0]
            && y - half.y < bounds[3] && y + half.y > bounds[1]
    }
}

/// The opening plate's camera, or a fresh fit. The weak reference identifies the outgoing
/// content even after the worker replaces it, without keeping its layout or geometry alive.
pub(crate) struct ViewOnArrival<T> {
    pub saved: Option<SavedView2d>,
    previous: Weak<T>,
}

impl<T> ViewOnArrival<T> {
    pub fn new(saved: Option<SavedView2d>, previous: Option<&Arc<T>>) -> Option<Self> {
        if saved.is_none() && previous.is_none() {
            return None; // an empty pane already fits its first content
        }
        Some(Self { saved, previous: previous.map(Arc::downgrade).unwrap_or_default() })
    }

    /// Called just before `place`, which supplies the current rect and DPI and fits an unplaced
    /// pane. A fit also waits for new content when the outgoing plate still occupies the pane.
    pub fn prepare<K>(&self, pane: &mut CanvasPane<K>, content: &Arc<T>, size: Vec2d, bounds: [f64; 4]) -> bool {
        if self.previous.as_ptr() == Arc::as_ptr(content) || !can_place(size, bounds) {
            return false;
        }
        if let Some(saved) = self.saved.filter(|saved| saved.shows(size, bounds)) {
            pane.cam.center = saved.center;
            pane.cam.px_per_unit = saved.px_per_unit;
            pane.placed = true;
            pane.keeps_fit = false;
            pane.nav.stop();
        } else {
            pane.placed = false;
        }
        true
    }
}

/// The per-pane canvas state.
pub struct CanvasPane<K> {
    pub cam: Camera2d,
    pub nav: Nav,
    hover: Option<K>,
    hover_at: Option<Vec2d>,
    /// The world point and zoom (px per unit) the hover was last picked at.
    picked: Option<([f64; 2], f64)>,
    hover_start: f64,
    hover_mix: f64,
    press: Option<Press<K>>,
    space_held: bool,
    frame: NextFrame,
    nodes: Vec<(u64, CanvasNode<K>)>,
    /// The pane has been laid out at least once (its camera has a view).
    pub placed: bool,
    /// The camera shows the content whole, as fitted: it fits again when the clear area changes,
    /// until the person pans or zooms ([`Self::moved_by_hand`]); a fit of the whole content
    /// ([`Self::fit_whole`]) sets it again.
    pub keeps_fit: bool,
    /// The edges covered by overlays, in logical points `[left, top, right, bottom]`.
    covered_inset: [f64; 4],
    inset_changed: bool,
    /// This pane in `CanvasKeys`.
    id: u64,
}

impl<K> Default for CanvasPane<K> {
    fn default() -> Self {
        Self {
            cam: Camera2d::default(),
            nav: Nav::default(),
            hover: None,
            hover_at: None,
            picked: None,
            hover_start: 0.0,
            hover_mix: 1.0,
            press: None,
            space_held: false,
            frame: NextFrame::default(),
            nodes: Vec::new(),
            placed: false,
            keeps_fit: false,
            covered_inset: [0.0; 4],
            inset_changed: false,
            id: NEXT_PANE.fetch_add(1, Ordering::Relaxed),
        }
    }
}

/// The stable semantic-part key of an item key.
pub(crate) fn part_key<K: Hash>(key: &K) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    // 0 is never a part key.
    h.finish().max(1)
}

pub(crate) fn node_part<K: Hash>(n: &CanvasNode<K>) -> (u64, a11y_tree::Node) {
    let mut node = a11y_tree::Node {
        role: a11y_tree::Role::Button,
        bounds: makepad_platform::a11y::semantic_rect(n.rect),
        actions: a11y_tree::Actions::PRESS,
        ..Default::default()
    };
    makepad_platform::a11y::set_string(&mut node.name, &n.name);
    makepad_platform::a11y::set_string(&mut node.role_description, n.role);
    if let Some(v) = &n.value {
        makepad_platform::a11y::set_string(&mut node.value, v);
    }
    (part_key(&n.sel), node)
}

impl<K> CanvasPane<K> {
    pub fn held_view(&self) -> Option<SavedView2d> {
        (self.placed && !self.keeps_fit).then_some(SavedView2d {
            center: self.cam.center,
            px_per_unit: self.cam.px_per_unit,
        })
    }

    /// A pane that zooms in no deeper than `max_ppu` points per world unit (the deepest zoom that
    /// still shows something new), so nothing the pane draws grows without bound.
    pub fn with_max_ppu(max_ppu: f64) -> Self {
        let mut pane = Self::default();
        pane.nav.max_ppu = max_ppu;
        pane
    }
}

impl<K: Clone + PartialEq + Hash + std::fmt::Debug> CanvasPane<K> {
    /// The hovered item.
    pub fn hovered(&self) -> Option<&K> {
        self.hover.as_ref()
    }

    /// The hover fade, 0..1 over 120 ms after the hover changed.
    pub fn hover_mix(&self) -> f32 {
        self.hover_mix as f32
    }

    /// An item drag is running.
    pub fn dragging(&self) -> Option<&K> {
        self.press.as_ref().filter(|p| p.dragging && !p.pan).and_then(|p| p.key.as_ref())
    }

    /// A press on the pane is held (a click, a drag or a pan): its moves are the pane's wherever
    /// the pointer goes.
    pub fn pressed(&self) -> bool {
        self.press.is_some()
    }

    /// The pointer (absolute), when it is over the pane.
    pub fn pointer(&self) -> Option<Vec2d> {
        self.hover_at
    }

    /// The world point under the pointer, when it is over the pane.
    pub fn pointer_world(&self) -> Option<[f64; 2]> {
        self.hover_at.map(|s| self.cam.to_world(s))
    }

    /// Records the edges covered by overlays for the next placement or fit. O(1).
    pub fn set_covered_inset(&mut self, inset: [f64; 4]) {
        if self.covered_inset != inset {
            self.covered_inset = inset;
            self.inset_changed = true;
        }
    }

    /// Fits `bounds` on first placement and when the clear area changes while the camera keeps
    /// its fit. Otherwise keeps the world point at the pane's centre. O(1) per draw.
    pub fn place(&mut self, cx: &Cx2d, rect: Rect, bounds: Option<[f64; 4]>) {
        self.place_at(rect, cx.current_dpi_factor(), bounds);
    }

    /// [`Self::place`] at `dpi` physical pixels per point.
    pub fn place_at(&mut self, rect: Rect, dpi: f64, bounds: Option<[f64; 4]>) {
        if rect.size.x <= 0.0 || rect.size.y <= 0.0 {
            if !self.placed {
                return;
            }
        } else if !self.placed || self.keeps_fit && bounds.is_some()
            && (rect.size != self.cam.view.size || dpi != self.cam.dpi || self.inset_changed) {
            self.cam = match bounds {
                Some(b) => Camera2d::fit_with_inset(rect, dpi, b, FIT_MARGIN, self.covered_inset),
                None => Camera2d { view: rect, dpi, ..Camera2d::default() },
            };
            // A fit flying in for the old size lands here at once.
            self.nav.stop();
            self.inset_changed = false;
            self.keeps_fit = bounds.is_some();
            self.placed = true;
            return;
        }
        self.cam.view = rect;
        self.cam.dpi = dpi;
    }

    /// Flies to show the whole content with `margin` points inside the clear area. The camera
    /// keeps that fit as the clear area changes until the person pans or zooms.
    pub fn fit_whole(&mut self, bounds: [f64; 4], margin: f64, now: f64) {
        self.nav.fly_to_box_with_inset(&self.cam, bounds, margin, self.covered_inset, now, None);
        self.inset_changed = false;
        self.keeps_fit = true;
    }

    /// Zooms by `factor` about the pane's centre (above 1 in, below 1 out), as a short flight
    /// (the zoom buttons). The person moved the camera.
    pub fn zoom_by(&mut self, factor: f64, now: f64) {
        self.nav.fly_to(&self.cam, self.cam.center, self.cam.px_per_unit * factor, now, Some(ZOOM_STEP_S));
        self.keeps_fit = false;
    }

    /// The person moved the camera (a pan, a wheel or pinch zoom), or a view flew it to show
    /// something other than the whole content: a resize keeps the world point at the centre
    /// from now on, not the fit.
    pub fn moved_by_hand(&mut self) {
        self.keeps_fit = false;
    }

    /// Asks for the next frame while the camera or the hover fade moves.
    pub fn animate(&mut self, cx: &mut Cx) {
        if self.nav.moving() || self.hover_mix < 1.0 {
            self.frame = cx.new_next_frame();
        }
    }

    /// Advances the camera and the fades to `now`; true when something changed (redraw).
    pub fn step(&mut self, now: f64) -> bool {
        let moved = self.nav.moving();
        self.nav.step(&mut self.cam, now);
        let fading = self.hover_mix < 1.0;
        if fading {
            self.hover_mix = ((now - self.hover_start) / STATE_SECONDS).clamp(0.0, 1.0);
        }
        moved || fading
    }

    /// The pointer went onto something the pane's owner draws over its items (a probe flag): no
    /// item is hovered any more. The hover input when one was; the cursor is the owner's.
    pub fn clear_hover(&mut self, now: f64) -> Option<CanvasInput<K>> {
        self.picked = None;
        self.hover.take()?;
        self.hover_start = now;
        self.hover_mix = 0.0;
        Some(CanvasInput::Hover(None))
    }

    fn set_hover<I: CanvasItems<Key = K>>(&mut self, cx: &mut Cx, items: &I, at: Option<Vec2d>, now: f64, out: &mut Vec<CanvasInput<K>>) {
        self.hover_at = at;
        let keys = canvas_keys(cx);
        if at.is_some() {
            keys.hovered = Some(self.id);
        } else if keys.hovered == Some(self.id) {
            keys.hovered = None;
        }
        self.picked = at.map(|s| (self.cam.to_world(s), self.cam.px_per_unit));
        let r = HIT_RADIUS / self.cam.px_per_unit.max(1e-12);
        let under = at.and_then(|s| items.hit(self.cam.to_world(s), r).into_iter().next());
        if under != self.hover {
            self.hover = under.clone();
            self.hover_start = now;
            self.hover_mix = 0.0;
            out.push(CanvasInput::Hover(under));
        }
        if self.press.is_none() {
            // A hand over what a drag would move; a pour's inside draws the marquee instead.
            let drags = at.zip(self.hover.as_ref()).is_some_and(|(s, key)| !items.marquee_over(key, self.cam.to_world(s), r));
            cx.set_cursor(if drags { MouseCursor::Hand } else { MouseCursor::Default });
        }
    }

    /// Turns one event into canvas inputs. `area` is the pane's drawn area; `items` answers the
    /// hit tests.
    pub fn handle_event<I: CanvasItems<Key = K>>(&mut self, cx: &mut Cx, event: &Event, area: Area, items: &I) -> Vec<CanvasInput<K>> {
        let mut out = Vec::new();
        if let Some(fe) = self.frame.is_event(event) {
            if self.step(fe.time) {
                out.push(CanvasInput::Camera);
            }
            self.animate(cx);
            return out;
        }
        let scroll_is_mouse = match event {
            Event::Scroll(e) => Some(e.is_mouse),
            _ => None,
        };
        let focus = self.key_focus(cx, area);
        if canvas_keys(cx).hovered == Some(self.id) && !area.is_valid(cx) {
            // Hidden while the pointer rested on it: no longer under the pointer.
            canvas_keys(cx).hovered = None;
        }
        let hovered = canvas_keys(cx).hovered;
        if let Event::KeyDown(k) = event {
            // The focused pane's keys come as hits below; a key for the hovered one comes here.
            if focus != KeyFocus::Mine && takes_key(self.id, hovered, focus) {
                cx.set_key_focus(area);
                self.key_down(k, &mut out);
                return out;
            }
        }
        match event.hits(cx, area) {
            // A move that leaves the pointer over the same world point at the same zoom (a still
            // pointer's repeated events) keeps the hover as it is: no pick, no restarted fade.
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) if self.picked != Some((self.cam.to_world(e.abs), self.cam.px_per_unit)) => {
                self.set_hover(cx, items, Some(e.abs), e.time, &mut out);
                self.animate(cx);
            }
            Hit::FingerHoverOut(e) => {
                self.set_hover(cx, items, None, e.time, &mut out);
            }
            Hit::FingerDown(e) => {
                cx.set_key_focus(area);
                let button = e.device.mouse_button();
                let pan_button = button.is_some_and(|b| b.contains(MouseButton::MIDDLE) || b.contains(MouseButton::SECONDARY));
                let pan = self.space_held || e.modifiers.alt || pan_button;
                self.nav.stop();
                let r = HIT_RADIUS / self.cam.px_per_unit.max(1e-12);
                let world = self.cam.to_world(e.abs);
                let key = if pan { None } else { items.hit(world, r).into_iter().next() };
                let marquee = key.as_ref().is_some_and(|key| items.marquee_over(key, world, r));
                if pan {
                    self.moved_by_hand();
                    self.nav.drag_begin(e.time);
                    cx.set_cursor(MouseCursor::Grabbing);
                }
                if let Some(key) = &key { out.push(CanvasInput::Press { key: key.clone(), down: true }); }
                self.press = Some(Press { key, start: e.abs, last: e.abs, pan, dragging: false, clicks: e.tap_count.max(1), shift: e.modifiers.shift, marquee });
            }
            Hit::FingerMove(e) => {
                let cam = self.cam;
                let Some(press) = self.press.as_mut() else { return out };
                let delta = e.abs - press.last;
                press.last = e.abs;
                if press.pan {
                    self.nav.drag_move(&mut self.cam, delta, e.time);
                    out.push(CanvasInput::Camera);
                    return out;
                }
                if !press.dragging && (e.abs - press.start).length() > DRAG_THRESHOLD {
                    press.dragging = true;
                    match press.drags() {
                        Some(key) => {
                            cx.set_cursor(MouseCursor::Grabbing);
                            out.push(CanvasInput::DragStart { key: key.clone(), from: cam.to_world(press.start), at: cam.to_world(e.abs) });
                        }
                        None => cx.set_cursor(MouseCursor::Crosshair),
                    }
                }
                if press.dragging {
                    match press.drags() {
                        Some(key) => out.push(CanvasInput::DragMove { key: key.clone(), at: cam.to_world(e.abs) }),
                        None => out.push(CanvasInput::Marquee {
                            rect: cam.rect_to_world(Rect { pos: press.start, size: e.abs - press.start }),
                            done: false, shift: press.shift, crossing: e.abs.x < press.start.x,
                        }),
                    }
                }
            }
            Hit::FingerUp(e) => {
                let Some(press) = self.press.take() else { return out };
                if let Some(key) = &press.key { out.push(CanvasInput::Press { key: key.clone(), down: false }); }
                if press.pan {
                    self.nav.drag_end(e.time);
                    self.animate(cx);
                } else if press.dragging {
                    match press.drags() {
                        Some(key) => out.push(CanvasInput::DragEnd { key: key.clone(), at: self.cam.to_world(e.abs) }),
                        None => out.push(CanvasInput::Marquee {
                            rect: self.cam.rect_to_world(Rect { pos: press.start, size: e.abs - press.start }),
                            done: true, shift: press.shift, crossing: e.abs.x < press.start.x,
                        }),
                    }
                } else {
                    out.push(CanvasInput::Click { key: press.key, at: self.cam.to_world(e.abs), clicks: press.clicks, shift: press.shift });
                }
                self.set_hover(cx, items, Some(e.abs), e.time, &mut out);
            }
            Hit::FingerScroll(e) => {
                let zoom = scroll_is_mouse.unwrap_or(false) || e.modifiers.control || e.modifiers.logo;
                self.moved_by_hand();
                if zoom {
                    self.nav.wheel(e.abs, e.scroll.y, e.time);
                    self.animate(cx);
                } else {
                    self.nav.scroll_pan(&mut self.cam, dvec2(-e.scroll.x, -e.scroll.y));
                    out.push(CanvasInput::Camera);
                }
            }
            Hit::FingerPinch(e) => {
                self.moved_by_hand();
                self.nav.pinch(&mut self.cam, e.abs, e.scale);
                out.push(CanvasInput::Camera);
            }
            Hit::KeyDown(k) => {
                if takes_key(self.id, hovered, focus) {
                    self.key_down(&k, &mut out);
                }
            }
            Hit::KeyUp(k) if k.key_code == KeyCode::Space => {
                self.space_held = false;
            }
            _ => {}
        }
        out
    }

    fn key_down(&mut self, k: &KeyEvent, out: &mut Vec<CanvasInput<K>>) {
        if k.key_code == KeyCode::Space {
            self.space_held = true;
        } else if k.key_code == KeyCode::Escape {
            // Escape ends the press: its item is released before any drag is cancelled.
            let press = self.press.take();
            if let Some(key) = press.as_ref().and_then(|press| press.key.clone()) { out.push(CanvasInput::Press { key, down: false }); }
            match press {
                Some(Press { key: Some(key), dragging: true, pan: false, marquee: false, .. }) => out.push(CanvasInput::DragCancel { key }),
                Some(_) => {}
                None => out.push(CanvasInput::Escape),
            }
        } else if !k.is_repeat {
            out.push(CanvasInput::Key { code: k.key_code, modifiers: k.modifiers, under: self.hover.clone() });
        }
    }

    /// Who holds the key focus as this pane sees it. O(1).
    fn key_focus(&self, cx: &Cx, area: Area) -> KeyFocus {
        if cx.has_key_focus(area) {
            KeyFocus::Mine
        } else if cx.key_focus_takes_text() {
            KeyFocus::Text
        } else {
            KeyFocus::Elsewhere
        }
    }

    /// Gathers the semantic nodes of what the camera shows (call at draw time).
    pub fn gather_nodes<I: CanvasItems<Key = K>>(&mut self, items: &I) {
        self.nodes = items.nodes(&self.cam).into_iter().map(|n| (part_key(&n.sel), n)).collect();
    }

    /// The gathered nodes as the widget's semantic parts (`Widget::semantics_parts`).
    pub fn semantics_parts(&self, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for (_, n) in &self.nodes {
            let (key, node) = node_part(n);
            out.push((key, None, node));
        }
    }

    /// A remote or assistive press on one of the nodes: the click it stands for.
    pub fn a11y_press(&self, cx: &Cx, request: &a11y_tree::ActionRequest) -> Option<CanvasInput<K>> {
        if request.action != a11y_tree::Actions::PRESS {
            return None;
        }
        let key = cx.a11y.action_part_key(request.node)?;
        let (_, n) = self.nodes.iter().find(|(k, _)| *k == key)?;
        let centre = n.rect.center();
        Some(CanvasInput::Click { key: Some(n.sel.clone()), at: self.cam.to_world(centre), clicks: 1, shift: false })
    }
}

fn canvas_keys(cx: &mut Cx) -> &mut CanvasKeys {
    if !cx.has_global::<CanvasKeys>() {
        cx.set_global(CanvasKeys::default());
    }
    cx.get_global::<CanvasKeys>()
}

#[cfg(test)]
mod tests {
    use super::{takes_key, CanvasPane, KeyFocus, SavedView2d, ViewOnArrival};
    use makepad_widgets_core::*;
    use std::sync::Arc;

    /// The content's box shows whole inside the pane's view, margin included.
    fn shows_whole(pane: &CanvasPane<u32>, b: [f64; 4]) -> bool {
        let (lo, hi) = (pane.cam.to_screen([b[0], b[3]]), pane.cam.to_screen([b[2], b[1]]));
        let v = pane.cam.view;
        lo.x >= v.pos.x - 1e-6 && lo.y >= v.pos.y - 1e-6 && hi.x <= v.pos.x + v.size.x + 1e-6 && hi.y <= v.pos.y + v.size.y + 1e-6
    }

    /// The sheet fits the clear area through resizes and overlay changes until the person moves
    /// the camera. A fit of the whole restores that behaviour.
    #[test]
    fn a_canvas_keeps_its_content_whole_through_resizes_until_moved_by_hand() {
        // Wide content, so the pane's width decides the fit.
        let content = [0.0, 0.0, 800.0, 200.0];
        let wide = Rect { pos: dvec2(10.0, 20.0), size: dvec2(1600.0, 600.0) };
        let narrow = Rect { pos: dvec2(10.0, 20.0), size: dvec2(1360.0, 600.0) };
        let mut pane = CanvasPane::<u32>::default();
        pane.place_at(Rect::default(), 2.0, Some(content));
        assert!(!pane.placed, "an unsized pane waits for its size");
        assert_eq!(pane.held_view(), None, "an unplaced pane has no view to save");
        pane.place_at(wide, 2.0, Some(content));
        assert!(pane.placed && pane.keeps_fit && shows_whole(&pane, content), "the first show fits the content");
        assert_eq!(pane.held_view(), None, "an automatic fit has no view to save");
        let fitted_wide = pane.cam.px_per_unit;
        pane.place_at(narrow, 2.0, Some(content));
        assert!(shows_whole(&pane, content), "a narrower pane fits it again: nothing is cut at the new edge");
        assert!(pane.cam.px_per_unit < fitted_wide, "by zooming out");
        pane.place_at(wide, 2.0, Some(content));
        assert_eq!(pane.cam.px_per_unit, fitted_wide, "a wider pane fits it again too");
        let whole_pane_fit = pane.cam;
        let inset = [200.0, 40.0, 80.0, 100.0];
        pane.set_covered_inset(inset);
        pane.place_at(wide, 2.0, Some(content));
        let clear = Rect { pos: wide.pos + dvec2(200.0, 40.0), size: wide.size - dvec2(280.0, 140.0) };
        let inset_fit = pane.cam;
        let shown = inset_fit.rect_to_screen(content);
        let margin = super::FIT_MARGIN;
        assert_eq!(inset_fit.view, wide, "drawing and picking still use the whole pane");
        assert!((shown.center() - clear.center()).length() < 1e-6, "changing only the inset centres the content in the clear area");
        assert!((shown.size.x + 2.0 * margin - clear.size.x).abs() < 1e-6, "wide content fills the clear width with the fit margin");
        assert!(shown.size.y + 2.0 * margin <= clear.size.y + 1e-6, "the vertical margins also fit inside the clear area");
        for covered in [[1201.0, 0.0, 0.0, 100.0], [200.0, 0.0, 0.0, 451.0]] {
            pane.set_covered_inset(covered);
            pane.place_at(wide, 2.0, Some(content));
            assert_eq!(pane.cam, whole_pane_fit, "less than a quarter clear on either axis fits the whole pane for {covered:?}");
        }
        pane.moved_by_hand();
        pane.cam.center = [300.0, 100.0];
        let (center, ppu) = (pane.cam.center, pane.cam.px_per_unit);
        let held = pane.held_view().expect("a panned view is saved");
        assert_eq!(held, SavedView2d { center, px_per_unit: ppu });
        let size = dvec2(4.0 * ppu, 2.0 * ppu);
        assert!(held.shows(size, [301.0, 100.5, 303.0, 102.0]), "even a partial overlap shows content");
        assert!(!held.shows(size, [301.0, 102.0, 303.0, 104.0]), "the bounds are outside the visible height");
        assert!(!held.shows(size, [900.0, 100.0, 901.0, 101.0]), "distant content is off screen");
        assert!(!held.shows(size, [300.0, 99.0, 300.0, 101.0]), "zero-width bounds show nothing");
        assert!(!held.shows(size, [299.0, 101.0, 301.0, 99.0]), "inverted bounds show nothing");
        pane.set_covered_inset(inset);
        pane.place_at(wide, 2.0, Some(content));
        assert_eq!((pane.cam.center, pane.cam.px_per_unit), (center, ppu), "changing an overlay preserves a panned camera");
        pane.place_at(narrow, 2.0, Some(content));
        assert_eq!((pane.cam.center, pane.cam.px_per_unit, pane.cam.view), (center, ppu, narrow), "once panned, a resize keeps the person's view");
        pane.fit_whole(content, margin, 0.0);
        pane.nav.step(&mut pane.cam, 10.0);
        let target = super::Camera2d::fit_with_inset(narrow, 2.0, content, margin, inset);
        assert_eq!(pane.cam, target, "an explicit fit lands at the clear-area centre and zoom");
        pane.place_at(wide, 2.0, Some(content));
        assert_eq!(pane.cam, inset_fit, "resizing after a whole-content flight restores the clear-area fit");
        pane.set_covered_inset([0.0; 4]);
        pane.place_at(wide, 2.0, Some(content));
        assert!(pane.keeps_fit && shows_whole(&pane, content) && pane.cam.px_per_unit == fitted_wide, "a fit of the whole keeps through resizes again");

        let old = Arc::new(content);
        let new = Arc::new(content);
        let opening = ViewOnArrival::new(Some(held), Some(&old)).unwrap();
        assert!(!opening.prepare(&mut pane, &old, narrow.size, content), "the outgoing drawing keeps its camera");
        assert!(!opening.prepare(&mut pane, &new, narrow.size, [0.0; 4]), "an empty layout leaves the restore pending");
        assert!(!opening.prepare(&mut pane, &new, Vec2d::default(), content), "an unsized body leaves the restore pending");
        pane.zoom_by(2.0, 0.0);
        assert!(opening.prepare(&mut pane, &new, narrow.size, content), "new content can restore even at identical bounds");
        pane.place_at(narrow, 1.0, Some(content));
        assert_eq!((pane.held_view(), pane.cam.view, pane.cam.dpi), (Some(held), narrow, 1.0));
        assert!(!pane.nav.moving(), "a flight cannot move the restored camera");
    }

    #[test]
    fn a_key_goes_to_one_canvas_and_never_from_a_text_taker() {
        let (board, sheet) = (1, 2);
        // A text field or code editor holds focus with the pointer resting over the board.
        assert!(!takes_key(board, Some(board), KeyFocus::Text), "a text taker keeps its keys while the pointer is over a canvas");
        assert!(!takes_key(sheet, Some(board), KeyFocus::Text), "and no other canvas takes them");
        // Nothing, a button, Fab's viewport or another canvas holds the focus.
        assert!(takes_key(board, Some(board), KeyFocus::Elsewhere), "with the focus on no text taker, the canvas under the pointer takes the key");
        assert!(!takes_key(board, None, KeyFocus::Elsewhere), "with the focus elsewhere and no canvas under the pointer, nobody does");
        assert!(takes_key(board, None, KeyFocus::Mine), "the focused canvas keeps its keys while the pointer is elsewhere");
        // The sheet holds the focus and the pointer is over the board: only the board acts.
        assert!(!takes_key(sheet, Some(board), KeyFocus::Mine), "the focused canvas gives way to the hovered one");
        for hovered in [None, Some(board), Some(sheet)] {
            for (focus_board, focus_sheet) in [(KeyFocus::Elsewhere, KeyFocus::Elsewhere), (KeyFocus::Mine, KeyFocus::Elsewhere), (KeyFocus::Elsewhere, KeyFocus::Mine), (KeyFocus::Text, KeyFocus::Text)] {
                let n = takes_key(board, hovered, focus_board) as u32 + takes_key(sheet, hovered, focus_sheet) as u32;
                assert!(n <= 1, "at most one canvas takes a key (hovered {hovered:?}, focus {focus_board:?}/{focus_sheet:?})");
            }
        }
    }
}

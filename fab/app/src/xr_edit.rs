//! Mixed-reality editing in the headset: what the hands do to the plate's parts and to the
//! slice, as plain state stepped once per headset frame ([`XrEdit::step`]). No drawing and no
//! document access here: the widget (`xr.rs`) feeds the frame in, draws what this says and sends
//! the commits through the model worker's edit path, so this runs the same under a recorded or
//! synthetic input stream as it does on a headset.
//!
//! Gestures (either hand; tracked-hand equivalents in brackets). The touch marker is the
//! controller's front aim point or the index fingertip; a pinching hand carries things at its
//! thumb/index midpoint. The marker lights what it would take:
//! - Trigger or grip [index pinch] on or near a part: grab it, carrying its selection too.
//!   A grip can also grab by aim. Move and turn the grab point to carry the part; small tilt
//!   is cancelled, deliberate tilt is kept (and laid flat on the nearest face on commit).
//!   Parts never sink into the plate. Releasing drops them and commits one undo step.
//! - The other trigger near a held part, or the other grip [index pinch]: two-hand move,
//!   turn about the vertical and scale about the grab points' midpoint.
//! - Grip [middle pinch] in empty space: shared navigation (move, turn, scale the plate).
//! - An unclaimed trigger [index pinch] aiming at a distant part selects it; at nothing it
//!   clears selection. A press on a wrist-menu cell activates only that cell. Existing grabs
//!   and touched draggables claim the press before menu activation or selection.
//! - Thumbstick click held [ring pinch], in Preview only (elsewhere it does nothing):
//!   grab the slice level. Hand height picks the layer
//!   (`layer_gain_mm` of travel per layer, five times finer with the trigger half pressed),
//!   moving across picks the point along the layer's toolpath. A tick on every layer, a stronger
//!   one at a layer with issues. Releasing keeps the level.
//! - Y [the left palm turned up]: the wrist menu (`makepad_xr_ui`, the widget's rows). With it
//!   open the left stick steps layers. B: undo, X: play/pause the slice (in Preview).
//!
//! The workspace (`AppState::ui.workspaces`, the desktop's) changes only through the wrist menu's
//! rows: no button or gesture switches it. The plate mode in force (`UiState::mode`) comes into
//! every step: parts move in Design; Monitor shows them and Mechanics' hands are the sim's
//! (`crate::xr_mech`).
//!
//! Per frame: constant work per hand plus one bounds test per part; no allocation except when a
//! grab starts (the selection snapshot) or commits (the move list).

use fab::api::{PlateMode, Workspace};
use fab_plate::Bounds;
use makepad_widgets::makepad_platform::event::xr::{XrController, XrHand, XrState};
use makepad_widgets::*;
use makepad_xr_nav::{dead_zoned, XrNavView};
use makepad_xr_ui::haptic::{self, Haptics};
use makepad_xr_ui::input::{aim, grab, tip, trigger, Buttons, PRESS};
use makepad_xr_ui::{GrabEdits, MenuStep};

/// Row-major affine in printer mm (Z up), the same as [`crate::live_slice::PartTransform`].
pub type Affine = [[f64; 4]; 3];
pub const IDENTITY: Affine = crate::live_slice::IDENTITY;

/// A tilt under this (degrees) is the hand's wobble, not a turn: it is cancelled.
const TILT_DEADBAND_DEG: f64 = 12.0;
/// How near the hand's tip must be to a part to grab it (room metres).
const REACH_M: f32 = 0.03;
/// The longest aim that grabs or selects (room metres).
const RAY_M: f32 = 4.0;
/// Across-the-layer travel that covers a whole layer's toolpath (room metres).
const ALONG_FULL_M: f32 = 0.15;

/// What the menu and the face buttons ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    SetWorkspace(Workspace),
    /// The Slice render mode on or off (Design's row).
    SliceView(bool),
    /// The plate back to real size (about where it stands).
    RealSize,
    TogglePlay,
    Undo,
    Redo,
    /// Go to an issue (index into the report's issues).
    Issue(usize),
    PrevIssue,
    NextIssue,
    /// The left stick stepped the layer (+1 up).
    StepLayer(i32),
}

/// The slice as the hands see it: how many layers, where the playhead is.
#[derive(Clone, Copy, Debug, Default)]
pub struct SliceAxis<'a> {
    pub layers: usize,
    /// The current layer (index) and the point along its toolpath (0..1).
    pub layer: usize,
    pub along: f32,
    /// Layer indices with printability issues, ascending.
    pub issue_layers: &'a [u32],
}

/// A hand's grab of parts.
#[derive(Clone, Debug)]
struct Hold {
    /// The hand's pose in printer mm when the grab (or its last re-reference) started.
    from: Affine,
    /// The grabbed parts' deltas then.
    deltas: Vec<(usize, Affine)>,
}

#[derive(Clone, Debug)]
struct TwoHands {
    mid: Vec3f,
    span: Vec3f,
    deltas: Vec<(usize, Affine)>,
}

#[derive(Clone, Copy, Debug)]
struct LayerGrab {
    hand: usize,
    y: f32,
    across: f32,
    right: Vec3f,
    layer: f32,
    along: f32,
    shown: usize,
}

/// The per-frame answer, reused frame to frame.
#[derive(Debug, Default)]
pub struct EditOut {
    /// Hands whose grip the navigation may use (not holding parts).
    pub nav_grip: [bool; 2],
    /// Hands whose stick the navigation may use.
    pub nav_stick: [bool; 2],
    /// A finished grab: the moved parts and their deltas, to commit as one edit.
    pub commit: Option<Vec<(usize, Affine)>>,
    /// The parts' deltas changed this frame (draw them, tell the live slice).
    pub moved: bool,
    pub selection_changed: bool,
    /// The layer grab moved the playhead: (layer index, along 0..1).
    pub scrub: Option<(usize, f32)>,
    pub haptics: Haptics,
    pub command: Option<Command>,
    /// The layer grab's hand (room metres) while it is held.
    pub readout_at: Option<Vec3f>,
    /// The part each free hand's tip is on or near (it would take it), or the hand holds.
    pub touch: [Option<usize>; 2],
}

/// One headset's editing state.
pub struct XrEdit {
    pub selected: Vec<bool>,
    /// Each part's delta on top of the plate as evaluated (printer mm).
    pub deltas: Vec<Affine>,
    /// Parts whose committed move waits for the rebuilt plate.
    pub edits: GrabEdits,
    /// Hand travel per layer (mm).
    pub layer_gain_mm: f32,
    holds: [Option<Hold>; 2],
    two: Option<TwoHands>,
    /// The parts the grab in progress moves.
    grab_ids: Vec<usize>,
    /// The hand grabbing the navigation (so it is not taken for a part grab while held).
    nav_held: [bool; 2],
    grip_down: [bool; 2],
    trigger_down: [bool; 2],
    stick_click: [bool; 2],
    layer: Option<LayerGrab>,
    stick_step_at: f64,
}

impl Default for XrEdit {
    fn default() -> Self {
        Self {
            selected: Vec::new(),
            deltas: Vec::new(),
            edits: GrabEdits::default(),
            layer_gain_mm: 1.0,
            holds: [None, None],
            two: None,
            grab_ids: Vec::new(),
            nav_held: [false; 2],
            grip_down: [false; 2],
            trigger_down: [false; 2],
            stick_click: [false; 2],
            layer: None,
            stick_step_at: 0.0,
        }
    }
}

// ---- printer mm <-> room -------------------------------------------------------------------

/// Printer mm (Z up) to the world (glTF frame: metres, Y up).
fn world_of(p: [f64; 3]) -> Vec3f {
    vec3f((p[0] * 0.001) as f32, (p[2] * 0.001) as f32, (-p[1] * 0.001) as f32)
}

fn printer_of(w: Vec3f) -> [f64; 3] {
    [w.x as f64 * 1000.0, -w.z as f64 * 1000.0, w.y as f64 * 1000.0]
}

pub fn room_from_printer(view: &XrNavView, p: [f64; 3]) -> Vec3f {
    view.room_from_world(world_of(p))
}

pub fn printer_from_room(view: &XrNavView, r: Vec3f) -> [f64; 3] {
    printer_of(view.world_from_room(r))
}

/// A room direction in printer axes (unscaled).
fn printer_dir(view: &XrNavView, d: Vec3f) -> [f64; 3] {
    let (s, c) = (view.yaw.sin(), view.yaw.cos());
    // inverse of R_y(yaw): world = (c x - s z, y, s x + c z)
    let w = vec3f(d.x * c - d.z * s, d.y, d.x * s + d.z * c);
    [w.x as f64, -w.z as f64, w.y as f64]
}

/// A room pose as a rigid printer-mm transform (rotation columns: the pose's axes).
pub fn pose_affine(view: &XrNavView, orientation: Quat, position: Vec3f) -> Affine {
    let p = printer_from_room(view, position);
    let ax: [[f64; 3]; 3] = [vec3f(1.0, 0.0, 0.0), vec3f(0.0, 1.0, 0.0), vec3f(0.0, 0.0, 1.0)].map(|e| printer_dir(view, orientation.rotate_vec3(&e)));
    // the room's axes map to printer axes by `printer_dir`; express the pose in printer axes:
    // R_p = B^T Q B, with B taking printer axes to room axes.
    let b: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].map(|e| {
        let w = world_of([e[0] * 1000.0, e[1] * 1000.0, e[2] * 1000.0]);
        let r = view.room_from_world(w) - view.room_from_world(Vec3f::default());
        let n = r.length().max(1e-9);
        [(r.x / n) as f64, (r.y / n) as f64, (r.z / n) as f64]
    });
    // column j of R_p = sum_k b_j[k] * (printer image of room axis k)
    let mut m = [[0.0; 4]; 3];
    for j in 0..3 {
        for i in 0..3 {
            m[i][j] = (0..3).map(|k| b[j][k] * ax[k][i]).sum();
        }
    }
    for i in 0..3 {
        m[i][3] = p[i];
    }
    m
}

pub fn mul(a: &Affine, b: &Affine) -> Affine {
    let mut m = [[0.0; 4]; 3];
    for i in 0..3 {
        for j in 0..4 {
            m[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum::<f64>() + if j == 3 { a[i][3] } else { 0.0 };
        }
    }
    m
}

/// The inverse of a turn, a uniform scale and a shift.
pub fn inverse(a: &Affine) -> Affine {
    let k2 = a[0][0] * a[0][0] + a[1][0] * a[1][0] + a[2][0] * a[2][0];
    let k2 = if k2 > 1e-18 { k2 } else { 1.0 };
    let mut m = [[0.0; 4]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = a[j][i] / k2;
        }
    }
    for i in 0..3 {
        m[i][3] = -(0..3).map(|k| m[i][k] * a[k][3]).sum::<f64>();
    }
    m
}

pub fn apply(a: &Affine, p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| a[i][0] * p[0] + a[i][1] * p[1] + a[i][2] * p[2] + a[i][3])
}

fn scale_of(a: &Affine) -> f64 {
    (a[0][0] * a[0][0] + a[1][0] * a[1][0] + a[2][0] * a[2][0]).sqrt()
}

/// The box's corners moved by `a`: their bounds.
pub fn moved_bounds(b: &Bounds, a: &Affine) -> Bounds {
    let mut out = Bounds::EMPTY;
    for i in 0..8 {
        let c = [if i & 1 == 0 { b.min[0] } else { b.max[0] }, if i & 2 == 0 { b.min[1] } else { b.max[1] }, if i & 4 == 0 { b.min[2] } else { b.max[2] }];
        out.add_point(apply(a, c));
    }
    out
}

/// Keep the delta upright when its tilt is within the deadband (about the part's centre `c`).
fn untilt(a: &mut Affine, c: [f64; 3]) {
    let k = scale_of(a).max(1e-9);
    let z = [a[0][2] / k, a[1][2] / k, a[2][2] / k];
    if z[2].clamp(-1.0, 1.0).acos().to_degrees() >= TILT_DEADBAND_DEG {
        return;
    }
    let moved_c = apply(a, c);
    let yaw = a[1][0].atan2(a[0][0]);
    let (s, co) = (yaw.sin() * k, yaw.cos() * k);
    *a = [[co, -s, 0.0, 0.0], [s, co, 0.0, 0.0], [0.0, 0.0, k, 0.0]];
    let now_c = apply(a, c);
    for i in 0..3 {
        a[i][3] = moved_c[i] - now_c[i];
    }
}

/// Lift the delta so the part's (moved) box does not go under the plate.
fn above_plate(a: &mut Affine, b: &Bounds) {
    let lo = moved_bounds(b, a).min[2];
    if lo < 0.0 {
        a[2][3] -= lo;
    }
}

/// Drop the delta onto the plate (what the committed edit does exactly).
pub fn onto_plate(a: &mut Affine, b: &Bounds) {
    let lo = moved_bounds(b, a).min[2];
    a[2][3] -= lo;
}

/// Where a ray (printer mm) first enters the box moved by `a`, as a distance along `dir`.
fn ray_box(origin: [f64; 3], dir: [f64; 3], b: &Bounds, a: &Affine) -> Option<f64> {
    let inv = inverse(a);
    let o = apply(&inv, origin);
    let d = {
        let mut lin = inv;
        for r in lin.iter_mut() {
            r[3] = 0.0;
        }
        apply(&lin, dir)
    };
    let (mut t0, mut t1) = (0.0f64, f64::INFINITY);
    for i in 0..3 {
        if d[i].abs() < 1e-12 {
            if o[i] < b.min[i] || o[i] > b.max[i] {
                return None;
            }
            continue;
        }
        let (mut a0, mut a1) = ((b.min[i] - o[i]) / d[i], (b.max[i] - o[i]) / d[i]);
        if a0 > a1 {
            std::mem::swap(&mut a0, &mut a1);
        }
        t0 = t0.max(a0);
        t1 = t1.min(a1);
        if t0 > t1 {
            return None;
        }
    }
    Some(t0)
}

fn inside(p: [f64; 3], b: &Bounds, a: &Affine, margin: f64) -> bool {
    let q = apply(&inverse(a), p);
    let m = margin / scale_of(a).max(1e-9);
    (0..3).all(|i| q[i] >= b.min[i] - m && q[i] <= b.max[i] + m)
}

// ---- input per hand ------------------------------------------------------------------------

fn layer_grip(c: &XrController, h: &XrHand) -> Option<Vec3f> {
    if c.active() {
        return (c.buttons & XrController::CLICK_THUMBSTICK != 0).then_some(c.grip_pose.position);
    }
    h.pinch_ring().then(|| h.tracking_pose().map(|p| p.position)).flatten()
}

impl XrEdit {
    /// The held part takes priority over a selection made before the grab.
    pub fn part_target(&self) -> Option<usize> {
        self.holds.iter().flatten().find_map(|h| h.deltas.first().map(|(i, _)| *i))
            .or_else(|| self.selected.iter().position(|selected| *selected))
    }

    /// A new plate: parts and their deltas follow it. Deltas of parts committed by `job` or
    /// earlier are folded into the plate now; a part still held keeps its delta.
    pub fn plate(&mut self, parts: usize, job: u64, same_parts: bool) {
        if !same_parts {
            self.selected = vec![false; parts];
            self.deltas = vec![IDENTITY; parts];
            self.edits.reset(parts);
            self.holds = [None, None];
            self.two = None;
            return;
        }
        self.selected.resize(parts, false);
        self.deltas.resize(parts, IDENTITY);
        self.edits.resize(parts);
        let holds = &self.holds;
        let deltas = &mut self.deltas;
        self.edits.settle(job, |i| holds.iter().flatten().any(|h| h.deltas.iter().any(|(j, _)| *j == i)), |i| deltas[i] = IDENTITY);
    }

    pub fn held(&self, part: usize) -> bool {
        self.holds.iter().flatten().any(|h| h.deltas.iter().any(|(i, _)| *i == part))
    }

    pub fn grabbing(&self) -> bool {
        self.holds.iter().any(|h| h.is_some())
    }

    /// Existing captures have priority over new handles/menu presses, including release.
    pub fn held_hands(&self) -> [bool; 2] {
        [0, 1].map(|i| self.holds[i].is_some() || self.layer.is_some_and(|g| g.hand == i))
    }

    /// Visual feedback for the existing free part grabs. These use the desktop transform
    /// vocabulary; they do not add axis constraints or change the part-bounds hit test.
    pub fn draw_grab_controls(&self, shapes: &mut makepad_xr_ui::XrShapes, tips: [Option<Pose>; 2], touch: [Option<usize>; 2], mode: fab::api::GizmoMode, head: Vec3f, time: f64) {
        use fab::handle_style::*;
        use crate::xr_handles::{ball_radius, shape_style};
        for i in 0..2 {
            let Some(tip) = tips[i].filter(|_| touch[i].is_some() || self.holds[i].is_some()) else { continue };
            let at = tip.position;
            let visual = if self.holds[i].is_some() { HandleState::Active } else { HandleState::Hover };
            let r = ball_radius((at - head).length(), REACH_M) * visual.scale();
            let n = (head - at).normalize();
            let x = tip.orientation.rotate_vec3(&vec3f(1.0, 0.0, 0.0));
            let y = tip.orientation.rotate_vec3(&vec3f(0.0, 1.0, 0.0));
            match mode {
                fab::api::GizmoMode::Move => {
                    for (axis, col) in [(x, AXIS_X), (y, AXIS_Y)] {
                        let style = shape_style(col, visual, time);
                        shapes.segment(at, at + axis * r * 3.0, r * 0.18, style);
                        shapes.cone(at + axis * r * 2.6, at + axis * r * 4.0, r * 0.6, style);
                    }
                    shapes.sphere(at, r, shape_style(MOVE_FREE, visual, time));
                }
                fab::api::GizmoMode::Rotate => {
                    let style = shape_style(RING, visual, time);
                    shapes.ring(at - x * r * 2.5, Vec3f::cross(x, y), r * 2.5, r * 0.18, style);
                    shapes.sphere(at, r, style);
                }
                fab::api::GizmoMode::Scale => {
                    let style = shape_style(SCALE, visual, time);
                    shapes.segment(at - x * r * 3.0, at, r * 0.15, style);
                    shapes.badge(at - x * r * 2.5, x, y, vec3f(r * 0.7, r * 0.7, r * 0.2), r * 0.2, style);
                    shapes.sphere(at, r, style);
                }
                _ => {
                    shapes.ring(at, n, r * 1.8, r * 0.18, shape_style(GRAB_HALO, visual, time));
                    shapes.sphere(at, r, shape_style(GRAB_BALL, visual, time));
                }
            }
        }
        if self.two.is_some() {
            if let [Some(a), Some(b)] = tips {
                let r = ball_radius(((a.position + b.position) * 0.5 - head).length(), REACH_M);
                shapes.segment(a.position, b.position, r * 0.12, shape_style(SCALE, HandleState::Active, time));
            }
        }
    }

    pub fn layer_grab(&self) -> bool {
        self.layer.is_some()
    }

    /// One headset frame in `workspace`. `parts`: each part's bounds as evaluated (printer mm); `buttons`: the
    /// frame's face button presses; `menu`: the wrist menu's frame (its presses are the
    /// widget's).
    #[allow(clippy::too_many_arguments)]
    pub fn step(&mut self, mode: Option<PlateMode>, state: &XrState, view: &XrNavView, parts: &[Bounds], slice: Option<SliceAxis>, now: f64, buttons: &mut Buttons, menu: &MenuStep, out: &mut EditOut) {
        out.commit = None;
        out.moved = false;
        out.selection_changed = false;
        out.scrub = None;
        out.haptics = Haptics::default();
        out.command = None;
        out.readout_at = None;
        out.touch = [None, None];
        if self.deltas.len() != parts.len() {
            self.plate(parts.len(), 0, false);
        }
        let ctrl = [&state.left_controller, &state.right_controller];
        let hand = [&state.left_hand, &state.right_hand];
        let mm_per_m = 1000.0 / view.scale.max(1e-6) as f64;
        // the parts are there to shape and place, not while a print or the sim shows them
        let movable = mode == Some(PlateMode::Design);

        // face buttons (Y, the menu, and the left menu button, the stream's on/off switch, are
        // not the editing's)
        if buttons.hit(0, XrController::CLICK_X) && slice.is_some() {
            out.command = Some(Command::TogglePlay);
        }
        if buttons.hit(1, XrController::CLICK_B) {
            out.command = Some(Command::Undo);
        }

        // the layer grab (thumbstick click / ring pinch): only over the slice
        out.nav_stick = [true, true];
        if let Some(axis) = slice.filter(|s| s.layers > 0) {
            match self.layer {
                None => {
                    for i in 0..2 {
                        let now_down = layer_grip(ctrl[i], hand[i]).is_some();
                        if now_down && !self.stick_click[i] {
                            let p = layer_grip(ctrl[i], hand[i]).unwrap();
                            let fwd = state.head_pose.orientation.rotate_vec3(&vec3f(0.0, 0.0, -1.0));
                            let flat = vec3f(fwd.x, 0.0, fwd.z);
                            let flat = if flat.length() > 1e-4 { flat.normalize() } else { vec3f(0.0, 0.0, -1.0) };
                            let right = vec3f(-flat.z, 0.0, flat.x);
                            self.layer = Some(LayerGrab { hand: i, y: p.y, across: p.dot(right), right, layer: axis.layer as f32, along: axis.along, shown: axis.layer });
                            out.nav_stick[i] = false;
                            out.readout_at = Some(p);
                            break;
                        }
                    }
                }
                Some(mut g) => match layer_grip(ctrl[g.hand], hand[g.hand]) {
                    None => self.layer = None,
                    Some(p) => {
                        let fine = ctrl[g.hand].active() && (0.15..0.85).contains(&ctrl[g.hand].trigger);
                        let gain_m = self.layer_gain_mm.max(0.1) * 0.001 * if fine { 5.0 } else { 1.0 };
                        let target = (g.layer + (p.y - g.y) / gain_m).clamp(0.0, (axis.layers - 1) as f32);
                        // re-reference so a clamp at either end does not store travel
                        if target <= 0.0 || target >= (axis.layers - 1) as f32 {
                            g.layer = target;
                            g.y = p.y;
                        }
                        // hysteresis: a layer is taken once the hand is 0.3 layer past its edge
                        let shown = g.shown as f32;
                        let next = if target > shown + 0.8 || target < shown - 0.8 { target.round() as usize } else { g.shown };
                        let across = p.dot(g.right);
                        let along_full = ALONG_FULL_M * if fine { 5.0 } else { 1.0 };
                        let along = (g.along + (across - g.across) / along_full).clamp(0.0, 1.0);
                        if along <= 0.0 || along >= 1.0 {
                            g.along = along;
                            g.across = across;
                        }
                        if next != g.shown {
                            let issue = axis.issue_layers.binary_search(&(next as u32)).is_ok();
                            out.haptics.add(g.hand, if issue { haptic::LIMIT } else { haptic::SNAP });
                            g.shown = next;
                        }
                        if next != axis.layer || (along - axis.along).abs() > 1e-4 {
                            out.scrub = Some((next, along));
                        }
                        out.readout_at = Some(p);
                        out.nav_stick[g.hand] = false;
                        self.layer = Some(g);
                    }
                },
            }
        } else {
            self.layer = None;
        }
        for i in 0..2 {
            self.stick_click[i] = layer_grip(ctrl[i], hand[i]).is_some();
        }

        // the menu's stick: layer steps (repeating while held)
        if menu.open && slice.is_some() && ctrl[0].active() {
            out.nav_stick[0] = false;
            let y = -dead_zoned(ctrl[0].stick.y);
            if y.abs() > 0.5 && now >= self.stick_step_at {
                out.command = Some(Command::StepLayer(if y > 0.0 { 1 } else { -1 }));
                self.stick_step_at = now + if y.abs() > 0.95 { 0.06 } else { 0.18 };
            } else if y.abs() <= 0.5 {
                self.stick_step_at = 0.0;
            }
        }

        // grips: part grabs, two hands, else navigation
        let grips = [grab(ctrl[0], hand[0], self.grip_down[0]), grab(ctrl[1], hand[1], self.grip_down[1])];
        for i in 0..2 {
            let down = grips[i].is_some();
            // Near draggables claim first; menu cells and distant selections come after.
            let pressed = down && !self.grip_down[i] && !buttons.claimed(i)
                && !self.layer.is_some_and(|g| g.hand == i);
            if self.holds[i].is_some() {
                buttons.claim(i); // keep ownership through release
            }
            let released = !down && self.grip_down[i];
            self.grip_down[i] = down;
            if released {
                self.nav_held[i] = false;
                if self.holds[i].take().is_some() || self.two.is_some() {
                    self.two = None;
                    // the other hand, if still holding, takes over from here
                    let other = 1 - i;
                    if let (Some(h), Some(p)) = (self.holds[other].as_mut(), grips[other]) {
                        h.from = pose_affine(view, p.orientation, p.position);
                        h.deltas = h.deltas.iter().map(|(j, _)| (*j, self.deltas[*j])).collect();
                    } else {
                        self.holds[other] = None;
                        out.commit = Some(self.finish(parts));
                        out.moved = true;
                    }
                }
            }
            if pressed {
                let p = grips[i].unwrap();
                let other = 1 - i;
                let near = tip(ctrl[i], hand[i]).and_then(|t| self.near(view, parts, t.position, mm_per_m));
                let remote_grip = !ctrl[i].active() || ctrl[i].grip >= PRESS;
                if self.holds[other].is_some() && grips[other].is_some() && (near.is_some() || remote_grip) {
                    // the second hand on a held part: two-hand move/turn/scale
                    let po = grips[other].unwrap().position;
                    let (l, r) = if i == 0 { (p.position, po) } else { (po, p.position) };
                    let ids = self.holds[other].as_ref().unwrap().deltas.iter().map(|(j, _)| (*j, self.deltas[*j])).collect();
                    let both: Vec<(usize, Affine)> = ids;
                    self.two = Some(TwoHands { mid: (l + r) * 0.5, span: r - l, deltas: both.clone() });
                    self.holds[i] = Some(Hold { from: pose_affine(view, p.orientation, p.position), deltas: both });
                    out.haptics.add(i, haptic::GRAB);
                } else if !self.nav_held[other] || self.holds[other].is_none() {
                    if let Some(part) = movable.then(|| near.or_else(|| {
                        if !remote_grip || (i == 1 && menu.hover.is_some()) { return None; }
                        aim(ctrl[i], hand[i]).and_then(|a| self.pick(view, parts, a, mm_per_m)).filter(|&j| !self.edits.waiting(j))
                    })).flatten() {
                        if !self.selected[part] {
                            for s in self.selected.iter_mut() {
                                *s = false;
                            }
                            self.selected[part] = true;
                            out.selection_changed = true;
                        }
                        let ids: Vec<(usize, Affine)> = (0..parts.len()).filter(|&j| self.selected[j] && !self.edits.waiting(j)).map(|j| (j, self.deltas[j])).collect();
                        self.grab_ids.clear();
                        self.grab_ids.extend(ids.iter().map(|(j, _)| *j));
                        self.holds[i] = Some(Hold { from: pose_affine(view, p.orientation, p.position), deltas: ids });
                        out.haptics.add(i, haptic::GRAB);
                    }
                }
                if self.holds[i].is_some() {
                    buttons.claim(i);
                } else if remote_grip {
                    self.nav_held[i] = true;
                }
            }
        }
        // triggers: a menu click, or select by aim
        for i in 0..2 {
            let down = trigger(ctrl[i], hand[i], self.trigger_down[i]);
            let pressed = down && !self.trigger_down[i];
            self.trigger_down[i] = down;
            if !pressed || buttons.claimed(i) || self.layer.is_some_and(|g| g.hand == i) {
                continue;
            }
            // a press on the menu is the menu's
            if i == 1 && menu.hover.is_some() {
                continue;
            }
            if self.grabbing() || !movable {
                continue;
            }
            let hit = aim(ctrl[i], hand[i]).and_then(|a| self.pick(view, parts, a, mm_per_m));
            for (j, s) in self.selected.iter_mut().enumerate() {
                let want = hit == Some(j);
                if *s != want {
                    *s = want;
                    out.selection_changed = true;
                }
            }
        }

        // the grabs move the parts
        if let (Some(two), [Some(a), Some(b)]) = (&self.two, grips) {
            let (l, r) = (a.position, b.position);
            let (mid, span) = ((l + r) * 0.5, r - l);
            let (len0, len) = (two.span.length(), span.length());
            if len0 > 0.02 && len > 0.02 {
                let k = (len / len0) as f64;
                let turn = -(span.z.atan2(span.x) - two.span.z.atan2(two.span.x)) as f64;
                // the room similarity: r' = mid + R_y(turn) k (r - mid0), in printer mm
                let (s, c) = (turn.sin(), turn.cos());
                let room = |q: Vec3f| {
                    let d = (q - two.mid) * k as f32;
                    mid + vec3f(d.x * c as f32 + d.z * s as f32, d.y, -d.x * s as f32 + d.z * c as f32)
                };
                let o = printer_from_room(view, room(room_from_printer(view, [0.0; 3])));
                let ex = printer_from_room(view, room(room_from_printer(view, [1.0, 0.0, 0.0])));
                let ey = printer_from_room(view, room(room_from_printer(view, [0.0, 1.0, 0.0])));
                let ez = printer_from_room(view, room(room_from_printer(view, [0.0, 0.0, 1.0])));
                let d: Affine = std::array::from_fn(|i| [ex[i] - o[i], ey[i] - o[i], ez[i] - o[i], o[i]]);
                for (j, d0) in &two.deltas {
                    let mut m = mul(&d, d0);
                    let ks = scale_of(&m);
                    if !(0.05..=20.0).contains(&ks) {
                        continue;
                    }
                    above_plate(&mut m, &parts[*j]);
                    self.deltas[*j] = m;
                    out.moved = true;
                }
            }
        } else {
            for i in 0..2 {
                let (Some(h), Some(p)) = (&self.holds[i], grips[i]) else { continue };
                let d = mul(&pose_affine(view, p.orientation, p.position), &inverse(&h.from));
                for (j, d0) in &h.deltas {
                    let mut m = mul(&d, d0);
                    untilt(&mut m, parts[*j].center());
                    above_plate(&mut m, &parts[*j]);
                    if self.deltas[*j] != m {
                        self.deltas[*j] = m;
                        out.moved = true;
                    }
                }
            }
        }
        for i in 0..2 {
            out.nav_grip[i] = self.holds[i].is_none() && self.two.is_none();
            out.touch[i] = match &self.holds[i] {
                Some(h) => h.deltas.first().map(|d| d.0),
                None => tip(ctrl[i], hand[i]).filter(|_| movable).and_then(|t| self.near(view, parts, t.position, mm_per_m)),
            };
        }
    }

    /// The grab ended: every moved part drops onto the plate; their deltas go to the commit.
    fn finish(&mut self, parts: &[Bounds]) -> Vec<(usize, Affine)> {
        let mut moves = Vec::new();
        let ids = std::mem::take(&mut self.grab_ids);
        for &i in &ids {
            if self.deltas[i] != IDENTITY {
                onto_plate(&mut self.deltas[i], &parts[i]);
                moves.push((i, self.deltas[i]));
            }
        }
        self.grab_ids = ids;
        self.grab_ids.clear();
        moves
    }

    /// The part a tip (room metres) is in or near, the nearest centre first.
    fn near(&self, view: &XrNavView, parts: &[Bounds], tip: Vec3f, mm_per_m: f64) -> Option<usize> {
        let at = printer_from_room(view, tip);
        let margin = REACH_M as f64 * mm_per_m;
        (0..parts.len()).filter(|&j| !self.edits.waiting(j) && inside(at, &parts[j], &self.deltas[j], margin)).min_by(|&a, &b| {
            let da = dist2(apply(&self.deltas[a], parts[a].center()), at);
            let db = dist2(apply(&self.deltas[b], parts[b].center()), at);
            da.total_cmp(&db)
        })
    }

    /// The nearest part an aim ray enters.
    fn pick(&self, view: &XrNavView, parts: &[Bounds], a: Pose, mm_per_m: f64) -> Option<usize> {
        let o = printer_from_room(view, a.position);
        let d = printer_dir(view, a.orientation.rotate_vec3(&vec3f(0.0, 0.0, -1.0)));
        let max = RAY_M as f64 * mm_per_m;
        (0..parts.len())
            .filter_map(|j| ray_box(o, d, &parts[j], &self.deltas[j]).filter(|t| *t <= max).map(|t| (j, t)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(j, _)| j)
    }
}

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]) * (a[i] - b[i])).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_xr_nav::{ClipBox, XrNavView};

    fn view() -> XrNavView {
        // the bed's origin 0.6 m in front, at table height, real size
        XrNavView { clip: ClipBox { center: vec3f(0.0, 0.9, -0.6), yaw: 0.0, half: vec3f(0.5, 0.3, 0.5) }, origin: vec3f(-0.175, 0.9, -0.44), yaw: 0.0, scale: 1.0 }
    }

    fn cube(min: [f64; 3], size: f64) -> Bounds {
        Bounds { min, max: [min[0] + size, min[1] + size, min[2] + size] }
    }

    /// A controller with its tip at `at`.
    fn held(at: Vec3f, grip: bool) -> XrController {
        let mut c = XrController { buttons: XrController::ACTIVE, ..Default::default() };
        c.grip_pose.orientation = Quat { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };
        c.aim_pose = c.grip_pose;
        makepad_xr_ui::input::place_tip(&mut c, at);
        c.grip = if grip { 1.0 } else { 0.0 };
        c
    }

    fn state(l: XrController, r: XrController) -> XrState {
        let mut s = XrState { left_controller: l, right_controller: r, ..Default::default() };
        s.head_pose.orientation = Quat { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };
        s
    }

    fn room_of(p: [f64; 3]) -> Vec3f {
        room_from_printer(&view(), p)
    }

    #[test]
    fn frames_round_trip() {
        let v = view();
        let p = [120.0, 80.0, 15.0];
        let q = printer_from_room(&v, room_from_printer(&v, p));
        assert!(dist2(p, q) < 1e-6, "{q:?}");
        let a = pose_affine(&v, Quat { x: 0.0, y: 0.0, z: 0.0, w: 1.0 }, room_of(p));
        assert!(dist2(apply(&a, [0.0; 3]), p) < 1e-6);
        assert!(dist2(apply(&a, [1.0, 0.0, 0.0]), [121.0, 80.0, 15.0]) < 1e-6, "{:?}", apply(&a, [1.0, 0.0, 0.0]));
    }

    #[test]
    fn either_grab_control_on_a_part_moves_it_as_one_commit_and_it_drops_onto_the_plate() {
        for use_trigger in [false, true] {
            let held = |at, down| {
                let mut c = held(at, down);
                if use_trigger {
                    c.trigger = c.grip;
                    c.grip = 0.0;
                }
                c
            };
            let parts = [cube([100.0, 100.0, 0.0], 20.0), cube([200.0, 100.0, 0.0], 20.0)];
            let mut e = XrEdit::default();
            let mut out = EditOut::default();
            let v = view();
            let on = room_of([110.0, 110.0, 10.0]);
            let away = vec3f(-0.5, 1.5, 0.0);
            e.step(Some(PlateMode::Design), &state(held(away, false), held(on, false)), &v, &parts, None, 0.0, &mut Buttons::default(), &MenuStep::default(), &mut out);
            e.step(Some(PlateMode::Design), &state(held(away, false), held(on, true)), &v, &parts, None, 0.01, &mut Buttons::default(), &MenuStep::default(), &mut out);
            assert!(e.grabbing() && !out.nav_grip[1] && out.nav_grip[0], "the right grip took the part, not the plate");
            assert!(e.selected[0] && !e.selected[1]);
            // 50 mm to the right and 30 mm up (in the air)
            let to = room_of([160.0, 110.0, 40.0]);
            e.step(Some(PlateMode::Design), &state(held(away, false), held(to, true)), &v, &parts, None, 0.02, &mut Buttons::default(), &MenuStep::default(), &mut out);
            assert!(out.moved && out.commit.is_none());
            assert!((e.deltas[0][0][3] - 50.0).abs() < 1e-3 && (e.deltas[0][2][3] - 30.0).abs() < 1e-3, "{:?}", e.deltas[0]);
            e.step(Some(PlateMode::Design), &state(held(away, false), held(to, false)), &v, &parts, None, 0.03, &mut Buttons::default(), &MenuStep::default(), &mut out);
            let commit = out.commit.take().expect("the release commits");
            assert_eq!(commit.len(), 1, "only the moved part");
            assert_eq!(commit[0].0, 0);
            assert!(commit[0].1[2][3].abs() < 1e-9, "dropped onto the plate: {:?}", commit[0].1);
            assert!((commit[0].1[0][3] - 50.0).abs() < 1e-3);
        }
    }

    #[test]
    fn a_grip_in_empty_space_is_the_navigation() {
        let parts = [cube([100.0, 100.0, 0.0], 20.0)];
        let mut e = XrEdit::default();
        let mut out = EditOut::default();
        let v = view();
        let far = vec3f(0.6, 1.6, 0.2);
        e.step(Some(PlateMode::Design), &state(held(far, false), held(far, false)), &v, &parts, None, 0.0, &mut Buttons::default(), &MenuStep::default(), &mut out);
        // aimed away from the plate (the aim looks down -Z from behind the head): nothing hit
        let mut c = held(far, true);
        c.aim_pose.orientation = Quat { x: 0.0, y: 1.0, z: 0.0, w: 0.0 };
        e.step(Some(PlateMode::Design), &state(held(far, false), c), &v, &parts, None, 0.01, &mut Buttons::default(), &MenuStep::default(), &mut out);
        assert!(!e.grabbing() && out.nav_grip[1]);
    }

    #[test]
    fn a_small_tilt_is_cancelled_a_large_one_kept() {
        let parts = [cube([100.0, 100.0, 0.0], 20.0)];
        let v = view();
        let on = room_of([110.0, 110.0, 10.0]);
        for (deg, kept) in [(6.0f32, false), (40.0, true)] {
            let mut e = XrEdit::default();
            let mut out = EditOut::default();
            e.step(Some(PlateMode::Design), &state(held(vec3f(-1.0, 1.0, 0.0), false), held(on, false)), &v, &parts, None, 0.0, &mut Buttons::default(), &MenuStep::default(), &mut out);
            e.step(Some(PlateMode::Design), &state(held(vec3f(-1.0, 1.0, 0.0), false), held(on, true)), &v, &parts, None, 0.01, &mut Buttons::default(), &MenuStep::default(), &mut out);
            let mut c = held(on, true);
            c.aim_pose.orientation = Quat::from_axis_angle(vec3f(1.0, 0.0, 0.0), deg.to_radians());
            e.step(Some(PlateMode::Design), &state(held(vec3f(-1.0, 1.0, 0.0), false), c), &v, &parts, None, 0.02, &mut Buttons::default(), &MenuStep::default(), &mut out);
            let tilted = e.deltas[0][2][2] < 0.999;
            assert_eq!(tilted, kept, "{deg} degrees: {:?}", e.deltas[0]);
        }
    }

    #[test]
    fn two_hands_scale_the_held_part() {
        let parts = [cube([100.0, 100.0, 0.0], 20.0)];
        let mut e = XrEdit::default();
        let mut out = EditOut::default();
        let v = view();
        let on = room_of([110.0, 110.0, 10.0]);
        let l0 = on + vec3f(-0.1, 0.0, 0.0);
        e.step(Some(PlateMode::Design), &state(held(l0, false), held(on, false)), &v, &parts, None, 0.0, &mut Buttons::default(), &MenuStep::default(), &mut out);
        e.step(Some(PlateMode::Design), &state(held(l0, false), held(on, true)), &v, &parts, None, 0.01, &mut Buttons::default(), &MenuStep::default(), &mut out);
        e.step(Some(PlateMode::Design), &state(held(l0, true), held(on, true)), &v, &parts, None, 0.02, &mut Buttons::default(), &MenuStep::default(), &mut out);
        // the hands twice as far apart
        e.step(Some(PlateMode::Design), &state(held(on + vec3f(-0.15, 0.0, 0.0), true), held(on + vec3f(0.05, 0.0, 0.0), true)), &v, &parts, None, 0.03, &mut Buttons::default(), &MenuStep::default(), &mut out);
        assert!((scale_of(&e.deltas[0]) - 2.0).abs() < 1e-3, "{:?}", e.deltas[0]);
        // the left lets go: still held by the right, no commit yet
        e.step(Some(PlateMode::Design), &state(held(on, false), held(on + vec3f(0.05, 0.0, 0.0), true)), &v, &parts, None, 0.04, &mut Buttons::default(), &MenuStep::default(), &mut out);
        assert!(out.commit.is_none() && e.grabbing());
        e.step(Some(PlateMode::Design), &state(held(on, false), held(on + vec3f(0.05, 0.0, 0.0), false)), &v, &parts, None, 0.05, &mut Buttons::default(), &MenuStep::default(), &mut out);
        let commit = out.commit.take().unwrap();
        assert!((scale_of(&commit[0].1) - 2.0).abs() < 1e-3);
    }

    #[test]
    fn no_button_or_gesture_asks_for_another_workspace() {
        let parts = [cube([100.0, 100.0, 0.0], 20.0)];
        let mut e = XrEdit::default();
        let mut out = EditOut::default();
        let v = view();
        let axis = Some(SliceAxis { layers: 100, layer: 10, along: 1.0, issue_layers: &[] });
        let at = vec3f(0.2, 1.0, -0.4);
        for mode in [Some(PlateMode::Design), Some(PlateMode::Mechanics), Some(PlateMode::Monitor), None] {
            // A on the right controller: the frame it goes down
            let mut a = held(at, false);
            a.buttons |= XrController::CLICK_A;
            let mut buttons = Buttons::default();
            buttons.step(&state(held(at, false), a.clone()));
            e.step(mode, &state(held(at, false), a), &v, &parts, axis, 0.0, &mut buttons, &MenuStep::default(), &mut out);
            assert!(!matches!(out.command, Some(Command::SetWorkspace(_))));
        }
        // the thumbstick click without the slice showing (no axis): no layer grab, the stick navigates
        let mut c = held(at, false);
        c.buttons |= XrController::CLICK_THUMBSTICK;
        e.step(Some(PlateMode::Design), &state(held(at, false), c), &v, &parts, None, 0.01, &mut Buttons::default(), &MenuStep::default(), &mut out);
        assert!(!e.layer_grab() && out.command.is_none() && out.nav_stick[1]);
    }

    #[test]
    fn parts_do_not_move_in_monitor_or_mechanics() {
        let parts = [cube([100.0, 100.0, 0.0], 20.0)];
        let v = view();
        let on = room_of([110.0, 110.0, 10.0]);
        let away = vec3f(-0.5, 1.5, 0.0);
        for mode in [PlateMode::Monitor, PlateMode::Mechanics] {
            let mut e = XrEdit::default();
            let mut out = EditOut::default();
            e.step(Some(mode), &state(held(away, false), held(on, false)), &v, &parts, None, 0.0, &mut Buttons::default(), &MenuStep::default(), &mut out);
            e.step(Some(mode), &state(held(away, false), held(on, true)), &v, &parts, None, 0.01, &mut Buttons::default(), &MenuStep::default(), &mut out);
            assert!(!e.grabbing() && out.nav_grip[1], "the grip stays the navigation's in {mode:?}");
        }
    }

    #[test]
    fn the_layer_grab_maps_hand_height_to_layers_with_ticks() {
        let parts = [cube([100.0, 100.0, 0.0], 20.0)];
        let mut e = XrEdit::default();
        let mut out = EditOut::default();
        let v = view();
        let issues = [12u32];
        let axis = |layer: usize| Some(SliceAxis { layers: 100, layer, along: 1.0, issue_layers: &issues });
        let at = vec3f(0.2, 1.0, -0.4);
        let mut c = held(at, false);
        e.step(Some(PlateMode::Design), &state(held(at, false), c.clone()), &v, &parts, axis(10), 0.0, &mut Buttons::default(), &MenuStep::default(), &mut out);
        c.buttons |= XrController::CLICK_THUMBSTICK;
        e.step(Some(PlateMode::Design), &state(held(at, false), c.clone()), &v, &parts, axis(10), 0.01, &mut Buttons::default(), &MenuStep::default(), &mut out);
        assert!(e.layer_grab() && !out.nav_stick[1]);
        // 2 mm up: two layers, the second has an issue
        c.grip_pose.position.y += 0.0011;
        e.step(Some(PlateMode::Design), &state(held(at, false), c.clone()), &v, &parts, axis(10), 0.02, &mut Buttons::default(), &MenuStep::default(), &mut out);
        assert_eq!(out.scrub.map(|s| s.0), Some(11));
        assert_eq!(out.haptics.0[1], Some(haptic::SNAP));
        c.grip_pose.position.y += 0.001;
        e.step(Some(PlateMode::Design), &state(held(at, false), c.clone()), &v, &parts, axis(11), 0.03, &mut Buttons::default(), &MenuStep::default(), &mut out);
        assert_eq!(out.scrub.map(|s| s.0), Some(12));
        assert_eq!(out.haptics.0[1], Some(haptic::LIMIT));
        // across: halfway back along the layer
        c.grip_pose.position.x -= ALONG_FULL_M * 0.5;
        e.step(Some(PlateMode::Design), &state(held(at, false), c.clone()), &v, &parts, axis(12), 0.04, &mut Buttons::default(), &MenuStep::default(), &mut out);
        assert!((out.scrub.unwrap().1 - 0.5).abs() < 1e-3);
        // released: the level stays, the stick is the navigation's again
        c.buttons &= !XrController::CLICK_THUMBSTICK;
        e.step(Some(PlateMode::Design), &state(held(at, false), c), &v, &parts, axis(12), 0.05, &mut Buttons::default(), &MenuStep::default(), &mut out);
        assert!(!e.layer_grab() && out.scrub.is_none() && out.nav_stick[1]);
    }

    #[test]
    fn a_trigger_click_selects_the_part_aimed_at() {
        let parts = [cube([100.0, 100.0, 0.0], 20.0), cube([200.0, 100.0, 0.0], 20.0)];
        let mut e = XrEdit::default();
        let mut out = EditOut::default();
        let v = view();
        // above the second part, aiming straight down
        let mut c = held(room_of([210.0, 110.0, 200.0]), false);
        c.aim_pose.orientation = Quat::from_axis_angle(vec3f(1.0, 0.0, 0.0), -std::f32::consts::FRAC_PI_2);
        e.step(Some(PlateMode::Design), &state(held(vec3f(-1.0, 1.0, 0.0), false), c.clone()), &v, &parts, None, 0.0, &mut Buttons::default(), &MenuStep::default(), &mut out);
        c.trigger = 1.0;
        e.step(Some(PlateMode::Design), &state(held(vec3f(-1.0, 1.0, 0.0), false), c), &v, &parts, None, 0.01, &mut Buttons::default(), &MenuStep::default(), &mut out);
        assert!(out.selection_changed && !e.selected[0] && e.selected[1]);
        assert!(!e.grabbing(), "a distant trigger selects without starting a drag");
    }
}

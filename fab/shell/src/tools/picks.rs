//! Picks: what the person points at in the Edit shape mode (E), for the AI. A click picks what is
//! under the pointer, Shift adds or takes off, a double click picks the whole body; holding the
//! press still opens Select Other, everything along the pointer's ray. The kind filter (Any,
//! Face, Edge, Feature, Body) says what a click takes; on a part drawn from its field a face is
//! the edit there and an edge a crease or an edit's own edges. The model worker resolves and
//! describes each pick (`fab.select`); the view keeps what it draws: a marker at each pick's spot
//! (a box for a pick named without one), an exact part's face or edge outlined, what a click
//! would take at the pointer (its words, and its outline on an exact part), and Select Other's
//! list. Every pick is also a chip on the chat's composer.
//!
//! On a part drawn from its field the edits picked, and the one a click would take, are tinted
//! where the part draws them (each tile holds its edit's ordinal; a mask row per part says how
//! strongly), and a crease under the pointer gets a tick along it. An exact part's outlines draw
//! in the viewport against its G-buffer: what is behind a part dimmed and dashed.
//!
//! Cost: at rest nothing; a pointer move in E sends one ray to the worker (the app keeps one in
//! flight, the newest waiting); while picks exist, a marker each in the tool overlay, their
//! outlines' points carried to the world each frame (geometry made again only when they move),
//! and per field part they tint one mask compare; a changed tint writes one mask row.

use crate::api::*;
use makepad_widgets_core::*;
use std::sync::Arc;

type V3 = [f64; 3];

/// One pick as the view draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct PickView {
    /// The worker's name for it (`p3`): the chat's chip says it back.
    pub id: String,
    /// edit, crease, edges, spot, feature or body.
    pub kind: String,
    pub label: String,
    /// The printed object it is on (`instance/object`).
    pub object: String,
    /// Where it was pointed at, in the object's model frame: point and outward normal.
    pub at: Option<(V3, V3)>,
    /// Its solid's box in that frame.
    pub bounds: Option<(V3, V3)>,
    /// An exact part's face outline or edge curves, in that frame.
    pub outline: Arc<Vec<Vec<V3>>>,
    /// On a part drawn from its field, the features it lights up and how strongly
    /// (`csg_sdf::SdfNode::Id` ids): an edit's, a crease's two at half.
    pub tint: Vec<(u32, f32)>,
}

/// What a click picks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PickKind {
    /// As it lands: on an exact part the edge near the pointer, else the face; on a part drawn
    /// from its field a crease near where two edits meet, an edit's edges near its own edge, else
    /// the edit; on any other the feature.
    #[default]
    Any,
    /// An exact part's face; the edit on a part drawn from its field.
    Face,
    /// An exact part's edge; a crease or an edit's own edges on a part drawn from its field.
    Edge,
    Feature,
    Body,
}

impl PickKind {
    pub const ALL: [PickKind; 5] = [PickKind::Any, PickKind::Face, PickKind::Edge, PickKind::Feature, PickKind::Body];

    /// As the worker takes it.
    pub fn name(self) -> &'static str {
        match self {
            PickKind::Any => "any",
            PickKind::Face => "face",
            PickKind::Edge => "edge",
            PickKind::Feature => "feature",
            PickKind::Body => "body",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PickKind::Any => "Any",
            PickKind::Face => "Face",
            PickKind::Edge => "Edge",
            PickKind::Feature => "Feature",
            PickKind::Body => "Body",
        }
    }
}

/// One row of Select Other.
#[derive(Clone, Debug, PartialEq)]
pub struct OtherRow {
    pub kind: String,
    pub label: String,
    /// The pick as `fab.select`'s `add` takes it (JSON).
    pub pick: String,
}

/// Select Other's list, at the pointer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OtherList {
    pub at: DVec2,
    pub rows: Vec<OtherRow>,
    pub hover: Option<usize>,
    /// The press that opened it was with Shift: a row adds.
    pub add: bool,
}

/// The picks and what the view shows of them.
#[derive(Clone, Debug, Default)]
pub struct PickState {
    pub list: Arc<Vec<PickView>>,
    /// The worker's revision of `list`.
    pub rev: u64,
    pub filter: PickKind,
    /// The pick lit up: its chat chip under the pointer.
    pub lit: Option<String>,
    /// What a click under the pointer would pick, in words, at the pointer.
    pub under: Option<Under>,
    /// Select Other, while it is open.
    pub other: Option<OtherList>,
}

/// Where the pointer rests: its ray (plate mm), plate mm per pixel at the eye and per mm of
/// depth, the window point, and the view it rests on (camera and rect).
#[derive(Clone, Copy, Debug)]
pub struct Hover {
    pub from: V3,
    pub dir: V3,
    pub pixel: [f64; 2],
    pub at: DVec2,
    pub view: (Camera, Rect),
}

/// What a click at the pointer would pick, in words. It speaks for the view it was asked in:
/// once the camera or the view's rect moves (a panel opens, the camera glides), it is not shown
/// until the pointer asks again.
#[derive(Clone, Debug)]
pub struct Under {
    pub at: DVec2,
    pub words: String,
    pub view: (Camera, Rect),
    /// On an exact part: the printed object, and the outline of what a click would take there
    /// (model frame).
    pub outline: Option<(String, Vec<Vec<V3>>)>,
    /// The printed object it is on.
    pub object: String,
    /// On a part drawn from its field: as [`PickView::tint`]; on a crease, its point and way
    /// along it (model frame).
    pub tint: Vec<(u32, f32)>,
    pub tick: Option<(V3, V3)>,
}

/// What the view asks of the app (which asks the model worker).
#[derive(Clone, Debug)]
pub enum PickAction {
    /// A click: the pointer's ray (plate mm), plate mm per pixel at the eye and per mm of depth,
    /// Shift (adds, or takes off what is picked), a double click (the body).
    At { from: V3, dir: V3, pixel: [f64; 2], add: bool, body: bool },
    /// The pointer rests; None: it left.
    Hover(Option<Hover>),
    /// Press and hold: Select Other at `at`.
    Other { from: V3, dir: V3, pixel: [f64; 2], at: DVec2, add: bool },
    /// Select Other's row `n` was chosen; None: it closed.
    Row(Option<usize>),
    /// A kind for the filter.
    Filter(PickKind),
}

/// Plate mm per pixel of a view: at the eye, and per mm of depth.
pub fn pixel(cam: &Camera, rect: Rect) -> [f64; 2] {
    let h = rect.size.y.max(1.0);
    if cam.ortho {
        [cam.ortho_height as f64 * 1000.0 / h, 0.0]
    } else {
        [0.0, 2.0 * ((cam.fov_y_deg as f64).to_radians() * 0.5).tan() / h]
    }
}

/// Picks are made in the Edit shape mode of the Select tool, in Design.
pub fn active(state: &AppState) -> bool {
    state.ui.gizmo == GizmoMode::Shape && state.tool == Tool::Select && state.ui.in_mode(PlateMode::Design)
}

/// A model point of part `object` on the plate (mm), when the part is there.
pub fn on_plate(state: &AppState, object: &str, p: V3) -> Option<V3> {
    let part = state.plate.parts.iter().find(|part| part.object == object)?;
    let m = invert(&part.model_from_printer)?;
    Some(xf(&m, p))
}

fn xf(m: &[[f64; 4]; 3], p: V3) -> V3 {
    std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
}

/// The inverse of an affine map (row-major 3x4).
fn invert(m: &[[f64; 4]; 3]) -> Option<[[f64; 4]; 3]> {
    let a = |r: usize, c: usize| m[r][c];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0)) + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    if det.abs() < 1e-15 {
        return None;
    }
    let inv = [
        [(a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) / det, (a(0, 2) * a(2, 1) - a(0, 1) * a(2, 2)) / det, (a(0, 1) * a(1, 2) - a(0, 2) * a(1, 1)) / det],
        [(a(1, 2) * a(2, 0) - a(1, 0) * a(2, 2)) / det, (a(0, 0) * a(2, 2) - a(0, 2) * a(2, 0)) / det, (a(0, 2) * a(1, 0) - a(0, 0) * a(1, 2)) / det],
        [(a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0)) / det, (a(0, 1) * a(2, 0) - a(0, 0) * a(2, 1)) / det, (a(0, 0) * a(1, 1) - a(0, 1) * a(1, 0)) / det],
    ];
    let t = [m[0][3], m[1][3], m[2][3]];
    Some(std::array::from_fn(|r| [inv[r][0], inv[r][1], inv[r][2], -(inv[r][0] * t[0] + inv[r][1] * t[1] + inv[r][2] * t[2])]))
}

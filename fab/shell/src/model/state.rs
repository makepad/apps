//! Mutable per-session scene state: selection, visibility, section, the
//! dragged part's preview offset.
//!
//! Everything here is cheap to mutate and bumps `revision`. The renderer keeps
//! the last revision it uploaded its per-element lookup for and refreshes only
//! when it changes. No geometry is ever modified by state changes.

use crate::model::ids::ElementId;
use makepad_math::{Aabb, Plane, Vec3f};
use std::collections::HashSet;

/// A rigid pose, row-major 3x4 in Fab's frame (metres, Z up): `p' = R p + t`.
pub type Pose = [[f32; 4]; 3];

pub const POSE_IDENTITY: Pose = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];

/// `p` moved by `pose`.
pub fn pose_apply(pose: &Pose, p: Vec3f) -> Vec3f {
    let r = |i: usize| pose[i][0] * p.x + pose[i][1] * p.y + pose[i][2] * p.z + pose[i][3];
    Vec3f { x: r(0), y: r(1), z: r(2) }
}

/// `a` after `b`.
pub fn pose_mul(a: &Pose, b: &Pose) -> Pose {
    std::array::from_fn(|i| std::array::from_fn(|j| a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j] + if j == 3 { a[i][3] } else { 0.0 }))
}

/// A shift by `by` (metres) after a turn of `turn_deg` about the vertical through `about`.
pub fn pose_turn_shift(about: Vec3f, turn_deg: f32, by: Vec3f) -> Pose {
    let (s, c) = turn_deg.to_radians().sin_cos();
    [
        [c, -s, 0.0, about.x - (c * about.x - s * about.y) + by.x],
        [s, c, 0.0, about.y - (s * about.x + c * about.y) + by.y],
        [0.0, 0.0, 1.0, by.z],
    ]
}

/// A uniform scale by `by` about `about`.
pub fn pose_scale_about(about: Vec3f, by: f32) -> Pose {
    let k = 1.0 - by;
    [[by, 0.0, 0.0, about.x * k], [0.0, by, 0.0, about.y * k], [0.0, 0.0, by, about.z * k]]
}

/// A part's own X, Y, Z (the columns) in Fab's frame under its `place` turn `turn` (degrees
/// about the printer's X, then Y, then Z: `R = Rz Ry Rx`).
pub fn part_axes(turn: [f64; 3]) -> [[f32; 3]; 3] {
    turn_matrix(turn).map(|row| row.map(|v| v as f32))
}

/// The rotation of a turn: degrees about X, then Y, then Z. The columns are its own axes.
pub fn turn_matrix(turn: [f64; 3]) -> [[f64; 3]; 3] {
    let (sx, cx) = turn[0].to_radians().sin_cos();
    let (sy, cy) = turn[1].to_radians().sin_cos();
    let (sz, cz) = turn[2].to_radians().sin_cos();
    [[cz * cy, cz * sy * sx - sz * cx, cz * sy * cx + sz * sx], [sz * cy, sz * sy * sx + cz * cx, sz * sy * cx - cz * sx], [-sy, cy * sx, cy * cx]]
}

/// A scale by `by` along the axes `axes` (columns: a part's own X, Y, Z in Fab's frame, unit and
/// at right angles: its turn) about `about`: a part scaled along its own axes, whatever its turn.
pub fn pose_stretch_about(about: Vec3f, axes: &[[f32; 3]; 3], by: [f32; 3]) -> Pose {
    // A diag(by) Aᵀ, with the shift that keeps `about` where it is
    let lin: [[f32; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| axes[i][k] * by[k] * axes[j][k]).sum()));
    let a = [about.x, about.y, about.z];
    std::array::from_fn(|i| [lin[i][0], lin[i][1], lin[i][2], a[i] - (lin[i][0] * a[0] + lin[i][1] * a[1] + lin[i][2] * a[2])])
}

/// What `pose` does to a part turned by `axes` (as in [`pose_stretch_about`]): its factors
/// along the part's own axes and its turn about Z (degrees), from the pose's linear part `L`
/// (`L A` is the part's axes turned and scaled: its columns' lengths are the factors).
pub fn pose_scale_turn(pose: &Pose, axes: &[[f32; 3]; 3]) -> ([f32; 3], f32) {
    let la: [[f32; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| pose[i][k] * axes[k][j]).sum()));
    let by: [f32; 3] = std::array::from_fn(|j| (0..3).map(|i| la[i][j] * la[i][j]).sum::<f32>().sqrt());
    // the turn: L A diag(1/by) Aᵀ, its X column's heading
    let t: [f32; 2] = std::array::from_fn(|i| (0..3).map(|j| la[i][j] / by[j].max(1e-12) * axes[0][j]).sum());
    (by, t[1].atan2(t[0]).to_degrees())
}

/// Whether `pose` turns, moves and scales evenly (a distance stays one under it), not a scale
/// along some axes more than others.
pub fn pose_similar(pose: &Pose) -> bool {
    let col = |c: usize| [pose[0][c], pose[1][c], pose[2][c]];
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let k2 = dot(col(0), col(0));
    (0..3).all(|i| (0..3).all(|j| (dot(col(i), col(j)) - if i == j { k2 } else { 0.0 }).abs() <= 1e-5 * k2))
}

#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub set: HashSet<ElementId>,
    /// The last-clicked element: the one the part panel shows and the one
    /// drawn with the brighter outline.
    pub active: Option<ElementId>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
    pub fn contains(&self, id: ElementId) -> bool {
        self.set.contains(&id)
    }
    pub fn len(&self) -> usize {
        self.set.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionPlane {
    /// Half-space to *keep* is `a*x + b*y + c*z + d >= 0`.
    pub plane: Plane,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SectionState {
    pub enabled: bool,
    /// Up to 6 planes are honoured by the renderer; more are ignored.
    pub planes: Vec<SectionPlane>,
    /// Section box: keeps only what is inside. Combined with planes.
    pub boxed: Option<Aabb>,
    /// Draw the cut faces with a cap fill (vs. seeing inside hollow shells).
    pub caps: bool,
}

impl SectionPlane {
    /// Signed distance; positive is the half-space that survives the cut.
    #[inline]
    pub fn distance(&self, p: Vec3f) -> f32 {
        self.plane.a * p.x + self.plane.b * p.y + self.plane.c * p.z + self.plane.d
    }

    #[inline]
    pub fn keeps(&self, p: Vec3f) -> bool {
        !self.enabled || self.distance(p) >= 0.0
    }
}

impl SectionState {
    /// True when a world point survives every active cut — the exact test the
    /// mesh shader does, so a pick and the picture agree about what is there.
    pub fn keeps(&self, p: Vec3f) -> bool {
        if !self.enabled {
            return true;
        }
        for pl in &self.planes {
            if !pl.keeps(p) {
                return false;
            }
        }
        if let Some(b) = &self.boxed {
            if p.x < b.min.x
                || p.y < b.min.y
                || p.z < b.min.z
                || p.x > b.max.x
                || p.y > b.max.y
                || p.z > b.max.z
            {
                return false;
            }
        }
        true
    }

    /// True when nothing is cut away at all (fast path for pickers).
    pub fn is_inactive(&self) -> bool {
        !self.enabled || (self.planes.iter().all(|p| !p.enabled) && self.boxed.is_none())
    }
}

/// Poses a pane shows instead of the stage's ([`SceneState::pose`]): the assembled device, its kit,
/// or a frame of the animation between (element -> pose, rest -> now, Fab's frame, metres, Z up).
/// It has its own revision and is mutated in place, so an animated frame allocates nothing once
/// its parts are in it.
#[derive(Clone, Debug, Default)]
pub struct PoseSet {
    poses: std::collections::HashMap<ElementId, Pose>,
    revision: u64,
}

impl PoseSet {
    /// Moves on every change through the methods below.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn get(&self, element: ElementId) -> Option<Pose> {
        self.poses.get(&element).copied()
    }

    pub fn contains(&self, element: ElementId) -> bool {
        self.poses.contains_key(&element)
    }

    pub fn is_empty(&self) -> bool {
        self.poses.is_empty()
    }

    pub fn len(&self) -> usize {
        self.poses.len()
    }

    /// Every element away from rest, in no order.
    pub fn iter(&self) -> impl Iterator<Item = (ElementId, Pose)> + '_ {
        self.poses.iter().map(|(e, p)| (*e, *p))
    }

    /// Show `element` at `pose`; the revision moves only when it moved.
    pub fn set(&mut self, element: ElementId, pose: Pose) {
        if self.poses.insert(element, pose) != Some(pose) {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Show `element` at rest.
    pub fn remove(&mut self, element: ElementId) {
        if self.poses.remove(&element).is_some() {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Keep only the elements `keep` passes, the others at rest.
    pub fn retain(&mut self, mut keep: impl FnMut(ElementId) -> bool) {
        let len = self.poses.len();
        self.poses.retain(|e, _| keep(*e));
        if self.poses.len() != len {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Every element at rest; the storage stays for the next poses.
    pub fn clear(&mut self) {
        if !self.poses.is_empty() {
            self.poses.clear();
            self.revision = self.revision.wrapping_add(1);
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SceneState {
    pub selection: Selection,
    /// Explicitly hidden elements (H).
    pub hidden: HashSet<ElementId>,
    /// When `Some`, only these elements are visible (Shift+H isolate).
    pub isolated: Option<HashSet<ElementId>>,
    pub section: SectionState,
    /// A part being placed (the gizmo's drag, the panel's numbers, a headset grab): its element
    /// and its pose, rigid or uniformly scaled (row-major 3x4 in Fab's frame, metres, Z up; rest
    /// -> now). The part, its field and the gizmo all show there from the drag's first frame
    /// until the committed plate has it, so a move, a turn or a scale is a transform change: no
    /// geometry rebuilt, no re-bake.
    pub nudge: Option<(ElementId, Pose)>,
    /// Mechanics: every moving part's rigid pose (row-major 3x4 in Fab's frame, metres, Z up; rest -> now).
    pub moves: Option<std::sync::Arc<std::collections::HashMap<ElementId, [[f32; 4]; 3]>>>,
    /// A resin printer's slice shows the print where it is: these parts this far up Fab's Z
    /// (metres), on their supports, after any pose above.
    pub lift: Option<(std::sync::Arc<HashSet<ElementId>>, f32)>,
    /// Incremented on every mutation through the methods below.
    pub revision: u64,
}

impl SceneState {
    fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Show `element` at `pose` (or clear the placement preview).
    pub fn set_nudge(&mut self, nudge: Option<(ElementId, Pose)>) {
        if self.nudge != nudge {
            self.nudge = nudge;
            self.bump();
        }
    }

    /// Show parts at these poses (Mechanics), or at rest.
    pub fn set_moves(&mut self, moves: Option<std::sync::Arc<std::collections::HashMap<ElementId, [[f32; 4]; 3]>>>) {
        if self.moves.is_some() || moves.is_some() {
            self.moves = moves;
            self.bump();
        }
    }

    /// Show these parts this far up (metres), or where they are.
    pub fn set_lift(&mut self, lift: Option<(std::sync::Arc<HashSet<ElementId>>, f32)>) {
        if self.lift != lift {
            self.lift = lift;
            self.bump();
        }
    }

    /// Where `element` shows, when not at rest: Mechanics' pose, else the placement preview's,
    /// then the lift.
    pub fn pose(&self, element: ElementId) -> Option<Pose> {
        let moved = self.moves.as_ref().and_then(|m| m.get(&element).copied()).or_else(|| self.nudge.filter(|(e, _)| *e == element).map(|(_, p)| p));
        match self.lift.as_ref().filter(|(parts, _)| parts.contains(&element)) {
            Some((_, up)) => {
                let mut p = moved.unwrap_or(POSE_IDENTITY);
                p[2][3] += up;
                Some(p)
            }
            None => moved,
        }
    }

    /// Every element shown away from rest, at its [`pose`](Self::pose), each once.
    pub fn posed(&self) -> Vec<(ElementId, Pose)> {
        let mut ids: Vec<ElementId> = self.moves.iter().flat_map(|m| m.keys().copied()).chain(self.nudge.map(|(e, _)| e)).chain(self.lift.iter().flat_map(|(parts, _)| parts.iter().copied())).collect();
        ids.sort_by_key(|e| e.index());
        ids.dedup();
        ids.into_iter().filter_map(|e| Some((e, self.pose(e)?))).collect()
    }

    /// The placement preview's pose of `element` (identity when it has none).
    pub fn nudge_of(&self, element: Option<ElementId>) -> Pose {
        match (self.nudge, element) {
            (Some((e, p)), Some(el)) if e == el => p,
            _ => POSE_IDENTITY,
        }
    }

    /// True when the element is not hidden by any rule.
    pub fn is_visible(&self, id: ElementId) -> bool {
        if self.hidden.contains(&id) {
            return false;
        }
        match &self.isolated {
            Some(iso) => iso.contains(&id),
            None => true,
        }
    }

    pub fn select_only(&mut self, id: ElementId) {
        self.selection.set.clear();
        self.selection.set.insert(id);
        self.selection.active = Some(id);
        self.bump();
    }

    pub fn select_add(&mut self, id: ElementId) {
        self.selection.set.insert(id);
        self.selection.active = Some(id);
        self.bump();
    }

    pub fn select_toggle(&mut self, id: ElementId) {
        if self.selection.set.remove(&id) {
            if self.selection.active == Some(id) {
                self.selection.active = self.selection.set.iter().next().copied();
            }
        } else {
            self.selection.set.insert(id);
            self.selection.active = Some(id);
        }
        self.bump();
    }

    pub fn select_set(&mut self, ids: impl IntoIterator<Item = ElementId>) {
        self.selection.set = ids.into_iter().collect();
        self.selection.active = self.selection.set.iter().next().copied();
        self.bump();
    }

    pub fn clear_selection(&mut self) {
        if !self.selection.set.is_empty() || self.selection.active.is_some() {
            self.selection.set.clear();
            self.selection.active = None;
            self.bump();
        }
    }

    /// H — hide the selection.
    pub fn hide_selected(&mut self) {
        for id in self.selection.set.drain() {
            self.hidden.insert(id);
        }
        self.selection.active = None;
        self.bump();
    }

    /// Alt+H — unhide everything, drop isolation.
    pub fn unhide_all(&mut self) {
        self.hidden.clear();
        self.isolated = None;
        self.bump();
    }

    /// Shift+H — isolate the selection (hide everything else).
    pub fn isolate_selected(&mut self) {
        if self.selection.set.is_empty() {
            return;
        }
        self.isolated = Some(self.selection.set.clone());
        self.bump();
    }

    pub fn set_hidden(&mut self, id: ElementId, hidden: bool) {
        if hidden {
            self.hidden.insert(id);
        } else {
            self.hidden.remove(&id);
        }
        self.bump();
    }

    pub fn set_section(&mut self, section: SectionState) {
        self.section = section;
        self.bump();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_keep_the_right_half_space() {
        use makepad_math::Plane;
        let mut s = SectionState {
            enabled: true,
            planes: vec![SectionPlane {
                // keep z >= 0
                plane: Plane {
                    a: 0.0,
                    b: 0.0,
                    c: 1.0,
                    d: 0.0,
                },
                enabled: true,
            }],
            boxed: None,
            caps: true,
        };
        let up = Vec3f { x: 0.0, y: 0.0, z: 1.0 };
        let down = Vec3f { x: 0.0, y: 0.0, z: -1.0 };
        assert!(s.keeps(up) && !s.keeps(down));
        assert!(!s.is_inactive());
        s.enabled = false;
        assert!(s.keeps(down) && s.is_inactive());
        s.enabled = true;
        s.planes[0].enabled = false;
        assert!(s.keeps(down) && s.is_inactive());
        // A box keeps only what is inside it.
        s.boxed = Some(Aabb {
            min: Vec3f { x: -1.0, y: -1.0, z: -1.0 },
            max: Vec3f { x: 1.0, y: 1.0, z: 1.0 },
        });
        assert!(!s.is_inactive());
        assert!(s.keeps(down));
        assert!(!s.keeps(Vec3f { x: 0.0, y: 0.0, z: 9.0 }));
    }
}

//! One pure deform stage: FK -> follow -> IK -> linear-blend skinning.
//! Bone rotations are parent-relative degrees, clockwise with y down. Rest
//! uses the authored constant or first key, never the pose at a seek time.

use crate::frame::{Affine, ImageMesh, Item, ItemKind, Local};
use crate::geometry::{self, PathCmd, VectorPath};
use crate::model::{Join, Layer, LayerKind, Loc, Media, PathShape, Prop};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bend { Cw, Ccw }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TwoBone {
    pub joint: [f64; 2],
    pub end: [f64; 2],
    pub unreachable: bool,
}

/// Reach with fixed lengths. Far targets fully extend the chain; targets
/// inside its inner radius fold it as far as possible. `bend` is the turn
/// from the upper to the lower segment (clockwise in y-down coordinates).
pub fn two_bone(base: [f64; 2], lengths: [f64; 2], target: [f64; 2], bend: Bend) -> TwoBone {
    let [a, b] = lengths.map(|v| v.max(0.0));
    let v = [target[0] - base[0], target[1] - base[1]];
    let distance = v[0].hypot(v[1]);
    let dir = if distance > 0.0 { [v[0] / distance, v[1] / distance] } else { [1.0, 0.0] };
    let d = distance.clamp((a - b).abs(), a + b);
    let x = if d > 0.0 { (a * a - b * b + d * d) / (2.0 * d) } else { 0.0 };
    let h = (a * a - x * x).max(0.0).sqrt() * if bend == Bend::Cw { -1.0 } else { 1.0 };
    TwoBone {
        joint: [base[0] + dir[0] * x - dir[1] * h, base[1] + dir[1] * x + dir[0] * h],
        end: [base[0] + dir[0] * d, base[1] + dir[1] * d],
        unreachable: distance > a + b + 1e-9 || distance + 1e-9 < (a - b).abs(),
    }
}

pub fn angle(a: [f64; 2], b: [f64; 2]) -> f64 { (b[1] - a[1]).atan2(b[0] - a[0]).to_degrees() }
pub fn turn(from: f64, to: f64) -> f64 { (to - from + 180.0).rem_euclid(360.0) - 180.0 }

#[derive(Clone, Debug, PartialEq)]
pub struct Bone {
    pub name: String,
    pub parent: Option<usize>,
    pub length: f64,
    pub rotate: Prop<1>,
    pub rest: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IK {
    pub end: usize,
    pub target: Prop<2>,
    pub bend: Bend,
    pub mix: Prop<1>,
    pub loc: Loc,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Follow {
    pub bone: usize,
    pub path: Arc<crate::Polyline>,
    pub along: Prop<1>,
    pub orient: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bones {
    pub bones: Vec<Bone>,
    /// Parent-first indices, computed once at load, including forward references.
    pub order: Vec<usize>,
    pub rest: Vec<BonePose>,
    pub follow: Vec<Follow>,
    pub ik: Vec<IK>,
    pub layers: Vec<Layer>,
    pub skins: Vec<Skin>,
}

impl Bones {
    pub fn is_animated(&self) -> bool {
        self.bones.iter().any(|b| b.rotate.is_animated())
            || self.follow.iter().any(|f| f.along.is_animated())
            || self.ik.iter().any(|ik| ik.target.is_animated() || ik.mix.is_animated())
            || self.layers.iter().any(|l| l.animated || l.span.is_some())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BonePose {
    pub base: [f64; 2],
    pub end: [f64; 2],
    pub angle: f64,
}

impl BonePose {
    pub fn map(&self) -> Affine { Affine::translate(self.base[0], self.base[1]).then(&Affine::rotate(self.angle)) }
    pub fn inverse(&self) -> Affine { Affine::rotate(-self.angle).then(&Affine::translate(-self.base[0], -self.base[1])) }
}

/// FK allocates just its output; `rotations` can sample authored properties
/// directly, without an intermediate angle array.
pub fn fk(bones: &[Bone], order: &[usize], mut rotations: impl FnMut(usize) -> f64) -> Vec<BonePose> {
    let mut pose = vec![BonePose::default(); bones.len()];
    for &i in order {
        let (base, parent_angle) = bones[i].parent.map_or(([0.0, 0.0], 0.0), |p| (pose[p].end, pose[p].angle));
        let angle = parent_angle + rotations(i);
        let (s, c) = angle.to_radians().sin_cos();
        pose[i] = BonePose { base, end: [base[0] + c * bones[i].length, base[1] + s * bones[i].length], angle };
    }
    pose
}

/// Per-evaluation workspace; each constraint walks the bones once and
/// reuses the marks. It has no state from another frame.
pub struct Solver<'a> {
    rig: &'a Bones,
    pub pose: Vec<BonePose>,
    marks: Vec<usize>,
    generation: usize,
}

impl<'a> Solver<'a> {
    pub fn new(rig: &'a Bones, rotations: impl FnMut(usize) -> f64) -> Self {
        Self { rig, pose: fk(&rig.bones, &rig.order, rotations), marks: vec![0; rig.bones.len()], generation: 0 }
    }

    fn move_branch(&mut self, root: usize, map: Affine, degrees: f64) {
        self.generation += 1;
        for &i in &self.rig.order {
            if i == root || self.rig.bones[i].parent.is_some_and(|p| self.marks[p] == self.generation) {
                self.marks[i] = self.generation;
                let p = &mut self.pose[i];
                p.base = map.apply(p.base);
                p.end = map.apply(p.end);
                p.angle += degrees;
            }
        }
    }

    pub fn follow(&mut self, follow: &Follow, along: f64) {
        let (position, tangent) = follow.path.sample(along);
        let old = self.pose[follow.bone];
        let degrees = if follow.orient { turn(old.angle, angle([0.0, 0.0], tangent)) } else { 0.0 };
        let map = Affine::translate(position[0], position[1]).then(&Affine::rotate(degrees)).then(&Affine::translate(-old.base[0], -old.base[1]));
        self.move_branch(follow.bone, map, degrees);
    }

    pub fn ik(&mut self, ik: &IK, target: [f64; 2], mix: f64) -> bool {
        let Some(upper) = self.rig.bones[ik.end].parent else { return false };
        let mix = mix.clamp(0.0, 1.0);
        if mix == 0.0 { return false; }
        let base = self.pose[upper].base;
        let solved = two_bone(base, [self.rig.bones[upper].length, self.rig.bones[ik.end].length], target, ik.bend);
        let old_upper = self.pose[upper].angle;
        let old_lower = self.pose[ik.end].angle - old_upper;
        let upper_angle = angle(base, solved.joint);
        let lower_angle = angle(solved.joint, solved.end) - upper_angle;
        let degrees = turn(old_upper, upper_angle) * mix;
        self.move_branch(upper, Affine::about(base, &Affine::rotate(degrees)), degrees);
        // A follow on the lower bone can disconnect it; IK reconnects the
        // two segments before it solves their relative bend.
        let joint = self.pose[upper].end;
        let lower_base = self.pose[ik.end].base;
        let degrees = turn(old_lower, lower_angle) * mix;
        let map = Affine::translate((joint[0] - lower_base[0]) * mix, (joint[1] - lower_base[1]) * mix)
            .then(&Affine::about(lower_base, &Affine::rotate(degrees)));
        self.move_branch(ik.end, map, degrees);
        solved.unreachable
    }

    pub fn skin_maps(&self) -> Vec<Affine> {
        self.pose.iter().zip(&self.rig.rest).map(|(pose, rest)| pose.map().then(&rest.inverse())).collect()
    }
}

/// The nearest three rest segments, inverse squared distance, normalized.
/// Fixed-size storage avoids an allocation/sort per vertex.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Weights(pub [(usize, f64); 3]);

impl Weights {
    pub fn at(point: [f64; 2], rest: &[BonePose]) -> Self {
        let mut nearest = [(0, f64::INFINITY); 3];
        for (i, bone) in rest.iter().enumerate() {
            let v = [bone.end[0] - bone.base[0], bone.end[1] - bone.base[1]];
            let w = [point[0] - bone.base[0], point[1] - bone.base[1]];
            let len2 = v[0] * v[0] + v[1] * v[1];
            let u = if len2 > 0.0 { ((w[0] * v[0] + w[1] * v[1]) / len2).clamp(0.0, 1.0) } else { 0.0 };
            let d = (w[0] - u * v[0]).powi(2) + (w[1] - u * v[1]).powi(2);
            let mut entry = (i, d);
            for slot in &mut nearest {
                if entry.1 < slot.1 { std::mem::swap(slot, &mut entry); }
            }
        }
        // Scale by the nearest distance before normalizing: bounded
        // arithmetic even at a joint, where several segments touch.
        let scale = nearest[0].1.max(1e-12);
        let mut total = 0.0;
        for (_, d) in &mut nearest {
            *d = if d.is_finite() { scale / d.max(1e-12) } else { 0.0 };
            total += *d;
        }
        if total > 0.0 { for (_, w) in &mut nearest { *w /= total; } }
        Self(nearest)
    }

    pub fn apply(&self, point: [f64; 2], maps: &[Affine]) -> [f64; 2] {
        let mut out = [0.0; 2];
        for &(i, w) in &self.0 {
            if w == 0.0 { continue; }
            let p = maps[i].apply(point);
            out[0] += p[0] * w;
            out[1] += p[1] * w;
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MeshGrid {
    pub uvs: Arc<[[f64; 2]]>,
    pub indices: Arc<[u32]>,
}

impl MeshGrid {
    pub fn new(cols: usize, rows: usize) -> Self {
        let mut uvs = Vec::with_capacity((cols + 1) * (rows + 1));
        for y in 0..=rows { for x in 0..=cols { uvs.push([x as f64 / cols as f64, y as f64 / rows as f64]); } }
        let mut indices = Vec::with_capacity(cols * rows * 6);
        for y in 0..rows { for x in 0..cols {
            let a = (y * (cols + 1) + x) as u32;
            let b = a + cols as u32 + 1;
            indices.extend_from_slice(&[a, a + 1, b + 1, a, b + 1, b]);
        } }
        Self { uvs: uvs.into(), indices: indices.into() }
    }
}

/// A static child's contour and weights are bound once at load. Animated
/// geometry is sampled before deforming and gets weights for those points.
#[derive(Clone, Debug, PartialEq)]
pub struct Skin {
    pub rigid: Option<usize>,
    pub contour: Option<Arc<VectorPath>>,
    pub size: Option<[f64; 2]>,
    pub weights: Arc<[Weights]>,
}

fn static_map(layer: &Layer, size: [f64; 2]) -> Option<Affine> {
    let t = &layer.transform;
    if t.along.is_some() { return None; }
    let at = t.at.constant()?;
    Some(Affine::translate(at[0] + t.x.constant()?[0], at[1] + t.y.constant()?[0])
        .then(&Local { anchor: t.anchor, scale: t.scale.constant()?, rotate: t.rotate.constant()?[0] }.affine(size)))
}

fn path_size(path: &VectorPath) -> [f64; 2] {
    geometry::bounds(path).map_or([0.0, 0.0], |b| [b[2].max(0.0), b[3].max(0.0)])
}

/// Subdivide outlines according to the shortest rest bone. Long straight
/// edges must have vertices near a joint to bend there. Curves use the
/// same flattening as Motion's other path operations, in document units.
fn contour(path: &VectorPath, rest: &[BonePose]) -> VectorPath {
    let step = rest.iter().map(|b| (b.end[0] - b.base[0]).hypot(b.end[1] - b.base[1]) * 0.25).fold(f64::INFINITY, f64::min).max(0.1);
    let mut out = VectorPath { cmds: Vec::new(), fill_rule: path.fill_rule };
    for (points, closed) in geometry::flatten(path, 0.1) {
        if let Some(p) = points.first() { out.move_to(p[0] as f32, p[1] as f32); }
        for pair in points.windows(2) {
            let [a, b] = [pair[0], pair[1]];
            let n = ((b[0] - a[0]).hypot(b[1] - a[1]) / step).ceil().max(1.0) as usize;
            for i in 1..=n {
                let u = i as f64 / n as f64;
                out.line_to((a[0] + (b[0] - a[0]) * u) as f32, (a[1] + (b[1] - a[1]) * u) as f32);
            }
        }
        if closed { out.close(); }
    }
    out
}

impl Skin {
    pub fn bind(layer: &Layer, rest: &[BonePose], rigid: Option<usize>) -> Self {
        let mut skin = Self { rigid, contour: None, size: None, weights: Arc::from([]) };
        if rigid.is_some() { return skin; }
        let shape = match &layer.kind {
            LayerKind::Path { data: PathShape::Static(path), .. } => Some((path.clone(), path_size(path))),
            LayerKind::Rect { size, radius, .. } => size.constant().zip(radius.constant()).map(|(s, r)| (Arc::new(geometry::rect_path(s, r[0])), s)),
            LayerKind::Ellipse { size, .. } => size.constant().map(|s| (Arc::new(geometry::ellipse_path(s)), s)),
            _ => None,
        };
        if let Some((path, size)) = shape {
            let path = contour(&path, rest);
            skin.size = Some(size);
            if let Some(map) = static_map(layer, size) {
                skin.weights = path.cmds.iter().filter_map(|cmd| match cmd {
                    PathCmd::MoveTo(x, y) | PathCmd::LineTo(x, y) => Some(Weights::at(map.apply([*x as f64, *y as f64]), rest)),
                    _ => None,
                }).collect::<Vec<_>>().into();
            }
            skin.contour = Some(path.into());
        } else if let LayerKind::Image { size: Some(size), mesh: Some(grid), .. } = &layer.kind {
            if let Some(size) = size.constant() {
                if let Some(map) = static_map(layer, size) {
                    skin.weights = grid.uvs.iter().map(|uv| Weights::at(map.apply([uv[0] * size[0], uv[1] * size[1]]), rest)).collect::<Vec<_>>().into();
                }
            }
        }
        skin
    }

    /// Item was evaluated in Bones coordinates. Bake its transform into
    /// points once, deform there, then let `placed` position the container.
    pub fn deform(&self, item: &mut Item, layer: &Layer, rest: &[BonePose], maps: &[Affine], placed: &Affine) {
        if let Some(bone) = self.rigid {
            place_item(item, &placed.then(&maps[bone]));
            return;
        }
        let (path, size, style) = match &item.kind {
            ItemKind::Path { path, fill, stroke, width, trim, cap, join } => (Some(path.clone()), self.size.unwrap_or_else(|| path_size(path)), Some((*fill, *stroke, *width, *trim, *cap, *join))),
            ItemKind::Rect { size, radius, fill, stroke, width, trim, cap } => (Some(self.contour.clone().unwrap_or_else(|| Arc::new(geometry::rect_path(*size, *radius)))), *size, Some((*fill, *stroke, *width, *trim, *cap, Join::Miter))),
            ItemKind::Ellipse { size, fill, stroke, width, trim, cap } => (Some(self.contour.clone().unwrap_or_else(|| Arc::new(geometry::ellipse_path(*size)))), *size, Some((*fill, *stroke, *width, *trim, *cap, Join::Round))),
            ItemKind::Image { size: Some(size), .. } => (None, *size, None),
            _ => { place_item(item, placed); return; }
        };
        let map = item.transform(size);
        let mut index = 0;
        let mut point = |p: [f64; 2]| {
            let p = map.apply(p);
            let weights = self.weights.get(index).copied().unwrap_or_else(|| Weights::at(p, rest));
            index += 1;
            weights.apply(p, maps)
        };
        if let (Some(path), Some((fill, stroke, width, trim, cap, join))) = (path, style) {
            let mut path = self.contour.as_deref().cloned().unwrap_or_else(|| contour(&path, rest));
            for cmd in &mut path.cmds {
                if let PathCmd::MoveTo(x, y) | PathCmd::LineTo(x, y) = cmd {
                    let p = point([*x as f64, *y as f64]);
                    *x = p[0] as f32;
                    *y = p[1] as f32;
                }
            }
            item.kind = ItemKind::Path { path: path.into(), fill, stroke, width: width * map.mean_scale(), trim, cap, join };
        } else if let (LayerKind::Image { mesh: Some(grid), .. }, ItemKind::Image { mesh, .. }) = (&layer.kind, &mut item.kind) {
            *mesh = Some(Arc::new(ImageMesh {
                positions: grid.uvs.iter().map(|uv| point([uv[0] * size[0], uv[1] * size[1]])).collect(),
                uvs: grid.uvs.clone(), indices: grid.indices.clone(),
            }));
        }
        if let ItemKind::Image { src: Media::Backdrop, focus, .. } = &mut item.kind {
            if focus.is_none() {
                let centre = map.apply([size[0] * 0.5, size[1] * 0.5]);
                *focus = Some(placed.apply(Weights::at(centre, rest).apply(centre, maps)));
            }
        }
        item.feather *= map.mean_scale();
        item.blur *= map.mean_scale();
        item.world = *placed;
        item.local = Local { anchor: [0.0, 0.0], scale: [1.0, 1.0], rotate: 0.0 };
    }
}

fn place_item(item: &mut Item, placed: &Affine) {
    item.world = placed.then(&item.world);
    match &mut item.kind {
        ItemKind::Group { children, mask, .. } => {
            for child in children { place_item(child, placed); }
            if let Some(mask) = mask { place_item(mask, placed); }
        }
        ItemKind::Fly { inner, .. } | ItemKind::Anchored { inner, .. } => place_item(inner, placed),
        _ => {}
    }
}

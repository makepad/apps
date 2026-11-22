//! What of a part prints apart, read off the plate's slice: each layer's outlines, joined to the
//! outlines they overlap on the layers next to it, and on a layer to those its slicer's closing
//! fuses them with (nearer than twice its radius: a mesh's slice is closed so already, a field's
//! is cut without it). A part whose outlines join into one piece prints as one; a ball printed in
//! its cage prints as two. A piece none of whose outlines has a path prints nothing, and is none.
//!
//! A piece is its layers' outlines on the plate, each layer a slab as tall as the layer: its mass
//! and inertia are theirs (area times height, with the outlines' moments), its hulls are their
//! corners, and the part's field drawn or met as the piece is that field within the piece's mask:
//! its outlines grown into the gaps round them, over runs of its layers.

use makepad_csg_mech::geom::P3;
use makepad_csg_mech::mass::MassProps;
use makepad_csg_sdf::{SdfNode, SdfOp, SdfShape, SketchForm, SketchSolid};
use makepad_fab_slicer::clip::{difference_ex, intersection_ex, union2_ex, union_ex};
use makepad_fab_slicer::geom::{bbox_of, dist_point_segment, BBox, ExPolygon, Polygon, Pt, SCALE};
use makepad_fab_slicer::offset::{closing, offset_mm, DEFAULT_MITER};
use makepad_fab_slicer::slice::SliceParams;
use makepad_fab_toolpath::{Island, ObjectPlan};
use std::sync::Arc;

/// Plate frame from a shape's (row major: `p_plate = m * p_shape`), turned, moved and evenly scaled.
pub type Affine = [[f64; 4]; 3];

/// A part's slice on the plate: its model layers' slabs (plate mm, bottom up), outlines (plate XY)
/// and what prints them (the plan's frame, `at` from the plate's).
#[derive(Clone, Debug, Default)]
pub struct Layers {
    pub slabs: Vec<(f64, f64)>,
    pub regions: Vec<Vec<ExPolygon>>,
    pub islands: Vec<Vec<Arc<Island>>>,
    pub at: Pt,
}

/// One piece that prints apart: per layer it has outlines on, the layer and those outlines.
#[derive(Clone, Debug, Default)]
pub struct Piece {
    pub layers: Vec<(usize, Vec<ExPolygon>)>,
}

/// A run of a piece's mask: a slab of the plate (mm), and the outlines the mask keeps over it
/// (plate XY, grown into the gaps round the piece), none where no other piece is.
#[derive(Clone, Debug)]
pub struct MaskRun {
    pub lo: f64,
    pub hi: f64,
    pub regions: Option<Vec<ExPolygon>>,
}

/// How far a mask grows a piece's outlines into the gaps round it (mm), most first: the most that
/// keeps clear of the other pieces' grown as far. The slicer keeps regions of a layer at least twice
/// its closing radius (0.098 mm) apart, so the last but one always fits within a layer.
const GROW: [f64; 6] = [1.0, 0.5, 0.2, 0.08, 0.04, 0.0];

impl Layers {
    /// The plan's model layers (its support layers left out, a painted layer's regions as one), on
    /// the plate: `at` is where the plan's frame sits. A layer prints from its top down by its height;
    /// the first model layer's bottom is the part's (a raft under it lifts the print, not the part).
    pub fn of(plan: &ObjectPlan, at: Pt) -> Layers {
        let mut model: Vec<(f64, f64, Vec<&ExPolygon>, Vec<Arc<Island>>)> = Vec::new();
        for (z, h, l) in &plan.layers {
            if l.support_only {
                continue;
            }
            match model.iter_mut().find(|(mz, ..)| (mz - z).abs() < 1e-9) {
                Some(m) => {
                    m.2.extend(l.slices.iter());
                    m.3.extend(l.islands.iter().cloned());
                }
                None => model.push((*z, *h, l.slices.iter().collect(), l.islands.clone())),
            }
        }
        model.sort_by(|a, b| a.0.total_cmp(&b.0));
        let lift = model.first().map_or(0.0, |(z, h, ..)| z - h);
        let mut out = Layers { at, ..Default::default() };
        for (z, h, slices, islands) in model {
            let owned: Vec<ExPolygon> = slices.into_iter().cloned().collect();
            let mut regions = if owned.len() > 1 { union_ex(&owned) } else { owned };
            regions.iter_mut().for_each(|r| r.translate(at));
            out.slabs.push((z - h - lift, z - lift));
            out.regions.push(regions);
            out.islands.push(islands);
        }
        out
    }

    /// Its pieces: each layer's outlines joined to those they overlap on the next layer, bottom up
    /// (the lowest first, then by their order on its first layer).
    pub fn pieces(&self) -> Vec<Piece> {
        let first: Vec<usize> = self.regions.iter().scan(0, |n, r| {
            let at = *n;
            *n += r.len();
            Some(at)
        }).collect();
        let total = self.regions.iter().map(Vec::len).sum();
        let mut parent: Vec<usize> = (0..total).collect();
        fn root(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        let boxes: Vec<Vec<BBox>> = self.regions.iter().map(|rs| rs.iter().map(ExPolygon::bbox).collect()).collect();
        // on a layer, the regions the slicer's closing fuses
        let r = SliceParams::default().closing_radius * SCALE;
        let near = |a: &BBox, b: &BBox| a.min.x as f64 - 2.0 * r <= b.max.x as f64 && b.min.x as f64 - 2.0 * r <= a.max.x as f64 && a.min.y as f64 - 2.0 * r <= b.max.y as f64 && b.min.y as f64 - 2.0 * r <= a.max.y as f64;
        for (l, rs) in self.regions.iter().enumerate() {
            let b = &boxes[l];
            if !(0..rs.len()).any(|a| (a + 1..rs.len()).any(|c| near(&b[a], &b[c]))) {
                continue;
            }
            for closed in closing(rs, r, r, DEFAULT_MITER) {
                let cb = closed.bbox();
                let within: Vec<usize> = (0..rs.len()).filter(|&a| cb.overlaps(&b[a]) && !intersection_ex(std::slice::from_ref(&closed), std::slice::from_ref(&rs[a])).is_empty()).collect();
                for w in within.windows(2) {
                    let (x, y) = (root(&mut parent, first[l] + w[0]), root(&mut parent, first[l] + w[1]));
                    parent[x.max(y)] = x.min(y);
                }
            }
        }
        for l in 0..self.regions.len().saturating_sub(1) {
            for (a, ra) in self.regions[l].iter().enumerate() {
                for (b, rb) in self.regions[l + 1].iter().enumerate() {
                    let (na, nb) = (first[l] + a, first[l + 1] + b);
                    if root(&mut parent, na) == root(&mut parent, nb) || !boxes[l][a].overlaps(&boxes[l + 1][b]) {
                        continue;
                    }
                    if !intersection_ex(std::slice::from_ref(ra), std::slice::from_ref(rb)).is_empty() {
                        let (x, y) = (root(&mut parent, na), root(&mut parent, nb));
                        parent[x.max(y)] = x.min(y);
                    }
                }
            }
        }
        let mut index: Vec<Option<usize>> = vec![None; total];
        let mut pieces: Vec<Piece> = Vec::new();
        for (l, rs) in self.regions.iter().enumerate() {
            for (a, r) in rs.iter().enumerate() {
                let top = root(&mut parent, first[l] + a);
                let k = *index[top].get_or_insert_with(|| {
                    pieces.push(Piece::default());
                    pieces.len() - 1
                });
                let piece = &mut pieces[k];
                match piece.layers.last_mut().filter(|(pl, _)| *pl == l) {
                    Some((_, regions)) => regions.push(r.clone()),
                    None => piece.layers.push((l, vec![r.clone()])),
                }
            }
        }
        pieces.retain(|p| self.prints(p));
        pieces
    }

    /// Whether a path of the plan lies in one of the piece's outlines: a speck too small for a
    /// line prints nothing.
    fn prints(&self, piece: &Piece) -> bool {
        piece.layers.iter().any(|(l, regions)| {
            let boxes: Vec<BBox> = regions.iter().map(ExPolygon::bbox).collect();
            self.islands[*l].iter().flat_map(|i| i.walls.iter().chain(&i.fills)).flat_map(|p| p.pts.iter()).any(|p| {
                let q = p.add(self.at);
                regions.iter().zip(&boxes).any(|(r, b)| q.x >= b.min.x && q.x <= b.max.x && q.y >= b.min.y && q.y <= b.max.y && r.contains(q))
            })
        })
    }

    /// Each piece's mask, over its layers and a layer past each end of them.
    pub fn masks(&self, pieces: &[Piece]) -> Vec<Vec<MaskRun>> {
        let n = self.slabs.len();
        (0..pieces.len()).map(|k| {
            let piece = &pieces[k];
            let (Some(&(a, _)), Some(&(b, _))) = (piece.layers.first(), piece.layers.last()) else { return Vec::new() };
            let own = |l: usize| piece.layers.iter().find(|(pl, _)| *pl == l).map_or(&[][..], |(_, r)| &r[..]);
            let others = |l: usize| -> Vec<ExPolygon> {
                if l >= n {
                    return Vec::new();
                }
                pieces.iter().enumerate().filter(|(j, _)| *j != k).flat_map(|(_, p)| p.layers.iter().filter(|(pl, _)| *pl == l).flat_map(|(_, r)| r.iter().cloned())).collect()
            };
            // the runs' layers: a layer's others, and for the ends the layer past it (the mask reaches into it)
            let near = |l: usize| -> Vec<ExPolygon> {
                let mut o = others(l);
                if l == a && a > 0 {
                    o.extend(others(a - 1));
                }
                if l == b {
                    o.extend(others(b + 1));
                }
                o
            };
            let mut runs: Vec<MaskRun> = Vec::new();
            let mut l = a;
            while l <= b {
                let (mut mine, mut theirs) = (own(l).to_vec(), near(l));
                let grow = GROW.iter().copied().find(|g| clear(&mine, &theirs, *g)).unwrap_or(0.0);
                let mut end = l;
                while end < b {
                    let (m2, t2) = (union2_ex(&mine, own(end + 1)), union2_ex(&theirs, &near(end + 1)));
                    if !clear(&m2, &t2, grow) || theirs.is_empty() != t2.is_empty() {
                        break;
                    }
                    (mine, theirs) = (m2, t2);
                    end += 1;
                }
                let lo = if l == a { self.slabs[a].0 - (self.slabs[a].1 - self.slabs[a].0) } else { self.slabs[l].0 };
                let hi = if end == b { self.slabs[b].1 + (self.slabs[b].1 - self.slabs[b].0) } else { self.slabs[end].1 };
                let regions = (!theirs.is_empty()).then(|| if grow > 0.0 { offset_mm(&mine, grow) } else { mine });
                runs.push(MaskRun { lo, hi, regions });
                l = end + 1;
            }
            runs
        }).collect()
    }
}

/// Whether `mine` and `theirs`, each grown by `grow` (mm), keep clear of each other.
fn clear(mine: &[ExPolygon], theirs: &[ExPolygon], grow: f64) -> bool {
    if mine.is_empty() || theirs.is_empty() {
        return true;
    }
    let (a, b) = if grow > 0.0 { (offset_mm(mine, grow), offset_mm(theirs, grow)) } else { (mine.to_vec(), theirs.to_vec()) };
    !bbox_of(&a).overlaps(&bbox_of(&b)) || intersection_ex(&a, &b).is_empty()
}

fn mm(p: Pt) -> [f64; 2] {
    [p.x as f64 / SCALE, p.y as f64 / SCALE]
}

impl Piece {
    /// Its mass, centre and inertia (plate mm, g), of a plastic `density` g/mm^3 filled to `fill`:
    /// each layer's outlines as a prism of the layer's slab.
    pub fn mass(&self, layers: &Layers, density: f64, fill: f64) -> MassProps {
        // about a point of the piece, for the precision of the second moments
        let o = self.layers.first().and_then(|(_, r)| r.first()).map_or([0.0; 2], |r| mm(r.contour.pts[0]));
        let (mut v, mut first, mut second) = (0.0, [0.0; 3], [[0.0; 3]; 3]);
        for (l, regions) in &self.layers {
            let (z0, z1) = layers.slabs[*l];
            let h = z1 - z0;
            let (z_1, z_2) = ((z1 * z1 - z0 * z0) / 2.0, (z1 * z1 * z1 - z0 * z0 * z0) / 3.0);
            // the outlines' area and moments: holes run clockwise and take theirs away
            let (mut a, mut sx, mut sy, mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
            for ring in regions.iter().flat_map(ExPolygon::rings) {
                let pts = &ring.pts;
                for i in 0..pts.len() {
                    let (p, q) = (mm(pts[i]), mm(pts[(i + 1) % pts.len()]));
                    let (x0, y0, x1, y1) = (p[0] - o[0], p[1] - o[1], q[0] - o[0], q[1] - o[1]);
                    let c = x0 * y1 - x1 * y0;
                    a += c / 2.0;
                    sx += (x0 + x1) * c / 6.0;
                    sy += (y0 + y1) * c / 6.0;
                    sxx += (x0 * x0 + x0 * x1 + x1 * x1) * c / 12.0;
                    syy += (y0 * y0 + y0 * y1 + y1 * y1) * c / 12.0;
                    sxy += (x0 * y1 + 2.0 * x0 * y0 + 2.0 * x1 * y1 + x1 * y0) * c / 24.0;
                }
            }
            v += a * h;
            first[0] += sx * h;
            first[1] += sy * h;
            first[2] += a * z_1;
            second[0][0] += sxx * h;
            second[1][1] += syy * h;
            second[2][2] += a * z_2;
            second[0][1] += sxy * h;
            second[0][2] += sx * z_1;
            second[1][2] += sy * z_1;
        }
        second[1][0] = second[0][1];
        second[2][0] = second[0][2];
        second[2][1] = second[1][2];
        let c = if v > 1e-12 { first.map(|f| f / v) } else { [0.0; 3] };
        let cov: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| second[i][j] - v * c[i] * c[j]));
        let rho = density * fill;
        let tr = rho * (cov[0][0] + cov[1][1] + cov[2][2]);
        let inertia = std::array::from_fn(|i| std::array::from_fn(|j| if i == j { tr - rho * cov[i][j] } else { -rho * cov[i][j] }));
        MassProps { volume: v, mass: v * rho, com: [c[0] + o[0], c[1] + o[1], c[2]], inertia }
    }

    /// The corners of its outlines at the bottom and the top of each layer (plate mm): what its hulls
    /// are made of. A hole's corners are inside its outline's hull, and left out.
    pub fn corners(&self, layers: &Layers) -> Vec<P3> {
        let mut out = Vec::new();
        for (l, regions) in &self.layers {
            let (z0, z1) = layers.slabs[*l];
            for r in regions {
                for p in &r.contour.pts {
                    let [x, y] = mm(*p);
                    out.push([x, y, z0]);
                    out.push([x, y, z1]);
                }
            }
        }
        out
    }

    /// Its box on the plate (mm).
    pub fn bounds(&self, layers: &Layers) -> (P3, P3) {
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for (l, regions) in &self.layers {
            let (z0, z1) = layers.slabs[*l];
            let b = bbox_of(regions);
            let ([x0, y0], [x1, y1]) = (mm(b.min), mm(b.max));
            lo = [lo[0].min(x0), lo[1].min(y0), lo[2].min(z0)];
            hi = [hi[0].max(x1), hi[1].max(y1), hi[2].max(z1)];
        }
        (lo, hi)
    }

    /// How far `p` (plate mm) is from it in its layer: 0 inside one of the layer's outlines, else to
    /// the nearest of their edges; a point under or over every layer of it is as far as its nearest
    /// layer is, plus the height between.
    pub fn distance(&self, layers: &Layers, p: P3) -> f64 {
        let q = Pt::from_mm(p[0], p[1]);
        self.layers.iter().map(|(l, regions)| {
            let (z0, z1) = layers.slabs[*l];
            let dz = (z0 - p[2]).max(p[2] - z1).max(0.0);
            let across = if regions.iter().any(|r| r.contains(q)) {
                0.0
            } else {
                regions.iter().flat_map(ExPolygon::rings).flat_map(|ring| (0..ring.pts.len()).map(move |i| (ring.pts[i], ring.pts[(i + 1) % ring.pts.len()]))).map(|(a, b)| dist_point_segment(q, a, b) / SCALE).fold(f64::MAX, f64::min)
            };
            across.hypot(dz)
        }).fold(f64::MAX, f64::min)
    }
}

/// The piece of `pieces` a point (plate mm) is in, or else the nearest.
pub fn which(layers: &Layers, pieces: &[Piece], p: P3) -> Option<usize> {
    (0..pieces.len()).min_by(|a, b| pieces[*a].distance(layers, p).total_cmp(&pieces[*b].distance(layers, p)))
}

/// The inverse of a turn with an even scale and a move: per plate axis its direction in the shape's
/// frame (unit), the scale, and the move.
fn frame_of(m: &Affine) -> Result<([[f64; 3]; 3], f64, [f64; 3]), String> {
    let s = (m[0][0] * m[0][0] + m[1][0] * m[1][0] + m[2][0] * m[2][0]).sqrt();
    if !(s > 1e-12) {
        return Err("a part's placement flattens it".into());
    }
    let rows: [[f64; 3]; 3] = std::array::from_fn(|r| std::array::from_fn(|c| m[r][c] / s));
    Ok((rows, s, [m[0][3], m[1][3], m[2][3]]))
}

/// A mask's runs as one solid in a frame `to_plate` puts on the plate (rigid with an even scale):
/// each run its outlines pulled through its slab, a run with none the whole of `box_xy` (plate mm).
/// Two runs that meet share a plane, which each reads as its face: the union would read zero there
/// inside the solid, where no surface is. So where one run's outlines lie within the other's, that
/// run is pulled on through the other's slab; else the pair has a bridge, the outlines both keep
/// pulled through both slabs. Either way the plane is inside an extrusion wherever both runs are,
/// and the solid is the same: what is pulled on stays within the other run.
fn mask_node(runs: &[MaskRun], to_plate: &Affine, box_xy: ([f64; 2], [f64; 2])) -> Result<SdfNode, String> {
    let (rows, s, t) = frame_of(to_plate)?;
    let loop_of = |ring: &Polygon| -> Vec<[f64; 2]> { ring.pts.iter().map(|p| { let [x, y] = mm(*p); [(x - t[0]) / s, (y - t[1]) / s] }).collect() };
    // whether outlines `a` lie within `b` (none: the whole box)
    let within = |a: &Option<Vec<ExPolygon>>, b: &Option<Vec<ExPolygon>>| match (a, b) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(a), Some(b)) => difference_ex(a, b).is_empty(),
    };
    let mut slabs: Vec<MaskRun> = runs.to_vec();
    for (i, pair) in runs.windows(2).enumerate() {
        let (a, b) = (&pair[0], &pair[1]);
        if within(&a.regions, &b.regions) {
            slabs[i].hi = b.hi;
        } else if within(&b.regions, &a.regions) {
            slabs[i + 1].lo = a.lo;
        } else if let (Some(ra), Some(rb)) = (&a.regions, &b.regions) {
            slabs.push(MaskRun { lo: a.lo, hi: b.hi, regions: Some(intersection_ex(ra, rb)) });
        }
    }
    let mut of = Vec::with_capacity(slabs.len());
    for run in &slabs {
        let regions: Vec<Vec<Vec<[f64; 2]>>> = match &run.regions {
            Some(rs) => rs.iter().map(|r| r.rings().map(loop_of).collect()).collect(),
            None => {
                let (lo, hi) = box_xy;
                let c = |x: f64, y: f64| [(x - t[0]) / s, (y - t[1]) / s];
                vec![vec![vec![c(lo[0], lo[1]), c(hi[0], lo[1]), c(hi[0], hi[1]), c(lo[0], hi[1])]]]
            }
        };
        if regions.is_empty() {
            continue;
        }
        let form = SketchForm::Extrude { u: rows[0], v: rows[1], w: rows[2], z0: (run.lo - t[2]) / s, z1: (run.hi - t[2]) / s, draft: 0.0 };
        of.push(SdfNode::Sketch(Arc::new(SketchSolid::new(&regions, form)?)));
    }
    match of.len() {
        0 => Err("a piece's mask is empty".into()),
        1 => Ok(of.pop().unwrap()),
        _ => Ok(SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of }),
    }
}

/// The part's field `shape` (on the plate by `to_plate`: turned, moved and evenly scaled) within a
/// piece's mask: the piece as a field, in the shape's frame. `bounds`: the piece's box on the plate.
pub fn masked(shape: &SdfShape, to_plate: &Affine, runs: &[MaskRun], bounds: (P3, P3)) -> Result<SdfShape, String> {
    let (lo, hi) = bounds;
    let pad = GROW[0] + 1.0;
    let mask = mask_node(runs, to_plate, ([lo[0] - pad, lo[1] - pad], [hi[0] + pad, hi[1] + pad]))?;
    // the piece's box in the shape's frame, within the shape's
    let (rows, s, t) = frame_of(to_plate)?;
    let (mut min, mut max) = ([f64::MAX; 3], [f64::MIN; 3]);
    for c in 0..8 {
        let p: P3 = std::array::from_fn(|k| if c & (1 << k) == 0 { lo[k] - pad } else { hi[k] + pad });
        let d = [p[0] - t[0], p[1] - t[1], p[2] - t[2]];
        for k in 0..3 {
            let q = (rows[0][k] * d[0] + rows[1][k] * d[1] + rows[2][k] * d[2]) / s;
            min[k] = min[k].min(q);
            max[k] = max[k].max(q);
        }
    }
    let min = std::array::from_fn(|k| min[k].max(shape.min[k]));
    let max = std::array::from_fn(|k| max[k].min(shape.max[k]));
    Ok(SdfShape { root: SdfNode::Combine { op: SdfOp::Intersection, blend: 0.0, of: vec![shape.root.clone(), mask] }, min, max, materials: shape.materials.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mask_reads_below_zero_where_its_runs_meet() {
        let square = |x0: f64, x1: f64| ExPolygon::new(Polygon::new(vec![Pt::from_mm(x0, -2.0), Pt::from_mm(x1, -2.0), Pt::from_mm(x1, 2.0), Pt::from_mm(x0, 2.0)]));
        let within = SdfShape { root: SdfNode::Box { size: [20.0; 3], radius: 0.0 }, min: [-10.0; 3], max: [10.0; 3], materials: Vec::new() };
        let to_plate: Affine = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let mask = |upper: ExPolygon| {
            let runs = [MaskRun { lo: 0.0, hi: 1.0, regions: Some(vec![square(-2.0, 2.0)]) }, MaskRun { lo: 1.0, hi: 2.0, regions: Some(vec![upper]) }];
            crate::solid::Solid::of_field(&masked(&within, &to_plate, &runs, ([-2.0, -2.0, 0.0], [3.0, 2.0, 2.0])).expect("a mask"), to_plate, None)
        };
        // runs meeting at z 1: the upper one's outline moved along x (they share [-1, 2]: a
        // bridge), then within the lower one's (the upper is pulled down through the lower slab).
        // On the shared plane 0.2 inside both outlines; on the step only the lower run covers.
        for (upper, inside, step) in [(square(-1.0, 3.0), [1.8, 0.0, 1.0], [-1.5, 0.0, 1.0]), (square(-1.0, 1.0), [0.8, 0.0, 1.0], [1.5, 0.0, 1.0])] {
            let mut v = [0.0; 2];
            mask(upper).values(&[inside, step], &mut v);
            assert!(v[0] <= -0.19, "where the runs meet, 0.2 inside both at {inside:?}: {:.4}, wanted at most -0.19", v[0]);
            assert!(v[1].abs() < 1e-6, "on the step's face at {step:?}: {:.2e}, wanted 0", v[1]);
        }
    }
}

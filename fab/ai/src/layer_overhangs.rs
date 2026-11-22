//! A part's overhangs read from its slice, not from a mesh: on each layer, the faces between it
//! and the layer under it that lean past the limit, as the bed sees them, joined across layers
//! into patches. A field part is never meshed for them (its field is the model): the layers are
//! the ones its slice cuts.

use makepad_fab_slicer::clip::{difference_ex, intersection_ex, union_ex};
use makepad_fab_slicer::geom::{BBox, ExPolygon, Pt, SCALE};
use makepad_fab_slicer::offset::offset_mm;
use makepad_fab_toolpath::ObjectLayer;

/// Less than this (mm²) is a sliver of the outlines' rounding, no face.
const LEAST: f64 = 0.01;
/// Faces of two layers this close (mm) are one surface: a slope's faces meet at the outline
/// between them.
const LINK: f64 = 0.05;
/// A slope this near flat (degrees from vertical) stands for flat: its reach is long, not endless.
const FLATTEST: f64 = 89.9;

/// A part's model layers as its slice cuts them.
pub struct PartLayers {
    /// Each layer's top and height on the plate (mm), the part's foot at 0, and its outline in the
    /// plan's frame.
    pub layers: Vec<(f64, f64, Vec<ExPolygon>)>,
}

impl PartLayers {
    /// Layers at print heights (top, height, outline), lowest first: a raft lifts the print, and
    /// the part's first layer stands at the plate's 0.
    pub fn new(mut layers: Vec<(f64, f64, Vec<ExPolygon>)>) -> PartLayers {
        layers.sort_by(|a, b| a.0.total_cmp(&b.0));
        let lift = layers.first().map_or(0.0, |l| l.0 - l.1);
        for l in &mut layers {
            l.0 -= lift;
        }
        PartLayers { layers }
    }

    /// A sliced part's own layers: no support's, a painted layer's filaments joined.
    pub fn of_plan(plan: &[(f64, f64, ObjectLayer)]) -> PartLayers {
        let mut own: Vec<&(f64, f64, ObjectLayer)> = plan.iter().filter(|l| !l.2.support_only).collect();
        own.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut layers: Vec<(f64, f64, Vec<ExPolygon>, usize)> = Vec::new();
        for (z, h, layer) in own {
            match layers.last_mut() {
                Some(last) if (last.0 - z).abs() < 1e-9 => {
                    last.2.extend(layer.slices.iter().cloned());
                    last.3 += 1;
                }
                _ => layers.push((*z, *h, layer.slices.to_vec(), 1)),
            }
        }
        PartLayers::new(layers.into_iter().map(|(z, h, slices, parts)| (z, h, if parts > 1 { union_ex(&slices) } else { slices })).collect())
    }
}

/// One layer's overhang: a face, or faces side by side, between it and the layer under it.
pub struct Face {
    /// The layer it is the underside of.
    pub layer: usize,
    /// Its height on the plate: that layer's bottom (mm).
    pub z: f64,
    /// Plan frame.
    pub region: ExPolygon,
    pub area_mm2: f64,
    /// How far past the limit it leans, 1 to 5, as a facet's degree.
    pub degree: u8,
}

/// Faces of layers one over the other that meet: one overhanging surface. Plate mm.
pub struct LayerPatch {
    pub faces: Vec<Face>,
    /// As the bed sees it.
    pub area_mm2: f64,
    pub centroid: [f64; 3],
    /// A point on its lowest face, and that face's layer.
    pub lowest: [f64; 3],
    pub low: usize,
    /// A point on it near its middle, to write its name at (a ring's middle is in its hole), and
    /// that point's layer.
    pub mark: [f64; 3],
    pub mark_layer: usize,
    pub min: [f64; 3],
    pub max: [f64; 3],
    /// Its steepest face's degree.
    pub worst: u8,
}

impl LayerPatch {
    /// Its faces' outlines, each at its height (plate mm; `at` takes the plan's frame there).
    pub fn outline_points(&self, at: [f64; 2]) -> Vec<[f64; 3]> {
        self.faces.iter().flat_map(|f| f.region.rings().flat_map(|r| r.pts.iter()).map(move |p| [p.x_mm() + at[0], p.y_mm() + at[1], f.z])).collect()
    }
}

/// The middle of a region (plan mm), by its area.
fn centroid(e: &ExPolygon) -> [f64; 2] {
    let (mut a, mut x, mut y) = (0.0, 0.0, 0.0);
    for ring in e.rings() {
        let n = ring.pts.len();
        for i in 0..n {
            let (p, q) = (ring.pts[i], ring.pts[(i + 1) % n]);
            let (px, py, qx, qy) = (p.x_mm(), p.y_mm(), q.x_mm(), q.y_mm());
            let cross = px * qy - qx * py;
            a += cross;
            x += (px + qx) * cross;
            y += (py + qy) * cross;
        }
    }
    if a.abs() < 1e-12 {
        let b = e.bbox().center();
        return [b.x_mm(), b.y_mm()];
    }
    [x / (3.0 * a), y / (3.0 * a)]
}

/// Points of a region a spacing apart (plan mm): inside it on a grid, and along its rings, so a
/// band thinner than the spacing has its share.
pub fn region_samples(e: &ExPolygon, spacing: f64) -> Vec<[f64; 2]> {
    let mut out = Vec::new();
    let b = e.bbox();
    let (x0, y0, x1, y1) = (b.min.x_mm(), b.min.y_mm(), b.max.x_mm(), b.max.y_mm());
    let (i0, i1) = ((x0 / spacing).floor() as i64, (x1 / spacing).ceil() as i64);
    let (j0, j1) = ((y0 / spacing).floor() as i64, (y1 / spacing).ceil() as i64);
    for i in i0..i1 {
        for j in j0..j1 {
            let (x, y) = ((i as f64 + 0.5) * spacing, (j as f64 + 0.5) * spacing);
            if x >= x0 && x <= x1 && y >= y0 && y <= y1 && e.contains(Pt::from_mm(x, y)) {
                out.push([x, y]);
            }
        }
    }
    for ring in e.rings() {
        let n = ring.pts.len();
        let mut walked = spacing;
        for i in 0..n {
            let (p, q) = (ring.pts[i], ring.pts[(i + 1) % n]);
            let (px, py, qx, qy) = (p.x_mm(), p.y_mm(), q.x_mm(), q.y_mm());
            let length = (qx - px).hypot(qy - py);
            let mut at = spacing - walked;
            while at < length {
                let t = at / length;
                out.push([px + t * (qx - px), py + t * (qy - py)]);
                at += spacing;
            }
            walked = length - (at - spacing);
        }
    }
    if out.is_empty() {
        if let Some(p) = e.contour.pts.first() {
            out.push([p.x_mm(), p.y_mm()]);
        }
    }
    out
}

fn area(ex: &[ExPolygon]) -> f64 {
    ex.iter().map(ExPolygon::area_mm2).sum()
}

fn grown(b: &BBox, mm: f64) -> BBox {
    let d = (mm * SCALE) as i64;
    BBox { min: Pt::new(b.min.x - d, b.min.y - d), max: Pt::new(b.max.x + d, b.max.y + d) }
}

/// Every layer's faces that lean past `limit` (degrees from vertical a face may lean).
pub fn faces(part: &PartLayers, limit: f64) -> Vec<Face> {
    let step = (90.0 - limit) / 5.0;
    let mut out = Vec::new();
    for k in 1..part.layers.len() {
        let (z, h, upper) = (part.layers[k].0, part.layers[k].1, &part.layers[k].2);
        let lower = &part.layers[k - 1].2;
        let between = difference_ex(upper, lower);
        if area(&between) < LEAST {
            continue;
        }
        // How far out of the layer under it a face leaning `d` degree steps past the limit gets.
        let reach = |d: usize| h * (limit + step * d as f64).min(FLATTEST).to_radians().tan();
        let past = |d: usize| -> Vec<ExPolygon> { difference_ex(&between, &offset_mm(lower, reach(d))).into_iter().filter(|e| e.area_mm2() >= LEAST).collect() };
        let first = past(0);
        if first.is_empty() {
            continue;
        }
        // The faces whole: what leans past the limit, grown back over the band it stands out of.
        let leaning = intersection_ex(&between, &offset_mm(&first, reach(0)));
        let mut beyond = vec![first];
        for d in 1..5 {
            let b = past(d);
            if b.is_empty() {
                break;
            }
            beyond.push(b);
        }
        for region in leaning {
            let area_mm2 = region.area_mm2();
            if area_mm2 < LEAST {
                continue;
            }
            let bb = region.bbox();
            let one = [region.clone()];
            let steeper = (1..beyond.len()).filter(|&d| {
                let near: Vec<ExPolygon> = beyond[d].iter().filter(|b| b.bbox().overlaps(&bb)).cloned().collect();
                !near.is_empty() && !intersection_ex(&one, &near).is_empty()
            }).count();
            out.push(Face { layer: k, z: z - h, region, area_mm2, degree: 1 + steeper as u8 });
        }
    }
    out
}

fn root(sets: &mut [usize], mut i: usize) -> usize {
    while sets[i] != i {
        sets[i] = sets[sets[i]];
        i = sets[i];
    }
    i
}

/// The faces joined into patches where a layer's meet the next one's; `at` takes the plan's
/// frame to the plate.
pub fn patches(part: &PartLayers, at: [f64; 2], limit: f64, spacing: f64) -> Vec<LayerPatch> {
    let faces = faces(part, limit);
    let mut sets: Vec<usize> = (0..faces.len()).collect();
    let boxes: Vec<BBox> = faces.iter().map(|f| grown(&f.region.bbox(), LINK)).collect();
    // Faces come out by layer: each meets only the next layer's.
    let mut start = 0;
    while start < faces.len() {
        let layer = faces[start].layer;
        let end = start + faces[start..].iter().take_while(|f| f.layer == layer).count();
        let next_end = end + faces[end..].iter().take_while(|f| f.layer == layer + 1).count();
        for a in start..end {
            let near = offset_mm(&[faces[a].region.clone()], LINK);
            for b in end..next_end {
                if boxes[a].overlaps(&boxes[b]) && !intersection_ex(&near, &[faces[b].region.clone()]).is_empty() {
                    let (ra, rb) = (root(&mut sets, a), root(&mut sets, b));
                    if ra != rb {
                        sets[ra.max(rb)] = ra.min(rb);
                    }
                }
            }
        }
        start = end;
    }
    let mut groups: Vec<Vec<Face>> = Vec::new();
    let mut group_of: Vec<Option<usize>> = vec![None; faces.len()];
    for (i, face) in faces.into_iter().enumerate() {
        let r = root(&mut sets, i);
        let g = *group_of[r].get_or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(face);
    }
    groups.into_iter().map(|faces| patch_of(faces, at, spacing)).collect()
}

fn patch_of(faces: Vec<Face>, at: [f64; 2], spacing: f64) -> LayerPatch {
    let area_mm2: f64 = faces.iter().map(|f| f.area_mm2).sum();
    let (mut c, mut min, mut max) = ([0.0; 3], [f64::MAX; 3], [f64::MIN; 3]);
    for f in &faces {
        let m = centroid(&f.region);
        let w = f.area_mm2 / area_mm2.max(1e-12);
        c[0] += w * (m[0] + at[0]);
        c[1] += w * (m[1] + at[1]);
        c[2] += w * f.z;
        let b = f.region.bbox();
        for (k, (lo, hi)) in [(b.min.x_mm() + at[0], b.max.x_mm() + at[0]), (b.min.y_mm() + at[1], b.max.y_mm() + at[1]), (f.z, f.z)].into_iter().enumerate() {
            min[k] = min[k].min(lo);
            max[k] = max[k].max(hi);
        }
    }
    let worst = faces.iter().map(|f| f.degree).max().unwrap_or(1);
    // On a face: its sample nearest a point.
    let on = |f: &Face, p: [f64; 3]| -> [f64; 3] {
        let far = |q: &[f64; 2]| (q[0] + at[0] - p[0]).powi(2) + (q[1] + at[1] - p[1]).powi(2);
        let q = region_samples(&f.region, spacing).into_iter().min_by(|a, b| far(a).total_cmp(&far(b))).unwrap_or([p[0] - at[0], p[1] - at[1]]);
        [q[0] + at[0], q[1] + at[1], f.z]
    };
    let low = faces.iter().min_by(|a, b| a.z.total_cmp(&b.z).then(b.area_mm2.total_cmp(&a.area_mm2))).expect("a patch has faces");
    let lowest = on(low, [centroid(&low.region)[0] + at[0], centroid(&low.region)[1] + at[1], low.z]);
    let near = faces.iter().min_by(|a, b| (a.z - c[2]).abs().total_cmp(&(b.z - c[2]).abs()).then(b.area_mm2.total_cmp(&a.area_mm2))).expect("a patch has faces");
    let mark = on(near, c);
    let (low, mark_layer) = (low.layer, near.layer);
    LayerPatch { faces, area_mm2, centroid: c, lowest, low, mark, mark_layer, min, max, worst }
}

/// The top of the highest layer under `p` (plate mm) whose outline holds it, below `layer`.
pub fn below(part: &PartLayers, at: [f64; 2], layer: usize, p: [f64; 3]) -> Option<f64> {
    let q = Pt::from_mm(p[0] - at[0], p[1] - at[1]);
    (0..layer.min(part.layers.len())).rev().find(|&j| part.layers[j].2.iter().any(|e| {
        let b = e.bbox();
        q.x >= b.min.x && q.x <= b.max.x && q.y >= b.min.y && q.y <= b.max.y && e.contains(q)
    })).map(|j| part.layers[j].0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_slicer::geom::Polygon;

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon { pts: vec![Pt::from_mm(x0, y0), Pt::from_mm(x1, y0), Pt::from_mm(x1, y1), Pt::from_mm(x0, y1)] })
    }

    /// Layers 0.2 mm: `n` of them, each `of(k)`.
    fn stack(n: usize, of: impl Fn(usize) -> Vec<ExPolygon>) -> PartLayers {
        PartLayers::new((0..n).map(|k| (0.2 * (k + 1) as f64, 0.2, of(k))).collect())
    }

    /// An arch: two legs 5 mm wide to z 10, a 24 mm roof on them: its underside, 14 x 12 mm,
    /// faces the bed from z 10, one flat patch over the bed.
    #[test]
    fn an_arch_has_one_flat_underside() {
        let part = stack(70, |k| if k < 50 { vec![square(0.0, 0.0, 5.0, 12.0), square(19.0, 0.0, 24.0, 12.0)] } else { vec![square(0.0, 0.0, 24.0, 12.0)] });
        let p = patches(&part, [100.0, 100.0], 45.0, 1.0);
        assert_eq!(p.len(), 1);
        assert!((p[0].area_mm2 - 168.0).abs() < 0.5, "{}", p[0].area_mm2);
        assert_eq!(p[0].worst, 5);
        assert!((p[0].lowest[2] - 10.0).abs() < 1e-9 && (p[0].centroid[0] - 112.0).abs() < 0.1, "{:?} {:?}", p[0].lowest, p[0].centroid);
        assert_eq!(below(&part, [100.0, 100.0], p[0].faces[0].layer, p[0].lowest), None, "nothing of the part under it");
    }

    /// A wall leaning out 30 degrees from vertical stands on itself at a 45 degree limit; one
    /// leaning 60 degrees is one patch, a band a layer, as wide as it is long on every one.
    #[test]
    fn a_slope_overhangs_past_the_limit_as_one_patch() {
        let lean = |deg: f64| stack(25, move |k| vec![square(0.0, 0.0, 10.0 + 0.2 * k as f64 * deg.to_radians().tan(), 10.0)]);
        assert!(patches(&lean(30.0), [0.0; 2], 45.0, 1.0).is_empty());
        let p = patches(&lean(60.0), [0.0; 2], 45.0, 1.0);
        assert_eq!(p.len(), 1, "one surface");
        let wide = 24.0 * 0.2 * 60f64.to_radians().tan();
        assert!((p[0].area_mm2 - wide * 10.0).abs() < 0.5, "{} against {}", p[0].area_mm2, wide * 10.0);
        assert!(p[0].worst >= 1 && p[0].worst < 5, "{}", p[0].worst);
    }

    /// A ledge inside a cup is over the cup's floor.
    #[test]
    fn a_ledge_over_a_floor_finds_the_floor_under_it() {
        // a floor to z 1, walls 3 mm thick to z 4, then a lid with a hole 4 mm square: the lid's
        // underside over the hollow is the ledge
        let wall = || difference_ex(&[square(0.0, 0.0, 20.0, 20.0)], &[square(3.0, 3.0, 17.0, 17.0)]);
        let lid = || difference_ex(&[square(0.0, 0.0, 20.0, 20.0)], &[square(8.0, 8.0, 12.0, 12.0)]);
        let part = stack(30, |k| if k < 5 { vec![square(0.0, 0.0, 20.0, 20.0)] } else if k < 20 { wall() } else { lid() });
        let p = patches(&part, [0.0; 2], 45.0, 1.0);
        assert_eq!(p.len(), 1);
        let face = &p[0].faces[0];
        assert_eq!(below(&part, [0.0; 2], face.layer, p[0].lowest), Some(1.0), "the floor's top");
    }
}

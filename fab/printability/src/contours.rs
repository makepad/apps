//! Live checks on a part's layer contours alone: what can be said while the part is still
//! being moved, before walls, infill or supports exist. Per layer in parallel, then one
//! linear pass that merges consecutive layers into one issue per run.
//!
//! - `mid_air`: an island with nothing of the part below it (it needs support).
//! - `weak_joint`: an island that rests on the layer below by too little area for its size
//!   (a bridge-like or point contact: poor connectivity, it breaks or wobbles).
//! - `thin_neck`: a layer whose whole cross-section is small against what stands on it
//!   within the next few millimetres (a stem that snaps or sways).
//! - `plate_contact`: the first layer touches the plate by too little for the part's height.
//!
//! Positions are in the contours' frame; the caller moves them with the part.

use crate::{Footprint, Issue, Severity};
use makepad_fab_slicer::clip::intersection_ex;
use makepad_fab_slicer::geom::{total_area_mm2, ExPolygon};
use makepad_fab_toolpath::par;

/// Below this share of its own area, an island's footing is a weak joint.
const JOINT_SHARE: f64 = 0.08;
/// Islands smaller than this (mm²) are not judged for joints.
const JOINT_MIN_AREA: f64 = 4.0;
/// A neck: the cross-section under this share of the largest one standing on it...
const NECK_SHARE: f64 = 0.12;
/// ...within this height above it (mm)...
const NECK_REACH: f64 = 8.0;
/// ...and under this area (mm²).
const NECK_MAX_AREA: f64 = 40.0;

struct LayerFacts {
    area: f64,
    /// (island area, footing area, centroid) of islands with a weak or no footing.
    loose: Vec<(f64, f64, [f64; 2], Footprint)>,
}

fn centroid(e: &ExPolygon) -> [f64; 2] {
    let (mut sum, mut x, mut y) = (0.0, 0.0, 0.0);
    for ring in e.rings() {
        for i in 0..ring.pts.len() {
            let (a, b) = (ring.pts[i], ring.pts[(i + 1) % ring.pts.len()]);
            let c = a.x_mm() * b.y_mm() - b.x_mm() * a.y_mm();
            sum += c;
            x += (a.x_mm() + b.x_mm()) * c;
            y += (a.y_mm() + b.y_mm()) * c;
        }
    }
    if sum.abs() > 1e-12 {
        [x / (3.0 * sum), y / (3.0 * sum)]
    } else {
        e.contour.pts.first().map_or([0.0; 2], |p| [p.x_mm(), p.y_mm()])
    }
}

/// A spiral vase climbs one outline: every layer above the solid bottom must
/// be a single region without holes. Runs of layers that are not are one issue.
pub fn vase_checks<R: AsRef<[ExPolygon]>>(object: &str, layers: &[(f64, f64, R)], bottom_layers: usize) -> Vec<Issue> {
    let mut out: Vec<Issue> = Vec::new();
    for (i, (z, _, regions)) in layers.iter().enumerate().skip(bottom_layers.max(1)) {
        let real: Vec<&ExPolygon> = regions.as_ref().iter().filter(|e| e.area_mm2() >= 0.05).collect();
        let ok = real.len() == 1 && real[0].holes.is_empty();
        if ok || real.is_empty() {
            continue;
        }
        if let Some(last) = out.last_mut() {
            if last.layer_to == Some(i as u32) {
                last.layer_to = Some(i as u32 + 1);
                last.highest_z = *z;
                continue;
            }
        }
        let at = centroid(real[0]);
        let what = if real.len() > 1 { format!("{} separate outlines", real.len()) } else { "an outline with a hole".to_string() };
        let mut issue = Issue::new("vase_outline", Severity::Warning, Some(object), [at[0], at[1], *z], Footprint::of_regions(regions.as_ref()), real.len() as f64,
            format!("Spiral vase: {what} on a layer; the vase climbs only one"), "Those layers print flat with a seam; make the part one closed outline per layer, or turn spiral vase off.");
        issue.layer = Some(i as u32 + 1);
        issue.layer_to = Some(i as u32 + 1);
        out.push(issue);
    }
    out
}

/// The contour checks of one part. `layers` are (print z, height, regions), bottom up;
/// layer numbers in the issues are 1-based like the slice's. `stop` is asked before each layer:
/// once it is true the checks end with None, telling nothing of what they found.
pub fn contour_checks<R: AsRef<[ExPolygon]> + Sync>(object: &str, layers: &[(f64, f64, R)], stop: &(dyn Fn() -> bool + Sync)) -> Option<Vec<Issue>> {
    let n = layers.len();
    let facts: Vec<LayerFacts> = par::try_map(par::Ctl { cancel: Some(stop), ..Default::default() }, n, |i| {
        let regions = layers[i].2.as_ref();
        let area = total_area_mm2(regions);
        let mut loose = Vec::new();
        if i > 0 {
            let below = layers[i - 1].2.as_ref();
            for e in regions {
                let a = e.area_mm2();
                if a < 0.05 {
                    continue;
                }
                let b = e.bbox();
                let near: Vec<ExPolygon> = below.iter().filter(|x| x.bbox().overlaps(&b)).cloned().collect();
                let footing = if near.is_empty() { 0.0 } else { total_area_mm2(&intersection_ex(std::slice::from_ref(e), &near)) };
                if footing <= 1e-6 || (a >= JOINT_MIN_AREA && footing < JOINT_SHARE * a) {
                    loose.push((a, footing, centroid(e), Footprint::of_box(b)));
                }
            }
        }
        LayerFacts { area, loose }
    })
    .ok()?;
    let mut out: Vec<Issue> = Vec::new();
    // Runs: the issue index per kind while consecutive layers continue it.
    let mut runs: [Option<usize>; 3] = [None; 3];
    let extend = |out: &mut Vec<Issue>, k: usize, mut issue: Issue, layer: usize, z: f64, runs: &mut [Option<usize>; 3]| {
        if let Some(ix) = runs[k] {
            let it = &mut out[ix];
            if it.layer_to.map_or(false, |t| t as usize + 1 == layer + 1) {
                it.layer_to = Some(layer as u32 + 1);
                it.highest_z = z;
                it.measure = it.measure.min(issue.measure);
                it.area_mm2 = it.area_mm2.max(issue.area_mm2);
                return;
            }
        }
        issue.layer = Some(layer as u32 + 1);
        issue.layer_to = Some(layer as u32 + 1);
        runs[k] = Some(out.len());
        out.push(issue);
    };
    // The plate.
    if let (Some(first), Some(top)) = (facts.first(), layers.last()) {
        let height = top.0;
        let biggest = facts.iter().map(|f| f.area).fold(0.0, f64::max);
        if height > 10.0 && first.area > 0.0 && (first.area < 20.0 || first.area < 0.04 * biggest) {
            let at = layers[0].2.as_ref().iter().max_by(|a, b| a.area_mm2().total_cmp(&b.area_mm2())).map(centroid).unwrap_or([0.0; 2]);
            let mut issue = Issue::new("plate_contact", Severity::Warning, Some(object), [at[0], at[1], layers[0].0], Footprint::of_regions(layers[0].2.as_ref()), first.area,
                format!("{:.1} mm² on the plate holds a part {height:.0} mm tall", first.area), "Lay the part on a larger face, add a brim, or add support under it.");
            issue.layer = Some(1);
            issue.layer_to = Some(1);
            issue.area_mm2 = first.area;
            out.push(issue);
        }
    }
    for i in 1..n {
        let z = layers[i].0;
        let mut weak_here = false;
        let mut air_here = false;
        for &(a, footing, c, footprint) in &facts[i].loose {
            if footing <= 1e-6 {
                let mut issue = Issue::new("mid_air", Severity::Error, Some(object), [c[0], c[1], z], footprint, a,
                    format!("{a:.1} mm² island starts in mid-air"), "Add support beneath it, or turn the part so it grows from below.");
                issue.area_mm2 = a;
                extend(&mut out, 0, issue, i, z, &mut runs);
                air_here = true;
            } else {
                let mut issue = Issue::new("weak_joint", Severity::Warning, Some(object), [c[0], c[1], z], footprint, footing,
                    format!("a {a:.1} mm² island rests on {footing:.2} mm² of the layer below"), "Thicken the joint, add a fillet, or turn the part so the island grows from a wider base.");
                issue.area_mm2 = a;
                extend(&mut out, 1, issue, i, z, &mut runs);
                weak_here = true;
            }
        }
        if !air_here {
            runs[0] = None;
        }
        if !weak_here {
            runs[1] = None;
        }
        // A neck: small against what stands on it within reach.
        let a = facts[i].area;
        let mut above = 0.0f64;
        for j in i + 1..n {
            if layers[j].0 - z > NECK_REACH {
                break;
            }
            above = above.max(facts[j].area);
        }
        if a > 0.0 && a < NECK_MAX_AREA && a < NECK_SHARE * above {
            let at = layers[i].2.as_ref().iter().max_by(|x, y| x.area_mm2().total_cmp(&y.area_mm2())).map(centroid).unwrap_or([0.0; 2]);
            let mut issue = Issue::new("thin_neck", Severity::Warning, Some(object), [at[0], at[1], z], Footprint::of_regions(layers[i].2.as_ref()), a,
                format!("{a:.1} mm² carries {above:.0} mm² standing on it"), "Thicken the neck, or turn the part so the narrow part prints last.");
            issue.area_mm2 = above;
            extend(&mut out, 2, issue, i, z, &mut runs);
        } else {
            runs[2] = None;
        }
    }
    for it in &mut out {
        if let (Some(a), Some(b)) = (it.layer, it.layer_to) {
            if b > a {
                it.message = format!("Layers {a}–{b}: {}", it.message);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_slicer::geom::{Polygon, Pt};

    fn square(x: f64, y: f64, s: f64) -> ExPolygon {
        ExPolygon::new(Polygon::new(vec![Pt::from_mm(x, y), Pt::from_mm(x + s, y), Pt::from_mm(x + s, y + s), Pt::from_mm(x, y + s)]))
    }

    fn stack(f: impl Fn(usize) -> Vec<ExPolygon>, n: usize) -> Vec<(f64, f64, Vec<ExPolygon>)> {
        (0..n).map(|i| (0.2 * (i + 1) as f64, 0.2, f(i))).collect()
    }

    #[test]
    fn a_plain_block_is_clean() {
        assert!(contour_checks("block", &stack(|_| vec![square(0.0, 0.0, 20.0)], 100), &|| false).expect("not stopped").is_empty());
    }

    #[test]
    fn a_floating_island_a_neck_and_a_point_contact() {
        // A 20 mm block on a 1.5 mm stem (neck, small plate contact), with an island that
        // appears out of nothing at layer 60, and one that rests on a corner at layer 80.
        let layers = stack(
            |i| {
                let mut v = vec![if i < 30 { square(9.25, 9.25, 1.5) } else { square(0.0, 0.0, 20.0) }];
                if (60..70).contains(&i) {
                    v.push(square(40.0, 0.0, 5.0));
                }
                if i == 79 {
                    v.push(square(19.9, 19.9, 0.1));
                }
                if (80..85).contains(&i) {
                    v.push(square(19.95, 19.95, 6.0));
                }
                v
            },
            100,
        );
        let issues = contour_checks("t", &layers, &|| false).expect("not stopped");
        let kinds: Vec<&str> = issues.iter().map(|i| i.kind).collect();
        assert!(kinds.contains(&"plate_contact"), "{kinds:?}");
        assert!(kinds.contains(&"thin_neck"), "{kinds:?}");
        let air = issues.iter().find(|i| i.kind == "mid_air").expect("mid-air island");
        assert_eq!(air.layer, Some(61));
        assert_eq!(air.footprint, Footprint { min: [40.0, 0.0], max: [45.0, 5.0] });
        assert!(issues.iter().any(|i| i.kind == "weak_joint"), "{kinds:?}");
    }
}

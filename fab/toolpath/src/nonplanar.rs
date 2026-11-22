//! Non-planar top finish (NONPLANAR-DESIGN §3): the job's settings for the kernel
//! (`makepad_fab_slicer::nonplanar`) and a planned region's rings as 3D extrusion paths for
//! the writer's block after the region's schedule layer.

use crate::{ExtrusionPath, Role};
use makepad_fab_settings::machine::Job;
use makepad_fab_slicer::geom::Pt;
use makepad_fab_slicer::nonplanar::{NpPlan, NpRegion, NpSettings};

/// The experimental mode of these paths (`makepad_fab_gcode::record::modes::NONPLANAR`).
pub const MODE: u8 = crate::exp::mode::NONPLANAR;

/// The kernel's settings from the job (process keys `fab_nonplanar_shells`, `_slope_cap`,
/// `_rise`, `_tolerance`; unset keys keep the v1 defaults: 2 shells, 15 deg, 3 mm rise,
/// 0.005 mm tolerance).
pub fn settings(job: &Job, layer_h: f64, line_w: f64) -> NpSettings {
    let d = NpSettings::default();
    let pos = |k: &str, dflt: f64| { let v = job.f(k); if v > 0.0 { v } else { dflt } };
    NpSettings {
        shells: pos("fab_nonplanar_shells", d.shells as f64).round() as usize,
        layer_h,
        line_w,
        slope_cap_deg: pos("fab_nonplanar_slope_cap", d.slope_cap_deg),
        rise_cap: pos("fab_nonplanar_rise", d.rise_cap),
        tol: pos("fab_nonplanar_tolerance", d.tol),
        ..d
    }
}

/// The highest planar layer top at or under `cap` (the layer tops in print order).
fn layer_under(tops: &[f64], cap: f64) -> Option<f64> {
    let i = tops.partition_point(|&t| t <= cap + 1e-9);
    (i > 0).then(|| tops[i - 1])
}

/// A region's rings as extrusion paths, in print order (lowest shell first), with Z offsets
/// from `base_z` (the schedule layer's print z) and per-segment flow.
///
/// The flow follows the bead's thickness over what lies under it: the lowest shell (and the
/// top shell where no lower bead lies) sits on the planar body, whose top is the last layer
/// top at or under the column's cap; a shell above another one is a layer height thick.
pub fn block_paths(plan: &NpPlan, region: &NpRegion, base_z: f64, layer_tops: &[f64], speed: f64) -> Vec<ExtrusionPath> {
    let s = &plan.settings;
    let h = s.layer_h;
    let area = crate::flow::cross_section(s.line_w, h);
    let ns = region.shells.len();
    let mut out = Vec::new();
    for (sh, rings) in region.shells.iter().enumerate() {
        let top = sh + 1 == ns;
        for ring in rings {
            if ring.len() < 2 { continue; }
            let pts: Vec<Pt> = ring.iter().map(|p| Pt::from_mm(p[0], p[1])).collect();
            let z: Vec<f64> = ring.iter().map(|p| p[2] - base_z).collect();
            let flow: Vec<f64> = ring.windows(2).map(|w| {
                let (x, y, zm) = (0.5 * (w[0][0] + w[1][0]), 0.5 * (w[0][1] + w[1][1]), 0.5 * (w[0][2] + w[1][2]));
                // On the planar body: the lowest shell everywhere, the top shell where it is the only one.
                let on_body = sh == 0 || (top && plan.cap_at(x, y).is_some_and(|c| zm - c < h + s.t_min - 1e-6));
                let t = if on_body { plan.cap_at(x, y).and_then(|c| layer_under(layer_tops, c)).map_or(h, |u| zm - u) } else { h };
                (t / h).clamp(0.3, 2.0)
            }).collect();
            out.push(ExtrusionPath {
                role: if top { Role::TopSurface } else { Role::InternalSolidInfill },
                pts,
                closed: false,
                loop_pts: Vec::new(),
                width: s.line_w,
                height: h,
                area,
                speed,
                dry: Vec::new(),
                z,
                flow,
                mode: MODE, core: false,
                overhang: 0.0,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_slicer::nonplanar::{plan, NpBox};

    fn low_dome(x: f64, y: f64, z: f64) -> f64 {
        let (x, y) = (x - 100.0, y - 100.0);
        let cyl = (x * x + y * y).sqrt() - 22.0;
        let sph = (x * x + y * y + (z - 13.1 + 80.0).powi(2)).sqrt() - 80.0;
        cyl.max(-z).max(sph)
    }

    fn head() -> Vec<NpBox> {
        let b = |name: &str, hx: f64, hole: f64, e: f64, margin: f64| NpBox { name: name.into(), ox: 0.0, oy: 0.0, hx, hy: hx, hole, e, margin };
        vec![b("cone 1", 1.4, 1.1, 0.69, 0.15), b("cone 2", 1.8, 1.4, 1.05, 0.2), b("block", 12.0, 0.0, 4.0, 1.0)]
    }

    #[test]
    fn a_region_becomes_3d_paths_with_thickness_flow() {
        let s = NpSettings::default();
        let p = plan(&low_dome, ([78.0, 78.0, 0.0], [122.0, 122.0, 13.1]), &s, &head()).unwrap();
        let r = &p.regions[0];
        // Planar layers every 0.2 mm from a 0.2 mm first layer; the block follows the layer at its top.
        let tops: Vec<f64> = (1..=70).map(|i| i as f64 * 0.2).collect();
        let base = layer_under(&tops, r.schedule_z).unwrap();
        let paths = block_paths(&p, r, base, &tops, 60.0);
        assert!(!paths.is_empty());
        let n_top = paths.iter().filter(|q| q.role == Role::TopSurface).count();
        assert_eq!(n_top, r.shells.last().unwrap().len());
        for q in &paths {
            assert_eq!((q.mode, q.z.len(), q.flow.len()), (MODE, q.pts.len(), q.pts.len() - 1));
            assert!(q.flow.iter().all(|f| (0.3..=2.0).contains(f)));
            // Every point of the block is at most the region's top above the base layer.
            assert!(q.z.iter().all(|dz| *dz <= r.schedule_z - base + 1e-6));
        }
        // The lowest shell's beads are thicker than a layer somewhere (they fill the stair-step).
        let lower = paths.iter().filter(|q| q.role == Role::InternalSolidInfill).flat_map(|q| q.flow.iter()).fold(0f64, |a, b| a.max(*b));
        assert!(lower > 1.0 && lower <= 2.0, "lower shell flow max {lower}");
    }
}

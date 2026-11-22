//! Z ramps (EXPSLICE §2.1–2.3): the spiral vase and scarf seams. Both are
//! sequential post-passes over a finished plan. Each ramp needs the
//! previous layer's end, so they run after the per-layer passes and never
//! enter the plan memo: a replan recomputes them all, which is cheap.
//!
//! A ramp is an `ExtrusionPath` with `z` offsets from the layer's print z,
//! always in [−h, 0], never falling within a path. Its `flow` is the local
//! bead thickness as a share of h, so a ramp that starts on a flat layer
//! lays a wedge. The writer turns it into G1 X Y Z E moves.
//! The validator's monotone-ramp rule checks it: no Z drop, the head at the
//! top of the print, a shallow pitch and the Z speed limit.

use crate::exp::{Scarf, Vase, VaseVariant};
use crate::{ExtrusionPath, Island, ObjectLayer, Params, Role};
use makepad_fab_slicer::geom::*;
use std::f64::consts::TAU;
use std::sync::Arc;

/// The whole closed ring of a wall loop, first point repeated at the end.
fn ring_of(path: &ExtrusionPath) -> Option<Vec<Pt>> {
    if !path.closed {
        return None;
    }
    let mut r = if path.loop_pts.len() > 3 { path.loop_pts.clone() } else { path.pts.clone() };
    if r.len() < 4 {
        return None;
    }
    if r.first() != r.last() {
        let f = r[0];
        r.push(f);
    }
    Some(r)
}

/// The closed ring started at its vertex nearest `near`.
fn rotate_to(ring: &[Pt], near: Pt) -> Vec<Pt> {
    let n = ring.len() - 1;
    let k = (0..n).min_by_key(|&i| ring[i].dist2(near)).unwrap_or(0);
    let mut out: Vec<Pt> = (0..n).map(|i| ring[(k + i) % n]).collect();
    out.push(out[0]);
    out
}

/// Cumulative arc length (mm) at each point.
fn arclen(pts: &[Pt]) -> Vec<f64> {
    let mut s = Vec::with_capacity(pts.len());
    let mut acc = 0.0;
    s.push(0.0);
    for w in pts.windows(2) {
        acc += (w[1].x_mm() - w[0].x_mm()).hypot(w[1].y_mm() - w[0].y_mm());
        s.push(acc);
    }
    s
}

/// Split a ring's segments so that none is longer than `step` mm (a ramp's Z
/// resolution: each segment climbs at most h·step/L).
fn densify(pts: &[Pt], step: f64) -> Vec<Pt> {
    let mut out = Vec::with_capacity(pts.len() * 2);
    for w in pts.windows(2) {
        let (ax, ay, bx, by) = (w[0].x_mm(), w[0].y_mm(), w[1].x_mm(), w[1].y_mm());
        let k = ((bx - ax).hypot(by - ay) / step).ceil().max(1.0) as usize;
        for j in 0..k {
            let t = j as f64 / k as f64;
            out.push(Pt::from_mm(ax + (bx - ax) * t, ay + (by - ay) * t));
        }
    }
    if let Some(p) = pts.last() {
        out.push(*p);
    }
    out.dedup();
    out
}

fn wall_path(template: &ExtrusionPath, role: Role, pts: Vec<Pt>, z: Vec<f64>, flow: Vec<f64>) -> ExtrusionPath {
    ExtrusionPath { role, loop_pts: Vec::new(), closed: false, pts, z, flow, ..template.clone() }
}

/// The walls of a layer a vase can climb: one island whose walls are one
/// closed outer loop (and, double-walled, one closed inner loop).
fn vase_walls(ol: &ObjectLayer, double: bool) -> Option<(ExtrusionPath, Option<ExtrusionPath>)> {
    let with_walls: Vec<&Island> = ol.islands.iter().map(AsRef::as_ref).filter(|i| !i.walls.is_empty()).collect();
    if with_walls.len() != 1 {
        return None;
    }
    let walls = &with_walls[0].walls;
    let outer: Vec<&ExtrusionPath> = walls.iter().filter(|w| w.role == Role::OuterWall).collect();
    let inner: Vec<&ExtrusionPath> = walls.iter().filter(|w| w.role == Role::InnerWall).collect();
    if outer.len() != 1 || !outer[0].closed || walls.iter().any(|w| !w.closed) {
        return None;
    }
    if double {
        if inner.len() != 1 {
            return None;
        }
        return Some((outer[0].clone(), Some(inner[0].clone())));
    }
    if !inner.is_empty() {
        return None;
    }
    Some((outer[0].clone(), None))
}

/// The layers that will climb, for live printability: a vase needs one
/// outline per layer above its bottom.
pub fn vase_breaks(layers: &[(f64, f64, ObjectLayer)], p: &Params) -> Vec<usize> {
    let Some(v) = &p.exp.vase else { return Vec::new() };
    let first = p.bottom_layers.max(1);
    (first..layers.len()).filter(|&li| vase_walls(&layers[li].2, v.variant == VaseVariant::DoubleWall).is_none() && layers[li].2.has_paths()).collect()
}

/// Turn the walls above the solid bottom into one continuous climb.
pub fn vase(layers: &mut [(f64, f64, ObjectLayer)], p: &Params, v: &Vase, center: Pt) {
    let n = layers.len();
    let first = p.bottom_layers.max(1);
    let double = v.variant == VaseVariant::DoubleWall;
    let mut prev_end: Option<Pt> = None;
    // Rib count, fixed for the whole part from its first climbing layer.
    let mut ribs: Option<f64> = None;
    let (cx, cy) = (center.x_mm(), center.y_mm());
    for li in first..n {
        let h = layers[li].1;
        let Some((outer, inner)) = vase_walls(&layers[li].2, double) else {
            // A layer the vase cannot climb prints as it is; the next climb ramps in again.
            prev_end = None;
            continue;
        };
        let ramp_in = prev_end.is_none();
        let last = li + 1 == n;
        let start = prev_end.unwrap_or_else(|| outer.first());
        let ring_o = densify(&rotate_to(&ring_of(&outer).unwrap(), start), 1.0);
        let mut loops: Vec<(ExtrusionPath, Vec<Pt>, f64, f64)> = Vec::new(); // (template, ring, z0 share, z1 share)
        match &inner {
            None => loops.push((outer.clone(), ring_o, 0.0, 1.0)),
            Some(inn) => {
                let end_o = *ring_o.last().unwrap();
                let ring_i = densify(&rotate_to(&ring_of(inn).unwrap(), end_o), 1.0);
                loops.push((outer.clone(), ring_o, 0.0, 0.5));
                loops.push((inn.clone(), ring_i, 0.5, 1.0));
            }
        }
        let mut paths = Vec::with_capacity(loops.len());
        for (k, (tpl, ring, a, b)) in loops.into_iter().enumerate() {
            let s = arclen(&ring);
            let len = s.last().copied().unwrap_or(0.0).max(1e-9);
            let w0 = tpl.width.max(0.1);
            let mut pts = ring.clone();
            // Variable width: ribs swell the bead inward; the outer surface stays put.
            let width_at = |q: Pt, r: &mut Option<f64>| -> f64 {
                if v.variant != VaseVariant::VariableWidth || k > 0 {
                    return w0;
                }
                let rr = *r.get_or_insert_with(|| {
                    let mean = ring.iter().map(|p| (p.x_mm() - cx).hypot(p.y_mm() - cy)).sum::<f64>() / ring.len() as f64;
                    (TAU * mean / v.rib_spacing).round().max(3.0)
                });
                let th = (q.y_mm() - cy).atan2(q.x_mm() - cx);
                let c = 0.5 * (1.0 + (rr * th).cos());
                w0 + (v.width_max.max(w0) - w0) * c * c
            };
            let widths: Vec<f64> = ring.iter().map(|q| width_at(*q, &mut ribs)).collect();
            if v.variant == VaseVariant::VariableWidth && k == 0 {
                // Shift each point inward (left of travel on a counter-clockwise outline) by half the extra width.
                let m = ring.len();
                for i in 0..m {
                    let (pa, pb) = (ring[if i == 0 { m - 2 } else { i - 1 }], ring[if i + 1 == m { 1 } else { i + 1 }]);
                    let (dx, dy) = (pb.x_mm() - pa.x_mm(), pb.y_mm() - pa.y_mm());
                    let l = dx.hypot(dy).max(1e-9);
                    let d = 0.5 * (widths[i] - w0);
                    pts[i] = Pt::from_mm(ring[i].x_mm() - dy / l * d, ring[i].y_mm() + dx / l * d);
                }
            }
            let share = |i: usize| a + (b - a) * s[i] / len;
            let mut z: Vec<f64> = (0..pts.len()).map(|i| -h + h * share(i)).collect();
            let mut flow: Vec<f64> = (1..pts.len())
                .map(|i| {
                    let mid = 0.5 * (share(i - 1) + share(i));
                    let wf = 0.5 * (widths[i - 1] + widths[i]) / w0;
                    // Ramping in from the flat bottom: the bead is as thick as the climb so far.
                    (if ramp_in { mid } else { 1.0 }) * wf
                })
                .collect();
            // The top: one more loop at the top height, its flow running out, to end flat.
            if last && k + 1 == if double { 2 } else { 1 } {
                for i in 1..ring.len() {
                    pts.push(pts[i]);
                    z.push(0.0);
                    let mid = 0.5 * (s[i - 1] + s[i]) / len;
                    flow.push(1.0 - mid);
                }
            }
            let mut wp = wall_path(&tpl, tpl.role, pts, z, flow);
            wp.mode = crate::exp::mode::VASE;
            paths.push(wp);
        }
        prev_end = paths.last().map(|p: &ExtrusionPath| p.last());
        let ol = &mut layers[li].2;
        ol.islands = vec![Arc::new(Island { walls: paths, ..Default::default() })];
    }
}

/// A scarf seam on every closed wall loop of one layer: the loop starts on a
/// ramp from `start·h` above the layer below up to the layer top over
/// `length` mm, runs round, and ends by passing over the ramp again with its
/// flow running out, filling the wedge. The seam blends into the wall.
pub fn scarf(ol: &mut ObjectLayer, h: f64, sc: &Scarf) {
    for island in &mut ol.islands {
        let island = Arc::make_mut(island);
        for w in &mut island.walls {
            let wants = w.role == Role::OuterWall || (!sc.outer_only && w.role == Role::InnerWall);
            if !wants || !w.z.is_empty() {
                continue;
            }
            let Some(ring) = ring_of(w) else { continue };
            // A variable-width bead's own flow along the loop.
            let bead_s = arclen(&ring);
            let bead = |t: f64| {
                if w.flow.is_empty() {
                    return 1.0;
                }
                let t = t.rem_euclid(*bead_s.last().unwrap());
                let i = bead_s.partition_point(|&x| x <= t).saturating_sub(1);
                w.flow[i.min(w.flow.len() - 1)]
            };
            let ring = densify(&ring, 0.5);
            let s = arclen(&ring);
            let len = *s.last().unwrap();
            let ls = sc.length.min(0.5 * len);
            if ls < 2.0 {
                continue;
            }
            let dip = h * (1.0 - sc.start);
            // The ring, then on along it for the overlap.
            let mut pts = ring.clone();
            let mut ss = s.clone();
            for i in 1..ring.len() {
                if s[i - 1] >= ls {
                    break;
                }
                pts.push(ring[i]);
                ss.push(len + s[i]);
            }
            let z: Vec<f64> = ss.iter().map(|&t| if t < ls { -dip * (1.0 - t / ls) } else { 0.0 }).collect();
            let flow: Vec<f64> = (1..pts.len())
                .map(|i| {
                    let t = 0.5 * (ss[i - 1] + ss[i]);
                    let ramp = if t < ls {
                        sc.start + (1.0 - sc.start) * t / ls
                    } else if t > len {
                        ((1.0 - sc.start) * (1.0 - (t - len) / ls)).max(0.0)
                    } else {
                        1.0
                    };
                    ramp * bead(t)
                })
                .collect();
            *w = wall_path(w, w.role, pts, z, flow);
            w.mode = crate::exp::mode::SCARF;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn circle(r: f64, n: usize) -> Vec<Pt> {
        let mut v: Vec<Pt> = (0..n).map(|i| { let a = TAU * i as f64 / n as f64; Pt::from_mm(r * a.cos(), r * a.sin()) }).collect();
        v.push(v[0]);
        v
    }

    fn wall(role: Role, r: f64) -> ExtrusionPath {
        let pts = circle(r, 120);
        ExtrusionPath { role, loop_pts: pts.clone(), pts, closed: true, width: 0.45, height: 0.2, area: 0.08, speed: 60.0, ..Default::default() }
    }

    fn layer(walls: Vec<ExtrusionPath>) -> ObjectLayer {
        ObjectLayer { islands: vec![Arc::new(Island { walls, ..Default::default() })], ..Default::default() }
    }

    /// Absolute (x, y, z) of every point of every path, in print order.
    fn points(layers: &[(f64, f64, ObjectLayer)]) -> Vec<(f64, f64, f64)> {
        let mut out = Vec::new();
        for (z, _, ol) in layers {
            for i in &ol.islands {
                for w in &i.walls {
                    for (k, p) in w.pts.iter().enumerate() {
                        out.push((p.x_mm(), p.y_mm(), z + w.z.get(k).copied().unwrap_or(0.0)));
                    }
                }
            }
        }
        out
    }

    fn params(v: VaseVariant) -> Params {
        let mut cfg = makepad_fab_settings::config::Config::new();
        for key in ["sparse_infill_pattern", "top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern"] {
            cfg.set_str(key, "zig-zag");
        }
        for (key, value) in [("nozzle_diameter", "0.4"), ("layer_height", "0.2"), ("initial_layer_print_height", "0.2"), ("wall_loops", "2")] {
            cfg.set_str(key, value);
        }
        cfg.set_vec("filament_map", vec!["1".into()]);
        let mut p = Params::from_job(&makepad_fab_settings::machine::Job::new(cfg), 0);
        p.bottom_layers = 3;
        p.exp.vase = Some(Vase { variant: v, width_max: 0.8, rib_spacing: 6.0 });
        p
    }

    fn stack(n: usize, double: bool) -> Vec<(f64, f64, ObjectLayer)> {
        (0..n).map(|i| {
            let mut ws = vec![wall(Role::OuterWall, 20.0)];
            if double {
                ws.insert(0, wall(Role::InnerWall, 19.6));
            }
            (0.2 * (i + 1) as f64, 0.2, layer(ws))
        }).collect()
    }

    #[test]
    fn a_vase_climbs_without_ever_dropping() {
        for v in [VaseVariant::Plain, VaseVariant::DoubleWall, VaseVariant::VariableWidth] {
            let p = params(v);
            let mut ls = stack(30, v == VaseVariant::DoubleWall);
            vase(&mut ls, &p, p.exp.vase.as_ref().unwrap(), Pt::from_mm(0.0, 0.0));
            let pts = points(&ls[3..]);
            let mut max_step = 0.0f64;
            for w in pts.windows(2) {
                assert!(w[1].2 >= w[0].2 - 1e-9, "{v:?}: Z drops {} -> {}", w[0].2, w[1].2);
                max_step = max_step.max((w[1].0 - w[0].0).hypot(w[1].1 - w[0].1));
            }
            // From one revolution into the next is a hop of at most a bead.
            assert!(max_step < 1.1, "{v:?}: a jump of {max_step} mm");
            // It starts on the bottom's top and ends at the part's top.
            assert!((pts[0].2 - 0.6).abs() < 1e-9 && (pts.last().unwrap().2 - 6.0).abs() < 1e-9, "{v:?}: {} .. {}", pts[0].2, pts.last().unwrap().2);
            // Ramp-in: the first climbing loop starts thin.
            let f0 = &ls[3].2.islands[0].walls[0].flow;
            assert!(f0[0] < 0.05 && f0.iter().all(|f| *f >= 0.0));
            // Every climbing layer is G1-only.
            assert!(ls[3..].iter().all(|(_, _, ol)| ol.islands.len() == 1 && ol.islands[0].fills.is_empty()));
        }
    }

    #[test]
    fn the_variable_width_vase_keeps_its_outside() {
        let p = params(VaseVariant::VariableWidth);
        let mut ls = stack(10, false);
        vase(&mut ls, &p, p.exp.vase.as_ref().unwrap(), Pt::from_mm(0.0, 0.0));
        let w = &ls[5].2.islands[0].walls[0];
        let fmax = w.flow.iter().cloned().fold(0.0, f64::max);
        assert!(fmax > 1.6 && fmax < 1.9, "max flow {fmax}");
        // Outer edge = centreline radius + half the local width: constant.
        let edges: Vec<f64> = (1..w.pts.len()).map(|i| {
            let r = 0.5 * (w.pts[i].x_mm().hypot(w.pts[i].y_mm()) + w.pts[i - 1].x_mm().hypot(w.pts[i - 1].y_mm()));
            r + 0.5 * 0.45 * w.flow[i - 1]
        }).collect();
        let (lo, hi) = edges.iter().fold((f64::MAX, 0.0f64), |(a, b), e| (a.min(*e), b.max(*e)));
        assert!(hi - lo < 0.03, "outer edge varies {lo}..{hi}");
    }

    #[test]
    fn a_scarf_rises_then_overlaps_and_keeps_its_flow() {
        let sc = Scarf { outer_only: true, length: 10.0, start: 0.1 };
        let mut ol = layer(vec![wall(Role::InnerWall, 19.6), wall(Role::OuterWall, 20.0)]);
        scarf(&mut ol, 0.2, &sc);
        let inner = &ol.islands[0].walls[0];
        assert!(inner.z.is_empty(), "inner walls keep their seam with outer_only");
        let w = &ol.islands[0].walls[1];
        assert!((w.z[0] + 0.18).abs() < 1e-9 && w.z.windows(2).all(|p| p[1] >= p[0] - 1e-12) && *w.z.last().unwrap() == 0.0);
        // Total material = one full loop's worth.
        let s = arclen(&w.pts);
        let e: f64 = (1..w.pts.len()).map(|i| (s[i] - s[i - 1]) * w.flow[i - 1]).sum();
        let loop_len = TAU * 20.0;
        assert!((e - loop_len).abs() / loop_len < 0.01, "material {e} vs {loop_len}");
        // Pitch stays shallow.
        let pitch = (1..w.pts.len()).map(|i| (w.z[i] - w.z[i - 1]).abs() / (s[i] - s[i - 1]).max(1e-9)).fold(0.0, f64::max);
        assert!(pitch.atan().to_degrees() < 5.0, "pitch {}", pitch.atan().to_degrees());
    }
}

//! Material interlocking (EXPSLICE §2.8): where two filaments of one painted
//! part meet, a band along the boundary changes hands in alternating beams.
//! Each material reaches into the other in fingers, and the beam direction
//! turns every few layers, so the two lock together mechanically (PLA/TPU,
//! PLA/PETG and other pairs that barely bond).
//!
//! A pure function of one layer's paint split, its index and the settings:
//! the paint pass stays parallel per layer.

use crate::exp::Interlock;
use makepad_fab_slicer::clip::{difference_ex, intersection_ex, union_ex};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::{offset_ex, DEFAULT_MITER};
use makepad_fab_slicer::paint::LayerPaint;

/// Every other beam-wide stripe at `angle` over `bbox` (the "odd" stripes).
fn stripes(b: &BBox, angle: f64, width: f64) -> Vec<ExPolygon> {
    let (cx, cy) = (unscaled(b.min.x + b.max.x) * 0.5, unscaled(b.min.y + b.max.y) * 0.5);
    let r = 0.5 * unscaled(b.max.x - b.min.x).hypot(unscaled(b.max.y - b.min.y)) + width;
    let (c, s) = (angle.cos(), angle.sin());
    // Stripes are anchored to the plate frame (multiples of the width from 0), so layers line up.
    let along = cx * c + cy * s;
    let k0 = ((along - r) / width).floor() as i64;
    let k1 = ((along + r) / width).ceil() as i64;
    let across = -cx * s + cy * c;
    let mut out = Vec::new();
    for k in k0..k1 {
        if k.rem_euclid(2) != 1 {
            continue;
        }
        let (u0, u1) = (k as f64 * width, (k + 1) as f64 * width);
        let (v0, v1) = (across - r, across + r);
        let pt = |u: f64, v: f64| Pt::from_mm(u * c - v * s, u * s + v * c);
        out.push(ExPolygon::new(Polygon::new(vec![pt(u0, v0), pt(u1, v0), pt(u1, v1), pt(u0, v1)])));
    }
    out
}

/// Interlock the boundaries between the states of one layer.
pub fn apply(layer: &mut LayerPaint, li: usize, il: &Interlock) {
    if layer.len() < 2 {
        return;
    }
    let band = il.beam_width * il.depth as f64;
    let group = il.layers_per_direction.max(1);
    let angle = il.orientation + if (li / group) % 2 == 1 { std::f64::consts::FRAC_PI_2 } else { 0.0 };
    let min_area = 0.25 * il.beam_width * il.beam_width;
    // Pairwise, in state order (state 0 first): A's band toward B, B's toward A.
    for a in 0..layer.len() {
        for b in a + 1..layer.len() {
            let (ra, rb) = (layer[a].1.clone(), layer[b].1.clone());
            if ra.is_empty() || rb.is_empty() {
                continue;
            }
            // Where the two meet: each side's strip within `band` of the other.
            let near_b = offset_ex(&rb, band * SCALE, DEFAULT_MITER);
            let near_a = offset_ex(&ra, band * SCALE, DEFAULT_MITER);
            let side_a = intersection_ex(&ra, &near_b);
            let side_b = intersection_ex(&rb, &near_a);
            if side_a.is_empty() || side_b.is_empty() {
                continue;
            }
            let bb = bbox_of(&[side_a.clone(), side_b.clone()].concat());
            let odd = stripes(&bb, angle, il.beam_width);
            // A gives its odd stripes to B; B gives the even ones (its band minus the odd stripes) to A.
            let a_to_b: Vec<ExPolygon> = intersection_ex(&side_a, &odd).into_iter().filter(|e| e.area_mm2() >= min_area).collect();
            let b_to_a: Vec<ExPolygon> = difference_ex(&side_b, &odd).into_iter().filter(|e| e.area_mm2() >= min_area).collect();
            if a_to_b.is_empty() && b_to_a.is_empty() {
                continue;
            }
            let new_a = union_ex(&[difference_ex(&ra, &a_to_b), b_to_a.clone()].concat());
            let new_b = union_ex(&[difference_ex(&rb, &b_to_a), a_to_b].concat());
            layer[a].1 = new_a;
            layer[b].1 = new_b;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon::new(vec![Pt::from_mm(x0, y0), Pt::from_mm(x1, y0), Pt::from_mm(x1, y1), Pt::from_mm(x0, y1)]))
    }

    #[test]
    fn two_halves_swap_fingers_and_keep_their_total() {
        let il = Interlock { beam_width: 0.8, depth: 2, layers_per_direction: 2, orientation: 0.0 };
        for li in [0usize, 2] {
            let mut layer: LayerPaint = vec![(0, vec![rect(0.0, 0.0, 10.0, 10.0)]), (2, vec![rect(10.0, 0.0, 20.0, 10.0)])];
            apply(&mut layer, li, &il);
            let (a, b) = (total_area_mm2(&layer[0].1), total_area_mm2(&layer[1].1));
            assert!((a + b - 200.0).abs() < 0.05, "area kept: {a} + {b}");
            // Each now reaches across the old boundary.
            let ba = bbox_of(&layer[0].1);
            let bb = bbox_of(&layer[1].1);
            if li == 0 {
                // Beams along x at angle 0 cross the vertical boundary.
                assert!(unscaled(ba.max.x) > 10.5 && unscaled(bb.min.x) < 9.5, "a to {} b from {}", unscaled(ba.max.x), unscaled(bb.min.x));
            }
            // No overlap between the two.
            assert!(intersection_ex(&layer[0].1, &layer[1].1).iter().map(|e| e.area_mm2()).sum::<f64>() < 1e-6);
        }
    }
}

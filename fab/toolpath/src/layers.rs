//! Adaptive layer heights (EXPSLICE §2.5): each layer is as thick as the
//! surface allows. The cusp (stair-step) of a layer of height h on a surface
//! with normal n is about h·|n_z|, so h = cusp / max |n_z| over the layer,
//! clamped to the machine's [min, max] layer height. Vertical walls get the
//! thickest layers, gentle slopes and domes the thinnest. A layer boundary is
//! placed exactly at every flat face, so flat tops never land mid-layer.

use crate::exp::Adaptive;
use makepad_fab_slicer::source::{ContourSource, SlopeProfile};

/// The object's layers (top z, height): adaptive when the mode is on and the
/// source knows its slopes, else uniform. Shared by the planner and the
/// live engine's cut, so both cut the same planes.
pub fn object_heights(src: &dyn ContourSource, p: &crate::Params) -> Vec<(f64, f64)> {
    let (_, hi) = src.bounds();
    let first = first_height(p);
    let mut heights = match src.layer_stack() {
        Some(own) => own.to_vec(),
        None => match p.exp.adaptive.as_ref().and_then(|a| src.slope_profile(0.02).map(|prof| adaptive(hi[2], first, a, &prof))) {
            Some(h) => h,
            None => crate::layer_heights(hi[2], first, p.layer_height),
        },
    };
    if p.raft_layers > 0 {
        for (z, _) in &mut heights { *z += p.raft_lift; }
    }
    heights
}

/// How thick an object's first layer prints: on a raft, a plain layer.
pub fn first_height(p: &crate::Params) -> f64 {
    if p.raft_layers > 0 { p.layer_height } else { p.first_layer_height }
}

/// The thickest layer an object prints (mm): its first, its others, or as thick as its adaptive
/// layers grow, with [`crate::SLACK`] for the rounding of where a stack ends.
pub fn thickest(p: &crate::Params) -> f64 {
    let adaptive = p.exp.adaptive.as_ref().map_or(0.0, |a| a.max);
    first_height(p).max(p.layer_height).max(adaptive) + crate::SLACK
}

/// The layer stack from the first layer up to `height`.
pub fn adaptive(height: f64, first: f64, a: &Adaptive, prof: &SlopeProfile) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    if height <= 0.0 {
        return out;
    }
    let first = first.min(height);
    out.push((first, first));
    let mut z = first;
    let flats: Vec<f64> = prof.flats.iter().copied().filter(|f| *f > z + 1e-6 && *f < height - 1e-6).collect();
    let mut fi = 0;
    while z < height - 1e-6 {
        let mut h = a.max;
        for _ in 0..4 {
            let nz = prof.max_nz(z, z + h);
            h = if nz > 1e-6 { (a.cusp / nz).clamp(a.min, a.max) } else { a.max };
        }
        // A layer may grow by at most half again over the one below: no
        // sudden jumps in flow or cooling.
        let below = out.last().map(|l: &(f64, f64)| l.1).unwrap_or(first);
        h = h.min(below * 1.5).max(a.min);
        // Land exactly on the next flat face.
        while fi < flats.len() && flats[fi] <= z + 1e-6 {
            fi += 1;
        }
        if fi < flats.len() && flats[fi] < z + h + a.min {
            let to_flat = flats[fi] - z;
            if to_flat >= 0.5 * a.min {
                // Split evenly when it is more than one layer away.
                let n = (to_flat / h).ceil().max(1.0);
                h = to_flat / n;
            }
        }
        // The top: never a sliver.
        let left = height - z;
        if left < h + a.min {
            h = if left <= a.max { left } else { 0.5 * left };
        }
        z += h;
        out.push((z, h));
    }
    if let Some(l) = out.last_mut() {
        // The stack ends exactly at the top.
        let d = height - l.0;
        l.0 = height;
        l.1 += d;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere_profile(r: f64, step: f64) -> SlopeProfile {
        let mut p = SlopeProfile::empty(0.0, 2.0 * r, step);
        for k in 0..p.nz.len() {
            let za = k as f64 * step;
            let zb = za + step;
            // |n_z| on a sphere at height z is |z - r| / r; take the band's max.
            let nz = ((za - r).abs().max((zb - r).abs()) / r).min(0.9999);
            p.nz[k] = nz as f32;
        }
        p
    }

    #[test]
    fn a_sphere_gets_thin_layers_at_the_poles_and_thick_at_the_equator() {
        let a = Adaptive { cusp: 0.05, min: 0.08, max: 0.28 };
        let ls = adaptive(50.0, 0.2, &a, &sphere_profile(25.0, 0.02));
        let h_at = |z: f64| ls.iter().find(|l| l.0 >= z).unwrap().1;
        assert!(h_at(25.0) > 0.27, "equator {}", h_at(25.0));
        assert!(h_at(49.0) < 0.1, "pole {}", h_at(49.0));
        assert!((ls.last().unwrap().0 - 50.0).abs() < 1e-9);
        for w in ls.windows(2) {
            assert!(w[1].1 >= a.min - 1e-9 && w[1].1 <= a.max + 1e-9, "h {}", w[1].1);
            assert!((w[1].0 - w[0].0 - w[1].1).abs() < 1e-9);
        }
        // Fewer layers than a uniform stack fine enough for the poles.
        assert!(ls.len() < (50.0 / 0.08) as usize);
    }

    /// Every layer sits on the one below and the stack ends at the top.
    fn stacked(ls: &[(f64, f64)], height: f64) {
        assert!((ls.last().unwrap().0 - height).abs() < 1e-12, "top {}", ls.last().unwrap().0);
        for w in ls.windows(2) {
            assert!((w[1].0 - w[0].0 - w[1].1).abs() < 1e-9, "{w:?}");
        }
    }

    #[test]
    fn a_six_mm_cube_has_thirty_layers_up_to_its_top() {
        // Past the top by rounding, as an f32 field's bounds may end, or as a proven box reaches
        // past its solid, is no layer of its own.
        for height in [6.0, 6.0 + 2.4e-7, 6.0 + 3.6e-6, 6.0 + 4.4e-15, 6.005] {
            let ls = crate::layer_heights(height, 0.2, 0.2);
            assert_eq!(ls.len(), 30, "{height}: {:?}", &ls[28..]);
            stacked(&ls, 6.0);
        }
    }

    #[test]
    fn the_top_lands_on_the_layer_boundary_nearest_the_part_s_height() {
        // Every layer as planned: a rest under half a layer is no layer, half a layer or more is
        // a whole one, so the top is at most half a layer over or under the part's.
        for (height, top, n) in [(10.0, 10.05, 50), (10.04, 10.05, 50), (10.1, 10.05, 50), (10.2, 10.25, 51), (9.94, 9.85, 49)] {
            let ls = crate::layer_heights(height, 0.25, 0.2);
            assert_eq!(ls.len(), n, "{height}");
            stacked(&ls, top);
            assert!(ls[1..].iter().all(|l| (l.1 - 0.2).abs() < 1e-9), "{height}: {ls:?}");
        }
    }

    #[test]
    fn flat_faces_land_on_layer_boundaries() {
        let a = Adaptive { cusp: 0.05, min: 0.08, max: 0.28 };
        let mut p = SlopeProfile::empty(0.0, 20.0, 0.02);
        p.flats = vec![7.33, 12.05];
        let ls = adaptive(20.0, 0.2, &a, &p);
        for f in [7.33, 12.05] {
            assert!(ls.iter().any(|l| (l.0 - f).abs() < 1e-9), "no boundary at {f}");
        }
    }
}

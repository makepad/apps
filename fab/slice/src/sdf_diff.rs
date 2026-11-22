//! Where two SDF shapes can slice differently: the frame-space Z range a
//! shape edit reaches (csg_sdf's change region, placed and grown by the
//! tracer's reach: its coarse search and fine cells read values that far from
//! the surface), so an edited SDF part re-traces only those layers.

use crate::live::PartField;
use makepad_csg_sdf::SdfNode;

/// The frame-space Z ranges where `a` and `b` (both placed by `to_frame`)
/// can slice differently, given the tracer's reach in mm: a range for each
/// change ([`makepad_csg_sdf::changes`]), in order and merged where they meet,
/// so the layers between two far changes stay. Empty when they are the same
/// shape, None when a change reaches everywhere.
pub fn changed_z(a: &SdfNode, b: &SdfNode, to_frame: &[[f64; 4]; 3], reach: f64) -> Option<Vec<(f64, f64)>> {
    let m = to_frame;
    let mut ranges = Vec::new();
    for (lo, hi) in makepad_csg_sdf::changes(a, b) {
        if lo.iter().chain(&hi).any(|v| !v.is_finite()) {
            return None;
        }
        let (mut zlo, mut zhi) = (f64::MAX, f64::MIN);
        for k in 0..8 {
            let c = [if k & 1 == 0 { lo[0] } else { hi[0] }, if k & 2 == 0 { lo[1] } else { hi[1] }, if k & 4 == 0 { lo[2] } else { hi[2] }];
            let z = m[2][0] * c[0] + m[2][1] * c[1] + m[2][2] * c[2] + m[2][3];
            zlo = zlo.min(z);
            zhi = zhi.max(z);
        }
        ranges.push((zlo - reach, zhi + reach));
    }
    ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
    ranges.dedup_by(|next, merged| {
        let meets = next.0 <= merged.1;
        if meets {
            merged.1 = merged.1.max(next.1);
        }
        meets
    });
    Some(ranges)
}

/// The frame-space Z ranges where part fields `a` and `b` (in the same place) can cut
/// different contours, at the tracer's reach: [`changed_z`]'s, and those whose planes the tracer
/// reads with other slopes ([`PartField::slopes_changed`]): a layer an edit leaves alone is cut
/// again alike only with the slope it was cut with. None: everywhere.
pub fn changed_layers(a: &PartField, b: &PartField, reach: f64) -> Option<Vec<(f64, f64)>> {
    let mut ranges = changed_z(&a.shape.root, &b.shape.root, &b.to_frame, reach)?;
    ranges.extend(a.slopes_changed(b));
    Some(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_csg_sdf::{Extent, SdfOp, SdfShape};
    use std::sync::Arc;

    #[test]
    fn a_moved_child_reaches_its_old_and_new_place() {
        let at = |z: f64| SdfNode::Transform { at: [0.0, 0.0, z], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Sphere { radius: 1.0 }) };
        let tree = |z: f64| SdfNode::Combine { op: SdfOp::Union, blend: 0.5, of: vec![SdfNode::Box { size: [10.0; 3], radius: 0.0 }, at(z)] };
        let id = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let z = changed_z(&tree(4.0), &tree(6.0), &id, 1.0).unwrap();
        assert_eq!(z.len(), 1);
        // The sphere's old and new places (3..7), grown by the fold's reach and the tracer's.
        assert!(z[0].0 <= 3.0 - 1.0 && z[0].1 >= 7.0 + 1.0, "{z:?}");
        let fold = makepad_csg_sdf::bounds::fold_reach(0.5, 2);
        assert!(z[0].0 >= 3.0 - 1.0 - fold - 1e-9 && z[0].1 <= 7.0 + 1.0 + fold + 1e-9, "{z:?}");
        assert!(changed_z(&tree(4.0), &tree(4.0), &id, 1.0).unwrap().is_empty());
        let g = |p: f64| SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![SdfNode::Gyroid { period: p, wall: 1.0 }] };
        assert!(changed_z(&g(5.0), &g(6.0), &id, 1.0).is_none());
    }

    /// Two changes far apart are two ranges, each as it is alone, and the layers between them
    /// are in neither; two that meet are one, and one with no bounds among them is everywhere.
    #[test]
    fn two_far_changes_leave_the_layers_between_them() {
        let at = |z: f64, r: f64| SdfNode::Transform { at: [0.0, 0.0, z], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Sphere { radius: r }) };
        let tree = |low: f64, high: f64, apart: f64| SdfNode::Combine { op: SdfOp::Union, blend: 0.5, of: vec![SdfNode::Box { size: [10.0, 10.0, 100.0], radius: 0.0 }, at(-apart, low), at(apart, high)] };
        let id = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let z = |a: &SdfNode, b: &SdfNode| changed_z(a, b, &id, 1.0).unwrap();
        let both = z(&tree(1.0, 1.0, 30.0), &tree(2.0, 2.0, 30.0));
        assert_eq!(both, [z(&tree(1.0, 1.0, 30.0), &tree(2.0, 1.0, 30.0))[0], z(&tree(1.0, 1.0, 30.0), &tree(1.0, 2.0, 30.0))[0]], "each as it is alone");
        assert!(both[0].1 < -20.0 && both[1].0 > 20.0, "the layers between them are in neither: {both:?}");
        let near = z(&tree(1.0, 1.0, 1.0), &tree(2.0, 2.0, 1.0));
        assert_eq!(near.len(), 1, "two that meet: {near:?}");
        assert!(near[0].0 <= -1.0 - 2.0 - 1.0 && near[0].1 >= 1.0 + 2.0 + 1.0, "it holds both: {near:?}");
        let with = |period: f64| SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![SdfNode::Gyroid { period, wall: 1.0 }, at(30.0, period)] };
        assert!(changed_z(&with(5.0), &with(6.0), &id, 1.0).is_none(), "one with no bounds among them");
    }

    /// Five times a ball's distance, of radius 5: a kernel no near rung proves.
    struct Steep;
    impl makepad_csg_sdf::Sdf3 for Steep {
        fn distance(&self, p: makepad_csg_math::Vec3d) -> f64 {
            5.0 * (p.length() - 5.0)
        }
    }

    /// A piece edited far from a layer leaves it alone, whatever its slope: a band whose own
    /// pieces prove its slope keeps it when a far piece is made steeper, and so does the band
    /// round a kernel no band window proves, whose leaves prove their own slopes at their own
    /// windows, not the whole shape's.
    #[test]
    fn a_layer_is_kept_only_at_the_slope_it_was_traced_with() {
        let to = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let tree = |near: SdfNode, far: SdfNode| {
            let root = SdfNode::Combine { op: SdfOp::Union, blend: 0.5, of: vec![near, SdfNode::Transform { at: [0.0, 0.0, 30.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(far) }] };
            let (min, max) = makepad_csg_sdf::node_bounds(&root).expect("bounded");
            PartField::new(Arc::new(SdfShape { root, min, max, materials: Vec::new() }), to)
        };
        let ball = || SdfNode::Sphere { radius: 5.0 };
        let kernel = || SdfNode::Kernel(makepad_csg_sdf::SdfKernel { key: 0x51de, field: Arc::new(Steep), splash: None, bounds: Some(([-5.0; 3], [5.0; 3])), solid: None });
        let flat = || SdfNode::Ellipsoid { radii: [6.0, 6.0, 0.8] };
        let (a, bigger) = (tree(ball(), SdfNode::Sphere { radius: 2.0 }), tree(ball(), SdfNode::Sphere { radius: 2.5 }));
        let r = changed_layers(&a, &bigger, 1.0).expect("a bounded change");
        assert!(r.len() == 1 && r[0].0 > 5.0, "only the far piece's layers: {r:?}");
        let steeper = tree(ball(), flat());
        assert!(steeper.slope().0 > a.slope().0, "the flat ellipsoid is steeper: {:?} over {:?}", steeper.slope(), a.slope());
        assert!(changed_z(&a.shape.root, &steeper.shape.root, &to, 1.0).is_some_and(|r| r.len() == 1 && r[0].0 > 5.0), "its values change only far up");
        assert_eq!(steeper.layer_slope(0.5), a.layer_slope(0.5), "the ball's band proves its own slope");
        let r = changed_layers(&a, &steeper, 1.0).expect("a bounded change");
        assert!(!r.iter().any(|&(lo, hi)| lo <= 4.0 && hi >= -4.0), "the ball's layers are kept: {r:?}");
        let (k, ksteeper) = (tree(kernel(), SdfNode::Sphere { radius: 2.0 }), tree(kernel(), flat()));
        assert!(k.layer_slope(0.5) != k.slope(), "the kernel's band reads its leaves' slopes, not the whole shape's: {:?}, {:?}", k.layer_slope(0.5), k.slope());
        assert!(ksteeper.slope().0 > k.slope().0, "the flat ellipsoid is steeper: {:?} over {:?}", ksteeper.slope(), k.slope());
        assert!(changed_z(&k.shape.root, &ksteeper.shape.root, &to, 1.0).is_some_and(|r| r.len() == 1 && r[0].0 > 5.0), "its values change only far up");
        assert!(ksteeper.plane_slope(0.5).same(&k.plane_slope(0.5)), "the kernel's band keeps its slopes: {:?}, {:?}", ksteeper.plane_slope(0.5), k.plane_slope(0.5));
        let r = changed_layers(&k, &ksteeper, 1.0).expect("a bounded change");
        assert!(!r.iter().any(|&(lo, hi)| lo <= 4.0 && hi >= -4.0), "the kernel's layers are kept: {r:?}");
    }

    /// A sharp union of two flat ellipsoids side by side, a crease in each of their layers, under a
    /// third flat one above them: made flatter, the far one changes no value within the tracer's
    /// reach of the pair, so [`changed_z`] keeps their layers, but the pair's band reads it within
    /// the window its own slope needs, so the band's slope changes and with it the corners the
    /// tracer finds at the crease. Some of the pair's layers cut other contours then, and every
    /// layer [`changed_layers`] keeps cuts the contours a fresh slice cuts.
    #[test]
    fn a_far_edit_that_reslopes_a_band_retraces_its_layers() {
        let to = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let p = makepad_fab_slicer::sdf::SdfParams::default();
        let reach = 2.0 * p.coarse * 2f64.sqrt() + p.fine;
        let at = |c: [f64; 3], of: SdfNode| SdfNode::Transform { at: c, rotate: [0.0; 3], scale: 1.0, of: Box::new(of) };
        let flat = |z: f64| SdfNode::Ellipsoid { radii: [6.0, 6.0, z] };
        let shape = |far: f64| {
            let root = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![flat(1.2), at([6.0, 0.0, 0.0], flat(1.2)), at([3.0, 0.0, 5.5], flat(far))] };
            let (min, max) = makepad_csg_sdf::node_bounds(&root).expect("bounded");
            Arc::new(SdfShape { root, min, max, materials: Vec::new() })
        };
        let (was, now) = (shape(1.2), shape(1.02));
        let ranges = changed_z(&was.root, &now.root, &to, reach).expect("a bounded change");
        assert!(ranges.iter().all(|r| r.0 > 1.2), "its values change only above the pair: {ranges:?}");
        let (a, b) = (PartField::new(was.clone(), to), PartField::new(now.clone(), to));
        a.layer_slope(0.0);
        let changed = changed_layers(&a, &b, reach).expect("a bounded change");
        let zs: Vec<f64> = (0..48).map(|i| -1.175 + 0.05 * i as f64).collect();
        let slice = |s: &Arc<SdfShape>| {
            let e = makepad_csg_sdf::shape_bounds(s, &to, 0.01).expect("a solid");
            PartField::new(s.clone(), to).with_bounds(Some((e.min, e.max))).source().expect("a contour source").contours(&zs)
        };
        let (before, after) = (slice(&was), slice(&now));
        let moved: Vec<f64> = zs.iter().zip(before.iter().zip(&after)).filter(|(_, (x, y))| x != y).map(|(z, _)| *z).collect();
        assert!(!moved.is_empty(), "the band's new slope moves some of the pair's contours");
        for ((z, x), y) in zs.iter().zip(&before).zip(&after) {
            let kept = !changed.iter().any(|&(lo, hi)| *z >= lo && *z <= hi);
            assert!(!kept || x == y, "the layer at {z} is kept, yet a fresh slice cuts it otherwise: retraced {changed:?}, moved {moved:?}");
        }
    }

    /// A small xorshift, for random edits from fixed seeds.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        /// Uniform in `0..1`.
        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// A small figure to edit at random: a tall flat ellipsoid body and four pieces in one
    /// smooth union, an arm of three more in a union of its own inside it, and a flat cut
    /// bottom. A piece is a ball, a rounded box, an ellipsoid or a capsule.
    #[derive(Clone)]
    struct Figure {
        kind: [u8; 7],
        size: [f64; 7],
        at: [[f64; 3]; 7],
        blend: [f64; 2],
    }

    fn placed(p: [f64; 3], of: SdfNode) -> SdfNode {
        SdfNode::Transform { at: p, rotate: [0.0; 3], scale: 1.0, of: Box::new(of) }
    }

    impl Figure {
        fn new() -> Figure {
            Figure {
                kind: [0, 1, 2, 3, 0, 2, 1],
                size: [3.0, 2.5, 3.5, 2.0, 2.5, 2.0, 1.8],
                at: [[11.0, -12.0, 0.0], [-10.0, -4.0, 1.0], [0.0, 21.0, 0.0], [3.0, 6.0, 5.5], [8.0, 12.0, 0.0], [11.0, 15.0, 0.0], [13.5, 17.5, 1.0]],
                blend: [1.5, 1.0],
            }
        }

        fn piece(&self, i: usize) -> SdfNode {
            let s = self.size[i];
            let node = match self.kind[i] {
                0 => SdfNode::Sphere { radius: s },
                1 => SdfNode::Box { size: [1.6 * s, 1.2 * s, 1.4 * s], radius: 0.3 * s },
                2 => SdfNode::Ellipsoid { radii: [s, 0.7 * s, 1.2 * s] },
                _ => SdfNode::Capsule { a: [-s, 0.0, 0.0], b: [s, 0.5 * s, 0.0], radius: 0.6 * s },
            };
            placed(self.at[i], node)
        }

        fn shape(&self) -> Arc<SdfShape> {
            let arm = SdfNode::Combine { op: SdfOp::Union, blend: self.blend[1], of: (4..7).map(|i| self.piece(i)).collect() };
            let mut of = vec![SdfNode::Ellipsoid { radii: [10.0, 22.0, 6.0] }];
            of.extend((0..4).map(|i| self.piece(i)));
            of.push(arm);
            let body = SdfNode::Combine { op: SdfOp::Union, blend: self.blend[0], of };
            let root = SdfNode::Combine { op: SdfOp::Difference, blend: 0.2, of: vec![body, placed([0.0, -25.0, 0.0], SdfNode::Box { size: [60.0, 10.0, 60.0], radius: 0.0 })] };
            let (min, max) = makepad_csg_sdf::node_bounds(&root).expect("bounded");
            Arc::new(SdfShape { root, min, max, materials: Vec::new() })
        }

        /// One random edit, its kind's name: a piece's size, a piece replaced by another kind,
        /// a piece moved, or a blend's radius.
        fn edit(&mut self, r: &mut Rng) -> &'static str {
            let i = r.below(7);
            match r.below(4) {
                0 => {
                    self.size[i] = (self.size[i] * (0.8 + 0.45 * r.unit())).clamp(1.0, 5.0);
                    "size"
                }
                1 => {
                    self.kind[i] = ((self.kind[i] as usize + 1 + r.below(3)) % 4) as u8;
                    "replace"
                }
                2 => {
                    for k in 0..3 {
                        self.at[i][k] = (self.at[i][k] + 6.0 * (r.unit() - 0.5)).clamp(-18.0, 24.0);
                    }
                    "move"
                }
                _ => {
                    self.blend[r.below(2)] = 0.3 + 2.7 * r.unit();
                    "blend"
                }
            }
        }
    }

    /// A band whose every leaf proves a slope traces the segments its plane's slope alone traces,
    /// bit for bit: a circle its map clears holds no surface, so the cells the map leaves out hold
    /// no segment.
    #[test]
    fn a_proven_band_traces_as_its_planes_slope_alone_does() {
        use makepad_fab_slicer::sdf::{contour_segments_mapped, SdfParams, SdfSource};
        let to: [[f64; 4]; 3] = [[1.0, 0.0, 0.0, 30.0], [0.0, 0.0, -1.0, 30.0], [0.0, 1.0, 0.0, 25.0]];
        let (mut proven, mut open) = (0, 0);
        for seed in [1, 2, 3, 20u64] {
            let mut r = Rng(0x6a09_e667_f3bc_c908 ^ (seed << 32));
            let mut fig = Figure::new();
            for _ in 0..30 {
                let what = fig.edit(&mut r);
                let shape = fig.shape();
                let e = makepad_csg_sdf::shape_bounds(&shape, &to, 0.01).expect("a solid");
                let pf = PartField::new(shape, to);
                let f = pf.field().expect("a field");
                for _ in 0..3 {
                    let z = e.min[2] + r.unit() * (e.max[2] - e.min[2]);
                    let plane = pf.plane_slope(z);
                    let Some(map) = plane.map.as_deref() else { continue };
                    if !map.of.iter().all(|q| q.0.is_finite()) {
                        open += 1;
                        continue;
                    }
                    proven += 1;
                    let at = |x: f64, y: f64| f(x, y, z - SdfSource::BELOW_PLANE);
                    let many = |pts: &[(f64, f64)], reach: f64, out: &mut [f64]| {
                        for (o, q) in out.iter_mut().zip(pts) {
                            *o = at(q.0, q.1).clamp(-reach, reach);
                        }
                    };
                    let p = SdfParams { lipschitz: plane.lipschitz, window: plane.window, ..SdfParams::default() };
                    let trace = |map| contour_segments_mapped(&at, &many, [e.min[0], e.min[1]], [e.max[0], e.max[1]], &p, map);
                    assert!(trace(Some(map)) == trace(None), "seed {seed}, a {what} edit: the band at {z} traces otherwise by its map: {plane:?}");
                }
            }
        }
        assert!(proven > 100, "the figures' bands are proven: {proven}, {open} are not");
    }

    /// Random edits of a small figure from fixed seeds, as the live slicer takes them: each
    /// extent, proven after the last few ([`makepad_csg_sdf::shape_bounds_after`]), holds the
    /// fresh proof's solid and lies within `tol` of its bounds or past them; the part sliced over
    /// it cuts the contours it cuts over the fresh proof's box; and a layer outside the edit's z
    /// ranges ([`changed_layers`], at the tracer's reach, with the bands whose slopes it changed)
    /// cuts the contours it cut before the edit. Seed 20 once kept a layer the edit's new slope
    /// traced otherwise.
    #[test]
    fn random_edits_slice_as_a_fresh_proof_does() {
        // the figure stands up: its y is the frame's z, its cut bottom 5 mm up
        let to: [[f64; 4]; 3] = [[1.0, 0.0, 0.0, 30.0], [0.0, 0.0, -1.0, 30.0], [0.0, 1.0, 0.0, 25.0]];
        let tol = 0.01;
        let reach = 2.0 * makepad_fab_slicer::sdf::SdfParams::default().coarse * 2f64.sqrt() + makepad_fab_slicer::sdf::SdfParams::default().fine;
        let slice = |shape: &Arc<SdfShape>, e: &Extent, zs: &[f64]| PartField::new(shape.clone(), to).with_bounds(Some((e.min, e.max))).source().expect("a contour source").contours(zs);
        let (mut layers, mut kept_layers, mut resloped) = (0, 0, 0);
        for seed in [1, 2, 3, 20u64] {
            let mut r = Rng(0x6a09_e667_f3bc_c908 ^ (seed << 32));
            let mut fig = Figure::new();
            let mut recent: Vec<(Arc<SdfShape>, Extent)> = Vec::new();
            for _ in 0..20 {
                let what = fig.edit(&mut r);
                let shape = fig.shape();
                let before: Vec<(&SdfShape, &Extent)> = recent.iter().rev().map(|(s, e)| (&**s, e)).collect();
                let after = makepad_csg_sdf::shape_bounds_after(&shape, &to, tol, &before).expect("a solid");
                let fresh = makepad_csg_sdf::shape_bounds(&shape, &to, tol).expect("a solid");
                let (lo, hi) = fresh.found.expect("solid points");
                for a in 0..3 {
                    assert!(after.min[a] <= lo[a] && after.max[a] >= hi[a], "seed {seed}, a {what} edit: {:?}..{:?} short of the fresh proof's solid {lo:?}..{hi:?}", after.min, after.max);
                    assert!(after.min[a] <= fresh.min[a] + tol && after.max[a] >= fresh.max[a] - tol, "seed {seed}, a {what} edit: {:?}..{:?} inside the fresh bounds {:?}..{:?}", after.min, after.max, fresh.min, fresh.max);
                }
                // a few layers across the part, the lowest and highest among them
                let span = |e: &Extent, t: f64| e.min[2] + t * (e.max[2] - e.min[2]);
                let zs: Vec<f64> = [0.002, 0.998].into_iter().chain((0..2).map(|_| r.unit())).map(|t| span(&fresh, t)).collect();
                assert!(slice(&shape, &after, &zs) == slice(&shape, &fresh, &zs), "seed {seed}, a {what} edit: the part sliced over its extent {:?}..{:?} cuts other contours than over the fresh proof's {:?}..{:?} at {zs:?}", after.min, after.max, fresh.min, fresh.max);
                layers += zs.len();
                if let Some((old, old_e)) = recent.last() {
                    let ranges = changed_z(&old.root, &shape.root, &to, reach).expect("a bounded change");
                    // as the live slicer keeps them: none in a band of a new slope
                    let (was, now) = (PartField::new(old.clone(), to), PartField::new(shape.clone(), to));
                    was.layer_slope(0.0);
                    let changed = changed_layers(&was, &now, reach).expect("a bounded change");
                    // the edited field's bands, taken from the last field's where the edit leaves
                    // them, are a fresh field's
                    let fresh_field = PartField::new(shape.clone(), to);
                    for z in (fresh.min[2].floor() as i64 - 1)..=(fresh.max[2].ceil() as i64 + 1) {
                        let z = z as f64 + 0.5;
                        assert!(now.plane_slope(z).same(&fresh_field.plane_slope(z)), "seed {seed}, a {what} edit: the band at {z} reads another slope than a fresh field's: {:?}, {:?}", now.plane_slope(z), fresh_field.plane_slope(z));
                    }
                    assert!(changed.starts_with(&ranges), "the change's own ranges first: {changed:?} of {ranges:?}");
                    resloped += (changed.len() > ranges.len()) as usize;
                    let both = (old_e.min[2].min(fresh.min[2]), old_e.max[2].max(fresh.max[2]));
                    let out: Vec<f64> = (0..8).map(|_| both.0 + r.unit() * (both.1 - both.0)).filter(|&z| !changed.iter().any(|&(a, b)| z >= a && z <= b)).collect();
                    assert!(slice(old, old_e, &out) == slice(&shape, &fresh, &out), "seed {seed}, a {what} edit moved the contours at {out:?}, outside its z ranges {changed:?}");
                    kept_layers += out.len();
                }
                recent.push((shape, after));
                if recent.len() > 8 {
                    recent.remove(0);
                }
            }
        }
        assert!(kept_layers > 150 && resloped > 0, "{kept_layers} layers outside an edit's ranges, of {layers}; {resloped} edits changed a band's slope");
    }
}

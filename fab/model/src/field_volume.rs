//! The volume of a part made from a distance field, read from its field alone: no mesh is built
//! for such a part.

use makepad_csg_math::Vec3d;
use makepad_csg_sdf::{Chunks, Sdf3};
use std::sync::Arc;

/// The volume (mm³) of the solid `field <= 0` inside the box `min..max`, read in cubes of `cell`.
/// Each cube counts for the share of it the solid fills, `clamp(1/2 - s / cell, 0, 1)`, with `s`
/// the signed distance at its middle: the field's value there over its slope there. That is exact
/// for a flat face at any tilt and offset; for a curved one it is off by about the cube's size
/// squared over the curve's radius (a ball 60 cells across reads 0.03% large, a box's edges add
/// about as much). The box's sides cut the solid as flat faces. Blocks of cubes their value and
/// slope prove all solid or all empty count whole, unread inside: the same sum, for far less
/// work. A block's slope is what its chunks' pieces prove on the side of the surface its middle
/// is on, at the values between its middle's and 0 ([`Chunks::slopes_in`]: all a line from the
/// middle passes before the surface), or `lipschitz` (the most the field's value changes per mm)
/// where they prove none.
/// Deterministic: the same field, box and cell give the same bits.
pub fn field_volume(field: &Arc<Chunks>, min: [f64; 3], max: [f64; 3], cell: f64, lipschitz: f64) -> f64 {
    let lip = lipschitz.max(1.0);
    // The cubes reach one past the box on each side, so its sides' ramps are read whole.
    let origin = min.map(|v| v - cell);
    let n: [u64; 3] = std::array::from_fn(|a| ((max[a] - min[a]) / cell).ceil().max(0.0) as u64 + 2);
    let levels = n.iter().map(|&k| u64::BITS - (k - 1).leading_zeros()).max().unwrap_or(0);
    let (middle, half) = (Vec3d::new((min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0, (min[2] + max[2]) / 2.0), Vec3d::new((max[0] - min[0]) / 2.0, (max[1] - min[1]) / 2.0, (max[2] - min[2]) / 2.0));
    // the solid's value at points: the field's, cut by the box's sides
    let values = |points: Vec<Vec3d>| -> Vec<f64> {
        let cut: Vec<f64> = points.iter().map(|&p| outside_box(p - middle, half)).collect();
        sample(field, points).into_iter().zip(cut).map(|(d, b)| d.max(b)).collect()
    };
    let at = |b: [u64; 3], size: u64| Vec3d::new(origin[0] + (b[0] as f64 + 0.5) * (size as f64) * cell, origin[1] + (b[1] as f64 + 0.5) * (size as f64) * cell, origin[2] + (b[2] as f64 + 0.5) * (size as f64) * cell);
    let mut filled = 0.0f64;
    let mut blocks: Vec<[u64; 3]> = vec![[0; 3]];
    let mut surface: Vec<(Vec3d, f64)> = Vec::new();
    for level in (0..=levels).rev() {
        let size = 1u64 << level;
        let read = values(blocks.iter().map(|&b| at(b, size)).collect());
        // the farthest a cube's middle in the block lies from the block's, and a cube's half
        let span = (size - 1) as f64 * cell * 0.5 * 3f64.sqrt() + cell * 0.5;
        let mut next = Vec::new();
        for (&b, &g) in blocks.iter().zip(&read) {
            // what the value can change by over the block before it changes sign: at the slope
            // its pieces prove on its middle's side, where a slope could decide it
            let reach = if lip > 1.0 && g.abs() >= span {
                let s = field.slopes_in(&block_box(at(b, size), size as f64 * cell), g.abs());
                let local = if g > 0.0 { s.outside } else { s.inside };
                span * if local.is_finite() { local.max(1.0) } else { lip }
            } else {
                lip * span
            };
            if g <= -reach {
                filled += (0..3).map(|a| (((b[a] + 1) * size).min(n[a]) - b[a] * size) as f64).product::<f64>();
            } else if g >= reach {
            } else if level == 0 {
                surface.push((at(b, 1), g));
            } else {
                for k in 0..8u64 {
                    let c = [2 * b[0] + (k & 1), 2 * b[1] + (k >> 1 & 1), 2 * b[2] + (k >> 2)];
                    if (0..3).all(|a| c[a] * (size / 2) < n[a]) {
                        next.push(c);
                    }
                }
            }
        }
        blocks = next;
    }
    // A cube the surface passes near: its share from the distance over the slope, read a
    // thousandth of a cube along each axis.
    let step = cell * 1e-3;
    let ahead = values(surface.iter().flat_map(|&(p, _)| [Vec3d::new(p.x + step, p.y, p.z), Vec3d::new(p.x, p.y + step, p.z), Vec3d::new(p.x, p.y, p.z + step)]).collect());
    for (&(_, g), ahead) in surface.iter().zip(ahead.chunks_exact(3)) {
        let slope = ahead.iter().map(|a| ((a - g) / step).powi(2)).sum::<f64>().sqrt();
        filled += if slope.is_finite() && slope > 1e-9 {
            (0.5 - g / slope / cell).clamp(0.0, 1.0)
        } else if g < 0.0 {
            1.0
        } else if g > 0.0 {
            0.0
        } else {
            0.5
        };
    }
    filled * cell * cell * cell
}

/// The box of side `side` about `c`.
fn block_box(c: Vec3d, side: f64) -> makepad_csg_sdf::Bounds {
    let h = 0.5 * side;
    ([c.x - h, c.y - h, c.z - h], [c.x + h, c.y + h, c.z + h])
}

/// The signed distance from the box of half sides `half` about the origin to `p`.
fn outside_box(p: Vec3d, half: Vec3d) -> f64 {
    let q = Vec3d::new(p.x.abs() - half.x, p.y.abs() - half.y, p.z.abs() - half.z);
    let out = Vec3d::new(q.x.max(0.0), q.y.max(0.0), q.z.max(0.0));
    (out.x * out.x + out.y * out.y + out.z * out.z).sqrt() + q.x.max(q.y).max(q.z).min(0.0)
}

/// The field's values at `points`, in parallel chunks, in order.
fn sample(field: &Arc<Chunks>, points: Vec<Vec3d>) -> Vec<f64> {
    if points.is_empty() {
        return Vec::new();
    }
    let size = points.len().div_ceil(makepad_csg_math::thread_count() * 4).max(64);
    let points = Arc::new(points);
    let tasks: Vec<_> = (0..points.len().div_ceil(size))
        .map(|c| {
            let (field, points) = (field.clone(), points.clone());
            move || {
                let part = &points[c * size..((c + 1) * size).min(points.len())];
                let mut out = vec![0.0; part.len()];
                field.distance_many(part, &mut out);
                out
            }
        })
        .collect();
    makepad_csg_math::thread_pool::parallel_for(tasks).concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_csg_sdf::{SdfNode, SdfShape};

    fn volume(root: SdfNode, min: [f64; 3], max: [f64; 3], cell: f64) -> f64 {
        let shape = SdfShape { root, min, max, materials: Vec::new() };
        field_volume(&Arc::new(Chunks::of_shape(&shape)), min, max, cell, shape.lipschitz())
    }

    #[test]
    fn a_field_ball_has_a_balls_volume() {
        let exact = 4.0 / 3.0 * std::f64::consts::PI * 15f64.powi(3);
        let v = volume(SdfNode::Sphere { radius: 15.0 }, [-16.0; 3], [16.0; 3], 0.5);
        println!("ball: {v:.3} mm3 against {exact:.3}, {:+.4}%", (v - exact) / exact * 100.0);
        assert!((v - exact).abs() <= 0.001 * exact);
    }

    #[test]
    fn a_turned_field_box_has_a_boxs_volume() {
        let turned = SdfNode::Transform { at: [0.0, 1.0, 0.0], rotate: [30.0, 20.0, 10.0], scale: 1.0, of: Box::new(SdfNode::Box { size: [20.0, 10.0, 30.0], radius: 0.0 }) };
        let v = volume(turned, [-20.0; 3], [20.0; 3], 0.5);
        println!("turned box: {v:.3} mm3 against 6000, {:+.4}%", (v - 6000.0) / 6000.0 * 100.0);
        assert!((v - 6000.0).abs() <= 0.002 * 6000.0);
    }

    /// Twice a capsule's distance: a kernel twice as steep as a distance.
    struct SteepCapsule;
    impl Sdf3 for SteepCapsule {
        fn distance(&self, p: Vec3d) -> f64 {
            let y = p.y.clamp(-8.0, 8.0);
            2.0 * (p.x.hypot(p.z).hypot(p.y - y) - 3.0)
        }
    }

    /// A steep kernel's capsule beside a ball: the ball's blocks count at a distance's slope, the
    /// kernel's at the field's, and both solids read whole.
    #[test]
    fn a_steep_kernel_beside_a_ball_reads_both_volumes() {
        let capsule = SdfNode::Kernel(makepad_csg_sdf::SdfKernel { key: 3, field: Arc::new(SteepCapsule), splash: None, bounds: Some(([-4.0, -12.0, -4.0], [4.0, 12.0, 4.0])), solid: None });
        let ball = SdfNode::Transform { at: [20.0, 0.0, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Sphere { radius: 8.0 }) };
        let root = SdfNode::Combine { op: makepad_csg_sdf::SdfOp::Union, blend: 0.0, of: vec![capsule, ball] };
        let pi = std::f64::consts::PI;
        let exact = pi * 9.0 * 16.0 + 4.0 / 3.0 * pi * 27.0 + 4.0 / 3.0 * pi * 512.0;
        let v = volume(root, [-5.0, -13.0, -9.0], [29.0, 13.0, 9.0], 0.25);
        println!("capsule and ball: {v:.3} mm3 against {exact:.3}, {:+.4}%", (v - exact) / exact * 100.0);
        assert!((v - exact).abs() <= 0.002 * exact);
    }

    #[test]
    fn the_fields_box_cuts_its_solid() {
        // a ball whose box holds only its upper half
        let exact = 2.0 / 3.0 * std::f64::consts::PI * 15f64.powi(3);
        let v = volume(SdfNode::Sphere { radius: 15.0 }, [-16.0, 0.0, -16.0], [16.0; 3], 0.5);
        println!("half ball: {v:.3} mm3 against {exact:.3}, {:+.4}%", (v - exact) / exact * 100.0);
        assert!((v - exact).abs() <= 0.001 * exact);
    }
}

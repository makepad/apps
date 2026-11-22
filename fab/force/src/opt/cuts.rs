//! The cuts a lightening variant makes, as the optimiser plans them (printer frame, mm, Z the
//! build direction), and their distance fields. The script's generators (`m.hole_row`,
//! `m.pocket`) build the same shapes; the fields let the optimiser and its tests solve a
//! variant of a part given as a field without a script.
//!
//! What keeps them printable: a hole or pocket whose axis lies across the build direction gets a
//! 45-degree roof (a hole becomes a teardrop, a pocket a gable over its top edge), so nothing
//! overhangs flatter than 45 degrees.

use crate::V3;

/// Holes or pockets whose axis is further than this from the build direction get a roof
/// (cos 45 degrees).
pub const ROOF_COS: f64 = std::f64::consts::FRAC_1_SQRT_2;

#[derive(Clone, Debug, PartialEq)]
pub enum Cut {
    /// `count` equal holes centred evenly from `from` to `to`, `length` long along `axis`.
    Holes { from: V3, to: V3, count: usize, diameter: f64, length: f64, axis: V3, teardrop: bool },
    /// A rounded rectangle `size` (along `along`, then across) opening at `at` on a face, `depth`
    /// into the material along `axis`, its corners rounded to `corner`.
    Pocket { at: V3, size: [f64; 2], along: V3, axis: V3, depth: f64, corner: f64, roof: bool },
}

pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
pub fn unit(a: V3) -> V3 {
    let l = dot(a, a).sqrt().max(1e-12);
    a.map(|v| v / l)
}

/// The build direction's part across `axis`, as a unit vector (any vector across it when the
/// axis is the build direction).
pub fn up_across(axis: V3) -> V3 {
    let z = [0.0, 0.0, 1.0];
    let u = sub(z, axis.map(|v| v * dot(z, axis)));
    if dot(u, u) < 1e-9 {
        return unit(cross(axis, [1.0, 0.0, 0.0]));
    }
    unit(u)
}

/// Whether a hole or pocket along `axis` needs a roof to print.
pub fn needs_roof(axis: V3) -> bool {
    dot(unit(axis), [0.0, 0.0, 1.0]).abs() < ROOF_COS
}

/// The distance to a box of half sizes `b` with its edges rounded to `r`, centred on 0.
fn round_box(p: V3, b: V3, r: f64) -> f64 {
    let q: V3 = std::array::from_fn(|a| p[a].abs() - b[a] + r);
    let out = (q.iter().map(|v| v.max(0.0).powi(2)).sum::<f64>()).sqrt();
    out + q[0].max(q[1]).max(q[2]).min(0.0) - r
}

impl Cut {
    /// The holes' centres.
    pub fn centres(&self) -> Vec<V3> {
        match self {
            Cut::Holes { from, to, count, .. } => {
                if *count <= 1 {
                    return vec![std::array::from_fn(|a| 0.5 * (from[a] + to[a]))];
                }
                (0..*count).map(|i| std::array::from_fn(|a| from[a] + (to[a] - from[a]) * i as f64 / (*count - 1) as f64)).collect()
            }
            Cut::Pocket { at, .. } => vec![*at],
        }
    }

    /// The distance to the material this cut takes away (negative inside the cut).
    pub fn distance(&self, p: V3) -> f64 {
        match self {
            Cut::Holes { diameter, length, axis, teardrop, .. } => {
                let r = 0.5 * diameter;
                let c = unit(*axis);
                let up = up_across(c);
                let side = cross(c, up);
                self.centres()
                    .into_iter()
                    .map(|o| {
                        let d = sub(p, o);
                        let (t, x, y) = (dot(d, c), dot(d, side), dot(d, up));
                        let mut across = (x * x + y * y).sqrt() - r;
                        if *teardrop {
                            // the 45-degree roof: a diamond from the centre to the tip r sqrt 2 above it
                            let k = r * std::f64::consts::FRAC_1_SQRT_2;
                            across = across.min((x.abs() + (y - k).abs() - k) * std::f64::consts::FRAC_1_SQRT_2);
                        }
                        let along = t.abs() - 0.5 * length;
                        let o = [across.max(0.0), along.max(0.0)];
                        (o[0] * o[0] + o[1] * o[1]).sqrt() + across.max(along).min(0.0)
                    })
                    .fold(f64::INFINITY, f64::min)
            }
            Cut::Pocket { at, size, along, axis, depth, corner, roof } => {
                let c = unit(*axis);
                let a = unit(*along);
                let b = cross(c, a);
                let d = sub(p, *at);
                // out past the opening by the corner (and a little), so the rounding stays outside
                let over = corner + 0.5;
                let (pa, pb, pc) = (dot(d, a), dot(d, b), dot(d, c) - 0.5 * (depth - over));
                let r = corner.min(0.5 * size[0]).min(0.5 * size[1]).min(0.5 * (depth + over));
                let mut dist = round_box([pa, pb, pc], [0.5 * size[0], 0.5 * size[1], 0.5 * (depth + over)], r);
                if *roof {
                    // the gable over the edge that faces up: a diamond in the (b, axis) plane at
                    // the face, `depth` to each tip, as long as the straight part of that edge
                    let up = if dot(b, [0.0, 0.0, 1.0]) >= 0.0 { 1.0 } else { -1.0 };
                    let (qb, qc) = (up * dot(d, b) - 0.5 * size[1], dot(d, c));
                    let diamond = (qb.abs() + qc.abs() - depth) * std::f64::consts::FRAC_1_SQRT_2;
                    let ends = pa.abs() - (0.5 * size[0] - r).max(0.0);
                    dist = dist.min(diamond.max(ends));
                }
                dist
            }
        }
    }
}

/// A part's field with `cuts` taken away.
pub fn cut_field<'a>(part: &'a dyn Fn(V3) -> f64, cuts: &'a [Cut]) -> impl Fn(V3) -> f64 + 'a {
    move |p| cuts.iter().fold(part(p), |d, c| d.max(-c.distance(p)))
}

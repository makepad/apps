//! Whether a placed body mesh sits on its footprint. The body's own geometry where it meets the
//! board (a lead, a termination, a leg in a hole) must reach every land, and on average sit on the
//! lands' centres. The converter checks a supplier's model on the supplier's footprint with it, and
//! the same check holds for a body placed on any footprint. A bounding-box centre does not do: a
//! connector that overhangs the board edge sits off its pads' centre by design.

use crate::mesh::Mesh;

/// How far (mm) the body's contact may lie outside a land or a drill and still reach it.
pub const REACH: f64 = 0.1;

/// How far (mm) the body's contact may sit off its lands' centres on average. Correctly placed
/// supplier models measured up to 0.19 mm (a 6-pin USB-C whose pin tails sit at the back of their
/// pads); symmetric parts measure 0.
pub const SHIFT: f64 = 0.25;

/// Geometry at or below this height (mm above the board surface) meets the board.
pub const BOARD_LEVEL: f64 = 0.1;

/// Geometry at or below this height (mm) goes into a hole.
const IN_HOLE: f64 = -0.01;

/// A land to reach, in the body's frame (mm, Z up from the board surface): the axis-aligned box
/// around a surface pad, or around a hole's drill.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Land {
    pub lo: [f64; 2],
    pub hi: [f64; 2],
    pub hole: bool,
}

impl Land {
    /// The box around a `size` (`[w, h]`) rectangle centred on `at` and turned by `rot` degrees.
    pub fn turned(at: [f64; 2], size: [f64; 2], rot: f64, hole: bool) -> Land {
        let (s, c) = rot.to_radians().sin_cos();
        let half = [
            (size[0] * c).abs() * 0.5 + (size[1] * s).abs() * 0.5,
            (size[0] * s).abs() * 0.5 + (size[1] * c).abs() * 0.5,
        ];
        Land { lo: [at[0] - half[0], at[1] - half[1]], hi: [at[0] + half[0], at[1] + half[1]], hole }
    }

    fn centre(&self) -> [f64; 2] {
        [(self.lo[0] + self.hi[0]) * 0.5, (self.lo[1] + self.hi[1]) * 0.5]
    }
}

/// How a body sits on its lands.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Fit {
    /// The indices of the lands the body does not reach.
    pub missed: Vec<usize>,
    /// The mean, over the lands it reaches, of the centre of the body's contact on a land (clipped to
    /// the land grown by [`REACH`]) less the land's centre, mm. A body moved off its lands moves this.
    pub shift: [f64; 2],
}

impl Fit {
    /// Every land reached, and the contact on average within [`SHIFT`] of the lands' centres.
    pub fn sits(&self) -> bool {
        self.missed.is_empty() && self.shift_len() <= SHIFT
    }

    pub fn shift_len(&self) -> f64 {
        self.shift[0].hypot(self.shift[1])
    }
}

/// How `mesh` sits on `lands`. A surface land is reached by geometry at board level over it. A hole
/// is reached by geometry inside its drill below the surface when the model goes below the surface
/// anywhere (its pins or pegs enter the board), else at board level over the drill (a model whose
/// pins stop at the surface). Each land is grown by [`REACH`].
pub fn fit(mesh: &Mesh, lands: &[Land]) -> Fit {
    let hole_top = if mesh.points().any(|p| f64::from(p[2]) <= IN_HOLE) { IN_HOLE } else { BOARD_LEVEL };
    let mut out = Fit::default();
    let mut sum = [0.0; 2];
    let mut reached = 0usize;
    for (i, land) in lands.iter().enumerate() {
        let top = if land.hole { hole_top } else { BOARD_LEVEL };
        let lo = [land.lo[0] - REACH, land.lo[1] - REACH];
        let hi = [land.hi[0] + REACH, land.hi[1] + REACH];
        let mut extent: Option<([f64; 2], [f64; 2])> = None;
        for prim in &mesh.prims {
            for t in prim.indices.chunks_exact(3) {
                let at = |i: u32| prim.positions.get(i as usize).map(|p| p.map(f64::from));
                let (Some(a), Some(b), Some(c)) = (at(t[0]), at(t[1]), at(t[2])) else { continue };
                if let Some((l, h)) = contact([a, b, c], lo, hi, top) {
                    extent = Some(match extent {
                        None => (l, h),
                        Some((el, eh)) => ([el[0].min(l[0]), el[1].min(l[1])], [eh[0].max(h[0]), eh[1].max(h[1])]),
                    });
                }
            }
        }
        match extent {
            None => out.missed.push(i),
            Some((l, h)) => {
                let c = land.centre();
                sum[0] += (l[0] + h[0]) * 0.5 - c[0];
                sum[1] += (l[1] + h[1]) * 0.5 - c[1];
                reached += 1;
            }
        }
    }
    if reached > 0 {
        out.shift = [sum[0] / reached as f64, sum[1] / reached as f64];
    }
    out
}

/// The XY box of the part of the triangle over the box `lo..hi` at or below `top`, if any.
fn contact(t: [[f64; 3]; 3], lo: [f64; 2], hi: [f64; 2], top: f64) -> Option<([f64; 2], [f64; 2])> {
    let min = |k: usize| t[0][k].min(t[1][k]).min(t[2][k]);
    let max = |k: usize| t[0][k].max(t[1][k]).max(t[2][k]);
    if min(2) > top || max(0) < lo[0] || min(0) > hi[0] || max(1) < lo[1] || min(1) > hi[1] {
        return None;
    }
    let mut poly = t.to_vec();
    for (axis, limit, below) in [(2, top, true), (0, lo[0], false), (0, hi[0], true), (1, lo[1], false), (1, hi[1], true)] {
        poly = clip(&poly, axis, limit, below);
        if poly.is_empty() {
            return None;
        }
    }
    let first = [poly[0][0], poly[0][1]];
    Some(poly.iter().fold((first, first), |(l, h), p| {
        ([l[0].min(p[0]), l[1].min(p[1])], [h[0].max(p[0]), h[1].max(p[1])])
    }))
}

/// The part of a convex polygon on one side of the plane `p[axis] == limit`.
fn clip(poly: &[[f64; 3]], axis: usize, limit: f64, below: bool) -> Vec<[f64; 3]> {
    let inside = |p: &[f64; 3]| if below { p[axis] <= limit } else { p[axis] >= limit };
    let mut out = Vec::with_capacity(poly.len() + 1);
    for (i, &a) in poly.iter().enumerate() {
        let b = poly[(i + 1) % poly.len()];
        if inside(&a) {
            out.push(a);
        }
        if inside(&a) != inside(&b) {
            let s = (limit - a[axis]) / (b[axis] - a[axis]);
            out.push([0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * s));
        }
    }
    out
}

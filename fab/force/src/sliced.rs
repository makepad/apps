//! A part as the slicer printed it: its walls' and skins' beads, each extrusion a segment
//! with its width across it and its layer's height under it, measured into the lattice's
//! octants (one Gauss point each), so a Gauss point's wall and skin shares, and its walls'
//! bead direction, are the slice's instead of the settings' estimate ([`crate::grid::classify`]).
//! The rest of a filled octant is sparse infill, its stiffness the print's density's as before:
//! an octant (a quarter to half a millimetre) is far smaller than an infill pattern's cell, so
//! the infill's own lines would read as noise, not density.
//!
//! A bead's volume is width x length x layer height (beads side by side tile the layer), cut
//! into pieces no longer, wider or higher than half an octant, each piece shared among the
//! octants its box overlaps by the overlap (cloud in cell; the box is the piece's extent along
//! X, Y and Z, exact for a bead along an axis), so a bead's edge lands where it is at any
//! cell size. An octant overlapping beads overfill keeps their proportions.

use crate::element::V3;
use crate::grid::Lattice;

/// What a bead is to the material model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A wall loop (outer, inner, a thin wall's gap fill): beads along the outline.
    Wall,
    /// Solid layers (top, bottom, internal solid infill, bridges).
    Skin,
}

/// One extrusion segment, printer mm: from `a` to `b` on its layer's top (`a[2]` is the
/// layer's top), `width` across and `height` down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bead {
    pub a: V3,
    pub b: V3,
    pub width: f64,
    pub height: f64,
    pub role: Role,
}

/// Per lattice sample (octant): the volume of walls and of skins in it (mm^3), and its walls'
/// direction as a doubled-angle sum (a bead's direction is a line: theta and theta + pi
/// agree), each piece weighted by its volume.
#[derive(Clone, Debug, Default)]
pub struct Printed {
    pub wall: Vec<f32>,
    pub skin: Vec<f32>,
    pub dir: Vec<[f32; 2]>,
}

impl Printed {
    /// The beads measured into `lattice`'s octants (its samples are their centres).
    pub fn new(lattice: &Lattice, beads: &[Bead]) -> Printed {
        let count = lattice.d.len();
        let mut p = Printed { wall: vec![0.0; count], skin: vec![0.0; count], dir: vec![[0.0; 2]; count] };
        let s = lattice.spacing;
        let step = 0.5 * s;
        // per axis: the octants a piece's extent `c +- half` overlaps (at most two, the piece no
        // wider than an octant) and its share in each
        let span = |a: usize, c: f64, half: f64| -> Option<[(usize, f64); 2]> {
            let n = lattice.n[a] as f64;
            let k = |v: f64| ((v - lattice.origin[a]) / s).round();
            let (k0, k1) = (k(c - half), k(c + half));
            if k1 < 0.0 || k0 >= n {
                return None;
            }
            let w0 = if k0 == k1 { 1.0 } else { ((lattice.origin[a] + (k0 + 0.5) * s - (c - half)) / (2.0 * half)).clamp(0.0, 1.0) };
            let at = |k: f64, w: f64| if k >= 0.0 && k < n { (k as usize, w) } else { (0, 0.0) };
            Some([at(k0, w0), at(k1, if k0 == k1 { 0.0 } else { 1.0 - w0 })])
        };
        for bead in beads {
            let d = [bead.b[0] - bead.a[0], bead.b[1] - bead.a[1]];
            let length = (d[0] * d[0] + d[1] * d[1]).sqrt();
            if length <= 0.0 || bead.width <= 0.0 || bead.height <= 0.0 {
                continue;
            }
            let (along, across, down) = ((length / step).ceil().max(1.0), (bead.width / step).ceil().max(1.0), (bead.height / step).ceil().max(1.0));
            let (dl, dw, dh) = (length / along, bead.width / across, bead.height / down);
            let piece = dl * dw * dh;
            let u = [d[0] / length, d[1] / length];
            let side = [-u[1], u[0]];
            let twice = [u[0] * u[0] - u[1] * u[1], 2.0 * u[0] * u[1]];
            let half = [0.5 * (u[0].abs() * dl + u[1].abs() * dw), 0.5 * (u[1].abs() * dl + u[0].abs() * dw), 0.5 * dh];
            for i in 0..along as usize {
                let t = (i as f64 + 0.5) / along;
                let c = [bead.a[0] + t * d[0], bead.a[1] + t * d[1]];
                for j in 0..across as usize {
                    let w = ((j as f64 + 0.5) / across - 0.5) * bead.width;
                    let (x, y) = (c[0] + w * side[0], c[1] + w * side[1]);
                    let (Some(sx), Some(sy)) = (span(0, x, half[0]), span(1, y, half[1])) else { continue };
                    for k in 0..down as usize {
                        let z = bead.a[2] - (k as f64 + 0.5) * dh;
                        let Some(sz) = span(2, z, half[2]) else { continue };
                        for (kz, wz) in sz {
                            for (ky, wy) in sy {
                                for (kx, wx) in sx {
                                    let v = piece * wx * wy * wz;
                                    if v <= 0.0 {
                                        continue;
                                    }
                                    let o = lattice.index(kx, ky, kz);
                                    match bead.role {
                                        Role::Wall => {
                                            p.wall[o] += v as f32;
                                            p.dir[o][0] += (v * twice[0]) as f32;
                                            p.dir[o][1] += (v * twice[1]) as f32;
                                        }
                                        Role::Skin => p.skin[o] += v as f32,
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        p
    }

    /// Lattice sample `s`'s wall and skin shares of its filled volume (`filled`, mm^3: the
    /// octant's filled share times its volume), together no more than all of it, and its
    /// walls' bead direction (radians), if it has walls.
    pub fn shares(&self, s: usize, filled: f64) -> (f64, f64, Option<f64>) {
        if filled <= 0.0 {
            return (0.0, 0.0, None);
        }
        let (wall, skin) = (self.wall[s] as f64 / filled, self.skin[s] as f64 / filled);
        let over = (wall + skin).max(1.0);
        let (wall, skin) = (wall / over, skin / over);
        let [c, sn] = self.dir[s];
        let dir = (self.wall[s] > 0.0).then(|| 0.5 * (sn as f64).atan2(c as f64));
        (wall, skin, dir)
    }
}

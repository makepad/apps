//! How thick the part is under its surface, against the cells that read it: from points of the
//! surface a ray goes straight in along the normal, and how far it gets before the surface
//! across is the thickness there. Each ray is sphere traced on the part's own distances (a mesh's
//! too), so this sees the part as it is and not as the cells sampled it.

use crate::element::V3;
use crate::mesh::Mesh;
use crate::surface::{dot, field_normals, norm, Surface};

/// Cells through a section under which a study does not resolve it. A beam bent on grids at
/// three alignments, round, square and flat: at 3 cells its stiffness comes within 17 % of the
/// beam's at each of them; at 2 cells up to 34 % off, at 1.2 up to 61 %, and under one cell the
/// grid can lose the section altogether.
pub const RESOLVED_CELLS: f64 = 3.0;

/// How many times too high a bent member's safety factor can read, at [`RESOLVED_CELLS`] cells
/// through it: a cell's stress is the cell's mean, under the outer fibre's. The same beams' worst
/// read, of the sections and alignments.
pub const READS_HIGH_RESOLVED: f64 = 1.6;

/// The same at fewer cells: the worst read at each of these cells through, or more.
const READS_HIGH: [(f64, f64); 4] = [(2.5, 1.8), (2.0, 2.7), (1.5, 3.5), (1.2, 4.4)];

/// How many times too high a bent member's safety factor can read at `cells` through it (under
/// [`RESOLVED_CELLS`]); None under the fewest cells the beams give a usable number for: the cell
/// means read a small part of the stress there.
pub fn reads_high(cells: f64) -> Option<f64> {
    READS_HIGH.iter().find(|r| cells >= r.0).map(|r| r.1)
}

/// The share of the surface under which what is thin is left unsaid: the odd short ray at an
/// edge is no feature, and a part all fine but for a nub reads as what it is.
const WORTH_SAYING: f64 = 0.05;

/// How far up the thin area, thinnest first, its thickness is read. A wall reads the same all
/// over; a strut's sides read its width and its faces its depth, and the sides are the larger
/// share where the width is the less, with the odd short ray at an edge under them all.
const READ_AT: f64 = 0.25;

/// How many points of the surface are measured, at most: a thin feature worth saying has dozens.
const POINTS: usize = 1500;

/// How many steps a ray takes at most; one still inside by then reads as thick.
const ROUNDS: usize = 64;

/// A ray's smallest step, and how close to a triangle counts as on it, in lattice spacings.
const ON: f64 = 0.02;

/// What is thin at some cell size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thin {
    /// How thick what is thin is across its thinner way, mm.
    pub mm: f64,
    /// Its share of the surface.
    pub share: f64,
}

/// The part's thickness at points of its surface.
#[derive(Clone, Debug, Default)]
pub struct Thickness {
    /// Thickness in mm (infinite: the ray met nothing) and surface area, thinnest first.
    points: Vec<(f64, f64)>,
    area: f64,
}

impl Thickness {
    /// `surface`'s thickness in a part given by its distance field (`spacing`: its lattice's).
    pub fn of_field(surface: &Surface, spacing: f64, distance: &mut dyn FnMut(&[V3], &mut [f64])) -> Thickness {
        let (mut at, out, area) = points(surface);
        let mut normal = vec![[0.0; 3]; at.len()];
        let mut d = vec![0.0; at.len()];
        let eps = 1e-3 * spacing;
        // the contour's points onto the field's surface
        for _ in 0..2 {
            field_normals(&mut *distance, eps)(&at, &mut normal);
            distance(&at, &mut d);
            for ((p, n), d) in at.iter_mut().zip(&normal).zip(&d) {
                *p = std::array::from_fn(|a| p[a] - n[a] * d);
            }
        }
        field_normals(&mut *distance, eps)(&at, &mut normal);
        // through the far surface: parts that meet read zero where they meet, inside the part
        Thickness::traced(surface, spacing, &at, &normal, &out, &area, 0.0, distance)
    }

    /// `surface`'s thickness in a part given as triangles (`spacing`: its lattice's).
    pub fn of_mesh(surface: &Surface, spacing: f64, mesh: &Mesh) -> Thickness {
        let (mut at, out, area) = points(surface);
        mesh.onto(&mut at);
        let mut normal = vec![[0.0; 3]; at.len()];
        mesh.normals(&at, &mut normal);
        // every point a ray reaches is inside until it meets the triangles
        let mut inside = |ps: &[V3], d: &mut [f64]| {
            mesh.distances(ps, d);
            d.iter_mut().for_each(|d| *d = -*d);
        };
        Thickness::traced(surface, spacing, &at, &normal, &out, &area, -ON * spacing, &mut inside)
    }

    /// Traces the rays in from `at`: along `normal` (either sign, none at some points), turned
    /// to go against the contour's `out`, each until its distance reads over `end`.
    #[allow(clippy::too_many_arguments)]
    fn traced(surface: &Surface, spacing: f64, at: &[V3], normal: &[V3], out: &[V3], area: &[f64], end: f64, distance: &mut dyn FnMut(&[V3], &mut [f64])) -> Thickness {
        let inward: Vec<V3> = normal.iter().zip(out).map(|(n, o)| match dot(*n, *o) {
            s if s > 0.0 => n.map(|v| -v),
            s if s < 0.0 => *n,
            _ => o.map(|v| -v),
        }).collect();
        let on = ON * spacing;
        let reach = diagonal(surface);
        let mut through = vec![f64::INFINITY; at.len()];
        // past the point's own surface, where its distance reads again
        let mut t = vec![4.0 * on; at.len()];
        let mut going: Vec<usize> = (0..at.len()).collect();
        let (mut ps, mut d) = (Vec::new(), Vec::new());
        for _ in 0..ROUNDS {
            ps.clear();
            ps.extend(going.iter().map(|&i| -> V3 { std::array::from_fn(|a| at[i][a] + inward[i][a] * t[i]) }));
            d.resize(ps.len(), 0.0);
            distance(&ps, &mut d);
            let mut k = 0;
            going.retain(|&i| {
                let here = d[k];
                k += 1;
                if here > end {
                    through[i] = t[i] - here;
                    return false;
                }
                t[i] += (-here).max(on);
                t[i] < reach
            });
            if going.is_empty() {
                break;
            }
        }
        let mut points: Vec<(f64, f64)> = through.into_iter().zip(area.iter().copied()).collect();
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        Thickness { area: area.iter().sum(), points }
    }

    /// What is under [`RESOLVED_CELLS`] cells of `h` through, if enough of the surface is: how
    /// thick it is ([`READ_AT`]), and its share.
    pub fn thin_at(&self, h: f64) -> Option<Thin> {
        let thin = &self.points[..self.points.partition_point(|p| p.0 < RESOLVED_CELLS * h)];
        let area: f64 = thin.iter().map(|p| p.1).sum();
        let share = area / self.area.max(f64::MIN_POSITIVE);
        if share < WORTH_SAYING {
            return None;
        }
        let mut below = 0.0;
        let read = thin.iter().find(|p| {
            below += p.1;
            below >= READ_AT * area
        })?;
        Some(Thin { mm: read.0, share })
    }
}

/// Some quads of `surface`, evenly through them: their centres, outward normals and areas.
fn points(surface: &Surface) -> (Vec<V3>, Vec<V3>, Vec<f64>) {
    let chosen = || (0..surface.quads.len()).step_by(surface.quads.len().div_ceil(POINTS).max(1));
    (
        chosen().map(|q| surface.quad_center[q].map(f64::from)).collect(),
        chosen().map(|q| norm(surface.quad_normal[q].map(f64::from))).collect(),
        chosen().map(|q| surface.quad_area[q] as f64).collect(),
    )
}

/// The surface's box, corner to corner: no ray inside the part goes further.
fn diagonal(surface: &Surface) -> f64 {
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for p in &surface.pos {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a] as f64);
            hi[a] = hi[a].max(p[a] as f64);
        }
    }
    (0..3).map(|a| (hi[a] - lo[a]).max(0.0).powi(2)).sum::<f64>().sqrt()
}

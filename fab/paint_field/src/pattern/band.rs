//! The cells a pattern grows in: a band round the part's surface on a lattice of one pitch
//! (model millimetres), each cell with its six face neighbours, built from the part's signed
//! distance (a mesh's or a field's alike).
//!
//! The distance is read on the coarse lattice, only in bricks the surface can reach (a brick's
//! centre tells, by the distance's Lipschitz bound), a brick's points at once. The fine lattice
//! reads it between those samples, and again at each of its points the samples round it cannot
//! keep out of the band by that bound: a wall thinner than the coarse lattice's cells reads
//! thinner between them than it is, and its middle is no band's. A skin band reaches just past
//! half a cell's diagonal either side of the surface, so it is joined face to face at any slope
//! and a spot is the same through it: the pattern follows the skin. Across a wall thinner than
//! the band (or a gap narrower than it) the cells of its two sides are not neighbours (their
//! slopes turn apart), so a pattern on one side does not grow through to the other. A depth
//! grows it that far in, through the part.
//!
//! Past the band, down to [`SKIN`] under the surface, are the cells the samplers read and
//! nothing steps: each takes the mean of its neighbours nearer the band, so the print's skin
//! and the walls under it show the surface's pattern.

use super::{pack, CellMap, Layout, BRICK};
use crate::bake::Fan;
use crate::program::P3;
use makepad_csg_sdf::Clearance;
use std::sync::{Arc, OnceLock};

/// A part's surface as the band reads it: its signed distance (model mm, negative inside) at
/// many points at once, how far that proves the surface away (a true distance's
/// `Clearance::uniform(1.0)`), its box and what it is (equal keys, one shape).
#[derive(Clone)]
pub struct Surface {
    pub distance: Many,
    pub clearance: Clearance,
    pub bounds: (P3, P3),
    pub key: u64,
}

/// A signed distance read at many points (`out` takes one per point).
pub type Many = Arc<dyn Fn(&[P3], &mut [f64]) + Send + Sync>;

/// A distance read a point at a time, as one read at many.
pub fn each(distance: impl Fn(P3) -> f64 + Send + Sync + 'static) -> Many {
    Arc::new(move |ps: &[P3], out: &mut [f64]| {
        for (p, o) in ps.iter().zip(out.iter_mut()) {
            *o = distance(*p);
        }
    })
}

/// How deep under the surface the samplers read a pattern grown on the skin (mm): past the
/// paint's skin (`fab_paint_skin_depth`, 0.5 mm by default); deeper, the print's inside takes
/// the first material.
pub const SKIN: f64 = 1.0;

/// How far either side of the surface a skin band reaches, in cells: past half a cell's
/// diagonal (0.87), so the band is joined face to face at any slope.
const HALF: f64 = 0.9;

/// The cosine past which two neighbours on one side of the skin, their slopes turned apart
/// (about 105 degrees), lie on a thin wall's two sides.
const ACROSS: f32 = -0.25;

/// How thick a band at `pitch` growing `depth` deep is (mm).
pub fn thickness(pitch: f64, depth: f64) -> f64 {
    (HALF * pitch).max(depth) + HALF * pitch
}

/// The part's distance on the coarse lattice where the band can be: bricks of 8³ samples.
pub struct Distances {
    pub pitch: f64,
    distance: Many,
    lipschitz: f64,
    bricks: CellMap<u32>,
    /// The bricks kept, in order.
    order: Vec<[i32; 3]>,
    d: Vec<f32>,
}

/// The lattice point `j` of brick `b` (8³, x fastest).
fn point(b: [i32; 3], j: usize) -> [i32; 3] {
    [b[0] * BRICK + (j & 7) as i32, b[1] * BRICK + ((j >> 3) & 7) as i32, b[2] * BRICK + (j >> 6) as i32]
}

impl Distances {
    /// The distance at `pitch` in every brick within `reach` (mm) of the surface.
    pub fn new(surface: &Surface, pitch: f64, reach: f64, fan: Fan) -> Distances {
        let edge = BRICK as f64 * pitch;
        let (lo, hi) = surface.bounds;
        let first: [i32; 3] = std::array::from_fn(|k| ((lo[k] - reach) / edge).floor() as i32 - 1);
        let last: [i32; 3] = std::array::from_fn(|k| ((hi[k] + reach) / edge).floor() as i32 + 1);
        let mut candidates = Vec::new();
        for z in first[2]..=last[2] {
            for y in first[1]..=last[1] {
                for x in first[0]..=last[0] {
                    candidates.push([x, y, z]);
                }
            }
        }
        // a brick's points lie within half its diagonal of its centre
        let half = 0.5 * 7.0 * 3f64.sqrt() * pitch;
        // the slope between a lattice point and its neighbours, near the surface
        let lip = surface.clearance.at(3f64.sqrt() * pitch).0;
        // the bricks' centres 512 at a time
        let chunks = candidates.len().div_ceil(512);
        let keep: Vec<OnceLock<Vec<bool>>> = (0..chunks).map(|_| OnceLock::new()).collect();
        fan(chunks, &|i| {
            let part = &candidates[i * 512..((i + 1) * 512).min(candidates.len())];
            let centres: Vec<P3> = part.iter().map(|b| std::array::from_fn(|k| (b[k] as f64 * BRICK as f64 + 3.5) * pitch)).collect();
            let mut d = vec![0.0; centres.len()];
            (surface.distance)(&centres, &mut d);
            let _ = keep[i].set(d.iter().map(|d| d.is_finite() && surface.clearance.radius(*d) <= half + reach).collect());
        });
        let kept: Vec<bool> = keep.into_iter().flat_map(|k| k.into_inner().unwrap_or_default()).collect();
        let order: Vec<[i32; 3]> = candidates.iter().zip(&kept).filter(|(_, k)| **k).map(|(b, _)| *b).collect();
        let samples: Vec<OnceLock<Vec<f32>>> = (0..order.len()).map(|_| OnceLock::new()).collect();
        fan(order.len(), &|i| {
            let b = order[i];
            let points: Vec<P3> = (0..512).map(|j| point(b, j).map(|v| v as f64 * pitch)).collect();
            let mut d = vec![0.0; 512];
            (surface.distance)(&points, &mut d);
            let _ = samples[i].set(d.iter().map(|d| *d as f32).collect());
        });
        let mut bricks = CellMap::default();
        let mut d = Vec::with_capacity(order.len() * 512);
        for (i, (b, s)) in order.iter().zip(samples).enumerate() {
            bricks.insert(pack(*b), i as u32);
            d.extend(s.into_inner().unwrap_or_else(|| vec![f32::NAN; 512]));
        }
        Distances { pitch, distance: surface.distance.clone(), lipschitz: lip, bricks, order, d }
    }

    fn at(&self, c: [i32; 3]) -> Option<f32> {
        let i = self.bricks.get(&pack(c.map(|v| v.div_euclid(BRICK))))?;
        let l = c.map(|v| v.rem_euclid(BRICK));
        let d = self.d[*i as usize * 512 + (l[0] + (l[1] + l[2] * BRICK) * BRICK) as usize];
        d.is_finite().then_some(d)
    }

    /// The distance at `q` (in lattice cells), trilinear, and the least and most it can be by
    /// the Lipschitz bound from each corner; `None` where a corner was not read (far from the
    /// surface).
    fn between(&self, q: [f64; 3]) -> Option<(f64, f64, f64)> {
        let i: [i32; 3] = q.map(|v| v.floor() as i32);
        let f: [f64; 3] = std::array::from_fn(|k| q[k] - i[k] as f64);
        let (mut sum, mut least, mut most) = (0.0, f64::MIN, f64::MAX);
        for corner in 0..8 {
            let o = [corner & 1, (corner >> 1) & 1, corner >> 2];
            let d = self.at([i[0] + o[0], i[1] + o[1], i[2] + o[2]])? as f64;
            let w: f64 = (0..3).map(|k| if o[k] == 1 { f[k] } else { 1.0 - f[k] }).product();
            let off = self.lipschitz * self.pitch * (0..3).map(|k| (o[k] as f64 - f[k]).powi(2)).sum::<f64>().sqrt();
            sum += w * d;
            least = least.max(d - off);
            most = most.min(d + off);
        }
        Some((sum, least, most))
    }

    /// How many coarse lattice points it read.
    pub fn samples(&self) -> usize {
        self.d.len()
    }
}

/// One lattice's cells: the band's (stepped), then the skin's under it (sampled).
pub struct Level {
    pub pitch: f64,
    pub cells: Vec<[i32; 3]>,
    /// How many of `cells` are the band's.
    pub band: usize,
    /// Per band cell its six neighbours (-x +x -y +y -z +z); itself where it has none (past
    /// the band's edge, or across a wall thinner than the band), so no flux crosses there.
    pub nb: Vec<[u32; 6]>,
    /// Per band cell whether it lies outside the surface (its neighbours of the other side are
    /// across the skin from it).
    pub outside: Vec<bool>,
    /// Per band cell the nearest point of the surface (in this lattice's cells): the cell moved
    /// its distance down the distance's slope, so the cells either side of a spot of the skin
    /// share it.
    pub surface: Vec<[f32; 3]>,
    /// Per band cell the way out of the surface there (the distance's slope, a unit vector; 0
    /// where it has none).
    pub normal: Vec<[f32; 3]>,
    /// Per skin cell (in order) the cells nearer the band it takes the mean of, and how many.
    pub skin: Vec<([u32; 6], u8)>,
    index: CellMap<u32>,
    pub layout: Arc<Layout>,
}

const DIRS: [[i32; 3]; 6] = [[-1, 0, 0], [1, 0, 0], [0, -1, 0], [0, 1, 0], [0, 0, -1], [0, 0, 1]];

impl Level {
    /// The cells at `pitch` (the distances' pitch, or half it) a pattern `depth` mm deep grows
    /// in, and the skin under them.
    pub fn new(distances: &Distances, pitch: f64, depth: f64, fan: Fan) -> Level {
        let ratio = (distances.pitch / pitch).round().max(1.0) as i32;
        let w_out = HALF * pitch;
        let w_in = (HALF * pitch).max(depth);
        let reach = w_in.max(SKIN) + pitch;
        let n = BRICK * ratio;
        // per distance brick: its band and skin cells, with their distances
        type Found = (Vec<([i32; 3], f32)>, Vec<([i32; 3], f32)>);
        let found: Vec<OnceLock<Found>> = (0..distances.order.len()).map(|_| OnceLock::new()).collect();
        fan(distances.order.len(), &|i| {
            let b = distances.order[i];
            let (mut band, mut skin) = (Vec::new(), Vec::new());
            let mut sort = |c: [i32; 3], d: f64| {
                if d <= w_out && d >= -w_in {
                    band.push((c, d as f32));
                } else if d < -w_in && d >= -reach {
                    skin.push((c, d as f32));
                }
            };
            // the points the samples round them may put in the band, read again
            let mut again = Vec::new();
            for z in 0..n {
                for y in 0..n {
                    for x in 0..n {
                        let c = [b[0] * n + x, b[1] * n + y, b[2] * n + z];
                        if ratio == 1 {
                            if let Some(d) = distances.at(c) {
                                sort(c, d as f64);
                            }
                            continue;
                        }
                        let Some((d, least, most)) = distances.between(c.map(|v| v as f64 / ratio as f64)) else { continue };
                        if least > w_out {
                            // outside the band
                        } else if most < -w_in {
                            // under it: the skin's, if not too deep
                            sort(c, d.min(most));
                        } else {
                            again.push(c);
                        }
                    }
                }
            }
            if !again.is_empty() {
                let points: Vec<P3> = again.iter().map(|c| c.map(|v| v as f64 * pitch)).collect();
                let mut d = vec![0.0; points.len()];
                (distances.distance)(&points, &mut d);
                for (c, d) in again.into_iter().zip(d) {
                    if d.is_finite() {
                        sort(c, d);
                    }
                }
            }
            let _ = found[i].set((band, skin));
        });
        let mut cells = Vec::new();
        let mut d = Vec::new();
        let mut skin_cells = Vec::new();
        for f in found {
            let (band, skin) = f.into_inner().unwrap_or_default();
            for (c, at) in band {
                cells.push(c);
                d.push(at);
            }
            skin_cells.extend(skin);
        }
        let band = cells.len();
        let mut index: CellMap<u32> = CellMap::default();
        index.reserve(band);
        for (i, c) in cells.iter().enumerate() {
            index.insert(pack(*c), i as u32);
        }
        let mut nb: Vec<[u32; 6]> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| DIRS.map(|d| index.get(&pack([c[0] + d[0], c[1] + d[1], c[2] + d[2]])).copied().unwrap_or(i as u32)))
            .collect();
        let outside: Vec<bool> = d.iter().map(|d| *d > 0.0).collect();
        // the slope from the neighbours either side on each axis (one side where it has one), of
        // the band or the skin under it (where a band one cell thick has no other)
        let mut under: CellMap<f32> = CellMap::default();
        under.reserve(skin_cells.len());
        for (c, at) in &skin_cells {
            under.insert(pack(*c), *at);
        }
        let at = |c: [i32; 3]| index.get(&pack(c)).map(|j| d[*j as usize]).or_else(|| under.get(&pack(c)).copied());
        let slopes: Vec<[f32; 3]> = (0..band)
            .map(|i| {
                std::array::from_fn(|k| {
                    let (mut lo, mut hi) = (cells[i], cells[i]);
                    lo[k] -= 1;
                    hi[k] += 1;
                    match (at(lo), at(hi)) {
                        (Some(a), Some(b)) => 0.5 * (b - a),
                        (Some(a), None) => d[i] - a,
                        (None, Some(b)) => b - d[i],
                        (None, None) => 0.0,
                    }
                })
            })
            .collect();
        let length = |g: &[f32; 3]| g.iter().map(|g| g * g).sum::<f32>().sqrt();
        let surface = (0..band)
            .map(|i| {
                let l = length(&slopes[i]);
                let by = if l > 0.0 { d[i] / pitch as f32 / l } else { 0.0 };
                std::array::from_fn(|k| cells[i][k] as f32 - by * slopes[i][k])
            })
            .collect();
        let unit = |g: [f32; 3]| {
            let l = length(&g);
            if l > 0.0 { g.map(|v| v / l) } else { [0.0; 3] }
        };
        let mut normal: Vec<[f32; 3]> = slopes.iter().map(|g| unit(*g)).collect();
        // a cell on an edge has the same distance as its neighbours either way: the mean of
        // theirs, twice (a corner's from its edges')
        for _ in 0..2 {
            let flat: Vec<(usize, [f32; 3])> = (0..band)
                .filter(|&i| normal[i] == [0.0; 3])
                .map(|i| (i, unit(nb[i].iter().fold([0.0; 3], |m, &j| std::array::from_fn(|k| m[k] + normal[j as usize][k])))))
                .collect();
            for (i, n) in flat {
                normal[i] = n;
            }
        }
        // a wall (or a gap) thinner than the band joins the bands of its two sides: neighbours on
        // one side of the skin whose slopes turn apart past [`ACROSS`] lie on its two sides, and
        // are not each other's (round a sharp edge, the cells on the edge's other side of the
        // skin still join)
        for i in 0..band {
            for k in 0..6 {
                let j = nb[i][k] as usize;
                let (a, b) = (&slopes[i], &slopes[j]);
                if j != i && outside[i] == outside[j] && a[0] * b[0] + a[1] * b[1] + a[2] * b[2] < ACROSS * length(a) * length(b) {
                    nb[i][k] = i as u32;
                }
            }
        }
        // the skin from the band down, each cell after the ones it averages
        skin_cells.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut placed: CellMap<u32> = CellMap::default();
        let mut skin = Vec::with_capacity(skin_cells.len());
        for (c, _) in &skin_cells {
            let mut parents = [0u32; 6];
            let mut count = 0u8;
            for d in DIRS {
                let k = pack([c[0] + d[0], c[1] + d[1], c[2] + d[2]]);
                if let Some(j) = index.get(&k).or_else(|| placed.get(&k)) {
                    parents[count as usize] = *j;
                    count += 1;
                }
            }
            placed.insert(pack(*c), (cells.len()) as u32);
            cells.push(*c);
            skin.push((parents, count));
        }
        let layout = Arc::new(Layout::new(pitch, &cells));
        Level { pitch, cells, band, nb, outside, surface, normal, skin, index, layout }
    }

    /// The band cell at lattice point `c`.
    pub fn cell_index(&self, c: [i32; 3]) -> Option<usize> {
        self.index.get(&pack(c)).map(|i| *i as usize)
    }

    /// `values` (one per band cell) at `q` (in this lattice's cells): trilinear over the band
    /// cells round it; `None` where it has none.
    pub fn between(&self, values: &[f32], q: [f64; 3]) -> Option<f32> {
        let i: [i32; 3] = q.map(|v| v.floor() as i32);
        let f: [f64; 3] = std::array::from_fn(|k| q[k] - i[k] as f64);
        let (mut sum, mut weight) = (0.0, 0.0);
        for corner in 0..8 {
            let o = [corner & 1, (corner >> 1) & 1, corner >> 2];
            let Some(j) = self.index.get(&pack([i[0] + o[0], i[1] + o[1], i[2] + o[2]])) else { continue };
            let w: f64 = (0..3).map(|k| if o[k] == 1 { f[k] } else { 1.0 - f[k] }).product();
            sum += w * values[*j as usize] as f64;
            weight += w;
        }
        (weight > 1e-9).then(|| (sum / weight) as f32)
    }

    /// Every cell's value (band then skin) from the band's: each skin cell the mean of its
    /// parents (NaN with none).
    pub fn with_skin(&self, band: Vec<f32>) -> Vec<f32> {
        let mut all = band;
        all.reserve(self.skin.len());
        for (parents, count) in &self.skin {
            let (sum, n) = parents[..*count as usize].iter().map(|j| all[*j as usize]).filter(|x| !x.is_nan()).fold((0.0, 0u32), |(s, n), x| (s + x, n + 1));
            let v = if n == 0 { f32::NAN } else { sum / n as f32 };
            all.push(v);
        }
        all
    }
}

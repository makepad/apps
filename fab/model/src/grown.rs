//! A grown part's settled shape, kept beside its script (`<script>.<part>.grow`), so a project opens
//! showing the shape it was saved with, with no AI hub and no growing. Only what a part settled at
//! is kept. Its surface comes from the density the same way it did when it grew.

use makepad_csg_sdf::SdfGrid;
use std::sync::Arc;

const MAGIC: &[u8; 4] = b"GROW";
/// Another version is not read: the part grows again.
const VERSION: u32 = 1;
const HEADER: usize = 4 + 4 + 8 + 8 + 4 + 4 + 12 + 12 + 8 + 8;

/// One settled shape, as its file holds it.
#[derive(Clone, Debug)]
pub struct Stored {
    /// What it answers ([`inputs_hash`]): another answer is grown.
    pub inputs: u64,
    /// What it grows in: in another, this shape says nothing of the part.
    pub space: u64,
    /// The level it settled at, and its cells' size in mm.
    pub level: u32,
    pub h: f32,
    pub bytes: Arc<[u8]>,
}

/// What a shape answers: what it grows in, its loads and its mass. Loads and mass are compared as a
/// file says them, so what a turn on the plate leaves in their last digits changes nothing.
pub fn inputs_hash(space: u64, loads: &impl std::fmt::Debug, mass: f64) -> u64 {
    format!("{space}|{loads:.4?}|{mass:.6}").bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

impl Stored {
    /// The projected density `xp` of a shape on a grid of `n` cells of `h` mm, the first centred at
    /// `origin`, of which the densest `solid` cells are the part. It is kept as it is, so the part
    /// opens with the very surface it settled at.
    #[allow(clippy::too_many_arguments)]
    pub fn new(inputs: u64, space: u64, level: u32, n: [u32; 3], h: f32, origin: [f32; 3], solid: f64, compliance: f64, xp: &[f32]) -> Stored {
        let density: Vec<u8> = xp.iter().flat_map(|d| d.to_le_bytes()).collect();
        let mut bytes = Vec::with_capacity(HEADER);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&inputs.to_le_bytes());
        bytes.extend_from_slice(&space.to_le_bytes());
        bytes.extend_from_slice(&level.to_le_bytes());
        bytes.extend_from_slice(&h.to_le_bytes());
        for c in n {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        for c in origin {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        bytes.extend_from_slice(&solid.to_le_bytes());
        bytes.extend_from_slice(&compliance.to_le_bytes());
        bytes.extend_from_slice(&makepad_fast_inflate::zlib_compress(&density, 6));
        Stored { inputs, space, level, h, bytes: bytes.into() }
    }

    /// A file's shape and the surface it makes.
    pub fn read(bytes: &[u8]) -> Result<(Stored, SdfGrid), String> {
        if bytes.len() < HEADER || &bytes[..4] != MAGIC {
            return Err("not a grown shape".into());
        }
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"));
        let real = |at: usize| f32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"));
        let long = |at: usize| f64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"));
        if word(4) != VERSION {
            return Err(format!("format {} of a grown shape, this build reads {VERSION}", word(4)));
        }
        let inputs = u64::from_le_bytes(bytes[8..16].try_into().expect("eight bytes"));
        let space = u64::from_le_bytes(bytes[16..24].try_into().expect("eight bytes"));
        let (level, h) = (word(24), real(28));
        let n = [word(32), word(36), word(40)];
        let origin = [real(44), real(48), real(52)];
        let solid = long(56);
        let inflated = makepad_fast_inflate::zlib_decompress_vec(&bytes[HEADER..]).map_err(|e| format!("a grown shape's cells: {e:?}"))?;
        let cells = n.iter().try_fold(1usize, |all, c| all.checked_mul(*c as usize));
        if cells.and_then(|c| c.checked_mul(4)) != Some(inflated.len()) || cells == Some(0) || !(h > 0.0) {
            return Err(format!("a grown shape of {} bytes on a grid of {n:?}", inflated.len()));
        }
        let density: Vec<f32> = inflated.chunks_exact(4).map(|d| f32::from_le_bytes(d.try_into().expect("four bytes"))).collect();
        let data = distance_of(n.map(|c| c as usize), h as f64, &density, solid);
        Ok((Stored { inputs, space, level, h, bytes: bytes.into() }, SdfGrid::new(origin, h, n, 0.0, data.into())))
    }
}

/// The file beside script `file` that keeps its part `part`'s shape.
pub fn file_name(file: &str, part: &str) -> String {
    let stem = file.split('.').next().unwrap_or(file);
    format!("{stem}.{part}.grow")
}

/// The part a file beside script `file` keeps the shape of, if it is one.
pub fn part_of(file: &str, name: &str) -> Option<String> {
    let stem = file.split('.').next().unwrap_or(file);
    let part = name.strip_prefix(stem)?.strip_prefix('.')?.strip_suffix(".grow")?;
    (!part.is_empty() && !part.contains('.')).then(|| part.to_string())
}

/// The distance (mm, negative inside) to the surface of a density's `solid` densest cells, at the
/// cells' centres. The density is smoothed a cell's width so the surface does not show its cells.
/// Cells beside it find it along the axes; the others take their nearest neighbour's nearest point,
/// sweeping the grid once each way.
pub fn distance_of(n: [usize; 3], h: f64, density: &[f32], solid: f64) -> Vec<f32> {
    let cells = n[0] * n[1] * n[2];
    assert_eq!(density.len(), cells);
    let index = |i: usize, j: usize, k: usize| i + n[0] * (j + n[1] * k);
    let far = [(n[0] + n[1] + n[2]) as f32 * h as f32; 3];

    // 1 2 1 along each axis, an edge repeating
    let mut phi = density.to_vec();
    let mut next = vec![0f32; cells];
    let strides = [1, n[0], n[0] * n[1]];
    for a in 0..3 {
        for k in 0..n[2] {
            for j in 0..n[1] {
                for i in 0..n[0] {
                    let (c, at) = (index(i, j, k), [i, j, k][a]);
                    let below = if at > 0 { phi[c - strides[a]] } else { phi[c] };
                    let above = if at + 1 < n[a] { phi[c + strides[a]] } else { phi[c] };
                    next[c] = 0.25 * (below + above) + 0.5 * phi[c];
                }
            }
        }
        std::mem::swap(&mut phi, &mut next);
    }

    // the density at the surface: that of the last cell that is solid
    let mut sorted = phi.clone();
    let last = cells - (solid.round() as usize).clamp(1, cells);
    let (_, &mut iso, _) = sorted.select_nth_unstable_by(last, f32::total_cmp);

    // The vector to the nearest crossing, from each cell beside one: where the density crosses the
    // surface's along each axis (the nearer way) is where a plane cuts that axis, and the plane's
    // foot from the cell is the vector.
    let mut to = vec![far; cells];
    // a cell with no density is never solid, though fewer than `solid` cells have any
    let inside = |v: f32| v >= iso && v > 0.0;
    for k in 0..n[2] {
        for j in 0..n[1] {
            for i in 0..n[0] {
                let c = index(i, j, k);
                let at = [i, j, k];
                // per axis, 1 / the signed way to the crossing, in mm
                let mut cuts = [0f32; 3];
                let mut crossed = false;
                for a in 0..3 {
                    let mut way = f32::MAX;
                    if at[a] > 0 && inside(phi[c - strides[a]]) != inside(phi[c]) {
                        way = -(iso - phi[c]) / (phi[c - strides[a]] - phi[c]);
                    }
                    if at[a] + 1 < n[a] && inside(phi[c + strides[a]]) != inside(phi[c]) {
                        let up = (iso - phi[c]) / (phi[c + strides[a]] - phi[c]);
                        if up < way.abs() {
                            way = up;
                        }
                    }
                    if way != f32::MAX {
                        crossed = true;
                        cuts[a] = 1.0 / (way * h as f32);
                    }
                }
                if crossed {
                    let sum = cuts[0] * cuts[0] + cuts[1] * cuts[1] + cuts[2] * cuts[2];
                    // on the crossing itself the cuts are infinite, and the cell is the foot
                    to[c] = if sum.is_finite() { cuts.map(|u| u / sum) } else { [0.0; 3] };
                }
            }
        }
    }

    // The 13 neighbours a cell has before it in the grid's order: those sweep forward, and the
    // same taken the other way sweep back.
    let mut before = Vec::new();
    for dk in -1isize..=0 {
        for dj in -1isize..=1 {
            for di in -1isize..=1 {
                if dk < 0 || dj < 0 || (dj == 0 && di < 0) {
                    before.push([di, dj, dk]);
                }
            }
        }
    }
    let mut relax = |i: usize, j: usize, k: usize, sign: isize| {
        let c = index(i, j, k);
        let mut best = to[c];
        let mut len = best[0] * best[0] + best[1] * best[1] + best[2] * best[2];
        for d in &before {
            let d = [d[0] * sign, d[1] * sign, d[2] * sign];
            let q = [i as isize + d[0], j as isize + d[1], k as isize + d[2]];
            if (0..3).any(|a| q[a] < 0 || q[a] >= n[a] as isize) {
                continue;
            }
            let o = to[index(q[0] as usize, q[1] as usize, q[2] as usize)];
            let v = [o[0] + d[0] as f32 * h as f32, o[1] + d[1] as f32 * h as f32, o[2] + d[2] as f32 * h as f32];
            let l = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
            if l < len {
                best = v;
                len = l;
            }
        }
        to[c] = best;
    };
    for k in 0..n[2] {
        for j in 0..n[1] {
            for i in 0..n[0] {
                relax(i, j, k, 1);
            }
        }
    }
    for k in (0..n[2]).rev() {
        for j in (0..n[1]).rev() {
            for i in (0..n[0]).rev() {
                relax(i, j, k, -1);
            }
        }
    }
    to.iter().zip(&phi).map(|(v, p)| {
        let d = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if inside(*p) { -d } else { d }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ball's density: stored and read, the same surface to the bit as it made when it grew; and
    /// it takes under half a byte a cell.
    #[test]
    fn a_stored_shape_reads_back_the_same_surface() {
        let n = [32usize; 3];
        let (h, r) = (1.5f32, 14.0f32);
        let mut xp = vec![0f32; 32 * 32 * 32];
        for k in 0..32 {
            for j in 0..32 {
                for i in 0..32 {
                    let c = [i, j, k].map(|a| (a as f32 + 0.5 - 16.0) * h);
                    let d = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt() - r;
                    xp[i + 32 * (j + 32 * k)] = (0.5 - d / h).clamp(0.0, 1.0);
                }
            }
        }
        let solid = xp.iter().map(|d| *d as f64).sum::<f64>();
        let direct = distance_of(n, h as f64, &xp, solid);
        let stored = Stored::new(7, 8, 3, [32; 3], h, [-23.25; 3], solid, 0.5, &xp);
        assert!(stored.bytes.len() < 32 * 32 * 32 / 2, "{} bytes", stored.bytes.len());
        let (read, grid) = Stored::read(&stored.bytes).expect("it reads");
        assert_eq!((read.inputs, read.space, read.level, read.h), (7, 8, 3, h));
        assert_eq!((grid.origin, grid.dims), ([-23.25; 3], [32; 3]));
        assert_eq!(&*grid.data, &direct[..], "the very surface it settled at");
    }

    /// What is damaged or from another format is not read, whichever part of it.
    #[test]
    fn a_damaged_file_is_not_read() {
        let stored = Stored::new(7, 8, 3, [4; 3], 1.0, [0.0; 3], 8.0, 0.5, &[1.0; 64]);
        assert!(Stored::read(&stored.bytes).is_ok());
        assert!(Stored::read(b"GROW").is_err());
        let mut other = stored.bytes.to_vec();
        other[4] = 9;
        assert!(Stored::read(&other).is_err());
        let mut torn = stored.bytes.to_vec();
        torn.truncate(torn.len() - 3);
        assert!(Stored::read(&torn).is_err());
        let wrong = Stored::new(7, 8, 3, [5; 3], 1.0, [0.0; 3], 8.0, 0.5, &[1.0; 64]);
        assert!(Stored::read(&wrong.bytes).is_err());
    }

    #[test]
    fn its_file_is_named_from_script_and_part() {
        assert_eq!(file_name("bracket.cad.splash", "frame"), "bracket.frame.grow");
        assert_eq!(part_of("bracket.cad.splash", "bracket.frame.grow").as_deref(), Some("frame"));
        assert_eq!(part_of("bracket.cad.splash", "other.frame.grow"), None);
        assert_eq!(part_of("bracket.cad.splash", "bracket.cad.splash"), None);
    }

    /// Digits past what a file says of a load change nothing; a load does.
    #[test]
    fn what_it_answers_ignores_the_last_digits() {
        let a = inputs_hash(1, &vec![[1.0f64, 2.0]], 0.3);
        assert_eq!(a, inputs_hash(1, &vec![[1.0 + 1e-9, 2.0 - 1e-9]], 0.3 + 1e-9));
        assert_ne!(a, inputs_hash(1, &vec![[1.5, 2.0]], 0.3));
        assert_ne!(a, inputs_hash(2, &vec![[1.0, 2.0]], 0.3));
        assert_ne!(a, inputs_hash(1, &vec![[1.0, 2.0]], 0.31));
    }


    /// A sphere's density gives the sphere's distance, within what cells of it can tell: under
    /// half a cell or so by the surface and a little more anywhere, a tenth of a cell off on
    /// average, and it never climbs much faster than distance does.
    #[test]
    fn a_density_gives_a_distance() {
        let (n, h, r) = ([32usize; 3], 1.0f64, 9.0f64);
        let centre = |i: usize| i as f64 + 0.5 - 16.0;
        let at = |i: usize, j: usize, k: usize| i + 32 * (j + 32 * k);
        let mut density = vec![0f32; 32 * 32 * 32];
        for k in 0..32 {
            for j in 0..32 {
                for i in 0..32 {
                    let d = (centre(i).powi(2) + centre(j).powi(2) + centre(k).powi(2)).sqrt() - r;
                    density[at(i, j, k)] = (0.5 - d).clamp(0.0, 1.0) as f32;
                }
            }
        }
        let solid = 4.0 / 3.0 * std::f64::consts::PI * r.powi(3);
        let got = distance_of(n, h, &density, solid);
        let (mut near, mut any, mut steep, mut sum, mut count) = (0f64, 0f64, 0f64, 0f64, 0f64);
        for k in 0..32 {
            for j in 0..32 {
                for i in 0..32 {
                    let d = (centre(i).powi(2) + centre(j).powi(2) + centre(k).powi(2)).sqrt() - r;
                    let g = got[at(i, j, k)] as f64;
                    any = any.max((g - d).abs());
                    if d.abs() < 2.0 {
                        near = near.max((g - d).abs());
                        sum += (g - d).abs();
                        count += 1.0;
                    }
                    if i > 0 {
                        steep = steep.max((g - got[at(i - 1, j, k)] as f64).abs() / h);
                    }
                }
            }
        }
        let mean = sum / count;
        assert!(near < 0.6, "by the surface: {near}");
        assert!(mean < 0.12, "on average by the surface: {mean}");
        assert!(any < 0.7, "anywhere: {any}");
        assert!(steep < 1.2, "slope {steep}");
    }

    /// Fewer cells with any density than it should have: those, not the whole grid.
    #[test]
    fn a_density_short_of_its_solid_is_not_everything() {
        let mut density = vec![0f32; 512];
        density[0] = 1.0;
        let got = distance_of([8, 8, 8], 2.0, &density, 100.0);
        // the cell and the seven beside it that the smoothing reaches, in the grid's corner
        assert_eq!(got.iter().filter(|d| **d < 0.0).count(), 8);
    }

    /// A density that says nothing yet leaves the whole space.
    #[test]
    fn an_even_density_is_inside_everywhere() {
        let got = distance_of([8, 8, 8], 2.0, &vec![0.1; 512], 100.0);
        assert!(got.iter().all(|d| *d < 0.0));
    }
}

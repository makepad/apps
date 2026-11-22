//! The see-through view's grid (the plate view's `viewport::see_through`): the paint as a
//! hologram through a part's box, and how far inside the part's surface each point is, in
//! bricks a GPU filters alone.
//!
//! Lattice: sample `g` sits at `g * cell` in the paint's frame (the part's model frame, mm).
//! Brick `b` spans cells `8b .. 8b + 8` per axis and holds the 9³ samples on their corners: a
//! face's samples are its neighbour's too, so a filtered read anywhere in a brick reads that
//! brick alone. A sample is two things. Its colour texel: the material the paint gives there
//! with no part holding it ([`Program::unheld`]: a mix's field reaches past the part, a grown
//! pattern through its band), as the square root of its linear colour so a dark one keeps its
//! steps, and in alpha how much the paint is there. And its occupancy byte: 0 outside, 255 deep
//! inside, the surface at a half, the signed distance ramped over [`RAMP`] cells either side.
//!
//! A brick the surface is farther from than its corners (by the distance's Lipschitz bound) is
//! outside or inside whole; one of those whose samples all hold one colour is whole too: air
//! where no paint reaches, a glow of one colour, or solid. Only the rest keep samples.
//!
//! The shape's side of its surface is baked once per shape and pitch ([`Shape`]); the colours go
//! over it again whenever the paint changes ([`Grid::colour`]), each state of a growing pattern.
//! Both run on the caller's workers.

use crate::bake::Fan;
use crate::program::{Channel, Program, Unheld, P3};
use makepad_csg_sdf::Clearance;
use std::sync::OnceLock;

/// Samples per brick edge: the corners of its 8 cells.
pub const EDGE: usize = 9;
/// Samples a brick holds.
pub const SAMPLES: usize = EDGE * EDGE * EDGE;
/// Cells per brick edge.
pub const BS: i32 = 8;
/// The occupancy's ramp either side of the surface (cells): a sample farther in or out holds
/// 1 or 0, so a read is the distance within a cell less than it.
pub const RAMP: f64 = 2.0;
/// The finest lattice pitch (mm): the paint's own.
pub const PITCH: f64 = 0.4;
/// At most this many bricks over a part's box (each paint sample is read): a coarser lattice
/// past it.
pub const MAX_BRICKS: usize = 8192;
/// The distance is read every this many cells across a brick the surface passes, and between.
const READ_EVERY: usize = 2;

/// The shape's side of its surface over one brick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Out,
    In,
    /// Its samples' occupancy, at this brick of [`Shape::occ`].
    Edge(u32),
}

/// A part's inside on a lattice: per brick outside, inside, or where its surface passes the
/// occupancy of its samples.
pub struct Shape {
    pub cell: f64,
    /// The first brick, and the bricks per axis.
    pub min: [i32; 3],
    pub dim: [i32; 3],
    sides: Vec<Side>,
    /// [`SAMPLES`] per brick the surface passes, x fastest: 0 outside .. 255 inside.
    occ: Vec<u8>,
}

/// A solid's signed distance (negative inside, the paint frame's mm) at many points at once, into
/// `out`: the points of one brick, nearby ones in a row, so a reader can share work between them
/// (a mesh, the side of its surface).
pub type Distances<'a> = dyn Fn(&[P3], &mut [f64]) + Sync + 'a;

impl Shape {
    /// The side of `distance` (its `clearance` how far its values prove its surface away) over
    /// `bounds` grown by `pad` (mm), at `pitch` or coarser where the box would hold more than
    /// [`MAX_BRICKS`]. Nothing when `fan`
    /// skipped a brick.
    pub fn bake(distance: &Distances, clearance: Clearance, bounds: (P3, P3), pad: f64, pitch: f64, fan: Fan) -> Option<Shape> {
        let cell = Self::cell_for(bounds, pad, pitch);
        let (min, dim) = lattice(bounds, pad, cell);
        let n = (dim[0] * dim[1] * dim[2]) as usize;
        let out: Vec<OnceLock<(bool, Option<Vec<u8>>)>> = (0..n).map(|_| OnceLock::new()).collect();
        fan(n, &|i| {
            let _ = out[i].set(side(distance, &clearance, cell, brick_at(min, dim, i)));
        });
        let sides: Vec<(bool, Option<Vec<u8>>)> = out.into_iter().map(OnceLock::into_inner).collect::<Option<_>>()?;
        let edges = sides.iter().filter(|(_, s)| s.is_some()).count();
        let mut shape = Shape { cell, min, dim, sides: Vec::with_capacity(n), occ: Vec::with_capacity(edges * SAMPLES) };
        for (inside, s) in sides {
            let side = match s {
                Some(s) => {
                    shape.occ.extend(s);
                    Side::Edge((shape.occ.len() / SAMPLES - 1) as u32)
                }
                None if inside => Side::In,
                None => Side::Out,
            };
            shape.sides.push(side);
        }
        Some(shape)
    }

    /// The lattice pitch [`Shape::bake`] takes for `bounds` grown by `pad`: `pitch`, or twice
    /// it until the box holds at most [`MAX_BRICKS`].
    pub fn cell_for(bounds: (P3, P3), pad: f64, pitch: f64) -> f64 {
        let mut cell = pitch;
        while {
            let (_, dim) = lattice(bounds, pad, cell);
            (dim[0] * dim[1] * dim[2]) as usize > MAX_BRICKS
        } {
            cell *= 2.0;
        }
        cell
    }

    /// Bricks its surface passes.
    pub fn edges(&self) -> usize {
        self.occ.len() / SAMPLES
    }

    /// Bricks over its box.
    pub fn bricks(&self) -> usize {
        self.sides.len()
    }
}

/// The bricks over `bounds` grown by `pad` (at least the ramp and a cell) at `cell`: the first,
/// and how many per axis.
fn lattice(bounds: (P3, P3), pad: f64, cell: f64) -> ([i32; 3], [i32; 3]) {
    let pad = pad.max((RAMP + 1.0) * cell);
    let edge = BS as f64 * cell;
    let min: [i32; 3] = std::array::from_fn(|k| ((bounds.0[k] - pad) / edge).floor() as i32);
    let max: [i32; 3] = std::array::from_fn(|k| ((bounds.1[k] + pad) / edge).floor() as i32);
    (min, std::array::from_fn(|k| max[k] - min[k] + 1))
}

/// Brick `i` of a lattice (x fastest).
fn brick_at(min: [i32; 3], dim: [i32; 3], i: usize) -> [i32; 3] {
    let i = i as i32;
    [min[0] + i % dim[0], min[1] + (i / dim[0]) % dim[1], min[2] + i / (dim[0] * dim[1])]
}

/// Sample `j` of a brick (x fastest) as its lattice offset.
fn sample_at(j: usize) -> [usize; 3] {
    [j % EDGE, (j / EDGE) % EDGE, j / (EDGE * EDGE)]
}

/// Brick `b`: whole (inside or not), or its samples' occupancy where the surface can pass.
fn side(distance: &Distances, clearance: &Clearance, cell: f64, b: [i32; 3]) -> (bool, Option<Vec<u8>>) {
    let ramp = RAMP * cell;
    let half = 0.5 * BS as f64 * cell * 3f64.sqrt();
    let at = |o: [f64; 3]| -> P3 { std::array::from_fn(|k| (b[k] as f64 * BS as f64 + o[k]) * cell) };
    let mut centre = [0f64];
    distance(&[at([0.5 * BS as f64; 3])], &mut centre);
    let d = centre[0];
    // past the ramp all over the brick
    if clearance.radius_past(d, ramp) > half {
        return (d < 0.0, None);
    }
    // the distance on every other sample, the rest between them
    const N: usize = EDGE / READ_EVERY + 1;
    let points: [P3; N * N * N] = std::array::from_fn(|r| at([r % N, (r / N) % N, r / (N * N)].map(|x| (x * READ_EVERY) as f64)));
    let mut read = [0f64; N * N * N];
    distance(&points, &mut read);
    let coarse = |x: usize, y: usize, z: usize| read[(z * N + y) * N + x];
    let mut occ = vec![0u8; SAMPLES];
    for (j, o) in occ.iter_mut().enumerate() {
        let s = sample_at(j);
        let (i, f): ([usize; 3], [f64; 3]) = (s.map(|v| (v / READ_EVERY).min(N - 2)), s.map(|v| (v as f64 / READ_EVERY as f64) - ((v / READ_EVERY).min(N - 2)) as f64));
        let mut d = 0.0;
        for c in 0..8 {
            let k = [c & 1, (c >> 1) & 1, c >> 2];
            let w: f64 = (0..3).map(|a| if k[a] == 1 { f[a] } else { 1.0 - f[a] }).product();
            d += w * coarse(i[0] + k[0], i[1] + k[1], i[2] + k[2]);
        }
        *o = ((0.5 - d / (2.0 * ramp)).clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    let first = occ[0];
    if (first == 0 || first == 255) && occ.iter().all(|v| *v == first) {
        return (first == 255, None);
    }
    (d < 0.0, Some(occ))
}

/// A brick of the grid as the GPU reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// Outside the part, and no paint reaches it.
    Air,
    /// Inside the part in one colour (a texel's).
    Solid(u32),
    /// Outside the part, the paint all through it in one colour (a texel's).
    Glow(u32),
    /// Its samples: [`SAMPLES`] texels and occupancies at this tile of [`Grid::texels`] and
    /// [`Grid::occ`].
    Tile(u32),
}

/// A part's see-through grid: its paint over its box, and its shape's inside.
pub struct Grid {
    pub cell: f64,
    pub min: [i32; 3],
    pub dim: [i32; 3],
    /// Per brick, x fastest.
    pub entries: Vec<Entry>,
    /// [`SAMPLES`] colour texels per tile, x fastest.
    pub texels: Vec<u32>,
    /// [`SAMPLES`] occupancies per tile, x fastest.
    pub occ: Vec<u8>,
}

/// A texel of a linear colour and how much paint is there: RGBA bytes, red in bits 16..24.
pub fn texel(rgb: [f32; 3], presence: f32) -> u32 {
    let c = rgb.map(|v| (v.clamp(0.0, 1.0).sqrt() * 255.0).round() as u32);
    ((presence.clamp(0.0, 1.0) * 255.0).round() as u32) << 24 | c[0] << 16 | c[1] << 8 | c[2]
}

/// A texel's linear colour and how much paint is there (0..1).
pub fn untexel(t: u32) -> ([f32; 3], f32) {
    let c = |shift: u32| {
        let v = ((t >> shift) & 255) as f32 / 255.0;
        v * v
    };
    ([c(16), c(8), c(0)], (t >> 24) as f32 / 255.0)
}

/// What a sample reads: its colour, how much paint is there, its occupancy (all 0..1).
pub type Read = ([f32; 3], f32, f32);

impl Grid {
    /// `shape`'s box in the colours of `paint` (in the paint's frame; slot `s` in `palette[s]`,
    /// the part's `own` colour where nothing paints it). Nothing when `fan` skipped a brick.
    pub fn colour(shape: &Shape, paint: Option<&Program>, palette: &[[f32; 3]], own: [f32; 3], fan: Fan) -> Option<Grid> {
        let colours: Vec<[f32; 3]> = (0..256usize).map(|s| if s == 0 { own } else { palette.get(s).copied().unwrap_or(own) }).collect();
        let n = shape.sides.len();
        let out: Vec<OnceLock<Brick>> = (0..n).map(|_| OnceLock::new()).collect();
        fan(n, &|i| {
            let _ = out[i].set(brick_colour(shape, i, paint, &colours));
        });
        let bricks: Vec<Brick> = out.into_iter().map(OnceLock::into_inner).collect::<Option<_>>()?;
        let tiles = bricks.iter().filter(|(_, t)| t.is_some()).count();
        let mut grid = Grid { cell: shape.cell, min: shape.min, dim: shape.dim, entries: Vec::with_capacity(n), texels: Vec::with_capacity(tiles * SAMPLES), occ: Vec::with_capacity(tiles * SAMPLES) };
        for (e, t) in bricks {
            match t {
                Some((texels, occ)) => {
                    grid.entries.push(Entry::Tile((grid.texels.len() / SAMPLES) as u32));
                    grid.texels.extend(texels);
                    grid.occ.extend(occ);
                }
                None => grid.entries.push(e),
            }
        }
        Some(grid)
    }

    /// Tile `t`'s colour texels.
    pub fn tile(&self, t: u32) -> &[u32] {
        &self.texels[t as usize * SAMPLES..(t as usize + 1) * SAMPLES]
    }

    /// Tile `t`'s occupancies.
    pub fn tile_occ(&self, t: u32) -> &[u8] {
        &self.occ[t as usize * SAMPLES..(t as usize + 1) * SAMPLES]
    }

    pub fn tiles(&self) -> usize {
        self.texels.len() / SAMPLES
    }

    /// What `p` (the paint frame, mm) reads, filtered as the view reads it: nothing past the
    /// grid.
    pub fn at(&self, p: P3) -> Read {
        let g: [f64; 3] = p.map(|v| v / self.cell);
        let b: [i32; 3] = std::array::from_fn(|k| (g[k] / BS as f64).floor() as i32 - self.min[k]);
        if (0..3).any(|k| b[k] < 0 || b[k] >= self.dim[k]) {
            return ([0.0; 3], 0.0, 0.0);
        }
        match self.entries[((b[2] * self.dim[1] + b[1]) * self.dim[0] + b[0]) as usize] {
            Entry::Air => ([0.0; 3], 0.0, 0.0),
            Entry::Solid(t) => (untexel(t).0, untexel(t).1, 1.0),
            Entry::Glow(t) => (untexel(t).0, 1.0, 0.0),
            Entry::Tile(t) => {
                let (tile, occ) = (self.tile(t), self.tile_occ(t));
                let l: [f64; 3] = std::array::from_fn(|k| (g[k] - ((b[k] + self.min[k]) * BS) as f64).clamp(0.0, BS as f64));
                let i: [usize; 3] = l.map(|v| (v.floor() as usize).min(EDGE - 2));
                let f: [f64; 3] = std::array::from_fn(|k| l[k] - i[k] as f64);
                let (mut rgb, mut presence, mut inside) = ([0f32; 3], 0f32, 0f32);
                for c in 0..8 {
                    let o = [c & 1, (c >> 1) & 1, c >> 2];
                    let w = (0..3).map(|a| if o[a] == 1 { f[a] } else { 1.0 - f[a] }).product::<f64>() as f32;
                    let j = ((i[2] + o[2]) * EDGE + i[1] + o[1]) * EDGE + i[0] + o[0];
                    let (c, a) = untexel(tile[j]);
                    for k in 0..3 {
                        rgb[k] += w * c[k].sqrt();
                    }
                    presence += w * a;
                    inside += w * occ[j] as f32 / 255.0;
                }
                (rgb.map(|v| v * v), presence, inside)
            }
        }
    }
}

/// A brick coloured: whole, or its samples' texels and occupancies.
type Brick = (Entry, Option<(Vec<u32>, Vec<u8>)>);

/// The texel of what the paint gives at a point.
fn paint_texel(u: Option<Unheld>, colours: &[[f32; 3]]) -> u32 {
    match u {
        None => texel(colours[0], 0.0),
        Some(u) => {
            let a = colours[u.slot as usize];
            let rgb = match u.mix {
                Some((other, share)) => {
                    let b = colours[other as usize];
                    std::array::from_fn(|k| a[k] + (b[k] - a[k]) * share)
                }
                None => a,
            };
            texel(rgb, u.presence)
        }
    }
}

/// Brick `i` of `shape` coloured: air, a glow or solid in one colour, or its samples.
fn brick_colour(shape: &Shape, i: usize, paint: Option<&Program>, colours: &[[f32; 3]]) -> Brick {
    let b = brick_at(shape.min, shape.dim, i);
    let side = shape.sides[i];
    let at = |s: [usize; 3]| -> P3 { std::array::from_fn(|k| (b[k] * BS + s[k] as i32) as f64 * shape.cell) };
    let texels: Vec<u32> = match paint {
        None => vec![paint_texel(None, colours); SAMPLES],
        Some(program) => {
            // every other sample read first; a sample within the reach of its nearest one of those
            // reads the same, and only the rest (near an edge of the paint, or in a mix's field)
            // are read after them, each read all at once
            const N: usize = EDGE / 2 + 1;
            let coarse: Vec<(Option<Unheld>, f64)> = program.unheld_many(Channel::Color, &(0..N * N * N).map(|r| at([r % N, (r / N) % N, r / (N * N)].map(|x| 2 * x))).collect::<Vec<_>>());
            let mut texels = vec![0u32; SAMPLES];
            let mut rest: Vec<usize> = Vec::new();
            for (j, t) in texels.iter_mut().enumerate() {
                let s = sample_at(j);
                // the read sample at or just below it on each axis: a cell off on each odd one
                let near = s.map(|v| v / 2);
                let r = (near[2] * N + near[1]) * N + near[0];
                let off = (s.iter().filter(|v| *v % 2 == 1).count() as f64).sqrt() * shape.cell;
                if off < coarse[r].1 || off == 0.0 {
                    *t = paint_texel(coarse[r].0, colours);
                } else {
                    rest.push(j);
                }
            }
            for (j, (u, _)) in rest.iter().zip(program.unheld_many(Channel::Color, &rest.iter().map(|j| at(sample_at(*j))).collect::<Vec<_>>())) {
                texels[*j] = paint_texel(u, colours);
            }
            texels
        }
    };
    let rgb = |t: u32| t & 0x00ff_ffff;
    match side {
        Side::Out if texels.iter().all(|t| t >> 24 == 0) => (Entry::Air, None),
        Side::Out if texels.iter().all(|t| *t == texels[0]) && texels[0] >> 24 == 255 => (Entry::Glow(texels[0]), None),
        Side::In if texels.iter().all(|t| rgb(*t) == rgb(texels[0])) => (Entry::Solid(texels[0]), None),
        Side::Out => (Entry::Air, Some((texels, vec![0; SAMPLES]))),
        Side::In => (Entry::Air, Some((texels, vec![255; SAMPLES]))),
        Side::Edge(e) => (Entry::Air, Some((texels, shape.occ[e as usize * SAMPLES..(e as usize + 1) * SAMPLES].to_vec()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::{Layer, Op, Region};
    use makepad_csg_sdf::SdfNode;

    fn serial(len: usize, f: &(dyn Fn(usize) + Sync)) {
        (0..len).for_each(f);
    }

    fn ball(r: f64) -> impl Fn(&[P3], &mut [f64]) + Sync {
        move |ps: &[P3], out: &mut [f64]| {
            for (p, d) in ps.iter().zip(out) {
                *d = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt() - r;
            }
        }
    }

    #[test]
    fn a_ball_reads_its_surface_and_is_whole_away_from_it() {
        let r = 10.0;
        let shape = Shape::bake(&ball(r), Clearance::uniform(1.0), ([-r; 3], [r; 3]), 2.0, PITCH, &serial).expect("baked");
        let grid = Grid::colour(&shape, None, &[], [0.8, 0.2, 0.1], &serial).expect("coloured");
        let ramp = RAMP * grid.cell;
        // the occupancy is the distance ramped, a half on the surface, to a twentieth of a cell
        let mut worst = 0f64;
        for k in 0..2000 {
            let t = k as f64 * 0.618_034;
            let dir = [(t * 6.283).cos() * (t * 0.37).sin(), (t * 6.283).sin() * (t * 0.37).sin(), (t * 0.37).cos()];
            let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
            // within the ramp less a cell: the samples round it are not held at 0 or 1
            let d = ((k % 7) as f64 - 3.0) / 3.0 * (ramp - grid.cell);
            let p = dir.map(|v| v / len * (r + d));
            let (_, _, occ) = grid.at(p);
            let read = (0.5 - occ as f64) * 2.0 * ramp;
            worst = worst.max((read - d).abs());
        }
        assert!(worst < 0.05 * grid.cell, "occupancy off the surface by {worst} mm");
        // deep inside is one solid brick, far outside air (nothing paints it), and only the skin
        // keeps samples
        assert_eq!(grid.at([0.0; 3]).2, 1.0);
        assert_eq!(grid.at([r + 3.0, 0.0, 0.0]), ([0.0; 3], 0.0, 0.0));
        let area = 4.0 * std::f64::consts::PI * r * r;
        let brick = BS as f64 * grid.cell;
        assert!(grid.tiles() > 0 && (grid.tiles() as f64) < 4.0 * area / (brick * brick), "{} tiles for {area:.0} mm²", grid.tiles());
        let (rgb, _, _) = grid.at([0.0, 0.0, -r + 0.1]);
        assert!((rgb[0] - 0.8).abs() < 0.02 && (rgb[1] - 0.2).abs() < 0.02, "the part's colour: {rgb:?}");
    }

    #[test]
    fn the_paint_shows_inside_the_part_and_past_it() {
        let r = 10.0;
        // a fat stroke over the top in slot 1, through the ball down to just above its centre
        // and out past it either side
        let over = Region::Stroke { points: vec![[-30.0, 0.0, 15.0], [30.0, 0.0, 15.0]], radius: 14.5 };
        let program = Program::new(vec![Layer { region: over, op: Op::Paint { channel: Channel::Color, slot: 1 } }]);
        let shape = Shape::bake(&ball(r), Clearance::uniform(1.0), ([-r; 3], [r; 3]), 4.0, PITCH, &serial).expect("baked");
        let palette = [[0.0; 3], [0.1, 0.1, 0.9]];
        let grid = Grid::colour(&shape, Some(&program), &palette, [0.9, 0.9, 0.9], &serial).expect("coloured");
        // inside the part: the paint above its boundary, the part's own colour below
        let (top, _, inside) = grid.at([0.0, 0.0, 1.0]);
        assert!(top[2] > 0.8 && top[0] < 0.2 && inside == 1.0, "the paint inside: {top:?} {inside}");
        let (bottom, presence, _) = grid.at([0.0, 0.0, -r + 0.2]);
        assert!(bottom[0] > 0.8 && presence == 0.0, "the part's own below: {bottom:?} {presence}");
        // past the part the stroke glows on, and where nothing paints there is nothing
        let (past, presence, inside) = grid.at([r + 2.0, 0.0, 6.0]);
        assert!(past[2] > 0.8 && presence == 1.0 && inside == 0.0, "the paint past the part: {past:?} {presence} {inside}");
        assert_eq!(grid.at([r + 2.0, 0.0, -6.0]).1, 0.0);
    }

    /// The colour pass reads only the samples near an edge of the paint, and every sample holds
    /// what reading it would give.
    #[test]
    fn a_sample_read_by_its_neighbour_holds_what_reading_it_gives() {
        let r = 10.0;
        let over = Region::Stroke { points: vec![[-30.0, 0.0, 15.0], [30.0, 0.0, 15.0]], radius: 14.5 };
        let dot = Region::Stroke { points: vec![[3.0, -2.0, 4.0], [3.0, -2.0, 4.0]], radius: 2.2 };
        // and a field region, read for a brick's points at once
        let sphere = SdfNode::Transform { at: [-4.0, 3.0, -2.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Sphere { radius: 3.1 }) };
        let program = Program::new(vec![
            Layer { region: over, op: Op::Paint { channel: Channel::Color, slot: 1 } },
            Layer { region: dot, op: Op::Paint { channel: Channel::Color, slot: 2 } },
            Layer { region: Region::Field { node: sphere, grow: 0.0 }, op: Op::Paint { channel: Channel::Color, slot: 3 } },
        ]);
        let shape = Shape::bake(&ball(r), Clearance::uniform(1.0), ([-r; 3], [r; 3]), 4.0, PITCH, &serial).expect("baked");
        let colours: Vec<[f32; 3]> = (0..256).map(|s| [s as f32 / 255.0, 0.5, 0.25]).collect();
        for i in 0..shape.sides.len() {
            let b = brick_at(shape.min, shape.dim, i);
            let (entry, tile) = brick_colour(&shape, i, Some(&program), &colours);
            for j in 0..SAMPLES {
                let s = sample_at(j);
                let p: P3 = std::array::from_fn(|k| (b[k] * BS + s[k] as i32) as f64 * shape.cell);
                let want = paint_texel(program.unheld(Channel::Color, p), &colours);
                match (entry, &tile) {
                    (_, Some((texels, _))) => assert_eq!(texels[j], want, "brick {b:?} sample {s:?}"),
                    (Entry::Air, None) => assert_eq!(want >> 24, 0, "brick {b:?} sample {s:?}: air where paint is"),
                    (Entry::Glow(t), None) => assert_eq!(t, want, "brick {b:?} sample {s:?}"),
                    (Entry::Solid(t), None) => assert_eq!(t & 0x00ff_ffff, want & 0x00ff_ffff, "brick {b:?} sample {s:?}"),
                    (Entry::Tile(_), None) => unreachable!("a tile holds its samples"),
                }
            }
        }
    }
}

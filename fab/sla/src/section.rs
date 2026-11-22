//! A part's cross-section on the pixel grid, from either of its two sources behind one query
//! ([`Section::cover`]): every pixel's covered share at a height.
//!
//! - [`FieldSection`]: a distance field. The field is read at the pixel centres; a pixel's
//!   share comes from its distance to the surface and the surface's direction there
//!   ([`half_plane`]). Whole tiles of pixels the field proves clear of the surface are filled or
//!   skipped from one value at their centre.
//! - [`MeshSection`]: a triangle mesh. Its slice contours at the height are drawn by signed area
//!   ([`Polygons`]): each pixel's exact share inside them.
//!
//! Both say how far a point is from their surface and which way is out ([`Section::near`]): the
//! field from its values a step either way, the mesh from its nearest triangle ([`Triangles`]).

use crate::coverage::{half_plane, Polygons};
use crate::grid::{PixelGrid, PixelRect};
use crate::tri::Triangles;
use fab_plate::PartMesh;
use fab_sdf_tiles::field::{compile, CompiledField};
use makepad_csg_sdf::{Clearance, SdfShape};
use makepad_fab_slicer::mesh::Mesh;
use makepad_fab_slicer::slice::{SliceIndex, SliceParams};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

/// A box in printer mm, (min, max).
pub type Box3 = ([f64; 3], [f64; 3]);

/// What a solid says of a point: how far it is from the surface (printer mm, below 0 inside),
/// and the way that grows fastest (a unit vector: out of the surface near it).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Near {
    pub distance: f64,
    pub normal: [f64; 3],
}

/// A solid that can say how much of each pixel it covers at a height.
pub trait Section: Send + Sync {
    /// Its box (printer mm): nothing of it lies outside.
    fn bounds(&self) -> Box3;
    /// Its covered share of each pixel of `rect` at height `z` (mm), in 255ths, taken into `cov`
    /// (row-major over `rect`) where it is more than what `cov` holds.
    fn cover(&self, z: f64, grid: &PixelGrid, rect: PixelRect, cov: &mut [u8]);
    /// Its distance and outward direction at each of `p` (printer mm), into `out`.
    fn near(&self, p: &[[f64; 3]], out: &mut [Near]);
    /// How far each of `p` is from it at least (printer mm): a distance the point is proven not
    /// to come closer than, 0 or below where it is inside or nothing is proven. The supports'
    /// clearance ([`crate::clear`]) marches on it. `need` is what each point's caller needs: a
    /// source whose first proof falls short of it may work harder to prove it (a field steeper
    /// than a distance, [`FieldSection`]); none ever says more than it proves.
    fn clear(&self, p: &[[f64; 3]], need: &[f64], out: &mut [f64]);
}

/// The pixels a box's footprint touches, within `rect`.
fn footprint(b: &Box3, grid: &PixelGrid, rect: PixelRect) -> PixelRect {
    grid.rect_of([b.0[0], b.0[1]], [b.1[0], b.1[1]]).intersect(&rect)
}

/// A share (0 to 1) in 255ths.
fn share_u8(share: f32) -> u8 {
    (share.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// `inner` (row-major over `sub`) taken into `cov` (row-major over `rect`) where more.
fn take_max(cov: &mut [u8], rect: PixelRect, inner: &[f32], sub: PixelRect) {
    let (w, sw) = (rect.width() as usize, sub.width() as usize);
    for row in 0..sub.height() as usize {
        let at = (sub.y0 - rect.y0) as usize + row;
        let dst = &mut cov[at * w + (sub.x0 - rect.x0) as usize..][..sw];
        for (d, s) in dst.iter_mut().zip(&inner[row * sw..(row + 1) * sw]) {
            *d = (*d).max(share_u8(*s));
        }
    }
}

/// The tiles a field's proof tries to settle whole, largest first: each one it can't splits in
/// four. What is left open after the last are pixels near the surface.
const TILES: [u32; 5] = [32, 16, 8, 4, 2];
/// Points per field call.
const BATCH: usize = 8192;
/// A pixel is refined on a grid of this many sub-pixels a side where its surface creases.
const SUB: usize = 4;

/// A part that is one distance field.
pub struct FieldSection {
    field: CompiledField,
    /// Printer mm into the field's own frame: `q = to_field * p`, row-major.
    to_field: [[f64; 4]; 3],
    /// Printer mm per unit of the field's frame (the placement's uniform scale).
    scale: f64,
    /// How far the field's values prove its surface away, in its own frame's units.
    clearance: Clearance,
    bounds: Box3,
    /// Points the field was read at.
    evals: AtomicU64,
}

impl FieldSection {
    /// `shape` placed on the plate by `to_printer` (row-major, `p = to_printer * q`: a turn, a
    /// move and a uniform scale), its solid within `bounds` (printer mm).
    pub fn new(shape: &SdfShape, to_printer: [[f64; 4]; 3], bounds: Box3) -> Result<FieldSection, String> {
        let m = to_printer;
        let col = |c: usize| [m[0][c], m[1][c], m[2][c]];
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let k = dot(col(0), col(0)).sqrt();
        let similar = k > 1e-9 && (0..3).all(|i| (0..3).all(|j| (dot(col(i), col(j)) - if i == j { k * k } else { 0.0 }).abs() <= 1e-9 * k * k));
        if !similar {
            return Err("a field part's placement must be a turn, a move and a uniform scale".into());
        }
        // q = (1/k) Rᵀ (p - t), with R = M / k: the rows of the inverse are M's columns / k².
        let t = [m[0][3], m[1][3], m[2][3]];
        let to_field = std::array::from_fn(|r| {
            let c = col(r).map(|v| v / (k * k));
            [c[0], c[1], c[2], -dot(c, t)]
        });
        let field = compile(&shape.root, false)?;
        Ok(FieldSection { field, to_field, scale: k, clearance: shape.clearance(), bounds, evals: AtomicU64::new(0) })
    }

    /// Points the field was read at so far.
    pub fn evals(&self) -> u64 {
        self.evals.load(Ordering::Relaxed)
    }
}

/// A distance field the pixel cover reads ([`field_cover`]): its values at bed points on a
/// plane, with what they prove.
pub(crate) trait Field {
    /// Its box (printer mm): nothing of it lies outside.
    fn field_bounds(&self) -> Box3;
    /// The most its value climbs per unit of distance near its surface (at least 1).
    fn lipschitz(&self) -> f64;
    /// Its value (printer mm), its proven distance outside and its proven depth inside (printer
    /// mm, either at most 0 where nothing is proven) at bed points `uv` (pixel coordinates) on
    /// the plane `z`.
    fn read(&self, grid: &PixelGrid, z: f64, uv: &[[f32; 2]], value: &mut Vec<f32>, outside: &mut Vec<f32>, inside: &mut Vec<f32>);
}

impl Field for FieldSection {
    fn field_bounds(&self) -> Box3 {
        self.bounds
    }

    fn lipschitz(&self) -> f64 {
        // at the values near the surface a pixel's share reads
        self.clearance.at(0.0).0
    }

    /// The field's value (printer mm), its proven distance outside and its proven depth inside
    /// (printer mm, either at most 0 where nothing is proven) at bed points `uv` (pixel
    /// coordinates) on the plane `z`.
    fn read(&self, grid: &PixelGrid, z: f64, uv: &[[f32; 2]], value: &mut Vec<f32>, outside: &mut Vec<f32>, inside: &mut Vec<f32>) {
        let n = uv.len();
        self.evals.fetch_add(n as u64, Ordering::Relaxed);
        let m = &self.to_field;
        // the plane's part of each row, as each point's sum takes it
        let rz = [m[0][2] * z, m[1][2] * z, m[2][2] * z];
        let mut xyz = vec![0.0f32; n * 3];
        for (q, p) in xyz.chunks_exact_mut(3).zip(uv) {
            let b = grid.at(p[0] as f64, p[1] as f64);
            for r in 0..3 {
                q[r] = (m[r][0] * b[0] + m[r][1] * b[1] + rz[r] + m[r][3]) as f32;
            }
        }
        value.resize(n, 0.0);
        outside.resize(n, 0.0);
        inside.resize(n, 0.0);
        self.field.eval(&xyz, value, outside, inside);
        let s = self.scale;
        for v in value.iter_mut() {
            *v *= s as f32;
        }
        // a proven distance is a value's: the radius its clearance proves (the depth on the
        // inside), in printer mm
        let radius = |d: f32, side: f64| if d > 0.0 { (self.clearance.radius(side * d as f64) * s) as f32 } else { d * s as f32 };
        for v in outside.iter_mut() {
            *v = radius(*v, 1.0);
        }
        for v in inside.iter_mut() {
            *v = radius(*v, -1.0);
        }
    }
}

/// `tiles` sorted into those the field proves wholly inside (lit) and those it leaves open, from
/// its value at their centres; those proven outside are dropped.
fn prove<F: Field>(f: &F, grid: &PixelGrid, z: f64, tiles: &[PixelRect], lit: &mut Vec<PixelRect>, open: &mut Vec<PixelRect>, scratch: &mut Scratch) {
    let [px, py] = grid.pitch;
    for chunk in tiles.chunks(BATCH) {
        scratch.uv.clear();
        scratch.uv.extend(chunk.iter().map(|t| [0.5 * (t.x0 + t.x1) as f32, 0.5 * (t.y0 + t.y1) as f32]));
        f.read(grid, z, &scratch.uv, &mut scratch.value, &mut scratch.outside, &mut scratch.inside);
        // a pixel is wholly in or out once its centre is half its diagonal from the surface;
        // the tile's pixel centres are within its half-diagonal of its centre (worked out again
        // only for a tile of another size: most are the level's)
        let mut size = (u32::MAX, u32::MAX, 0.0);
        for (k, t) in chunk.iter().enumerate() {
            if (t.width(), t.height()) != (size.0, size.1) {
                size = (t.width(), t.height(), 0.5 * (((t.width() - 1) as f64 * px).hypot((t.height() - 1) as f64 * py) + px.hypot(py)));
            }
            let reach = size.2;
            if scratch.inside[k] as f64 >= reach {
                lit.push(*t);
            } else if (scratch.outside[k] as f64) < reach {
                open.push(*t);
            }
        }
    }
}

/// The footprint `fp` in tiles the field proves lit, and the tiles it leaves open (none
/// larger than the last of [`TILES`]), from the largest down.
fn tiles<F: Field>(f: &F, grid: &PixelGrid, z: f64, fp: PixelRect, scratch: &mut Scratch) -> (Vec<PixelRect>, Vec<PixelRect>) {
    let first = TILES[0];
    let mut open = Vec::new();
    for y in (fp.y0..fp.y1).step_by(first as usize) {
        for x in (fp.x0..fp.x1).step_by(first as usize) {
            open.push(PixelRect { x0: x, y0: y, x1: (x + first).min(fp.x1), y1: (y + first).min(fp.y1) });
        }
    }
    let mut lit = Vec::new();
    for (level, &size) in TILES.iter().enumerate() {
        let tiles = if level == 0 {
            std::mem::take(&mut open)
        } else {
            let mut split = Vec::with_capacity(open.len() * 4);
            for t in open.drain(..) {
                let (xm, ym) = ((t.x0 + size).min(t.x1), (t.y0 + size).min(t.y1));
                for (y0, y1) in [(t.y0, ym), (ym, t.y1)] {
                    for (x0, x1) in [(t.x0, xm), (xm, t.x1)] {
                        let s = PixelRect { x0, y0, x1, y1 };
                        if !s.is_empty() {
                            split.push(s);
                        }
                    }
                }
            }
            split
        };
        prove(f, grid, z, &tiles, &mut lit, &mut open, scratch);
    }
    (lit, open)
}

/// The covered share of a pixel from its value `c` and its four neighbours' `[left, right,
/// below, above]` (printer mm, `pitch` apart), and whether its surface creases there: the
/// slopes on its two sides disagree, as at a corner, so one straight edge does not describe
/// it.
fn share(lipschitz: f64, c: f32, [l, r, d, u]: [f32; 4], pitch: [f64; 2]) -> (f32, bool) {
    let (fx, bx, fy, by) = (r - c, c - l, u - c, c - d);
    let pick = |a: f32, b: f32| if a.abs() >= b.abs() { a } else { b };
    let gx = pick(fx, bx) as f64 / pitch[0];
    let gy = pick(fy, by) as f64 / pitch[1];
    let crease = ((fx - bx) as f64).abs() > CREASE * pitch[0] || ((fy - by) as f64).abs() > CREASE * pitch[1];
    // lengths as roots of squares (the slopes are near 1: nothing overflows), not hypot's
    // careful call, which cost as much again as the rest
    let g = (gx * gx + gy * gy).sqrt();
    // level in the plane: the layer grazes a flat face, and no edge crosses the pixel, so the
    // whole of it is on the side its value says
    if g <= 1e-9 {
        return (if c < 0.0 { 1.0 } else if c > 0.0 { 0.0 } else { 0.5 }, crease);
    }
    // the field's slope in the plane as it is: where the plane grazes a face it is less
    // than the field's own, and the edge is that much further than the value
    let (dx, dy, mag) = (gx / g, gy / g, g.min(lipschitz));
    // the slope per pixel step, and the centre's distance in pixels
    let (sx, sy) = (dx * mag * pitch[0], dy * mag * pitch[1]);
    let step = (sx * sx + sy * sy).sqrt();
    (half_plane((-(c as f64) / step) as f32, (sx / step) as f32, (sy / step) as f32), crease)
}

/// How far (in pixels' worth of value) the slopes either side of a pixel may differ along an
/// axis before its surface counts as creased: a curve of radius r bends them by a pixel / r,
/// a corner by up to two.
const CREASE: f64 = 0.3;

/// The buffers one cover reuses.
#[derive(Default)]
struct Scratch {
    uv: Vec<[f32; 2]>,
    value: Vec<f32>,
    outside: Vec<f32>,
    inside: Vec<f32>,
    /// A band of rows' values, with a ring of one pixel around it, and which of them are read.
    band: Vec<f32>,
    read: Vec<bool>,
    /// Pixels waiting for a read: a band's own, and their neighbours by place in the band.
    ij: Vec<(u32, u32)>,
    at: Vec<usize>,
    near: Vec<(u32, u32)>,
}

impl Section for FieldSection {
    fn bounds(&self) -> Box3 {
        self.bounds
    }

    fn near(&self, p: &[[f64; 3]], out: &mut [Near]) {
        // the value at each point and a step either way along each axis: its slope by central
        // differences, which a point exactly on a face (where the field's own derivative is
        // undefined, as on a box's or a cone's flat cap) gets as well as any other
        const STEP: f64 = 1e-3;
        let m = &self.to_field;
        let mut xyz = Vec::with_capacity(p.len() * 21);
        for q in p {
            for k in 0..7 {
                let mut a = *q;
                if k > 0 {
                    a[(k - 1) / 2] += if k % 2 == 1 { STEP } else { -STEP };
                }
                for r in m {
                    xyz.push((r[0] * a[0] + r[1] * a[1] + r[2] * a[2] + r[3]) as f32);
                }
            }
        }
        let n = p.len() * 7;
        let (mut value, mut bound, mut solid) = (vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n]);
        self.evals.fetch_add(n as u64, Ordering::Relaxed);
        self.field.eval(&xyz, &mut value, &mut bound, &mut solid);
        for (i, o) in out.iter_mut().enumerate() {
            let v = |k: usize| value[7 * i + k] as f64 * self.scale;
            let g: [f64; 3] = std::array::from_fn(|c| (v(1 + 2 * c) - v(2 + 2 * c)) / (2.0 * STEP));
            let l = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
            // flat across the step (a point on the field's middle): no way is out; up, which no
            // support takes
            *o = Near { distance: v(0), normal: if l > 0.0 { g.map(|c| c / l) } else { [0.0, 0.0, 1.0] } };
        }
    }

    fn cover(&self, z: f64, grid: &PixelGrid, rect: PixelRect, cov: &mut [u8]) {
        field_cover(self, z, grid, rect, cov);
    }

    fn clear(&self, p: &[[f64; 3]], need: &[f64], out: &mut [f64]) {
        self.proven(p, out);
        if self.clearance.is_distance() {
            return;
        }
        // steeper than a distance: where the proof falls short of the need, the ball of the need
        // (and a little more first, for a longer step) covered by boxes each proven clear
        for extra in [COVER_EXTRA, 0.0] {
            let (at, r): (Vec<usize>, Vec<f64>) = (0..p.len()).filter(|&i| out[i] > 0.0 && out[i] < need[i] + extra).map(|i| (i, need[i] + extra)).unzip();
            if at.is_empty() {
                return;
            }
            let pts: Vec<[f64; 3]> = at.iter().map(|&i| p[i]).collect();
            for ((&i, r), free) in at.iter().zip(&r).zip(self.covered(&pts, &r)) {
                if free {
                    out[i] = out[i].max(*r);
                }
            }
        }
    }
}

/// A steep field's ball is first tried this much (mm) wider than its need.
const COVER_EXTRA: f64 = 0.25;
/// A ball's cover gives up past this many boxes, or boxes this small against the ball.
const COVER_BOXES: usize = 4096;
const COVER_DEPTH: u32 = 6;
/// A ball's open boxes its dive splits each level ([`FieldSection::covered`]).
const DIVE: usize = 8;

impl FieldSection {
    /// What the field proves of each of `p`: inside, how deep (as negative); outside, the radius
    /// its bound proves clear ([`Clearance::radius`], printer mm).
    fn proven(&self, p: &[[f64; 3]], out: &mut [f64]) {
        let m = &self.to_field;
        let mut xyz = Vec::with_capacity(p.len() * 3);
        for q in p {
            for r in m {
                xyz.push((r[0] * q[0] + r[1] * q[1] + r[2] * q[2] + r[3]) as f32);
            }
        }
        let n = p.len();
        let (mut value, mut outside, mut inside) = (vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n]);
        self.evals.fetch_add(n as u64, Ordering::Relaxed);
        self.field.eval(&xyz, &mut value, &mut outside, &mut inside);
        let s = self.scale;
        for (o, (v, d)) in out.iter_mut().zip(value.iter().zip(&outside)) {
            *o = if *v < 0.0 { *v as f64 * s } else if *d > 0.0 { self.clearance.radius(*d as f64) * s } else { *d as f64 * s };
        }
    }

    /// Whether the ball of radius `r[i]` about each `p[i]` is proven clear of the solid: the cube
    /// round it split in eight, level by level, a box settled once its centre's proven distance
    /// holds its corners (half its diagonal), a box off the ball dropped. A box with its centre
    /// inside, or a ball past its boxes or its depth, is not proven.
    ///
    /// A ball that isn't is most often found by a dive first: level by level only its [`DIVE`]
    /// least proven open boxes split, down the way the solid is nearest. The dive's boxes are
    /// the split's own, so one inside, or still open at the last depth, fails the ball just as
    /// the split comes to it (or fails it before); the split runs for the balls the dive left.
    fn covered(&self, p: &[[f64; 3]], r: &[f64]) -> Vec<bool> {
        let mut ok = vec![true; p.len()];
        let mut d = Vec::new();
        // 1. the dive: (ball, centre, half the side), a ball's boxes side by side
        let mut dive: Vec<(usize, [f64; 3], f64)> = (0..p.len()).map(|i| (i, p[i], r[i])).collect();
        for depth in 0..=COVER_DEPTH {
            if dive.is_empty() {
                break;
            }
            let centres: Vec<[f64; 3]> = dive.iter().map(|b| b.1).collect();
            d.resize(centres.len(), 0.0);
            self.proven(&centres, &mut d);
            let mut next = Vec::new();
            let mut k = 0;
            while k < dive.len() {
                let i = dive[k].0;
                let end = k + dive[k..].iter().take_while(|b| b.0 == i).count();
                let mut unsettled: Vec<usize> = Vec::new();
                for (b, &dc) in (k..end).zip(&d[k..end]) {
                    if dc >= dive[b].2 * 3f64.sqrt() {
                        continue;
                    }
                    if dc <= 0.0 || depth == COVER_DEPTH {
                        ok[i] = false;
                        break;
                    }
                    unsettled.push(b);
                }
                if ok[i] {
                    unsettled.sort_by(|a, b| d[*a].total_cmp(&d[*b]).then(a.cmp(b)));
                    for &b in unsettled.iter().take(DIVE) {
                        split_box(p[i], r[i], dive[b].1, dive[b].2, |c, q| next.push((i, c, q)));
                    }
                }
                k = end;
            }
            dive = next;
        }
        // 2. the split, of the balls the dive left
        let mut used = vec![1usize; p.len()];
        let mut open: Vec<(usize, [f64; 3], f64)> = (0..p.len()).filter(|&i| ok[i]).map(|i| (i, p[i], r[i])).collect();
        for depth in 0..=COVER_DEPTH {
            // a failed ball's boxes go no further
            open.retain(|b| ok[b.0]);
            if open.is_empty() {
                break;
            }
            let centres: Vec<[f64; 3]> = open.iter().map(|b| b.1).collect();
            d.resize(centres.len(), 0.0);
            self.proven(&centres, &mut d);
            let mut next = Vec::new();
            for (&(i, c, h), &dc) in open.iter().zip(&d) {
                if !ok[i] || dc >= h * 3f64.sqrt() {
                    continue;
                }
                if dc <= 0.0 || depth == COVER_DEPTH || used[i] + 8 > COVER_BOXES {
                    ok[i] = false;
                    continue;
                }
                split_box(p[i], r[i], c, h, |cc, q| {
                    next.push((i, cc, q));
                    used[i] += 1;
                });
            }
            open = next;
        }
        ok
    }
}

/// The eighths of the box about `c` (half its side `h`) that reach into the ball of radius `r`
/// about `at`, each its centre and half its side.
fn split_box(at: [f64; 3], r: f64, c: [f64; 3], h: f64, mut f: impl FnMut([f64; 3], f64)) {
    let q = 0.5 * h;
    for k in 0..8 {
        let cc = [c[0] + if k & 1 == 0 { -q } else { q }, c[1] + if k & 2 == 0 { -q } else { q }, c[2] + if k & 4 == 0 { -q } else { q }];
        // the box's nearest point to the ball's centre
        let off: f64 = (0..3).map(|a| ((cc[a] - at[a]).abs() - q).max(0.0).powi(2)).sum::<f64>().sqrt();
        if off < r {
            f(cc, q);
        }
    }
}

/// A distance field's cover of each pixel at height `z` ([`Section::cover`]): tiles it proves
/// clear of the surface filled or skipped from one value at their centre, then the pixels near
/// the surface each from its value and its neighbours' (a creased one from its sub-pixels').
pub(crate) fn field_cover<F: Field>(f: &F, z: f64, grid: &PixelGrid, rect: PixelRect, cov: &mut [u8]) {
    let b = &f.field_bounds();
    let fp = footprint(b, grid, rect);
    if fp.is_empty() || z < b.0[2] || z > b.1[2] {
        return;
    }
    let w = rect.width() as usize;
    let mut scratch = Scratch::default();
    // 1. tiles, largest first: lit, dropped, or split
    let (lit, open) = tiles(f, grid, z, fp, &mut scratch);
    for t in &lit {
        for j in t.y0..t.y1 {
            let row = (j - rect.y0) as usize * w;
            cov[row + (t.x0 - rect.x0) as usize..row + (t.x1 - rect.x0) as usize].fill(255);
        }
    }
    // 2. the open tiles' pixels a band of rows at a time, the values of a band and the ring
    // of pixels around it kept, so a pixel's slopes read its neighbours' values
    let half = 0.5 * grid.pitch[0].hypot(grid.pitch[1]);
    let band_rows = TILES[0];
    let bw = fp.width() as usize + 2;
    let bh = band_rows as usize + 2;
    scratch.band.resize(bw * bh, 0.0);
    scratch.read.resize(bw * bh, false);
    // the open tiles by band: each lies within one band of rows from the footprint's top
    let bands = (fp.height() as usize).div_ceil(band_rows as usize);
    let mut start = vec![0usize; bands + 1];
    for t in &open {
        start[((t.y0 - fp.y0) / band_rows) as usize + 1] += 1;
    }
    for b in 0..bands {
        start[b + 1] += start[b];
    }
    let mut placed = start.clone();
    let mut by_band = vec![PixelRect::EMPTY; open.len()];
    for t in &open {
        let b = ((t.y0 - fp.y0) / band_rows) as usize;
        by_band[placed[b]] = *t;
        placed[b] += 1;
    }
    let mut creased: Vec<(u32, u32)> = Vec::new();
    for band in 0..bands {
        let tiles = &by_band[start[band]..start[band + 1]];
        if tiles.is_empty() {
            continue;
        }
        let y0 = fp.y0 + band as u32 * band_rows;
        // a pixel's place in the band: its row and column from the ring's corner
        let slot = |i: u32, j: u32| (j + 1 - y0) as usize * bw + (i + 1 - fp.x0) as usize;
        // the pixels; those proven wholly in or out of their pixel are settled by the read
        scratch.uv.clear();
        scratch.ij.clear();
        for t in tiles {
            for j in t.y0..t.y1 {
                for i in t.x0..t.x1 {
                    scratch.uv.push([i as f32 + 0.5, j as f32 + 0.5]);
                    scratch.ij.push((i, j));
                }
            }
        }
        scratch.near.clear();
        for c0 in (0..scratch.ij.len()).step_by(BATCH) {
            let c1 = (c0 + BATCH).min(scratch.ij.len());
            f.read(grid, z, &scratch.uv[c0..c1], &mut scratch.value, &mut scratch.outside, &mut scratch.inside);
            for k in 0..c1 - c0 {
                let (i, j) = scratch.ij[c0 + k];
                let s = slot(i, j);
                scratch.band[s] = scratch.value[k];
                scratch.read[s] = true;
                if scratch.inside[k] as f64 >= half {
                    cov[(j - rect.y0) as usize * w + (i - rect.x0) as usize] = 255;
                } else if (scratch.outside[k] as f64) < half {
                    scratch.near.push((i, j));
                }
            }
        }
        // the pixels near the surface: their neighbours not read yet
        scratch.at.clear();
        scratch.uv.clear();
        for &(i, j) in &scratch.near {
            let s = slot(i, j);
            for (n, di, dj) in [(s - 1, -1i64, 0i64), (s + 1, 1, 0), (s - bw, 0, -1), (s + bw, 0, 1)] {
                if !scratch.read[n] {
                    scratch.read[n] = true;
                    scratch.at.push(n);
                    scratch.uv.push([(i as i64 + di) as f32 + 0.5, (j as i64 + dj) as f32 + 0.5]);
                }
            }
        }
        for c0 in (0..scratch.at.len()).step_by(BATCH) {
            let c1 = (c0 + BATCH).min(scratch.at.len());
            f.read(grid, z, &scratch.uv[c0..c1], &mut scratch.value, &mut scratch.outside, &mut scratch.inside);
            for k in 0..c1 - c0 {
                scratch.band[scratch.at[c0 + k]] = scratch.value[k];
            }
        }
        // their shares; a creased pixel is kept for 3
        for &(i, j) in &scratch.near {
            let s = slot(i, j);
            let v = &scratch.band;
            let c = v[s];
            let (share, crease) = share(f.lipschitz(), c, [v[s - 1], v[s + 1], v[s - bw], v[s + bw]], grid.pitch);
            if crease && c.abs() < (grid.pitch[0] + grid.pitch[1]) as f32 {
                creased.push((i, j));
                continue;
            }
            let o = &mut cov[(j - rect.y0) as usize * w + (i - rect.x0) as usize];
            *o = (*o).max(share_u8(share));
        }
        scratch.read.fill(false);
    }
    // 3. a creased pixel as the mean of its sub-pixels, each read with a ring around it
    let sub = [grid.pitch[0] / SUB as f64, grid.pitch[1] / SUB as f64];
    let n = SUB + 2;
    for chunk in creased.chunks(BATCH / (n * n)) {
        scratch.uv.clear();
        for &(i, j) in chunk {
            for b in 0..n {
                for a in 0..n {
                    let s = 1.0 / SUB as f32;
                    scratch.uv.push([i as f32 + (a as f32 - 0.5) * s, j as f32 + (b as f32 - 0.5) * s]);
                }
            }
        }
        f.read(grid, z, &scratch.uv, &mut scratch.value, &mut scratch.outside, &mut scratch.inside);
        for (k, &(i, j)) in chunk.iter().enumerate() {
            let v = &scratch.value[k * n * n..(k + 1) * n * n];
            let mut sum = 0.0;
            for b in 1..=SUB {
                for a in 1..=SUB {
                    let s = b * n + a;
                    sum += share(f.lipschitz(), v[s], [v[s - 1], v[s + 1], v[s - n], v[s + n]], sub).0;
                }
            }
            let o = &mut cov[(j - rect.y0) as usize * w + (i - rect.x0) as usize];
            *o = (*o).max(share_u8(sum / (SUB * SUB) as f32));
        }
    }
}

/// The outline `rings` (bed mm: outer loops one way, holes the other) drawn into the pixels of
/// `rect` by signed area, taken into `cov` where more.
pub(crate) fn outline_cover(rings: &[Vec<[f64; 2]>], grid: &PixelGrid, rect: PixelRect, cov: &mut [u8]) {
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for p in rings.iter().flatten() {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let fp = grid.rect_of(lo, hi).intersect(&rect);
    if fp.is_empty() {
        return;
    }
    let mut poly = Polygons::new(fp.width() as usize, fp.height() as usize);
    let o = [fp.x0 as f64, fp.y0 as f64];
    for ring in rings {
        for k in 0..ring.len() {
            let (a, b) = (grid.to_pixels(ring[k]), grid.to_pixels(ring[(k + 1) % ring.len()]));
            poly.edge([a[0] - o[0], a[1] - o[1]], [b[0] - o[0], b[1] - o[1]]);
        }
    }
    let mut inner = vec![0.0f32; fp.pixels()];
    poly.cover(&mut inner);
    take_max(cov, rect, &inner, fp);
}

/// A part that is a closed triangle mesh (printer mm).
pub struct MeshSection {
    mesh: Mesh,
    index: SliceIndex,
    bounds: Box3,
    /// Its triangles for distances, made at the first question.
    triangles: OnceLock<Triangles>,
}

impl MeshSection {
    pub fn new(part: &PartMesh) -> MeshSection {
        let mesh = Mesh { verts: part.positions.clone(), tris: part.triangles.clone(), paint: Vec::new() };
        let index = SliceIndex::new(&mesh);
        let bounds = mesh.bbox();
        MeshSection { mesh, index, bounds, triangles: OnceLock::new() }
    }

    fn triangles(&self) -> &Triangles {
        self.triangles.get_or_init(|| Triangles::new(&self.mesh.verts, &self.mesh.tris))
    }
}

impl Section for MeshSection {
    fn bounds(&self) -> Box3 {
        self.bounds
    }

    fn near(&self, p: &[[f64; 3]], out: &mut [Near]) {
        let t = self.triangles();
        for (q, o) in p.iter().zip(out.iter_mut()) {
            let (distance, normal) = t.near(*q);
            *o = Near { distance, normal };
        }
    }

    fn clear(&self, p: &[[f64; 3]], _need: &[f64], out: &mut [f64]) {
        let t = self.triangles();
        for (q, o) in p.iter().zip(out.iter_mut()) {
            *o = t.near(*q).0;
        }
    }

    fn cover(&self, z: f64, grid: &PixelGrid, rect: PixelRect, cov: &mut [u8]) {
        let fp = footprint(&self.bounds, grid, rect);
        if fp.is_empty() || z < self.bounds.0[2] || z > self.bounds.1[2] {
            return;
        }
        // contours true to a hundredth of a pixel (a grey level of 256), gaps under a quarter of
        // one healed
        let p = grid.pitch[0].min(grid.pitch[1]);
        let params = SliceParams { closing_radius: 0.25 * p, resolution: 0.01 * p, ..SliceParams::default() };
        let Some(regions) = self.index.cut(&self.mesh, &[z], &params).pop() else { return };
        let mut poly = Polygons::new(fp.width() as usize, fp.height() as usize);
        let o = [fp.x0 as f64, fp.y0 as f64];
        for r in &regions {
            for ring in r.rings() {
                let n = ring.pts.len();
                for k in 0..n {
                    let (a, b) = (ring.pts[k], ring.pts[(k + 1) % n]);
                    let a = grid.to_pixels([a.x_mm(), a.y_mm()]);
                    let b = grid.to_pixels([b.x_mm(), b.y_mm()]);
                    poly.edge([a[0] - o[0], a[1] - o[1]], [b[0] - o[0], b[1] - o[1]]);
                }
            }
        }
        let mut inner = vec![0.0f32; fp.pixels()];
        poly.cover(&mut inner);
        take_max(cov, rect, &inner, fp);
    }
}

/// A section raised by `lift` (mm): a part as it stands on its supports.
pub struct Lifted {
    pub section: Arc<dyn Section>,
    pub lift: f64,
}

impl Lifted {
    fn down(&self, p: &[[f64; 3]]) -> Vec<[f64; 3]> {
        p.iter().map(|q| [q[0], q[1], q[2] - self.lift]).collect()
    }
}

impl Section for Lifted {
    fn bounds(&self) -> Box3 {
        let (lo, hi) = self.section.bounds();
        ([lo[0], lo[1], lo[2] + self.lift], [hi[0], hi[1], hi[2] + self.lift])
    }

    fn cover(&self, z: f64, grid: &PixelGrid, rect: PixelRect, cov: &mut [u8]) {
        self.section.cover(z - self.lift, grid, rect, cov);
    }

    fn near(&self, p: &[[f64; 3]], out: &mut [Near]) {
        self.section.near(&self.down(p), out);
    }

    fn clear(&self, p: &[[f64; 3]], need: &[f64], out: &mut [f64]) {
        self.section.clear(&self.down(p), need, out);
    }
}

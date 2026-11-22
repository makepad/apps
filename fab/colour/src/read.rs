//! The swatch read back from a photo of its face: what each stack of a set really shows.
//!
//! The person photographs the printed swatch face up on plain white paper in daylight, the whole
//! swatch and some paper round it in the picture. The reader:
//! - finds the corner marks by the first one's dot (a dark square with a small one beside it)
//!   and, from the four marks, the photo's perspective: a homography from the layout's mm;
//! - takes how lit each point is from the frame (the lightest filament round the patches): a
//!   smooth field, and the paper's brightness against it, which sets the scale;
//! - takes the photo's values as linear light (its sRGB decoded) and checks it on the strip:
//!   lines of the darkest filament across the lightest, in five shares, lie on a straight line
//!   in linear light. Where the lens mixes the lines, how far off they lie says how far the
//!   camera's tone is from sRGB; a photo that resolves them shows no tone that way (each pixel
//!   is one filament or the other), so the reader corrects for none;
//! - reads each patch's middle (a trimmed mean) in linear light against the paper.
//!
//! Each stack patch is a [`Measured`] of the set, over the background the tiles are seen on.

use crate::image::Image;
use crate::oklab::{self, Rgb};
use crate::palette::{FilamentSet, Measured};
use crate::swatch::{self, Content, Layout, DOT, MARK};
use crate::tile::{Face, Tile, TileSpec};

/// What plain white printer paper reflects (linear, each channel): the photo's white.
pub const PAPER: f32 = 0.85;
/// How far inside a patch its reading keeps (mm): clear of its edges and of a little misregistration.
const INSET: f64 = 1.0;
/// The sampling pitch over the patches, the frame and the paper (mm).
const STEP: f64 = 0.25;
/// The share of a patch's samples left out at each end of each channel (dust, a glint).
const TRIM: f64 = 0.15;

/// A patch as read: its colour (linear) and how much its samples spread round it (their mean
/// OKLab distance from it: noise, dust or a patch that is not one colour).
#[derive(Clone, Debug, PartialEq)]
pub struct Read {
    pub colour: Rgb,
    pub spread: f32,
}

/// A swatch photo read.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    /// Each of the layout's patches as read, in its order.
    pub patches: Vec<Read>,
    /// The stack patches as measurements of the set (one per physical stack).
    pub measured: Vec<Measured>,
    /// How far the strip's means lie off a straight line in their share of the darkest
    /// filament, at most, as a share of the widest channel's range: how far the photo is from
    /// linear light.
    pub ramp_error: f32,
    /// How far the dot lies from where the marks put it (mm): how well the photo registered.
    pub registration: f64,
    /// The mean OKLab distance between each pair over the light backing and over the darkest:
    /// how much the third layer shows (above about 0.02, a third dithered layer pays).
    pub backing_shows: f32,
}

/// Read `photo`, a picture of the swatch of `set` printed as `spec` lays it out
/// ([`swatch::tile`]).
pub fn read(photo: &Image, set: &FilamentSet, spec: &TileSpec) -> Result<Reading, String> {
    let (tile, lay) = swatch::tile(set, spec);
    let (h, registration) = register(photo, &lay, tile.cell)?;
    let light = Light::fit(photo, &h, &lay, [tile.size[0] as f64 * tile.cell, tile.size[1] as f64 * tile.cell])?;
    // each patch's samples against the frame's light there
    let samples: Vec<Vec<Rgb>> = lay
        .patches
        .iter()
        .map(|p| {
            grid([p.at[0] + INSET, p.at[1] + INSET], [p.at[0] + p.size[0] - INSET, p.at[1] + p.size[1] - INSET])
                .filter_map(|q| {
                    let c = sample(photo, h.map(q))?;
                    let f = light.at(q);
                    Some(std::array::from_fn(|k| c[k] / f[k]))
                })
                .collect()
        })
        .collect();
    if samples.iter().any(|s| s.len() < 8) {
        return Err("part of the swatch lies outside the photo: the whole swatch in the picture".into());
    }
    // the strip (the layout's last five patches): the share of each one's samples on the darkest
    // filament, and how far their means lie off a line in it
    let darkest = (0..set.filaments.len()).min_by(|&a, &b| set.filaments[a].lightness().total_cmp(&set.filaments[b].lightness())).unwrap_or(0) as u8;
    let strip = lay.patches.len() - 5;
    let shares: Vec<f64> = lay.patches[strip..]
        .iter()
        .map(|p| {
            let cells: Vec<bool> = grid([p.at[0] + INSET, p.at[1] + INSET], [p.at[0] + p.size[0] - INSET, p.at[1] + p.size[1] - INSET]).filter_map(|q| face_at(&tile, spec, q)).map(|f| f == darkest).collect();
            cells.iter().filter(|d| **d).count() as f64 / cells.len().max(1) as f64
        })
        .collect();
    // as a share of the widest channel's range (a channel the two filaments nearly share has
    // no range to measure against)
    let lines: Vec<(f64, f64)> = (0..3).map(|k| off_line(&samples[strip..], &shares, k)).collect();
    let ramp_error = (lines.iter().map(|l| l.0).fold(0.0, f64::max) / lines.iter().map(|l| l.1).fold(1e-9, f64::max)) as f32;
    // the frame's own reflectance, from the paper's against it
    let frame: Rgb = std::array::from_fn(|k| PAPER / light.paper[k]);
    let patches: Vec<Read> = lay
        .patches
        .iter()
        .zip(&samples)
        .map(|(p, s)| {
            let lin: Vec<Rgb> = s.iter().map(|c| std::array::from_fn(|k| frame[k] * c[k].max(0.0))).collect();
            let colour: Rgb = std::array::from_fn(|k| match p.content {
                // lines: their mean
                Content::Ramp(_) => (lin.iter().map(|c| c[k] as f64).sum::<f64>() / lin.len() as f64) as f32,
                Content::Stack(_) => trimmed_mean(lin.iter().map(|c| c[k]).collect()),
            });
            let lab = oklab::oklab(colour);
            let spread = lin.iter().map(|c| oklab::delta_e(oklab::oklab(*c), lab)).sum::<f32>() / lin.len() as f32;
            Read { colour, spread }
        })
        .collect();
    // one measurement per physical stack (the strip's pure ones repeat a block's)
    let mut groups: Vec<(Vec<(u8, f64)>, Vec<Rgb>)> = Vec::new();
    for (p, r) in lay.patches.iter().zip(&patches) {
        let Content::Stack(stack) = &p.content else { continue };
        let physical = physical(stack, spec.layer);
        match groups.iter_mut().find(|g| g.0 == physical) {
            Some(g) => g.1.push(r.colour),
            None => groups.push((physical, vec![r.colour])),
        }
    }
    let measured = groups
        .into_iter()
        .map(|(stack, colours)| Measured { stack, background: spec.background, colour: std::array::from_fn(|k| colours.iter().map(|c| c[k]).sum::<f32>() / colours.len() as f32) })
        .collect();
    let n = set.filaments.len();
    let backing_shows = (0..n * n).map(|i| oklab::delta_e(oklab::oklab(patches[i].colour), oklab::oklab(patches[n * n + i].colour))).sum::<f32>() / (n * n) as f32;
    Ok(Reading { patches, measured, ramp_error, registration, backing_shows })
}

/// A stack (a filament per layer, the face first) as filament runs of their whole thickness.
fn physical(stack: &[u8], layer: f64) -> Vec<(u8, f64)> {
    let mut out: Vec<(u8, f64)> = Vec::new();
    for f in stack {
        match out.last_mut() {
            Some((g, t)) if g == f => *t += layer,
            _ => out.push((*f, layer)),
        }
    }
    out
}

/// The filament the swatch's face shows at a point of the layout (as the face is seen), if the
/// point is on it.
fn face_at(tile: &Tile, spec: &TileSpec, q: [f64; 2]) -> Option<u8> {
    let [nx, ny] = tile.size;
    let (cu, cv) = ((q[0] / tile.cell).floor(), (q[1] / tile.cell).floor());
    if cu < 0.0 || cv < 0.0 || cu as usize >= nx || cv as usize >= ny {
        return None;
    }
    let x = match spec.face {
        Face::Down => nx - 1 - cu as usize,
        Face::Up => cu as usize,
    };
    let y = ny - 1 - cv as usize;
    Some(tile.layers[spec.layer_of(0)][y * nx + x])
}

/// The points of a `STEP` grid over a rectangle (mm, its corners).
fn grid(lo: [f64; 2], hi: [f64; 2]) -> impl Iterator<Item = [f64; 2]> {
    let n = |a: f64, b: f64| ((b - a) / STEP).floor().max(0.0) as usize + 1;
    let (nu, nv) = (n(lo[0], hi[0]), n(lo[1], hi[1]));
    (0..nu * nv).map(move |i| [lo[0] + (i % nu) as f64 * STEP, lo[1] + (i / nu) as f64 * STEP])
}

/// The photo at a point (pixels, from the top left corner of the first pixel), bilinear; none
/// off the photo.
fn sample(photo: &Image, p: [f64; 2]) -> Option<Rgb> {
    let (x, y) = (p[0] - 0.5, p[1] - 0.5);
    if x < 0.0 || y < 0.0 || x >= (photo.width - 1) as f64 || y >= (photo.height - 1) as f64 {
        return None;
    }
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
    let (a, b, c, d) = (photo.at(x0, y0), photo.at(x0 + 1, y0), photo.at(x0, y0 + 1), photo.at(x0 + 1, y0 + 1));
    Some(std::array::from_fn(|k| (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy) + (c[k] * (1.0 - fx) + d[k] * fx) * fy))
}

fn trimmed_mean(mut v: Vec<f32>) -> f32 {
    v.sort_by(f32::total_cmp);
    let cut = (v.len() as f64 * TRIM) as usize;
    let kept = &v[cut..v.len() - cut];
    kept.iter().sum::<f32>() / kept.len().max(1) as f32
}

/// How far the strip's means (channel `k`) lie off the line through them in their share of the
/// darkest filament, at most, and their range.
fn off_line(strip: &[Vec<Rgb>], shares: &[f64], k: usize) -> (f64, f64) {
    let means: Vec<f64> = strip.iter().map(|s| s.iter().map(|c| c[k] as f64).sum::<f64>() / s.len() as f64).collect();
    let n = means.len() as f64;
    let (ms, mm) = (shares.iter().sum::<f64>() / n, means.iter().sum::<f64>() / n);
    let sxx: f64 = shares.iter().map(|s| (s - ms).powi(2)).sum();
    let slope = shares.iter().zip(&means).map(|(s, m)| (s - ms) * (m - mm)).sum::<f64>() / sxx.max(1e-12);
    let range = means.iter().cloned().fold(f64::MIN, f64::max) - means.iter().cloned().fold(f64::MAX, f64::min);
    (shares.iter().zip(&means).map(|(s, m)| (m - mm - slope * (s - ms)).abs()).fold(0.0, f64::max), range)
}

/// A dark blob of the photo: its size (pixels), middle and how elongated it is (1: round or
/// square).
struct Blob {
    area: f64,
    c: [f64; 2],
    elong: f64,
}

/// The photo's dark blobs: 4-connected pixels darker than 0.45 of its white (the paper, the
/// frame).
fn blobs(photo: &Image) -> Vec<Blob> {
    let (w, h) = (photo.width, photo.height);
    let lum: Vec<f32> = photo.pixels.iter().map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect();
    let mut sorted = lum.clone();
    sorted.sort_by(f32::total_cmp);
    let white = sorted[sorted.len() * 9 / 10];
    let dark: Vec<bool> = lum.iter().map(|v| *v < 0.45 * white).collect();
    let mut seen = vec![false; w * h];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..w * h {
        if !dark[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let mut s = [0.0f64; 6];
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
            for (a, v) in s.iter_mut().zip([1.0, fx, fy, fx * fx, fy * fy, fx * fy]) {
                *a += v;
            }
            let near = [(x > 0).then(|| i - 1), (x + 1 < w).then(|| i + 1), (y > 0).then(|| i - w), (y + 1 < h).then(|| i + w)];
            for j in near.into_iter().flatten() {
                if dark[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        let n = s[0];
        let c = [s[1] / n, s[2] / n];
        let (vxx, vyy, vxy) = (s[3] / n - c[0] * c[0], s[4] / n - c[1] * c[1], s[5] / n - c[0] * c[1]);
        let (half, det) = (0.5 * (vxx + vyy), vxx * vyy - vxy * vxy);
        let disc = (half * half - det).max(0.0).sqrt();
        out.push(Blob { area: n, c, elong: ((half + disc) / (half - disc).max(1e-9)).sqrt() });
    }
    out
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// The photo's perspective from the four corner marks, found by the first one's dot, and how far
/// the dot lies from where they put it (mm). A mark's middle is that of the cells it prints in
/// (`cell` mm, those whose middles lie in it), as the photo shows it.
fn register(photo: &Image, lay: &Layout, cell: f64) -> Result<(Homography, f64), String> {
    let squares: Vec<Blob> = blobs(photo).into_iter().filter(|b| b.area >= 12.0 && b.elong < 1.6).collect();
    let centre = |at: [f64; 2], side: f64| -> [f64; 2] {
        let mid = |a: f64| {
            let cells: Vec<f64> = ((a / cell).floor() as i64..=((a + side) / cell).ceil() as i64).map(|i| (i as f64 + 0.5) * cell).filter(|m| *m >= a && *m < a + side).collect();
            cells.iter().sum::<f64>() / cells.len().max(1) as f64
        };
        [mid(at[0]), mid(at[1])]
    };
    let first = centre(lay.marks[0], MARK);
    let dot = centre(lay.dot, DOT);
    // the dot beside the first mark: a fifth of its side, the layout's gap from it
    let (want, gap) = ((MARK / DOT).powi(2), dist(first, dot) / MARK);
    let mut best: Option<(f64, usize, usize)> = None;
    for (i, d) in squares.iter().enumerate() {
        for (j, m) in squares.iter().enumerate() {
            let ratio = m.area / d.area;
            let off = dist(m.c, d.c) / m.area.sqrt() / gap;
            if i == j || !(0.5..2.0).contains(&(ratio / want)) || !(0.7..1.4).contains(&off) {
                continue;
            }
            let score = (ratio / want).ln().abs() + off.ln().abs();
            if best.map_or(true, |(s, _, _)| score < s) {
                best = Some((score, i, j));
            }
        }
    }
    let (_, di, mi) = best.ok_or("the photo shows no corner mark with its dot: the whole swatch face up, in focus, on white paper")?;
    let (d, m) = (&squares[di], &squares[mi]);
    // the layout's u along the dot, v a quarter turn on (the photo's y runs down, as v does)
    let k = m.area.sqrt() / MARK;
    let u = [(d.c[0] - m.c[0]) / dist(d.c, m.c), (d.c[1] - m.c[1]) / dist(d.c, m.c)];
    let v = [-u[1], u[0]];
    let mut found = vec![m.c];
    for at in &lay.marks[1..] {
        let c = centre(*at, MARK);
        let (du, dv) = (k * (c[0] - first[0]), k * (c[1] - first[1]));
        let p = [m.c[0] + du * u[0] + dv * v[0], m.c[1] + du * u[1] + dv * v[1]];
        let near = squares.iter().filter(|b| (0.4..2.5).contains(&(b.area / m.area))).min_by(|a, b| dist(a.c, p).total_cmp(&dist(b.c, p)));
        match near {
            Some(b) if dist(b.c, p) < 0.15 * k * lay.size[0].hypot(lay.size[1]) => found.push(b.c),
            _ => return Err("a corner mark of the swatch is not in the photo: the whole swatch in the picture".into()),
        }
    }
    let marks: Vec<[f64; 2]> = lay.marks.iter().map(|a| centre(*a, MARK)).collect();
    let h = Homography::fit(&marks, &found)?;
    Ok((h, dist(h.map(dot), d.c) / k))
}

/// A plane's perspective: layout mm to photo pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Homography(pub [[f64; 3]; 3]);

impl Homography {
    pub fn map(&self, p: [f64; 2]) -> [f64; 2] {
        let m = &self.0;
        let w = m[2][0] * p[0] + m[2][1] * p[1] + m[2][2];
        [(m[0][0] * p[0] + m[0][1] * p[1] + m[0][2]) / w, (m[1][0] * p[0] + m[1][1] * p[1] + m[1][2]) / w]
    }

    /// The one taking four points `from` to `to` (each moved to their middle and scaled to a unit
    /// spread first, so the equations are well conditioned).
    pub fn fit(from: &[[f64; 2]], to: &[[f64; 2]]) -> Result<Homography, String> {
        let norm = |pts: &[[f64; 2]]| -> [[f64; 3]; 3] {
            let n = pts.len() as f64;
            let c = [pts.iter().map(|p| p[0]).sum::<f64>() / n, pts.iter().map(|p| p[1]).sum::<f64>() / n];
            let s = std::f64::consts::SQRT_2 / (pts.iter().map(|p| dist(*p, c)).sum::<f64>() / n).max(1e-12);
            [[s, 0.0, -s * c[0]], [0.0, s, -s * c[1]], [0.0, 0.0, 1.0]]
        };
        let (a, b) = (norm(from), norm(to));
        let apply = |t: &[[f64; 3]; 3], p: [f64; 2]| [t[0][0] * p[0] + t[0][2], t[1][1] * p[1] + t[1][2]];
        // h00 u + h01 v + h02 - h20 u x - h21 v x = x, and the same for y
        let mut m = [[0.0f64; 9]; 8];
        for (i, (f, t)) in from.iter().zip(to).take(4).enumerate() {
            let ([u, v], [x, y]) = (apply(&a, *f), apply(&b, *t));
            m[2 * i] = [u, v, 1.0, 0.0, 0.0, 0.0, -u * x, -v * x, x];
            m[2 * i + 1] = [0.0, 0.0, 0.0, u, v, 1.0, -u * y, -v * y, y];
        }
        for col in 0..8 {
            let pivot = (col..8).max_by(|&r, &s| m[r][col].abs().total_cmp(&m[s][col].abs())).unwrap_or(col);
            if m[pivot][col].abs() < 1e-12 {
                return Err("the corner marks found lie on a line: the whole swatch in the picture".into());
            }
            m.swap(col, pivot);
            for r in 0..8 {
                if r != col {
                    let f = m[r][col] / m[col][col];
                    for c in col..9 {
                        m[r][c] -= f * m[col][c];
                    }
                }
            }
        }
        let h: Vec<f64> = (0..8).map(|r| m[r][8] / m[r][r]).collect();
        let hn = [[h[0], h[1], h[2]], [h[3], h[4], h[5]], [h[6], h[7], 1.0]];
        // undo the scaling: b^-1 * hn * a
        let bi = [[1.0 / b[0][0], 0.0, -b[0][2] / b[0][0]], [0.0, 1.0 / b[1][1], -b[1][2] / b[1][1]], [0.0, 0.0, 1.0]];
        let mul = |x: &[[f64; 3]; 3], y: &[[f64; 3]; 3]| -> [[f64; 3]; 3] { std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| x[r][k] * y[k][c]).sum())) };
        Ok(Homography(mul(&mul(&bi, &hn), &a)))
    }
}

/// How lit the photo is: per channel the frame's value as a smooth field over the layout
/// (log-quadratic in u and v), and the paper's value over the frame's.
struct Light {
    coef: [[f64; 6]; 3],
    size: [f64; 2],
    paper: Rgb,
}

impl Light {
    fn terms(&self, q: [f64; 2]) -> [f64; 6] {
        terms(q, self.size)
    }

    fn at(&self, q: [f64; 2]) -> Rgb {
        let t = self.terms(q);
        std::array::from_fn(|k| self.coef[k].iter().zip(&t).map(|(c, x)| c * x).sum::<f64>().exp() as f32)
    }

    /// The frame's samples (clear of every patch and mark by a millimetre) and the paper's (a
    /// band two to six mm round the swatch), fitted at once: log value = field + paper's step.
    fn fit(photo: &Image, h: &Homography, lay: &Layout, size: [f64; 2]) -> Result<Light, String> {
        let clear = |q: [f64; 2]| {
            let rects = lay.patches.iter().map(|p| (p.at, p.size)).chain(lay.marks.iter().map(|m| (*m, [MARK, MARK]))).chain([(lay.dot, [DOT, DOT])]);
            let inside = q[0] > 1.0 && q[1] > 1.0 && q[0] < size[0] - 1.0 && q[1] < size[1] - 1.0;
            inside && rects.into_iter().all(|(at, s)| q[0] < at[0] - 1.0 || q[1] < at[1] - 1.0 || q[0] > at[0] + s[0] + 1.0 || q[1] > at[1] + s[1] + 1.0)
        };
        let paper = |q: [f64; 2]| {
            let out = |v: f64, n: f64| if v < 0.0 { -v } else if v > n { v - n } else { 0.0 };
            let d = out(q[0], size[0]).max(out(q[1], size[1]));
            (2.0..=6.0).contains(&d)
        };
        let mut rows: Vec<([f64; 6], bool, Rgb)> = Vec::new();
        for q in grid([-6.0, -6.0], [size[0] + 6.0, size[1] + 6.0]).filter(|q| ((q[0] / STEP).round() as i64 % 4 == 0) && ((q[1] / STEP).round() as i64 % 4 == 0)) {
            let on_paper = paper(q);
            if !(on_paper || clear(q)) {
                continue;
            }
            if let Some(c) = sample(photo, h.map(q)).filter(|c| c.iter().all(|v| *v > 1e-4)) {
                rows.push((terms(q, size), on_paper, c));
            }
        }
        let papers = rows.iter().filter(|r| r.1).count();
        if papers < 20 {
            return Err("the photo needs white paper round the swatch, a few mm on every side".into());
        }
        let mut coef = [[0.0; 6]; 3];
        let mut step = [0.0f32; 3];
        for k in 0..3 {
            let mut keep = vec![true; rows.len()];
            let mut x = [0.0f64; 7];
            for pass in 0..2 {
                // least squares over the 6 field terms and the paper's step
                let mut a = [[0.0f64; 8]; 7];
                for (r, _) in rows.iter().zip(&keep).filter(|(_, k)| **k) {
                    let v: [f64; 7] = std::array::from_fn(|i| if i < 6 { r.0[i] } else if r.1 { 1.0 } else { 0.0 });
                    let y = (r.2[k] as f64).ln();
                    for i in 0..7 {
                        for j in 0..7 {
                            a[i][j] += v[i] * v[j];
                        }
                        a[i][7] += v[i] * y;
                    }
                }
                x = solve7(a).ok_or("the photo's light does not fit: an even light on the swatch and the paper")?;
                if pass == 0 {
                    let res: Vec<f64> = rows.iter().map(|r| ((r.2[k] as f64).ln() - (0..6).map(|i| x[i] * r.0[i]).sum::<f64>() - if r.1 { x[6] } else { 0.0 }).abs()).collect();
                    let mut sorted = res.clone();
                    sorted.sort_by(f64::total_cmp);
                    let cut = (3.0 * sorted[sorted.len() / 2]).max(0.02);
                    keep = res.iter().map(|r| *r <= cut).collect();
                }
            }
            coef[k].copy_from_slice(&x[..6]);
            step[k] = x[6].exp() as f32;
        }
        Ok(Light { coef, size, paper: step })
    }
}

/// The field's terms at a point: 1, u, v, u², uv, v² over the swatch scaled to -1..1.
fn terms(q: [f64; 2], size: [f64; 2]) -> [f64; 6] {
    let (u, v) = (2.0 * q[0] / size[0] - 1.0, 2.0 * q[1] / size[1] - 1.0);
    [1.0, u, v, u * u, u * v, v * v]
}

/// Solves 7 equations (an augmented 7 x 8 matrix) by elimination with pivoting.
fn solve7(mut m: [[f64; 8]; 7]) -> Option<[f64; 7]> {
    for col in 0..7 {
        let pivot = (col..7).max_by(|&r, &s| m[r][col].abs().total_cmp(&m[s][col].abs()))?;
        if m[pivot][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, pivot);
        for r in 0..7 {
            if r != col {
                let f = m[r][col] / m[col][col];
                for c in col..8 {
                    m[r][c] -= f * m[col][c];
                }
            }
        }
    }
    Some(std::array::from_fn(|r| m[r][7] / m[r][r]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dither::Rules;
    use crate::palette::Filament;
    use crate::tile::Axis;

    /// A camera's photo of a swatch's face, as `truth`'s filaments print it: on paper, turned and
    /// seen a little from the side, lit unevenly with a tint, with noise (decoded to linear).
    fn photograph(truth: &FilamentSet, spec: &TileSpec) -> Image {
        let (tile, _) = swatch::tile(truth, spec);
        let [nx, ny] = tile.size;
        let (w, h) = (1300usize, 1000usize);
        // layout mm to pixels: about 12 px a mm, turned 5 degrees, the right side further off
        let g = Homography([[11.9, -1.05, 180.0], [1.2, 11.7, 160.0], [0.00035, -0.0005, 1.0]]);
        let back = Homography::fit(&[[0.0, 0.0], [100.0, 0.0], [0.0, 100.0], [100.0, 100.0]].map(|p| g.map(p)), &[[0.0, 0.0], [100.0, 0.0], [0.0, 100.0], [100.0, 100.0]]).unwrap();
        let mut seed = 7u64;
        let mut noise = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.01
        };
        let tint = [1.08f32, 1.0, 0.86];
        let pixels = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f64 + 0.5, (i / w) as f64 + 0.5);
                let q = back.map([x, y]);
                let (cu, cv) = ((q[0] / tile.cell).floor(), (q[1] / tile.cell).floor());
                let r = if cu >= 0.0 && cv >= 0.0 && (cu as usize) < nx && (cv as usize) < ny {
                    tile.shown[(ny - 1 - cv as usize) * nx + nx - 1 - cu as usize]
                } else {
                    [PAPER; 3]
                };
                let lit = (0.7 + 0.3 * x / w as f64) * (1.0 - 0.15 * (y / h as f64 - 0.4).powi(2));
                std::array::from_fn(|k| (0.95 * tint[k] * lit as f32 * r[k]).min(1.0) + noise())
            })
            .collect();
        Image { width: w, height: h, pixels }
    }

    fn set(name: &str, colours: [[u8; 3]; 4], names: [&str; 4], scatter: f32) -> FilamentSet {
        FilamentSet::new(name, colours.iter().zip(names).map(|(c, n)| Filament { scatter: [scatter; 3], ..Filament::guess(n, *c) }).collect())
    }

    /// A photo of the swatch as its true filaments print it (not as first guessed: other
    /// colours, other scattering) reads back each stack within a just-seen difference, where
    /// the guess is several times further off; the marks register it within a fifth of a mm and
    /// the strip lies on its line. With a white and without one.
    #[test]
    fn a_swatch_photo_reads_back_what_its_stacks_show() {
        let names = ["Cyan", "Magenta", "Yellow", "White"];
        let guessed = [[0, 160, 224], [224, 0, 122], [255, 224, 0], [242, 242, 242]];
        let printed = [[0, 140, 200], [205, 20, 120], [250, 205, 30], [236, 236, 228]];
        let cases = [
            (set("cmyw", guessed, names, 3.0), set("cmyw as printed", printed, names, 1.8)),
            (set("cmyk", [guessed[0], guessed[1], guessed[2], [26, 26, 26]], ["Cyan", "Magenta", "Yellow", "Black"], 3.0), set("cmyk as printed", [printed[0], printed[1], printed[2], [30, 28, 30]], ["Cyan", "Magenta", "Yellow", "Black"], 1.8)),
        ];
        for (guess, truth) in cases {
            let cell = 0.4;
            let size = swatch::size(4);
            let spec = TileSpec { cell, size: [(size[0] / cell) as usize, (size[1] / cell) as usize], layers: 4, layer: 0.16, face: Face::Down, depth: 2, rules: Rules { min_run: 1, switch: 0.0, lightness: 1.0 }, first_lines: Axis::X, background: [0.8; 3], viewing: 0.0 };
            let photo = photograph(&truth, &spec);
            let r = read(&photo, &guess, &spec).unwrap_or_else(|e| panic!("{}: {e}", guess.name));
            assert!(r.registration < 0.2, "{}: the dot is {:.2} mm off", guess.name, r.registration);
            assert!(r.ramp_error < 0.02, "{}: the strip is {:.3} off its line", guess.name, r.ramp_error);
            let (shown, lay) = swatch::tile(&truth, &spec);
            let [nx, ny] = shown.size;
            let (mut read_off, mut guess_off, mut worst, mut n) = (0.0f32, 0.0f32, 0.0f32, 0);
            for (p, got) in lay.patches.iter().zip(&r.patches) {
                let Content::Stack(stack) = &p.content else { continue };
                let (cu, cv) = (((p.at[0] + 0.5 * p.size[0]) / cell) as usize, ((p.at[1] + 0.5 * p.size[1]) / cell) as usize);
                let true_colour = oklab::oklab(shown.shown[(ny - 1 - cv) * nx + nx - 1 - cu]);
                let off = oklab::delta_e(oklab::oklab(got.colour), true_colour);
                let predicted = guess.colour_of(&physical(stack, spec.layer), spec.background).0;
                read_off += off;
                worst = worst.max(off);
                guess_off += oklab::delta_e(oklab::oklab(predicted), true_colour);
                n += 1;
            }
            let (read_off, guess_off) = (read_off / n as f32, guess_off / n as f32);
            assert!(read_off < 0.01 && worst < 0.025, "{}: read {read_off:.4} off on average, {worst:.4} at worst", guess.name);
            assert!(guess_off > 3.0 * read_off, "{}: the guess is {guess_off:.4} off, the reading {read_off:.4}", guess.name);
            // every stack measured once, over the tiles' background
            for p in &lay.patches {
                let Content::Stack(stack) = &p.content else { continue };
                assert_eq!(r.measured.iter().filter(|m| m.stack == physical(stack, spec.layer)).count(), 1, "{}: {stack:?}", guess.name);
            }
            assert!(r.measured.iter().all(|m| m.background == spec.background));
        }
    }
}

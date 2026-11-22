//! How a picture looks from a viewing distance, and how far one looks from another.
//!
//! The eye resolves lightness finely and colour coarsely: cells of different hues side by side
//! blend into one at a distance where different lightnesses still show. A picture is seen
//! through the eye's filters, the spatial extension of CIELAB that Zhang and Wandell published:
//! in an opponent space (lightness, red-green, blue-yellow), each channel is blurred by its own
//! sum of Gaussians whose spreads are angles, so the blur in mm grows with the distance. Two
//! pictures seen so are compared in OKLab, pixel by pixel.

use crate::image::Image;
use crate::oklab;

/// How far a tile is looked at from (mm): at arm's length.
pub const VIEWING: f64 = 300.0;

/// Each opponent channel's filter: Gaussians exp(-(r / spread)^2), (weight, spread in degrees).
const FILTERS: [&[(f64, f64)]; 3] = [&[(0.921, 0.0283), (0.105, 0.133), (-0.108, 4.336)], &[(0.531, 0.0392), (0.330, 0.494)], &[(0.488, 0.0536), (0.371, 0.386)]];

/// XYZ (D65) to the opponent channels.
const OPPONENT: [[f64; 3]; 3] = [[0.279, 0.72, -0.107], [-0.449, 0.29, -0.077], [0.086, -0.59, 0.501]];

/// Linear sRGB to XYZ (D65).
const XYZ: [[f64; 3]; 3] = [[0.4124, 0.3576, 0.1805], [0.2126, 0.7152, 0.0722], [0.0193, 0.1192, 0.9505]];

type M3 = [[f64; 3]; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

fn invert(m: &M3) -> M3 {
    // cofactors by cyclic indices carry their own signs
    let c = |i: usize, j: usize| m[(i + 1) % 3][(j + 1) % 3] * m[(i + 2) % 3][(j + 2) % 3] - m[(i + 1) % 3][(j + 2) % 3] * m[(i + 2) % 3][(j + 1) % 3];
    let det: f64 = (0..3).map(|j| m[0][j] * c(0, j)).sum();
    std::array::from_fn(|i| std::array::from_fn(|j| c(j, i) / det))
}

fn apply(m: &M3, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| (0..3).map(|k| m[i][k] * v[k]).sum())
}

/// A linear colour in the opponent channels (lightness, red-green, blue-yellow).
pub fn opponent(c: [f32; 3]) -> [f64; 3] {
    apply(&mul(&OPPONENT, &XYZ), c.map(|x| x as f64))
}

/// Per opponent channel, what a small difference there weighs as a squared OKLab distance near
/// colour `c` (linear): the channels' own share of the eye's metric there.
pub fn weights(c: [f32; 3]) -> [f64; 3] {
    let lab = oklab::oklab(c.map(|x| x.clamp(1e-4, 1.0)));
    let h = 1e-3f32;
    // the opponent channels' change per unit of L, a and b, then its inverse: Lab per channel
    let at = |d: [f32; 3]| opponent(oklab::linear(std::array::from_fn(|k| lab[k] + d[k])));
    let mut j = [[0.0f64; 3]; 3];
    for axis in 0..3 {
        let mut d = [0.0f32; 3];
        d[axis] = h;
        let (p, m) = (at(d), at(d.map(|x| -x)));
        for ch in 0..3 {
            j[ch][axis] = (p[ch] - m[ch]) / (2.0 * h as f64);
        }
    }
    let inv = invert(&j);
    // ΔE² = |J⁻¹ δ|²: its diagonal
    std::array::from_fn(|ch| (0..3).map(|k| inv[k][ch] * inv[k][ch]).sum())
}

/// Blurs wider than this many cells are left out of the eye a dither is refined against: they
/// weigh a picture's large areas, which the dither's error diffusion already keeps.
const WIDEST: f64 = 8.0;

/// The eye's filter per opponent channel from `distance` mm, for cells `pitch` mm apart: its
/// Gaussians as (weight, sigma in cells), the weights summing to one.
pub fn eye(distance: f64, pitch: f64) -> [Vec<(f64, f64)>; 3] {
    let per_degree = distance * 1f64.to_radians().tan() / pitch;
    std::array::from_fn(|ch| {
        let kept: Vec<(f64, f64)> = FILTERS[ch].iter().map(|&(w, spread)| (w, spread * per_degree / std::f64::consts::SQRT_2)).filter(|&(_, sigma)| sigma <= WIDEST).collect();
        let total: f64 = kept.iter().map(|k| k.0).sum();
        kept.into_iter().map(|(w, sigma)| (w / total, sigma)).collect()
    })
}

/// `image` (linear, `mm` a pixel) as the eye resolves it from `distance` mm.
pub fn seen(image: &Image, mm: f64, distance: f64) -> Image {
    let (w, h) = (image.width, image.height);
    let to = mul(&OPPONENT, &XYZ);
    let from = invert(&to);
    let per_degree = distance * 1f64.to_radians().tan() / mm;
    let mut channels: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::with_capacity(w * h));
    for p in &image.pixels {
        let o = apply(&to, p.map(|x| x as f64));
        for k in 0..3 {
            channels[k].push(o[k]);
        }
    }
    for (k, filter) in FILTERS.iter().enumerate() {
        let total: f64 = filter.iter().map(|f| f.0).sum();
        let mut out = vec![0.0; w * h];
        for &(weight, spread) in filter.iter() {
            let blurred = gauss(&channels[k], w, h, spread * per_degree / std::f64::consts::SQRT_2);
            for (o, b) in out.iter_mut().zip(blurred) {
                *o += weight / total * b;
            }
        }
        channels[k] = out;
    }
    let pixels = (0..w * h).map(|i| apply(&from, [channels[0][i], channels[1][i], channels[2][i]]).map(|x| x.max(0.0) as f32)).collect();
    Image { width: w, height: h, pixels }
}

/// `v` (`w` x `h`) blurred by a Gaussian of `sigma` pixels, each pixel's sum weighed by the part
/// of the kernel inside the picture.
fn gauss(v: &[f64], w: usize, h: usize, sigma: f64) -> Vec<f64> {
    if sigma < 0.25 {
        return v.to_vec();
    }
    let r = (3.0 * sigma).ceil() as isize;
    let kernel: Vec<f64> = (-r..=r).map(|d| (-((d * d) as f64) / (2.0 * sigma * sigma)).exp()).collect();
    let pass = |src: &[f64], len: usize, lines: usize, at: &dyn Fn(usize, usize) -> usize| -> Vec<f64> {
        let mut out = vec![0.0; w * h];
        for line in 0..lines {
            for i in 0..len {
                let (mut s, mut n) = (0.0, 0.0);
                let lo = (i as isize - r).max(0) as usize;
                let hi = (i as isize + r).min(len as isize - 1) as usize;
                for j in lo..=hi {
                    let k = kernel[(j as isize - i as isize + r) as usize];
                    s += k * src[at(line, j)];
                    n += k;
                }
                out[at(line, i)] = s / n;
            }
        }
        out
    };
    let across = pass(v, w, h, &|y, x| y * w + x);
    pass(&across, h, w, &|x, y| y * w + x)
}

/// How far apart two pictures of one size look, pixel by pixel: their OKLab distance x 100 (on
/// the scale of CIELAB's).
pub fn delta_e(a: &Image, b: &Image) -> Vec<f32> {
    a.pixels.iter().zip(&b.pixels).map(|(p, q)| 100.0 * oklab::delta_e(oklab::oklab(*p), oklab::oklab(*q))).collect()
}

/// The mean and the 95th percentile of some differences.
pub fn spread(d: &[f32]) -> (f32, f32) {
    if d.is_empty() {
        return (0.0, 0.0);
    }
    let mut s = d.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    (s.iter().sum::<f32>() / s.len() as f32, s[((s.len() - 1) as f64 * 0.95).round() as usize])
}

/// The ΔE a [`heat`] map shows white.
pub const HEAT_TOP: f32 = 30.0;

/// Differences as a picture: black at 0, red at a third of [`HEAT_TOP`], yellow at two thirds,
/// white from it on.
pub fn heat(d: &[f32], width: usize, height: usize) -> Image {
    let pixels = d
        .iter()
        .map(|&v| {
            let t = (v / HEAT_TOP).clamp(0.0, 1.0) * 3.0;
            let srgb = [t.min(1.0), (t - 1.0).clamp(0.0, 1.0), (t - 2.0).clamp(0.0, 1.0)];
            srgb.map(oklab::to_linear)
        })
        .collect();
    Image { width, height, pixels }
}

/// A picture beside the tile made of it, seen four ways, and how far apart they look.
pub struct Comparison {
    /// Two by two: the picture as the cells take it and the tile's face cell by cell above, the
    /// face seen from [`VIEWING`] and its [`heat`] map against the picture seen so below.
    pub image: Image,
    /// ΔE (mean, 95th percentile) seen from [`VIEWING`]: picture to face, picture to the colours
    /// the dither aimed at (what the gamut cost), and those to the face (what the dither cost).
    pub total: (f32, f32),
    pub gamut: (f32, f32),
    pub dither: (f32, f32),
}

/// `picture`, `wanted` and `face` are one size, cell by cell as the face is seen, `mm` a cell;
/// each cell `scale` pixels on a side in the image.
pub fn compare(picture: &Image, wanted: &Image, face: &Image, mm: f64, scale: usize) -> Comparison {
    let (sp, sw, sf) = (seen(picture, mm, VIEWING), seen(wanted, mm, VIEWING), seen(face, mm, VIEWING));
    let total = delta_e(&sp, &sf);
    let map = heat(&total, face.width, face.height);
    let (gap, white) = (4 * scale, [1.0; 3]);
    let top = picture.scaled(scale).beside(&face.scaled(scale), gap, white);
    let bottom = sf.scaled(scale).beside(&map.scaled(scale), gap, white);
    Comparison { image: top.above(&bottom, gap, white), total: spread(&total), gamut: spread(&delta_e(&sp, &sw)), dither: spread(&delta_e(&sw, &sf)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// From afar a fine checker of two hues of one lightness looks like their mix, much nearer
    /// it than a checker of black and white looks like grey; a flat picture looks as it is; the
    /// opponent space goes there and back.
    #[test]
    fn colour_detail_blends_before_lightness_detail() {
        let to = mul(&OPPONENT, &XYZ);
        let back = mul(&invert(&to), &to);
        for i in 0..3 {
            for j in 0..3 {
                assert!((back[i][j] - if i == j { 1.0 } else { 0.0 }).abs() < 1e-9);
            }
        }
        let n = 64;
        let checker = |a: [f32; 3], b: [f32; 3]| Image { width: n, height: n, pixels: (0..n * n).map(|i| if (i % n + i / n) % 2 == 0 { a } else { b }).collect() };
        let flat = |c: [f32; 3]| Image { width: n, height: n, pixels: vec![c; n * n] };
        let mix = |a: [f32; 3], b: [f32; 3]| std::array::from_fn::<f32, 3, _>(|k| 0.5 * (a[k] + b[k]));
        // a pair of one OKLab lightness, far apart in hue
        let (p, q) = (oklab::linear([0.7, 0.1, 0.08]), oklab::linear([0.7, -0.1, -0.08]));
        let (k, w) = ([0.0; 3], [1.0; 3]);
        let mid = |img: &Image| img.pixels[n / 2 * n + n / 2];
        let off = |a: [f32; 3], b: [f32; 3]| {
            let s = seen(&checker(a, b), 0.4, VIEWING);
            100.0 * oklab::delta_e(oklab::oklab(mid(&s)), oklab::oklab(mix(a, b)))
        };
        let (hues, greys) = (off(p, q), off(k, w));
        assert!(hues < 0.5 * greys, "hues blend more: {hues} vs {greys}");
        let g = seen(&flat([0.3, 0.4, 0.5]), 0.4, VIEWING);
        assert!(delta_e(&g, &flat([0.3, 0.4, 0.5])).iter().all(|d| *d < 0.1));
        assert_eq!(spread(&[1.0, 2.0, 3.0, 4.0]), (2.5, 4.0));
    }
}

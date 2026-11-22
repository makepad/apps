//! A filament's look as a layer (Kubelka-Munk): a thin layer reflects some light, passes some to
//! what lies under it and absorbs the rest, per channel of linear light. A material is its
//! reflectance when thick enough to hide what is under it (R∞, the colour of the spool) and how
//! strongly it scatters (per mm): together they give any thickness's reflectance and
//! transmittance, and layers over one another add up exactly ([`over`]).

use crate::oklab::Rgb;

/// What a layer does to light, per channel: the share it reflects and the share it passes on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layer {
    pub r: Rgb,
    pub t: Rgb,
}

/// A layer `mm` thick of a material that reflects `colour` (R∞, linear) when thick and scatters
/// `scatter` per mm.
pub fn layer(colour: Rgb, scatter: Rgb, mm: f64) -> Layer {
    let mut r = [0.0; 3];
    let mut t = [0.0; 3];
    for k in 0..3 {
        let (rk, tk) = channel(colour[k] as f64, scatter[k] as f64 * mm.max(0.0));
        r[k] = rk as f32;
        t[k] = tk as f32;
    }
    Layer { r, t }
}

/// One channel's (reflectance, transmittance) of a layer of reflectance `r_inf` when thick, `sx`
/// its scattering times its thickness. Written with e^-2x so a thick layer does not overflow.
fn channel(r_inf: f64, sx: f64) -> (f64, f64) {
    let r_inf = r_inf.clamp(1e-4, 1.0 - 1e-7);
    // absorption over scattering, from the reflectance of a thick layer
    let k_s = (1.0 - r_inf).powi(2) / (2.0 * r_inf);
    let a = 1.0 + k_s;
    let b = (a * a - 1.0).sqrt();
    let x = b * sx;
    if x < 1e-9 {
        // no absorption to speak of: what scatters back of a white layer
        return (sx / (1.0 + sx), 1.0 / (1.0 + sx));
    }
    let e2 = (-2.0 * x).exp();
    let den = a * (1.0 - e2) + b * (1.0 + e2);
    ((1.0 - e2) / den, 2.0 * b * (-x).exp() / den)
}

/// The light a layer reflects over a surface that reflects `under`: its own and what passes it
/// twice, back and forth between the two.
pub fn over(top: Layer, under: Rgb) -> Rgb {
    std::array::from_fn(|k| {
        let (r, t, u) = (top.r[k], top.t[k], under[k]);
        r + t * t * u / (1.0 - r * u).max(1e-6)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layer thick enough is its spool colour; a thin one passes light, a white one more than a
    /// dark one; a layer of a colour over the same colour is that colour; and two half layers are
    /// one whole layer (the layers add up exactly).
    #[test]
    fn layers_hide_pass_and_add_up() {
        let s = [10.0; 3];
        let thick = layer([0.6, 0.3, 0.05], s, 5.0);
        assert!((0..3).all(|k| (thick.r[k] - [0.6, 0.3, 0.05][k]).abs() < 1e-3 && thick.t[k] < 1e-3), "{thick:?}");
        let (white, dark) = (layer([0.85; 3], s, 0.16), layer([0.03; 3], s, 0.16));
        assert!(white.t[0] > 0.3 && dark.t[0] < 0.05, "{white:?} {dark:?}");
        assert!((0..3).all(|k| white.r[k] + white.t[k] <= 1.0 + 1e-6));
        let c = [0.7, 0.2, 0.1];
        let on_itself = over(layer(c, s, 0.16), c);
        assert!((0..3).all(|k| (on_itself[k] - c[k]).abs() < 1e-4), "{on_itself:?}");
        let under = [0.1, 0.5, 0.9];
        let (half, whole) = (layer(c, s, 0.08), layer(c, s, 0.16));
        let (a, b) = (over(half, over(half, under)), over(whole, under));
        assert!((0..3).all(|k| (a[k] - b[k]).abs() < 1e-4), "{a:?} {b:?}");
    }
}

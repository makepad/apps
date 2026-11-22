//! Linear light, sRGB and OKLab. Side-by-side cells mix in linear light (an average); how far
//! two colours look apart is a distance in OKLab.

pub type Rgb = [f32; 3];
pub type Lab = [f32; 3];

/// An sRGB channel (0..1) in linear light.
pub fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// A linear channel (0..1) in sRGB.
pub fn to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// An 8-bit sRGB colour in linear light.
pub fn linear_of_srgb8(c: [u8; 3]) -> Rgb {
    c.map(|v| to_linear(v as f32 / 255.0))
}

/// A linear colour as 8-bit sRGB.
pub fn srgb8_of_linear(c: Rgb) -> [u8; 3] {
    c.map(|v| (to_srgb(v) * 255.0).round() as u8)
}

/// Linear RGB to OKLab (L 0..1).
pub fn oklab(c: Rgb) -> Lab {
    let [r, g, b] = c;
    let l = 0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_99 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// OKLab to linear RGB (outside 0..1 where the colour is outside sRGB).
pub fn linear(c: Lab) -> Rgb {
    let [l, a, b] = c;
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

/// How far two colours look apart: their distance in OKLab (about 0.01 is just seen).
pub fn delta_e(a: Lab, b: Lab) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OKLab goes there and back, white is L 1 with no hue, and sRGB round-trips its bytes.
    #[test]
    fn oklab_round_trips_and_white_is_neutral() {
        for c in [[1.0, 1.0, 1.0], [0.2, 0.5, 0.9], [0.9, 0.05, 0.1], [0.0, 0.0, 0.0]] {
            let back = linear(oklab(c));
            assert!((0..3).all(|k| (back[k] - c[k]).abs() < 1e-4), "{c:?} -> {back:?}");
        }
        let w = oklab([1.0; 3]);
        assert!((w[0] - 1.0).abs() < 1e-3 && w[1].abs() < 1e-3 && w[2].abs() < 1e-3, "{w:?}");
        for v in [0u8, 1, 17, 128, 254, 255] {
            assert_eq!(srgb8_of_linear(linear_of_srgb8([v; 3])), [v; 3]);
        }
    }
}

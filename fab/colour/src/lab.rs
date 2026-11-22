//! CIE L*a*b* (D65) and the perceptual colour differences used for standard colour codes: the
//! nearest standard colour to a picked one, and whether a colour is close enough to call it that.

use crate::oklab::to_linear;

/// CIE L*a*b* against the D65 white: L 0..100.
pub type CieLab = [f32; 3];

fn lab_f(t: f32) -> f32 {
    if t > 216.0 / 24389.0 {
        t.cbrt()
    } else {
        (24389.0 / 27.0 * t + 16.0) / 116.0
    }
}

/// A gamma-encoded sRGB colour (0..1) in CIE L*a*b*: sRGB to linear to XYZ (D65) to Lab.
pub fn srgb_to_lab(c: [f32; 3]) -> CieLab {
    let [r, g, b] = c.map(to_linear);
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175 * b;
    let z = 0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b;
    let (fx, fy, fz) = (lab_f(x / 0.950_47), lab_f(y), lab_f(z / 1.088_83));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIE76: the straight distance in Lab.
pub fn delta_e76(a: CieLab, b: CieLab) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// CIEDE2000 with kL = kC = kH = 1 (the published implementation notes' formulation).
pub fn delta_e2000(x: CieLab, y: CieLab) -> f32 {
    let rad = std::f64::consts::PI / 180.0;
    let (xl, xa, xb) = (x[0] as f64, x[1] as f64, x[2] as f64);
    let (yl, ya, yb) = (y[0] as f64, y[1] as f64, y[2] as f64);
    let pow25_7 = 25f64.powi(7);
    let cm = (xa.hypot(xb) + ya.hypot(yb)) / 2.0;
    let g = 0.5 * (1.0 - (cm.powi(7) / (cm.powi(7) + pow25_7)).sqrt());
    let (a1, a2) = ((1.0 + g) * xa, (1.0 + g) * ya);
    let (c1, c2) = (a1.hypot(xb), a2.hypot(yb));
    let hue = |b: f64, a: f64| {
        if a == 0.0 && b == 0.0 {
            return 0.0;
        }
        let h = b.atan2(a) / rad;
        if h < 0.0 {
            h + 360.0
        } else {
            h
        }
    };
    let (h1, h2) = (hue(xb, a1), hue(yb, a2));
    let (dl, dc) = (yl - xl, c2 - c1);
    let mut dh = 0.0;
    if c1 * c2 != 0.0 {
        dh = h2 - h1;
        if dh > 180.0 {
            dh -= 360.0;
        } else if dh < -180.0 {
            dh += 360.0;
        }
    }
    let d_big_h = 2.0 * (c1 * c2).sqrt() * (dh * rad / 2.0).sin();
    let lm = (xl + yl) / 2.0;
    let cm2 = (c1 + c2) / 2.0;
    let mut hm = h1 + h2;
    if c1 * c2 != 0.0 {
        hm = if (h1 - h2).abs() <= 180.0 {
            (h1 + h2) / 2.0
        } else if h1 + h2 < 360.0 {
            (h1 + h2 + 360.0) / 2.0
        } else {
            (h1 + h2 - 360.0) / 2.0
        };
    }
    let t = 1.0 - 0.17 * ((hm - 30.0) * rad).cos() + 0.24 * (2.0 * hm * rad).cos() + 0.32 * ((3.0 * hm + 6.0) * rad).cos()
        - 0.20 * ((4.0 * hm - 63.0) * rad).cos();
    let d_theta = 30.0 * (-((hm - 275.0) / 25.0).powi(2)).exp();
    let rc = 2.0 * (cm2.powi(7) / (cm2.powi(7) + pow25_7)).sqrt();
    let sl = 1.0 + 0.015 * (lm - 50.0).powi(2) / (20.0 + (lm - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cm2;
    let sh = 1.0 + 0.015 * cm2 * t;
    let rt = -(2.0 * d_theta * rad).sin() * rc;
    let (l, c, h) = (dl / sl, dc / sc, d_big_h / sh);
    (l * l + c * c + h * h + rt * c * h).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CIEDE2000 matches the published test pairs 1 and 7, and white is L 100 with no hue.
    #[test]
    fn ciede2000_reference_pairs_and_white() {
        let p1 = delta_e2000([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485]);
        assert!((p1 - 2.0425).abs() < 1e-3, "pair 1: {p1}");
        let p7 = delta_e2000([50.0, 0.0, 0.0], [50.0, -1.0, 2.0]);
        assert!((p7 - 2.3669).abs() < 1e-3, "pair 7: {p7}");
        let w = srgb_to_lab([1.0; 3]);
        assert!((w[0] - 100.0).abs() < 0.05 && w[1].abs() < 0.05, "white: {w:?}");
        assert_eq!(delta_e2000(w, w), 0.0, "a colour is no distance from itself");
    }
}

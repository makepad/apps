//! Pure colour maths behind the colour effects: tone curves as lookup
//! tables, `.cube` 3D LUTs, gradient ramps and white balance.

/// A monotone cubic (Fritsch–Carlson) through `points` (x, y in 0..1),
/// sampled at `n` evenly spaced inputs. Points are sorted by x; the curve
/// holds its end values outside the first and last point. Monotone data
/// gives a monotone curve (no overshoot), which is what a tone curve needs.
pub fn curve_table(points: &[[f64; 2]], n: usize) -> Vec<f64> {
    let mut pts: Vec<[f64; 2]> = points.iter().copied().filter(|p| p[0].is_finite() && p[1].is_finite()).collect();
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
    pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-9);
    let x_of = |i: usize| i as f64 / (n.max(2) - 1) as f64;
    match pts.len() {
        0 => return (0..n).map(x_of).collect(),
        1 => return vec![pts[0][1].clamp(0.0, 1.0); n],
        _ => {}
    }
    let k = pts.len();
    let slopes: Vec<f64> = pts.windows(2).map(|w| (w[1][1] - w[0][1]) / (w[1][0] - w[0][0])).collect();
    let mut m = vec![0.0; k];
    m[0] = slopes[0];
    m[k - 1] = slopes[k - 2];
    for i in 1..k - 1 {
        m[i] = if slopes[i - 1] * slopes[i] <= 0.0 { 0.0 } else { (slopes[i - 1] + slopes[i]) * 0.5 };
    }
    for i in 0..k - 1 {
        if slopes[i] == 0.0 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
            continue;
        }
        let a = m[i] / slopes[i];
        let b = m[i + 1] / slopes[i];
        let s = a * a + b * b;
        if s > 9.0 {
            let tau = 3.0 / s.sqrt();
            m[i] = tau * a * slopes[i];
            m[i + 1] = tau * b * slopes[i];
        }
    }
    (0..n)
        .map(|j| {
            let x = x_of(j);
            if x <= pts[0][0] {
                return pts[0][1].clamp(0.0, 1.0);
            }
            if x >= pts[k - 1][0] {
                return pts[k - 1][1].clamp(0.0, 1.0);
            }
            let i = pts.partition_point(|p| p[0] <= x) - 1;
            let h = pts[i + 1][0] - pts[i][0];
            let t = (x - pts[i][0]) / h;
            let (t2, t3) = (t * t, t * t * t);
            let y = (2.0 * t3 - 3.0 * t2 + 1.0) * pts[i][1]
                + (t3 - 2.0 * t2 + t) * h * m[i]
                + (-2.0 * t3 + 3.0 * t2) * pts[i + 1][1]
                + (t3 - t2) * h * m[i + 1];
            y.clamp(0.0, 1.0)
        })
        .collect()
}

/// The curves effect as one 256×1 BGRA table: each channel holds its own
/// curve applied after the master curve.
pub fn curves_texture(master: &[[f64; 2]], red: &[[f64; 2]], green: &[[f64; 2]], blue: &[[f64; 2]]) -> Vec<u32> {
    let m = curve_table(master, 256);
    let [r, g, b] = [red, green, blue].map(|c| curve_table(c, 256));
    let look = |table: &[f64], v: f64| {
        let x = v.clamp(0.0, 1.0) * 255.0;
        let i = (x.floor() as usize).min(254);
        let f = x - i as f64;
        table[i] + (table[i + 1] - table[i]) * f
    };
    (0..256)
        .map(|i| {
            let v = m[i];
            let byte = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u32;
            0xff00_0000 | (byte(look(&r, v)) << 16) | (byte(look(&g, v)) << 8) | byte(look(&b, v))
        })
        .collect()
}

/// A parsed `.cube` 3D LUT: `size`³ entries, red fastest.
#[derive(Clone, Debug, PartialEq)]
pub struct CubeLut {
    pub size: usize,
    pub domain_min: [f64; 3],
    pub domain_max: [f64; 3],
    pub table: Vec<[f64; 3]>,
}

impl CubeLut {
    /// Parses Adobe/Resolve `.cube` text (3D; `LUT_3D_SIZE`, optional
    /// `DOMAIN_MIN`/`DOMAIN_MAX`, `TITLE`, comments).
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut size = None;
        let mut domain_min = [0.0; 3];
        let mut domain_max = [1.0; 3];
        let mut table = Vec::new();
        for (line_no, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("TITLE") {
                continue;
            }
            let mut words = line.split_whitespace();
            let first = words.next().unwrap_or_default();
            let three = |words: std::str::SplitWhitespace| -> Result<[f64; 3], String> {
                let v: Vec<f64> = words.map(|w| w.parse::<f64>()).collect::<Result<_, _>>().map_err(|_| format!("line {}: `{line}` is not three numbers", line_no + 1))?;
                v.try_into().map_err(|_| format!("line {}: `{line}` needs three numbers", line_no + 1))
            };
            match first {
                "LUT_3D_SIZE" => {
                    let n: usize = words.next().and_then(|w| w.parse().ok()).ok_or_else(|| format!("line {}: LUT_3D_SIZE needs a number", line_no + 1))?;
                    if !(2..=256).contains(&n) {
                        return Err(format!("line {}: LUT_3D_SIZE {n} is outside 2..256", line_no + 1));
                    }
                    size = Some(n);
                }
                "LUT_1D_SIZE" => return Err("a 1D .cube LUT is not supported; use curves, or export a 3D LUT".into()),
                "DOMAIN_MIN" => domain_min = three(words)?,
                "DOMAIN_MAX" => domain_max = three(words)?,
                "LUT_3D_INPUT_RANGE" | "LUT_1D_INPUT_RANGE" => {}
                _ => {
                    let v: Vec<f64> = line.split_whitespace().map(|w| w.parse::<f64>()).collect::<Result<_, _>>().map_err(|_| format!("line {}: unexpected `{line}`", line_no + 1))?;
                    let v: [f64; 3] = v.try_into().map_err(|_| format!("line {}: `{line}` needs three numbers", line_no + 1))?;
                    table.push(v);
                }
            }
        }
        let size = size.ok_or("no LUT_3D_SIZE line; is this a 3D .cube file?")?;
        if table.len() != size * size * size {
            return Err(format!("LUT_3D_SIZE {size} needs {} entries, the file has {}", size * size * size, table.len()));
        }
        Ok(Self { size, domain_min, domain_max, table })
    }

    /// The identity LUT of a size.
    pub fn identity(size: usize) -> Self {
        let mut table = Vec::with_capacity(size * size * size);
        let s = (size - 1) as f64;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    table.push([r as f64 / s, g as f64 / s, b as f64 / s]);
                }
            }
        }
        Self { size, domain_min: [0.0; 3], domain_max: [1.0; 3], table }
    }

    /// Trilinear lookup, on the CPU (the reference for the GPU test).
    pub fn sample(&self, rgb: [f64; 3]) -> [f64; 3] {
        let n = self.size;
        let s = (n - 1) as f64;
        let coord = |c: usize| ((rgb[c] - self.domain_min[c]) / (self.domain_max[c] - self.domain_min[c])).clamp(0.0, 1.0) * s;
        let (x, y, z) = (coord(0), coord(1), coord(2));
        let (x0, y0, z0) = ((x.floor() as usize).min(n - 2), (y.floor() as usize).min(n - 2), (z.floor() as usize).min(n - 2));
        let (fx, fy, fz) = (x - x0 as f64, y - y0 as f64, z - z0 as f64);
        let at = |r: usize, g: usize, b: usize| self.table[(b * n + g) * n + r];
        let mut out = [0.0; 3];
        for (c, o) in out.iter_mut().enumerate() {
            let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
            let c00 = lerp(at(x0, y0, z0)[c], at(x0 + 1, y0, z0)[c], fx);
            let c10 = lerp(at(x0, y0 + 1, z0)[c], at(x0 + 1, y0 + 1, z0)[c], fx);
            let c01 = lerp(at(x0, y0, z0 + 1)[c], at(x0 + 1, y0, z0 + 1)[c], fx);
            let c11 = lerp(at(x0, y0 + 1, z0 + 1)[c], at(x0 + 1, y0 + 1, z0 + 1)[c], fx);
            *o = lerp(lerp(c00, c10, fy), lerp(c01, c11, fy), fz);
        }
        out
    }

    /// The LUT as a 2D BGRA atlas of `size` slices side by side (blue picks
    /// the slice): `size²` wide, `size` high. The shader interpolates red
    /// and green in the slice and blue between two slices.
    pub fn atlas(&self) -> (usize, usize, Vec<u32>) {
        let n = self.size;
        let mut data = vec![0u32; n * n * n];
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    let v = self.table[(b * n + g) * n + r];
                    let byte = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u32;
                    data[g * n * n + b * n + r] = 0xff00_0000 | (byte(v[0]) << 16) | (byte(v[1]) << 8) | byte(v[2]);
                }
            }
        }
        (n * n, n, data)
    }
}

/// Gradient stops as a 256×1 ramp of straight colours (BGRA words).
/// Stops are sorted; the ends hold outside the first and last stop.
pub fn ramp(stops: &[(f64, [f64; 4])]) -> Vec<u32> {
    let mut stops: Vec<(f64, [f64; 4])> = stops.iter().copied().filter(|(p, _)| p.is_finite()).collect();
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    if stops.is_empty() {
        stops = vec![(0.0, [0.0, 0.0, 0.0, 1.0]), (1.0, [1.0; 4])];
    }
    (0..256)
        .map(|i| {
            let x = i as f64 / 255.0;
            let c = if x <= stops[0].0 {
                stops[0].1
            } else if x >= stops[stops.len() - 1].0 {
                stops[stops.len() - 1].1
            } else {
                let k = stops.partition_point(|s| s.0 <= x);
                let (a, b) = (stops[k - 1], stops[k]);
                let u = if b.0 > a.0 { (x - a.0) / (b.0 - a.0) } else { 1.0 };
                [0, 1, 2, 3].map(|c| a.1[c] + (b.1[c] - a.1[c]) * u)
            };
            let byte = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u32;
            (byte(c[3]) << 24) | (byte(c[0]) << 16) | (byte(c[1]) << 8) | byte(c[2])
        })
        .collect()
}

/// White balance as per-channel gains, normalised so luma (Rec. 709) is
/// unchanged: temperature moves along blue ↔ amber, tint along green ↔
/// magenta, both -100..100.
pub fn white_balance_gains(temperature: f64, tint: f64) -> [f64; 3] {
    let t = temperature.clamp(-100.0, 100.0) / 100.0;
    let g = tint.clamp(-100.0, 100.0) / 100.0;
    let gains = [1.0 + 0.3 * t + 0.1 * g, 1.0 - 0.25 * g, 1.0 - 0.3 * t + 0.1 * g];
    let luma = 0.2126 * gains[0] + 0.7152 * gains[1] + 0.0722 * gains[2];
    gains.map(|v| v / luma)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_pass_through_their_points_and_stay_monotone() {
        let pts = [[0.0, 0.0], [0.25, 0.1], [0.75, 0.9], [1.0, 1.0]];
        let t = curve_table(&pts, 1001);
        assert!((t[250] - 0.1).abs() < 1e-9 && (t[750] - 0.9).abs() < 1e-9);
        assert!(t.windows(2).all(|w| w[1] >= w[0] - 1e-12), "monotone");
        let identity = curve_table(&[[0.0, 0.0], [1.0, 1.0]], 11);
        assert!(identity.iter().enumerate().all(|(i, v)| (v - i as f64 / 10.0).abs() < 1e-12));
        // A flat segment stays flat (no overshoot).
        let flat = curve_table(&[[0.0, 0.0], [0.4, 0.5], [0.6, 0.5], [1.0, 1.0]], 101);
        assert!(flat[40..=60].iter().all(|v| (v - 0.5).abs() < 1e-9));
        let table = curves_texture(&[[0.0, 0.0], [1.0, 1.0]], &[[0.0, 1.0], [1.0, 0.0]], &[[0.0, 0.0], [1.0, 1.0]], &[[0.0, 0.0], [1.0, 1.0]]);
        assert_eq!(table[0], 0xffff0000, "red inverted at black");
        assert_eq!(table[255], 0xff00ffff);
    }

    #[test]
    fn cube_files_parse_and_sample() {
        let text = "TITLE \"t\"\n# comment\nLUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        let lut = CubeLut::parse(text).unwrap();
        assert_eq!(lut, CubeLut::identity(2));
        let v = lut.sample([0.2, 0.5, 0.9]);
        assert!((v[0] - 0.2).abs() < 1e-12 && (v[1] - 0.5).abs() < 1e-12 && (v[2] - 0.9).abs() < 1e-12);
        let err = CubeLut::parse("LUT_3D_SIZE 2\n0 0 0\n").unwrap_err();
        assert!(err.contains("needs 8 entries"), "{err}");
        assert!(CubeLut::parse("0 0 0").unwrap_err().contains("LUT_3D_SIZE"));
        let (w, h, data) = CubeLut::identity(4).atlas();
        assert_eq!((w, h, data.len()), (16, 4, 64));
        // Slice b = 3, g = 2, r = 1.
        assert_eq!(data[2 * 16 + 3 * 4 + 1], 0xff000000 | (85 << 16) | (170 << 8) | 255);
    }

    #[test]
    fn ramps_interpolate_stops() {
        let r = ramp(&[(0.0, [0.0, 0.0, 0.0, 1.0]), (1.0, [1.0, 1.0, 1.0, 1.0])]);
        assert_eq!(r[0], 0xff000000);
        assert_eq!(r[255], 0xffffffff);
        assert_eq!(r[128] & 0xff, 128);
        let r = ramp(&[(0.5, [1.0, 0.0, 0.0, 1.0])]);
        assert!(r.iter().all(|v| *v == 0xffff0000));
    }

    #[test]
    fn white_balance_keeps_luma() {
        for (t, g) in [(50.0, 0.0), (-80.0, 30.0), (0.0, -100.0)] {
            let k = white_balance_gains(t, g);
            assert!((0.2126 * k[0] + 0.7152 * k[1] + 0.0722 * k[2] - 1.0).abs() < 1e-12);
        }
        let warm = white_balance_gains(60.0, 0.0);
        assert!(warm[0] > 1.0 && warm[2] < 1.0);
    }
}

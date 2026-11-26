//! Paths for `stroke_path`: SVG path data flattened to polylines, trimmed
//! to a window of their length and tapered, as the segments the stroke
//! shader draws (each a capsule whose radius varies along it).

/// Parses SVG path data (M, L, H, V, C, S, Q, T, Z; absolute and relative;
/// arcs are drawn as straight lines to their end point) into polylines, one
/// per subpath, curves flattened to within `tolerance` pixels.
pub fn parse_svg(data: &str, tolerance: f64) -> Result<Vec<(Vec<[f64; 2]>, bool)>, String> {
    let mut tokens = Tokens { s: data.as_bytes(), at: 0 };
    let mut out: Vec<(Vec<[f64; 2]>, bool)> = Vec::new();
    let mut current: Vec<[f64; 2]> = Vec::new();
    let mut pos = [0.0, 0.0];
    let mut start = [0.0, 0.0];
    let mut last_ctrl: Option<[f64; 2]> = None;
    let mut last_quad: Option<[f64; 2]> = None;
    let mut command = b'M';
    let tol = tolerance.max(0.01);
    loop {
        tokens.skip_separators();
        if tokens.at >= tokens.s.len() {
            break;
        }
        let c = tokens.s[tokens.at];
        if c.is_ascii_alphabetic() {
            command = c;
            tokens.at += 1;
        } else if command == b'Z' || command == b'z' {
            return Err(format!("path data: a number follows Z at byte {}", tokens.at));
        }
        let rel = command.is_ascii_lowercase();
        let base = if rel { pos } else { [0.0, 0.0] };
        let pt = |tokens: &mut Tokens| -> Result<[f64; 2], String> {
            let x = tokens.number()?;
            let y = tokens.number()?;
            Ok([base[0] + x, base[1] + y])
        };
        match command.to_ascii_uppercase() {
            b'M' => {
                if current.len() > 1 {
                    out.push((std::mem::take(&mut current), false));
                }
                current.clear();
                pos = pt(&mut tokens)?;
                start = pos;
                current.push(pos);
                // Further pairs after M are line-tos.
                command = if rel { b'l' } else { b'L' };
                last_ctrl = None;
                last_quad = None;
            }
            b'L' => {
                pos = pt(&mut tokens)?;
                current.push(pos);
                last_ctrl = None;
                last_quad = None;
            }
            b'H' => {
                let x = tokens.number()?;
                pos = [if rel { pos[0] + x } else { x }, pos[1]];
                current.push(pos);
                last_ctrl = None;
                last_quad = None;
            }
            b'V' => {
                let y = tokens.number()?;
                pos = [pos[0], if rel { pos[1] + y } else { y }];
                current.push(pos);
                last_ctrl = None;
                last_quad = None;
            }
            b'C' | b'S' => {
                let c1 = if command.to_ascii_uppercase() == b'C' {
                    pt(&mut tokens)?
                } else {
                    last_ctrl.map(|c| [2.0 * pos[0] - c[0], 2.0 * pos[1] - c[1]]).unwrap_or(pos)
                };
                let c2 = pt(&mut tokens)?;
                let end = pt(&mut tokens)?;
                flatten_cubic(&mut current, pos, c1, c2, end, tol);
                last_ctrl = Some(c2);
                last_quad = None;
                pos = end;
            }
            b'Q' | b'T' => {
                let q = if command.to_ascii_uppercase() == b'Q' {
                    pt(&mut tokens)?
                } else {
                    last_quad.map(|c| [2.0 * pos[0] - c[0], 2.0 * pos[1] - c[1]]).unwrap_or(pos)
                };
                let end = pt(&mut tokens)?;
                let c1 = [pos[0] + (q[0] - pos[0]) * 2.0 / 3.0, pos[1] + (q[1] - pos[1]) * 2.0 / 3.0];
                let c2 = [end[0] + (q[0] - end[0]) * 2.0 / 3.0, end[1] + (q[1] - end[1]) * 2.0 / 3.0];
                flatten_cubic(&mut current, pos, c1, c2, end, tol);
                last_quad = Some(q);
                last_ctrl = None;
                pos = end;
            }
            b'A' => {
                for _ in 0..5 {
                    tokens.number()?;
                }
                pos = pt(&mut tokens)?;
                current.push(pos);
                last_ctrl = None;
                last_quad = None;
            }
            b'Z' => {
                if current.len() > 1 {
                    out.push((std::mem::take(&mut current), true));
                }
                pos = start;
                current.clear();
                current.push(pos);
                last_ctrl = None;
                last_quad = None;
            }
            other => return Err(format!("path data: unknown command `{}` at byte {}", other as char, tokens.at)),
        }
    }
    if current.len() > 1 {
        out.push((current, false));
    }
    Ok(out)
}

struct Tokens<'a> {
    s: &'a [u8],
    at: usize,
}

impl Tokens<'_> {
    fn skip_separators(&mut self) {
        while self.at < self.s.len() && (self.s[self.at].is_ascii_whitespace() || self.s[self.at] == b',') {
            self.at += 1;
        }
    }

    fn number(&mut self) -> Result<f64, String> {
        self.skip_separators();
        let begin = self.at;
        let mut seen_dot = false;
        let mut seen_e = false;
        while self.at < self.s.len() {
            let c = self.s[self.at];
            let first = self.at == begin;
            let after_e = self.at > begin && matches!(self.s[self.at - 1], b'e' | b'E');
            if c.is_ascii_digit() || ((c == b'-' || c == b'+') && (first || after_e)) {
            } else if c == b'.' && !seen_dot && !seen_e {
                seen_dot = true;
            } else if (c == b'e' || c == b'E') && !seen_e && !first {
                seen_e = true;
            } else {
                break;
            }
            self.at += 1;
        }
        std::str::from_utf8(&self.s[begin..self.at])
            .ok()
            .and_then(|t| t.parse::<f64>().ok())
            .ok_or_else(|| format!("path data: expected a number at byte {begin}"))
    }
}

fn flatten_cubic(out: &mut Vec<[f64; 2]>, p0: [f64; 2], p1: [f64; 2], p2: [f64; 2], p3: [f64; 2], tol: f64) {
    // Segments from the control polygon's deviation (Wang's bound).
    let dd = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| ((a[0] - 2.0 * b[0] + c[0]).powi(2) + (a[1] - 2.0 * b[1] + c[1]).powi(2)).sqrt();
    let m = dd(p0, p1, p2).max(dd(p1, p2, p3));
    let n = ((0.75 * m / tol).sqrt().ceil() as usize).clamp(1, 4096);
    for k in 1..=n {
        let t = k as f64 / n as f64;
        let u = 1.0 - t;
        let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        out.push([a * p0[0] + b * p1[0] + c * p2[0] + d * p3[0], a * p0[1] + b * p1[1] + c * p2[1] + d * p3[1]]);
    }
}

/// One drawn piece of a stroke: a segment with the radius at each end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokeSegment {
    pub a: [f64; 2],
    pub b: [f64; 2],
    pub ra: f64,
    pub rb: f64,
}

/// The trim window's pieces of `lengths`-long path, as (from, to) in path
/// length: `[start, end]` shifted by `offset` (fractions of the length);
/// closed paths wrap around, open ones clip.
pub fn trim_window(total: f64, start: f64, end: f64, offset: f64, closed: bool) -> Vec<(f64, f64)> {
    let (s, e) = (start.clamp(0.0, 1.0).min(end.clamp(0.0, 1.0)), end.clamp(0.0, 1.0).max(start.clamp(0.0, 1.0)));
    if e - s <= 0.0 || total <= 0.0 {
        return Vec::new();
    }
    if closed {
        let s = (s + offset).rem_euclid(1.0);
        let e2 = s + (e - (start.clamp(0.0, 1.0).min(end.clamp(0.0, 1.0))));
        if e2 <= 1.0 {
            vec![(s * total, e2 * total)]
        } else {
            vec![(s * total, total), (0.0, (e2 - 1.0) * total)]
        }
    } else {
        let (s, e) = ((s + offset).clamp(0.0, 1.0), (e + offset).clamp(0.0, 1.0));
        if e > s { vec![(s * total, e * total)] } else { Vec::new() }
    }
}

/// A polyline trimmed to `[start, end]` (+ `offset`) of its length, with
/// the width tapering from 0 over the first `taper_start` and to 0 over the
/// last `taper_end` of the drawn length.
#[allow(clippy::too_many_arguments)]
pub fn stroke_segments(points: &[[f64; 2]], closed: bool, width: f64, start: f64, end: f64, offset: f64, taper_start: f64, taper_end: f64) -> Vec<StrokeSegment> {
    let mut pts: Vec<[f64; 2]> = points.to_vec();
    if closed && pts.len() > 2 && pts.first() != pts.last() {
        pts.push(pts[0]);
    }
    if pts.len() < 2 {
        return Vec::new();
    }
    let lens: Vec<f64> = pts.windows(2).map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt()).collect();
    let total: f64 = lens.iter().sum();
    let windows = trim_window(total, start, end, offset, closed);
    let drawn: f64 = windows.iter().map(|(a, b)| b - a).sum();
    let radius = width.max(0.0) * 0.5;
    let taper = |d: f64| {
        // d: distance along the drawn length.
        let ts = taper_start.clamp(0.0, 1.0) * drawn;
        let te = taper_end.clamp(0.0, 1.0) * drawn;
        let mut r = radius;
        if ts > 0.0 && d < ts {
            r *= d / ts;
        }
        if te > 0.0 && d > drawn - te {
            r *= ((drawn - d) / te).max(0.0);
        }
        r
    };
    let point_at = |l: f64| {
        let mut acc = 0.0;
        for (k, len) in lens.iter().enumerate() {
            if l <= acc + len || k == lens.len() - 1 {
                let u = if *len > 0.0 { ((l - acc) / len).clamp(0.0, 1.0) } else { 0.0 };
                let (a, b) = (pts[k], pts[k + 1]);
                return [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u];
            }
            acc += len;
        }
        pts[pts.len() - 1]
    };
    let mut out = Vec::new();
    let mut drawn_before = 0.0;
    for (from, to) in windows {
        // Split at the polyline's vertices inside the window.
        let mut cuts = vec![from];
        let mut acc = 0.0;
        for len in &lens {
            acc += len;
            if acc > from && acc < to {
                cuts.push(acc);
            }
        }
        cuts.push(to);
        // Long straight pieces are split further so the taper stays smooth.
        let step = (drawn / 64.0).max(1.0);
        let mut fine = Vec::new();
        for w in cuts.windows(2) {
            let n = (((w[1] - w[0]) / step).ceil() as usize).clamp(1, 4096);
            for k in 0..n {
                fine.push(w[0] + (w[1] - w[0]) * k as f64 / n as f64);
            }
        }
        fine.push(to);
        for w in fine.windows(2) {
            if w[1] - w[0] <= 1e-9 {
                continue;
            }
            out.push(StrokeSegment {
                a: point_at(w[0]),
                b: point_at(w[1]),
                ra: taper(drawn_before + w[0] - from),
                rb: taper(drawn_before + w[1] - from),
            });
        }
        drawn_before += to - from;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_paths_parse_and_flatten() {
        let paths = parse_svg("M10 10 H 110 v100 l-100 0 Z", 0.25).unwrap();
        assert_eq!(paths, vec![(vec![[10.0, 10.0], [110.0, 10.0], [110.0, 110.0], [10.0, 110.0]], true)]);
        let curve = parse_svg("M0,0 C 0,100 100,100 100,0", 0.1).unwrap();
        let (pts, closed) = &curve[0];
        assert!(!closed && pts.len() > 8);
        assert_eq!(*pts.last().unwrap(), [100.0, 0.0]);
        let peak = pts.iter().map(|p| p[1]).fold(0.0, f64::max);
        assert!((peak - 75.0).abs() < 0.2, "a symmetric cubic peaks at 3/4 of its control height: {peak}");
        let smooth = parse_svg("M0 0 Q 50 -50 100 0 T 200 0", 0.1).unwrap();
        assert!(smooth[0].0.iter().any(|p| p[1] > 20.0), "T mirrors the control point");
        assert!(parse_svg("M0 0 L 10", 0.1).unwrap_err().contains("expected a number"));
        assert!(parse_svg("M0 0 X 1 1", 0.1).unwrap_err().contains("unknown command"));
        assert_eq!(parse_svg("M1e1-2.5.5 1", 0.1).unwrap()[0].0, vec![[10.0, -2.5], [0.5, 1.0]]);
    }

    #[test]
    fn trimming_draws_the_right_part() {
        let line = [[0.0, 0.0], [100.0, 0.0]];
        let segs = stroke_segments(&line, false, 10.0, 0.0, 0.5, 0.0, 0.0, 0.0);
        let far = segs.iter().map(|s| s.b[0]).fold(0.0, f64::max);
        assert!((far - 50.0).abs() < 1e-9);
        assert!(segs.iter().all(|s| s.ra == 5.0 && s.rb == 5.0));
        assert!(stroke_segments(&line, false, 10.0, 0.3, 0.3, 0.0, 0.0, 0.0).is_empty());
        // A closed square trimmed across its start wraps around.
        assert_eq!(trim_window(400.0, 0.0, 0.5, 0.75, true), vec![(300.0, 400.0), (0.0, 100.0)]);
        assert_eq!(trim_window(400.0, 0.0, 0.5, 0.75, false), vec![(300.0, 400.0)]);
    }

    #[test]
    fn tapers_reach_zero_at_the_ends() {
        let line = [[0.0, 0.0], [200.0, 0.0]];
        let segs = stroke_segments(&line, false, 20.0, 0.0, 1.0, 0.0, 0.25, 0.25);
        assert_eq!(segs.first().unwrap().ra, 0.0);
        assert!(segs.last().unwrap().rb.abs() < 1e-9);
        let widest = segs.iter().map(|s| s.ra).fold(0.0, f64::max);
        assert_eq!(widest, 10.0);
        // Monotone growth over the first quarter.
        let head: Vec<f64> = segs.iter().filter(|s| s.a[0] < 50.0).map(|s| s.ra).collect();
        assert!(head.windows(2).all(|w| w[1] >= w[0]));
    }
}

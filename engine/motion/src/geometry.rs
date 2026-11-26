//! Vector geometry of shape layers, without a GPU: the outline of a Rect,
//! Ellipse or Path in its own coordinates, mapped through an affine,
//! bounds (for inspectors: out-of-frame layers), and trimming a path to a
//! part of its length (a path "drawing itself").

use crate::frame::Affine;
pub use makepad_svg::path::{PathCmd, VectorPath};
/// Parsed SVG path data (M L H V C S Q T A Z, absolute and relative),
/// arcs as cubic Béziers: makepad-svg's parser.
pub fn parse_path(data: &str) -> VectorPath {
    let mut path = VectorPath::new();
    makepad_svg::path_data::parse_path_data(data, &mut path);
    path
}

/// A loaded SVG command list with slots in the keyed number arrays. Implicit
/// repetitions are expanded (extra moveto pairs become lineto commands), but
/// relative coordinates, smooth controls and arcs stay in SVG form until eval.
/// Replaying through SVG's own builder preserves its f32 arithmetic and arc
/// subdivision; interpolating already-expanded Béziers would change the arcs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PathTemplate {
    commands: Vec<PathCommand>,
    capacity: usize,
}

#[derive(Clone, Debug, PartialEq)]
struct PathCommand {
    kind: u8,
    numbers: std::ops::Range<usize>,
}

impl PathTemplate {
    pub fn parse(data: &str) -> Result<(Self, Vec<f64>), String> {
        let bytes = data.as_bytes();
        let mut pos = 0;
        let mut command = 0u8;
        let mut template = Self::default();
        let mut numbers = Vec::new();
        let skip = |pos: &mut usize| {
            while *pos < bytes.len() && (bytes[*pos].is_ascii_whitespace() || bytes[*pos] == b',') {
                *pos += 1;
            }
        };
        loop {
            skip(&mut pos);
            if pos == bytes.len() {
                break;
            }
            if bytes[pos].is_ascii_alphabetic() {
                command = bytes[pos];
                pos += 1;
            }
            let count = match command.to_ascii_uppercase() {
                b'M' | b'L' | b'T' => 2,
                b'H' | b'V' => 1,
                b'C' => 6,
                b'S' | b'Q' => 4,
                b'A' => 7,
                b'Z' => 0,
                _ => return Err("expected an SVG command (M L H V C S Q T A Z)".into()),
            };
            let start = numbers.len();
            for slot in 0..count {
                skip(&mut pos);
                // SVG flags consume one byte, including adjacent flags and
                // an endpoint without separators, e.g. `A10 20 0 0110 30`.
                if command.eq_ignore_ascii_case(&b'A') && (slot == 3 || slot == 4) {
                    match bytes.get(pos) {
                        Some(b'0' | b'1') => {
                            numbers.push((bytes[pos] - b'0') as f64);
                            pos += 1;
                            continue;
                        }
                        _ => return Err("an SVG arc flag must be 0 or 1".into()),
                    }
                }
                let begin = pos;
                if matches!(bytes.get(pos), Some(b'+' | b'-')) {
                    pos += 1;
                }
                while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
                    pos += 1;
                }
                if bytes.get(pos) == Some(&b'.') {
                    pos += 1;
                    while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
                        pos += 1;
                    }
                }
                if matches!(bytes.get(pos), Some(b'e' | b'E')) {
                    pos += 1;
                    if matches!(bytes.get(pos), Some(b'+' | b'-')) {
                        pos += 1;
                    }
                    while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
                        pos += 1;
                    }
                }
                let value = data[begin..pos].parse::<f64>().map_err(|_| {
                    format!("SVG command `{}` expects {count} numbers (missing or invalid number {})", command as char, slot + 1)
                })?;
                numbers.push(value);
            }
            template.commands.push(PathCommand { kind: command, numbers: start..numbers.len() });
            // One output command per ordinary command, up to four per arc.
            template.capacity += if command.eq_ignore_ascii_case(&b'A') { 4 } else { 1 };
            command = match command {
                b'M' => b'L',
                b'm' => b'l',
                b'Z' | b'z' => 0,
                other => other,
            };
        }
        Ok((template, numbers))
    }

    /// Only the resulting path owns an allocation. Operand interpolation uses
    /// a stack buffer sized for SVG's largest command (the seven-value arc).
    /// Arc flags are discrete: choose the nearer key, never a fractional flag.
    pub fn interpolate(&self, a: &[f64], b: &[f64], u: f64) -> VectorPath {
        let mut path = VectorPath { cmds: Vec::with_capacity(self.capacity), fill_rule: None };
        let mut builder = makepad_svg::path_data::PathDataBuilder::default();
        let mut values = [0.0f32; 7];
        for command in &self.commands {
            for (slot, index) in command.numbers.clone().enumerate() {
                values[slot] = if command.kind.eq_ignore_ascii_case(&b'A') && (slot == 3 || slot == 4) {
                    if u < 0.5 { a[index] as f32 } else { b[index] as f32 }
                } else {
                    (a[index] + (b[index] - a[index]) * u) as f32
                };
            }
            builder.command(command.kind, &values[..command.numbers.len()], &mut path);
        }
        path
    }
}

/// Closed polygons as a path: one `M`, the `L`s and `Z` per contour.
pub fn polygons_path(contours: &[Vec<[f64; 2]>]) -> VectorPath {
    let mut path = VectorPath::new();
    for contour in contours {
        for (i, p) in contour.iter().enumerate() {
            if i == 0 {
                path.move_to(p[0] as f32, p[1] as f32);
            } else {
                path.line_to(p[0] as f32, p[1] as f32);
            }
        }
        path.close();
    }
    path
}

pub fn rect_path(size: [f64; 2], radius: f64) -> VectorPath {
    let mut path = VectorPath::new();
    let r = radius.min(size[0] * 0.5).min(size[1] * 0.5).max(0.0);
    path.rounded_rect(0.0, 0.0, size[0] as f32, size[1] as f32, r as f32);
    path.close();
    path
}

/// An ellipse filling `size`, drawn clockwise from 12 o'clock (so a trim
/// starts at the top).
pub fn ellipse_path(size: [f64; 2]) -> VectorPath {
    use makepad_svg::path_data::arc_to_beziers;
    let (w, h) = (size[0] as f32, size[1] as f32);
    let (rx, ry) = (w * 0.5, h * 0.5);
    let mut path = VectorPath::new();
    path.move_to(rx, 0.0);
    arc_to_beziers(&mut path, rx, 0.0, rx, ry, 0.0, false, true, w, ry);
    arc_to_beziers(&mut path, w, ry, rx, ry, 0.0, false, true, rx, h);
    arc_to_beziers(&mut path, rx, h, rx, ry, 0.0, false, true, 0.0, ry);
    arc_to_beziers(&mut path, 0.0, ry, rx, ry, 0.0, false, true, rx, 0.0);
    path.close();
    path
}

/// `path` with every point mapped through `m` (exact for lines and
/// Béziers under an affine).
pub fn transformed(path: &VectorPath, m: &Affine) -> VectorPath {
    let p = |x: f32, y: f32| {
        let q = m.apply([x as f64, y as f64]);
        (q[0] as f32, q[1] as f32)
    };
    let cmds = path.cmds.iter().map(|cmd| match cmd {
        PathCmd::MoveTo(x, y) => {
            let (x, y) = p(*x, *y);
            PathCmd::MoveTo(x, y)
        }
        PathCmd::LineTo(x, y) => {
            let (x, y) = p(*x, *y);
            PathCmd::LineTo(x, y)
        }
        PathCmd::BezierTo(a, b, c, d, x, y) => {
            let (a, b) = p(*a, *b);
            let (c, d) = p(*c, *d);
            let (x, y) = p(*x, *y);
            PathCmd::BezierTo(a, b, c, d, x, y)
        }
        other => other.clone(),
    }).collect();
    VectorPath { cmds, fill_rule: path.fill_rule }
}

/// `path` without segments shorter than `min` (the stroker turns a
/// near-zero last segment, like the end of a nearly full circle arc, into
/// NaN joins and draws nothing).
pub fn without_tiny_segments(path: &VectorPath, min: f64) -> VectorPath {
    let mut cmds = Vec::with_capacity(path.cmds.len());
    let mut at: Option<(f32, f32)> = None;
    let far = |a: (f32, f32), b: (f32, f32)| (((a.0 - b.0) as f64).powi(2) + ((a.1 - b.1) as f64).powi(2)).sqrt() >= min;
    for cmd in &path.cmds {
        match cmd {
            PathCmd::MoveTo(x, y) => {
                at = Some((*x, *y));
                cmds.push(cmd.clone());
            }
            PathCmd::LineTo(x, y) | PathCmd::BezierTo(_, _, _, _, x, y) => {
                if at.is_none_or(|p| far(p, (*x, *y))) || matches!(cmd, PathCmd::BezierTo(a, b, c, d, _, _) if at.is_some_and(|p| far(p, (*a, *b)) || far(p, (*c, *d)))) {
                    at = Some((*x, *y));
                    cmds.push(cmd.clone());
                }
            }
            other => cmds.push(other.clone()),
        }
    }
    VectorPath { cmds, fill_rule: path.fill_rule }
}

/// The path as polylines, one per subpath, with whether each is closed.
/// Curves are split until each piece is within `tolerance` of the curve.
pub fn flatten(path: &VectorPath, tolerance: f64) -> Vec<(Vec<[f64; 2]>, bool)> {
    let mut out: Vec<(Vec<[f64; 2]>, bool)> = Vec::new();
    let mut current: Vec<[f64; 2]> = Vec::new();
    let tol = tolerance.max(1e-4);
    for cmd in &path.cmds {
        match cmd {
            PathCmd::MoveTo(x, y) => {
                if current.len() > 1 {
                    out.push((std::mem::take(&mut current), false));
                }
                current = vec![[*x as f64, *y as f64]];
            }
            PathCmd::LineTo(x, y) => {
                if current.is_empty() {
                    current.push([0.0, 0.0]);
                }
                current.push([*x as f64, *y as f64]);
            }
            PathCmd::BezierTo(a, b, c, d, x, y) => {
                let p0 = *current.last().unwrap_or(&[0.0, 0.0]);
                if current.is_empty() {
                    current.push(p0);
                }
                let (p1, p2, p3) = ([*a as f64, *b as f64], [*c as f64, *d as f64], [*x as f64, *y as f64]);
                // Segments from the control polygon's length and curvature.
                let dd = |p: [f64; 2], q: [f64; 2]| ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt();
                let deviation = dd(p0, p1) + dd(p1, p2) + dd(p2, p3) - dd(p0, p3);
                let n = ((deviation / tol).sqrt() * 2.0 + dd(p0, p3) / (tol * 64.0)).ceil().clamp(1.0, 512.0) as usize;
                for k in 1..=n {
                    let t = k as f64 / n as f64;
                    let u = 1.0 - t;
                    let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    current.push([
                        w0 * p0[0] + w1 * p1[0] + w2 * p2[0] + w3 * p3[0],
                        w0 * p0[1] + w1 * p1[1] + w2 * p2[1] + w3 * p3[1],
                    ]);
                }
            }
            PathCmd::Close => {
                if let Some(first) = current.first().copied() {
                    if current.last() != Some(&first) {
                        current.push(first);
                    }
                    if current.len() > 1 {
                        out.push((std::mem::take(&mut current), true));
                    }
                    current = vec![first];
                }
            }
            PathCmd::Winding(_) => {}
        }
    }
    if current.len() > 1 {
        out.push((current, false));
    }
    out
}

/// `[min_x, min_y, max_x, max_y]` of a path, `None` when it is empty.
pub fn bounds(path: &VectorPath) -> Option<[f64; 4]> {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    let mut any = false;
    for (points, _) in flatten(path, 0.25) {
        for p in points {
            any = true;
            b = [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])];
        }
    }
    any.then_some(b)
}

/// The total length of a path's subpaths.
pub fn length(path: &VectorPath, tolerance: f64) -> f64 {
    flatten(path, tolerance).iter().map(|(points, _)| polyline_length(points)).sum()
}

fn polyline_length(points: &[[f64; 2]]) -> f64 {
    points.windows(2).map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt()).sum()
}

/// The part of `path` between fractions `from` and `to` of its whole
/// length (subpaths in order), as open polylines. `[0, 1]` is the whole
/// path (closed subpaths stay closed).
pub fn trim(path: &VectorPath, from: f64, to: f64, tolerance: f64) -> VectorPath {
    let (from, to) = (from.clamp(0.0, 1.0), to.clamp(0.0, 1.0));
    if from <= 0.0 && to >= 1.0 {
        return path.clone();
    }
    let mut out = VectorPath::new();
    if to <= from {
        return out;
    }
    let pieces = flatten(path, tolerance);
    let total: f64 = pieces.iter().map(|(p, _)| polyline_length(p)).sum();
    if total <= 0.0 {
        return out;
    }
    let (a, b) = (from * total, to * total);
    let mut offset = 0.0;
    for (points, _) in pieces {
        let mut open = false;
        for w in points.windows(2) {
            let seg = ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt();
            let (s0, s1) = (offset, offset + seg);
            offset = s1;
            if s1 <= a || s0 >= b || seg <= 0.0 {
                if s0 >= b {
                    break;
                }
                continue;
            }
            let lerp = |s: f64| {
                let u = ((s - s0) / seg).clamp(0.0, 1.0);
                [w[0][0] + (w[1][0] - w[0][0]) * u, w[0][1] + (w[1][1] - w[0][1]) * u]
            };
            let start = lerp(a.max(s0));
            let end = lerp(b.min(s1));
            if !open {
                out.move_to(start[0] as f32, start[1] as f32);
                open = true;
            }
            out.line_to(end[0] as f32, end[1] as f32);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_by_length_across_subpaths() {
        let path = parse_path("M0 0 L100 0 M0 10 L100 10");
        assert!((length(&path, 0.1) - 200.0).abs() < 1e-9);
        let half = trim(&path, 0.0, 0.5, 0.1);
        assert_eq!(bounds(&half).unwrap(), [0.0, 0.0, 100.0, 0.0]);
        let middle = trim(&path, 0.25, 0.75, 0.1);
        let pieces = flatten(&middle, 0.1);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].0, vec![[50.0, 0.0], [100.0, 0.0]]);
        assert_eq!(pieces[1].0, vec![[0.0, 10.0], [50.0, 10.0]]);
        assert!(trim(&path, 0.5, 0.5, 0.1).cmds.is_empty());
    }

    #[test]
    fn relative_commands_and_curves_parse() {
        let p = parse_path("m10 10 h 20 v 10 l -20 0 z M0 0 Q 10 20 20 0 T 40 0 C 50 10 60 10 70 0 S 90 -10 100 0");
        let b = bounds(&p).unwrap();
        assert!((b[0] - 0.0).abs() < 1e-6 && (b[2] - 100.0).abs() < 1e-6, "{b:?}");
        assert!((b[1] + 10.0).abs() < 1.0 && (b[3] - 20.0).abs() < 1.0, "{b:?}");
        // Flags written without separators.
        let arc = bounds(&parse_path("M0 0 a10 10 0 1110 0")).unwrap();
        assert!(arc[3] - arc[1] > 15.0, "a large arc: {arc:?}");
    }

    #[test]
    fn numeric_templates_replay_the_svg_parsers_commands() {
        for data in [
            "m10 20 5 6 -2-3 h7 9 v-4 -2 H50 V60 l1 2 L40 50 Z m1 2 l3 4 z",
            "M0 0 C1 2 3 4 5 6 7 8 9 10 11 12 S13 14 15 16 17 18 19 20 c1 2 3 4 5 6 s7 8 9 10",
            "M1 2 Q3 4 5 6 7 8 9 10 T11 12 13 14 q1 2 3 4 t5 6 Z q2 3 4 5 t6 7",
            "M10 20 A30 15 37 0110 30 a12 8 -22 10-5-6 A0 10 0 0 1 20 30 Z",
            "M.5.25 L1e2-2E-1 l+3.5-.75 Z M0 0 a10 10 0 1110 0",
        ] {
            let (template, numbers) = PathTemplate::parse(data).unwrap();
            assert_eq!(template.interpolate(&numbers, &numbers, 0.0), parse_path(data), "{data}");
        }
    }

    #[test]
    fn arc_flags_remain_discrete_and_templates_reject_missing_operands() {
        let (template, a) = PathTemplate::parse("M0 0 A10 15 20 0 0 10 12").unwrap();
        let (_, b) = PathTemplate::parse("M0 0 A10 15 20 1 1 10 12").unwrap();
        for (u, flags) in [(0.0, "0 0"), (0.49, "0 0"), (0.5, "1 1"), (1.0, "1 1")] {
            assert_eq!(template.interpolate(&a, &b, u), parse_path(&format!("M0 0 A10 15 20 {flags} 10 12")));
        }
        for data in ["M0 0 C1 2 3", "M0 0 A10 10 0 2 0 1 1", "M0 0 Z 1 2"] {
            assert!(PathTemplate::parse(data).is_err(), "{data}");
        }
    }

    #[test]
    fn arcs_and_rects_have_their_size() {
        let ring = parse_path("M 0 -160 A 160 160 0 1 1 -0.01 -160");
        let b = bounds(&ring).unwrap();
        assert!((b[0] + 160.0).abs() < 1.0 && (b[2] - 160.0).abs() < 1.0 && (b[3] - 160.0).abs() < 1.0, "{b:?}");
        assert!((length(&ring, 0.05) - 2.0 * std::f64::consts::PI * 160.0).abs() < 2.0);
        let rect = rect_path([40.0, 20.0], 5.0);
        assert_eq!(bounds(&rect).unwrap(), [0.0, 0.0, 40.0, 20.0]);
        let moved = transformed(&rect, &Affine::translate(10.0, 5.0).then(&Affine::scale(2.0, 2.0)));
        assert_eq!(bounds(&moved).unwrap(), [10.0, 5.0, 90.0, 45.0]);
    }
}

/// Outlines of any shapes made morphable: aligned ([`crate::align`]:
/// contours paired, segments split to equal counts, start and winding
/// chosen to travel least), so they share their commands (`M`, then `C`s,
/// `Z` where closed) and tween number by number: a play icon into a pause
/// icon, a logo into a word, a square into a circle into a glyph.
pub fn morphable(datas: &[&str]) -> Vec<String> {
    let shapes: Vec<_> = datas.iter().map(|data| crate::align::shape_of(&parse_path(data))).collect();
    crate::align::align_shapes(&shapes).iter().map(|shape| {
        let mut data = String::new();
        for contour in shape {
            let Some(first) = contour.segs.first() else { continue };
            data.push_str(&format!("M {:.3} {:.3} ", first[0][0], first[0][1]));
            for s in &contour.segs {
                data.push_str(&format!("C {:.3} {:.3} {:.3} {:.3} {:.3} {:.3} ", s[1][0], s[1][1], s[2][0], s[2][1], s[3][0], s[3][1]));
            }
            if contour.closed {
                data.push_str("Z ");
            }
        }
        data.trim_end().to_string()
    }).collect()
}

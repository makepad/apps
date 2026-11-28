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

/// SVG path data through `points`: lines, or a Catmull-Rom curve through
/// them (`smooth`), closed back to the first point (`closed`).
pub fn points_path(points: &[[f64; 2]], closed: bool, smooth: bool) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let n = points.len();
    if n == 0 {
        return out;
    }
    let _ = write!(out, "M{} {}", points[0][0], points[0][1]);
    let segments = if closed { n } else { n - 1 };
    for i in 0..segments {
        let p1 = points[i];
        let p2 = points[(i + 1) % n];
        if smooth {
            let p0 = if i == 0 && !closed { p1 } else { points[(i + n - 1) % n] };
            let p3 = if i + 2 >= n && !closed { p2 } else { points[(i + 2) % n] };
            let c1 = [p1[0] + (p2[0] - p0[0]) / 6.0, p1[1] + (p2[1] - p0[1]) / 6.0];
            let c2 = [p2[0] - (p3[0] - p1[0]) / 6.0, p2[1] - (p3[1] - p1[1]) / 6.0];
            let _ = write!(out, " C{} {} {} {} {} {}", c1[0], c1[1], c2[0], c2[1], p2[0], p2[1]);
        } else {
            let _ = write!(out, " L{} {}", p2[0], p2[1]);
        }
    }
    if closed {
        out.push_str(" Z");
    }
    out
}

/// Whether SVG path data parses: commands and the numbers they take.
pub fn check_path_data(data: &str) -> Result<(), String> {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return Err("`svg` path data is empty".into());
    }
    let first = trimmed.chars().next().unwrap();
    if !matches!(first, 'M' | 'm') {
        return Err(format!("path data starts with a move (`M x y`), not `{}`", crate::truncate(trimmed, 12)));
    }
    for c in trimmed.chars() {
        if !(c.is_ascii_digit() || c.is_ascii_whitespace() || "MmLlHhVvCcSsQqTtAaZz.,-+eE".contains(c)) {
            return Err(format!("`{c}` is not part of SVG path data (commands M L H V C S Q T A Z and numbers)"));
        }
    }
    Ok(())
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

/// The point `progress` (0..1) of the way along SVG path data, by length.
pub fn point_along(data: &str, progress: f64) -> [f64; 2] {
    Polyline::of(data).point(progress)
}

/// A path flattened once, for many points along it (a `Repeat` along a path).
#[derive(Clone, Debug, PartialEq)]
pub struct Polyline {
    pieces: Vec<(Vec<[f64; 2]>, bool)>,
    total: f64,
}

impl Polyline {
    pub fn of(data: &str) -> Self {
        Self::from_path(&crate::geometry::parse_path(data))
    }

    pub fn from_path(path: &makepad_svg::VectorPath) -> Self {
        let pieces = crate::geometry::flatten(path, 0.1);
        let total = pieces.iter().map(|(p, _)| p.windows(2).map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt()).sum::<f64>()).sum();
        Self { pieces, total }
    }

    pub fn length(&self) -> f64 {
        self.total
    }

    pub fn point(&self, progress: f64) -> [f64; 2] {
        self.sample(progress).0
    }

    /// Arc-length position and unit tangent, including either endpoint.
    pub fn sample(&self, progress: f64) -> ([f64; 2], [f64; 2]) {
        let target = progress.clamp(0.0, 1.0) * self.total;
        let mut walked = 0.0;
        let mut last = ([0.0, 0.0], [1.0, 0.0]);
        for (points, _) in &self.pieces {
            for w in points.windows(2) {
                let delta = [w[1][0] - w[0][0], w[1][1] - w[0][1]];
                let seg = delta[0].hypot(delta[1]);
                if seg == 0.0 { last.0 = w[1]; continue; }
                let tangent = [delta[0] / seg, delta[1] / seg];
                if walked + seg >= target {
                    let u = (target - walked) / seg;
                    return ([w[0][0] + delta[0] * u, w[0][1] + delta[1] * u], tangent);
                }
                walked += seg;
                last = (w[1], tangent);
            }
        }
        last
    }
}

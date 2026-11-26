//! Pure plotting geometry. Sampling uses screen distances; returned points stay
//! in data coordinates, including interpolated intersections with the box.

use std::collections::HashMap;
use std::fmt::Write;

pub type Point = [f64; 2];

/// An axis-aligned box, in either data or screen coordinates.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub x: [f64; 2],
    pub y: [f64; 2],
}

impl Bounds {
    pub fn valid(self) -> bool {
        self.x.into_iter().chain(self.y).all(f64::is_finite)
            && self.x[0] < self.x[1] && self.y[0] < self.y[1]
            && (self.x[1] - self.x[0]).is_finite() && (self.y[1] - self.y[0]).is_finite()
    }

    pub fn clamp(self, p: Point) -> Point {
        let clamp = |v: f64, r: [f64; 2]| if v.is_nan() { r[0] } else { v.clamp(r[0], r[1]) };
        [clamp(p[0], self.x), clamp(p[1], self.y)]
    }

    fn nearby(self, p: Point) -> bool {
        [self.x, self.y].iter().zip(p).all(|(r, v)| {
            v.is_finite() && v >= r[0] - (r[1] - r[0]) * 0.25 && v <= r[1] + (r[1] - r[0]) * 0.25
        })
    }
}

/// Multiples of a positive step inside an inclusive range (zero is exact).
pub fn ticks_at(lo: f64, hi: f64, step: f64) -> Vec<f64> {
    if !lo.is_finite() || !hi.is_finite() || !step.is_finite() || step <= 0.0 || hi <= lo {
        return Vec::new();
    }
    let first = (lo / step - 1e-10).ceil();
    let last = (hi / step + 1e-10).floor();
    (0..((last - first + 1.0).max(0.0) as usize)).map(|i| {
        let v = (first + i as f64) * step;
        if v.abs() < step * 1e-10 { 0.0 } else { v }
    }).collect()
}

/// A 1–2–5 step near `target` intervals and its tick values.
pub fn nice_ticks(lo: f64, hi: f64, target: usize) -> (f64, Vec<f64>) {
    if !lo.is_finite() || !hi.is_finite() || hi <= lo || !(hi - lo).is_finite() {
        return (1.0, Vec::new());
    }
    let raw = ((hi - lo) / target.max(1) as f64).max(f64::from_bits(1));
    let decade = 10.0_f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0].into_iter().map(|m| m * decade)
        .filter(|step| step.is_finite() && *step > 0.0)
        .min_by(|a, b| (raw / a).ln().abs().total_cmp(&(raw / b).ln().abs())).unwrap_or(raw);
    (step, ticks_at(lo, hi, step))
}

pub fn mix(a: Point, b: Point, u: f64) -> Point {
    [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u]
}

pub fn finite(p: Point) -> bool { p.into_iter().all(f64::is_finite) }

/// Fractions of a DATA segment, refined by the mapped midpoint's distance
/// from the mapped chord midpoint. Call with the full-strength map, then reuse
/// these fractions when interpolating from the original to the mapped points.
/// Initial intervals are at most `max_length`; each gets at most eight splits.
pub fn map_subdivision<E>(
    a: Point, b: Point, map: &mut impl FnMut(Point) -> Result<Point, E>,
    tolerance: f64, max_length: f64,
) -> Result<Vec<f64>, E> {
    fn split<E>(
        a: Point, b: Point, map: &mut impl FnMut(Point) -> Result<Point, E>,
        interval: [f64; 2], ends: [Point; 2], tolerance: f64, depth: usize, out: &mut Vec<f64>,
    ) -> Result<(), E> {
        let mid = (interval[0] + interval[1]) * 0.5;
        let p = map(mix(a, b, mid))?;
        let all_finite = finite(ends[0]) && finite(p) && finite(ends[1]);
        let chord = mix(ends[0], ends[1], 0.5);
        let refine = if all_finite {
            (p[0] - chord[0]).hypot(p[1] - chord[1]) > tolerance
        } else { finite(ends[0]) || finite(p) || finite(ends[1]) };
        if refine && depth < 8 {
            split(a, b, map, [interval[0], mid], [ends[0], p], tolerance, depth + 1, out)?;
            split(a, b, map, [mid, interval[1]], [p, ends[1]], tolerance, depth + 1, out)?;
        } else {
            // Retain invalid midpoints even at the depth limit: they must break
            // the line, including when both endpoints happen to be finite.
            if !finite(p) { out.push(mid); }
            out.push(interval[1]);
        }
        Ok(())
    }
    if !finite(a) || !finite(b) { return Ok(vec![0.0, 1.0]); }
    let length = (b[0] - a[0]).hypot(b[1] - a[1]);
    let count = if max_length.is_finite() && max_length > 0.0 && length.is_finite() {
        (length / max_length).ceil().max(1.0) as usize
    } else { 1 };
    let mut out = vec![0.0];
    let mut previous = map(a)?;
    for i in 0..count {
        let interval = [i as f64 / count as f64, (i + 1) as f64 / count as f64];
        let next = map(mix(a, b, interval[1]))?;
        split(a, b, map, interval, [previous, next], tolerance, 0, &mut out)?;
        previous = next;
    }
    Ok(out)
}

/// Fractions of the segment inside the box; intersections are exact on lines.
pub fn clip_segment(a: Point, b: Point, bounds: Bounds) -> Option<(f64, f64)> {
    if !a.into_iter().chain(b).all(f64::is_finite) { return None; }
    let (mut enter, mut leave) = (0.0_f64, 1.0_f64);
    for (i, r) in [bounds.x, bounds.y].into_iter().enumerate() {
        let d = b[i] - a[i];
        if d.abs() < 1e-14 {
            if a[i] < r[0] || a[i] > r[1] { return None; }
        } else {
            let (u, v) = ((r[0] - a[i]) / d, (r[1] - a[i]) / d);
            enter = enter.max(u.min(v));
            leave = leave.min(u.max(v));
            if enter > leave { return None; }
        }
    }
    Some((enter, leave))
}

fn chord_error(a: Point, p: Point, b: Point) -> f64 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let len = d[0] * d[0] + d[1] * d[1];
    let u = if len > 0.0 { (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / len).clamp(0.0, 1.0) } else { 0.0 };
    let q = mix(a, b, u);
    (p[0] - q[0]).hypot(p[1] - q[1])
}

/// At least 64 initial intervals, recursively bisected to a 0.35 pixel chord
/// tolerance (depth 10). Invalid/far-out samples break the curve, never bridge a
/// pole. `to_screen` is the single mapping used for both error and clipping.
pub fn adaptive_curve(
    mut f: impl FnMut(f64) -> Option<Point>, s0: f64, s1: f64,
    to_screen: impl Fn(Point) -> Point, bounds: Bounds,
) -> Vec<Vec<Point>> {
    if !s0.is_finite() || !s1.is_finite() || s0 == s1 || !bounds.valid() { return Vec::new(); }
    let mut sample = |s| f(s).filter(|p| bounds.nearby(to_screen(*p)));
    fn split(
        f: &mut impl FnMut(f64) -> Option<Point>, map: &impl Fn(Point) -> Point,
        interval: [f64; 2], ends: [Option<Point>; 2], depth: usize, out: &mut Vec<Option<Point>>,
    ) {
        let m = (interval[0] + interval[1]) * 0.5;
        let p = f(m);
        let refine = match (ends[0], p, ends[1]) {
            (Some(a), Some(p), Some(b)) => chord_error(map(a), map(p), map(b)) > 0.35,
            (None, None, None) => false,
            _ => true,
        };
        if refine && depth < 10 {
            split(f, map, [interval[0], m], [ends[0], p], depth + 1, out);
            split(f, map, [m, interval[1]], [p, ends[1]], depth + 1, out);
        } else {
            // Keep the midpoint too: an unresolved discontinuity remains a break.
            out.push(p);
            out.push(ends[1]);
        }
    }
    let mut a = sample(s0);
    let mut samples = vec![a];
    for i in 0..64 {
        let interval = [s0 + (s1 - s0) * i as f64 / 64.0, s0 + (s1 - s0) * (i + 1) as f64 / 64.0];
        let b = sample(interval[1]);
        split(&mut sample, &to_screen, interval, [a, b], 0, &mut samples);
        a = b;
    }
    let mut lines = Vec::new();
    let mut line: Vec<Point> = Vec::new();
    for pair in samples.windows(2) {
        if let (Some(a), Some(b)) = (pair[0], pair[1]) {
            if let Some((u, v)) = clip_segment(to_screen(a), to_screen(b), bounds) {
                let (p, q) = (mix(a, b, u), mix(a, b, v));
                if line.last().is_some_and(|last| (last[0] - p[0]).hypot(last[1] - p[1]) > 1e-9) {
                    if line.len() > 1 { lines.push(std::mem::take(&mut line)); } else { line.clear(); }
                }
                if line.is_empty() { line.push(p); }
                line.push(q);
                continue;
            }
        }
        if line.len() > 1 { lines.push(std::mem::take(&mut line)); } else { line.clear(); }
    }
    if line.len() > 1 { lines.push(line); }
    lines
}

/// Clip a closed polygon, retaining its fill at the boundary.
pub fn clip_polygon(mut points: Vec<Point>, bounds: Bounds) -> Vec<Point> {
    if points.iter().flatten().any(|v| !v.is_finite()) { return Vec::new(); }
    for (axis, edge, greater) in [(0, bounds.x[0], true), (0, bounds.x[1], false), (1, bounds.y[0], true), (1, bounds.y[1], false)] {
        let mut out = Vec::new();
        if let Some(&last) = points.last() {
            let inside = |p: Point| if greater { p[axis] >= edge } else { p[axis] <= edge };
            let mut a = last;
            for &b in &points {
                if inside(a) != inside(b) {
                    out.push(mix(a, b, (edge - a[axis]) / (b[axis] - a[axis])));
                }
                if inside(b) { out.push(b); }
                a = b;
            }
        }
        points = out;
    }
    points
}

/// Zero contours on square data cells (`cells` across the longer side).
/// Shared edge/vertex identities join segments without a distance heuristic;
/// the function's centre value chooses the connections in saddle cells.
pub fn marching_squares(mut f: impl FnMut(f64, f64) -> f64, bounds: Bounds, cells: usize) -> Vec<(Vec<Point>, bool)> {
    if !bounds.valid() || cells == 0 { return Vec::new(); }
    let step = (bounds.x[1] - bounds.x[0]).max(bounds.y[1] - bounds.y[0]) / cells as f64;
    let nx = ((bounds.x[1] - bounds.x[0]) / step).ceil() as usize;
    let ny = ((bounds.y[1] - bounds.y[0]) / step).ceil() as usize;
    let point = |i: usize, j: usize| [
        (bounds.x[0] + i as f64 * step).min(bounds.x[1]),
        (bounds.y[0] + j as f64 * step).min(bounds.y[1]),
    ];
    let mut values = Vec::with_capacity((nx + 1) * (ny + 1));
    for j in 0..=ny { for i in 0..=nx { let p = point(i, j); values.push(f(p[0], p[1])); } }
    let mut nodes = Vec::<Point>::new();
    let mut ids = HashMap::<(usize, usize), usize>::new();
    let mut segments = Vec::<(usize, usize)>::new();
    for j in 0..ny {
        for i in 0..nx {
            let vertices = [j * (nx + 1) + i, j * (nx + 1) + i + 1, (j + 1) * (nx + 1) + i + 1, (j + 1) * (nx + 1) + i];
            let ps = [point(i, j), point(i + 1, j), point(i + 1, j + 1), point(i, j + 1)];
            let vs = vertices.map(|k| values[k]);
            if vs.iter().any(|v| !v.is_finite()) { continue; }
            let mut edges = [None; 4];
            for e in 0..4 {
                let k = (e + 1) % 4;
                if (vs[e] >= 0.0) == (vs[k] >= 0.0) { continue; }
                let u = vs[e] / (vs[e] - vs[k]);
                let key = if u == 0.0 { (vertices[e], vertices[e]) } else if u == 1.0 { (vertices[k], vertices[k]) }
                    else { (vertices[e].min(vertices[k]), vertices[e].max(vertices[k])) };
                let id = *ids.entry(key).or_insert_with(|| { nodes.push(mix(ps[e], ps[k], u)); nodes.len() - 1 });
                edges[e] = Some(id);
            }
            let crossing: Vec<usize> = edges.iter().flatten().copied().collect();
            let mut add = |a, b| { if a != b { segments.push((a, b)); } };
            if crossing.len() == 2 { add(crossing[0], crossing[1]); }
            if crossing.len() == 4 {
                let center = mix(ps[0], ps[2], 0.5);
                let value = f(center[0], center[1]);
                if !value.is_finite() { continue; }
                if (vs[0] >= 0.0) == (value >= 0.0) {
                    add(crossing[0], crossing[1]); add(crossing[2], crossing[3]);
                } else {
                    add(crossing[3], crossing[0]); add(crossing[1], crossing[2]);
                }
            }
        }
    }
    let mut adjacent = vec![Vec::new(); nodes.len()];
    for (e, &(a, b)) in segments.iter().enumerate() { adjacent[a].push((b, e)); adjacent[b].push((a, e)); }
    let mut used = vec![false; segments.len()];
    let mut result = Vec::new();
    let starts = (0..nodes.len()).filter(|&n| adjacent[n].len() != 2).chain(0..nodes.len());
    for start in starts {
        while let Some(&(next, edge)) = adjacent[start].iter().find(|(_, e)| !used[*e]) {
            let mut line = vec![nodes[start]];
            let (mut at, mut e) = (next, edge);
            loop {
                used[e] = true;
                if at == start { break; }
                line.push(nodes[at]);
                match adjacent[at].iter().find(|(_, e)| !used[*e]) {
                    Some(&(next, edge)) => { at = next; e = edge; }
                    None => break,
                }
            }
            if line.len() > 1 { result.push((line, at == start)); }
        }
    }
    result
}

/// A shaft and a two-segment arrowhead, with head length in the same units.
pub fn arrow_segments(a: Point, b: Point, head: f64) -> Vec<Vec<Point>> {
    let d = [b[0] - a[0], b[1] - a[1]];
    let length = d[0].hypot(d[1]);
    if length <= 1e-12 { return Vec::new(); }
    let h = head.min(length * 0.45);
    let u = [d[0] / length, d[1] / length];
    vec![vec![a, b], vec![
        [b[0] - h * u[0] - h * 0.5 * u[1], b[1] - h * u[1] + h * 0.5 * u[0]],
        b,
        [b[0] - h * u[0] + h * 0.5 * u[1], b[1] - h * u[1] - h * 0.5 * u[0]],
    ]]
}

/// Paths in one coordinate system, with a new `M` per contour and `Z` on loops.
pub fn polylines_to_svg(lines: &[(Vec<Point>, bool)]) -> String {
    let mut out = String::new();
    for (points, closed) in lines {
        if points.is_empty() { continue; }
        let mut pen = false;
        let mut broken = false;
        for &p in points {
            if !finite(p) { pen = false; broken = true; continue; }
            let _ = write!(out, "{} {:.5} {:.5} ", if pen { 'L' } else { 'M' }, p[0], p[1]);
            pen = true;
        }
        if *closed && !broken { out.push_str("Z "); }
    }
    if out.is_empty() { out.push_str("M0 0"); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_choose_nice_steps_and_exact_zero() {
        for (lo, hi, expected) in [(-3.0, 3.0, 1.0), (0.0, 100.0, 10.0), (-0.003, 0.003, 0.001)] {
            let (step, ticks) = nice_ticks(lo, hi, 8);
            assert!((step - expected).abs() < expected * 1e-10);
            assert!(ticks.len() >= 6 && ticks.len() <= 11);
            assert!(ticks.contains(&0.0));
            assert!(ticks.iter().all(|v| *v >= lo - 1e-12 && *v <= hi + 1e-12));
        }
        assert_eq!(ticks_at(1.2, 3.8, 1.0), vec![2.0, 3.0]);
    }

    #[test]
    fn implicit_circle_is_one_closed_accurate_loop() {
        let lines = marching_squares(|x, y| x * x + y * y - 4.0, Bounds { x: [-3.0, 3.0], y: [-3.0, 3.0] }, 100);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].1);
        assert!(lines[0].0.iter().all(|p| (p[0].hypot(p[1]) - 2.0).abs() < 0.02));
    }

    #[test]
    fn tangent_function_breaks_at_both_poles_and_clips() {
        let bounds = Bounds { x: [-3.0, 3.0], y: [-4.0, 4.0] };
        let lines = adaptive_curve(|x| Some([x, x.tan()]), -3.0, 3.0, |p| p, bounds);
        assert_eq!(lines.len(), 3);
        for line in &lines {
            assert!(line.iter().all(|p| p[1].abs() <= 4.0 + 1e-9));
            assert!(line.windows(2).all(|p| (p[1][1] - p[0][1]).abs() < 2.0));
        }
        assert!(lines[1].first().unwrap()[1] < -3.999);
        assert!(lines[1].last().unwrap()[1] > 3.999);
    }

    #[test]
    fn adaptive_parabola_bounds_screen_chord_error() {
        // Extreme vertical zoom forces bisection near the vertex.
        let map = |p: Point| [p[0] * 100.0 + 300.0, 4_000_000.0 - (p[1] + 1.0) * 400_000.0];
        let lines = adaptive_curve(|x| Some([x, x * x]), -3.0, 3.0, map, Bounds { x: [0.0, 600.0], y: [0.0, 4_000_000.0] });
        assert_eq!(lines.len(), 1);
        assert!(lines[0].len() > 129);
        for pair in lines[0].windows(2) {
            let x = (pair[0][0] + pair[1][0]) * 0.5;
            assert!(chord_error(map(pair[0]), map([x, x * x]), map(pair[1])) < 0.36);
        }
    }

    #[test]
    fn saddle_connections_follow_the_center_value() {
        let bounds = Bounds { x: [-1.0, 1.0], y: [-1.0, 1.0] };
        for offset in [-0.2, 0.2] {
            let lines = marching_squares(|x, y| x * y + offset, bounds, 1);
            assert_eq!(lines.len(), 2);
            assert!(lines.iter().all(|(_, closed)| !closed));
            assert!(lines.iter().all(|(p, _)| {
                let m = mix(p[0], p[1], 0.5);
                (m[0] * m[1]).signum() == -offset.signum()
            }));
        }
    }
}

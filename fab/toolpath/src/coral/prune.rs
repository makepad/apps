//! The crown's lines that carry a surface or the kept material above them.
//!
//! Dead fronds and line ends go down to where they leave a line that carries something.
//! A web closes from both sides by a layer's reach; holes up to 4 mm stay. A kept run is
//! at least two line widths long, where its line has room. The body stays as it is.

use super::plan::P2;
use crate::par::{Cancelled, Ctl};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::grid::SegGrid;
use std::collections::HashMap;

const FILL: f64 = 4.0;
const STEP: f64 = 0.1;
const CELL: f64 = 0.5;
const RUN_GAP: f64 = 1.5;

fn distance(a: P2, b: P2) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn between(a: P2, b: P2, t: f64) -> P2 {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn arc(line: &[P2]) -> Vec<f64> {
    let mut out = vec![0.0; line.len()];
    for i in 1..line.len() {
        out[i] = out[i - 1] + distance(line[i - 1], line[i]);
    }
    out
}

fn samples(line: &[P2], mut visit: impl FnMut(P2, usize, f64)) {
    for (i, s) in line.windows(2).enumerate() {
        let n = (distance(s[0], s[1]) / STEP).ceil().max(1.0) as usize;
        for j in 0..n {
            let t = j as f64 / n as f64;
            visit(between(s[0], s[1], t), i, t);
        }
    }
    if line.len() >= 2 {
        visit(line[line.len() - 1], line.len() - 2, 1.0);
    }
}

fn at(line: &[P2], arc: &[f64], s: f64) -> P2 {
    let i = arc.partition_point(|&v| v < s).saturating_sub(1).min(line.len() - 2);
    let len = arc[i + 1] - arc[i];
    let t = if len > 1e-12 { ((s - arc[i]) / len).clamp(0.0, 1.0) } else { 0.0 };
    between(line[i], line[i + 1], t)
}

fn sub(line: &[P2], arc: &[f64], a: f64, b: f64) -> Vec<P2> {
    let mut out = vec![at(line, arc, a)];
    for (i, &s) in arc.iter().enumerate() {
        if s > a + 1e-6 && s < b - 1e-6 {
            out.push(line[i]);
        }
    }
    out.push(at(line, arc, b));
    out
}

struct Segment {
    a: P2,
    b: P2,
    line: usize,
    start: f64,
    len: f64,
}

impl Segment {
    fn nearest(&self, p: P2) -> (f64, f64) {
        let (vx, vy) = (self.b.0 - self.a.0, self.b.1 - self.a.1);
        let l2 = vx * vx + vy * vy;
        let t = if l2 > 1e-18 { (((p.0 - self.a.0) * vx + (p.1 - self.a.1) * vy) / l2).clamp(0.0, 1.0) } else { 0.0 };
        (distance(p, between(self.a, self.b, t)), self.start + t * self.len)
    }
}

/// A layer's material, with each segment's place along its line.
struct Paths {
    segments: Vec<Segment>,
    cells: HashMap<(i64, i64), Vec<usize>>,
}

impl Paths {
    fn cell(p: P2) -> (i64, i64) {
        ((p.0 / CELL).floor() as i64, (p.1 / CELL).floor() as i64)
    }

    fn new<'a>(lines: impl Iterator<Item = &'a Vec<P2>>) -> Self {
        let mut out = Self { segments: Vec::new(), cells: HashMap::new() };
        for (line, pts) in lines.enumerate() {
            let mut start = 0.0;
            for s in pts.windows(2) {
                let len = distance(s[0], s[1]);
                let index = out.segments.len();
                out.segments.push(Segment { a: s[0], b: s[1], line, start, len });
                let (a, b) = (Self::cell(s[0]), Self::cell(s[1]));
                for i in a.0.min(b.0)..=a.0.max(b.0) {
                    for j in a.1.min(b.1)..=a.1.max(b.1) {
                        out.cells.entry((i, j)).or_default().push(index);
                    }
                }
                start += len;
            }
        }
        out
    }

    fn visit(&self, p: P2, radius: i64, mut f: impl FnMut(&Segment)) {
        let (ci, cj) = Self::cell(p);
        for i in ci - radius..=ci + radius {
            for j in cj - radius..=cj + radius {
                if let Some(indices) = self.cells.get(&(i, j)) {
                    for &index in indices {
                        f(&self.segments[index]);
                    }
                }
            }
        }
    }

    fn nearest(&self, p: P2) -> Option<(usize, f64)> {
        let mut best = f64::INFINITY;
        let mut found = None;
        for radius in [1, 3] {
            self.visit(p, radius, |s| {
                let (d, along) = s.nearest(p);
                if d < best {
                    best = d;
                    found = Some((s.line, along));
                }
            });
            if best <= radius as f64 * CELL {
                break;
            }
        }
        found
    }

    fn within(&self, p: P2, reach: f64) -> bool {
        self.within_but(p, reach, |_| false)
    }

    /// Whether some segment within `reach` of `p` is of a line `skip` does not pass over.
    fn within_but(&self, p: P2, reach: f64, skip: impl Fn(usize) -> bool) -> bool {
        let mut found = false;
        self.visit(p, ((reach + CELL) / CELL).ceil() as i64, |s| {
            found |= !skip(s.line) && s.nearest(p).0 <= reach;
        });
        found
    }
}

/// What went from over a crown, for pruning only under it.
#[derive(Clone, Copy)]
pub struct Under<'a> {
    /// Per layer, the lines that went (mm).
    pub gone: &'a [Vec<Vec<P2>>],
    /// How near (mm) kept material on its layer a line stands to brace it: a line that carries
    /// nothing but stands that near one that does is one column with it, and stays.
    pub brace: f64,
}

/// Adds `run` to the sorted, disjoint `runs`, merging what it meets.
fn merge(runs: &mut Vec<(f64, f64)>, (a, b): (f64, f64)) {
    if let Some(last) = runs.last_mut().filter(|last| a <= last.1 + 1e-6) {
        last.1 = last.1.max(b);
    } else {
        runs.push((a, b));
    }
}

/// Grows each line's `runs` (sorted, disjoint, mm along it) by what of the rest stands within
/// `brace` of another line's runs or of `body`, on the same layer, and of what that adds, until
/// it adds nothing: what is one column with kept material stays.
fn braced(lines: &[Vec<P2>], arcs: &[Vec<f64>], runs: &mut [Vec<(f64, f64)>], body: &[Vec<P2>], brace: f64) {
    let gaps = |rs: &[(f64, f64)], arc: &[f64]| rs.len() != 1 || rs[0].0 > 1e-6 || rs[0].1 < arc[arc.len() - 1] - 1e-6;
    while lines.iter().zip(arcs).zip(runs.iter()).any(|((line, arc), rs)| line.len() >= 2 && gaps(rs, arc)) && brace_once(lines, arcs, runs, body, brace) {}
}

/// One step of `braced`, and whether it added anything.
fn brace_once(lines: &[Vec<P2>], arcs: &[Vec<f64>], runs: &mut [Vec<(f64, f64)>], body: &[Vec<P2>], brace: f64) -> bool {
    let mut grew = false;
    let mut of = Vec::new();
    let mut kept = Vec::new();
    for (i, ((line, arc), rs)) in lines.iter().zip(arcs).zip(runs.iter()).enumerate() {
        if line.len() >= 2 {
            for &(a, b) in rs {
                kept.push(sub(line, arc, a, b));
                of.push(i);
            }
        }
    }
    let near = Paths::new(kept.iter().chain(body));
    for (i, ((line, arc), rs)) in lines.iter().zip(arcs).zip(runs.iter_mut()).enumerate() {
        if line.len() < 2 {
            continue;
        }
        let total = arc[arc.len() - 1];
        let mut more = Vec::new();
        let mut from = 0.0;
        for &(a, b) in rs.iter().chain([(total, total)].iter()) {
            let mut start: Option<f64> = None;
            let mut s = from;
            while s < a {
                let held = near.within_but(at(line, arc, s), brace, |k| of.get(k) == Some(&i));
                match (held, start) {
                    (true, None) => start = Some(s),
                    (false, Some(s0)) => {
                        more.push((s0, s));
                        start = None;
                    }
                    _ => {}
                }
                s += STEP;
            }
            if let Some(s0) = start {
                more.push((s0, a));
            }
            from = b;
        }
        if more.is_empty() {
            continue;
        }
        grew = true;
        let mut all: Vec<(f64, f64)> = rs.iter().copied().chain(more).collect();
        all.sort_by(|x, y| x.0.total_cmp(&y.0));
        rs.clear();
        for run in all {
            merge(rs, run);
        }
    }
    grew
}

fn touches(grid: &SegGrid<usize>, p: P2, lw: f64) -> bool {
    let p = Pt::from_mm(p.0, p.1);
    grid.contains_any(p) || grid.nearest(p, |i| i).is_some_and(|(d, _)| d <= lw * SCALE)
}

/// The crown that carries `surfaces` on their contact layers or kept material above, per layer
/// its kept runs, each with the index of the line on its layer it is a run of.
/// Points, `reach` per layer, and line width `lw` are in mm. `body` stays whole and
/// carries material just as the crown does. The result has the crown's layer count.
/// With `under`, only the lines that stood under what went, or under what this prune takes, are
/// pruned, and of what they would lose they keep what braces kept material: the rest stay whole.
pub fn prune(ctl: Ctl, crown: &[Vec<Vec<P2>>], body: &[Vec<Vec<P2>>], surfaces: &[Vec<ExPolygon>], under: Option<Under>, reach: f64, lw: f64) -> Result<Vec<Vec<(usize, Vec<P2>)>>, Cancelled> {
    let xr = reach + 0.05;
    let min_run = 2.0 * lw;
    let mut kept: Vec<Vec<(usize, Vec<P2>)>> = vec![Vec::new(); crown.len()];
    let mut taken: Vec<Vec<P2>> = Vec::new();
    for l in (0..crown.len()).rev() {
        if ctl.cancelled() {
            return Err(Cancelled);
        }
        let lines = &crown[l];
        let arcs: Vec<_> = lines.iter().map(|line| arc(line)).collect();
        let mut marks = vec![Vec::new(); lines.len()];
        let below = Paths::new(lines.iter().chain(body.get(l).into_iter().flatten()));
        let above: Vec<&Vec<P2>> = kept.get(l + 1).into_iter().flatten().map(|(_, line)| line).chain(body.get(l + 1).into_iter().flatten()).collect();
        for line in &above {
            samples(line, |p, _, _| {
                if let Some((i, s)) = below.nearest(p) {
                    if i < marks.len() {
                        marks[i].push(s);
                    }
                }
            });
        }
        let contact = SegGrid::of_regions(surfaces.get(l).map_or(&[], Vec::as_slice), |i| i);
        if !contact.is_empty() {
            for (i, line) in lines.iter().enumerate() {
                samples(line, |p, j, t| {
                    if touches(&contact, p, lw) {
                        marks[i].push(arcs[i][j] + t * (arcs[i][j + 1] - arcs[i][j]));
                    }
                });
            }
        }
        let above = Paths::new(above.into_iter());
        let loose = under.map(|u| Paths::new(u.gone.get(l + 1).into_iter().flatten().chain(&taken)));
        // Per line, the runs (mm along it) it keeps.
        let mut runs: Vec<Vec<(f64, f64)>> = Vec::with_capacity(lines.len());
        for ((line, arc), mut marks) in lines.iter().zip(&arcs).zip(marks) {
            let total = arc.last().copied().unwrap_or(0.0);
            let whole = line.len() < 2 || loose.as_ref().is_some_and(|g| {
                let mut under = false;
                if !g.segments.is_empty() {
                    samples(line, |p, _, _| under = under || g.within(p, xr + lw));
                }
                !under
            });
            if whole {
                runs.push(vec![(0.0, total)]);
                continue;
            }
            if marks.is_empty() {
                runs.push(Vec::new());
                continue;
            }
            marks.sort_by(f64::total_cmp);
            let mut found = Vec::new();
            let (mut a, mut b) = (marks[0], marks[0]);
            for s in marks {
                if s - b > RUN_GAP {
                    found.push((a, b));
                    a = s;
                }
                b = s;
            }
            found.push((a, b));
            let up = |s| above.within(at(line, arc, s), xr);
            let mut webbed = vec![found[0]];
            for &(a, b) in &found[1..] {
                let last = webbed.last_mut().unwrap();
                let (mut lo, mut hi) = (last.1, a);
                let mut s = last.1 + STEP;
                while s < a && up(s) {
                    lo = s;
                    s += STEP;
                }
                s = a - STEP;
                while s > lo && up(s) {
                    hi = s;
                    s -= STEP;
                }
                if hi - lo <= FILL {
                    last.1 = b;
                } else {
                    last.1 = lo;
                    webbed.push((hi, b));
                }
            }
            let mut merged: Vec<(f64, f64)> = Vec::new();
            for (mut a, mut b) in webbed {
                if b - a < min_run {
                    let c = (a + b) / 2.0;
                    a = (c - min_run / 2.0).max(0.0);
                    b = (a + min_run).min(total);
                    a = (b - min_run).max(0.0);
                }
                merge(&mut merged, (a, b));
            }
            runs.push(merged);
        }
        if let Some(brace) = under.map(|u| u.brace).filter(|&b| b > 0.0) {
            braced(lines, &arcs, &mut runs, body.get(l).map_or(&[], Vec::as_slice), brace);
        }
        let mut out = Vec::new();
        let mut take = Vec::new();
        for (i, ((line, arc), runs)) in lines.iter().zip(&arcs).zip(runs).enumerate() {
            let total = arc.last().copied().unwrap_or(0.0);
            if line.len() < 2 {
                if !runs.is_empty() {
                    out.push((i, line.clone()));
                }
                continue;
            }
            let mut from = 0.0;
            for (a, b) in runs {
                if a > from + 1e-6 {
                    take.push(sub(line, arc, from, a));
                }
                from = b;
                out.push((i, sub(line, arc, a, b)));
            }
            if total > from + 1e-6 {
                take.push(sub(line, arc, from, total));
            }
        }
        kept[l] = out;
        taken = take;
    }
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LH: f64 = 0.2;
    const REACH: f64 = LH;
    const LW: f64 = 0.45;

    fn line(y: f64, len: f64) -> Vec<P2> {
        vec![(0.0, y), (len, y)]
    }

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1)))
    }

    fn run(crown: Vec<Vec<Vec<P2>>>, body: &[Vec<Vec<P2>>], surfaces: &[Vec<ExPolygon>]) -> Vec<Vec<Vec<P2>>> {
        prune(Ctl::default(), &crown, body, surfaces, None, REACH, LW).unwrap().into_iter().map(|l| l.into_iter().map(|(_, line)| line).collect()).collect()
    }

    fn whole(got: &[Vec<P2>], expected: &[P2]) {
        assert_eq!(got.len(), 1, "one whole line");
        assert_eq!(got[0].len(), expected.len(), "all original vertices");
        for (&a, &b) in got[0].iter().zip(expected) {
            assert!(distance(a, b) < 1e-9, "whole line: {a:?} != {b:?}");
        }
    }

    fn nothing_kept_lacks_support_above(crown: &[Vec<Vec<P2>>], body: &[Vec<Vec<P2>>], surfaces: &[Vec<ExPolygon>]) {
        let limit = REACH + 0.05 + FILL / 2.0 + STEP;
        for l in 0..crown.len() - 1 {
            for line in &crown[l] {
                samples(line, |p, _, _| {
                    // An independent scan is small enough for these synthetic scenes.
                    let q = Pt::from_mm(p.0, p.1);
                    let supported = crown[l + 1].iter().chain(body.get(l + 1).into_iter().flatten()).any(|line| {
                        line.windows(2).any(|s| dist_point_segment(q, Pt::from_mm(s[0].0, s[0].1), Pt::from_mm(s[1].0, s[1].1)) / SCALE <= limit + 1e-9)
                    });
                    let contact = surfaces.get(l).into_iter().flatten().any(|e| {
                        e.contains(q) || e.rings().any(|r| {
                            (0..r.pts.len()).any(|i| dist_point_segment(q, r.pts[i], r.pts[(i + 1) % r.pts.len()]) / SCALE <= LW)
                        })
                    });
                    assert!(supported || contact, "layer {l}, {p:?} has nothing above within {limit} mm");
                });
            }
        }
    }

    #[test]
    fn a_line_that_reaches_no_surface_is_removed() {
        let out = run(vec![vec![line(0.0, 10.0)]; 20], &[], &[]);
        assert_eq!(out.len(), 20, "layer count survives removal");
        assert!(out.iter().all(Vec::is_empty), "no surface, no crown");
    }

    #[test]
    fn a_line_under_a_surface_stays_whole() {
        let mut surfaces = vec![vec![]; 20];
        surfaces[19].push(rect(-1.0, -1.0, 11.0, 1.0));
        let source = vec![(0.0, 0.0), (2.0, 0.0), (5.0, 0.0), (10.0, 0.0)];
        let out = run(vec![vec![source.clone()]; 20], &[], &surfaces);
        assert_eq!(out.len(), 20, "same layer count");
        for layer in &out {
            whole(layer, &source);
        }
    }

    #[test]
    fn a_frond_leading_nowhere_goes_down_to_its_fork() {
        let crown: Vec<_> = (0..20).map(|l| {
            if l < 10 { vec![line(0.0, 10.0)] } else {
                let dy = 0.15 * (l - 9) as f64;
                vec![line(dy, 10.0), line(-dy, 10.0)]
            }
        }).collect();
        let mut surfaces = vec![vec![]; 20];
        surfaces[19].push(rect(-1.0, 1.4, 11.0, 1.6));
        let out = run(crown.clone(), &[], &surfaces);
        for (l, layer) in out.iter().enumerate() {
            whole(layer, &crown[l][0]);
        }
        nothing_kept_lacks_support_above(&out, &[], &surfaces);
    }

    #[test]
    fn a_hole_up_to_fill_stays_and_a_wider_one_opens() {
        for len in [3.0, 10.0] {
            let source = line(0.0, len);
            let mut surfaces = vec![vec![]; 20];
            // At y = lw only the 0.2 mm end stretches touch, including their boundary.
            surfaces[19] = vec![rect(-0.1, LW, 0.2, LW + 0.1), rect(len - 0.2, LW, len + 0.1, LW + 0.1)];
            let out = run(vec![vec![source.clone()]; 20], &[], &surfaces);
            if len == 3.0 {
                for layer in &out {
                    whole(layer, &source);
                }
            } else {
                let mut previous: Option<f64> = None;
                let mut open = 0;
                let mut closed = false;
                for (l, layer) in out.iter().enumerate().rev() {
                    if layer.len() == 1 {
                        whole(layer, &source);
                        if !closed {
                            assert!(previous.unwrap() - 2.0 * REACH <= FILL + STEP, "only a fillable gap closes at layer {l}");
                        }
                        closed = true;
                    } else {
                        assert!(!closed, "the gap never reopens going down");
                        assert_eq!(layer.len(), 2, "two sides at layer {l}");
                        let gap = layer[1][0].0 - layer[0].last().unwrap().0;
                        assert!(gap > FILL, "a wider hole stays open: {gap}");
                        if let Some(prev) = previous {
                            assert!((prev - gap - 2.0 * REACH).abs() <= STEP + 1e-9, "layer {l}: gap {prev} -> {gap}, two sides close by reach");
                        }
                        previous = Some(gap);
                        open += 1;
                    }
                }
                assert!(open >= 5 && closed, "a notch closes over several layers");
            }
            nothing_kept_lacks_support_above(&out, &[], &surfaces);
        }
    }

    #[test]
    fn a_short_kept_run_grows_to_two_widths() {
        for x in [0.0, 4.9, 9.8] {
            let out = run(vec![vec![line(0.0, 10.0)]], &[], &[vec![rect(x, LW, x + 0.2, LW + 0.1)]]);
            assert_eq!(out[0].len(), 1, "one short contact");
            let kept = &out[0][0];
            let length: f64 = kept.windows(2).map(|s| distance(s[0], s[1])).sum();
            assert!((length - 2.0 * LW).abs() < 1e-9, "contact at {x}: kept {length} mm");
            assert!(kept[0].0 <= x && kept.last().unwrap().0 >= x + 0.2, "contact remains covered");
            assert!(kept[0].0 >= 0.0 && kept.last().unwrap().0 <= 10.0, "growth stays on the original line");
        }
    }

    #[test]
    fn under_what_went_only_what_braces_nothing_goes() {
        // Over nine layers: `a` under a surface, `b` 0.8 mm beside it, `c` 5 mm off and `d` 10 mm
        // off. What stood on `b` and `c` went; `d` never carried anything.
        let mut surfaces = vec![vec![]; 9];
        surfaces[8].push(rect(-1.0, -0.2, 11.0, 0.2));
        let crown = vec![vec![line(0.0, 10.0), line(0.8, 10.0), line(5.0, 10.0), line(10.0, 10.0)]; 9];
        let mut gone = vec![vec![]; 10];
        gone[9] = vec![line(0.8, 10.0), line(5.0, 10.0)];
        let out = prune(Ctl::default(), &crown, &[], &surfaces, Some(Under { gone: &gone, brace: 1.2 }), REACH, LW).unwrap();
        for (l, layer) in out.iter().enumerate() {
            let kept: Vec<(usize, f64)> = layer.iter().map(|(i, run)| (*i, run[0].1)).collect();
            assert_eq!(kept, vec![(0, 0.0), (1, 0.8), (3, 10.0)], "layer {l}: `b` braces `a` and stays, `c` goes, `d` stays");
            for (i, run) in layer {
                whole(&[run.clone()], &crown[l][*i]);
            }
        }
    }

    #[test]
    fn the_body_is_never_touched() {
        let source = line(0.0, 10.0);
        let mut crown = vec![vec![]; 11];
        let mut body = vec![vec![]; 11];
        for layer in &mut crown[..5] {
            layer.push(source.clone());
        }
        for layer in &mut body[5..] {
            layer.push(source.clone());
        }
        // Marks for this branch must land on the nearer body, not this unused crown.
        crown[5].push(line(0.3, 10.0));
        let mut surfaces = vec![vec![]; 11];
        surfaces[10].push(rect(-1.0, -1.0, 11.0, 1.0));
        let original = body.clone();
        let out = run(crown, &body, &surfaces);
        assert_eq!(body, original, "the body's paths do not change");
        for layer in &out[..5] {
            whole(layer, &source);
        }
        assert!(out[5..].iter().all(Vec::is_empty), "body wins nearest marks and is not returned as crown");
        nothing_kept_lacks_support_above(&out, &body, &surfaces);
    }
}

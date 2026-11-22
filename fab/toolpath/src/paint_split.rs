//! Split planned paths by positional paint, suppressing short colour runs
//! before distributing the pieces to their filaments.
//!
//! A blend of two colours (a `Mix` in the part's script) is dithered along each path: the path
//! is cut into cells of one shortest run (a bead where it shows, longer inside the part), and
//! each cell takes one colour so that the cells' shares follow the blend's ([`dither`]).
//!
//! Deeper under the part's surface than its skin, where no colour shows, a fill takes the paint
//! in large cells instead ([`Inside`]): each cell one material, so a layer's inside prints in
//! a few long pieces and not in hundreds of short ones.

use crate::clipper::Clipper;
use crate::{ExtrusionPath, Island, ObjectLayer};
use makepad_fab_slicer::geom::{lerp, scaled, ExPolygon, Pt, SCALE};
use makepad_fab_slicer::source::{PaintAt, PaintHere};
use std::sync::Arc;

/// How short a colour run may be before it takes its neighbour's colour.
#[derive(Clone, Copy, Debug)]
pub struct SplitRule {
    /// A run (one state along a path) shorter than this (mm), on a path whose colour shows on the part, takes the state of its longer neighbour.
    pub min_run: f64,
    /// The same on a path inside the part, where a short run shows nowhere and costs its retract and travel.
    pub min_run_inside: f64,
    /// The paint prints exactly all through (a part to be cut open, a picture whose inner layers
    /// show through its face): the fills inside keep runs as short as the surface's.
    pub exact_fills: bool,
    /// The smallest march step along a path (mm): the paint is sampled at least this far apart where its distance bound is smaller.
    pub step: f64,
    /// The print's resolution a blend is dithered at: a bead's width across (mm) and the layer
    /// height (mm).
    pub bead: f64,
    pub height: f64,
    /// The least length (mm) of a filament on one object's layer worth a head swap: where a
    /// blend's specks are all a filament prints on the layer and come to less, they gather on
    /// fewer layers, about this much on each, as many layers as keep the blend's share.
    pub min_swap: f64,
}

/// A layer's inside, where its fills take the paint coarsely: the region deeper under the part's
/// surface than the paint's skin (the layer's outline over the layers within the skin above and
/// below, shrunk by the skin), and the size of the cells (mm, along x and y) each of which
/// prints one material on the layer: the paint at the cell's middle on the layer's own plane, of
/// a blend its nearer material (a thin share would cost a head swap for a cell nobody sees). A
/// cell whose middle lies off the core takes the paint where the fill is instead: off the part
/// the paint may be one the part prints nowhere on the layer (a swap for a sliver). A
/// cell is one layer tall: one taller would print, under a colour that starts up the part, that
/// colour on the layers below it, a filament those layers print nothing else in (a swap each).
pub struct Inside {
    pub core: Vec<ExPolygon>,
    pub cell: f64,
}

/// The paint a fill takes: as it is in the skin, by cells in `inside`'s core.
fn coarse(paint: &PaintAt, inside: &Inside) -> PaintAt {
    let clip = Clipper::new(&inside.core);
    let (exact, cell) = (paint.clone(), inside.cell);
    Arc::new(move |x, y, z| {
        let p = Pt::from_mm(x, y);
        if !clip.contains(p) {
            // as it is, up to the core: no march steps over the core's edge
            let h = exact(x, y, z);
            let edge = clip.boundary_within(p, scaled(h.dist.min(cell))) / SCALE;
            return PaintHere { dist: h.dist.min(edge), ..h };
        }
        let g = [(x / cell).floor() as i64, (y / cell).floor() as i64];
        let (gx, gy) = (g[0] as f64 * cell, g[1] as f64 * cell);
        // the same material to the cell's walls and to the skin, whichever is nearer
        let walls = (x - gx).min(gx + cell - x).min(y - gy).min(gy + cell - y).max(0.0);
        let skin = clip.boundary_within(p, scaled(walls)) / SCALE;
        let mid = |i: i64| (i as f64 + 0.5) * cell;
        let (mx, my) = (mid(g[0]), mid(g[1]));
        let on_core = clip.contains(Pt::from_mm(mx, my));
        let h = if on_core { exact(mx, my, z) } else { exact(x, y, z) };
        let state = match h.mix {
            Some((other, share)) if share > 0.5 => other,
            _ => h.state,
        };
        let dist = if on_core { walls.min(skin) } else { walls.min(skin).min(h.dist) };
        PaintHere { state, dist, mix: None }
    })
}

/// One planned layer split by its paint at plane `z`: an `ObjectLayer` per filament in `fils`
/// (same order; `fils[0]` is `ol.filament`), each with the pieces of the paths that print in it.
/// `fil_of(state)` is the 0-based filament of a paint state. The own filament's layer keeps
/// `ol.slices`; the others get empty `slices`. Islands keep their order; an island with no
/// piece in a filament is left out of that filament's layer. With `inside`, a fill that does not
/// show takes the paint by its cells in its core ([`Inside`]); walls and surfaces as it is.
pub fn split_layer(
    ol: ObjectLayer,
    z: f64,
    paint: &PaintAt,
    fil_of: &(dyn Fn(u32) -> usize + Sync),
    fils: &[usize],
    rule: &SplitRule,
    inside: Option<&Inside>,
) -> Vec<ObjectLayer> {
    if fils.is_empty() {
        return Vec::new();
    }
    let coarse = inside.filter(|i| !i.core.is_empty()).map(|i| coarse(paint, i));
    let paint_of = |wall: bool, path: &ExtrusionPath| match &coarse {
        Some(c) if !wall && !path.role.shows() => c,
        _ => paint,
    };

    let slot_of = |state| {
        let filament = fil_of(state);
        fils.iter().position(|&f| f == filament).unwrap_or(0)
    };

    let ObjectLayer { object, islands, mut slices, support_only, .. } = ol;
    let mut out: Vec<ObjectLayer> = fils
        .iter()
        .enumerate()
        .map(|(k, &filament)| ObjectLayer {
            object,
            filament,
            islands: Vec::new(),
            slices: if k == 0 { std::mem::take(&mut slices) } else { Arc::default() },
            support_only,
        })
        .collect();
    // a wall's line marched whole (its pieces together); a fill by itself
    let march_one = |line: &Line, weigh: &dyn Fn(u32) -> f64| -> Vec<PathRuns> {
        if line.wall {
            march_line(&line.paths, z, paint, rule, weigh)
        } else {
            vec![march(&line.paths[0], z, paint_of(false, &line.paths[0]), rule, weigh)]
        }
    };
    let mut marched: Vec<Vec<Line>> = islands
        .into_iter()
        .map(|island| {
            let island = Arc::unwrap_or_clone(island);
            let walls = wall_lines(island.walls).into_iter().map(|paths| Line { wall: true, paths, runs: Vec::new() });
            let fills = island.fills.into_iter().map(|path| Line { wall: false, paths: vec![path], runs: Vec::new() });
            walls.chain(fills).map(|mut line| {
                line.runs = march_one(&line, &|_| 1.0);
                line
            }).collect()
        })
        .collect();
    // the walls and the surfaces by their own, the rest by all the paths'
    let [skin, all] = speck_weights(&marched, z, rule, fils.len(), &slot_of);
    if skin.iter().chain(&all).any(|&w| w != 1.0) {
        for line in marched.iter_mut().flatten() {
            if line.runs.iter().any(PathRuns::dithered) {
                let weights = if line.wall || line.paths[0].role.shows() { &skin } else { &all };
                line.runs = march_one(line, &|state: u32| weights[slot_of(state)]);
            }
        }
    }
    for island in marched {
        let mut targets = vec![Island::default(); fils.len()];
        let lines = island.into_iter().flat_map(|line| {
            let wall = line.wall;
            line.paths.into_iter().zip(line.runs).map(move |(path, runs)| (wall, path, runs))
        });
        for (wall, path, mut runs) in lines {
            // Resolve missing filaments before merging, so pieces falling back to
            // the own filament can also keep the original path intact.
            merge_equal(&mut runs.runs, path.closed, runs.length, |a, b| slot_of(a) == slot_of(b));
            runs.compact();
            if runs.runs.is_empty() {
                put_path(&mut targets[slot_of(runs.state)], wall, path);
                continue;
            }

            let pts = polyline(&path);
            let segments = segment_count(pts, path.closed);
            let mut arcs = Vec::with_capacity(segments + 1);
            arcs.push(0.0);
            for i in 0..segments {
                arcs.push(arcs[i] + pts[i].dist(pts[(i + 1) % pts.len()]) / SCALE);
            }
            for run in runs.runs {
                if let Some(piece) = cut_piece(&path, pts, &arcs, run) {
                    put_path(&mut targets[slot_of(run.state)], wall, piece);
                }
            }
        }
        for (layer, island) in out.iter_mut().zip(targets) {
            if !island.walls.is_empty() || !island.fills.is_empty() {
                layer.islands.push(Arc::new(island));
            }
        }
    }
    out
}

/// What weighs each filament's share of a blend on this layer (by its index in the layer's
/// filaments): 1 as the blend gives it, but a filament printing nothing on the layer except a
/// blend's specks, less than a head swap is worth in all, is either left out (0) or given a
/// swap's worth, by the layer's place in the golden-ratio sequence, so that the layers keeping
/// it hold its share on average. Two sets: the walls' and the surfaces', weighed by what they
/// print alone, so the outside prints the same whatever the inside takes ([`Inside`]); and the
/// rest's, by what all the paths print. A layer the skin keeps a filament on, the rest keeps it
/// on too (its specks are the more), so the two cost no more swaps than one.
fn speck_weights(marched: &[Vec<Line>], z: f64, rule: &SplitRule, fils: usize, slot_of: &dyn Fn(u32) -> usize) -> [Vec<f64>; 2] {
    // per filament: printed solid, printed by a blend; by the paths that show, and by all
    let tally = |skin: bool| {
        let mut prints = vec![[0.0f64; 2]; fils];
        let paths = marched.iter().flatten().flat_map(|line| line.paths.iter().zip(&line.runs).map(move |(path, runs)| (line.wall, path, runs)));
        for (_, _, runs) in paths.filter(|(wall, path, _)| !skin || *wall || path.role.shows()) {
            if runs.runs.is_empty() {
                prints[slot_of(runs.state)][runs.whole_dithered as usize] += runs.length;
            }
            for run in &runs.runs {
                prints[slot_of(run.state)][run.dithered as usize] += run.length();
            }
        }
        let blended: f64 = prints.iter().map(|p| p[1]).sum();
        (prints, blended)
    };
    let layer = (z / rule.height.max(1e-6)).floor();
    let weights = |(prints, blended): (Vec<[f64; 2]>, f64)| -> Vec<f64> { prints.iter().enumerate().map(|(k, &[solid, specks])| {
        if solid > 0.0 || specks <= 0.0 || specks >= rule.min_swap {
            return 1.0;
        }
        let keep = (0.5 + layer * 0.618_033_988_749_895 + k as f64 * 0.754_877_666_246_693).rem_euclid(1.0);
        let gain = rule.min_swap / specks;
        if specks / rule.min_swap <= keep {
            return 0.0;
        }
        // the weight that turns a share t into t * gain (t: this filament's share of the blends)
        let t = specks / blended;
        if t * gain >= 1.0 { 1e9 } else { gain * (1.0 - t) / (1.0 - t * gain) }
    }).collect() };
    [weights(tally(true)), weights(tally(false))]
}

/// A line the print lays without a break: one path, or a wall's loop the overhang split cut into
/// pieces (each piece starting where the one before ends, the last carrying the loop). Its
/// colours are marched along the whole line, so a run is as long as it is on the line: a piece's
/// end is no end of a run, and a run that starts on one piece and ends on the next is not cut
/// short and taken by its neighbour's colour (a stub of the other colour on that layer alone).
struct Line {
    wall: bool,
    paths: Vec<ExtrusionPath>,
    /// Each path's runs.
    runs: Vec<PathRuns>,
}

/// An island's walls as the lines they print: open pieces each starting where the one before
/// ends, up to the piece that carries their loop.
fn wall_lines(walls: Vec<ExtrusionPath>) -> Vec<Vec<ExtrusionPath>> {
    let mut out: Vec<Vec<ExtrusionPath>> = Vec::new();
    for path in walls {
        match out.last_mut() {
            Some(line) if continues(line, &path) => line.push(path),
            _ => out.push(vec![path]),
        }
    }
    out
}

fn continues(line: &[ExtrusionPath], next: &ExtrusionPath) -> bool {
    let last = &line[line.len() - 1];
    !last.closed && !next.closed && last.loop_pts.is_empty() && last.pts.last().is_some() && last.pts.last() == next.pts.first()
}

/// The runs of each of a line's paths ([`Line`]), marched along the whole line: a loop when its
/// last piece carries one.
fn march_line(paths: &[ExtrusionPath], z: f64, paint: &PaintAt, rule: &SplitRule, weigh: &dyn Fn(u32) -> f64) -> Vec<PathRuns> {
    if paths.len() == 1 {
        return vec![march(&paths[0], z, paint, rule, weigh)];
    }
    let closed = !paths[paths.len() - 1].loop_pts.is_empty();
    // the pieces' points end to start, and where each piece starts and ends along the line
    let mut pts: Vec<Pt> = Vec::new();
    let mut spans = Vec::with_capacity(paths.len());
    let mut at = 0.0;
    for p in paths {
        let from = at;
        for q in &p.pts {
            if let Some(&prev) = pts.last() {
                if prev == *q {
                    continue;
                }
                at += prev.dist(*q) / SCALE;
            }
            pts.push(*q);
        }
        spans.push((from, at));
    }
    // a line that shows marches as one that shows
    let role = paths.iter().map(|p| p.role).find(|r| r.shows()).unwrap_or(paths[0].role);
    let line = ExtrusionPath { role, loop_pts: if closed { pts.clone() } else { Vec::new() }, pts, closed, ..Default::default() };
    let whole = march(&line, z, paint, rule, weigh);
    // its runs on it, one past its end wrapped round to its start
    let mut on: Vec<Run> = Vec::with_capacity(whole.runs.len() + 1);
    for r in &whole.runs {
        on.push(Run { end: r.end.min(whole.length), ..*r });
        if r.end > whole.length {
            on.push(Run { start: 0.0, end: r.end - whole.length, ..*r });
        }
    }
    spans
        .into_iter()
        .zip(paths)
        .map(|((from, to), path)| {
            // the piece's own length as its cut reads it: a run's end past it would wrap round it
            let own = path.pts.windows(2).map(|w| w[0].dist(w[1]) / SCALE).sum::<f64>();
            if whole.runs.is_empty() {
                return PathRuns { state: whole.state, length: own, runs: Vec::new(), whole_dithered: whole.whole_dithered };
            }
            let local = |x: f64| if x >= to { own } else { (x - from).clamp(0.0, own) };
            let mut runs: Vec<Run> = on.iter().filter(|r| r.end > from && r.start < to).map(|r| Run { start: local(r.start), end: local(r.end), ..*r }).collect();
            runs.sort_by(|a, b| a.start.total_cmp(&b.start));
            merge_equal(&mut runs, false, own, |a, b| a == b);
            let mut piece = PathRuns { state: runs.first().map_or(whole.state, |r| r.state), length: own, runs, whole_dithered: false };
            piece.compact();
            piece
        })
        .collect()
}

fn put_path(island: &mut Island, wall: bool, path: ExtrusionPath) {
    if wall {
        island.walls.push(path);
    } else {
        island.fills.push(path);
    }
}

#[derive(Clone, Copy)]
struct Run {
    state: u32,
    start: f64,
    // The last run of a cycle may end beyond the ring's length.
    end: f64,
    /// A blend's dither chose its state (all of it, once runs merge).
    dithered: bool,
}

impl Run {
    fn length(self) -> f64 {
        self.end - self.start
    }
}

struct PathRuns {
    // Empty runs means the entire path has this state: no per-path allocation
    // is needed for the common case with no paint boundary.
    state: u32,
    length: f64,
    runs: Vec<Run>,
    /// With no runs: a blend's dither chose the whole path's state.
    whole_dithered: bool,
}

impl PathRuns {
    fn compact(&mut self) {
        if self.runs.len() == 1 {
            self.state = self.runs[0].state;
            self.whole_dithered = self.runs[0].dithered;
            self.runs.clear();
        }
    }

    /// Whether a blend chose any of the path's states.
    fn dithered(&self) -> bool {
        if self.runs.is_empty() { self.whole_dithered } else { self.runs.iter().any(|r| r.dithered) }
    }
}

fn polyline(path: &ExtrusionPath) -> &[Pt] {
    if path.closed && path.loop_pts.len() > 2 { &path.loop_pts } else { &path.pts }
}

fn segment_count(pts: &[Pt], closed: bool) -> usize {
    pts.len().saturating_sub(1) + usize::from(closed && pts.len() > 1 && pts.first() != pts.last())
}

fn sample(a: Pt, b: Pt, t: f64, z: f64, paint: &PaintAt) -> PaintHere {
    paint(a.x_mm() + (b.x_mm() - a.x_mm()) * t, a.y_mm() + (b.y_mm() - a.y_mm()) * t, z)
}

/// A march's state for a run in a blend: dithered once its ends are known.
const MIXED: u32 = u32::MAX;

fn class(h: &PaintHere) -> u32 {
    if h.mix.is_some() { MIXED } else { h.state }
}

/// A number 0..1 for lattice cell `g` (beads across, layers up): the R3 low-discrepancy
/// sequence, so neighbouring cells and layers spread over 0..1 with no repeating pattern (the
/// views dither their blends by it). A blend's carried error starts from it where a run starts,
/// so the layers' patterns do not line up.
fn threshold(g: [i64; 3]) -> f64 {
    const A: [u32; 3] = [3518319155, 2882110345, 2360945575];
    let h = (g[0] as u32).wrapping_mul(A[0]).wrapping_add((g[1] as u32).wrapping_mul(A[1])).wrapping_add((g[2] as u32).wrapping_mul(A[2]));
    (h >> 8) as f64 / (1u32 << 24) as f64
}

/// The mixed runs of a path dithered: each cut into cells of at least `cell` (mm along it), each
/// cell one colour of the blend at its middle, the shares carried from cell to cell so the
/// colours' lengths follow the blend.
fn dither(path: &ExtrusionPath, z: f64, paint: &PaintAt, rule: &SplitRule, cell: f64, runs: &[Run], length: f64, weigh: &dyn Fn(u32) -> f64) -> Vec<Run> {
    let pts = polyline(path);
    let segments = segment_count(pts, path.closed);
    let mut arcs = Vec::with_capacity(segments + 1);
    arcs.push(0.0);
    for i in 0..segments {
        arcs.push(arcs[i] + pts[i].dist(pts[(i + 1) % pts.len()]) / SCALE);
    }
    let at = |s: f64| -> (f64, f64) {
        let s = if path.closed && length > 0.0 { s.rem_euclid(length) } else { s.clamp(0.0, length) };
        let i = arcs.partition_point(|&a| a <= s).saturating_sub(1).min(segments.saturating_sub(1));
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        let t = if arcs[i + 1] > arcs[i] { (s - arcs[i]) / (arcs[i + 1] - arcs[i]) } else { 0.0 };
        (a.x_mm() + (b.x_mm() - a.x_mm()) * t, a.y_mm() + (b.y_mm() - a.y_mm()) * t)
    };
    let layer = (z / rule.height.max(1e-6)).floor() as i64;
    let lattice = |x: f64, y: f64| [(x / rule.bead).floor() as i64, (y / rule.bead).floor() as i64, layer];
    // a cell is at least a bead across, and no finer than the march resolves a boundary
    let cell = cell.max(rule.bead).max(0.001);
    let mut out = Vec::with_capacity(runs.len());
    for run in runs {
        if run.state != MIXED {
            out.push(*run);
            continue;
        }
        let n = ((run.length() / cell).floor() as usize).max(1);
        let w = run.length() / n as f64;
        let (x0, y0) = at(run.start);
        let mut carried = threshold(lattice(x0, y0)) - 0.5;
        for k in 0..n {
            let (x, y) = at(run.start + (k as f64 + 0.5) * w);
            let h = paint(x, y, z);
            let (state, dithered) = match h.mix {
                None => (h.state, false),
                Some((other, share)) => {
                    // the share of the higher state, so a pair reads the same from either side
                    let (lo, hi, t) = if other > h.state { (h.state, other, share as f64) } else { (other, h.state, 1.0 - share as f64) };
                    let (a, b) = (t * weigh(hi), (1.0 - t) * weigh(lo));
                    let t = if a + b > 0.0 { a / (a + b) } else { t };
                    let high = carried + t >= 0.5;
                    carried += t - high as u8 as f64;
                    (if high { hi } else { lo }, true)
                }
            };
            out.push(Run { state, start: run.start + k as f64 * w, end: run.start + (k + 1) as f64 * w, dithered });
        }
    }
    out
}

fn march(path: &ExtrusionPath, z: f64, paint: &PaintAt, rule: &SplitRule, weigh: &dyn Fn(u32) -> f64) -> PathRuns {
    let pts = polyline(path);
    let mut result = PathRuns { state: 0, length: 0.0, runs: Vec::new(), whole_dithered: false };
    let Some(&first) = pts.first() else { return result };
    let first_sample = paint(first.x_mm(), first.y_mm(), z);
    let mut previous = first_sample;
    let mut start = 0.0;
    let segments = segment_count(pts, path.closed);
    // Invalid steps must still make progress; use the boundary resolution.
    let step = if rule.step > 0.0 && rule.step.is_finite() { rule.step } else { 0.001 };
    let min_run = if path.role.shows() || (rule.exact_fills && !path.role.is_wall()) { rule.min_run } else { rule.min_run_inside };
    for i in 0..segments {
        let a = pts[i];
        let b = pts[(i + 1) % pts.len()];
        let length = a.dist(b) / SCALE;
        let mut at = 0.0;
        while at < length {
            // The last term only matters when the requested step is smaller
            // than floating-point spacing at this distance.
            let advance = previous.dist.max(step).max(at * f64::EPSILON);
            let next = (at + advance).min(length);
            let current = if next == length {
                if path.closed && i + 1 == segments && b == first {
                    first_sample
                } else {
                    paint(b.x_mm(), b.y_mm(), z)
                }
            } else {
                sample(a, b, next / length, z, paint)
            };
            if class(&previous) != class(&current) {
                let (mut lo, mut hi) = (at, next);
                for _ in 0..30 {
                    if hi - lo < 0.001 {
                        break;
                    }
                    let mid = lo + (hi - lo) * 0.5;
                    if mid <= lo || mid >= hi {
                        break;
                    }
                    if class(&sample(a, b, mid / length, z, paint)) == class(&previous) {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                let end = result.length + lo + (hi - lo) * 0.5;
                result.runs.push(Run { state: class(&previous), start, end, dithered: false });
                start = end;
            }
            previous = current;
            at = next;
        }
        result.length += length;
    }
    result.state = class(&previous);
    if result.runs.is_empty() && result.state == MIXED {
        result.runs.push(Run { state: MIXED, start: 0.0, end: result.length, dithered: false });
    } else if !result.runs.is_empty() {
        result.runs.push(Run { state: class(&previous), start, end: result.length, dithered: false });
        merge_equal(&mut result.runs, path.closed, result.length, |a, b| a == b);
    }
    if result.runs.iter().any(|r| r.state == MIXED) {
        result.runs = dither(path, z, paint, rule, min_run, &result.runs, result.length, weigh);
        merge_equal(&mut result.runs, path.closed, result.length, |a, b| a == b);
    }
    while result.runs.len() > 1 {
        let shortest = result.runs.iter().enumerate()
            .filter(|(_, run)| run.length() < min_run - 1e-9)
            .min_by(|(_, a), (_, b)| a.length().total_cmp(&b.length()))
            .map(|(i, _)| i);
        let Some(i) = shortest else { break };
        absorb(&mut result.runs, i, path.closed, result.length);
    }
    result.compact();
    result
}

fn merge_equal(runs: &mut Vec<Run>, closed: bool, length: f64, same: impl Fn(u32, u32) -> bool) {
    let mut count = 0;
    for i in 0..runs.len() {
        let run = runs[i];
        if count > 0 && same(runs[count - 1].state, run.state) {
            runs[count - 1].end = run.end;
            runs[count - 1].dithered &= run.dithered;
        } else {
            runs[count] = run;
            count += 1;
        }
    }
    runs.truncate(count);
    if count > 1 && closed && same(runs[0].state, runs[count - 1].state) {
        let first = runs.remove(0);
        runs[count - 2].end = length + first.end;
        runs[count - 2].dithered &= first.dithered;
    }
}

fn absorb(runs: &mut Vec<Run>, i: usize, closed: bool, length: f64) {
    let n = runs.len();
    let left = if i > 0 { Some(i - 1) } else if closed { Some(n - 1) } else { None };
    let right = if i + 1 < n { Some(i + 1) } else if closed { Some(0) } else { None };
    let neighbour = match (left, right) {
        (Some(a), Some(b)) => if runs[a].length() >= runs[b].length() { a } else { b },
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return,
    };
    runs[i].state = runs[neighbour].state;
    merge_equal(runs, closed, length, |a, b| a == b);
}

fn cut_piece(path: &ExtrusionPath, pts: &[Pt], arcs: &[f64], run: Run) -> Option<ExtrusionPath> {
    let segments = arcs.len().saturating_sub(1);
    let length = arcs.last().copied().unwrap_or(0.0);
    if segments == 0 || length <= 0.0 || run.end <= run.start {
        return None;
    }
    let keep_z = !path.z.is_empty() && path.z.len() == pts.len();
    let keep_dry = !path.dry.is_empty() && path.dry.len() == segments;
    let keep_flow = !path.flow.is_empty() && path.flow.len() == segments;
    let mut piece = ExtrusionPath {
        role: path.role,
        pts: Vec::new(),
        closed: false,
        loop_pts: Vec::new(),
        width: path.width,
        height: path.height,
        area: path.area,
        speed: path.speed,
        dry: Vec::new(),
        z: Vec::new(),
        flow: Vec::new(),
        mode: path.mode, core: path.core,
        overhang: path.overhang,
    };
    // Binary-search the first segment; each piece then visits only its own
    // vertices. A wrapping run is two ranges of the same ring.
    let ranges = [(run.start, run.end.min(length)), (0.0, run.end - length)];
    for (start, end) in ranges {
        if end <= start {
            continue;
        }
        let mut i = arcs.partition_point(|&s| s <= start).saturating_sub(1);
        while i < segments && arcs[i] < end {
            let a = arcs[i];
            let b = arcs[i + 1];
            if b > a {
                let next = (i + 1) % pts.len();
                let t0 = ((start.max(a) - a) / (b - a)).clamp(0.0, 1.0);
                let t1 = ((end.min(b) - a) / (b - a)).clamp(0.0, 1.0);
                if piece.pts.is_empty() {
                    piece.pts.push(lerp(pts[i], pts[next], t0));
                    if keep_z {
                        piece.z.push(path.z[i] + (path.z[next] - path.z[i]) * t0);
                    }
                }
                let point = lerp(pts[i], pts[next], t1);
                if piece.pts.last() != Some(&point) {
                    piece.pts.push(point);
                    if keep_z {
                        piece.z.push(path.z[i] + (path.z[next] - path.z[i]) * t1);
                    }
                    if keep_dry {
                        piece.dry.push(path.dry[i]);
                    }
                    if keep_flow {
                        piece.flow.push(path.flow[i]);
                    }
                }
            }
            i += 1;
        }
    }
    if piece.pts.len() < 2 { None } else { Some(piece) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Role;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn rule() -> SplitRule {
        SplitRule { min_run: 0.0, min_run_inside: 0.0, exact_fills: false, step: 0.05, bead: 0.4, height: 0.2, min_swap: 0.0 }
    }

    fn fil_of(state: u32) -> usize {
        if state == 0 { 0 } else { state as usize - 1 }
    }

    fn line(xs: &[f64]) -> ExtrusionPath {
        ExtrusionPath {
            pts: xs.iter().map(|&x| Pt::from_mm(x, 0.0)).collect(),
            role: Role::OuterWall,
            width: 0.4,
            height: 0.2,
            area: 0.07,
            speed: 35.0,
            mode: 3,
            overhang: 2.0,
            ..Default::default()
        }
    }

    fn layer(path: ExtrusionPath) -> ObjectLayer {
        ObjectLayer {
            object: 7,
            filament: 0,
            islands: vec![Arc::new(Island { walls: vec![path], fills: Vec::new() })],
            support_only: true,
            ..Default::default()
        }
    }

    fn half_plane(x0: f64) -> PaintAt {
        Arc::new(move |x, _, _| PaintHere::solid(if x > x0 { 2 } else { 0 }, (x - x0).abs()))
    }

    fn band(start: f64, end: f64) -> PaintAt {
        Arc::new(move |x, _, _| PaintHere::solid(if x > start && x < end { 2 } else { 0 }, (x - start).abs().min((x - end).abs())))
    }

    fn length(path: &ExtrusionPath) -> f64 {
        path.pts.windows(2).map(|p| p[0].dist(p[1]) / SCALE).sum()
    }

    #[test]
    fn straight_path_splits_at_boundary() {
        let out = split_layer(layer(line(&[0.0, 20.0])), 0.2, &half_plane(10.0), &fil_of, &[0, 1], &rule(), None);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].filament, out[1].filament), (0, 1));
        assert_eq!(out[0].islands[0].walls.len(), 1);
        assert_eq!(out[1].islands[0].walls.len(), 1);
        let left = &out[0].islands[0].walls[0];
        let right = &out[1].islands[0].walls[0];
        assert_eq!(left.pts.last(), right.pts.first());
        assert!((left.pts[left.pts.len() - 1].x_mm() - 10.0).abs() < 0.002);
        assert_eq!(left.pts[0], Pt::from_mm(0.0, 0.0));
        assert_eq!(right.pts[right.pts.len() - 1], Pt::from_mm(20.0, 0.0));
        for piece in [left, right] {
            assert!(!piece.closed);
            assert!(piece.loop_pts.is_empty());
            assert_eq!(piece.role, Role::OuterWall);
            assert_eq!((piece.width, piece.height, piece.area, piece.speed), (0.4, 0.2, 0.07, 35.0));
            assert_eq!((piece.mode, piece.overhang), (3, 2.0));
        }
        assert!(out.iter().all(|ol| ol.object == 7 && ol.support_only));
    }

    #[test]
    fn closed_ring_uses_whole_loop_and_wraps_seam() {
        for repeated_end in [false, true] {
            let mut ring = vec![Pt::from_mm(0.0, 0.0), Pt::from_mm(20.0, 0.0), Pt::from_mm(20.0, 20.0), Pt::from_mm(0.0, 20.0)];
            if repeated_end {
                ring.push(ring[0]);
            }
            let path = ExtrusionPath { closed: true, loop_pts: ring, ..line(&[0.0, 19.0]) };
            let out = split_layer(layer(path), 0.2, &half_plane(15.0), &fil_of, &[0, 1], &rule(), None);
            let mut lengths = [0.0; 2];
            for (k, ol) in out.iter().enumerate() {
                assert_eq!(ol.islands.len(), 1);
                assert_eq!(ol.islands[0].walls.len(), 1);
                for piece in &ol.islands[0].walls {
                    assert!(!piece.closed);
                    assert!(piece.loop_pts.is_empty());
                    assert!(piece.pts.windows(2).all(|p| p[0] != p[1]));
                    lengths[k] += length(piece);
                }
            }
            assert!((lengths[1] - 30.0).abs() < 0.01);
            assert!((lengths.iter().sum::<f64>() - 80.0).abs() < 0.01);
            assert!(out[0].islands[0].walls[0].pts.contains(&Pt::from_mm(0.0, 0.0)));
        }
    }

    /// A loop the overhang split cut into pieces marches as one line: a piece ending just past a
    /// colour boundary does not hand the short run left on it to the colour before (a stub of
    /// that colour on the layer), nor does one starting just before it.
    #[test]
    fn a_loop_cut_into_pieces_keeps_its_colour_edges() {
        let rule = SplitRule { min_run: 0.4, ..rule() };
        let ring = vec![Pt::from_mm(0.0, 0.0), Pt::from_mm(20.0, 0.0), Pt::from_mm(20.0, 20.0), Pt::from_mm(0.0, 20.0)];
        let piece = |pts: &[(f64, f64)]| ExtrusionPath { pts: pts.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect(), ..line(&[0.0]) };
        // cut 0.2 past the boundary at x = 10 below, 0.2 before it above
        let walls = vec![piece(&[(0.0, 0.0), (10.2, 0.0)]), piece(&[(10.2, 0.0), (20.0, 0.0), (20.0, 20.0), (10.2, 20.0)]), ExtrusionPath { loop_pts: ring, ..piece(&[(10.2, 20.0), (0.0, 20.0)]) }];
        let ol = ObjectLayer { islands: vec![Arc::new(Island { walls, fills: Vec::new() })], ..layer(line(&[0.0])) };
        let out = split_layer(ol, 0.2, &half_plane(10.0), &fil_of, &[0, 1], &rule, None);
        let painted: Vec<Pt> = out[1].islands.iter().flat_map(|i| &i.walls).flat_map(|p| p.pts.iter().copied()).collect();
        let (low, high) = (painted.iter().filter(|p| p.y_mm() < 1.0), painted.iter().filter(|p| p.y_mm() > 19.0));
        let least = |ps: &mut dyn Iterator<Item = &Pt>| ps.map(|p| p.x_mm()).fold(f64::INFINITY, f64::min);
        assert!((least(&mut low.clone()) - 10.0).abs() < 0.002 && (least(&mut high.clone()) - 10.0).abs() < 0.002, "filament 2 starts at x = 10 on both sides: {painted:?}");
        let own: f64 = out[0].islands.iter().flat_map(|i| &i.walls).filter(|p| p.pts.iter().all(|q| q.y_mm() < 1.0)).map(length).sum();
        assert!((own - 10.0).abs() < 0.002, "filament 1 prints the low side up to x = 10: {own}");
    }

    /// A ring cut into many pieces at odd places, a band of filament 2 across it: every piece's
    /// colour is the paint's along it, wherever the cuts fall.
    #[test]
    fn a_ring_in_many_pieces_prints_its_band_where_it_is() {
        let rule = SplitRule { min_run: 0.4, ..rule() };
        let n = 400;
        let ring: Vec<Pt> = (0..n).map(|k| {
            let a = k as f64 / n as f64 * std::f64::consts::TAU;
            Pt::from_mm(10.0 * a.cos(), 10.0 * a.sin())
        }).collect();
        for cuts in [vec![7usize, 95, 101, 103, 180, 260, 300, 301, 399], vec![50, 98, 99, 100, 102, 104, 106, 250], vec![3, 120, 397]] {
            let mut walls = Vec::new();
            let mut from = 0;
            for &c in cuts.iter().chain(std::iter::once(&n)) {
                let to = c.min(n - 1);
                walls.push(ExtrusionPath { pts: ring[from..=to].to_vec(), ..line(&[0.0]) });
                from = to;
            }
            walls.last_mut().unwrap().loop_pts = ring.clone();
            let ol = ObjectLayer { islands: vec![Arc::new(Island { walls, fills: Vec::new() })], ..layer(line(&[0.0])) };
            let band: PaintAt = Arc::new(|x, y, _| PaintHere::solid(if y > 0.0 && x.abs() < 2.34 { 2 } else { 0 }, (x.abs() - 2.34).abs().min(y.abs())));
            let out = split_layer(ol, 0.2, &band, &fil_of, &[0, 1], &rule, None);
            for (k, ol) in out.iter().enumerate() {
                for p in ol.islands.iter().flat_map(|i| &i.walls) {
                    for w in p.pts.windows(2) {
                        let m = Pt::from_mm(0.5 * (w[0].x_mm() + w[1].x_mm()), 0.5 * (w[0].y_mm() + w[1].y_mm()));
                        let painted = m.y_mm() > 0.0 && m.x_mm().abs() < 2.34;
                        assert_eq!(painted, k == 1, "cuts {cuts:?}: filament {} prints at ({:.3}, {:.3})", k + 1, m.x_mm(), m.y_mm());
                    }
                }
            }
        }
    }

    #[test]
    fn short_sliver_keeps_original_path() {
        let path = ExtrusionPath {
            z: vec![0.0, 0.2],
            dry: vec![true],
            flow: vec![0.5],
            ..line(&[0.0, 20.0])
        };
        let original = path.clone();
        let rule = SplitRule { min_run: 1.0, ..rule() };
        let out = split_layer(layer(path), 0.2, &band(10.0, 10.3), &fil_of, &[0, 1], &rule, None);
        assert!(out[1].islands.is_empty());
        assert_eq!(out[0].islands[0].walls.len(), 1);
        let path = &out[0].islands[0].walls[0];
        assert_eq!(path.pts, original.pts);
        assert_eq!(path.closed, original.closed);
        assert_eq!(path.loop_pts, original.loop_pts);
        assert_eq!(path.z, original.z);
        assert_eq!(path.dry, original.dry);
        assert_eq!(path.flow, original.flow);
    }

    #[test]
    fn a_short_run_keeps_its_colour_where_it_shows_and_merges_inside() {
        let rule = SplitRule { min_run: 0.4, min_run_inside: 1.2, ..rule() };
        let runs = |role| {
            let out = split_layer(layer(ExtrusionPath { role, ..line(&[0.0, 20.0]) }), 0.2, &band(10.0, 10.8), &fil_of, &[0, 1], &rule, None);
            out[1].has_paths()
        };
        assert!(runs(Role::OuterWall));
        assert!(!runs(Role::InnerWall));
    }

    #[test]
    fn cut_interpolates_z_and_keeps_parent_segment_values() {
        let path = ExtrusionPath {
            z: vec![0.0, 0.1, 0.2],
            flow: vec![1.0, 0.5],
            dry: vec![false, true],
            ..line(&[0.0, 10.0, 20.0])
        };
        let out = split_layer(layer(path), 0.2, &half_plane(15.0), &fil_of, &[0, 1], &rule(), None);
        let left = &out[0].islands[0].walls[0];
        let right = &out[1].islands[0].walls[0];
        for piece in [left, right] {
            assert_eq!(piece.z.len(), piece.pts.len());
            assert_eq!(piece.flow.len(), piece.pts.len() - 1);
            assert_eq!(piece.dry.len(), piece.pts.len() - 1);
            for (p, z) in piece.pts.iter().zip(&piece.z) {
                assert!((z - p.x_mm() * 0.01).abs() < 0.000001);
            }
        }
        assert_eq!(left.pts[1], Pt::from_mm(10.0, 0.0));
        assert_eq!(left.flow, vec![1.0, 0.5]);
        assert_eq!(right.flow, vec![0.5]);
        assert_eq!(left.dry, vec![false, true]);
        assert_eq!(right.dry, vec![true]);
        assert!((right.z[0] - 0.15).abs() < 0.00002);
    }

    #[test]
    fn uniform_ring_samples_vertices_once_and_stays_closed() {
        for repeated_end in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let paint: PaintAt = Arc::new(move |_, _, z| {
                assert_eq!(z, 0.25);
                count.fetch_add(1, Ordering::Relaxed);
                PaintHere::solid(2, f64::INFINITY)
            });
            let mut ring = vec![Pt::from_mm(0.0, 0.0), Pt::from_mm(20.0, 0.0), Pt::from_mm(20.0, 20.0), Pt::from_mm(0.0, 20.0)];
            if repeated_end {
                ring.push(ring[0]);
            }
            let original = ExtrusionPath { closed: true, loop_pts: ring, ..line(&[0.0, 19.0]) };
            let out = split_layer(layer(original.clone()), 0.25, &paint, &fil_of, &[0, 1], &rule(), None);
            assert_eq!(calls.load(Ordering::Relaxed), 4);
            assert!(out[0].islands.is_empty());
            let path = &out[1].islands[0].walls[0];
            assert!(path.closed);
            assert_eq!(path.pts, original.pts);
            assert_eq!(path.loop_pts, original.loop_pts);
        }
    }

    #[test]
    fn distinct_states_of_same_filament_keep_original_path() {
        let paint: PaintAt = Arc::new(|x, _, _| PaintHere::solid(if x > 10.0 { 1 } else { 0 }, (x - 10.0).abs()));
        let original = line(&[0.0, 20.0]);
        let out = split_layer(layer(original.clone()), 0.2, &paint, &fil_of, &[0, 1], &rule(), None);
        assert!(out[1].islands.is_empty());
        assert_eq!(out[0].islands[0].walls.len(), 1);
        assert_eq!(out[0].islands[0].walls[0].pts, original.pts);
        let fallback = split_layer(layer(original.clone()), 0.2, &half_plane(10.0), &fil_of, &[0], &rule(), None);
        assert_eq!(fallback[0].islands[0].walls.len(), 1);
        assert_eq!(fallback[0].islands[0].walls[0].pts, original.pts);
    }

    #[test]
    fn layer_totals_include_walls_and_fills_and_preserve_island_order() {
        use makepad_fab_slicer::geom::{ExPolygon, Polygon};

        let slices = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(0.0, 0.0), Pt::from_mm(20.0, 20.0)))];
        let ol = ObjectLayer {
            islands: vec![
                Arc::new(Island::default()),
                Arc::new(Island { walls: vec![line(&[0.0, 20.0])], fills: vec![line(&[0.0, 20.0])] }),
                Arc::new(Island { walls: Vec::new(), fills: vec![line(&[0.0, 8.0])] }),
                Arc::new(Island { walls: Vec::new(), fills: vec![line(&[11.0, 12.0])] }),
            ],
            slices: Arc::new(slices.clone()),
            ..Default::default()
        };
        let rule = SplitRule { min_run: 1.0, ..rule() };
        let out = split_layer(ol.clone(), 0.2, &band(10.0, 12.0), &fil_of, &[0, 1], &rule, None);
        assert_eq!(*out[0].slices, slices);
        assert!(out[1].slices.is_empty());
        assert_eq!(out[0].islands.len(), 2);
        assert_eq!(out[1].islands.len(), 2);
        assert_eq!(out[0].islands[0].walls.len(), 2);
        assert_eq!(out[0].islands[0].fills.len(), 2);
        assert_eq!(out[1].islands[0].walls.len(), 1);
        assert_eq!(out[1].islands[0].fills.len(), 1);
        assert_eq!(out[0].islands[1].fills[0].pts, ol.islands[2].fills[0].pts);
        assert_eq!(out[1].islands[1].fills[0].pts, ol.islands[3].fills[0].pts);
    }

    #[test]
    fn ring_metadata_wraps_and_mismatched_arrays_are_dropped() {
        let ring = vec![Pt::from_mm(0.0, 0.0), Pt::from_mm(20.0, 0.0), Pt::from_mm(20.0, 20.0), Pt::from_mm(0.0, 20.0)];
        let mut path = ExtrusionPath {
            closed: true,
            loop_pts: ring,
            z: vec![0.0, 2.0, 4.0, 6.0],
            flow: vec![1.0, 2.0, 3.0, 4.0],
            dry: vec![false, true, false, true],
            ..line(&[0.0, 19.0])
        };
        let out = split_layer(layer(path.clone()), 0.2, &half_plane(15.0), &fil_of, &[0, 1], &rule(), None);
        let own = &out[0].islands[0].walls[0];
        let red = &out[1].islands[0].walls[0];
        assert_eq!(own.flow, vec![3.0, 4.0, 1.0]);
        assert_eq!(red.flow, vec![1.0, 2.0, 3.0]);
        assert_eq!(own.dry, vec![false, true, false]);
        assert_eq!(red.dry, vec![false, true, false]);
        for (piece, expected) in [(own, [4.5, 6.0, 0.0, 1.5]), (red, [1.5, 2.0, 4.0, 4.5])] {
            assert_eq!(piece.z.len(), piece.pts.len());
            assert_eq!(piece.z.len(), expected.len());
            for (z, expected) in piece.z.iter().zip(expected) {
                assert!((z - expected).abs() < 0.0002);
            }
        }
        // These arrays match the shortened pts, but not the walked ring.
        path.z = vec![0.0, 0.1];
        path.flow = vec![0.5];
        path.dry = vec![true];
        let out = split_layer(layer(path), 0.2, &half_plane(15.0), &fil_of, &[0, 1], &rule(), None);
        for ol in out {
            for piece in &ol.islands[0].walls {
                assert!(piece.z.is_empty());
                assert!(piece.flow.is_empty());
                assert!(piece.dry.is_empty());
            }
        }
    }
}

#[cfg(test)]
mod blend {
    //! A blend dithered round a cylinder's outer wall, layer after layer, read back as the
    //! filaments' map over (arc, layer).
    use super::*;
    use crate::Role;
    use std::f64::consts::TAU;
    use std::sync::Arc;

    const R: f64 = 10.0;
    const LAYERS: usize = 80;
    const H: f64 = 0.2;
    const W: f64 = 0.42;
    /// The map's resolution along the wall (mm).
    const DS: f64 = 0.05;
    const MIN_SWAP: f64 = 20.0 * W;

    type Share = Arc<dyn Fn(f64, f64, f64) -> f64 + Send + Sync>;

    /// Layer k's wall: a ring of radius R, its seam turned a golden angle from the last.
    fn ring(k: usize, role: Role) -> ExtrusionPath {
        let n = 400;
        let seam = (k as f64 * 0.618034).fract() * TAU;
        let loop_pts: Vec<Pt> = (0..n).map(|i| {
            let a = seam + i as f64 / n as f64 * TAU;
            Pt::from_mm(R * a.cos(), R * a.sin())
        }).collect();
        ExtrusionPath { pts: loop_pts.clone(), loop_pts, closed: true, role, width: W, height: H, area: 0.07, speed: 35.0, ..Default::default() }
    }

    /// Paint blending state 0 into state 2 by `share` (of state 2), as a `Mix` reports it.
    fn blend(share: Share) -> PaintAt {
        Arc::new(move |x, y, z| {
            let t = share(x, y, z).clamp(0.0, 1.0);
            if t <= 0.0 {
                PaintHere::solid(0, 0.0)
            } else if t >= 1.0 {
                PaintHere::solid(2, 0.0)
            } else if t <= 0.5 {
                PaintHere { state: 0, dist: 0.0, mix: Some((2, t as f32)) }
            } else {
                PaintHere { state: 2, dist: 0.0, mix: Some((0, (1.0 - t) as f32)) }
            }
        })
    }

    fn arc_of(p: Pt) -> f64 {
        p.y_mm().atan2(p.x_mm()).rem_euclid(TAU) * R
    }

    struct Printed {
        /// Per layer, per DS along the wall: 1 where filament 1 (state 2) prints.
        map: Vec<Vec<u8>>,
        /// The shortest piece cut from a ring (mm), and the pieces per layer.
        shortest: f64,
        pieces: f64,
    }

    /// `min_swap` 0: every layer prints its own share, however thin.
    fn print(paint: &PaintAt, role: Role, min_swap: f64) -> Printed {
        let per = TAU * R;
        let cols = (per / DS) as usize;
        let rule = SplitRule { min_run: W, min_run_inside: 3.0 * W, exact_fills: false, step: 0.5 * W, bead: W, height: H, min_swap };
        let fil_of = |state: u32| if state == 0 { 0 } else { state as usize - 1 };
        let mut out = Printed { map: vec![vec![0u8; cols]; LAYERS], shortest: f64::INFINITY, pieces: 0.0 };
        for k in 0..LAYERS {
            let z = (k as f64 + 0.5) * H;
            let ol = ObjectLayer { object: 1, filament: 0, islands: vec![Arc::new(Island { walls: vec![ring(k, role)], fills: Vec::new() })], ..Default::default() };
            for (fil, layer) in split_layer(ol, z, paint, &fil_of, &[0, 1], &rule, None).iter().enumerate() {
                for p in layer.islands.iter().flat_map(|i| &i.walls) {
                    out.pieces += 1.0 / LAYERS as f64;
                    if !p.closed {
                        out.shortest = out.shortest.min(p.length_mm());
                    }
                    if fil != 1 {
                        continue;
                    }
                    let (s0, mut s1) = if p.closed { (0.0, per) } else { (arc_of(p.first()), arc_of(p.last())) };
                    if s1 <= s0 {
                        s1 += per;
                    }
                    for (j, c) in out.map[k].iter_mut().enumerate() {
                        let mut s = (j as f64 + 0.5) * DS;
                        if s < s0 {
                            s += per;
                        }
                        if s < s1 {
                            *c = 1;
                        }
                    }
                }
            }
        }
        out
    }

    /// How the map reads against the share: the RMS difference once blurred by a gaussian of
    /// `sigma` mm (the eye at some distance), and the worst difference of one layer's mean.
    fn reads(map: &[Vec<u8>], share: &Share, sigma: f64) -> (f64, f64) {
        let cols = map[0].len();
        let ks = (3.0 * sigma / DS).ceil() as isize;
        let kz = (3.0 * sigma / H).ceil() as isize;
        let gauss = |d: f64| (-(d * d) / (2.0 * sigma * sigma)).exp();
        let gs: Vec<f64> = (-ks..=ks).map(|d| gauss(d as f64 * DS)).collect();
        let gz: Vec<f64> = (-kz..=kz).map(|d| gauss(d as f64 * H)).collect();
        let (ns, nz): (f64, f64) = (gs.iter().sum(), gz.iter().sum());
        let along: Vec<Vec<f64>> = map.iter().map(|row| (0..cols).map(|j| {
            gs.iter().enumerate().map(|(i, g)| g * row[(j as isize + i as isize - ks).rem_euclid(cols as isize) as usize] as f64).sum::<f64>() / ns
        }).collect()).collect();
        let (mut se, mut n, mut worst) = (0.0, 0.0, 0.0f64);
        for k in kz as usize..map.len() - kz as usize {
            let z = (k as f64 + 0.5) * H;
            let mut mean = 0.0;
            for j in 0..cols {
                let v = gz.iter().enumerate().map(|(i, g)| g * along[(k as isize + i as isize - kz) as usize][j]).sum::<f64>() / nz;
                let a = (j as f64 + 0.5) * DS / R;
                let t = share(R * a.cos(), R * a.sin(), z).clamp(0.0, 1.0);
                se += (v - t).powi(2);
                n += 1.0;
                mean += map[k][j] as f64 - t;
            }
            worst = worst.max((mean / cols as f64).abs());
        }
        ((se / n).sqrt(), worst)
    }

    fn shares() -> Vec<(&'static str, Share)> {
        vec![
            ("0.1", Arc::new(|_, _, _| 0.1)),
            ("0.3", Arc::new(|_, _, _| 0.3)),
            ("0.5", Arc::new(|_, _, _| 0.5)),
            ("0.8", Arc::new(|_, _, _| 0.8)),
            ("gradient", Arc::new(|_, _, z| z / (LAYERS as f64 * H))),
        ]
    }

    /// Each layer's wall prints the blend's share of each filament, in runs no shorter than a
    /// bead, and the layers together read as the blend from a millimetre's blur: within about a
    /// hundredth (a threshold by position instead of the carried error reads 0.013 to 0.017 at
    /// the middle shares, and no dither prints the nearer colour only).
    #[test]
    fn a_blend_prints_its_share_in_runs_of_a_bead() {
        for (name, share) in shares() {
            let printed = print(&blend(share.clone()), Role::OuterWall, 0.0);
            let (rms, worst) = reads(&printed.map, &share, 1.0);
            println!("{name}: blurred rms {rms:.4}, worst layer {worst:.4}, {:.0} pieces a layer, shortest {:.3} mm", printed.pieces, printed.shortest);
            assert!(worst < 0.02, "{name}: a layer prints {worst:.3} off its share");
            assert!(rms < 0.012, "{name}: reads {rms:.4} off the blend at 1 mm");
            assert!(printed.shortest >= W - 1e-6, "{name}: a run of {:.3} mm", printed.shortest);
        }
    }

    /// A blend too thin to be worth a head swap on every layer gathers its specks on fewer
    /// layers, each with about a swap's worth, and keeps its share over them.
    #[test]
    fn a_thin_blend_gathers_its_specks_on_fewer_layers() {
        let share: Share = Arc::new(|_, _, _| 0.02);
        let printed = print(&blend(share.clone()), Role::OuterWall, MIN_SWAP);
        let per: Vec<f64> = printed.map.iter().map(|row| row.iter().map(|&c| c as f64).sum::<f64>() * DS).collect();
        let with = per.iter().filter(|&&l| l > 0.0).count();
        let mean = per.iter().sum::<f64>() / (LAYERS as f64 * TAU * R);
        println!("{with} of {LAYERS} layers print the blend's second filament, {:.1} to {:.1} mm, share {mean:.4}", per.iter().copied().filter(|&l| l > 0.0).fold(f64::INFINITY, f64::min), per.iter().copied().fold(0.0, f64::max));
        assert!(with * 4 < LAYERS, "{with} of {LAYERS} layers print the second filament");
        assert!(per.iter().all(|&l| l == 0.0 || l >= 0.8 * MIN_SWAP), "a layer's specks {per:?}");
        assert!((mean - 0.02).abs() < 0.003, "the share prints {mean:.4}");
    }

    /// Deeper than the skin a fill takes a blend in cells of one material, the blend's nearer: a
    /// few long pieces, not hundreds of short ones; the wall round it prints as it does with no
    /// inside, piece for piece.
    #[test]
    fn a_fill_inside_takes_a_blend_in_cells() {
        let (half, pitch) = (20.0, 0.84);
        let lines = (half * 2.0 / pitch) as usize;
        let fills: Vec<ExtrusionPath> = (0..lines).map(|i| {
            let y = -half + (i as f64 + 0.5) * pitch;
            let pts = vec![Pt::from_mm(-half, y), Pt::from_mm(half, y)];
            ExtrusionPath { pts, role: Role::SparseInfill, width: W, height: H, area: 0.07, speed: 35.0, ..Default::default() }
        }).collect();
        let core = vec![ExPolygon::new(makepad_fab_slicer::geom::Polygon::rect(Pt::from_mm(-half + 1.0, -half + 1.0), Pt::from_mm(half - 1.0, half - 1.0)))];
        let inside = Inside { core, cell: 6.0 };
        // more of state 2 to the right: the cells take state 0 on the left, state 2 on the right
        let share: Share = Arc::new(move |x, _, _| 0.5 + x / (4.0 * half));
        let paint = blend(share);
        let rule = SplitRule { min_run: W, min_run_inside: 3.0 * W, exact_fills: false, step: 0.5 * W, bead: W, height: H, min_swap: 0.0 };
        let fil_of = |state: u32| if state == 0 { 0 } else { state as usize - 1 };
        let split = |inside: Option<&Inside>| {
            let ol = ObjectLayer { object: 1, filament: 0, islands: vec![Arc::new(Island { walls: vec![ring(3, Role::OuterWall)], fills: fills.clone() })], ..Default::default() };
            split_layer(ol, 10.1, &paint, &fil_of, &[0, 1], &rule, inside)
        };
        let (exact, coarse) = (split(None), split(Some(&inside)));
        let pieces = |out: &[ObjectLayer], wall: bool| out.iter().flat_map(|l| &l.islands).map(|i| if wall { i.walls.len() } else { i.fills.len() }).sum::<usize>();
        let length_of = |out: &[ObjectLayer], fil: usize| out[fil].islands.iter().flat_map(|i| &i.fills).map(|p| p.length_mm()).sum::<f64>();
        let walls = |out: &[ObjectLayer]| out.iter().map(|l| l.islands.iter().flat_map(|i| &i.walls).map(|p| p.pts.clone()).collect::<Vec<_>>()).collect::<Vec<_>>();
        let share_of = |out: &[ObjectLayer]| length_of(out, 1) / (length_of(out, 0) + length_of(out, 1));
        let right_of = |out: &[ObjectLayer], fil: usize| out[fil].islands.iter().flat_map(|i| &i.fills).flat_map(|p| p.pts.windows(2)).map(|w| (w[0].x_mm() + w[1].x_mm(), w[0].dist(w[1]) / SCALE)).fold((0.0, 0.0), |(m, l), (x2, d)| (m + 0.5 * x2 * d, l + d));
        let mean_x = |out: &[ObjectLayer], fil: usize| { let (m, l) = right_of(out, fil); m / l.max(1e-9) };
        println!("fill pieces {} exact, {} inside; share {:.3} exact, {:.3} inside", pieces(&exact, false), pieces(&coarse, false), share_of(&exact), share_of(&coarse));
        assert_eq!(walls(&exact), walls(&coarse), "the wall prints differently with an inside");
        assert!(pieces(&coarse, false) * 5 < pieces(&exact, false), "{} pieces inside, {} exact", pieces(&coarse, false), pieces(&exact, false));
        assert!((share_of(&coarse) - 0.5).abs() < 0.15, "the inside prints a share of {:.3}", share_of(&coarse));
        assert!(mean_x(&coarse, 0) < -5.0 && mean_x(&coarse, 1) > 5.0, "state 0 inside at x {:.1} mm, state 2 at {:.1} mm", mean_x(&coarse, 0), mean_x(&coarse, 1));
    }

    /// A colour that starts up the part (above 5 mm) prints inside the part on the layers it
    /// starts on, not below them: a layer under it prints its fills in the colour of its walls,
    /// with no second filament for the inside alone.
    #[test]
    fn a_fill_inside_takes_its_own_layers_colour() {
        let half = 20.0;
        let fills: Vec<ExtrusionPath> = (0..40).map(|i| {
            let y = -half + (i as f64 + 0.5);
            ExtrusionPath { pts: vec![Pt::from_mm(-half, y), Pt::from_mm(half, y)], role: Role::SparseInfill, width: W, height: H, area: 0.07, speed: 35.0, ..Default::default() }
        }).collect();
        let core = vec![ExPolygon::new(makepad_fab_slicer::geom::Polygon::rect(Pt::from_mm(-half + 1.0, -half + 1.0), Pt::from_mm(half - 1.0, half - 1.0)))];
        let inside = Inside { core, cell: 12.0 };
        let paint = blend(Arc::new(|_, _, z| if z > 5.0 { 1.0 } else { 0.0 }));
        let rule = SplitRule { min_run: W, min_run_inside: 3.0 * W, exact_fills: false, step: 0.5 * W, bead: W, height: H, min_swap: 0.0 };
        let fil_of = |state: u32| if state == 0 { 0 } else { state as usize - 1 };
        for (z, fil) in [(1.1, 0), (4.9, 0), (5.1, 1), (11.9, 1)] {
            let ol = ObjectLayer { object: 1, filament: 0, islands: vec![Arc::new(Island { walls: vec![ring(3, Role::OuterWall)], fills: fills.clone() })], ..Default::default() };
            let out = split_layer(ol, z, &paint, &fil_of, &[0, 1], &rule, Some(&inside));
            let fills_in = |f: usize| out[f].islands.iter().map(|i| i.fills.len()).sum::<usize>();
            assert!(fills_in(fil) == fills.len() && fills_in(1 - fil) == 0, "at {z} mm the inside prints {} pieces in filament 0, {} in 1", fills_in(0), fills_in(1));
        }
    }

    /// A cell whose middle lies off the part takes the paint where its fills are, not the paint
    /// off the part: a small part painted all over in one colour, across cells' edges, prints
    /// its inside in that colour alone, with nothing in the part's own filament.
    #[test]
    fn a_cell_off_the_part_takes_the_paint_of_its_fills() {
        // a 6 mm part from 7 to 13 mm, painted only on it: 12 mm cells' middles (6 and 18 mm),
        // and a 30 mm cell's (15 mm), all lie off it
        let (lo, hi) = (7.0, 13.0);
        let fills: Vec<ExtrusionPath> = (0..10).map(|i| {
            let y = lo + 1.0 + 0.4 * i as f64;
            ExtrusionPath { pts: vec![Pt::from_mm(lo + 1.0, y), Pt::from_mm(hi - 1.0, y)], role: Role::SparseInfill, width: W, height: H, area: 0.07, speed: 35.0, ..Default::default() }
        }).collect();
        let core = vec![ExPolygon::new(makepad_fab_slicer::geom::Polygon::rect(Pt::from_mm(lo + 0.5, lo + 0.5), Pt::from_mm(hi - 0.5, hi - 0.5)))];
        let on = move |v: f64| v > lo - 1.0 && v < hi + 1.0;
        let paint: PaintAt = Arc::new(move |x, y, _| {
            let edge = [x - (lo - 1.0), hi + 1.0 - x, y - (lo - 1.0), hi + 1.0 - y].into_iter().map(f64::abs).fold(f64::MAX, f64::min);
            PaintHere::solid(if on(x) && on(y) { 2 } else { 0 }, edge)
        });
        let rule = SplitRule { min_run: W, min_run_inside: 3.0 * W, exact_fills: false, step: 0.5 * W, bead: W, height: H, min_swap: 0.0 };
        let fil_of = |state: u32| if state == 0 { 0 } else { state as usize - 1 };
        for cell in [12.0, 30.0] {
            let inside = Inside { core: core.clone(), cell };
            let ol = ObjectLayer { object: 1, filament: 0, islands: vec![Arc::new(Island { walls: Vec::new(), fills: fills.clone() })], ..Default::default() };
            let out = split_layer(ol, 5.1, &paint, &fil_of, &[0, 1], &rule, Some(&inside));
            let fills_in = |f: usize| out[f].islands.iter().map(|i| i.fills.len()).sum::<usize>();
            assert!(fills_in(0) == 0 && fills_in(1) == fills.len(), "{cell} mm cells: {} pieces in the part's own filament, {} in its paint", fills_in(0), fills_in(1));
        }
    }

    /// Inside the part a blend's runs are the longer inside run: fewer pieces, the same share.
    #[test]
    fn inside_a_blend_prints_coarser() {
        let share: Share = Arc::new(|_, _, _| 0.3);
        let (wall, inner) = (print(&blend(share.clone()), Role::OuterWall, 0.0), print(&blend(share.clone()), Role::InnerWall, 0.0));
        let (_, worst) = reads(&inner.map, &share, 1.0);
        assert!(inner.shortest >= 3.0 * W - 1e-6, "inside run {:.3} mm", inner.shortest);
        assert!(inner.pieces < 0.5 * wall.pieces, "inside {:.0} pieces a layer, wall {:.0}", inner.pieces, wall.pieces);
        assert!(worst < 0.03, "inside, a layer prints {worst:.3} off its share");
    }
}

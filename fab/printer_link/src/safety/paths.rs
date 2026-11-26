//! What the head really draws: G2/G3 arcs simulated from their words (the
//! sweep the firmware computes, its swept points and its flow), and the
//! extrusions of each layer, so a move that ploughs through what the same
//! layer has already printed is caught. A wall can brush the edge of an earlier
//! bead as it turns. Contact counts towards a plough when the nozzle reaches
//! the earlier bead's centre line somewhere along that stretch.

use std::collections::HashMap;
use std::f64::consts::TAU;

/// Checked geometry-derived counts; no saturating float-to-integer allocation sizes.
pub(super) fn sample_count(length: f64, step: f64, minimum: usize, bound: f64) -> Option<usize> {
    if !length.is_finite() || length < 0.0 || !bound.is_finite() || bound <= 0.0
        || length > bound + 1e-6 || !step.is_finite() || step <= 0.0 { return None; }
    let count = (length / step).ceil().max(minimum as f64);
    if !count.is_finite() || count >= usize::MAX as f64 { return None; }
    let n = count as usize;
    n.checked_add(1)?.checked_mul(std::mem::size_of::<[f64; 3]>()).filter(|bytes| *bytes <= isize::MAX as usize)?;
    Some(n)
}

pub(super) fn sampled<T>(n: usize, mut point: impl FnMut(usize) -> T) -> Option<Vec<T>> {
    let count = n.checked_add(1)?;
    let mut pts = Vec::new();
    pts.try_reserve_exact(count).ok()?;
    for k in 0..count { pts.push(point(k)); }
    Some(pts)
}

/// The kind of path a `; FEATURE:` names, for the arc and crossing rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Loops around a part or a hole: an arc may sweep nearly a full turn.
    Wall,
    /// Support, its interface and transition: tree branches are circles.
    Support,
    /// Skirt and brim: loops.
    Skirt,
    /// Infill and surfaces of every kind: patterns may cross themselves.
    Infill,
    /// Ironing goes over the top surface it irons, by design.
    Ironing,
    /// The prime (wipe) tower.
    Tower,
    /// Anything else (custom, unnamed).
    Other,
}

pub fn class(feature: &str) -> Class {
    let f = feature.trim().to_ascii_lowercase();
    if f.contains("ironing") {
        Class::Ironing
    } else if f.contains("support") {
        Class::Support
    } else if f.contains("prime tower") || f.contains("wipe tower") {
        Class::Tower
    } else if f.contains("skirt") || f.contains("brim") {
        Class::Skirt
    } else if f.contains("wall") || f.contains("perimeter") || f.contains("vertical shell") {
        Class::Wall
    } else if f.contains("infill") || f.contains("surface") || f.contains("bridge") || f.contains("gap") {
        Class::Infill
    } else {
        Class::Other
    }
}

impl Class {
    /// The largest sweep (degrees) one extruding arc of this feature may draw. Loops
    /// (walls, support circles, skirt) may nearly close; a pattern or a surface turns
    /// less (the vendor references and Fab's slices: concentric surfaces ≤ 239°, prime
    /// tower ≤ 104°).
    pub fn max_sweep_deg(self) -> f64 {
        match self {
            Class::Wall | Class::Support | Class::Skirt => 359.0,
            _ => 270.0,
        }
    }
    /// Crossings between two paths of this class on one layer are by design.
    fn crosses_itself(self) -> bool {
        matches!(self, Class::Infill | Class::Support | Class::Tower | Class::Ironing)
    }
}

/// An arc as the printer draws it from its words.
#[derive(Clone, Copy, Debug)]
pub struct Arc {
    pub center: [f64; 2],
    /// The radius at the start (the firmware's), and at the end.
    pub r: f64,
    pub r_end: f64,
    /// Radians swept, in (0, 2π]; equal start and end angles are a full circle.
    pub sweep: f64,
    pub clockwise: bool,
    a0: f64,
}

impl Arc {
    pub fn new(from: [f64; 2], to: [f64; 2], ij: [f64; 2], clockwise: bool) -> Self {
        let center = [from[0] + ij[0], from[1] + ij[1]];
        let r = ij[0].hypot(ij[1]);
        let r_end = (to[0] - center[0]).hypot(to[1] - center[1]);
        let a0 = (from[1] - center[1]).atan2(from[0] - center[0]);
        let a1 = (to[1] - center[1]).atan2(to[0] - center[0]);
        let mut sweep = if clockwise { a0 - a1 } else { a1 - a0 }.rem_euclid(TAU);
        if sweep < 1e-9 {
            sweep = TAU;
        }
        Arc { center, r, r_end, sweep, clockwise, a0 }
    }
    pub fn len(&self) -> f64 {
        self.r * self.sweep
    }
    /// The point `t` (0..=1) of the way along the sweep.
    pub fn at(&self, t: f64) -> [f64; 2] {
        let a = if self.clockwise { self.a0 - t * self.sweep } else { self.a0 + t * self.sweep };
        // The firmware blends the radius from start to end.
        let r = self.r + (self.r_end - self.r) * t;
        [self.center[0] + r * a.cos(), self.center[1] + r * a.sin()]
    }
    /// Points along the sweep no more than ~0.5 mm and 2° apart, both ends and every
    /// axis extreme included: what the swept area is checked by.
    pub fn points(&self, bound: f64) -> Option<Vec<[f64; 2]>> {
        let step = (2f64.to_radians()).min(0.5 / self.r.max(1e-6));
        let n = sample_count(self.len(), step * self.r, 1, bound)?;
        let mut pts = sampled(n, |k| self.at(k as f64 / n as f64))?;
        pts.try_reserve(6).ok()?;
        pts.extend(self.extremes());
        Some(pts)
    }

    /// Endpoints and axis extremes, before any allocation based on arc length.
    pub fn extremes(&self) -> Vec<[f64; 2]> {
        let mut pts = vec![self.at(0.0), self.at(1.0)];
        for k in 0..4 {
            let a = k as f64 * std::f64::consts::FRAC_PI_2;
            let d = if self.clockwise { (self.a0 - a).rem_euclid(TAU) } else { (a - self.a0).rem_euclid(TAU) };
            if d <= self.sweep { pts.push(self.at(d / self.sweep)); }
        }
        pts
    }

    /// The arc as chords within 0.02 mm of it (for the crossing store).
    pub fn chords(&self, bound: f64) -> Option<Vec<[f64; 2]>> {
        let max = if self.r > 0.02 { 2.0 * (1.0 - 0.02 / self.r).clamp(-1.0, 1.0).acos() } else { 1.0 };
        let n = sample_count(self.len(), max.max(1e-3) * self.r, 1, bound)?;
        sampled(n, |k| self.at(k as f64 / n as f64))
    }
}

/// The filament per mm a bead of `width` × `height` needs (mm of filament of
/// `diameter`): a rectangle with rounded ends.
pub fn bead_e_per_mm(width: f64, height: f64, diameter: f64) -> f64 {
    // The slicer's bead: a rectangle with round sides, or an ellipse when thinner than tall.
    let area = if width >= height { (width - height) * height + std::f64::consts::PI * height * height / 4.0 } else { std::f64::consts::PI * width * height / 4.0 };
    area / (std::f64::consts::PI * diameter * diameter / 4.0)
}

/// How far below / above the feature's bead an extruding arc's filament per swept mm may
/// be: the references and Fab's slices stay within 0.94..1.37 of it; a travel-like sweep
/// while extruding (an arc written the long way round) is a few percent of it.
pub const ARC_FLOW: [f64; 2] = [0.5, 2.0];

/// One extrusion of a layer.
#[derive(Clone, Copy)]
struct Bead {
    a: [f64; 2],
    b: [f64; 2],
    half: f64,
    class: Class,
    line: usize,
    /// Its ends that end a run (where the run met what it started from or ran into).
    end_a: bool,
    end_b: bool,
}

const CELL: f64 = 1.0;

/// The extrusions printed so far at one height.
#[derive(Default)]
struct Layer {
    beads: Vec<Bead>,
    grid: HashMap<(i32, i32), Vec<u32>>,
}

fn cell(p: [f64; 2]) -> (i32, i32) {
    ((p[0] / CELL).floor() as i32, (p[1] / CELL).floor() as i32)
}

fn point_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (vx, vy) = (b[0] - a[0], b[1] - a[1]);
    let l = vx * vx + vy * vy;
    let t = if l == 0.0 { 0.0 } else { (((p[0] - a[0]) * vx + (p[1] - a[1]) * vy) / l).clamp(0.0, 1.0) };
    (p[0] - a[0] - t * vx).hypot(p[1] - a[1] - t * vy)
}

/// Two nonparallel segments meet if each straddles the other's line, endpoints included.
fn intersects_across(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    fn straddles(a: [f64; 2], b: [f64; 2], p: [f64; 2], q: [f64; 2]) -> bool {
        let side = |v: [f64; 2]| (b[0] - a[0]) * (v[1] - a[1]) - (b[1] - a[1]) * (v[0] - a[0]);
        let (p, q) = (side(p), side(q));
        (p <= 0.0 && q >= 0.0) || (q <= 0.0 && p >= 0.0)
    }
    straddles(a, b, c, d) && straddles(c, d, a, b)
}

/// A move may run this much (in units of its width) over an earlier bead of another
/// feature at its own height, away from its ends, before it counts as ploughing through
/// it. A move at right angles over one bead runs one width over it.
pub const CROSS_WIDTHS: f64 = 2.5;

/// The sine of the smallest angle between a move and a bead that counts as crossing it (30°).
const CROSS_SIN: f64 = 0.5;

/// Along a bead, how near its centre line (in half widths) a move is on top of it.
const ON_TOP: f64 = 0.25;

/// The extrusions of the layers in progress, keyed by height (µm), and the run
/// (consecutive extrusions of one feature at one height) being printed.
#[derive(Default)]
pub struct Printed {
    layers: HashMap<i64, Layer>,
    features: Vec<String>,
    top: i64,
    run: Option<Run>,
    bound: f64,
    pub(super) sample_error: bool,
}

struct Run {
    key: i64,
    feature: u32,
    class: Class,
    /// Points, and per segment its half width and line.
    pts: Vec<[f64; 2]>,
    segs: Vec<(f64, usize)>,
}

/// An earlier bead that a run of extrusions goes over: the line of the move that does,
/// the earlier bead's line, and how far the run goes over beads (mm).
#[derive(Debug)]
pub struct Crossing {
    pub line: usize,
    pub over_line: usize,
    pub length: f64,
}

impl Printed {
    pub(super) fn new(bound: f64) -> Self { Self { bound, ..Self::default() } }

    fn feature_id(&mut self, feature: &str) -> u32 {
        match self.features.iter().position(|f| f == feature) {
            Some(i) => i as u32,
            None => {
                self.features.push(feature.to_string());
                (self.features.len() - 1) as u32
            }
        }
    }

    /// An extruding move (a line, or an arc's chords; `path[0]` is where the head was) at
    /// height `z`. It continues the run in progress or, ending that, starts one; a run is
    /// checked against its height when it ends.
    pub fn extrude(&mut self, z: f64, path: &[[f64; 2]], width: f64, feature: &str, line: usize) -> Option<Crossing> {
        let key = (z * 1000.0).round() as i64;
        let fid = self.feature_id(feature);
        let half = width.max(0.1) * 0.5;
        let continues = self.run.as_ref().is_some_and(|r| r.key == key && r.feature == fid && r.pts.last().is_some_and(|p| (p[0] - path[0][0]).hypot(p[1] - path[0][1]) < 1e-3));
        let found = if continues { None } else { self.finish() };
        let run = self.run.get_or_insert_with(|| Run { key, feature: fid, class: class(feature), pts: vec![path[0]], segs: Vec::new() });
        for p in &path[1..] {
            run.pts.push(*p);
            run.segs.push((half, line));
        }
        found
    }

    /// The run in progress ends (a travel, a retraction, the end of the file): check it
    /// against what its height already has, then add it there.
    pub fn finish(&mut self) -> Option<Crossing> {
        let run = self.run.take()?;
        if run.key > self.top {
            self.top = run.key;
            // Heights well below the top are finished layers: the nozzle is above them.
            let top = self.top;
            self.layers.retain(|k, _| *k > top - 2000);
        }
        let layer = self.layers.entry(run.key).or_default();
        for (k, w) in run.pts.windows(2).enumerate() {
            let length = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
            for step in [CELL * 0.5, (run.segs[k].0 * 0.5).min(0.1)] {
                if sample_count(length, step, 1, self.bound).is_none() {
                    self.sample_error = true;
                    return None;
                }
            }
        }
        let found = Self::crossing(layer, &run, self.bound);
        let last = run.segs.len().saturating_sub(1);
        for (k, w) in run.pts.windows(2).enumerate() {
            let (half, line) = run.segs[k];
            let i = layer.beads.len() as u32;
            layer.beads.push(Bead { a: w[0], b: w[1], half, class: run.class, line, end_a: k == 0, end_b: k == last });
            // Every cell within one cell of the segment (samples at half a cell).
            let len = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
            let n = sample_count(len, CELL * 0.5, 1, self.bound)?;
            let mut last = None;
            for s in 0..=n {
                let t = s as f64 / n as f64;
                let c = cell([w[0][0] + (w[1][0] - w[0][0]) * t, w[0][1] + (w[1][1] - w[0][1]) * t]);
                if last == Some(c) {
                    continue;
                }
                last = Some(c);
                for dx in -1..=1 {
                    for dy in -1..=1 {
                        let v = layer.grid.entry((c.0 + dx, c.1 + dy)).or_default();
                        if v.last() != Some(&i) {
                            v.push(i);
                        }
                    }
                }
            }
        }
        found
    }

    /// Sample contact with earlier beads at this height every quarter width, away from
    /// the run's ends. Count a continuous stretch when a segment intersects a bead's
    /// centre line or a sample is close enough to ride on top. A turn that only brushes
    /// the edge of a bead contributes no length.
    fn crossing(layer: &Layer, run: &Run, bound: f64) -> Option<Crossing> {
        if run.class == Class::Ironing || layer.beads.is_empty() || run.segs.is_empty() {
            return None;
        }
        let total: f64 = run.pts.windows(2).map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1])).sum();
        let half0 = run.segs.iter().map(|s| s.0).fold(0.0, f64::max);
        // The ends of a run meet what it starts from and runs into (a seam, an infill line
        // into its wall): two widths at each end are not counted.
        let end = 4.0 * half0;
        if total <= 2.0 * end {
            return None;
        }
        let mut over = 0.0;
        let mut at = 0.0;
        let mut first: Option<(usize, usize)> = None;
        let mut stretch_first = None;
        // Hold the length from the first contact until this stretch reaches a centre line.
        let mut pending = 0.0;
        let mut reached_centre = false;
        for (k, w) in run.pts.windows(2).enumerate() {
            let (half, line) = run.segs[k];
            let step = (half * 0.5).min(0.1);
            let len = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
            let n = sample_count(len, step, 0, bound)?;
            for s in 0..n {
                let d = at + (s as f64 + 0.5) * len / n as f64;
                if d < end || d > total - end {
                    continue;
                }
                let t = (s as f64 + 0.5) / n as f64;
                let p = [w[0][0] + (w[1][0] - w[0][0]) * t, w[0][1] + (w[1][1] - w[0][1]) * t];
                let dir = [(w[1][0] - w[0][0]) / len, (w[1][1] - w[0][1]) / len];
                let mut hit = None;
                for &i in layer.grid.get(&cell(p)).into_iter().flatten() {
                    let b = &layer.beads[i as usize];
                    let exempt = (b.class == run.class && run.class.crosses_itself()) || b.class == Class::Ironing;
                    // The ends of an earlier run reach into what it ran into (an infill line's
                    // end into the wall printed after it): two of its widths are not counted.
                    let tip = |e: bool, q: [f64; 2]| e && (p[0] - q[0]).hypot(p[1] - q[1]) < 4.0 * b.half;
                    if exempt || tip(b.end_a, b.a) || tip(b.end_b, b.b) {
                        continue;
                    }
                    // Over the bead: across it within the narrower half width of its centre line,
                    // or along it on top of it (a path printed twice). Beads laid side by side,
                    // overlapping in a thin wall, run along each other a little apart.
                    let (bx, by) = (b.b[0] - b.a[0], b.b[1] - b.a[1]);
                    let bl = bx.hypot(by);
                    let across = bl > 0.0 && (dir[0] * by - dir[1] * bx).abs() / bl > CROSS_SIN;
                    let d = point_segment(p, b.a, b.b);
                    let reach = b.half.min(half);
                    let on_top = d < ON_TOP * reach;
                    if on_top || (across && d < reach) {
                        // Several beads can touch one sample. Any of them can confirm the stretch.
                        hit.get_or_insert(b.line);
                        if reached_centre || on_top || intersects_across(w[0], w[1], b.a, b.b) {
                            reached_centre = true;
                            break;
                        }
                    }
                }
                if let Some(over_line) = hit {
                    let lines = *stretch_first.get_or_insert((line, over_line));
                    pending += len / n as f64;
                    if reached_centre {
                        first.get_or_insert(lines);
                        over += pending;
                        pending = 0.0;
                    }
                } else {
                    stretch_first = None;
                    pending = 0.0;
                    reached_centre = false;
                }
            }
            at += len;
        }
        let (line, over_line) = first?;
        (over > CROSS_WIDTHS * 2.0 * half0).then_some(Crossing { line, over_line, length: over })
    }
}

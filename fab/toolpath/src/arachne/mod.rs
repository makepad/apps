//! Variable-width walls (`wall_generator = arachne`).
//!
//! Each island is shrunk by half a bead overlap, its skeleton is built from
//! a Delaunay triangulation of boundary samples, and the local thickness
//! across it decides how many beads fit and how wide each is: where classic
//! walls leave a gap or print two beads on top of each other, the beads
//! here narrow and widen, and a thin feature gets one bead of its own width.

mod beading;
mod beads;
mod delaunay;
mod skeleton;
#[cfg(test)]
mod tests;

use crate::{flow, prof};
use makepad_fab_settings::machine::Job;
use makepad_fab_slicer::clip::{union, FillRule};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::{offset_ex, Join};
use makepad_fab_slicer::source::BeadLine;
use std::collections::HashMap;

/// A job's `wall_generator = arachne` settings: lengths in mm (their
/// percentages are of the smallest nozzle across all extruders), the angle
/// in radians. A key the job does not set takes its default.
#[derive(Clone, Debug)]
pub struct Settings {
    pub min_bead_width: f64,
    pub min_feature_size: f64,
    pub transition_length: f64,
    pub transition_filter_deviation: f64,
    pub transition_angle: f64,
    pub distribution_count: usize,
}

impl Settings {
    /// None when the job uses classic walls.
    pub fn from_job(job: &Job) -> Option<Self> {
        (job.s("wall_generator") == "arachne").then(|| Self::of_job(job))
    }

    /// The job's bead settings, whichever walls it uses: classic gap fill runs on them too.
    pub fn of_job(job: &Job) -> Self {
        let nozzle = (0..job.extruders()).map(|e| job.nozzle_diameter(e)).filter(|d| *d > 0.0).fold(f64::INFINITY, f64::min);
        let nozzle = if nozzle.is_finite() { nozzle } else { 0.4 };
        let num = |k: &str, default: f64| if job.cfg.has(k) { job.f(k) } else { default };
        let pct = |k: &str, default: f64| nozzle * num(k, default) / 100.0;
        Settings {
            min_bead_width: pct("min_bead_width", 85.0),
            min_feature_size: pct("min_feature_size", 25.0),
            transition_length: pct("wall_transition_length", 100.0),
            transition_filter_deviation: pct("wall_transition_filter_deviation", 25.0),
            transition_angle: num("wall_transition_angle", 10.0).to_radians(),
            distribution_count: num("wall_distribution_count", 1.0).max(0.0) as usize,
        }
    }

    /// One layer's walls: `loops` per side of the given extrusion widths at height `h`.
    pub fn config(&self, loops: usize, w_outer: f64, w_inner: f64, h: f64, precise: bool) -> Config {
        Config {
            loops,
            w_outer,
            w_inner,
            s_outer: flow::spacing(w_outer, h),
            s_inner: flow::spacing(w_inner, h),
            min_bead_width: self.min_bead_width,
            min_feature_size: self.min_feature_size,
            transition_length: self.transition_length,
            transition_filter_deviation: self.transition_filter_deviation,
            transition_angle: self.transition_angle,
            distribution_count: self.distribution_count,
            precise,
            widen: true,
        }
    }
}

/// The settings of one layer's walls. Distances in mm, the angle in radians.
#[derive(Clone, Debug)]
pub struct Config {
    pub loops: usize,
    /// Extrusion widths and their spacings (bead widths) of outer and inner walls.
    pub w_outer: f64,
    pub w_inner: f64,
    pub s_outer: f64,
    pub s_inner: f64,
    pub min_bead_width: f64,
    pub min_feature_size: f64,
    pub transition_length: f64,
    pub transition_filter_deviation: f64,
    pub transition_angle: f64,
    pub distribution_count: usize,
    pub precise: bool,
    /// A lone bead narrower than `min_bead_width` prints at that width. Off between walls, where
    /// a wider bead would run into them.
    pub widen: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Walls {
    pub lines: Vec<BeadLine>,
    /// The infill region inside the walls.
    pub inner: Vec<ExPolygon>,
}

/// Exact local parts and their simplified output, owned by one wall/gap-fill call.
pub(crate) struct Memo {
    cfg: Config,
    parts: HashMap<u64, Vec<(ExPolygon, PartOutput)>>,
}

struct PartOutput {
    lines: Vec<BeadLine>,
    markers: Vec<Vec<Pt>>,
}

impl Memo {
    pub(crate) fn new(cfg: &Config) -> Self {
        Self { cfg: cfg.clone(), parts: HashMap::new() }
    }

    /// The settings every part in the memo was made with.
    pub(crate) fn cfg(&self) -> &Config {
        &self.cfg
    }

    fn part(&mut self, local: ExPolygon, cfg: &Config, st: &beading::Strategy, reach: f64) -> &PartOutput {
        // A cheap mix of every coordinate: it only picks the bucket.
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for ring in local.rings() {
            hash = (hash ^ ring.pts.len() as u64).wrapping_mul(0x100_0000_01b3);
            for q in &ring.pts {
                hash = (hash ^ q.x as u64).wrapping_mul(0x100_0000_01b3);
                hash = (hash ^ q.y as u64).wrapping_mul(0x100_0000_01b3);
            }
        }
        let bucket = self.parts.entry(hash).or_default();
        // The hash only selects a bucket: every ring and point must match in order.
        let index = if let Some(index) = bucket.iter().position(|(part, _)| *part == local) {
            index
        } else {
            let output = part_output(&local, cfg, st, reach);
            bucket.push((local, output));
            bucket.len() - 1
        };
        &bucket[index].1
    }
}

fn part_output(local: &ExPolygon, cfg: &Config, st: &beading::Strategy, reach: f64) -> PartOutput {
    let skeleton_t = prof::scope(prof::Stage::ArachneSkeleton);
    let mut sk = skeleton::build(local, cfg.transition_angle, reach);
    drop(skeleton_t);
    let beads_t = prof::scope(prof::Stage::ArachneBeads);
    let (halves, owner) = beads::layouts(&mut sk, cfg, st);
    drop(beads_t);
    let _trace_t = prof::scope(prof::Stage::ArachneTrace);
    let (mut lines, rings) = beads::trace(&sk, &halves, &owner, cfg, st);
    for line in &mut lines {
        simplify(line, SIMPLIFY, SIMPLIFY_WIDTH);
    }
    let markers = rings.into_iter().map(|ring| simplify_ring(&ring, SIMPLIFY * SCALE)).collect();
    PartOutput { lines, markers }
}

impl Config {
    /// Outline preparation inset, precise outer-bead shift, and nominal wall
    /// centres (inset, bead spacing, extrusion width). The skeleton lays beads
    /// at spacing from the prepared outline; only the outer bead shifts back.
    /// Both CPU preparation and the field planner use this same construction.
    pub fn nominal_wall_levels(&self) -> (f64, f64, impl Iterator<Item = (f64, f64, f64)> + '_) {
        let shrink = (self.w_outer - self.s_outer) * if self.precise { 1.0 } else { 0.5 };
        let shift = if self.precise { 0.5 * (self.w_outer - self.s_outer) } else { 0.0 };
        let levels = (0..self.loops).map(move |k| {
            let d = if k == 0 { shrink + self.s_outer * 0.5 - shift }
                else { shrink + self.s_outer + (k - 1) as f64 * self.s_inner + self.s_inner * 0.5 };
            (d, if k == 0 { self.s_outer } else { self.s_inner }, if k == 0 { self.w_outer } else { self.w_inner })
        });
        (shrink, shift, levels)
    }

    /// Strategy's cap -> cap+1 boundary is nominal(cap+1) - 0.01.
    /// Half that thickness minus the nominal occupied radius in the prepared
    /// outline (s_outer + (loops-1)*s_inner) is s_inner/2 - 0.005.
    /// A central path's radius slope is at most sin(transition_angle/2): add
    /// the whole transition ramp and half the filter deviation to keep their
    /// influence outside islands accepted as nominal. min_bead_width affects
    /// lower count thresholds, not this nominal-layout boundary.
    pub fn nominal_wall_margin(&self) -> f64 {
        if self.loops == 0 { return 0.0; }
        let strategy = beading::Strategy::new(self).expect("validated arachne settings");
        let side = self.s_outer + (self.loops - 1) as f64 * self.s_inner;
        (0.5 * strategy.transition_thickness(strategy.cap) - side).max(0.0)
            + self.transition_length * (0.5 * self.transition_angle).sin()
            + 0.5 * self.transition_filter_deviation
    }

    /// Refuse settings no bead layout can honour.
    pub fn check(&self) -> Result<(), String> {
        let pos = [self.w_outer, self.w_inner, self.s_outer, self.s_inner, self.transition_length];
        if pos.iter().any(|v| !v.is_finite() || *v <= 0.0 || *v > 100.0) {
            return Err("wall widths and wall_transition_length must be finite and positive".into());
        }
        if !(self.min_bead_width.is_finite() && self.min_bead_width > 0.0 && self.min_bead_width <= 100.0) {
            return Err(format!("min_bead_width {:.3} mm (must be positive)", self.min_bead_width));
        }
        if !(self.min_feature_size.is_finite() && self.min_feature_size > 0.0 && self.min_feature_size <= 100.0) {
            return Err(format!("min_feature_size {:.3} mm (must be positive)", self.min_feature_size));
        }
        if !(self.transition_filter_deviation.is_finite() && self.transition_filter_deviation >= 0.0) {
            return Err("wall_transition_filter_deviation must be finite and not negative".into());
        }
        if !(self.transition_angle >= 1f64.to_radians() - 1e-9 && self.transition_angle <= 59f64.to_radians() + 1e-9) {
            return Err(format!("wall_transition_angle {:.1} (must be between 1 and 59 degrees)", self.transition_angle.to_degrees()));
        }
        if self.distribution_count == 0 {
            return Err("wall_distribution_count must be at least 1".into());
        }
        beading::Strategy::new(self).map(|_| ()).ok_or_else(|| {
            format!(
                "min_bead_width {:.3} mm and min_feature_size {:.3} mm give no increasing bead count for {:.3}/{:.3} mm walls",
                self.min_bead_width, self.min_feature_size, self.w_outer, self.w_inner
            )
        })
    }
}

/// The regions the skeleton is built on: the island shrunk by half a bead
/// overlap, without cracks, slivers and specks, simplified and valid.
fn prepare(island: &ExPolygon, cfg: &Config) -> Vec<ExPolygon> {
    // Beads touch at spacing: the outline moves in by half the overlap (all
    // of it for a precise outer wall, whose outer bead moves back out). Out
    // and back in, cracks under 0.025 mm close; the shrink drops slivers
    // under twice its width.
    let (shrink, _, _) = cfg.nominal_wall_levels();
    let eps = 0.0125 * SCALE;
    let grown = offset_ex(std::slice::from_ref(island), eps, Join::Miter(2.0));
    let src = offset_ex(&grown, -eps - shrink * SCALE, Join::Round(0.005 * SCALE));
    let mut rings = Vec::new();
    for e in &src {
        for r in e.rings() {
            let mut pts = simplify_ring(&r.pts, 0.0025 * SCALE);
            clean_ring(&mut pts);
            if pts.len() >= 3 {
                rings.push(Polygon::new(pts));
            }
        }
    }
    let min_area = (0.5 * cfg.s_outer).powi(2);
    let mut parts = union(&rings, FillRule::Positive);
    parts.retain(|part| {
        let b = part.bbox();
        part.area_mm2() >= min_area && b.max.x - b.min.x < 1 << 30 && b.max.y - b.min.y < 1 << 30 && holds_a_bead(part, cfg)
    });
    parts
}

/// Under this (mm) of the depth a bead needs, [`holds_a_bead`] still answers yes: the offset's
/// rounding moves a ring by a few units, this is a thousand.
const BEAD_DEPTH_MARGIN: f64 = 0.001;

/// Whether any point of a prepared part lies deep enough for a bead. A bead count above zero takes
/// a skeleton node at half the narrower of the smallest feature and the smallest lone bead from
/// the outline, and a part without one comes out of the skeleton with no line and no fill, so it
/// need not build it. The round join's chords never pass their arcs, so the inset holds every
/// point that deep.
fn holds_a_bead(part: &ExPolygon, cfg: &Config) -> bool {
    let depth = cfg.min_feature_size.min(cfg.s_inner) / 2.0 - BEAD_DEPTH_MARGIN;
    depth <= 0.0 || !offset_ex(std::slice::from_ref(part), -depth * SCALE, Join::Round(0.005 * SCALE)).is_empty()
}

/// Bead lines and the infill outline keep their shape to this (mm) and a
/// bead's width to `SIMPLIFY_WIDTH` (mm) when points are dropped.
const SIMPLIFY: f64 = 0.0125;
const SIMPLIFY_WIDTH: f64 = 0.005;

/// Drop the points of a bead line that lie within `tol` of the chord
/// between the points kept around them, and whose width the chord's
/// interpolation keeps within `wtol`: fewer, longer moves, the same bead.
pub(crate) fn simplify(l: &mut BeadLine, tol: f64, wtol: f64) {
    if l.pts.len() < 3 {
        return;
    }
    // A closed line runs back round to its first point.
    let mut pts: Vec<(f64, f64)> = l.pts.iter().map(|p| (p.x_mm(), p.y_mm())).collect();
    let mut widths = l.widths.clone();
    if l.closed {
        pts.push(pts[0]);
        widths.push(widths[0]);
    }
    let m = pts.len();
    let mut keep = vec![false; m];
    keep[0] = true;
    keep[m - 1] = true;
    let mut spans = vec![(0, m - 1)];
    while let Some((a, b)) = spans.pop() {
        let ((ax, ay), (bx, by)) = (pts[a], pts[b]);
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let (mut worst, mut at) = (1.0, 0);
        for i in a + 1..b {
            let (px, py) = pts[i];
            let t = if len2 > 0.0 { (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            let d = (px - ax - t * dx).hypot(py - ay - t * dy);
            let w = widths[a] + t * (widths[b] - widths[a]);
            let err = (d / tol).max((widths[i] - w).abs() / wtol);
            if err > worst {
                worst = err;
                at = i;
            }
        }
        if at > 0 {
            keep[at] = true;
            spans.push((a, at));
            spans.push((at, b));
        }
    }
    let n = if l.closed { m - 1 } else { m };
    let kept: Vec<usize> = (0..n).filter(|&i| keep[i]).collect();
    l.pts = kept.iter().map(|&i| l.pts[i]).collect();
    l.widths = kept.iter().map(|&i| l.widths[i]).collect();
}

/// Variable-width walls of one island.
pub fn walls(island: &ExPolygon, cfg: &Config) -> Walls {
    walls_with(island, cfg, None)
}

/// [`walls`] with the memo's settings, taking a part the memo has seen from it: a part's output
/// depends only on the part in local units and the settings.
pub(crate) fn walls_memo(island: &ExPolygon, memo: &mut Memo) -> Walls {
    let cfg = memo.cfg.clone();
    walls_with(island, &cfg, Some(memo))
}

fn walls_with(island: &ExPolygon, cfg: &Config, mut memo: Option<&mut Memo>) -> Walls {
    if cfg.loops == 0 {
        return Walls { lines: Vec::new(), inner: vec![island.clone()] };
    }
    // Settings are checked before slicing (`Config::check`).
    let Some(st) = beading::Strategy::new(cfg) else { return Walls::default() };
    let mut out = Walls::default();
    let mut markers = Vec::new();
    // Beyond the last count change (plus a bead) the layout is fixed.
    let reach = st.transition_thickness(st.cap) + cfg.s_inner;
    let skeleton_t = prof::scope(prof::Stage::ArachneSkeleton);
    let parts = prepare(island, cfg);
    drop(skeleton_t);
    for part in parts {
        let o = part.bbox().min;
        let mut local = part;
        local.translate(Pt::new(-o.x, -o.y));
        let (lines, rings) = match memo.as_deref_mut() {
            Some(memo) => {
                let output = memo.part(local, cfg, &st, reach);
                (output.lines.clone(), output.markers.clone())
            }
            None => {
                let output = part_output(&local, cfg, &st, reach);
                (output.lines, output.markers)
            }
        };
        for mut l in lines {
            for p in &mut l.pts {
                *p = p.add(o);
            }
            out.lines.push(l);
        }
        for r in rings {
            markers.push(Polygon::new(r.into_iter().map(|p| p.add(o)).collect()));
        }
    }
    out.inner = union(&markers, FillRule::Positive);
    out
}

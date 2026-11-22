// Ported from TiNRS Psychedelic Router 0.1.0: src/optimize/Cleanup.h (4-164), src/optimize/Cleanup.cpp (42-1408, 1519-1550, 1579-1747).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in the parent folder.

//! Finish cleanup on trace skeletons, with local geometry checks and a caller-owned judge.
//! ADAPTED cpp:1630-1648 (L6.2 ledger): a refused step restores named changed nets to a
//! fixpoint before whole-step undo. This net fixpoint is ours, not the reference's; with it the
//! edit counters are kept per net during a step and count only the edits that stand.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use super::super::diffpairs::CouplingPoly;
use super::super::geom::{std_clamp, std_max, std_min, Box2, Segment2, Vec2, K_PI};
use super::super::model::{OBoard, Trace, TracePoint, Via};
use super::super::padgeom;
use super::super::simplify::reverse_points;
use super::drcgeom;
use super::pairguard::PairGuard;
use super::{fanout_via_mask, Guard};

/// Cleanup.h:69-112. Distances are mm, angles degrees.
// ADAPTED h:96-105,111: no avoidPadGaps, timeBudgetMs, DrcOptions or verbose.
#[derive(Clone, Debug)]
pub struct CleanupOptions {
    pub rules: bool,
    pub collinear: bool,
    pub vias: bool,
    pub unloop: bool,
    pub detours: bool,
    pub acute: bool,
    pub collinear_tol: f32,
    pub via_rounds: i32,
    pub spike_turn_deg: f32,
    pub hop_reroute: bool,
    pub hop_max_length: f32,
    pub hop_max_extra: f32,
    pub detour_window: i32,
    pub detour_min_gain: f32,
    pub detour_elbows: bool,
    pub detour_rounds: i32,
    pub max_turn_deg: f32,
    pub acute_prefer135: bool,
    pub acute_max_leg: f32,
    pub tighten_rounds: i32,
    pub tighten_step: f32,
    pub tighten_max_cut: f32,
    pub clearance_slack: f32,
    pub step_guard: bool,
    pub edit_imported: bool,
    pub pair_aware: bool,
    pub resample: bool,
    pub resample_spacing: f32,
    pub corner_guard: f32,
}

impl Default for CleanupOptions {
    fn default() -> Self {
        Self {
            rules: true,
            collinear: true,
            vias: true,
            unloop: true,
            detours: true,
            acute: true,
            collinear_tol: 0.01,
            via_rounds: 4,
            spike_turn_deg: 170.0,
            hop_reroute: true,
            hop_max_length: 3.0,
            hop_max_extra: 6.0,
            detour_window: 8,
            detour_min_gain: 0.2,
            detour_elbows: true,
            detour_rounds: 12,
            max_turn_deg: 90.0,
            acute_prefer135: true,
            acute_max_leg: 0.6,
            tighten_rounds: 0,
            tighten_step: 0.1,
            tighten_max_cut: 2.0,
            clearance_slack: 0.002,
            step_guard: true,
            edit_imported: false,
            pair_aware: true,
            resample: true,
            resample_spacing: 0.8,
            corner_guard: 0.15,
        }
    }
}

/// ADAPTED cpp:1410-1517,1682-1684: the rules step needs per-item guard findings.
pub fn unported(opt: &CleanupOptions) -> Vec<&'static str> {
    if opt.rules { vec!["rules"] } else { Vec::new() }
}

/// Cleanup.h:114-126. ADAPTED h:124: no elapsed time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CleanupStepStats {
    pub name: String,
    pub applied: i32,
    pub rejected: i32,
    pub pair_skipped: i32,
    pub pair_edits: i32,
    pub segment_gain: i32,
    pub via_gain: i32,
    pub length_gain: f64,
    pub reverted: bool,
    /// Nets restored on partial accept (the net fixpoint); `applied` and `pair_edits` count
    /// only the edits on the nets that stand.
    pub nets_reverted: usize,
    /// ADAPTED cpp:143-144,1622-1628: retain diagnostics as counters, without printing.
    /// Same-net crossing, trace, pad, pad hole, via, edge, keepout (in reference order).
    pub fails: [i64; 7],
    pub self_cross: i64,
}

/// Cleanup.h:128-152. ADAPTED h:146-151: no budget, clock, reports or summary;
/// `judge_calls` counts all step/final calls, including refusals.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CleanupStats {
    pub segments_before: i32,
    pub segments_after: i32,
    pub nodes_before: i32,
    pub nodes_after: i32,
    pub vias_before: i32,
    pub vias_after: i32,
    pub traces_before: i32,
    pub traces_after: i32,
    pub length_before: f64,
    pub length_after: f64,
    pub loops_removed: i32,
    pub elbows: i32,
    pub straight_shortcuts: i32,
    pub chamfers: i32,
    pub vias_removed: i32,
    pub hops_rerouted: i32,
    pub hop_candidates: i32,
    pub hop_single_crossing: i32,
    pub step_reverts: i32,
    /// Sum of nets restored by partially accepted steps. The edit counters (vias_removed,
    /// chamfers, ...) count only the edits that stand: none of a reverted step, and none on a
    /// net the fixpoint put back.
    pub nets_reverted: usize,
    pub pair_skipped: i32,
    pub pair_edits: i32,
    pub reverted: bool,
    pub judge_calls: i32,
    pub steps: Vec<CleanupStepStats>,
}

// ADAPTED cpp:36-40,115,282,304,320,339,368,391,408,411,425-426,437-441,
// 475-483,506,514: no work units, budget constants, Ctx::out or DRC runner.
type Pts = Vec<TracePoint>;

// Cleanup.cpp:44-106.
fn point_seg_d(p: Vec2, a: Vec2, b: Vec2) -> f64 {
    padgeom::point_seg_d(p.x as f64, p.y as f64, a.x as f64, a.y as f64, b.x as f64, b.y as f64)
}

fn seg_seg_d(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> f64 {
    padgeom::seg_seg_d(a.x as f64, a.y as f64, b.x as f64, b.y as f64, c.x as f64, c.y as f64, d.x as f64, d.y as f64)
}

fn cross_strict(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> bool {
    Segment2::new(a, b).crosses_strictly(&Segment2::new(c, d), 1e-7f32)
}

fn point_in_poly(poly: &[Vec2], p: Vec2) -> bool {
    let mut inside = false;
    let mut j = poly.len().wrapping_sub(1);
    for i in 0..poly.len() {
        let a = poly[i];
        let c = poly[j];
        if (a.y > p.y) != (c.y > p.y) && p.x < (c.x - a.x) * (p.y - a.y) / (c.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn turn_deg(a: Vec2, b: Vec2, c: Vec2) -> f32 {
    let d1 = b - a;
    let d2 = c - b;
    let l1 = d1.length();
    let l2 = d2.length();
    if l1 < 1e-6f32 || l2 < 1e-6f32 { return 0.0; }
    let cs = std_clamp(d1.dot(d2) / (l1 * l2), -1.0f32, 1.0f32);
    cs.acos() * 57.29577951f32
}

fn anchor(p: &[TracePoint], i: i32) -> bool {
    if i <= 0 || i >= p.len() as i32 - 1 { return true; }
    let q = &p[i as usize];
    q.is_fixed || q.terminal_pad >= 0 || q.attached_via >= 0 || q.branch_trace >= 0 || q.straight_before || p[i as usize + 1].straight_before
}

fn poly_length(p: &[TracePoint]) -> f64 {
    let mut l = 0.0f64;
    for i in 0..p.len().saturating_sub(1) { l += p[i].pos.distance_to(p[i + 1].pos) as f64; }
    l
}

fn positions(p: &[TracePoint]) -> Vec<Vec2> { p.iter().map(|p| p.pos).collect() }

fn plain_node(pos: Vec2, width: f32) -> TracePoint { TracePoint { pos, width, ..TracePoint::default() } }

// Cleanup.cpp:111-414. ADAPTED cpp:113-114: pass the current board to methods,
// rather than storing aliases to the board that Ctx mutates.
#[derive(Default)]
struct Env {
    clr: f32,
    hole_clr: f32,
    edge_clr: f32,
    slack: f32,
    n_l: i32,
    world: Box2,
    cell: f32,
    nx: i32,
    ny: i32,
    segs: Vec<Seg>,
    trace_segs: Vec<Vec<usize>>,
    seg_grid: Vec<Vec<Vec<usize>>>,
    pad_grid: Vec<Vec<usize>>,
    via_grid: Vec<Vec<usize>>,
    edge_grid: Vec<Vec<usize>>,
    edges: Vec<(Vec2, Vec2)>,
    via_dead: Vec<bool>,
    pad_holes: Vec<padgeom::Hole>,
    pad_has_hole: Vec<bool>,
    seg_stamp: Vec<i32>,
    pad_stamp: Vec<i32>,
    via_stamp: Vec<i32>,
    edge_stamp: Vec<i32>,
    stamp: i32,
    fails: [i64; 7],
}

struct Seg {
    a: Vec2,
    c: Vec2,
    hw: f32,
    net: i32,
    trace: usize,
    alive: bool,
    // ADAPTED cpp:129,252: the layer is carried by seg_grid's bucket;
    // the reference never reads its redundant copy inside Seg.
    // ADAPTED cpp:125-130,252,283,326: the foreign trace's clearance row.
    rule: u16,
}

const F_SAME_NET_CROSS: usize = 0;
const F_TRACE: usize = 1;
const F_PAD: usize = 2;
const F_PAD_HOLE: usize = 3;
const F_VIA: usize = 4;
const F_EDGE: usize = 5;
const F_KEEPOUT: usize = 6;

impl Env {
    // ADAPTED cpp:146-159: an owned iterator replaces the callback so grid
    // traversal can mutate stamps/buckets without aliasing Env. Same y/x order.
    fn cells(&self, bb: Box2) -> impl Iterator<Item = usize> + 'static {
        let x0 = std_clamp(((bb.min.x - self.world.min.x) / self.cell).floor() as i32, 0, self.nx - 1);
        let y0 = std_clamp(((bb.min.y - self.world.min.y) / self.cell).floor() as i32, 0, self.ny - 1);
        let x1 = std_clamp(((bb.max.x - self.world.min.x) / self.cell).floor() as i32, 0, self.nx - 1);
        let y1 = std_clamp(((bb.max.y - self.world.min.y) / self.cell).floor() as i32, 0, self.ny - 1);
        let nx = self.nx;
        (y0..=y1).flat_map(move |y| (x0..=x1).map(move |x| (y * nx + x) as usize))
    }

    fn build(&mut self, board: &OBoard, opt: &CleanupOptions) {
        // ADAPTED cpp:166-169: ClearanceTable replaces NetClearances and
        // padRuleReach; its max already covers pad rules. The adapter stores
        // Fab's hole rule directly; there is no DRU net. DesignRules has no
        // projectEdgeClearance: use board_edge_clearance alone.
        self.clr = std_max(board.rules.min_clearance, board.clr.max);
        self.hole_clr = board.rules.hole_clearance;
        self.edge_clr = board.rules.board_edge_clearance;
        self.slack = opt.clearance_slack;
        self.n_l = std_max(1, board.layers.len() as i32);
        self.world = Box2::default();
        for &p in &board.outline { self.world.expand(p); }
        for p in &board.pads { self.world.expand(p.center); }
        for v in &board.vias { self.world.expand(v.pos); }
        for t in &board.traces {
            for p in &t.points {
                if p.pos.x.is_finite() && p.pos.y.is_finite() { self.world.expand(p.pos); }
            }
        }
        if !self.world.is_valid() { self.world = Box2::new(Vec2::new(0.0, 0.0), Vec2::new(1.0, 1.0)); }
        self.world.pad(5.0f32);
        let sz = self.world.size();
        self.cell = std_max(1.0f32, (sz.x * sz.y / 250000.0f32).sqrt());
        self.nx = std_max(1, (sz.x / self.cell).ceil() as i32);
        self.ny = std_max(1, (sz.y / self.cell).ceil() as i32);
        let nc = self.nx as usize * self.ny as usize;
        self.segs.clear();
        self.trace_segs = vec![Vec::new(); board.traces.len()];
        self.seg_grid = vec![vec![Vec::new(); nc]; self.n_l as usize];
        self.pad_grid = vec![Vec::new(); nc];
        self.via_grid = vec![Vec::new(); nc];
        self.edge_grid = vec![Vec::new(); nc];
        self.via_dead = vec![false; board.vias.len()];
        self.pad_holes = vec![padgeom::Hole::default(); board.pads.len()];
        self.pad_has_hole = vec![false; board.pads.len()];
        for (i, p) in board.pads.iter().enumerate() {
            if let Some(h) = padgeom::hole(p) {
                self.pad_holes[i] = h;
                self.pad_has_hole[i] = true;
            }
            let mut bb = Box2::default();
            bb.expand(p.center);
            bb.pad(std_max(p.size.x, p.size.y) * 0.75f32 + 0.01f32);
            for c in self.cells(bb) { self.pad_grid[c].push(i); }
        }
        for (i, v) in board.vias.iter().enumerate() {
            if !v.pos.x.is_finite() || !v.pos.y.is_finite() { continue; }
            let mut bb = Box2::default();
            bb.expand(v.pos);
            bb.pad(v.diameter * 0.5f32 + 0.01f32);
            for c in self.cells(bb) { self.via_grid[c].push(i); }
        }
        self.edges.clear();
        // cpp:216-230's addLoop, outline first, then cut-outs in board order.
        for ol in std::iter::once(&board.outline).chain(&board.cutouts) {
            if ol.len() < 3 { continue; }
            for i in 0..ol.len() {
                let a = ol[i];
                let c = ol[(i + 1) % ol.len()];
                if a.distance_to(c) < 1e-7f32 { continue; }
                let mut bb = Box2::default();
                bb.expand(a);
                bb.expand(c);
                let id = self.edges.len();
                self.edges.push((a, c));
                for cc in self.cells(bb) { self.edge_grid[cc].push(id); }
            }
        }
        for t in 0..board.traces.len() { self.set_trace(board, t); }
        self.pad_stamp = vec![0; board.pads.len()];
        self.via_stamp = vec![0; board.vias.len()];
        self.edge_stamp = vec![0; self.edges.len()];
        self.stamp = 0;
    }

    fn remove_trace(&mut self, t: usize) {
        for &id in &self.trace_segs[t] { self.segs[id].alive = false; }
        self.trace_segs[t].clear();
    }

    fn set_trace(&mut self, b: &OBoard, t: usize) {
        self.remove_trace(t);
        let tr = &b.traces[t];
        if !tr.is_routed || tr.points.len() < 2 { return; }
        let layer = std_clamp(tr.layer, 0, self.n_l - 1) as usize;
        for i in 0..tr.points.len() - 1 {
            let s = Seg { a: tr.points[i].pos, c: tr.points[i + 1].pos, hw: tr.width * 0.5f32, net: tr.net, trace: t, alive: true, rule: tr.rule };
            let mut bb = Box2::default();
            bb.expand(s.a);
            bb.expand(s.c);
            bb.pad(s.hw);
            let id = self.segs.len();
            self.segs.push(s);
            self.trace_segs[t].push(id);
            for c in self.cells(bb) { self.seg_grid[layer][c].push(id); }
        }
        self.seg_stamp.resize(self.segs.len(), 0);
    }

    // ADAPTED cpp:266-283: carry the candidate rule row as well as its net.
    fn foreign_traces_near(&mut self, b: &OBoard, a: Vec2, c: Vec2, hw: f32, net: i32, rule: u16, layer: i32) -> Vec<usize> {
        let mut out = Vec::new();
        let layer = std_clamp(layer, 0, self.n_l - 1);
        let mut bb = Box2::default();
        bb.expand(a);
        bb.expand(c);
        bb.pad(hw + self.clr + self.slack + 0.6f32);
        self.stamp += 1;
        for cidx in self.cells(bb) {
            for &id in &self.seg_grid[layer as usize][cidx] {
                if self.seg_stamp[id] == self.stamp { continue; }
                self.seg_stamp[id] = self.stamp;
                let s = &self.segs[id];
                if !s.alive || (s.net == net && net >= 0) { continue; }
                if seg_seg_d(a, c, s.a, s.c) - hw as f64 - (s.hw as f64) < (b.clr.get(rule, s.rule, layer) + self.slack) as f64 {
                    out.push(s.trace);
                }
            }
        }
        // cpp:286: std::sort ties are identical trace indices; retain its order.
        out.sort_unstable_by(|a, b| a.cmp(b));
        out.dedup();
        out
    }

    // ADAPTED cpp:294,326,341,370: carry the candidate rule row; use each
    // foreign object's row at the checked layer (-1 for a via disc).
    fn seg_ok(&mut self, b: &OBoard, a: Vec2, c: Vec2, hw: f32, net: i32, rule: u16, layer: i32, ignore: &[usize]) -> bool {
        if !a.x.is_finite() || !a.y.is_finite() || !c.x.is_finite() || !c.y.is_finite() { return false; }
        let layer = std_clamp(layer, 0, self.n_l - 1);
        let reach = hw + std_max(std_max(self.clr, self.hole_clr), self.edge_clr) + self.slack + 0.05f32;
        let mut bb = Box2::default();
        bb.expand(a);
        bb.expand(c);
        bb.pad(reach);
        self.stamp += 1;
        for cidx in self.cells(bb) {
            for &id in &self.seg_grid[layer as usize][cidx] {
                if self.seg_stamp[id] == self.stamp { continue; }
                self.seg_stamp[id] = self.stamp;
                let s = &self.segs[id];
                if !s.alive || ignore.contains(&s.trace) { continue; }
                if s.net == net && net >= 0 {
                    if cross_strict(a, c, s.a, s.c) { self.fails[F_SAME_NET_CROSS] += 1; return false; }
                    continue;
                }
                if seg_seg_d(a, c, s.a, s.c) - hw as f64 - (s.hw as f64) < (b.clr.get(rule, s.rule, layer) + self.slack) as f64 {
                    self.fails[F_TRACE] += 1;
                    return false;
                }
            }
        }
        for cidx in self.cells(bb) {
            for &pi in &self.pad_grid[cidx] {
                if self.pad_stamp[pi] == self.stamp { continue; }
                self.pad_stamp[pi] = self.stamp;
                let p = &b.pads[pi];
                if p.net == net && net >= 0 { continue; }
                if drcgeom::pad_on_layer(p, layer, self.n_l)
                    && padgeom::segment_distance_pad(p, a.x as f64, a.y as f64, c.x as f64, c.y as f64) - (hw as f64)
                        < (b.clr.get(rule, p.rule, layer) + self.slack) as f64 {
                    self.fails[F_PAD] += 1;
                    return false;
                }
                if self.pad_has_hole[pi] && self.hole_clr > 0.0f32
                    && padgeom::hole_segment_distance(&self.pad_holes[pi], a.x as f64, a.y as f64, c.x as f64, c.y as f64) - (hw as f64)
                        < (self.hole_clr + self.slack) as f64 {
                    self.fails[F_PAD_HOLE] += 1;
                    return false;
                }
            }
        }
        for cidx in self.cells(bb) {
            for &vi in &self.via_grid[cidx] {
                if self.via_stamp[vi] == self.stamp { continue; }
                self.via_stamp[vi] = self.stamp;
                if self.via_dead[vi] { continue; }
                let v = &b.vias[vi];
                if v.net == net && net >= 0 { continue; }
                // ADAPTED cpp:367: DrcOptions::viaThrough defaults to false.
                if !drcgeom::via_on_layer(v, layer) { continue; }
                let d = point_seg_d(v.pos, a, c) - hw as f64;
                if d - v.diameter as f64 * 0.5 < (b.clr.get(rule, v.rule, -1) + self.slack) as f64
                    || (self.hole_clr > 0.0f32 && d - v.drill as f64 * 0.5 < (self.hole_clr + self.slack) as f64) {
                    self.fails[F_VIA] += 1;
                    return false;
                }
            }
        }
        if !self.edges.is_empty() {
            let mut in_hole = false;
            for h in &b.cutouts { in_hole = in_hole || (h.len() >= 3 && (point_in_poly(h, a) || point_in_poly(h, c))); }
            if !point_in_poly(&b.outline, a) || !point_in_poly(&b.outline, c) || in_hole {
                self.fails[F_EDGE] += 1;
                return false;
            }
            for cidx in self.cells(bb) {
                for &ei in &self.edge_grid[cidx] {
                    if self.edge_stamp[ei] == self.stamp { continue; }
                    self.edge_stamp[ei] = self.stamp;
                    if seg_seg_d(a, c, self.edges[ei].0, self.edges[ei].1) - (hw as f64) < (self.edge_clr + self.slack) as f64 {
                        self.fails[F_EDGE] += 1;
                        return false;
                    }
                }
            }
        }
        for k in &b.keepouts {
            if !k.no_tracks || k.poly.len() < 3 { continue; }
            if !drcgeom::keepout_on_layer(b, k, layer) { continue; }
            let margin = drcgeom::keepout_margin(k);
            let mut kb = Box2::default();
            for &p in &k.poly { kb.expand(p); }
            kb.pad(margin);
            if !kb.overlaps(&bb) { continue; }
            if drcgeom::segment_keepout_distance(k, a, c) - hw < margin + self.slack {
                self.fails[F_KEEPOUT] += 1;
                return false;
            }
        }
        true
    }
}

// Cleanup.cpp:419-541. ADAPTED cpp:435: stats are owned by the context;
// board/options remain borrowed. Step-local point copies bound Rust borrows
// while the local/pair guards update counters (cpp:560,925,1012,1119,1191,1298).
/// ADAPTED (ours, with the net fixpoint): the edits a step made on one net. A step adds them to
/// the stats only for the nets that stand, so a counter never counts an edit that was put back.
#[derive(Clone, Copy, Debug, Default)]
struct EditCounts {
    applied: i32,
    pair_edits: i32,
    loops_removed: i32,
    elbows: i32,
    straight_shortcuts: i32,
    chamfers: i32,
    vias_removed: i32,
    hops_rerouted: i32,
}

struct Ctx<'a> {
    b: &'a mut OBoard,
    o: &'a CleanupOptions,
    st: CleanupStats,
    env: Env,
    editable: Vec<bool>,
    dead: Vec<bool>,
    plane_of_net: Vec<i32>,
    fanout_via: Vec<bool>,
    self_cross: i64,
    pg: PairGuard,
    /// This step's edits per net (BTreeMap: summed in ascending net order).
    edits: BTreeMap<i32, EditCounts>,
}

impl Ctx<'_> {
    fn edit(&mut self, net: i32) -> &mut EditCounts {
        self.edits.entry(net).or_default()
    }

    fn rebuild(&mut self) {
        self.env.build(self.b, self.o);
        self.env.fails.fill(0);
        self.self_cross = 0;
        self.dead = vec![false; self.b.traces.len()];
        self.editable = vec![true; self.b.traces.len()];
        // ADAPTED cpp:450: std::map -> BTreeMap. idx is only looked up,
        // never iterated; filling follows board trace order.
        let mut idx = BTreeMap::new();
        for (t, tr) in self.b.traces.iter().enumerate() { idx.insert(tr.id, t); }
        for (t, tr) in self.b.traces.iter().enumerate() {
            if tr.locked || (tr.imported && !self.o.edit_imported) || !tr.is_routed || tr.points.len() < 2 { self.editable[t] = false; }
            for p in &tr.points {
                if p.branch_trace >= 0 || !p.pos.x.is_finite() || !p.pos.y.is_finite() {
                    self.editable[t] = false;
                    if p.branch_trace >= 0 {
                        if let Some(&host) = idx.get(&p.branch_trace) { self.editable[host] = false; }
                    }
                }
            }
        }
        if self.plane_of_net.len() != self.b.nets.len() {
            self.plane_of_net = vec![-1; self.b.nets.len()];
            for n in 0..self.b.nets.len() { self.plane_of_net[n] = drcgeom::plane_layer_of_net(self.b, n as i32); }
            self.fanout_via = fanout_via_mask(self.b);
        }
    }

    fn path_ok(&mut self, t: usize, p: &[Vec2], lo: i32, hi: i32, layer: i32, ignore: &[usize]) -> bool {
        let tr = &self.b.traces[t];
        let hw = tr.width * 0.5f32;
        let n = p.len() as i32;
        let lo = std_max(lo, 0);
        let hi = std_min(hi, n - 1);
        for k in lo..hi {
            if !self.env.seg_ok(self.b, p[k as usize], p[k as usize + 1], hw, tr.net, tr.rule, layer, ignore) { return false; }
        }
        for k in lo..hi {
            for m in 0..n - 1 {
                if (m - k).abs() <= 1 { continue; }
                if cross_strict(p[k as usize], p[k as usize + 1], p[m as usize], p[m as usize + 1]) {
                    self.self_cross += 1;
                    return false;
                }
            }
        }
        true
    }

    fn pair_ok(&mut self, net: i32, o: &[CouplingPoly], n: &[CouplingPoly], s: &mut CleanupStepStats) -> bool {
        if !self.pg.member(net) { return true; }
        if self.pg.keeps(self.b, net, o, n, None) { return true; }
        s.pair_skipped += 1;
        self.st.pair_skipped += 1;
        false
    }

    fn pair_span_ok(&mut self, t: usize, q: &[Vec2], lo: i32, hi: i32, qlo: i32, qhi: i32, s: &mut CleanupStepStats) -> bool {
        let tr = &self.b.traces[t];
        if !self.pg.member(tr.net) { return true; }
        self.pair_ok(tr.net,
            &[PairGuard::poly_trace_points(&tr.points, tr.layer, tr.width, lo - 1, hi + 1)],
            &[PairGuard::poly_points(q, tr.layer, tr.width, qlo - 1, qhi + 1)], s)
    }

    fn pair_applied(&mut self, net: i32) {
        if !self.pg.member(net) { return; }
        self.edit(net).pair_edits += 1;
    }

    fn set_points(&mut self, t: usize, pts: Pts) {
        self.b.traces[t].points = pts;
        self.env.set_trace(self.b, t);
    }
}

// Cleanup.cpp:543-548.
fn skeleton_segments(b: &OBoard) -> i32 {
    let mut s = 0;
    for t in &b.traces { s += std_max(0, t.points.len() as i32 - 1); }
    s
}

// Cleanup.cpp:553-605.
fn step_decimate(c: &mut Ctx<'_>, tol: f32, guarded: bool, s: &mut CleanupStepStats) {
    // ADAPTED cpp:559,576: no budget. This visits a finite trace list; a
    // advances at least one node, and each candidate/node scan is finite.
    for t in 0..c.b.traces.len() {
        if !c.editable[t] || c.dead[t] { continue; }
        let p = c.b.traces[t].points.clone();
        let n = p.len() as i32;
        if n < 3 { continue; }
        let layer = c.b.traces[t].layer;
        let mut out_pts = vec![p[0].clone()];
        let mut a = 0;
        while a < n - 1 {
            let mut kmax = a + 1;
            for k in a + 2..n {
                if anchor(&p, k - 1) { break; }
                let mut within = true;
                for m in a + 1..k {
                    if !within { break; }
                    if point_seg_d(p[m as usize].pos, p[a as usize].pos, p[k as usize].pos) > tol as f64 { within = false; }
                }
                if !within { break; }
                kmax = k;
            }
            let mut best = a + 1;
            for k in (a + 2..=kmax).rev() {
                if !guarded { best = k; break; }
                let tr = &c.b.traces[t];
                if c.env.seg_ok(c.b, p[a as usize].pos, p[k as usize].pos, tr.width * 0.5f32, tr.net, tr.rule, layer, &[t]) {
                    best = k;
                    break;
                }
                s.rejected += 1;
            }
            out_pts.push(p[best as usize].clone());
            a = best;
        }
        if (out_pts.len() as i32) < n {
            let tr = &c.b.traces[t];
            let net = tr.net;
            if guarded && c.pg.member(net) && !c.pair_ok(net,
                &[PairGuard::poly_trace(tr)], &[PairGuard::poly_trace_points(&out_pts, tr.layer, tr.width, 0, -1)], s) {
                continue;
            }
            c.edit(net).applied += n - out_pts.len() as i32;
            if guarded { c.pair_applied(net); }
            c.set_points(t, out_pts);
        }
    }
}

// Cleanup.cpp:610-614.
#[derive(Clone, Copy)]
struct End { t: usize, back: bool }

// ADAPTED cpp:620-671: the stepVias lambdas are methods/functions so their
// borrows end before each edit; the tests and their order are unchanged.
impl Ctx<'_> {
    fn via_editable(&self, v: i32) -> bool {
        if v < 0 || v >= self.b.vias.len() as i32 || self.env.via_dead[v as usize] { return false; }
        let via = &self.b.vias[v as usize];
        if via.locked || via.is_fixed || (via.imported && !self.o.edit_imported) { return false; }
        if via.net >= 0 && via.net < self.plane_of_net.len() as i32 && self.plane_of_net[via.net as usize] >= 0 { return false; }
        if v < self.fanout_via.len() as i32 && self.fanout_via[v as usize] { return false; }
        true
    }

    fn touched_by_others(&self, v: usize, ex: &[usize]) -> bool {
        let via = &self.b.vias[v];
        let r = via.diameter * 0.5f32;
        for (t, tr) in self.b.traces.iter().enumerate() {
            if self.dead[t] || tr.net != via.net || tr.points.is_empty() { continue; }
            if ex.contains(&t) { continue; }
            // ADAPTED cpp:639: viaThrough is the DrcOptions default, false.
            if !drcgeom::via_on_layer(via, tr.layer) { continue; }
            for i in 0..tr.points.len() - 1 {
                if point_seg_d(via.pos, tr.points[i].pos, tr.points[i + 1].pos) <= (r + tr.width * 0.5f32 + 0.01f32) as f64 { return true; }
            }
            if tr.points.len() == 1 && tr.points[0].pos.distance_to(via.pos) <= r + tr.width * 0.5f32 + 0.01f32 { return true; }
        }
        for p in &self.b.pads {
            if p.net == via.net && padgeom::point_distance_pad(p, via.pos.x as f64, via.pos.y as f64) <= r as f64 + 0.01 { return true; }
        }
        for (u, other) in self.b.vias.iter().enumerate() {
            if u != v && !self.env.via_dead[u] && other.net == via.net
                && other.pos.distance_to(via.pos) <= r + other.diameter * 0.5f32 + 0.01f32 { return true; }
        }
        false
    }

    fn oriented(&self, t: usize, want_via_at_back: bool, via_is_back: bool) -> Pts {
        let mut p = self.b.traces[t].points.clone();
        // cpp:655: reuse TraceEdit.cpp:446-454's settled reverse_points port.
        if want_via_at_back != via_is_back { reverse_points(&mut p); }
        p
    }

    fn same_width(&self, x: usize, y: usize) -> bool { (self.b.traces[x].width - self.b.traces[y].width).abs() < 1e-4f32 }

    fn far_end_on_layer(&self, e: &TracePoint, l: i32) -> bool {
        if e.terminal_pad >= 0 && e.terminal_pad < self.b.pads.len() as i32 {
            return drcgeom::pad_on_layer(&self.b.pads[e.terminal_pad as usize], l, std_max(1, self.b.layers.len() as i32));
        }
        if e.attached_via >= 0 && e.attached_via < self.b.vias.len() as i32 {
            // ADAPTED cpp:662: viaThrough is false; retain the span check.
            return !self.env.via_dead[e.attached_via as usize] && drcgeom::via_on_layer(&self.b.vias[e.attached_via as usize], l);
        }
        false
    }
}

fn joint_ok(p: &[TracePoint], i: i32) -> bool {
    if i <= 0 || i >= p.len() as i32 - 1 { return true; }
    let i = i as usize;
    let stub_side = p[i].straight_before || p[i + 1].straight_before;
    !stub_side || turn_deg(p[i - 1].pos, p[i].pos, p[i + 1].pos) <= 90.0f32 + 1e-3f32
}

// ADAPTED cpp:755-811: captured values of build/tryLayer live in ViaJoin;
// try_layer borrows the live context for each attempt instead of a closure
// retaining an exclusive borrow throughout the remaining candidate search.
struct ViaJoin {
    ea: End,
    ex: End,
    ec: End,
    v: usize,
    w: usize,
    a: Pts,
    x: Pts,
    cc: Pts,
    la: i32,
    lx: i32,
    width: f32,
    len_before: f64,
}

impl ViaJoin {
    fn build(&self, mid: &[Vec2]) -> Pts {
        let mut m = self.a.clone();
        m.last_mut().unwrap().attached_via = -1;
        for k in 1..mid.len().saturating_sub(1) { m.push(plain_node(mid[k], self.width)); }
        let mut jn = self.cc[0].clone();
        jn.attached_via = -1;
        m.push(jn);
        m.extend_from_slice(&self.cc[1..]);
        m
    }

    fn try_layer(&self, c: &mut Ctx<'_>, l: i32, mids: &[Vec<Vec2>], move_outer: bool, s: &mut CleanupStepStats) -> bool {
        if drcgeom::is_plane_layer(c.b, l) { return false; }
        for mid in mids {
            let m = self.build(mid);
            let p = positions(&m);
            let ja = self.a.len() as i32 - 1;
            let jc = ja + mid.len() as i32 - 1;
            if !joint_ok(&m, ja) || !joint_ok(&m, jc) { s.rejected += 1; continue; }
            let lo = if move_outer { 0 } else { ja };
            let hi = if move_outer { p.len() as i32 - 1 } else { jc };
            if !c.path_ok(self.ea.t, &p, lo, hi, l, &[self.ea.t, self.ex.t, self.ec.t]) { s.rejected += 1; continue; }
            let net = c.b.traces[self.ea.t].net;
            if c.pg.member(net) && !c.pair_ok(net, &[
                PairGuard::poly_trace_points(&self.a, self.la, c.b.traces[self.ea.t].width, 0, -1),
                PairGuard::poly_trace_points(&self.x, self.lx, c.b.traces[self.ex.t].width, 0, -1),
                PairGuard::poly_trace_points(&self.cc, self.la, c.b.traces[self.ec.t].width, 0, -1),
            ], &[PairGuard::poly_points(&p, l, self.width, 0, -1)], s) { continue; }
            c.pair_applied(net);
            // ADAPTED cpp:793-794: OBoard stores only the copper stack index,
            // so no duplicate named layer mask needs updating.
            c.b.traces[self.ea.t].layer = l;
            c.set_points(self.ea.t, m);
            c.b.traces[self.ea.t].length = poly_length(&c.b.traces[self.ea.t].points) as f32;
            for dt in [self.ex.t, self.ec.t] {
                c.env.remove_trace(dt);
                c.b.traces[dt].points.clear();
                c.dead[dt] = true;
            }
            c.env.via_dead[self.v] = true;
            c.env.via_dead[self.w] = true;
            c.edit(net).applied += 1;
            s.length_gain += self.len_before - c.b.traces[self.ea.t].length as f64;
            c.edit(net).vias_removed += 2;
            return true;
        }
        false
    }
}

// Cleanup.cpp:616-911.
fn step_vias(c: &mut Ctx<'_>, s: &mut CleanupStepStats) {
    // ADAPTED cpp:678,650,692: no budget. via_rounds and the finite via list
    // bound these sweeps; a successful merge also marks vias/traces dead.
    for _round in 0..c.o.via_rounds {
        let mut applied_round = 0;
        for v in 0..c.b.vias.len() {
            if !c.via_editable(v as i32) { continue; }
            let mut ends = Vec::new();
            let mut foreign_attach = false;
            for (t, tr) in c.b.traces.iter().enumerate() {
                if c.dead[t] || tr.points.is_empty() { continue; }
                let p = &tr.points;
                if p[0].attached_via == v as i32 { ends.push(End { t, back: false }); }
                if p.len() > 1 && p.last().unwrap().attached_via == v as i32 { ends.push(End { t, back: true }); }
                for k in 1..p.len().saturating_sub(1) { foreign_attach |= p[k].attached_via == v as i32; }
            }
            if foreign_attach || ends.len() != 2 || ends[0].t == ends[1].t { continue; }
            if !c.editable[ends[0].t] || !c.editable[ends[1].t] { continue; }
            if !c.same_width(ends[0].t, ends[1].t) { continue; }
            if c.touched_by_others(v, &[ends[0].t, ends[1].t]) { continue; }
            if c.b.traces[ends[0].t].layer == c.b.traces[ends[1].t].layer {
                let a = c.oriented(ends[0].t, true, ends[0].back);
                let b = c.oriented(ends[1].t, false, ends[1].back);
                if a.last().unwrap().pos.distance_to(b[0].pos) > 0.01f32 { continue; }
                let mut m = a.clone();
                let last = m.last_mut().unwrap();
                last.attached_via = -1;
                last.is_fixed = last.is_fixed && last.straight_before;
                m.extend_from_slice(&b[1..]);
                let joint = a.len() as i32 - 1;
                if !joint_ok(&m, joint) { s.rejected += 1; continue; }
                let before = poly_length(&a) + poly_length(&b);
                c.set_points(ends[0].t, m);
                c.b.traces[ends[0].t].length = poly_length(&c.b.traces[ends[0].t].points) as f32;
                c.env.remove_trace(ends[1].t);
                c.b.traces[ends[1].t].points.clear();
                c.dead[ends[1].t] = true;
                c.env.via_dead[v] = true;
                let net = c.b.traces[ends[0].t].net;
                c.edit(net).applied += 1;
                s.length_gain += before - poly_length(&c.b.traces[ends[0].t].points);
                c.edit(net).vias_removed += 1;
                applied_round += 1;
                continue;
            }
            let mut done = false;
            for pick in 0..2 {
                if done { break; }
                let ea = ends[1 - pick];
                let ex = ends[pick];
                let xp = &c.b.traces[ex.t].points;
                let x_far = if ex.back { &xp[0] } else { xp.last().unwrap() };
                let w = x_far.attached_via;
                if w == v as i32 || w < v as i32 || !c.via_editable(w) { continue; }
                let w = w as usize;
                let mut ends_w = Vec::new();
                let mut foreign_w = false;
                for (t, tr) in c.b.traces.iter().enumerate() {
                    if c.dead[t] || tr.points.is_empty() { continue; }
                    let p = &tr.points;
                    if p[0].attached_via == w as i32 { ends_w.push(End { t, back: false }); }
                    if p.len() > 1 && p.last().unwrap().attached_via == w as i32 { ends_w.push(End { t, back: true }); }
                    for k in 1..p.len().saturating_sub(1) { foreign_w |= p[k].attached_via == w as i32; }
                }
                if foreign_w || ends_w.len() != 2 { continue; }
                let ec = if ends_w[0].t == ex.t { ends_w[1] } else { ends_w[0] };
                if (ends_w[0].t == ex.t) == (ends_w[1].t == ex.t) { continue; }
                if ec.t == ea.t || ec.t == ex.t || !c.editable[ec.t] { continue; }
                let la = c.b.traces[ea.t].layer;
                let lx = c.b.traces[ex.t].layer;
                if c.b.traces[ec.t].layer != la || lx == la { continue; }
                if !c.same_width(ea.t, ec.t) || !c.same_width(ea.t, ex.t) { continue; }
                let trio = [ea.t, ex.t, ec.t];
                if c.touched_by_others(w, &trio) || c.touched_by_others(v, &trio) { continue; }
                let a = c.oriented(ea.t, true, ea.back);
                let x = c.oriented(ex.t, false, ex.back);
                let cc = c.oriented(ec.t, false, ec.back);
                let j1 = a.last().unwrap().pos;
                let j2 = cc[0].pos;
                if j1.distance_to(x[0].pos) > 0.01f32 || j2.distance_to(x.last().unwrap().pos) > 0.01f32 { continue; }
                let width = c.b.traces[ea.t].width;
                let len_before = poly_length(&a) + poly_length(&x) + poly_length(&cc);
                let join = ViaJoin { ea, ex, ec, v, w, a, x, cc, la, lx, width, len_before };
                let mut mids = Vec::new();
                {
                    let mut own = positions(&join.x);
                    own[0] = j1;
                    *own.last_mut().unwrap() = j2;
                    mids.push(own);
                    let mut alt = vec![vec![j1, j2]];
                    let d = j2 - j1;
                    let adx = d.x.abs();
                    let ady = d.y.abs();
                    let m = std_min(adx, ady);
                    let sx = if d.x < 0.0 { -1.0f32 } else { 1.0f32 };
                    let sy = if d.y < 0.0 { -1.0f32 } else { 1.0f32 };
                    if m > 1e-3f32 && (adx - ady).abs() > 1e-3f32 {
                        alt.push(vec![j1, j1 + Vec2::new(sx * m, sy * m), j2]);
                        alt.push(vec![j1, j2 - Vec2::new(sx * m, sy * m), j2]);
                    }
                    if adx > 1e-3f32 && ady > 1e-3f32 {
                        alt.push(vec![j1, Vec2::new(j2.x, j1.y), j2]);
                        alt.push(vec![j1, Vec2::new(j1.x, j2.y), j2]);
                    }
                    for g in alt {
                        let mut l = 0.0f64;
                        for k in 0..g.len() - 1 { l += g[k].distance_to(g[k + 1]) as f64; }
                        if l < poly_length(&join.x) - 1e-4 { mids.push(g); }
                    }
                }
                let mut own = positions(&join.x);
                own[0] = j1;
                *own.last_mut().unwrap() = j2;
                let outer_first = poly_length(&join.a) + poly_length(&join.cc) < poly_length(&join.x);
                let outer_possible = c.far_end_on_layer(&join.a[0], lx) && c.far_end_on_layer(join.cc.last().unwrap(), lx);
                if outer_first && outer_possible { done = join.try_layer(c, lx, std::slice::from_ref(&own), true, s); }
                if !done { done = join.try_layer(c, la, &mids, false, s); }
                if !done && !outer_first && outer_possible { done = join.try_layer(c, lx, std::slice::from_ref(&own), true, s); }
                if !done && c.o.hop_reroute && poly_length(&join.x) < c.o.hop_max_length as f64 && !drcgeom::is_plane_layer(c.b, la) {
                    let hw = width * 0.5f32;
                    let tr = &c.b.traces[ea.t];
                    let blockers = c.env.foreign_traces_near(c.b, j1, j2, hw, tr.net, tr.rule, la);
                    c.st.hop_candidates += 1;
                    if blockers.len() == 1 {
                        c.st.hop_single_crossing += 1;
                        let f = &c.b.traces[blockers[0]];
                        let mut around = Vec::new();
                        for e in [&f.points[0], f.points.last().unwrap()] {
                            let mut r = f.width * 0.5f32;
                            if e.terminal_pad >= 0 && e.terminal_pad < c.b.pads.len() as i32 {
                                let sz = c.b.pads[e.terminal_pad as usize].size;
                                r = 0.5f32 * (sz.x * sz.x + sz.y * sz.y).sqrt();
                            } else if e.attached_via >= 0 && e.attached_via < c.b.vias.len() as i32 {
                                r = c.b.vias[e.attached_via as usize].diameter * 0.5f32;
                            }
                            for extra in [0.05f32, 0.3f32] {
                                // ADAPTED cpp:873: candidate/foreign trace rule rows on La.
                                let r = (r + c.b.clr.get(c.b.traces[ea.t].rule, f.rule, la) + hw + c.o.clearance_slack + extra) / (K_PI / 8.0f32).cos();
                                let mut w8 = [Vec2::default(); 8];
                                for k in 0..8 {
                                    let ang = K_PI / 8.0f32 + k as f32 * K_PI / 4.0f32;
                                    w8[k] = e.pos + Vec2::new(ang.cos(), ang.sin()) * r;
                                }
                                for k in 0..8 {
                                    around.push(vec![j1, w8[k], j2]);
                                    around.push(vec![j1, w8[k], w8[(k + 1) % 8], j2]);
                                    around.push(vec![j1, w8[(k + 1) % 8], w8[k], j2]);
                                    around.push(vec![j1, w8[k], w8[(k + 1) % 8], w8[(k + 2) % 8], j2]);
                                    around.push(vec![j1, w8[(k + 2) % 8], w8[(k + 1) % 8], w8[k], j2]);
                                }
                            }
                        }
                        let plen = |g: &[Vec2]| {
                            let mut l = 0.0f64;
                            for k in 0..g.len().saturating_sub(1) { l += g[k].distance_to(g[k + 1]) as f64; }
                            l
                        };
                        let mut keep = Vec::new();
                        for g in around {
                            if plen(&g) <= poly_length(&join.x) + c.o.hop_max_extra as f64 { keep.push(g); }
                        }
                        // cpp:898 explicitly uses stable_sort: preserve equal-length order.
                        keep.sort_by(|x, y| plen(x).partial_cmp(&plen(y)).unwrap_or(Ordering::Equal));
                        if !keep.is_empty() && join.try_layer(c, la, &keep, false, s) {
                            done = true;
                            let net = c.b.traces[ea.t].net;
                            c.edit(net).hops_rerouted += 1;
                        }
                    }
                }
                if done { applied_round += 1; }
            }
        }
        if applied_round == 0 { break; }
    }
}

// Cleanup.cpp:916-977.
fn step_unloop(c: &mut Ctx<'_>, s: &mut CleanupStepStats) {
    // ADAPTED cpp:922,929: no budget. The trace traversal is finite; the
    // reference's 32 sweeps remain, each accepted cut also removes a node.
    for t in 0..c.b.traces.len() {
        if !c.editable[t] || c.dead[t] { continue; }
        for _iter in 0..32 {
            let p = c.b.traces[t].points.clone();
            let n = p.len() as i32;
            if n < 4 { break; }
            let mut changed = false;
            for i in 0..n - 1 {
                if changed { break; }
                for j in i + 2..n - 1 {
                    if changed { break; }
                    let iu = i as usize;
                    let ju = j as usize;
                    if !cross_strict(p[iu].pos, p[iu + 1].pos, p[ju].pos, p[ju + 1].pos) { continue; }
                    let Some(x) = Segment2::new(p[iu].pos, p[iu + 1].pos).intersects(&Segment2::new(p[ju].pos, p[ju + 1].pos)) else { continue; };
                    let mut free = true;
                    for k in i + 1..=j {
                        if !free { break; }
                        free = !anchor(&p, k);
                    }
                    if !free { continue; }
                    let mut m = p[..iu + 1].to_vec();
                    m.push(plain_node(x, c.b.traces[t].width));
                    m.extend_from_slice(&p[ju + 1..]);
                    let q = positions(&m);
                    if !c.path_ok(t, &q, i, i + 2, c.b.traces[t].layer, &[t]) { s.rejected += 1; continue; }
                    if !c.pair_span_ok(t, &q, i, j + 1, i, i + 2, s) { continue; }
                    c.pair_applied(c.b.traces[t].net);
                    let before = poly_length(&p);
                    c.set_points(t, m);
                    s.length_gain += before - poly_length(&c.b.traces[t].points);
                    let net = c.b.traces[t].net;
                    c.edit(net).applied += 1;
                    c.edit(net).loops_removed += 1;
                    changed = true;
                }
            }
            if !changed {
                for k in 1..n - 1 {
                    if changed { break; }
                    let ku = k as usize;
                    if anchor(&p, k) { continue; }
                    if turn_deg(p[ku - 1].pos, p[ku].pos, p[ku + 1].pos) < c.o.spike_turn_deg { continue; }
                    let mut m = p.clone();
                    m.remove(ku);
                    let q = positions(&m);
                    if !c.path_ok(t, &q, k - 1, k, c.b.traces[t].layer, &[t]) { s.rejected += 1; continue; }
                    if !c.pair_span_ok(t, &q, k - 1, k + 1, k - 1, k, s) { continue; }
                    c.pair_applied(c.b.traces[t].net);
                    let before = poly_length(&p);
                    c.set_points(t, m);
                    s.length_gain += before - poly_length(&c.b.traces[t].points);
                    let net = c.b.traces[t].net;
                    c.edit(net).applied += 1;
                    c.edit(net).loops_removed += 1;
                    changed = true;
                }
            }
            if !changed { break; }
        }
    }
}

// Cleanup.cpp:982-1008.
struct Shortcut {
    gain: f64,
    t: usize,
    i: i32,
    j: i32,
    mid: Vec<Vec2>,
}

fn end_turns_ok(c: &Ctx<'_>, p: &[TracePoint], i: i32, j: i32, first_next: Vec2, last_prev: Vec2) -> bool {
    let n = p.len() as i32;
    if i > 0 {
        let i = i as usize;
        let old = turn_deg(p[i - 1].pos, p[i].pos, p[i + 1].pos);
        let mut lim = std_max(c.o.max_turn_deg, old);
        if p[i].straight_before { lim = std_min(lim, std_max(90.0f32, old)); }
        if turn_deg(p[i - 1].pos, p[i].pos, first_next) > lim + 1e-3f32 { return false; }
    }
    if j < n - 1 {
        let j = j as usize;
        let old = turn_deg(p[j - 1].pos, p[j].pos, p[j + 1].pos);
        let mut lim = std_max(c.o.max_turn_deg, old);
        if p[j + 1].straight_before { lim = std_min(lim, std_max(90.0f32, old)); }
        if turn_deg(last_prev, p[j].pos, p[j + 1].pos) > lim + 1e-3f32 { return false; }
    }
    true
}

// Cleanup.cpp:1010-1096. ADAPTED cpp:1010,1092-1095: Option carries the
// successful output instead of a bool plus an output reference.
fn best_shortcut(c: &mut Ctx<'_>, t: usize, s: &mut CleanupStepStats) -> Option<Shortcut> {
    let p = c.b.traces[t].points.clone();
    let n = p.len() as i32;
    if n < 3 { return None; }
    let mut cum = vec![0.0f64; n as usize];
    for k in 1..n as usize { cum[k] = cum[k - 1] + p[k - 1].pos.distance_to(p[k].pos) as f64; }
    struct Cand { gain: f64, i: i32, j: i32, mid: Vec<Vec2> }
    let mut cands = Vec::new();
    // ADAPTED cpp:1024-1037,1047,1066: avoidPadGaps/padGaps/newGap are
    // omitted; PadGap.h is not cleared for this port.
    for i in 0..n - 2 {
        for j in i + 2..n {
            if j - i > c.o.detour_window { break; }
            if anchor(&p, j - 1) { break; }
            let old_len = cum[j as usize] - cum[i as usize];
            let a = p[i as usize].pos;
            let cc = p[j as usize].pos;
            let straight = a.distance_to(cc) as f64;
            if old_len - straight <= c.o.detour_min_gain as f64 { continue; }
            cands.push(Cand { gain: old_len - straight, i, j, mid: Vec::new() });
            if !c.o.detour_elbows { continue; }
            let d = cc - a;
            let adx = d.x.abs();
            let ady = d.y.abs();
            let m = std_min(adx, ady);
            let sx = if d.x < 0.0 { -1.0f32 } else { 1.0f32 };
            let sy = if d.y < 0.0 { -1.0f32 } else { 1.0f32 };
            let mut corners = Vec::new();
            if m > 1e-3f32 && (adx - ady).abs() > 1e-3f32 {
                corners.push(a + Vec2::new(sx * m, sy * m));
                corners.push(cc - Vec2::new(sx * m, sy * m));
            }
            if adx > 1e-3f32 && ady > 1e-3f32 {
                corners.push(Vec2::new(cc.x, a.y));
                corners.push(Vec2::new(a.x, cc.y));
            }
            for k in corners {
                // cpp:1065: add in float, then promote the sum to double.
                let l = (a.distance_to(k) + k.distance_to(cc)) as f64;
                if old_len - l > c.o.detour_min_gain as f64 { cands.push(Cand { gain: old_len - l, i, j, mid: vec![k] }); }
            }
        }
    }
    // cpp:1070 explicitly uses stable_sort: preserve candidate order on gain ties.
    cands.sort_by(|x, y| y.gain.partial_cmp(&x.gain).unwrap_or(Ordering::Equal));
    let layer = c.b.traces[t].layer;
    // ADAPTED cpp:1069,1084,1089: no budget. There are finitely many node
    // windows and at most five shapes per window; each candidate is tried once.
    for cd in cands {
        let first_next = cd.mid.first().copied().unwrap_or(p[cd.j as usize].pos);
        let last_prev = cd.mid.last().copied().unwrap_or(p[cd.i as usize].pos);
        if !end_turns_ok(c, &p, cd.i, cd.j, first_next, last_prev) { continue; }
        let mut q = positions(&p[..cd.i as usize + 1]);
        q.extend_from_slice(&cd.mid);
        q.extend(p[cd.j as usize..].iter().map(|p| p.pos));
        if !c.path_ok(t, &q, cd.i, cd.i + 1 + cd.mid.len() as i32, layer, &[t]) { s.rejected += 1; continue; }
        if !c.pair_span_ok(t, &q, cd.i, cd.j, cd.i, cd.i + 1 + cd.mid.len() as i32, s) { continue; }
        return Some(Shortcut { gain: cd.gain, t, i: cd.i, j: cd.j, mid: cd.mid });
    }
    None
}

// Cleanup.cpp:1098-1166.
fn step_detours(c: &mut Ctx<'_>, s: &mut CleanupStepStats) {
    let mut dirty = vec![true; c.b.traces.len()];
    // ADAPTED cpp:1109,1163-1164: no budget. detour_rounds bounds the
    // rounds; each scans the finite trace list, with one shortcut per trace.
    for _round in 0..c.o.detour_rounds {
        let mut cands = Vec::new();
        for t in 0..c.b.traces.len() {
            if !dirty[t] || !c.editable[t] || c.dead[t] { continue; }
            if let Some(sc) = best_shortcut(c, t, s) { cands.push(sc); }
        }
        // cpp:1113 explicitly uses stable_sort, retaining trace order on ties.
        cands.sort_by(|x, y| y.gain.partial_cmp(&x.gain).unwrap_or(Ordering::Equal));
        let mut changed = Vec::new();
        let mut touched = vec![false; c.b.traces.len()];
        for sc in cands {
            let t = sc.t;
            let p = c.b.traces[t].points.clone();
            let mut q = positions(&p[..sc.i as usize + 1]);
            q.extend_from_slice(&sc.mid);
            q.extend(p[sc.j as usize..].iter().map(|p| p.pos));
            if !c.path_ok(t, &q, sc.i, sc.i + 1 + sc.mid.len() as i32, c.b.traces[t].layer, &[t]) {
                s.rejected += 1;
                touched[t] = true;
                continue;
            }
            if !c.pair_span_ok(t, &q, sc.i, sc.j, sc.i, sc.i + 1 + sc.mid.len() as i32, s) {
                touched[t] = true;
                continue;
            }
            c.pair_applied(c.b.traces[t].net);
            let mut bb = Box2::default();
            for k in sc.i..=sc.j { bb.expand(p[k as usize].pos); }
            let mut m = p[..sc.i as usize + 1].to_vec();
            for &mid in &sc.mid { m.push(plain_node(mid, c.b.traces[t].width)); }
            m.extend_from_slice(&p[sc.j as usize..]);
            let before = poly_length(&p);
            c.set_points(t, m);
            s.length_gain += before - poly_length(&c.b.traces[t].points);
            let net = c.b.traces[t].net;
            c.edit(net).applied += 1;
            if sc.mid.is_empty() { c.edit(net).straight_shortcuts += 1; } else { c.edit(net).elbows += 1; }
            bb.pad(1.0f32 + c.b.traces[t].width);
            changed.push(bb);
            touched[t] = true;
        }
        if changed.is_empty() { break; }
        dirty.fill(false);
        for t in 0..c.b.traces.len() {
            if touched[t] { dirty[t] = true; continue; }
            let mut tb = Box2::default();
            for p in &c.b.traces[t].points { tb.expand(p.pos); }
            for cb in &changed {
                if tb.is_valid() && tb.overlaps(cb) { dirty[t] = true; break; }
            }
        }
    }
}

// Cleanup.cpp:1177-1248.
fn step_tighten(c: &mut Ctx<'_>, s: &mut CleanupStepStats) {
    let step = std_max(0.02f32, c.o.tighten_step);
    let min_cut = 0.01f32;
    // ADAPTED cpp:1188,1208: no budget. tighten_rounds and the trace list
    // are finite. Each node edit advances past both new nodes, so a round
    // visits each original corner once. Growth stops at s_max or a refusal;
    // the reference's five bisections remain.
    for _round in 0..c.o.tighten_rounds {
        let mut cuts = 0;
        for t in 0..c.b.traces.len() {
            if !c.editable[t] || c.dead[t] { continue; }
            let mut i = 1;
            while i + 1 < c.b.traces[t].points.len() {
                let p = c.b.traces[t].points.clone();
                if anchor(&p, i as i32) { i += 1; continue; }
                let a = p[i - 1].pos;
                let bc = p[i].pos;
                let cc = p[i + 1].pos;
                let la = a.distance_to(bc);
                let lc = bc.distance_to(cc);
                if la < 4.0f32 * min_cut || lc < 4.0f32 * min_cut || turn_deg(a, bc, cc) < 1.0f32 { i += 1; continue; }
                let da = (a - bc) / la;
                let dc = (cc - bc) / lc;
                let s_max = std_min(c.o.tighten_max_cut, 0.5f32 * std_min(la, lc));
                // ADAPTED cpp:1199-1210: build returns the candidate rather
                // than capturing a mutable vector across guard calls.
                let build = |cut: f32| {
                    let mut q = positions(&p[..i]);
                    q.push(bc + da * cut);
                    q.push(bc + dc * cut);
                    q.extend(p[i + 1..].iter().map(|p| p.pos));
                    q
                };
                let mut good = 0.0f32;
                let mut bad = -1.0f32;
                let mut cut = std_min(step, s_max);
                loop {
                    let q = build(cut);
                    if !c.path_ok(t, &q, i as i32, i as i32 + 1, c.b.traces[t].layer, &[t]) { bad = cut; break; }
                    good = cut;
                    if cut >= s_max { break; }
                    cut = std_min(cut + step, s_max);
                }
                if bad > 0.0f32 {
                    for _it in 0..5 {
                        let cut = 0.5f32 * (good + bad);
                        let q = build(cut);
                        if c.path_ok(t, &q, i as i32, i as i32 + 1, c.b.traces[t].layer, &[t]) { good = cut; } else { bad = cut; }
                    }
                }
                if good < min_cut { s.rejected += 1; i += 1; continue; }
                let q = build(good);
                if !c.pair_span_ok(t, &q, i as i32 - 1, i as i32 + 1, i as i32 - 1, i as i32 + 2, s) { i += 1; continue; }
                c.pair_applied(c.b.traces[t].net);
                let mut m = p[..i].to_vec();
                m.push(plain_node(q[i], c.b.traces[t].width));
                m.push(plain_node(q[i + 1], c.b.traces[t].width));
                m.extend_from_slice(&p[i + 1..]);
                let before = poly_length(&p);
                c.set_points(t, m);
                s.length_gain += before - poly_length(&c.b.traces[t].points);
                let net = c.b.traces[t].net;
                c.edit(net).applied += 1;
                cuts += 1;
                i += 2;
            }
        }
        if cuts == 0 { break; }
    }
    step_detours(c, s);
}

// Cleanup.cpp:1254-1286.
fn fillet(a: Vec2, b: Vec2, c: Vec2, tl: f32, n_t: i32) -> Vec<Vec2> {
    let d0 = (b - a).normalized();
    let d1 = (c - b).normalized();
    let theta = d0.cross(d1).atan2(d0.dot(d1));
    let phi = theta / n_t as f32;
    let k1 = b - d0 * tl;
    let k2 = b + d1 * tl;
    let mut out = vec![k1];
    if n_t <= 2 { out.push(k2); return out; }
    let mut sum = Vec2::new(0.0, 0.0);
    let mut dirs = Vec::new();
    for m in 1..=n_t - 1 {
        let ang = phi * m as f32;
        let cs = ang.cos();
        let sn = ang.sin();
        let dm = Vec2::new(d0.x * cs - d0.y * sn, d0.x * sn + d0.y * cs);
        dirs.push(dm);
        sum += dm;
    }
    let sl = sum.length();
    if sl < 1e-6f32 { return vec![k1, k2]; }
    let s = (k2 - k1).length() / sl;
    let mut p = k1;
    for m in 0..dirs.len().saturating_sub(1) {
        p += dirs[m] * s;
        out.push(p);
    }
    out.push(k2);
    out
}

// Cleanup.cpp:1288-1341.
fn step_acute(c: &mut Ctx<'_>, s: &mut CleanupStepStats) {
    // ADAPTED cpp:1295: no budget. Finite traces/original corners, with at
    // most two turn-count choices and four scales. A chamfer advances k past
    // its inserted nodes, so this sweep never processes its own new corners.
    for t in 0..c.b.traces.len() {
        if !c.editable[t] || c.dead[t] { continue; }
        let mut k = 1;
        while k + 1 < c.b.traces[t].points.len() {
            let p = c.b.traces[t].points.clone();
            if anchor(&p, k as i32) { k += 1; continue; }
            let turn = turn_deg(p[k - 1].pos, p[k].pos, p[k + 1].pos);
            if turn <= 90.0f32 + 1e-3f32 { k += 1; continue; }
            let lp = p[k - 1].pos.distance_to(p[k].pos);
            let ln = p[k].pos.distance_to(p[k + 1].pos);
            let fp = if anchor(&p, k as i32 - 1) { 0.9f32 } else { 0.45f32 };
            let fn_ = if anchor(&p, k as i32 + 1) { 0.9f32 } else { 0.45f32 };
            let tmax = std_min(std_min(c.o.acute_max_leg, lp * fp), ln * fn_);
            if tmax < 0.01f32 { k += 1; continue; }
            let mut counts = Vec::new();
            if c.o.acute_prefer135 { counts.push(std_max(2, ((turn - 1e-3f32) / 45.0f32).ceil() as i32)); }
            if counts.is_empty() || counts.last() != Some(&2) { counts.push(2); }
            let mut done = false;
            for n_t in counts {
                for sc in [1.0f32, 0.6f32, 0.35f32, 0.2f32] {
                    let tl = tmax * sc;
                    if tl < 0.01f32 { break; }
                    let f = fillet(p[k - 1].pos, p[k].pos, p[k + 1].pos, tl, n_t);
                    let mut m = p[..k].to_vec();
                    for &point in &f { m.push(plain_node(point, c.b.traces[t].width)); }
                    m.extend_from_slice(&p[k + 1..]);
                    let q = positions(&m);
                    if !c.path_ok(t, &q, k as i32 - 1, (k + f.len()) as i32, c.b.traces[t].layer, &[t]) { s.rejected += 1; continue; }
                    if !c.pair_span_ok(t, &q, k as i32 - 1, k as i32 + 1, k as i32 - 1, (k + f.len()) as i32, s) { break; }
                    c.pair_applied(c.b.traces[t].net);
                    let before = poly_length(&p);
                    c.set_points(t, m);
                    s.length_gain += before - poly_length(&c.b.traces[t].points);
                    let net = c.b.traces[t].net;
                    c.edit(net).applied += 1;
                    c.edit(net).chamfers += 1;
                    k += f.len() - 1;
                    done = true;
                    break;
                }
                if done { break; }
            }
            k += 1;
        }
    }
}

// Cleanup.cpp:1346-1380.
fn resample(sk: &[TracePoint], width: f32, spacing: f32, guard0: f32) -> Pts {
    let m = sk.len();
    if m < 2 { return sk.to_vec(); }
    let mut out = Vec::new();
    for k in 0..m - 1 {
        let a = sk[k].pos;
        let c = sk[k + 1].pos;
        let len = a.distance_to(c);
        let stub = sk[k + 1].straight_before;
        let mut ts = Vec::new();
        let steps = if stub { 1 } else { std_max(1, (len / std_max(0.05f32, spacing)).ceil() as i32) };
        for s in 0..steps { ts.push(s as f32 / steps as f32); }
        let guard = std_min(guard0, len * 0.3f32);
        if len > 1e-4f32 && !stub && guard0 > 0.0f32 {
            let after_stub = sk[k].straight_before;
            let before_stub = k + 2 < m && sk[k + 2].straight_before;
            let gs = if after_stub { std_min(0.07f32, len * 0.3f32) } else { guard };
            let ge = if before_stub { std_min(0.07f32, len * 0.3f32) } else { guard };
            if k > 0 { ts.push(gs / len); }
            if k + 2 < m { ts.push(1.0f32 - ge / len); }
        }
        // cpp:1368: std::sort tie (equal particle parameters); retain the
        // same comparator and use an unstable sort, not a stable tie-break.
        ts.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        let mut last_t = -1.0f32;
        for t in ts {
            if t >= 1.0f32 - 1e-5f32 || (last_t >= 0.0f32 && (t - last_t) * len < 0.05f32) { continue; }
            if last_t < 0.0f32 { out.push(sk[k].clone()); } else { out.push(plain_node(Vec2::lerp(a, c, t), width)); }
            last_t = t;
        }
    }
    out.push(sk.last().unwrap().clone());
    out
}

// Cleanup.cpp:1382-1408.
fn compact_board(c: &mut Ctx<'_>) {
    let mut kept = Vec::with_capacity(c.b.traces.len());
    for (t, tr) in std::mem::take(&mut c.b.traces).into_iter().enumerate() {
        if !(t < c.dead.len() && c.dead[t]) { kept.push(tr); }
    }
    c.b.traces = kept;
    let mut remap = vec![-1; c.b.vias.len()];
    let mut vk = Vec::new();
    for (v, via) in c.b.vias.iter().enumerate() {
        if v < c.env.via_dead.len() && c.env.via_dead[v] { continue; }
        remap[v] = vk.len() as i32;
        let mut via = via.clone();
        via.id = remap[v];
        vk.push(via);
    }
    if vk.len() != c.b.vias.len() {
        for tr in &mut c.b.traces {
            for p in &mut tr.points {
                if p.attached_via >= 0 {
                    p.attached_via = if p.attached_via < remap.len() as i32 { remap[p.attached_via as usize] } else { -1 };
                }
            }
        }
        c.b.vias = vk;
    }
}

/// Cleanup.cpp:1519-1524: f32 segment distances accumulated in f64.
pub fn total_length(board: &OBoard) -> f64 {
    let mut l = 0.0f64;
    for tr in &board.traces { l += poly_length(&tr.points); }
    l
}

/// Cleanup.cpp:1526-1550. ADAPTED h:162: the default tolerance (1e-4 mm)
/// is an explicit argument, as in the other ported helpers.
pub fn corner_segments(board: &OBoard, tol: f32) -> i32 {
    let mut segs = 0;
    for tr in &board.traces {
        let n = tr.points.len();
        if n < 2 { continue; }
        let mut a = 0;
        while a < n - 1 {
            let mut k = a + 1;
            while k + 1 < n {
                let mut within = true;
                for m in a + 1..=k {
                    if !within { break; }
                    if point_seg_d(tr.points[m].pos, tr.points[a].pos, tr.points[k + 1].pos) > tol as f64 { within = false; }
                }
                if !within { break; }
                k += 1;
            }
            segs += 1;
            a = k;
        }
    }
    segs
}

// ADAPTED cpp:1552-1577: drcNotWorse belongs to Guard; summary/printf and
// clocks are omitted. cpp:1630-1636,1724-1737 use this one copper comparison.
// Trace/via multisets are keyed by content, never by ids: compactBoard can
// renumber otherwise unchanged vias and their attachments.
fn changed_nets(snap_t: &[Trace], snap_v: &[Via], board: &OBoard) -> Vec<i32> {
    type Pos = (u32, u32);
    type TraceKey = (i32, u32, Vec<(Pos, Option<Pos>)>);
    type ViaKey = (Pos, u32, u32, i32, i32);
    type Copper = (Vec<TraceKey>, Vec<ViaKey>);
    fn copper(traces: &[Trace], vias: &[Via]) -> BTreeMap<i32, Copper> {
        let mut out: BTreeMap<i32, Copper> = BTreeMap::new();
        for t in traces {
            let points = t.points.iter().map(|p| {
                let via_pos = if p.attached_via >= 0 {
                    vias.get(p.attached_via as usize).map(|v| (v.pos.x.to_bits(), v.pos.y.to_bits()))
                } else { None };
                ((p.pos.x.to_bits(), p.pos.y.to_bits()), via_pos)
            }).collect();
            out.entry(t.net).or_default().0.push((t.layer, t.width.to_bits(), points));
        }
        for v in vias {
            out.entry(v.net).or_default().1.push(((v.pos.x.to_bits(), v.pos.y.to_bits()), v.diameter.to_bits(), v.drill.to_bits(), v.layer_start, v.layer_end));
        }
        // New adapter containers: iterate BTreeMap values in net order to
        // canonicalise each multiset; duplicates are retained.
        for (traces, vias) in out.values_mut() {
            traces.sort_unstable_by(|a, b| a.cmp(b));
            vias.sort_unstable_by(|a, b| a.cmp(b));
        }
        out
    }
    let before = copper(snap_t, snap_v);
    let after = copper(&board.traces, &board.vias);
    // New adapter BTreeSet: iterate the union once in ascending net order;
    // BTreeMap key iterators also run in ascending net order.
    let nets: BTreeSet<_> = before.keys().chain(after.keys()).copied().collect();
    nets.into_iter().filter(|net| before.get(net) != after.get(net)).collect()
}

// ADAPTED cpp:1639-1648: our net restore, before the reference's whole-step undo.
// Traces and vias of a net only attach to that net's vias, so other nets' copper
// is untouched. Keep board order first, then snapshot order, remapping each source
// separately because its via indices belong to its own vector.
fn restore_nets(board: &mut OBoard, snap_t: &[Trace], snap_v: &[Via], nets: &BTreeSet<i32>) -> bool {
    let kept_ids: BTreeSet<i32> = board.traces.iter().filter(|tr| !nets.contains(&tr.net)).map(|tr| tr.id).collect();
    // This BTreeSet is queried only. Reject ambiguous trace ids before changing anything.
    if snap_t.iter().any(|tr| nets.contains(&tr.net) && kept_ids.contains(&tr.id)) { return false; }

    let mut board_remap = vec![-1; board.vias.len()];
    let mut snap_remap = vec![-1; snap_v.len()];
    let mut vias = Vec::new();
    for (i, via) in board.vias.iter().enumerate() {
        if nets.contains(&via.net) { continue; }
        let mut via = via.clone();
        via.id = vias.len() as i32;
        board_remap[i] = via.id;
        vias.push(via);
    }
    for (i, via) in snap_v.iter().enumerate() {
        if !nets.contains(&via.net) { continue; }
        let mut via = via.clone();
        via.id = vias.len() as i32;
        snap_remap[i] = via.id;
        vias.push(via);
    }
    fn remap_trace(tr: &Trace, remap: &[i32]) -> Trace {
        let mut tr = tr.clone();
        for p in &mut tr.points {
            p.attached_via = if p.attached_via >= 0 {
                remap.get(p.attached_via as usize).copied().unwrap_or(-1)
            } else { -1 };
        }
        tr
    }
    let mut traces = Vec::new();
    for tr in &board.traces {
        if !nets.contains(&tr.net) { traces.push(remap_trace(tr, &board_remap)); }
    }
    for tr in snap_t {
        if nets.contains(&tr.net) { traces.push(remap_trace(tr, &snap_remap)); }
    }
    board.traces = traces;
    board.vias = vias;
    true
}

impl Ctx<'_> {
    // ADAPTED cpp:1605-1663: explicit guard and accepted snapshot replace
    // captured DRC reports. No clock or printing; diagnostics go into S.
    fn run_step(&mut self, name: &str, enabled: bool, guard: &mut dyn Guard,
        accepted: &mut (Vec<Trace>, Vec<Via>), f: impl FnOnce(&mut Self, &mut CleanupStepStats)) {
        let mut s = CleanupStepStats { name: name.to_owned(), ..CleanupStepStats::default() };
        // ADAPTED cpp:1608: no budget gate. run has a fixed list of steps.
        if !enabled { self.st.steps.push(s); return; }
        let snap_t = self.b.traces.clone();
        let snap_v = self.b.vias.clone();
        let len0 = total_length(self.b);
        let segs0 = skeleton_segments(self.b);
        let vias0 = self.b.vias.len() as i32;
        self.edits.clear();
        f(self, &mut s);
        s.fails = self.env.fails;
        s.self_cross = self.self_cross;
        compact_board(self);
        let mut keep = true;
        let mut reverted = BTreeSet::<i32>::new();
        // ADAPTED cpp:1630-1636: judge only the sorted, unique changed nets
        // after compaction; no judge for an empty change. Guard owns DRC state.
        if self.o.step_guard {
            let changed = changed_nets(&snap_t, &snap_v, self.b);
            if !changed.is_empty() {
                self.st.judge_calls += 1;
                keep = guard.judge(self.b, &changed);
                if !keep {
                    // Our net fixpoint: each round adds at least one of the finite changed
                    // nets to reverted or leaves. BTreeSet iteration adds named nets in
                    // ascending order. Whole-step undo remains the fallback below.
                    loop {
                        let named: BTreeSet<i32> = guard.refusal().iter().flat_map(|f| f.nets.iter()).copied()
                            .filter(|net| changed.binary_search(net).is_ok() && !reverted.contains(net)).collect();
                        if named.is_empty() { break; }
                        reverted.extend(named);
                        if !restore_nets(self.b, &snap_t, &snap_v, &reverted) { break; }
                        let now = changed_nets(&snap_t, &snap_v, self.b);
                        if now.is_empty() { break; }
                        self.st.judge_calls += 1;
                        keep = guard.judge(self.b, &now);
                        if keep {
                            s.nets_reverted = reverted.len();
                            self.st.nets_reverted += reverted.len();
                            break;
                        }
                    }
                }
                if keep { *accepted = (self.b.traces.clone(), self.b.vias.clone()); }
            }
        }
        if !keep {
            self.b.traces = snap_t;
            self.b.vias = snap_v;
            s.reverted = true;
            self.st.step_reverts += 1;
            // the counters were kept per net (EditCounts): a reverted step adds none of them, as
            // the reference's restore of its counters does (cpp 1640-1646)
            s.length_gain = 0.0;
        } else {
            // ADAPTED (the net fixpoint): only the edits that stand count. A net the fixpoint put
            // back keeps none of its edits; the reference, without the fixpoint, keeps all of a
            // standing step's. pair_edits likewise (the reference does not restore it on a revert).
            for (net, e) in &self.edits {
                if reverted.contains(net) { continue; }
                s.applied += e.applied;
                s.pair_edits += e.pair_edits;
                self.st.pair_edits += e.pair_edits;
                self.st.loops_removed += e.loops_removed;
                self.st.elbows += e.elbows;
                self.st.straight_shortcuts += e.straight_shortcuts;
                self.st.chamfers += e.chamfers;
                self.st.vias_removed += e.vias_removed;
                self.st.hops_rerouted += e.hops_rerouted;
            }
            s.length_gain = len0 - total_length(self.b);
            s.segment_gain = segs0 - skeleton_segments(self.b);
            s.via_gain = vias0 - self.b.vias.len() as i32;
        }
        self.rebuild();
        self.st.steps.push(s);
    }
}

/// Cleanup.cpp:1579-1747: mutate the board in place, restoring the reference's
/// snapshots on refusal. `unported` reports requested options not yet available.
pub fn run(board: &mut OBoard, opt: &CleanupOptions, guard: &mut dyn Guard) -> CleanupStats {
    // ADAPTED cpp:1581-1582,1592,1744-1746: no clock, budget or printing.
    let mut st = CleanupStats {
        vias_before: board.vias.len() as i32,
        traces_before: board.traces.len() as i32,
        length_before: total_length(board),
        segments_before: corner_segments(board, 1e-4f32),
        ..CleanupStats::default()
    };
    for t in &board.traces { st.nodes_before += t.points.len() as i32; }
    let mut c = Ctx { b: board, o: opt, st, env: Env::default(), editable: Vec::new(), dead: Vec::new(),
        plane_of_net: Vec::new(), fanout_via: Vec::new(), self_cross: 0, pg: PairGuard::default(), edits: BTreeMap::new() };
    // ADAPTED cpp:1591 (and 431): no avoidPadGaps option, padGaps field or
    // call; PadGap.h is not cleared for this port.
    c.pg.init(c.b, opt.pair_aware);
    let orig_traces = c.b.traces.clone();
    let orig_vias = c.b.vias.clone();
    // ADAPTED cpp:1596-1597: no initial DRC, drcBefore or cur; Guard already
    // holds the input. This snapshot moves only when a step judge accepts.
    let mut accepted = (orig_traces.clone(), orig_vias.clone());
    // ADAPTED cpp:1600: std::map -> BTreeMap. orig_index is looked up only,
    // never iterated; filling follows orig_traces order.
    let mut orig_index = BTreeMap::new();
    for (t, tr) in orig_traces.iter().enumerate() { orig_index.insert(tr.id, t); }
    c.rebuild();
    step_decimate(&mut c, 2e-5f32, false, &mut CleanupStepStats::default());
    // ADAPTED cpp:1670,1678: omit the unused skelSegs0 value.
    // ADAPTED cpp:1671-1672: std::map -> BTreeMap. skel0 and skel_layer0
    // are only looked up, never iterated; filling follows board trace order.
    let mut skel0 = BTreeMap::new();
    let mut skel_layer0 = BTreeMap::new();
    for tr in &c.b.traces {
        skel0.insert(tr.id, positions(&tr.points));
        skel_layer0.insert(tr.id, tr.layer);
    }
    c.run_step("collinear", opt.collinear, guard, &mut accepted, |c, s| step_decimate(c, opt.collinear_tol, true, s));
    c.run_step("vias", opt.vias, guard, &mut accepted, step_vias);
    // ADAPTED cpp:1410-1517,1682-1684: rules (C5c): not ported; needs per-item findings from the guard (decided at L6.2 review)
    c.run_step("unloop", opt.unloop, guard, &mut accepted, step_unloop);
    c.run_step("detours", opt.detours, guard, &mut accepted, step_detours);
    c.run_step("tighten", opt.tighten_rounds > 0, guard, &mut accepted, step_tighten);
    c.run_step("acute", opt.acute, guard, &mut accepted, step_acute);
    c.run_step("merge", opt.collinear, guard, &mut accepted, |c, s| step_decimate(c, opt.collinear_tol, true, s));
    for tr in &mut c.b.traces {
        let it = skel0.get(&tr.id);
        let io = orig_index.get(&tr.id);
        let mut same = it.is_some() && io.is_some() && skel_layer0.get(&tr.id) == Some(&tr.layer)
            && it.unwrap().len() == tr.points.len();
        for k in 0..tr.points.len() {
            if !same { break; }
            // ADAPTED cpp:1699: memcmp(Vec2) is both coordinates' bits.
            let p = it.unwrap()[k];
            same = p.x.to_bits() == tr.points[k].pos.x.to_bits() && p.y.to_bits() == tr.points[k].pos.y.to_bits();
        }
        if same {
            let mut orig = orig_traces[*io.unwrap()].points.clone();
            let mut j = 0;
            for p in &mut orig {
                // ADAPTED cpp:1708: compare Vec2 bits, preserving signed zeros.
                if j < tr.points.len() && p.pos.x.to_bits() == tr.points[j].pos.x.to_bits() && p.pos.y.to_bits() == tr.points[j].pos.y.to_bits() {
                    p.attached_via = tr.points[j].attached_via;
                    j += 1;
                } else { p.attached_via = -1; }
            }
            tr.points = orig;
            continue;
        }
        if opt.resample { tr.points = resample(&tr.points, tr.width, opt.resample_spacing, opt.corner_guard); }
        tr.length = poly_length(&tr.points) as f32;
        tr.smoothed.clear();
        tr.point_widths.clear();
        tr.teardrop_front.clear();
        tr.teardrop_back.clear();
    }
    // ADAPTED cpp:1724-1737: compare the output against the last accepted
    // step snapshot (input when step_guard is off); skip an empty change.
    let changed = changed_nets(&accepted.0, &accepted.1, c.b);
    let verdict = if changed.is_empty() { true } else {
        c.st.judge_calls += 1;
        guard.judge(c.b, &changed)
    };
    let mut attach_ok = true;
    for (v, via) in c.b.vias.iter().enumerate() { attach_ok = attach_ok && via.id == v as i32; }
    for tr in &c.b.traces {
        for p in &tr.points {
            if p.attached_via >= 0 {
                attach_ok = attach_ok && p.attached_via < c.b.vias.len() as i32
                    && c.b.vias[p.attached_via as usize].net == tr.net
                    && c.b.vias[p.attached_via as usize].pos.distance_to(p.pos) < 0.01f32;
            }
        }
    }
    let keep = attach_ok && verdict;
    if !keep {
        let nets = changed_nets(&orig_traces, &orig_vias, c.b);
        c.b.traces = orig_traces;
        c.b.vias = orig_vias;
        // The judged steps moved the guard's accepted state, and the restore has to bring it back to the input.
        guard.sync(c.b, &nets);
        c.st.reverted = true;
    }
    c.st.vias_after = c.b.vias.len() as i32;
    c.st.traces_after = c.b.traces.len() as i32;
    c.st.length_after = total_length(c.b);
    c.st.segments_after = corner_segments(c.b, 1e-4f32);
    for t in &c.b.traces { c.st.nodes_after += t.points.len() as i32; }
    c.st
}

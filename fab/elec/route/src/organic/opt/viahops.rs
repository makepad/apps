// Ported from TiNRS Psychedelic Router 0.1.0: src/optimize/ViaHops.h (111-225, 247-248), src/optimize/ViaHops.cpp (32-1838, 2107-2707), src/core/Board.h (578-582).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in the parent folder.

//! Remove short via dips, with local clearance, pair coupling and dependency-aware Guard rollback.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use super::super::diffpairs;
use super::super::geom::{std_max, std_min, Box2, Vec2};
use super::super::model::{Keepout, OBoard, Pad, Trace, TracePoint, Via};
use super::super::simplify::reverse_points;
use super::drcgeom as drc;
use super::hops::count_hops;
use super::pairguard::PairGuard;
use super::{fanout_via_mask, Guard};

// ADAPTED ViaHops.cpp:24-30,174,178,209,334-335,349,402,473,2118,2132-2151,2705-2706:
// clocks, work units, budgets and the testSlowdown hook are omitted. Finite candidate sweeps
// finish; no machine-dependent early exit. No TestSlowdown source is needed.
// ADAPTED ViaHops.cpp:500-513,790,816,900,960,1127-1129,1133,1138,1143,1157,
// 1181,1228-1244,1249,1263-1266,1274,1283,1294,2078-2105,2392,2413-2414,
// 2448,2481-2484,2515-2517,2616,2679-2698: no printing, verbose, summary, or
// diagnostic-only calculations (Run::len / Spike's measured fields, cpp:152,1433).
// ADAPTED ViaHops.cpp:679,1036,1309,1560,1618,2461: force resets have no stored
// OBoard field; the relaxer computes forces in Engine scratch. Velocity resets remain.
// ADAPTED ViaHops.cpp:323,378,993,1065,1094,1765: DrcOptions is absent; its
// default viaThrough=false is explicit in the layer tests below.

/// ViaHops.h:111-117.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(i32)]
pub enum HopClass {
    #[default]
    Empty = 0,
    OneTrace,
    PadOrVia,
    Multiple,
}

/// ViaHops.h:119-171. Distances in mm; angles in degrees.
// ADAPTED h:138-144,168-170: budgets, maxGuardIterations, DrcOptions and verbose
// are omitted. max_rounds, slide_spots and candidate-distance settings remain.
#[derive(Clone, Debug)]
pub struct ViaHopOptions {
    pub max_hop_length: f32,
    pub detour_radius: f32,
    pub max_push: f32,
    pub merge_gap: f32,
    pub max_extra_length: f32,
    pub enable_go_around: bool,
    pub enable_push: bool,
    pub enable_flip: bool,
    pub enable_merge: bool,
    pub enable_absorb: bool,
    pub absorb_length: f32,
    pub slide_reach: f32,
    pub slide_spots: i32,
    pub grid_pitch: f32,
    pub pull_window: f32,
    pub clearance_slack: f32,
    pub max_rounds: i32,
    pub respect_keepouts: bool,
    pub edit_imported: bool,
    pub pair_aware: bool,
    pub spike_merge: bool,
    pub plane_taps: bool,
    pub spike_max_dist: f32,
    pub spike_min_turn_deg: f32,
    pub spike_detour: f32,
    pub node_spacing: f32,
    pub corner_guard: f32,
    pub drc_guard: bool,
}

impl Default for ViaHopOptions {
    fn default() -> Self {
        Self {
            max_hop_length: 3.0,
            detour_radius: 4.0,
            max_push: 1.5,
            merge_gap: 2.0,
            max_extra_length: 6.0,
            enable_go_around: true,
            enable_push: true,
            enable_flip: true,
            enable_merge: true,
            enable_absorb: true,
            absorb_length: 12.0,
            slide_reach: 10.0,
            slide_spots: 8,
            grid_pitch: 0.05,
            pull_window: 2.0,
            clearance_slack: 0.003,
            max_rounds: 3,
            respect_keepouts: true,
            edit_imported: false,
            pair_aware: true,
            spike_merge: false,
            plane_taps: true,
            spike_max_dist: 2.0,
            spike_min_turn_deg: 90.0,
            spike_detour: 1.5,
            node_spacing: 0.8,
            corner_guard: 0.15,
            drc_guard: true,
        }
    }
}

/// ViaHops.h:193-198. Final via indices after compaction.
// ADAPTED h:193: Rust exposes Site at module scope instead of nesting it in ViaHopStats.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    pub via_a: i32,
    pub via_b: i32,
    pub cls: HopClass,
    pub reachable: bool,
}
impl Default for Site {
    fn default() -> Self {
        Self {
            via_a: -1,
            via_b: -1,
            cls: HopClass::Empty,
            reachable: false,
        }
    }
}

/// ViaHops.h:173-216. guard_iterations counts every judge call, including refusals.
// ADAPTED h:209-215: no time/work/budget fields or DrcReport snapshots; Guard owns DRC.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViaHopStats {
    pub hops_before: i32,
    pub hops_after: i32,
    pub candidates: i32,
    pub by_class: [i32; 4],
    pub removed_empty: i32,
    pub go_around: i32,
    pub pushed: i32,
    pub flipped: i32,
    pub relayered: i32,
    pub slid: i32,
    pub merged: i32,
    pub absorbed: i32,
    pub unresolved: i32,
    pub unresolved_by_class: [i32; 4],
    pub one_trace_reachable: i32,
    pub resolvable: i32,
    pub resolvable_resolved: i32,
    pub unresolved_sites: Vec<Site>,
    pub attempts: i32,
    pub search_fails: i32,
    pub length_rejects: i32,
    pub check_rejects: i32,
    pub vias_before: i32,
    pub vias_after: i32,
    pub length_before: f64,
    pub length_after: f64,
    pub edits: i32,
    pub reverts: i32,
    pub guard_iterations: i32,
    pub spike_candidates: i32,
    pub spike_merged: i32,
    pub spike_straightened: i32,
    pub plane_tap_candidates: i32,
    pub plane_taps_removed: i32,
    pub pair_skipped: i32,
    pub pair_edits: i32,
    pub full_revert: bool,
}
impl ViaHopStats {
    pub fn resolved(&self) -> i32 {
        self.removed_empty
            + self.go_around
            + self.pushed
            + self.flipped
            + self.relayered
            + self.slid
            + self.merged
            + self.absorbed
            + self.plane_taps_removed
    }
}

// ViaHops.cpp:32-95. reversed uses the existing TraceEdit reversal (same flags).
fn point_seg(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let l2 = ab.length_squared();
    let mut t = if l2 > 1e-12f32 { (p - a).dot(ab) / l2 } else { 0.0 };
    t = std_max(0.0, std_min(1.0, t));
    p.distance_to(a + ab * t)
}
fn closest_on_seg(p: Vec2, a: Vec2, b: Vec2) -> Vec2 {
    let ab = b - a;
    let l2 = ab.length_squared();
    let mut t = if l2 > 1e-12f32 { (p - a).dot(ab) / l2 } else { 0.0 };
    t = std_max(0.0, std_min(1.0, t));
    a + ab * t
}
fn point_in_poly(poly: &[Vec2], p: Vec2) -> bool {
    let mut inside = false;
    let mut j = poly.len().wrapping_sub(1);
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}
fn segs_cross(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> bool {
    let orient = |p: Vec2, q: Vec2, r: Vec2| (q - p).cross(r - p);
    let (d1, d2, d3, d4) = (orient(c, d, a), orient(c, d, b), orient(a, b, c), orient(a, b, d));
    ((d1 > 1e-9f32 && d2 < -1e-9f32) || (d1 < -1e-9f32 && d2 > 1e-9f32))
        && ((d3 > 1e-9f32 && d4 < -1e-9f32) || (d3 < -1e-9f32 && d4 > 1e-9f32))
}
fn poly_len(pts: &[TracePoint]) -> f64 {
    let mut length = 0.0f64;
    for p in pts.windows(2) {
        length += p[0].pos.distance_to(p[1].pos) as f64;
    }
    length
}
fn boxes_overlap(a: Box2, b: Box2) -> bool {
    a.min.x <= b.max.x && b.min.x <= a.max.x && a.min.y <= b.max.y && b.min.y <= a.max.y
}
fn seg_box(a: Vec2, b: Vec2, g: f32) -> Box2 {
    let mut bx = Box2::default();
    bx.expand(a);
    bx.expand(b);
    bx.pad(g);
    bx
}
fn reversed(p: &[TracePoint]) -> Vec<TracePoint> {
    let mut r = p.to_vec();
    reverse_points(&mut r);
    r
}

// ADAPTED ViaHops.cpp:457-458,941: C++ numeric float ordering (including signed-zero
// ties), not total_cmp's bit ordering. Routing coordinates/scores are finite.
#[derive(Clone, Copy)]
struct FloatOrd(f32);
impl PartialEq for FloatOrd {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for FloatOrd {}
impl PartialOrd for FloatOrd {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for FloatOrd {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.0 < other.0 {
            Ordering::Less
        } else if other.0 < self.0 {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Qe(FloatOrd, i32);
impl PartialOrd for Qe {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Qe {
    fn cmp(&self, other: &Self) -> Ordering {
        // cpp:458 std::greater<pair<float,int>> pops minimum f, then minimum cell id.
        other.0.cmp(&self.0).then_with(|| other.1.cmp(&self.1))
    }
}

// ViaHops.cpp:98-153.
#[derive(Clone, Copy, Default)]
enum Strategy {
    #[default]
    Empty,
    Around,
    Push,
    Flip,
    Merge,
    Absorb,
    Relayer,
    Slide,
    SpikeMerge,
    SpikeStraight,
    PlaneTap,
}
#[derive(Clone)]
struct Edit {
    before: Vec<Trace>,
    after_ids: Vec<i32>,
    vias: Vec<usize>,
    new_vias: Vec<usize>,
    box_: Box2,
    nets: BTreeSet<i32>,
    strategy: Strategy,
    active: bool,
    pair: bool,
}
impl Default for Edit {
    fn default() -> Self {
        Self {
            before: Vec::new(),
            after_ids: Vec::new(),
            vias: Vec::new(),
            new_vias: Vec::new(),
            box_: Box2::default(),
            nets: BTreeSet::new(),
            strategy: Strategy::Empty,
            active: true,
            pair: false,
        }
    }
}
impl Edit {
    fn touched(&self) -> BTreeSet<i32> {
        let mut s: BTreeSet<_> = self.after_ids.iter().copied().collect();
        for t in &self.before {
            s.insert(t.id);
        }
        s
    }
    fn absorb(&mut self, o: Edit) {
        // ADAPTED cpp:120,124-141: BTreeSet. have is lookup-only; o.nets is
        // iterated here in ascending order; touched is iterated by rollback below.
        let have: BTreeSet<_> = self.before.iter().map(|t| t.id).collect();
        for t in o.before {
            if !have.contains(&t.id) {
                self.before.push(t);
            }
        }
        for id in o.after_ids {
            if !self.after_ids.contains(&id) {
                self.after_ids.push(id);
            }
        }
        self.vias.extend(o.vias);
        self.new_vias.extend(o.new_vias);
        if o.box_.is_valid() {
            self.box_.expand_box(&o.box_);
        }
        self.nets.extend(o.nets);
    }
}
// ADAPTED cpp:146-153,282: success-only Run and Option<Run> replace the output
// parameter's -1 sentinels. Indices inside a successful run are usize.
#[derive(Clone, Copy)]
struct Run {
    r: usize,
    vs: usize,
    ve: usize,
    p: usize,
    p_end: i32,
    q: usize,
    q_end: i32,
    outer: i32,
}

// ViaHops.cpp:158-196. ADAPTED cpp:164,169,172: clone immutable pad/keepout/outline
// data instead of storing C++ pointers across board mutations; these items never change.
struct CheckSeg {
    a: Vec2,
    b: Vec2,
    req: f32,
}
struct SoftSeg {
    a: Vec2,
    b: Vec2,
    hard: f32,
    full: f32,
    id: i32,
}
struct Disc {
    c: Vec2,
    req: f32,
}
struct CheckPad {
    p: Pad,
    req: f32,
}
struct Checker {
    hw: f32,
    slack: f32,
    segs: Vec<CheckSeg>,
    soft: Vec<SoftSeg>,
    discs: Vec<Disc>,
    pads: Vec<CheckPad>,
    kos: Vec<Keepout>,
    edges: Vec<(Vec2, Vec2)>,
    edge_req: f32,
    outline: Vec<Vec2>,
    box_inside: bool,
}
impl Checker {
    fn seg_ok(&self, a: Vec2, b: Vec2, hard_soft: bool) -> bool {
        let h = self.hw + self.slack;
        for s in &self.segs {
            if drc::segment_segment_distance(a, b, s.a, s.b) < s.req + h {
                return false;
            }
        }
        for s in &self.soft {
            if drc::segment_segment_distance(a, b, s.a, s.b) < (if hard_soft { s.hard } else { s.full }) + h {
                return false;
            }
        }
        for d in &self.discs {
            if point_seg(d.c, a, b) < d.req + h {
                return false;
            }
        }
        for p in &self.pads {
            if drc::segment_pad_distance(a, b, &p.p) < p.req + h {
                return false;
            }
        }
        for k in &self.kos {
            if drc::segment_keepout_distance(k, a, b) < drc::keepout_margin(k) + h {
                return false;
            }
        }
        for &(a0, b0) in &self.edges {
            if drc::segment_segment_distance(a, b, a0, b0) < self.edge_req + h {
                return false;
            }
        }
        if !self.edges.is_empty() && self.outline.len() >= 3 && (!point_in_poly(&self.outline, a) || !point_in_poly(&self.outline, b)) {
            return false;
        }
        if self.edges.is_empty() && !self.box_inside {
            return false;
        }
        true
    }
}

// ViaHops.cpp:198-328. ADAPTED cpp:202-203,526,884,1010,1208,1270:
// boolean masks replace vector<char>, including the per-search orig/anchor/suffix masks.
struct State<'a> {
    b: &'a mut OBoard,
    o: &'a ViaHopOptions,
    s: &'a mut ViaHopStats,
    via_dead: Vec<bool>,
    fanout_via: Vec<bool>,
    via_ends: Vec<Vec<(usize, i32)>>,
    via_interior: Vec<i32>,
    id_to_idx: BTreeMap<i32, usize>,
    branch_refs: BTreeMap<i32, Vec<(usize, usize)>>,
    plane_of_net: BTreeMap<i32, i32>,
    edits: Vec<Edit>,
    edge_clr: f32,
    pg: PairGuard,
    net_vias: BTreeMap<i32, Vec<usize>>,
}
impl<'a> State<'a> {
    fn new(b: &'a mut OBoard, o: &'a ViaHopOptions, s: &'a mut ViaHopStats) -> Self {
        let via_dead = vec![false; b.vias.len()];
        let fanout_via = fanout_via_mask(b);
        // ADAPTED cpp:214,222-223: ClearanceTable replaces NetClearances. Fab has
        // one edge rule (adapt.rs:285, including float margin); the reference's
        // board/project sources of that same rule have their maximum here.
        let edge_clr = b.rules.board_edge_clearance;
        Self {
            b,
            o,
            s,
            via_dead,
            fanout_via,
            via_ends: Vec::new(),
            via_interior: Vec::new(),
            id_to_idx: BTreeMap::new(),
            branch_refs: BTreeMap::new(),
            plane_of_net: BTreeMap::new(),
            edits: Vec::new(),
            edge_clr,
            pg: PairGuard::default(),
            net_vias: BTreeMap::new(),
        }
    }
    fn rebuild(&mut self) {
        self.via_ends = vec![Vec::new(); self.b.vias.len()];
        self.via_interior = vec![0; self.b.vias.len()];
        // ADAPTED cpp:206-208,1644: unordered_map/map -> BTreeMap. id_to_idx,
        // branch_refs, plane_of_net, net_vias are never map-iterated. Branch and
        // net-via vectors are visited in insertion (board) order.
        self.id_to_idx.clear();
        self.branch_refs.clear();
        for (t, tr) in self.b.traces.iter().enumerate() {
            self.id_to_idx.insert(tr.id, t);
            for (k, p) in tr.points.iter().enumerate() {
                if p.attached_via >= 0 && (p.attached_via as usize) < self.b.vias.len() {
                    let v = p.attached_via as usize;
                    if k == 0 {
                        self.via_ends[v].push((t, 0));
                    } else if k + 1 == tr.points.len() {
                        self.via_ends[v].push((t, 1));
                    } else {
                        self.via_interior[v] += 1;
                    }
                }
                if p.branch_trace >= 0 {
                    self.branch_refs.entry(p.branch_trace).or_default().push((t, k));
                }
            }
        }
    }
    fn has_plane(&mut self, net: i32) -> bool {
        if let Some(&l) = self.plane_of_net.get(&net) {
            return l >= 0;
        }
        let l = drc::plane_layer_of_net(self.b, net);
        self.plane_of_net.insert(net, l);
        l >= 0
    }
    fn trace_editable(&self, t: &Trace) -> bool {
        t.is_routed && !t.locked && (self.o.edit_imported || !t.imported)
    }
    fn via_editable(&self, v: usize) -> bool {
        let via = &self.b.vias[v];
        !self.via_dead[v]
            && !via.locked
            && !via.is_fixed
            && (self.o.edit_imported || !via.imported)
            && !(v < self.fanout_via.len() && self.fanout_via[v])
    }
    fn via_on_pad(&self, v: usize) -> bool {
        let via = &self.b.vias[v];
        for pad in &self.b.pads {
            if (pad.center.x - via.pos.x).abs() > 6.0 || (pad.center.y - via.pos.y).abs() > 6.0 {
                continue;
            }
            if drc::point_pad_distance(via.pos, pad) < via.diameter * 0.5 {
                return true;
            }
        }
        false
    }
    fn find_run(&mut self, r: usize, max_len: f32) -> Option<Run> {
        let tr = self.b.traces.get(r)?;
        if !self.trace_editable(tr) || tr.points.len() < 2 {
            return None;
        }
        let (vs, ve) = (tr.points.first()?.attached_via, tr.points.last()?.attached_via);
        if vs < 0 || ve < 0 || vs as usize >= self.b.vias.len() || ve as usize >= self.b.vias.len() || vs == ve {
            return None;
        }
        let (vs, ve) = (vs as usize, ve as usize);
        if !self.via_editable(vs) || !self.via_editable(ve) {
            return None;
        }
        if self.b.vias[vs].net != tr.net || self.b.vias[ve].net != tr.net {
            return None;
        }
        if self.via_interior[vs] != 0 || self.via_interior[ve] != 0 || self.via_ends[vs].len() != 2 || self.via_ends[ve].len() != 2 {
            return None;
        }
        if self.branch_refs.contains_key(&tr.id) {
            return None;
        }
        for p in &tr.points {
            if p.branch_trace >= 0 || p.terminal_pad >= 0 {
                return None;
            }
        }
        let len = poly_len(&tr.points) as f32;
        if len > max_len {
            return None;
        }
        let other = |v: usize, end| self.via_ends[v].iter().copied().filter(|&(ti, te)| !(ti == r && te == end)).last();
        let (p, p_end) = other(vs, 0)?;
        let (q, q_end) = other(ve, 1)?;
        if p == r || q == r || p == q {
            return None;
        }
        let (pt, qt) = (&self.b.traces[p], &self.b.traces[q]);
        if !self.trace_editable(pt) || !self.trace_editable(qt) {
            return None;
        }
        if pt.layer != qt.layer || pt.layer == tr.layer {
            return None;
        }
        if pt.net != tr.net || qt.net != tr.net || (pt.width - qt.width).abs() > 1e-4 {
            return None;
        }
        if drc::is_plane_layer(self.b, pt.layer) || pt.points.len() < 2 || qt.points.len() < 2 {
            return None;
        }
        if !drc::via_on_layer(&self.b.vias[vs], pt.layer) || !drc::via_on_layer(&self.b.vias[ve], pt.layer) {
            return None;
        }
        let (net, outer) = (tr.net, pt.layer);
        if self.has_plane(net) || self.via_on_pad(vs) || self.via_on_pad(ve) {
            return None;
        }
        Some(Run {
            r,
            vs,
            ve,
            p,
            p_end,
            q,
            q_end,
            outer,
        })
    }

    // ViaHops.cpp:330-393. ADAPTED cpp:331,338-379: callers carry the candidate's
    // rule row. Trace candidates use their layer; via discs use layer -1.
    // routingHoleClearance is rules.hole_clearance, already adapter-resolved.
    // ADAPTED cpp:348,366,1063-1070: layer filters geometry; clearance_layer is
    // separate so a candidate via uses the maximum clearance over its layers.
    fn build_checker(
        &self,
        layer: i32,
        clearance_layer: i32,
        net: i32,
        rule: u16,
        hw: f32,
        box_: Box2,
        ignore: &BTreeSet<i32>,
        push: bool,
    ) -> Checker {
        let mut ck = Checker {
            hw,
            slack: self.o.clearance_slack,
            segs: Vec::new(),
            soft: Vec::new(),
            discs: Vec::new(),
            pads: Vec::new(),
            kos: Vec::new(),
            edges: Vec::new(),
            edge_req: self.edge_clr,
            outline: self.b.outline.clone(),
            box_inside: true,
        };
        let hole_clr = self.b.rules.hole_clearance;
        let n_l = self.b.layers.len() as i32;
        let reach = std_max(self.b.rules.min_clearance, self.b.clr.max);
        let mut wb = box_;
        // ADAPTED cpp:341-342,353: checker query padding keeps the 2 mm window;
        // seg_box already includes the foreign trace width, so add candidate hw.
        wb.pad(std_max(2.0, reach + hw + ck.slack));
        for tr in &self.b.traces {
            if !tr.is_routed || tr.layer != layer || ignore.contains(&tr.id) {
                continue;
            }
            let same = tr.net == net;
            let pushable = push && !same && self.trace_editable(tr);
            let clr = self.b.clr.get(rule, tr.rule, clearance_layer);
            for p in tr.points.windows(2) {
                let (a, b) = (p[0].pos, p[1].pos);
                if !boxes_overlap(seg_box(a, b, tr.width), wb) {
                    continue;
                }
                if pushable {
                    ck.soft.push(SoftSeg {
                        a,
                        b,
                        hard: tr.width * 0.5 + 0.02,
                        full: tr.width * 0.5 + clr,
                        id: tr.id,
                    });
                } else {
                    ck.segs.push(CheckSeg {
                        a,
                        b,
                        req: tr.width * 0.5 + if same { 0.0 } else { clr },
                    });
                }
            }
        }
        for pad in &self.b.pads {
            if pad.net == net && net >= 0 {
                continue;
            }
            // ADAPTED cpp:363-364: checker pad cutoff keeps its half size plus
            // 1 mm, widening only when clearance, candidate hw and slack need it.
            let ext = std_max(pad.size.x, pad.size.y) * 0.5 + std_max(1.0, reach + hw + ck.slack);
            if pad.center.x + ext < wb.min.x
                || pad.center.x - ext > wb.max.x
                || pad.center.y + ext < wb.min.y
                || pad.center.y - ext > wb.max.y
            {
                continue;
            }
            if drc::pad_on_layer(pad, layer, n_l) {
                ck.pads.push(CheckPad {
                    p: pad.clone(),
                    req: self.b.clr.get(rule, pad.rule, clearance_layer),
                });
            }
            if !pad.is_smd && pad.drill_diameter > 0.0 {
                let de = pad.drill_extent();
                ck.discs.push(Disc {
                    c: pad.center,
                    req: std_max(de.x, de.y) * 0.5 + hole_clr,
                });
            }
        }
        for (v, via) in self.b.vias.iter().enumerate() {
            if self.via_dead[v] || via.net == net {
                continue;
            }
            // ADAPTED cpp:377-378: checker via cutoff keeps 1 mm or covers
            // the table reach, both copper radii and slack when larger.
            let ext = std_max(1.0, reach + hw + via.diameter * 0.5 + ck.slack);
            if via.pos.x < wb.min.x - ext || via.pos.x > wb.max.x + ext || via.pos.y < wb.min.y - ext || via.pos.y > wb.max.y + ext {
                continue;
            }
            if drc::via_on_layer(via, layer) {
                ck.discs.push(Disc {
                    c: via.pos,
                    req: via.diameter * 0.5 + self.b.clr.get(rule, via.rule, -1),
                });
            }
            ck.discs.push(Disc {
                c: via.pos,
                req: via.drill * 0.5 + hole_clr,
            });
        }
        if self.o.respect_keepouts {
            for k in &self.b.keepouts {
                if k.no_tracks && drc::keepout_on_layer(self.b, k, layer) {
                    ck.kos.push(k.clone());
                }
            }
        }
        let ol = &self.b.outline;
        if ol.len() >= 3 {
            let mut j = ol.len() - 1;
            for i in 0..ol.len() {
                if boxes_overlap(seg_box(ol[j], ol[i], ck.edge_req + hw + 0.5), wb) {
                    ck.edges.push((ol[j], ol[i]));
                }
                j = i;
            }
            ck.box_inside = point_in_poly(ol, box_.center());
        }
        ck
    }

    // ViaHops.cpp:395-523.
    fn search(&self, ck: &Checker, s: Vec2, g: Vec2, win: Box2, _push: bool, path: &mut Vec<Vec2>) -> bool {
        let mut pitch = self.o.grid_pitch;
        let sz = win.size();
        while (sz.x / pitch + 1.0) * (sz.y / pitch + 1.0) > 250000.0 {
            pitch *= 1.25;
        }
        let nx = (sz.x / pitch).ceil() as i32 + 1;
        let ny = (sz.y / pitch).ceil() as i32 + 1;
        let mut cell = vec![0u8; nx as usize * ny as usize];
        let infl = pitch * 0.75;
        let h = ck.hw + ck.slack + infl;
        let cc = |ix: i32, iy: i32| Vec2::new(win.min.x + ix as f32 * pitch, win.min.y + iy as f32 * pitch);
        let mut raster = |bb: Box2, blocked_at: &dyn Fn(Vec2) -> bool, val: u8| {
            let x0 = std_max(0, ((bb.min.x - win.min.x) / pitch).floor() as i32);
            let x1 = std_min(nx - 1, ((bb.max.x - win.min.x) / pitch).ceil() as i32);
            let y0 = std_max(0, ((bb.min.y - win.min.y) / pitch).floor() as i32);
            let y1 = std_min(ny - 1, ((bb.max.y - win.min.y) / pitch).ceil() as i32);
            for iy in y0..=y1 {
                for ix in x0..=x1 {
                    let c = &mut cell[(iy * nx + ix) as usize];
                    if *c >= val {
                        continue;
                    }
                    if blocked_at(cc(ix, iy)) {
                        *c = val;
                    }
                }
            }
        };
        for sg in &ck.segs {
            raster(seg_box(sg.a, sg.b, sg.req + h), &|c| point_seg(c, sg.a, sg.b) < sg.req + h, 2);
        }
        for sg in &ck.soft {
            raster(seg_box(sg.a, sg.b, sg.hard + h), &|c| point_seg(c, sg.a, sg.b) < sg.hard + h, 2);
            raster(seg_box(sg.a, sg.b, sg.full + h), &|c| point_seg(c, sg.a, sg.b) < sg.full + h, 1);
        }
        for d in &ck.discs {
            raster(seg_box(d.c, d.c, d.req + h), &|c| c.distance_to(d.c) < d.req + h, 2);
        }
        for p in &ck.pads {
            let ext = std_max(p.p.size.x, p.p.size.y) * 0.5 + p.req + h;
            raster(
                seg_box(p.p.center, p.p.center, ext),
                &|c| drc::point_pad_distance(c, &p.p) < p.req + h,
                2,
            );
        }
        for k in &ck.kos {
            let mut kb = Box2::default();
            for &v in &k.poly {
                kb.expand(v);
            }
            kb.pad(drc::keepout_margin(k) + h);
            if !boxes_overlap(kb, win) {
                continue;
            }
            raster(kb, &|c| drc::point_keepout_distance(k, c) < drc::keepout_margin(k) + h, 2);
        }
        for &(a, b) in &ck.edges {
            raster(seg_box(a, b, ck.edge_req + h), &|c| point_seg(c, a, b) < ck.edge_req + h, 2);
        }
        if !ck.edges.is_empty() {
            raster(win, &|c| !point_in_poly(&ck.outline, c), 2);
        } else if !ck.box_inside {
            return false;
        }
        let to_cell = |p: Vec2| {
            let ix = std_max(0, std_min(nx - 1, ((p.x - win.min.x) / pitch).round() as i32));
            let iy = std_max(0, std_min(ny - 1, ((p.y - win.min.y) / pitch).round() as i32));
            iy * nx + ix
        };
        let sc = to_cell(s);
        let gc = to_cell(g);
        cell[sc as usize] = 0;
        cell[gc as usize] = 0;
        let mut gs = vec![1e30f32; nx as usize * ny as usize];
        let mut prev = vec![-1; nx as usize * ny as usize];
        // ADAPTED cpp:457-458: priority_queue -> BinaryHeap, pop order including
        // ties is Qe::cmp. The heap is only pushed/popped, never iterated.
        let mut pq = BinaryHeap::new();
        let (gx, gy) = (gc % nx, gc / nx);
        let heur = |c: i32| {
            let dx = (c % nx - gx).abs() as f32 * pitch;
            let dy = (c / nx - gy).abs() as f32 * pitch;
            std_max(dx, dy) + 0.41421356f32 * std_min(dx, dy)
        };
        gs[sc as usize] = 0.0;
        pq.push(Qe(FloatOrd(heur(sc)), sc));
        const DX: [i32; 8] = [1, -1, 0, 0, 1, 1, -1, -1];
        const DY: [i32; 8] = [0, 0, 1, -1, 1, -1, 1, -1];
        let mut found = false;
        // cpp:468 has no budget check: finite grid, positive edge costs, and only
        // strictly improved scores are enqueued, so the search exhausts or finds g.
        while let Some(Qe(FloatOrd(f), c)) = pq.pop() {
            if f > gs[c as usize] + heur(c) + 1e-5 {
                continue;
            }
            if c == gc {
                found = true;
                break;
            }
            let (x, y) = (c % nx, c / nx);
            for k in 0..8 {
                let (xx, yy) = (x + DX[k], y + DY[k]);
                if xx < 0 || yy < 0 || xx >= nx || yy >= ny {
                    continue;
                }
                let n = yy * nx + xx;
                if cell[n as usize] == 2 {
                    continue;
                }
                if k >= 4 && (cell[(y * nx + xx) as usize] == 2 || cell[(yy * nx + x) as usize] == 2) {
                    continue;
                }
                let mut step = (if k >= 4 { 1.41421356f32 } else { 1.0 }) * pitch;
                if cell[n as usize] == 1 {
                    step *= 4.0;
                }
                let ng = gs[c as usize] + step;
                if ng < gs[n as usize] {
                    gs[n as usize] = ng;
                    prev[n as usize] = c;
                    pq.push(Qe(FloatOrd(ng + heur(n)), n));
                }
            }
        }
        if !found {
            return false;
        }
        let mut rev = Vec::new();
        let mut c = gc;
        while c >= 0 {
            rev.push(cc(c % nx, c / nx));
            c = prev[c as usize];
        }
        *path = rev.into_iter().rev().collect();
        *path.first_mut().unwrap() = s;
        *path.last_mut().unwrap() = g;
        true
    }

    // ViaHops.cpp:525-592.
    fn pull(&self, ck: &Checker, pts: &[Vec2], orig: &[bool], hard_soft: bool, out: &mut Vec<Vec2>) -> bool {
        out.clear();
        let n = pts.len();
        if n < 2 {
            return false;
        }
        let mut i = 0;
        out.push(pts[0]);
        while i < n - 1 {
            let mut best = None;
            let mut fails = 0;
            for j in i + 1..n {
                let ok = ck.seg_ok(pts[i], pts[j], hard_soft) || (j == i + 1 && orig[i] && orig[j]);
                if ok {
                    best = Some(j);
                    fails = 0;
                } else {
                    fails += 1;
                    if fails > 40 {
                        break;
                    }
                }
            }
            let Some(j) = best else {
                return false;
            };
            out.push(pts[j]);
            i = j;
        }
        true
    }
    fn particles(&self, c: &[Vec2]) -> Vec<Vec2> {
        let mut out = Vec::new();
        for k in 0..c.len().saturating_sub(1) {
            let len = c[k].distance_to(c[k + 1]);
            let mut ts = Vec::new();
            let steps = std_max(1, (len / std_max(0.05, self.o.node_spacing)).ceil() as i32);
            for s in 0..steps {
                ts.push(s as f32 / steps as f32);
            }
            let guard = std_min(self.o.corner_guard, len * 0.3);
            if len > 1e-4 {
                ts.push(guard / len);
                ts.push(1.0 - guard / len);
            }
            // cpp:572: strict numeric <; equal t values remain ties (unstable).
            ts.sort_unstable_by(|a, b| FloatOrd(*a).cmp(&FloatOrd(*b)));
            let mut last_t = -1.0;
            for t in ts {
                if t >= 1.0 - 1e-5 || (last_t >= 0.0 && (t - last_t) * len < 0.05) {
                    continue;
                }
                if !(k == 0 && t < 1e-6) {
                    out.push(Vec2::lerp(c[k], c[k + 1], t));
                }
                last_t = t;
            }
        }
        out
    }
    fn targeted_nodes(&self, trace_id: i32) -> BTreeSet<i32> {
        // ADAPTED cpp:585-591: set -> BTreeSet, used only for membership.
        let mut s = BTreeSet::new();
        if let Some(refs) = self.branch_refs.get(&trace_id) {
            for &(t, k) in refs {
                s.insert(self.b.traces[t].points[k].branch_point);
            }
        }
        s
    }

    // ViaHops.cpp:594-774. ADAPTED: local Trace snapshots end immutable borrows
    // before applying edits; these are the same pre-edit values as the C++ refs.
    fn apply_bridge(&mut self, run: Run, bridge: &[Vec2], ck: &Checker, hard_soft: bool, edit: &mut Edit, delta: &mut f64) -> bool {
        let pt = self.b.traces[run.p].clone();
        let qt = self.b.traces[run.q].clone();
        let rt = self.b.traces[run.r].clone();
        let (n_p, n_q) = (pt.points.len() as i32, qt.points.len() as i32);
        let pp = if run.p_end == 0 { reversed(&pt.points) } else { pt.points.clone() };
        let qq = if run.q_end == 1 { reversed(&qt.points) } else { qt.points.clone() };
        let p_orig = |i| if run.p_end == 0 { n_p - 1 - i } else { i };
        let q_orig = |j| if run.q_end == 1 { n_q - 1 - j } else { j };
        let p_t = self.targeted_nodes(pt.id);
        let q_t = self.targeted_nodes(qt.id);
        let anchor_p = |i: i32| {
            let t = &pp[i as usize];
            t.is_fixed
                || t.terminal_pad >= 0
                || t.attached_via >= 0
                || t.branch_trace >= 0
                || t.straight_before
                || (i + 1 < n_p && pp[(i + 1) as usize].straight_before)
                || p_t.contains(&p_orig(i))
        };
        let anchor_q = |j: i32| {
            let t = &qq[j as usize];
            t.is_fixed
                || t.terminal_pad >= 0
                || t.attached_via >= 0
                || t.branch_trace >= 0
                || t.straight_before
                || (j + 1 < n_q && qq[(j + 1) as usize].straight_before)
                || q_t.contains(&q_orig(j))
        };
        let mut k = 0;
        let mut acc = 0.0;
        for i in (0..=n_p - 2).rev() {
            acc += pp[i as usize].pos.distance_to(pp[(i + 1) as usize].pos);
            if i == 0 || anchor_p(i) || acc > self.o.pull_window {
                k = i;
                break;
            }
        }
        let mut m = n_q - 1;
        acc = 0.0;
        for j in 1..n_q {
            acc += qq[j as usize].pos.distance_to(qq[(j - 1) as usize].pos);
            if j == n_q - 1 || anchor_q(j) || acc > self.o.pull_window {
                m = j;
                break;
            }
        }
        let mut seq = Vec::new();
        let mut orig = Vec::new();
        for i in k..=n_p - 2 {
            seq.push(pp[i as usize].pos);
            orig.push(true);
        }
        for (b, &p) in bridge.iter().enumerate() {
            seq.push(p);
            orig.push(b == 0 || b + 1 == bridge.len());
        }
        for j in 1..=m {
            seq.push(qq[j as usize].pos);
            orig.push(true);
        }
        let mut pulled = Vec::new();
        if !self.pull(ck, &seq, &orig, hard_soft, &mut pulled) {
            return false;
        }
        let inner = self.particles(&pulled);
        let mut np = pp[..=k as usize].to_vec();
        for &v in &inner {
            np.push(TracePoint {
                pos: v,
                width: pt.width,
                ..TracePoint::default()
            });
        }
        let q_off = np.len() as i32 - m;
        np.extend_from_slice(&qq[m as usize..]);
        for t in &mut np {
            t.velocity = Vec2::default();
        }
        let (a0, a1) = (k, k + inner.len() as i32 + 1);
        for i in a0..a1 {
            if i + 1 >= np.len() as i32 {
                break;
            }
            for j in 0..np.len() as i32 - 1 {
                if (i - j).abs() <= 1 {
                    continue;
                }
                if segs_cross(
                    np[i as usize].pos,
                    np[(i + 1) as usize].pos,
                    np[j as usize].pos,
                    np[(j + 1) as usize].pos,
                ) {
                    return false;
                }
            }
        }
        *delta = poly_len(&np) - (poly_len(&pt.points) + poly_len(&rt.points) + poly_len(&qt.points));
        if *delta > self.o.max_extra_length as f64 {
            self.s.length_rejects += 1;
            return false;
        }
        let mut fixes = Vec::new();
        for which in 0..2 {
            let id = if which == 0 { pt.id } else { qt.id };
            let Some(refs) = self.branch_refs.get(&id) else {
                continue;
            };
            for &(t, kk) in refs {
                let oi = self.b.traces[t].points[kk].branch_point;
                let mut ni = -1;
                if which == 0 {
                    let oriented = if run.p_end == 0 { n_p - 1 - oi } else { oi };
                    if oriented >= 0 && oriented <= k {
                        ni = oriented;
                    }
                } else {
                    let oriented = if run.q_end == 1 { n_q - 1 - oi } else { oi };
                    if oriented >= m && oriented < n_q {
                        ni = q_off + oriented;
                    }
                }
                if ni < 0 || t == run.r {
                    return false;
                }
                fixes.push((t, kk, pt.id, ni));
            }
        }
        *edit = Edit::default();
        // ADAPTED cpp:606,727-729: p_t/q_t are membership-only. snap is a
        // BTreeSet, iterated ascending here to retain std::set snapshot order.
        let mut snap = BTreeSet::from([pt.id, qt.id, rt.id]);
        for &(t, _, _, _) in &fixes {
            snap.insert(self.b.traces[t].id);
        }
        for id in snap {
            edit.before.push(self.b.traces[self.id_to_idx[&id]].clone());
        }
        edit.after_ids.push(pt.id);
        for &(t, _, _, _) in &fixes {
            if self.b.traces[t].id != pt.id && self.b.traces[t].id != qt.id {
                edit.after_ids.push(self.b.traces[t].id);
            }
        }
        edit.vias = vec![run.vs, run.ve];
        edit.nets.insert(pt.net);
        for &v in &pulled {
            edit.box_.expand(v);
        }
        edit.box_.expand(self.b.vias[run.vs].pos);
        edit.box_.expand(self.b.vias[run.ve].pos);
        edit.box_.pad(1.0);
        for &(t, k, new_id, new_idx) in &fixes {
            let tp = &mut self.b.traces[t].points[k];
            tp.branch_trace = new_id;
            tp.branch_point = new_idx;
        }
        let mut merged = self.b.traces[run.p].clone();
        merged.points = np;
        clear_geometry(&mut merged);
        merged.length = poly_len(&merged.points) as f32;
        self.b.traces[run.p] = merged;
        let mut er = [run.r, run.q];
        er.sort_unstable_by(|a, b| b.cmp(a));
        for e in er {
            self.b.traces.remove(e);
        }
        self.via_dead[run.vs] = true;
        self.via_dead[run.ve] = true;
        self.rebuild();
        true
    }
    fn run_ignore(&self, run: Run) -> BTreeSet<i32> {
        // ADAPTED cpp:763-765: membership-only BTreeSet, extended by try_bridge.
        BTreeSet::from([self.b.traces[run.p].id, self.b.traces[run.q].id, self.b.traces[run.r].id])
    }
    fn run_window(&self, run: Run) -> Box2 {
        let mut w = Box2::default();
        for p in &self.b.traces[run.r].points {
            w.expand(p.pos);
        }
        w.pad(self.o.detour_radius);
        w
    }

    // ViaHops.cpp:776-967.
    fn try_bridge(&mut self, run: Run, push: bool, extra_ignore: &BTreeSet<i32>, edit: &mut Edit, delta: &mut f64) -> bool {
        self.s.attempts += 1;
        let pt = &self.b.traces[run.p];
        let merged_id = pt.id;
        let mut ign = self.run_ignore(run);
        // ADAPTED cpp:781-782: extra_ignore iterated ascending into ign.
        ign.extend(extra_ignore);
        let win = self.run_window(run);
        let ck = self.build_checker(run.outer, run.outer, pt.net, pt.rule, pt.width * 0.5, win, &ign, push);
        if push && ck.soft.is_empty() {
            return false;
        }
        let mut path = Vec::new();
        if !self.search(&ck, self.b.vias[run.vs].pos, self.b.vias[run.ve].pos, win, push, &mut path) {
            self.s.search_fails += 1;
            return false;
        }
        if !push {
            if !self.apply_bridge(run, &path, &ck, false, edit, delta) {
                self.s.check_rejects += 1;
                return false;
            }
            return true;
        }
        let saved_traces = self.b.traces.clone();
        let saved_dead = self.via_dead.clone();
        // ADAPTED cpp:812-813,841: soft_ids BTreeSet is iterated ascending.
        let soft_ids: BTreeSet<_> = ck.soft.iter().map(|s| s.id).collect();
        if !self.apply_bridge(run, &path, &ck, true, edit, delta) {
            self.s.check_rejects += 1;
            return false;
        }
        let merged = &self.b.traces[self.id_to_idx[&merged_id]];
        let hw_m = merged.width * 0.5;
        let msegs: Vec<_> = merged.points.windows(2).map(|p| (p[0].pos, p[1].pos)).collect();
        // ADAPTED cpp:824-837: return (distance, nearest) instead of an output reference.
        let dist_m = |p: Vec2| {
            let mut best = 1e30;
            let mut nearest = Vec2::default();
            for &(a, b) in &msegs {
                let c = closest_on_seg(p, a, b);
                let d = p.distance_to(c);
                if d < best {
                    best = d;
                    nearest = c;
                }
            }
            (best, nearest)
        };
        // ADAPTED cpp:839: conservative query/push reach is max(min_clearance, clr.max).
        let clr = std_max(self.b.rules.min_clearance, self.b.clr.max);
        let mut any_pushed = false;
        for sid in soft_ids {
            let Some(&ti) = self.id_to_idx.get(&sid) else {
                continue;
            };
            let tr = self.b.traces[ti].clone();
            let need = hw_m + tr.width * 0.5 + clr + 3.0 * self.o.clearance_slack;
            let mut close = false;
            for p in tr.points.windows(2) {
                if close {
                    break;
                }
                for &(a, b) in &msegs {
                    if drc::segment_segment_distance(p[0].pos, p[1].pos, a, b) < need {
                        close = true;
                        break;
                    }
                }
            }
            if !close {
                continue;
            }
            let tgt = self.targeted_nodes(tr.id);
            let mut np = Vec::new();
            let mut map = vec![-1; tr.points.len()];
            for i in 0..tr.points.len() {
                map[i] = np.len() as i32;
                np.push(tr.points[i].clone());
                if i + 1 == tr.points.len() {
                    break;
                }
                let (a, b) = (tr.points[i].pos, tr.points[i + 1].pos);
                if tr.points[i + 1].straight_before {
                    continue;
                }
                let mut near_seg = false;
                for &(c, d) in &msegs {
                    if drc::segment_segment_distance(a, b, c, d) < need + self.o.max_push {
                        near_seg = true;
                        break;
                    }
                }
                if !near_seg {
                    continue;
                }
                let pieces = (a.distance_to(b) / 0.25).ceil() as i32;
                for s in 1..pieces {
                    np.push(TracePoint {
                        pos: Vec2::lerp(a, b, s as f32 / pieces as f32),
                        width: tr.points[i].width,
                        ..TracePoint::default()
                    });
                }
            }
            let mut anchor = vec![false; np.len()];
            for (i, t) in tr.points.iter().enumerate() {
                if t.is_fixed
                    || t.terminal_pad >= 0
                    || t.attached_via >= 0
                    || t.branch_trace >= 0
                    || t.straight_before
                    || (i + 1 < tr.points.len() && tr.points[i + 1].straight_before)
                    || tgt.contains(&(i as i32))
                    || i == 0
                    || i + 1 == tr.points.len()
                {
                    anchor[map[i] as usize] = true;
                }
            }
            for i in 0..np.len() {
                let (d, nearest) = dist_m(np[i].pos);
                if d >= need {
                    continue;
                }
                let mv = need - d;
                if anchor[i] || mv > self.o.max_push || d < 1e-4 {
                    self.restore_traces(saved_traces, saved_dead);
                    return false;
                }
                let pos = np[i].pos;
                np[i].pos += (pos - nearest) / d * mv;
            }
            if let Some(refs) = self.branch_refs.get(&tr.id) {
                for &(t, kk) in refs {
                    let tp = &mut self.b.traces[t].points[kk];
                    if tp.branch_point >= 0 && (tp.branch_point as usize) < map.len() {
                        tp.branch_point = map[tp.branch_point as usize];
                    }
                    let id = self.b.traces[t].id;
                    if !edit.after_ids.contains(&id) {
                        if !edit.before.iter().any(|bt| bt.id == id) {
                            for st in &saved_traces {
                                if st.id == id {
                                    edit.before.push(st.clone());
                                }
                            }
                        }
                        edit.after_ids.push(id);
                    }
                }
            }
            if !edit.before.iter().any(|bt| bt.id == tr.id) {
                for st in &saved_traces {
                    if st.id == tr.id {
                        edit.before.push(st.clone());
                    }
                }
            }
            if !edit.after_ids.contains(&tr.id) {
                edit.after_ids.push(tr.id);
            }
            let target = &mut self.b.traces[ti];
            target.points = np;
            clear_geometry(target);
            target.length = poly_len(&target.points) as f32;
            for p in &target.points {
                edit.box_.expand(p.pos);
            }
            edit.nets.insert(target.net);
            any_pushed = true;
        }
        if !any_pushed {
            self.restore_traces(saved_traces, saved_dead);
            return false;
        }
        self.rebuild();
        // ADAPTED cpp:941-957: old_segs BTreeSet is membership-only; FloatOrd
        // preserves nested-pair numeric lexicographic ordering, including ties.
        let seg_key = |a: Vec2, b: Vec2| (FloatOrd(a.x), FloatOrd(a.y), FloatOrd(b.x), FloatOrd(b.y));
        let mut old_segs = BTreeSet::new();
        for bt in &edit.before {
            for p in bt.points.windows(2) {
                old_segs.insert(seg_key(p[0].pos, p[1].pos));
            }
        }
        for &id in &edit.after_ids {
            let Some(&ti) = self.id_to_idx.get(&id) else {
                self.restore_traces(saved_traces, saved_dead);
                return false;
            };
            let tr = &self.b.traces[ti];
            let mut tb = Box2::default();
            for p in &tr.points {
                tb.expand(p.pos);
            }
            let c2 = self.build_checker(tr.layer, tr.layer, tr.net, tr.rule, tr.width * 0.5, tb, &BTreeSet::from([tr.id]), false);
            for p in tr.points.windows(2) {
                let (a, b) = (p[0].pos, p[1].pos);
                if old_segs.contains(&seg_key(a, b)) || old_segs.contains(&seg_key(b, a)) {
                    continue;
                }
                if !c2.seg_ok(a, b, false) {
                    self.s.check_rejects += 1;
                    self.restore_traces(saved_traces, saved_dead);
                    return false;
                }
            }
        }
        true
    }
    // ADAPTED cpp:806-810,1171-1175,2297-2299,2329-2331: extract the repeated restore lambda.
    fn restore_traces(&mut self, traces: Vec<Trace>, dead: Vec<bool>) {
        self.b.traces = traces;
        self.via_dead = dead;
        self.rebuild();
    }

    // ViaHops.cpp:969-1054.
    fn try_relayer(&mut self, kid: i32, y: i32, edit: &mut Edit) -> bool {
        let Some(&idx) = self.id_to_idx.get(&kid) else {
            return false;
        };
        let kt = self.b.traces[idx].clone();
        if !self.trace_editable(&kt) || kt.points.len() < 2 || kt.layer == y || self.branch_refs.contains_key(&kt.id) {
            return false;
        }
        if drc::is_plane_layer(self.b, y) {
            return false;
        }
        let n_l = self.b.layers.len() as i32;
        for (i, p) in kt.points.iter().enumerate() {
            if p.branch_trace >= 0 {
                return false;
            }
            let end = i == 0 || i + 1 == kt.points.len();
            if !end && (p.attached_via >= 0 || p.terminal_pad >= 0) {
                return false;
            }
            if !end {
                continue;
            }
            if p.terminal_pad >= 0 {
                if p.terminal_pad as usize >= self.b.pads.len() || !drc::pad_on_layer(&self.b.pads[p.terminal_pad as usize], y, n_l) {
                    return false;
                }
            } else if p.attached_via >= 0 {
                if p.attached_via as usize >= self.b.vias.len() || self.via_dead[p.attached_via as usize] {
                    return false;
                }
                if !drc::via_on_layer(&self.b.vias[p.attached_via as usize], y) {
                    return false;
                }
            } else {
                return false;
            }
        }
        self.s.attempts += 1;
        let mut win = Box2::default();
        for p in &kt.points {
            win.expand(p.pos);
        }
        win.pad(self.o.detour_radius);
        let ck = self.build_checker(y, y, kt.net, kt.rule, kt.width * 0.5, win, &BTreeSet::from([kt.id]), false);
        let mut path = Vec::new();
        let mut pulled = Vec::new();
        if !self.search(&ck, kt.points[0].pos, kt.points.last().unwrap().pos, win, false, &mut path) {
            self.s.search_fails += 1;
            return false;
        }
        if !self.pull(&ck, &path, &vec![false; path.len()], false, &mut pulled) {
            self.s.check_rejects += 1;
            return false;
        }
        let mut new_len = 0.0f64;
        for p in pulled.windows(2) {
            new_len += p[0].distance_to(p[1]) as f64;
        }
        if new_len > poly_len(&kt.points) + self.o.max_extra_length as f64 {
            self.s.length_rejects += 1;
            return false;
        }
        *edit = Edit::default();
        edit.before.push(kt.clone());
        edit.after_ids.push(kt.id);
        edit.nets.insert(kt.net);
        for p in &kt.points {
            edit.box_.expand(p.pos);
        }
        for &p in &pulled {
            edit.box_.expand(p);
        }
        edit.box_.pad(1.0);
        let mut nk = kt.clone();
        nk.layer = y;
        // ADAPTED cpp:1032: OBoard Trace::layer is the stack index; no redundant KiCad layer mask.
        nk.points.clear();
        let mut a = kt.points[0].clone();
        let mut b = kt.points.last().unwrap().clone();
        a.straight_before = false;
        b.straight_before = false;
        a.velocity = Vec2::default();
        b.velocity = Vec2::default();
        nk.points.push(a);
        for v in self.particles(&pulled) {
            nk.points.push(TracePoint {
                pos: v,
                width: kt.width,
                ..TracePoint::default()
            });
        }
        nk.points.push(b);
        clear_geometry(&mut nk);
        nk.length = poly_len(&nk.points) as f32;
        self.b.traces[idx] = nk;
        self.rebuild();
        true
    }

    // ViaHops.cpp:1056-1104. proto supplies the candidate rule row.
    fn via_fits(&self, proto: &Via, p: Vec2, net: i32, ignore: &BTreeSet<i32>) -> bool {
        let n_l = self.b.layers.len() as i32;
        let mut bx = Box2::default();
        bx.expand(p);
        bx.pad(1.0);
        for l in 0..n_l {
            if !drc::via_on_layer(proto, l) || drc::is_plane_layer(self.b, l) {
                continue;
            }
            // ADAPTED cpp:1063-1070: l selects spanned geometry; the candidate
            // via disc checks foreign copper with the all-layer clearance (-1).
            let mut ck = self.build_checker(l, -1, net, proto.rule, proto.diameter * 0.5, bx, ignore, false);
            ck.kos.clear();
            if !ck.seg_ok(p, p, false) {
                return false;
            }
        }
        let hole_clr = std_max(self.b.rules.hole_clearance, effective_hole_to_hole(self.b));
        for (v, o) in self.b.vias.iter().enumerate() {
            if self.via_dead[v] {
                continue;
            }
            if o.pos.distance_to(p) < (o.drill + proto.drill) * 0.5 + hole_clr + self.o.clearance_slack {
                return false;
            }
            if o.net != net && o.pos.distance_to(p) < o.diameter * 0.5 + proto.drill * 0.5 + hole_clr + self.o.clearance_slack {
                return false;
            }
        }
        for pad in &self.b.pads {
            if (pad.center.x - p.x).abs() > 5.0 || (pad.center.y - p.y).abs() > 5.0 {
                continue;
            }
            if pad.net != net && drc::point_pad_distance(p, pad) < proto.drill * 0.5 + self.b.rules.hole_clearance + self.o.clearance_slack
            {
                return false;
            }
            if !pad.is_smd
                && pad.drill_diameter > 0.0
                && pad.center.distance_to(p)
                    < (std_max(pad.drill_extent().x, pad.drill_extent().y) + proto.drill) * 0.5 + hole_clr + self.o.clearance_slack
            {
                return false;
            }
        }
        for k in &self.b.keepouts {
            if !k.no_vias || k.poly.len() < 3 {
                continue;
            }
            let mut on = false;
            for l in 0..n_l {
                if drc::via_on_layer(proto, l) && drc::keepout_on_layer(self.b, k, l) {
                    on = true;
                }
            }
            if on && drc::point_keepout_distance(k, p) < proto.diameter * 0.5 + drc::keepout_margin(k) + self.o.clearance_slack {
                return false;
            }
        }
        if self.b.outline.len() >= 3 {
            if !point_in_poly(&self.b.outline, p) {
                return false;
            }
            let mut j = self.b.outline.len() - 1;
            for i in 0..self.b.outline.len() {
                if point_seg(p, self.b.outline[j], self.b.outline[i]) < self.edge_clr + proto.diameter * 0.5 + self.o.clearance_slack {
                    return false;
                }
                j = i;
            }
        }
        true
    }

    // ViaHops.cpp:1106-1361.
    fn try_slide(&mut self, run: Run, kid: i32, out: &mut Edit, delta: &mut f64) -> bool {
        let y = self.b.traces[run.r].layer;
        let mid = (self.b.vias[run.vs].pos + self.b.vias[run.ve].pos) * 0.5;
        let r_id = self.b.traces[run.r].id;
        for end in 0..2 {
            let Some(&ki) = self.id_to_idx.get(&kid) else {
                return false;
            };
            let Some(&ri) = self.id_to_idx.get(&r_id) else {
                return false;
            };
            let Some(cur) = self.find_run(ri, self.o.max_hop_length) else {
                return false;
            };
            let kt = self.b.traces[ki].clone();
            if !self.trace_editable(&kt) || self.branch_refs.contains_key(&kt.id) || kt.points.len() < 3 {
                return false;
            }
            let ep = if end == 0 { &kt.points[0] } else { kt.points.last().unwrap() };
            let w = ep.attached_via;
            if w < 0 || w as usize >= self.b.vias.len() {
                continue;
            }
            let w = w as usize;
            if !self.via_editable(w) || self.via_on_pad(w) || self.has_plane(kt.net) {
                continue;
            }
            if self.via_interior[w] != 0 || self.via_ends[w].len() != 2 {
                continue;
            }
            if self.b.vias[w].pos.distance_to(mid) > self.o.slide_reach {
                continue;
            }
            let mut other = None;
            for &(t, e) in &self.via_ends[w] {
                if t != ki {
                    other = Some((t, e));
                }
            }
            let Some((ti, te)) = other else {
                continue;
            };
            let tt = self.b.traces[ti].clone();
            if !self.trace_editable(&tt) || tt.layer != y || self.branch_refs.contains_key(&tt.id) || tt.net != kt.net {
                continue;
            }
            let mut br = false;
            for p in &tt.points {
                br |= p.branch_trace >= 0;
            }
            for p in &kt.points {
                br |= p.branch_trace >= 0;
            }
            if br {
                return false;
            }
            let (t_id, k_id) = (tt.id, kt.id);
            self.s.attempts += 1;
            let saved = self.b.traces.clone();
            let saved_dead = self.via_dead.clone();
            let saved_vias = self.b.vias.len();
            let mut e1 = Edit::default();
            let mut d1 = 0.0;
            self.via_dead[w] = true;
            if !self.try_bridge(cur, false, &BTreeSet::from([k_id]), &mut e1, &mut d1) {
                self.via_dead[w] = false;
                continue;
            }
            let merged = &self.b.traces[self.id_to_idx[&e1.after_ids[0]]];
            let msegs: Vec<_> = merged.points.windows(2).map(|p| (p[0].pos, p[1].pos)).collect();
            let hw_m = merged.width * 0.5;
            let k2 = &self.b.traces[self.id_to_idx[&k_id]];
            let kp = if end == 0 { k2.points.clone() } else { reversed(&k2.points) };
            let (kw, knet) = (k2.width, k2.net);
            // ADAPTED cpp:1200: conservative clearance reach uses the table maximum.
            let clr = std_max(self.b.rules.min_clearance, self.b.clr.max);
            let req = hw_m + kw * 0.5 + clr + 2.0 * self.o.clearance_slack;
            let seg_clear_of_m = |a, b| {
                for &(c, d) in &msegs {
                    if drc::segment_segment_distance(a, b, c, d) < req {
                        return false;
                    }
                }
                true
            };
            let nk = kp.len();
            let mut suf_ok = vec![true; nk + 1];
            for i in (0..nk - 1).rev() {
                suf_ok[i] = suf_ok[i + 1] && seg_clear_of_m(kp[i].pos, kp[i + 1].pos);
            }
            let proto = self.b.vias[w].clone();
            let mut spots = Vec::new();
            let mut along = 0.0;
            let along_cap = self.o.slide_reach + self.o.detour_radius;
            for i in 1..nk - 1 {
                if spots.len() as i32 >= self.o.slide_spots {
                    break;
                }
                along += kp[i - 1].pos.distance_to(kp[i].pos);
                if along > along_cap {
                    break;
                }
                let t = &kp[i];
                if t.is_fixed || t.terminal_pad >= 0 || t.attached_via >= 0 || t.straight_before || kp[i + 1].straight_before {
                    break;
                }
                if !suf_ok[i] {
                    continue;
                }
                let mut v_ok = true;
                for &(a, b) in &msegs {
                    if point_seg(t.pos, a, b) < hw_m + proto.diameter * 0.5 + clr + self.o.clearance_slack {
                        v_ok = false;
                    }
                }
                if !v_ok || !self.via_fits(&proto, t.pos, knet, &BTreeSet::from([k_id, t_id])) {
                    continue;
                }
                spots.push(i);
            }
            let mut j = None;
            let mut pulled = Vec::new();
            let mut ext = 0.0;
            let mut dropped = 0.0;
            for sj in spots {
                let sp = kp[sj].pos;
                let mut win = Box2::default();
                win.expand(proto.pos);
                win.expand(sp);
                win.pad(self.o.detour_radius);
                let t2 = &self.b.traces[self.id_to_idx[&t_id]];
                let ck = self.build_checker(y, y, t2.net, t2.rule, t2.width * 0.5, win, &BTreeSet::from([t_id, k_id]), false);
                let mut path = Vec::new();
                if !self.search(&ck, proto.pos, sp, win, false, &mut path) {
                    self.s.search_fails += 1;
                    continue;
                }
                pulled.clear();
                if !self.pull(&ck, &path, &vec![false; path.len()], false, &mut pulled) {
                    self.s.check_rejects += 1;
                    continue;
                }
                let mut e = 0.0f64;
                let mut d = 0.0f64;
                for p in pulled.windows(2) {
                    e += p[0].distance_to(p[1]) as f64;
                }
                for i in 0..sj {
                    d += kp[i].pos.distance_to(kp[i + 1].pos) as f64;
                }
                if d1 + e - d > self.o.max_extra_length as f64 {
                    self.s.length_rejects += 1;
                    continue;
                }
                j = Some(sj);
                ext = e;
                dropped = d;
                break;
            }
            let Some(j) = j else {
                self.b.vias.truncate(saved_vias);
                self.restore_traces(saved, saved_dead);
                continue;
            };
            let nv = kp[j].pos;
            let mut e2 = Edit::default();
            e2.before.push(self.b.traces[self.id_to_idx[&t_id]].clone());
            e2.before.push(self.b.traces[self.id_to_idx[&k_id]].clone());
            e2.after_ids = vec![t_id, k_id];
            e2.vias = vec![w];
            e2.nets.insert(knet);
            let mut v = proto;
            v.id = self.b.vias.len() as i32;
            v.pos = nv;
            v.velocity = Vec2::default();
            v.connected_pad = -1;
            let v_id = v.id;
            self.b.vias.push(v);
            self.via_dead.push(false);
            e2.new_vias = vec![v_id as usize];
            let inner = self.particles(&pulled);
            {
                let tm = &mut self.b.traces[self.id_to_idx[&t_id]];
                let mut tp = if te == 1 { tm.points.clone() } else { reversed(&tm.points) };
                tp.last_mut().unwrap().attached_via = -1;
                for q in inner {
                    tp.push(TracePoint {
                        pos: q,
                        width: tm.width,
                        ..TracePoint::default()
                    });
                }
                tp.push(TracePoint {
                    pos: nv,
                    width: tm.width,
                    attached_via: v_id,
                    ..TracePoint::default()
                });
                tm.points = if te == 1 { tp } else { reversed(&tp) };
                clear_geometry(tm);
                tm.length = poly_len(&tm.points) as f32;
                for p in &tm.points {
                    e2.box_.expand(p.pos);
                }
            }
            {
                let km = &mut self.b.traces[self.id_to_idx[&k_id]];
                let mut np = kp[j..].to_vec();
                np[0].attached_via = v_id;
                np[0].is_fixed = false;
                np[0].straight_before = false;
                km.points = if end == 0 { np } else { reversed(&np) };
                clear_geometry(km);
                km.length = poly_len(&km.points) as f32;
                for p in &km.points {
                    e2.box_.expand(p.pos);
                }
            }
            e2.box_.pad(1.0);
            self.via_dead[w] = true;
            self.rebuild();
            e1.absorb(e2);
            *out = e1;
            *delta = d1 + ext - dropped;
            return true;
        }
        false
    }

    // ViaHops.cpp:1363-1419.
    fn classify(&self, run: Run, crossed_traces: &mut BTreeSet<i32>) -> HopClass {
        let rt = &self.b.traces[run.r];
        let hw = rt.width * 0.5;
        let n_l = self.b.layers.len() as i32;
        let reach = std_max(self.b.rules.min_clearance, self.b.clr.max);
        let slack = self.o.clearance_slack;
        // ADAPTED cpp:1369: tids/nets -> BTreeSet; nets is never iterated. tids
        // transfers to crossed_traces, iterated by relayer and singleton lookups.
        let mut tids = BTreeSet::new();
        let mut nets = BTreeSet::new();
        let mut pads_vias = 0;
        let mut rb = Box2::default();
        for p in &rt.points {
            rb.expand(p.pos);
        }
        let unpadded_rb = rb;
        // ADAPTED cpp:1371-1373: classifier query padding keeps 2 mm or covers
        // the table reach, candidate half width and slack when larger.
        rb.pad(std_max(2.0, reach + hw + slack));
        for tr in &self.b.traces {
            if !tr.is_routed || tr.layer != run.outer || tr.net == rt.net {
                continue;
            }
            // ADAPTED cpp:1373,1380-1383: classifier trace cutoff compares bare
            // centerlines, so its 2 mm padding must cover both copper half widths.
            let mut tb = unpadded_rb;
            tb.pad(std_max(2.0, reach + hw + tr.width * 0.5 + slack));
            let mut hit = false;
            for p in tr.points.windows(2) {
                if hit {
                    break;
                }
                if !boxes_overlap(seg_box(p[0].pos, p[1].pos, 0.0), tb) {
                    continue;
                }
                for q in rt.points.windows(2) {
                    if hit {
                        break;
                    }
                    // ADAPTED cpp:1383,1397,1408: item rule rows replace ncl/pad policy.
                    if drc::segment_segment_distance(p[0].pos, p[1].pos, q[0].pos, q[1].pos)
                        < hw + tr.width * 0.5 + self.b.clr.get(rt.rule, tr.rule, run.outer)
                    {
                        hit = true;
                    }
                }
            }
            if hit {
                tids.insert(tr.id);
                nets.insert(tr.net);
            }
        }
        for pad in &self.b.pads {
            if pad.net == rt.net || !drc::pad_on_layer(pad, run.outer, n_l) {
                continue;
            }
            // ADAPTED cpp:1395-1397: classifier pad cutoff keeps 5 mm or covers
            // the table reach, pad half size, candidate half width and slack.
            let ext = std_max(5.0, reach + hw + std_max(pad.size.x, pad.size.y) * 0.5 + slack);
            if pad.center.x < rb.min.x - ext
                || pad.center.x > rb.max.x + ext
                || pad.center.y < rb.min.y - ext
                || pad.center.y > rb.max.y + ext
            {
                continue;
            }
            for q in rt.points.windows(2) {
                if drc::segment_pad_distance(q[0].pos, q[1].pos, pad) < hw + self.b.clr.get(rt.rule, pad.rule, run.outer) {
                    pads_vias += 1;
                    break;
                }
            }
        }
        for (v, via) in self.b.vias.iter().enumerate() {
            if self.via_dead[v] || via.net == rt.net {
                continue;
            }
            for q in rt.points.windows(2) {
                if point_seg(via.pos, q[0].pos, q[1].pos) < hw + via.diameter * 0.5 + self.b.clr.get(rt.rule, via.rule, -1) {
                    pads_vias += 1;
                    break;
                }
            }
        }
        *crossed_traces = tids;
        if crossed_traces.is_empty() && pads_vias == 0 {
            return HopClass::Empty;
        }
        if crossed_traces.is_empty() {
            return HopClass::PadOrVia;
        }
        if nets.len() == 1 && pads_vias == 0 {
            return HopClass::OneTrace;
        }
        HopClass::Multiple
    }

    // ViaHops.cpp:1435-1489. ADAPTED cpp:1430,1639: success-only structs returned
    // as Option rather than output parameters with invalid index sentinels.
    fn find_spike(&self, r: usize) -> Option<Spike> {
        let rt = self.b.traces.get(r)?;
        if !self.trace_editable(rt) || rt.points.len() < 2 || self.pg.member(rt.net) {
            return None;
        }
        let va = rt.points[0].attached_via;
        let vb = rt.points.last()?.attached_via;
        if va < 0 || vb < 0 || va as usize >= self.b.vias.len() || vb as usize >= self.b.vias.len() || va == vb {
            return None;
        }
        let (va, vb) = (va as usize, vb as usize);
        if !self.via_editable(va) || !self.via_editable(vb) {
            return None;
        }
        if self.b.vias[va].net != rt.net || self.b.vias[vb].net != rt.net || self.via_interior[va] != 0 || self.via_interior[vb] != 0 {
            return None;
        }
        if self.branch_refs.contains_key(&rt.id) {
            return None;
        }
        for p in &rt.points {
            if p.branch_trace >= 0 || p.terminal_pad >= 0 {
                return None;
            }
        }
        let d = self.b.vias[va].pos.distance_to(self.b.vias[vb].pos);
        if d > self.o.spike_max_dist {
            return None;
        }
        let len = poly_len(&rt.points) as f32;
        let mut q: Vec<Vec2> = Vec::new();
        for p in &rt.points {
            if q.is_empty() || p.pos.distance_to(*q.last().unwrap()) > 1e-4 {
                q.push(p.pos);
            }
        }
        let mut max_turn = 0.0;
        for i in 1..q.len().saturating_sub(1) {
            let a = q[i] - q[i - 1];
            let b = q[i + 1] - q[i];
            let t = a.cross(b).atan2(a.dot(b));
            max_turn = std_max(max_turn, t.abs() * 180.0 / 3.14159265f32);
        }
        if !(max_turn > self.o.spike_min_turn_deg || len > self.o.spike_detour * d + 0.2) {
            return None;
        }
        Some(Spike { r, va, vb })
    }
    fn via_clear_of_own_pads(&self, proto: &Via, p: Vec2) -> bool {
        for pad in &self.b.pads {
            if pad.net != proto.net {
                continue;
            }
            if (pad.center.x - p.x).abs() > 6.0 || (pad.center.y - p.y).abs() > 6.0 {
                continue;
            }
            if drc::point_pad_distance(p, pad) < proto.diameter * 0.5 + self.o.clearance_slack {
                return false;
            }
        }
        true
    }
    fn spike_ends(&self, sp: Spike) -> Vec<(usize, i32)> {
        let mut out = Vec::new();
        for v in [sp.va, sp.vb] {
            for &(t, e) in &self.via_ends[v] {
                if t != sp.r {
                    out.push((t, e));
                }
            }
        }
        out
    }

    // ViaHops.cpp:1491-1584.
    fn try_spike_merge(&mut self, sp: Spike, edit: &mut Edit) -> bool {
        let a = self.b.vias[sp.va].clone();
        let bv = self.b.vias[sp.vb].clone();
        if a.layer_start != bv.layer_start
            || a.layer_end != bv.layer_end
            || a.kind != bv.kind
            || (a.diameter - bv.diameter).abs() > 1e-4
            || (a.drill - bv.drill).abs() > 1e-4
        {
            return false;
        }
        if a.connected_pad >= 0 && bv.connected_pad >= 0 && a.connected_pad != bv.connected_pad {
            return false;
        }
        let ends = self.spike_ends(sp);
        for &(t, _) in &ends {
            if !self.trace_editable(&self.b.traces[t]) || self.pg.member(self.b.traces[t].net) {
                return false;
            }
        }
        // ADAPTED cpp:1504,1547: ign is membership-only; moved_ids is iterated
        // ascending at after_ids construction (cpp:1550).
        let mut ign = BTreeSet::from([self.b.traces[sp.r].id]);
        for &(t, _) in &ends {
            ign.insert(self.b.traces[t].id);
        }
        let (dead_a, dead_b) = (self.via_dead[sp.va], self.via_dead[sp.vb]);
        self.via_dead[sp.va] = true;
        self.via_dead[sp.vb] = true;
        let mut at = None;
        for f in [0.5f32, 0.35, 0.65, 0.2, 0.8, 0.0, 1.0] {
            let m = Vec2::lerp(a.pos, bv.pos, f);
            if !self.via_fits(&a, m, a.net, &ign) || !self.via_clear_of_own_pads(&a, m) {
                continue;
            }
            let mut ok = true;
            for &(t, e) in &ends {
                let tr = &self.b.traces[t];
                let n = tr.points.len();
                if n < 2 {
                    ok = false;
                    break;
                }
                let nxt = if e == 0 { tr.points[1].pos } else { tr.points[n - 2].pos };
                let ck = self.build_checker(tr.layer, tr.layer, tr.net, tr.rule, tr.width * 0.5, seg_box(m, nxt, 0.0), &ign, false);
                if !ck.seg_ok(m, nxt, false) {
                    ok = false;
                    break;
                }
                let seg = if e == 0 { 0 } else { n as i32 - 2 };
                for j in 0..n - 1 {
                    if !ok {
                        break;
                    }
                    if (j as i32 - seg).abs() > 1 && segs_cross(m, nxt, tr.points[j].pos, tr.points[j + 1].pos) {
                        ok = false;
                    }
                }
            }
            if ok {
                at = Some(m);
                break;
            }
        }
        self.via_dead[sp.va] = dead_a;
        self.via_dead[sp.vb] = dead_b;
        let Some(at) = at else {
            return false;
        };
        *edit = Edit::default();
        let rt = &self.b.traces[sp.r];
        edit.nets.insert(rt.net);
        edit.before.push(rt.clone());
        let mut moved_ids = BTreeSet::new();
        for &(t, _) in &ends {
            if moved_ids.insert(self.b.traces[t].id) {
                edit.before.push(self.b.traces[t].clone());
            }
        }
        edit.after_ids.extend(moved_ids);
        for p in &rt.points {
            edit.box_.expand(p.pos);
        }
        edit.box_.expand(a.pos);
        edit.box_.expand(bv.pos);
        edit.box_.pad(1.0);
        let mut nv = a;
        nv.pos = at;
        nv.id = self.b.vias.len() as i32;
        if nv.connected_pad < 0 {
            nv.connected_pad = bv.connected_pad;
        }
        nv.velocity = Vec2::default();
        let nid = nv.id;
        self.b.vias.push(nv);
        self.via_dead.push(false);
        edit.new_vias = vec![nid as usize];
        edit.vias = vec![sp.va, sp.vb];
        self.via_dead[sp.va] = true;
        self.via_dead[sp.vb] = true;
        for &(t, e) in &ends {
            let tr = &mut self.b.traces[t];
            let tp = if e == 0 {
                tr.points.first_mut().unwrap()
            } else {
                tr.points.last_mut().unwrap()
            };
            tp.pos = at;
            tp.attached_via = nid;
            tp.velocity = Vec2::default();
            clear_geometry(tr);
            tr.length = poly_len(&tr.points) as f32;
            for p in &tr.points {
                edit.box_.expand(p.pos);
            }
        }
        self.b.traces.remove(sp.r);
        self.rebuild();
        true
    }

    // ViaHops.cpp:1586-1629.
    fn try_spike_straight(&mut self, sp: Spike, edit: &mut Edit) -> bool {
        let rt = &self.b.traces[sp.r];
        let (a, b) = (self.b.vias[sp.va].pos, self.b.vias[sp.vb].pos);
        // ADAPTED cpp:1591-1592: ign is a membership-only BTreeSet.
        let mut ign = BTreeSet::from([rt.id]);
        for (t, _) in self.spike_ends(sp) {
            ign.insert(self.b.traces[t].id);
        }
        let ck = self.build_checker(rt.layer, rt.layer, rt.net, rt.rule, rt.width * 0.5, seg_box(a, b, 0.0), &ign, false);
        if !ck.seg_ok(a, b, false) {
            return false;
        }
        *edit = Edit::default();
        edit.nets.insert(rt.net);
        edit.before.push(rt.clone());
        edit.after_ids.push(rt.id);
        for p in &rt.points {
            edit.box_.expand(p.pos);
        }
        edit.box_.pad(1.0);
        let mut nt = rt.clone();
        let mut np = vec![rt.points[0].clone()];
        for v in self.particles(&[a, b]) {
            np.push(TracePoint {
                pos: v,
                width: rt.width,
                ..TracePoint::default()
            });
        }
        np.push(rt.points.last().unwrap().clone());
        np[0].pos = a;
        np.last_mut().unwrap().pos = b;
        for t in &mut np {
            t.velocity = Vec2::default();
        }
        nt.points = np;
        clear_geometry(&mut nt);
        nt.length = poly_len(&nt.points) as f32;
        // ADAPTED ViaHops.cpp:1464,1628,2514,2520: the reference admits an
        // already-straight candidate and overcounts it as an edit. Compare the
        // same copper bits as changed_nets before replacing the trace: an
        // unchanged candidate enters no ledger and counts nothing, even when
        // its non-copper metadata would change or the Guard is disabled.
        let mut before = Vec::new();
        let mut after = Vec::new();
        append_trace_copper(&mut before, rt);
        append_trace_copper(&mut after, &nt);
        if before == after {
            return false;
        }
        self.b.traces[sp.r] = nt;
        self.rebuild();
        true
    }

    // ViaHops.cpp:1631-1722.
    fn find_plane_tap(&mut self, t: usize) -> Option<PlaneTap> {
        let tr = self.b.traces.get(t)?;
        if !self.trace_editable(tr) || tr.points.len() < 2 {
            return None;
        }
        let net = tr.net;
        if !self.has_plane(net) {
            return None;
        }
        let tr = &self.b.traces[t];
        if drc::is_plane_layer(self.b, tr.layer) || self.branch_refs.contains_key(&tr.id) {
            return None;
        }
        let pl = self.plane_of_net[&tr.net];
        let n_v = self.b.vias.len();
        let n = tr.points.len();
        if !self.net_vias.contains_key(&tr.net) {
            let vs = self
                .b
                .vias
                .iter()
                .enumerate()
                .filter(|(_, v)| v.net == tr.net)
                .map(|(i, _)| i)
                .collect();
            self.net_vias.insert(tr.net, vs);
        }
        let vs = &self.net_vias[&tr.net];
        let tap_ok = |y: i32, x: usize| {
            y >= 0
                && (y as usize) < n_v
                && y as usize != x
                && !self.via_dead[y as usize]
                && self.b.vias[y as usize].net == tr.net
                && drc::via_on_layer(&self.b.vias[y as usize], pl)
                && drc::via_on_layer(&self.b.vias[y as usize], tr.layer)
        };
        for end in (0..=1).rev() {
            let x = if end == 1 {
                tr.points[n - 1].attached_via
            } else {
                tr.points[0].attached_via
            };
            if x < 0 || x as usize >= n_v || self.via_dead[x as usize] || self.b.vias[x as usize].net != tr.net {
                continue;
            }
            let x = x as usize;
            if !self.via_editable(x) || self.via_interior[x] != 0 || self.via_ends[x].len() != 1 || self.via_on_pad(x) {
                continue;
            }
            if !drc::via_on_layer(&self.b.vias[x], pl) {
                continue;
            }
            let via = &self.b.vias[x];
            let mut len = 0.0f64;
            for s in 1..n {
                let k = if end == 1 { n - 1 - s } else { s };
                let kp = if end == 1 { k + 1 } else { k - 1 };
                let p = &tr.points[k];
                let (a, b) = (tr.points[kp].pos, p.pos);
                let mut over = None;
                let mut best = 1e9;
                for &y in vs.iter() {
                    if !tap_ok(y as i32, x) {
                        continue;
                    }
                    let vy = &self.b.vias[y];
                    if vy.pos.distance_to(via.pos) < (vy.diameter + via.diameter) * 0.5 {
                        continue;
                    }
                    let d = point_seg(vy.pos, a, b);
                    if d <= vy.diameter * 0.5 && d < best {
                        best = d;
                        over = Some(y);
                    }
                }
                if let Some(over) = over {
                    if len + a.distance_to(self.b.vias[over].pos) as f64 <= self.o.max_hop_length as f64 {
                        return Some(PlaneTap {
                            end,
                            drop_via: x,
                            keep_via: over,
                            cut_node: k,
                            add_node: true,
                        });
                    }
                }
                len += a.distance_to(b) as f64;
                if len > self.o.max_hop_length as f64 {
                    break;
                }
                if p.branch_trace >= 0 || (p.terminal_pad >= 0 && k != 0 && k != n - 1) {
                    break;
                }
                let y = p.attached_via;
                if tap_ok(y, x) {
                    let y = y as usize;
                    if (k == 0 || k == n - 1) && self.via_ends[y].len() as i32 + self.via_interior[y] < 2 {
                        break;
                    }
                    return Some(PlaneTap {
                        end,
                        drop_via: x,
                        keep_via: y,
                        cut_node: k,
                        add_node: false,
                    });
                }
                if p.terminal_pad >= 0 {
                    break;
                }
            }
        }
        None
    }

    // ViaHops.cpp:1724-1774.
    fn reachable_end(&self, run: Run, kid: i32) -> bool {
        let Some(&ki) = self.id_to_idx.get(&kid) else {
            return false;
        };
        let rt = &self.b.traces[run.r];
        let kt = &self.b.traces[ki];
        let x = run.outer;
        let n_l = self.b.layers.len() as i32;
        let (a, b) = (self.b.vias[run.vs].pos, self.b.vias[run.ve].pos);
        for end in 0..2 {
            let ep = if end == 0 { &kt.points[0] } else { kt.points.last().unwrap() };
            if ep.terminal_pad >= 0 {
                continue;
            }
            let e = ep.pos;
            if point_seg(e, a, b) > self.o.detour_radius {
                continue;
            }
            let ev = ep.attached_via;
            // ADAPTED cpp:1738-1741: an attached endpoint uses its via's rule
            // row and layer -1; a free endpoint keeps its trace's row and layer.
            let (r_e, clr) = if ev >= 0 && (ev as usize) < self.b.vias.len() {
                let via = &self.b.vias[ev as usize];
                (via.diameter * 0.5, self.b.clr.get(via.rule, rt.rule, -1))
            } else {
                (kt.width * 0.5, self.b.clr.get(kt.rule, rt.rule, x))
            };
            // ADAPTED cpp:1740-1766: rule-row clearances; query reach uses
            // std_max(rules.min_clearance, clr.max), and via discs use layer -1.
            let base = r_e + rt.width + clr;
            let reach = base + std_max(self.b.rules.min_clearance, self.b.clr.max) + 2.0;
            let mut free = true;
            for tr in &self.b.traces {
                if !free {
                    break;
                }
                if !tr.is_routed || tr.layer != x || tr.id == kt.id || tr.net == rt.net {
                    continue;
                }
                let need = base + self.b.clr.get(rt.rule, tr.rule, x) + tr.width * 0.5;
                for p in tr.points.windows(2) {
                    if !free {
                        break;
                    }
                    if !boxes_overlap(seg_box(p[0].pos, p[1].pos, 0.0), seg_box(e, e, reach + tr.width)) {
                        continue;
                    }
                    if point_seg(e, p[0].pos, p[1].pos) < need {
                        free = false;
                    }
                }
            }
            for pad in &self.b.pads {
                if !free {
                    break;
                }
                if pad.net == rt.net || !drc::pad_on_layer(pad, x, n_l) {
                    continue;
                }
                if (pad.center.x - e.x).abs() > reach + 5.0 || (pad.center.y - e.y).abs() > reach + 5.0 {
                    continue;
                }
                if drc::point_pad_distance(e, pad) < base + self.b.clr.get(rt.rule, pad.rule, x) {
                    free = false;
                }
            }
            for (v, via) in self.b.vias.iter().enumerate() {
                if !free {
                    break;
                }
                if v as i32 == ev || self.via_dead[v] || via.net == rt.net {
                    continue;
                }
                if !drc::via_on_layer(via, x) {
                    continue;
                }
                if via.pos.distance_to(e) < base + self.b.clr.get(rt.rule, via.rule, -1) + via.diameter * 0.5 {
                    free = false;
                }
            }
            if free && self.b.outline.len() >= 3 {
                let mut j = self.b.outline.len() - 1;
                for i in 0..self.b.outline.len() {
                    if point_seg(e, self.b.outline[j], self.b.outline[i]) < base + self.edge_clr {
                        free = false;
                    }
                    j = i;
                }
            }
            if free {
                return true;
            }
        }
        false
    }
}

#[derive(Clone, Copy)]
struct Spike {
    r: usize,
    va: usize,
    vb: usize,
}
struct PlaneTap {
    end: i32,
    drop_via: usize,
    keep_via: usize,
    cut_node: usize,
    add_node: bool,
}

// ADAPTED ViaHops.cpp:1073: no existing helper; ported from MIT-cleared
// core/Board.h:578-582 (DesignRules::effectiveHoleToHole), including its floor.
fn effective_hole_to_hole(b: &OBoard) -> f32 {
    if b.rules.hole_to_hole > 0.25 {
        b.rules.hole_to_hole
    } else {
        0.25
    }
}
// ADAPTED ViaHops.cpp:748-751 (also 929-932,1046-1049,1331-1334,1345-1348,
// 1574-1577,1621-1624): shared spelling of the same four clears.
fn clear_geometry(t: &mut Trace) {
    t.smoothed.clear();
    t.point_widths.clear();
    t.teardrop_front.clear();
    t.teardrop_back.clear();
}

// ViaHops.cpp:1777-1793.
fn compact_dead_vias(b: &mut OBoard, dead: &[bool]) {
    let mut remap = vec![-1; b.vias.len()];
    let mut kept = Vec::new();
    for (i, via) in b.vias.iter().enumerate() {
        if i < dead.len() && dead[i] {
            continue;
        }
        remap[i] = kept.len() as i32;
        let mut v = via.clone();
        v.id = kept.len() as i32;
        kept.push(v);
    }
    b.vias = kept;
    for tr in &mut b.traces {
        for p in &mut tr.points {
            if p.attached_via >= 0 {
                p.attached_via = if (p.attached_via as usize) < remap.len() {
                    remap[p.attached_via as usize]
                } else {
                    -1
                };
            }
        }
    }
}

// ADAPTED ViaHops.cpp:1795-1818: gated/notWorse are Guard::judge's responsibility.

/// ViaHops.cpp:1822-1831.
pub fn class_name(c: HopClass) -> &'static str {
    match c {
        HopClass::Empty => "empty",
        HopClass::OneTrace => "one-trace",
        HopClass::PadOrVia => "pad/via",
        HopClass::Multiple => "multiple",
    }
}
/// ViaHops.cpp:1833-1838 (raw Trace::points, in board order).
pub fn total_length(board: &OBoard) -> f64 {
    let mut length = 0.0;
    for tr in &board.traces {
        length += poly_len(&tr.points);
    }
    length
}

// ADAPTED ViaHops.cpp:1844-2076: use hops::count_hops; ArcFit is used only there.

/// ViaHops.cpp:2107-2114.
pub fn spike_merge_only(board: &mut OBoard, opt: &ViaHopOptions, stats: &mut ViaHopStats, guard: &mut dyn Guard) -> bool {
    let mut o = opt.clone();
    o.enable_go_around = false;
    o.enable_push = false;
    o.enable_flip = false;
    o.enable_merge = false;
    o.enable_absorb = false;
    o.spike_merge = true;
    o.plane_taps = false;
    run(board, &o, stats, guard)
}

// ADAPTED cpp:572,756,2225,2429,2498: strict sorts use sort_unstable_by;
// equal numeric values remain comparator ties. cpp:2155,2564 retain stable ties below.
// ViaHops.cpp:2154-2159,2564-2567. ADAPTED cpp:2126: unordered_map ->
// BTreeMap; order_key is lookup-only. sort_by preserves stable_sort's ties.
fn restore_order(b: &mut OBoard, order_key: &BTreeMap<i32, usize>) {
    b.traces.sort_by(|a, b| {
        order_key
            .get(&a.id)
            .copied()
            .unwrap_or(1 << 30)
            .cmp(&order_key.get(&b.id).copied().unwrap_or(1 << 30))
    });
}

// ADAPTED ViaHops.cpp:2160-2214: extract run's lambda to give Rust explicit borrows.
fn pair_edit_ok(st: &State<'_>, e: &Edit, touches_pair: &mut bool) -> bool {
    *touches_pair = false;
    let ids = e.touched();
    let mut nets = BTreeSet::new();
    let mut box_ = Box2::default();
    let mut w = 0.0;
    for t in &e.before {
        nets.insert(t.net);
        for p in &t.points {
            box_.expand(p.pos);
        }
        w = std_max(w, t.width);
    }
    for t in &st.b.traces {
        if ids.contains(&t.id) {
            nets.insert(t.net);
            for p in &t.points {
                box_.expand(p.pos);
            }
            w = std_max(w, t.width);
        }
    }
    // ADAPTED cpp:2164-2182: ids/done BTreeSets are membership-only; nets is
    // iterated ascending here, visiting each pair once in reference order.
    let mut done = BTreeSet::new();
    for x in nets {
        if !st.pg.member(x) || done.contains(&x) {
            continue;
        }
        *touches_pair = true;
        let y = st.pg.partner(x);
        done.insert(x);
        done.insert(y);
        let mut bb = box_;
        bb.pad(diffpairs::member_gap(st.b, x) * 1.2 + 2.0 * w + 0.5);
        let near = |t: &Trace| {
            let mut tb = Box2::default();
            for p in &t.points {
                tb.expand(p.pos);
            }
            tb.is_valid() && !(tb.max.x < bb.min.x || tb.min.x > bb.max.x || tb.max.y < bb.min.y || tb.min.y > bb.max.y)
        };
        let mut old_x = Vec::new();
        let mut new_x = Vec::new();
        let mut old_y = Vec::new();
        let mut new_y = Vec::new();
        for t in &e.before {
            if t.net == x {
                old_x.push(PairGuard::poly_trace(t));
            } else if t.net == y {
                old_y.push(PairGuard::poly_trace(t));
            }
        }
        for t in &st.b.traces {
            if (t.net != x && t.net != y) || !t.is_routed || t.points.len() < 2 || !near(t) {
                continue;
            }
            if t.net == x {
                new_x.push(PairGuard::poly_trace(t));
            } else {
                new_y.push(PairGuard::poly_trace(t));
            }
            if !ids.contains(&t.id) {
                if t.net == x {
                    old_x.push(PairGuard::poly_trace(t));
                } else {
                    old_y.push(PairGuard::poly_trace(t));
                }
            }
        }
        let gap = diffpairs::member_gap(st.b, x);
        let before = diffpairs::coupled_length(&old_x, &old_y, gap, true);
        let after = diffpairs::coupled_length(&new_x, &new_y, gap, true);
        if after < before - st.pg.tol_mm as f64 {
            return false;
        }
    }
    true
}

// ADAPTED ViaHops.cpp:2383-2388,2528-2568: both rollback sites share trace restoration;
// only guard reverts change the edit ledger/counter.
fn restore_edit(st: &mut State<'_>, e: &Edit) {
    // ADAPTED cpp:2383,2531-2532: rm BTreeSet is membership-only.
    let rm = e.touched();
    st.b.traces.retain(|t| !rm.contains(&t.id));
    st.b.traces.extend(e.before.iter().cloned());
    for &v in &e.vias {
        st.via_dead[v] = false;
    }
    for &v in &e.new_vias {
        st.via_dead[v] = true;
    }
}
fn revert_with_dependents(st: &mut State<'_>, targets: BTreeSet<usize>, order_key: &BTreeMap<i32, usize>) {
    let mut closure = targets.clone();
    let mut ids = BTreeSet::new();
    // ADAPTED cpp:2541-2563: targets/closure/touched are iterated ascending
    // while building the dependency closure; closure is reversed for restore.
    // ids is a membership-only BTreeSet.
    for i in targets {
        for id in st.edits[i].touched() {
            ids.insert(id);
        }
    }
    for j in 0..st.edits.len() {
        if !st.edits[j].active || closure.contains(&j) {
            continue;
        }
        let mut dep = false;
        for &i in &closure {
            if i < j {
                for id in st.edits[j].touched() {
                    if ids.contains(&id) {
                        dep = true;
                    }
                }
            }
        }
        if dep {
            closure.insert(j);
            for id in st.edits[j].touched() {
                ids.insert(id);
            }
        }
    }
    for &i in closure.iter().rev() {
        if !st.edits[i].active {
            continue;
        }
        let e = st.edits[i].clone();
        restore_edit(st, &e);
        st.edits[i].active = false;
        st.s.reverts += 1;
    }
    restore_order(st.b, order_key);
}

// ADAPTED ViaHops.cpp:2570-2583: Guard needs the nets with differing copper.
// Compare geometry by bits, excluding IDs/attachments remapped by via compaction
// and solver/export metadata without copper. Include exported copper as well as
// points because edits clear smoothed paths, widths and teardrops.
fn append_trace_copper(out: &mut Vec<u64>, t: &Trace) {
    fn xy(out: &mut Vec<u64>, p: Vec2) {
        out.extend([p.x.to_bits() as u64, p.y.to_bits() as u64]);
    }
    fn poly(out: &mut Vec<u64>, p: &[Vec2]) {
        out.push(p.len() as u64);
        for &v in p {
            xy(out, v);
        }
    }
    out.extend([t.layer as u64, t.width.to_bits() as u64, t.is_routed as u64, t.points.len() as u64]);
    for p in &t.points {
        xy(out, p.pos);
        out.push(p.width.to_bits() as u64);
    }
    poly(out, &t.smoothed);
    out.push(t.point_widths.len() as u64);
    for w in &t.point_widths {
        out.push(w.to_bits() as u64);
    }
    poly(out, &t.teardrop_front);
    poly(out, &t.teardrop_back);
}
fn copper_of_net(b: &OBoard, net: i32) -> Vec<u64> {
    let mut out = Vec::new();
    out.push(b.traces.iter().filter(|t| t.net == net).count() as u64);
    for t in &b.traces {
        if t.net == net {
            append_trace_copper(&mut out, t);
        }
    }
    out.push(b.vias.iter().filter(|v| v.net == net).count() as u64);
    for v in &b.vias {
        if v.net == net {
            out.extend([
                v.pos.x.to_bits() as u64,
                v.pos.y.to_bits() as u64,
                v.diameter.to_bits() as u64,
                v.drill.to_bits() as u64,
                v.layer_start as u64,
                v.layer_end as u64,
                v.kind as u64,
            ]);
        }
    }
    out
}
fn changed_nets(input: &OBoard, out: &OBoard) -> Vec<i32> {
    // This additional Guard-adapter BTreeSet is iterated ascending once here.
    let mut nets = BTreeSet::new();
    for b in [input, out] {
        for t in &b.traces {
            nets.insert(t.net);
        }
        for v in &b.vias {
            nets.insert(v.net);
        }
    }
    nets.into_iter()
        .filter(|&n| copper_of_net(input, n) != copper_of_net(out, n))
        .collect()
}

// ADAPTED ViaHops.cpp:2520,2623,2633-2640: the reference overcounts edits
// after restoring the input. Clear every edit counter on every exact-input
// restore, including the Rust-only unchanged-copper path, independently of
// full_revert and reverts. Candidate/work counters and status stay intact.
fn clear_edit_counts(s: &mut ViaHopStats) {
    s.removed_empty = 0;
    s.go_around = 0;
    s.pushed = 0;
    s.flipped = 0;
    s.relayered = 0;
    s.slid = 0;
    s.merged = 0;
    s.absorbed = 0;
    s.edits = 0;
    s.pair_edits = 0;
    s.spike_merged = 0;
    s.spike_straightened = 0;
    s.plane_taps_removed = 0;
}

// ADAPTED ViaHops.cpp:2400-2412,2643-2656: share the identical strategy tally.
fn tally(s: &mut ViaHopStats, strategy: Strategy) {
    match strategy {
        Strategy::Empty => s.removed_empty += 1,
        Strategy::Around => s.go_around += 1,
        Strategy::Push => s.pushed += 1,
        Strategy::Flip => s.flipped += 1,
        Strategy::Merge => s.merged += 1,
        Strategy::Absorb => s.absorbed += 1,
        Strategy::Relayer => s.relayered += 1,
        Strategy::Slide => s.slid += 1,
        Strategy::SpikeMerge => s.spike_merged += 1,
        Strategy::SpikeStraight => s.spike_straightened += 1,
        Strategy::PlaneTap => s.plane_taps_removed += 1,
    }
}

/// ViaHops.cpp:2116-2707. Mutates the board; returns true on acceptance or restoration of input.
pub fn run(board: &mut OBoard, opt: &ViaHopOptions, stats: &mut ViaHopStats, guard: &mut dyn Guard) -> bool {
    *stats = ViaHopStats::default();
    stats.hops_before = count_hops(board, opt.max_hop_length, false, None, None, false).all();
    stats.vias_before = board.vias.len() as i32;
    stats.length_before = total_length(board);
    let input = board.clone();
    let mut order_key = BTreeMap::new();
    for (t, tr) in board.traces.iter().enumerate() {
        order_key.insert(tr.id, t);
    }
    // ADAPTED cpp:2121,2128: raw points are already explicit in count_hops;
    // no initial Drc::run: the caller's Guard owns the accepted input state.
    let mut st = State::new(board, opt, stats);
    let mut resolvable_ids = BTreeSet::new();
    st.pg.init(st.b, opt.pair_aware);
    // ADAPTED cpp:2216,2245: no budget. max_rounds bounds the sweep, each ids
    // list is a finite snapshot, and an unchanged sweep ends the rounds early.
    for round in 0..std_max(1, opt.max_rounds) {
        st.rebuild();
        let mut ids = Vec::new();
        for t in 0..st.b.traces.len() {
            if st.find_run(t, opt.max_hop_length).is_some() {
                ids.push(st.b.traces[t].id);
            }
        }
        ids.sort_unstable_by(|a, b| a.cmp(b));
        if round == 0 {
            st.s.candidates = ids.len() as i32;
            for &id in &ids {
                let r = st.find_run(st.id_to_idx[&id], opt.max_hop_length).unwrap();
                let mut cr = BTreeSet::new();
                let hc = st.classify(r, &mut cr);
                st.s.by_class[hc as usize] += 1;
                let reach = hc == HopClass::OneTrace && cr.len() == 1 && st.reachable_end(r, *cr.first().unwrap());
                st.s.one_trace_reachable += reach as i32;
                if hc == HopClass::Empty || reach {
                    resolvable_ids.insert(id);
                }
            }
            st.s.resolvable = resolvable_ids.len() as i32;
        }
        let mut changed = false;
        for id in ids {
            let Some(&idx) = st.id_to_idx.get(&id) else {
                continue;
            };
            let Some(run) = st.find_run(idx, opt.max_hop_length) else {
                continue;
            };
            let mut crossed = BTreeSet::new();
            let cls = st.classify(run, &mut crossed);
            let mut e = Edit::default();
            let mut delta = 0.0;
            let mut ok = false;
            let mut strat = Strategy::Empty;
            if opt.enable_go_around && st.try_bridge(run, false, &BTreeSet::new(), &mut e, &mut delta) {
                ok = true;
                strat = if cls == HopClass::Empty {
                    Strategy::Empty
                } else {
                    Strategy::Around
                };
            }
            if !ok && opt.enable_push && cls != HopClass::Empty && st.try_bridge(run, true, &BTreeSet::new(), &mut e, &mut delta) {
                ok = true;
                strat = Strategy::Push;
            }
            if !ok && opt.enable_flip && crossed.len() == 1 {
                let kid = *crossed.first().unwrap();
                let kr = st.id_to_idx.get(&kid).copied().and_then(|ki| st.find_run(ki, opt.max_hop_length));
                if let Some(kr) = kr.filter(|kr| kr.outer == st.b.traces[run.r].layer) {
                    let saved = st.b.traces.clone();
                    let saved_dead = st.via_dead.clone();
                    let mut e1 = Edit::default();
                    let mut e2 = Edit::default();
                    let mut d1 = 0.0;
                    let mut d2 = 0.0;
                    st.via_dead[kr.vs] = true;
                    st.via_dead[kr.ve] = true;
                    let first = st.try_bridge(run, false, &BTreeSet::from([kid]), &mut e1, &mut d1);
                    st.via_dead[kr.vs] = false;
                    st.via_dead[kr.ve] = false;
                    if first {
                        let kr2 = st.id_to_idx.get(&kid).copied().and_then(|ki| st.find_run(ki, opt.max_hop_length));
                        if let Some(kr2) = kr2 {
                            if st.try_bridge(kr2, false, &BTreeSet::new(), &mut e2, &mut d2) {
                                e1.absorb(e2);
                                e = e1;
                                delta = d1 + d2;
                                ok = true;
                                strat = Strategy::Flip;
                            }
                        }
                    }
                    if !ok {
                        st.restore_traces(saved, saved_dead);
                    }
                }
            }
            if !ok && opt.enable_flip && !crossed.is_empty() && crossed.len() <= 2 {
                let y = st.b.traces[run.r].layer;
                let saved = st.b.traces.clone();
                let saved_dead = st.via_dead.clone();
                let mut e1 = Edit::default();
                let mut d1 = 0.0;
                let mut good = st.try_bridge(run, false, &crossed, &mut e1, &mut d1);
                // ADAPTED cpp:2250,2313: crossed BTreeSet iterates ascending
                // here; all other uses take the first member or inspect its size.
                for &kid in &crossed {
                    if !good {
                        break;
                    }
                    let mut ek = Edit::default();
                    good = st.try_relayer(kid, y, &mut ek);
                    if good {
                        e1.absorb(ek);
                    }
                }
                if good {
                    e = e1;
                    delta = d1;
                    ok = true;
                    strat = Strategy::Relayer;
                } else {
                    st.restore_traces(saved, saved_dead);
                }
            }
            if !ok && opt.enable_flip && crossed.len() == 1 {
                let mut es = Edit::default();
                let mut ds = 0.0;
                if st.try_slide(run, *crossed.first().unwrap(), &mut es, &mut ds) {
                    e = es;
                    delta = ds;
                    ok = true;
                    strat = Strategy::Slide;
                }
            }
            if !ok && opt.enable_merge {
                for side in 0..2 {
                    if ok {
                        break;
                    }
                    let nb = if side == 0 { run.p } else { run.q };
                    let Some(nr) = st.find_run(nb, opt.merge_gap) else {
                        continue;
                    };
                    if nr.outer != st.b.traces[run.r].layer {
                        continue;
                    }
                    if st.try_bridge(nr, false, &BTreeSet::new(), &mut e, &mut delta) {
                        ok = true;
                        strat = Strategy::Merge;
                    }
                }
            }
            if !ok && opt.enable_absorb {
                for side in 0..2 {
                    if ok {
                        break;
                    }
                    let nb = if side == 0 { run.p } else { run.q };
                    let Some(nr) = st.find_run(nb, opt.absorb_length) else {
                        continue;
                    };
                    if nr.outer != st.b.traces[run.r].layer {
                        continue;
                    }
                    if st.try_bridge(nr, false, &BTreeSet::new(), &mut e, &mut delta) {
                        ok = true;
                        strat = Strategy::Absorb;
                    }
                }
            }
            // ADAPTED cpp:2414: delta's final value was printed only; its
            // intermediate values and every strategy's length checks remain.
            let _ = delta;
            if !ok {
                continue;
            }
            if st.pg.active() {
                let mut touches_pair = false;
                if !pair_edit_ok(&st, &e, &mut touches_pair) {
                    restore_edit(&mut st, &e);
                    restore_order(st.b, &order_key);
                    st.rebuild();
                    st.s.pair_skipped += 1;
                    continue;
                }
                e.pair = touches_pair;
            }
            e.strategy = strat;
            st.edits.push(e);
            changed = true;
            tally(st.s, strat);
        }
        if !changed {
            break;
        }
    }

    // ViaHops.cpp:2418-2487. ADAPTED cpp:2419: no budget gate; this one sweep
    // visits a finite snapshot of trace IDs, each at most once.
    if opt.plane_taps {
        st.rebuild();
        st.net_vias.clear();
        let mut ids = Vec::new();
        for t in 0..st.b.traces.len() {
            if st.find_plane_tap(t).is_some() {
                ids.push(st.b.traces[t].id);
            }
        }
        ids.sort_unstable_by(|a, b| a.cmp(b));
        st.s.plane_tap_candidates = ids.len() as i32;
        for id in ids {
            st.rebuild();
            let Some(&idx) = st.id_to_idx.get(&id) else {
                continue;
            };
            let Some(pt) = st.find_plane_tap(idx) else {
                continue;
            };
            let tr = &st.b.traces[idx];
            if st.pg.active() && st.pg.member(tr.net) {
                continue;
            }
            let n = tr.points.len();
            let whole = !pt.add_node && pt.cut_node == if pt.end == 1 { 0 } else { n - 1 };
            let mut e = Edit::default();
            e.before.push(tr.clone());
            e.vias.push(pt.drop_via);
            e.nets.insert(tr.net);
            let a = if pt.end == 1 { pt.cut_node } else { 0 };
            let b = if pt.end == 1 { n - 1 } else { pt.cut_node };
            for k in a..=b {
                e.box_.expand(tr.points[k].pos);
            }
            if whole {
                st.b.traces.remove(idx);
            } else {
                let mut np = tr.points[if pt.end == 1 { 0 } else { pt.cut_node }..if pt.end == 1 { pt.cut_node + 1 } else { n }].to_vec();
                if pt.add_node {
                    let mut tp = if pt.end == 1 { np.last().unwrap().clone() } else { np[0].clone() };
                    tp.pos = st.b.vias[pt.keep_via].pos;
                    tp.terminal_pad = -1;
                    tp.branch_trace = -1;
                    tp.velocity = Vec2::default();
                    if pt.end == 1 {
                        np.push(tp);
                    } else {
                        np.insert(0, tp);
                    }
                }
                for q in &mut np {
                    if q.attached_via == pt.drop_via as i32 {
                        q.attached_via = -1;
                    }
                }
                let cut = if pt.end == 1 { np.last_mut().unwrap() } else { &mut np[0] };
                cut.attached_via = pt.keep_via as i32;
                let tr = &mut st.b.traces[idx];
                tr.points = np;
                tr.smoothed.clear();
                tr.point_widths.clear();
                if pt.end == 1 {
                    tr.teardrop_back.clear();
                } else {
                    tr.teardrop_front.clear();
                }
                tr.length = poly_len(&tr.points) as f32;
                e.after_ids.push(tr.id);
            }
            st.via_dead[pt.drop_via] = true;
            e.strategy = Strategy::PlaneTap;
            st.edits.push(e);
            st.s.plane_taps_removed += 1;
        }
        st.rebuild();
    }

    // ViaHops.cpp:2488-2521. ADAPTED cpp:2489,2502: no budget. The snapshot
    // contains finitely many spike IDs, each attempted once; no re-enqueueing.
    if opt.spike_merge {
        st.rebuild();
        let mut ids = Vec::new();
        for t in 0..st.b.traces.len() {
            if st.find_spike(t).is_some() {
                ids.push(st.b.traces[t].id);
            }
        }
        ids.sort_unstable_by(|a, b| a.cmp(b));
        st.s.spike_candidates = ids.len() as i32;
        for id in ids {
            let Some(&idx) = st.id_to_idx.get(&id) else {
                continue;
            };
            let Some(sp) = st.find_spike(idx) else {
                continue;
            };
            let mut e = Edit::default();
            let strat = if st.try_spike_merge(sp, &mut e) {
                Strategy::SpikeMerge
            } else if st.try_spike_straight(sp, &mut e) {
                Strategy::SpikeStraight
            } else {
                continue;
            };
            e.strategy = strat;
            st.edits.push(e);
            tally(st.s, strat);
        }
    }

    // ViaHops.cpp:2523-2630. finalBoard remains a compacted copy, with the
    // un-compacted board and edit snapshots retained for dependency rollback.
    let mut out = st.b.clone();
    compact_dead_vias(&mut out, &st.via_dead);
    let mut restored_input = false;
    if opt.drc_guard && !st.edits.is_empty() {
        // ADAPTED cpp:2574-2591: before/after multiset matching of (kind, x*50,
        // y*50, layer) belongs to the judge; refusal() already contains only new
        // findings. No multiset remains here (no counted BTreeMap is needed).
        // ADAPTED cpp:2579: no maxGuardIterations. Every refusal targets at
        // least one active edit, and reverts it; finitely many edits terminate
        // the loop. Judge calls alone increment guard_iterations.
        let mut refused = false;
        loop {
            let changed = changed_nets(&input, &out);
            if changed.is_empty() {
                out = input.clone();
                clear_edit_counts(st.s);
                restored_input = true;
                break;
            }
            st.s.guard_iterations += 1;
            if guard.judge(&out, &changed) {
                break;
            }
            refused = true;
            // ADAPTED cpp:2585-2614: targets BTreeSet is populated in edit order,
            // iterated ascending by revert_with_dependents. Finding with no pos
            // uses only nets; a location match takes precedence over net matches.
            let mut targets = BTreeSet::new();
            for finding in guard.refusal() {
                let mut hit = false;
                if let Some(pos) = finding.pos {
                    for (i, e) in st.edits.iter().enumerate() {
                        if !e.active {
                            continue;
                        }
                        let mut bx = e.box_;
                        bx.pad(0.5);
                        if bx.contains(pos) {
                            targets.insert(i);
                            hit = true;
                        }
                    }
                }
                if !hit {
                    for (i, e) in st.edits.iter().enumerate() {
                        if e.active && finding.nets.iter().any(|n| e.nets.contains(n)) {
                            targets.insert(i);
                        }
                    }
                }
            }
            if targets.is_empty() {
                for (i, e) in st.edits.iter().enumerate() {
                    if e.active {
                        targets.insert(i);
                    }
                }
            }
            revert_with_dependents(&mut st, targets, &order_key);
            out = st.b.clone();
            compact_dead_vias(&mut out, &st.via_dead);
        }
        // ADAPTED cpp:2620-2625: no closing Drc::run. All edits reverted after a
        // refusal means full_revert and exact input. The reference's second way
        // to get here (cap reached and still worse) no longer exists.
        st.s.full_revert = refused && st.edits.iter().all(|e| !e.active);
        if st.s.full_revert {
            out = input.clone();
            clear_edit_counts(st.s);
            restored_input = true;
        }
    } else if st.edits.is_empty() {
        // ADAPTED cpp:2571,2628-2630: a no-op preserves even bookkeeping bits
        // (IDs/attachments); no judge call or normalization of the input.
        out = input.clone();
        clear_edit_counts(st.s);
        restored_input = true;
    }
    *st.b = out;

    // ADAPTED ViaHops.cpp:2520,2632-2657: the reference overcounts edits by
    // assigning the ledger size before the Guard and omitting it from this
    // recount. Count only active entries after the Guard, unless the exact
    // input was restored (whose counters stay zero, regardless of status).
    if !restored_input {
        clear_edit_counts(st.s);
        st.s.edits = st.edits.iter().filter(|e| e.active).count() as i32;
        for e in &st.edits {
            if e.active {
                st.s.pair_edits += e.pair as i32;
                tally(st.s, e.strategy);
            }
        }
    }
    // ViaHops.cpp:2659-2704. ADAPTED cpp:2131,2672: resolvable_ids BTreeSet
    // is membership-only. cr is used only for size and first-member lookup.
    {
        let mut fin = State::new(st.b, opt, st.s);
        fin.rebuild();
        let mut still_there = 0;
        for t in 0..fin.b.traces.len() {
            let Some(r) = fin.find_run(t, opt.max_hop_length) else {
                continue;
            };
            fin.s.unresolved += 1;
            let mut cr = BTreeSet::new();
            let hc = fin.classify(r, &mut cr);
            fin.s.unresolved_by_class[hc as usize] += 1;
            still_there += resolvable_ids.contains(&fin.b.traces[t].id) as i32;
            let site = Site {
                via_a: std_min(r.vs, r.ve) as i32,
                via_b: std_max(r.vs, r.ve) as i32,
                cls: hc,
                reachable: hc == HopClass::Empty
                    || (hc == HopClass::OneTrace && cr.len() == 1 && fin.reachable_end(r, *cr.first().unwrap())),
            };
            fin.s.unresolved_sites.push(site);
        }
        fin.s.resolvable_resolved = fin.s.resolvable - still_there;
    }
    st.s.hops_after = count_hops(st.b, opt.max_hop_length, false, None, None, false).all();
    st.s.vias_after = st.b.vias.len() as i32;
    st.s.length_after = total_length(st.b);
    // ADAPTED cpp:2707: off, accepted, or restored input all satisfy the contract.
    true
}

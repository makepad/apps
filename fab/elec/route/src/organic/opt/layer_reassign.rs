// Ported from TiNRS Psychedelic Router 0.1.0: src/optimize/LayerReassign.h/.cpp (whole).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in the parent folder.

//! Minimise vias by moving fixed-shape copper runs and their conflict closures between layers.

use super::super::diffpairs;
use super::super::geom::{std_max, std_min, Box2, Vec2};
use super::super::model::{OBoard, Trace, TracePoint, Via};
use super::drcgeom as drc;
use super::hops::count_hops;
use super::{fanout_via_mask, Guard};
use std::collections::{BTreeMap, BTreeSet};

/// LayerReassign.h:66-78.
#[derive(Clone, Debug)]
pub struct LayerReassignOptions {
    pub depth: i32,
    pub max_closure: i32,
    pub max_seed_work: i64,
    pub clearance_slack: f32,
    pub split: bool,
    pub edit_imported: bool,
}

impl Default for LayerReassignOptions {
    fn default() -> Self {
        // ADAPTED h:70,72,76-77: no whole-pass work/DRC cap, DRC options or printing.
        // Guard judges points; viaThrough is false. h:75 editImported -> edit_imported.
        Self { depth: 2, max_closure: 256, max_seed_work: 200000, clearance_slack: 0.003f32, split: true, edit_imported: false }
    }
}

/// LayerReassign.h:80-96. ADAPTED h:90,92-94: count judge calls, not an initial
/// DRC run; no budgetHit, time or reports. seed_limit counts exhausted seed searches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayerReassignStats {
    pub vias_before: i32,
    pub vias_after: i32,
    pub removed: i32,
    pub units: i32,
    pub pinned_units: i32,
    pub conflicts: i32,
    pub sites: i32,
    pub vias_movable: i32,
    pub found: i32,
    pub kept: i32,
    pub rolled_back: i32,
    pub units_moved: i32,
    pub judge_calls: i32,
    pub work: i64,
    pub seed_limit: i32,
}

impl LayerReassignStats {
    /// cpp:972-981. ADAPTED cpp:975-979: owned String, no clock or budget suffix;
    /// report judge calls and per-seed limits in place of DRC runs and elapsed ms.
    pub fn summary(&self) -> String {
        format!(
            "Layer reassign (O13): vias {} -> {} ({} removed by this stage), {} change(s) found, {} kept, {} rolled back, {} unit(s) moved; {} units ({} pinned), {} sites, {} conflicts, {} removable vias, {} judge calls, {} seed limits",
            self.vias_before, self.vias_after, self.removed, self.found, self.kept, self.rolled_back,
            self.units_moved, self.units, self.pinned_units, self.sites, self.conflicts, self.vias_movable,
            self.judge_calls, self.seed_limit,
        )
    }
}

// ADAPTED cpp:25,40-53: no Clock, drcCounts or notWorse; Guard owns accepted state.
// cpp:27-38.
fn boxes_touch(a: &Box2, b: &Box2, g: f32) -> bool {
    a.min.x - g <= b.max.x && b.min.x - g <= a.max.x && a.min.y - g <= b.max.y && b.min.y - g <= a.max.y
}

fn reversed_points(p: &[TracePoint]) -> Vec<TracePoint> {
    let n = p.len();
    let mut r: Vec<_> = p.iter().rev().cloned().collect();
    for j in 0..n { r[j].straight_before = if j == 0 { false } else { p[n - j].straight_before }; }
    r
}

// ADAPTED cpp:208-209,906: Board::viaSizeFor -> adapter-resolved RuleMatrix::via(net.class).
fn via_size_for(b: &OBoard, net: i32) -> (f32, f32) {
    let n = b.nets.get(net as usize);
    let dia = n.map_or(0.0, |n| n.via_diameter);
    let drill = n.map_or(0.0, |n| n.via_drill);
    // Without an adapter, zero (or a missing net) falls back to the board's design rules.
    (if dia == 0.0 { b.rules.via_diameter } else { dia }, if drill == 0.0 { b.rules.via_drill } else { drill })
}

// ADAPTED cpp:55-88,96-117: valid indices use usize (signed sentinels stay
// signed), vector<char> flags use bool, and units/segments retain clearance rows.
struct Piece { trace: usize, lo: usize, hi: usize }

struct Unit {
    pieces: Vec<Piece>,
    net: i32,
    layer: i32,
    width: f32,
    allowed: u32,
    pinned: bool,
    box_: Box2,
    vias: Vec<usize>,
    conflicts: Vec<usize>,
    rule: u16,
}

impl Default for Unit {
    fn default() -> Self {
        Self { pieces: Vec::new(), net: -1, layer: 0, width: 0.0, allowed: 0, pinned: false,
            box_: Box2::default(), vias: Vec::new(), conflicts: Vec::new(), rule: 0 }
    }
}

struct ViaInfo {
    units: Vec<usize>,
    fixed: bool,
    trace: i32,
    node: i32,
}

impl Default for ViaInfo {
    fn default() -> Self { Self { units: Vec::new(), fixed: true, trace: -1, node: -1 } }
}

#[derive(Clone, Copy)]
struct Seg { a: Vec2, b: Vec2, trace: usize, hw: f32, rule: u16 }

// cpp:90-92: the net and the position in whole micrometres (`std::lround` of the float
// product: f32 round, half away from zero). rejected is membership-only, never iterated.
type ViaKey = (i32, i32, i32);
fn via_key(v: &Via) -> ViaKey { (v.net, (v.pos.x * 1000.0f32).round() as i32, (v.pos.y * 1000.0f32).round() as i32) }

// ADAPTED cpp:94: both Cells unordered_maps -> BTreeMap. Neither map is
// iterated: query visits x, then y, then each cell's vector in insertion order.
type Cells = BTreeMap<i64, Vec<usize>>;

// cpp:96-146. ADAPTED: pass the live board explicitly instead of retaining a
// board borrow across application; stats are supplied to build only. ncl is clr.
struct Model<'a> {
    opt: &'a LayerReassignOptions,
    units: Vec<Unit>,
    vias: Vec<ViaInfo>,
    segs: Vec<Seg>,
    seg_conf: Vec<Vec<usize>>,
    seg_pads: Vec<Vec<usize>>,
    seg_grid: Cells,
    pad_grid: Cells,
    max_half_width: f32,
    cur: Vec<i32>,
    blocked_at: Vec<Vec<i8>>,
    needed: Vec<bool>,
    needed_count: i32,
    stamp: Vec<i32>,
    epoch: i32,
    n_l: i32,
    work: i64,
    work_stop: i64,
}

const K_CELL: f32 = 2.0;
fn cell_key(x: i32, y: i32) -> i64 { ((x as i64) << 32) ^ (y as u32 as i64) }
fn cell_of(v: f32) -> i32 { (v / K_CELL).floor() as i32 }
fn insert_box(g: &mut Cells, lo: Vec2, hi: Vec2, item: usize) {
    for x in cell_of(lo.x)..=cell_of(hi.x) {
        for y in cell_of(lo.y)..=cell_of(hi.y) { g.entry(cell_key(x, y)).or_default().push(item); }
    }
}
fn query(g: &Cells, lo: Vec2, hi: Vec2, mut cb: impl FnMut(usize)) {
    for x in cell_of(lo.x)..=cell_of(hi.x) {
        for y in cell_of(lo.y)..=cell_of(hi.y) {
            if let Some(items) = g.get(&cell_key(x, y)) { for &i in items { cb(i); } }
        }
    }
}

impl<'a> Model<'a> {
    fn new(b: &OBoard, opt: &'a LayerReassignOptions) -> Self {
        Self {
            opt, units: Vec::new(), vias: Vec::new(), segs: Vec::new(), seg_conf: Vec::new(), seg_pads: Vec::new(),
            seg_grid: BTreeMap::new(), pad_grid: BTreeMap::new(), max_half_width: 0.0, cur: Vec::new(),
            blocked_at: Vec::new(), needed: Vec::new(), needed_count: 0, stamp: Vec::new(), epoch: 0,
            n_l: b.layers.len() as i32, work: 0, work_stop: 0,
        }
    }
    fn is_site(&self, v: usize) -> bool { self.vias[v].trace >= 0 }
    fn trace_editable(&self, t: &Trace) -> bool { t.is_routed && !t.locked && (self.opt.edit_imported || !t.imported) }

    // cpp:148-182. ADAPTED cpp:152,178: OBoard stores Fab's resolved hole/edge
    // clearances, with no DRU/project override; retain effectiveHoleToHole's 0.25 floor.
    // ADAPTED cpp:159,166,174: disc clearances use the net's Via row, layer -1.
    fn site_fits(&self, b: &OBoard, net: i32, p: Vec2, dia: f32, drill: f32) -> bool {
        let rule = b.nets.get(net as usize).map_or(0, |n| n.via_rule);
        let rad = dia * 0.5f32 + self.opt.clearance_slack;
        let effective_hole_to_hole = if b.rules.hole_to_hole > 0.25f32 { b.rules.hole_to_hole } else { 0.25f32 };
        let hole_clr = std_max(b.rules.hole_clearance, effective_hole_to_hole);
        let reach = rad + std_max(b.clr.max, hole_clr) + self.max_half_width;
        let lo = Vec2::new(p.x - reach, p.y - reach);
        let hi = Vec2::new(p.x + reach, p.y + reach);
        let mut ok = true;
        query(&self.seg_grid, lo, hi, |i| {
            let s = &self.segs[i];
            if !ok || b.traces[s.trace].net == net { return; }
            let req = std_max(rad + b.clr.get(rule, s.rule, -1), drill * 0.5f32 + b.rules.hole_clearance + self.opt.clearance_slack) + s.hw;
            if drc::segment_segment_distance(p, p, s.a, s.b) < req { ok = false; }
        });
        if !ok { return false; }
        query(&self.pad_grid, Vec2::new(p.x - 0.1f32, p.y - 0.1f32), Vec2::new(p.x + 0.1f32, p.y + 0.1f32), |i| {
            let pad = &b.pads[i];
            if !ok { return; }
            let clr = if pad.net == net { b.rules.min_clearance } else { b.clr.get(rule, pad.rule, -1) };
            if drc::point_pad_distance(p, pad) < rad + clr { ok = false; }
        });
        if !ok { return false; }
        for o in &b.vias {
            let d = o.pos.distance_to(p);
            if d < (o.drill + drill) * 0.5f32 + hole_clr + self.opt.clearance_slack { return false; }
            if o.net != net && d < o.diameter * 0.5f32 + rad + b.clr.get(rule, o.rule, -1) { return false; }
        }
        for k in &b.keepouts {
            if k.no_vias && k.poly.len() >= 3 && drc::point_keepout_distance(k, p) < rad + drc::keepout_margin(k) { return false; }
        }
        let edge = b.rules.board_edge_clearance;
        let mut j = b.outline.len().wrapping_sub(1);
        for i in 0..b.outline.len() {
            if drc::segment_segment_distance(p, p, b.outline[j], b.outline[i]) < rad + edge { return false; }
            j = i;
        }
        true
    }

    // cpp:184-237.
    fn pick_sites(&self, b: &OBoard, t: usize, first_seg: usize, referenced: &[bool]) -> Vec<usize> {
        let tr = &b.traces[t];
        let n = tr.points.len();
        let mut out = Vec::new();
        let mut items = vec![Vec::<i32>::new(); n - 1];
        let mut any = false;
        for k in 0..n - 1 {
            let it = &mut items[k];
            for &j in &self.seg_conf[first_seg + k] { it.push(self.segs[j].trace as i32); }
            for &p in &self.seg_pads[first_seg + k] { it.push(-3 - p as i32); }
            any = any || !it.is_empty();
        }
        if !any { return out; }
        if tr.points[0].terminal_pad >= 0 { items[0].push(-1); }
        if tr.points[n - 1].terminal_pad >= 0 { items[n - 2].push(-2); }
        for it in &mut items {
            // ADAPTED cpp:205: integer comparator; equal item ids tie, then unique.
            it.sort_unstable_by(|a, b| a.cmp(b));
            it.dedup();
        }
        // ADAPTED cpp:208-209: the net's class sizes, resolved by the adapter.
        let (dia, drill) = via_size_for(b, tr.net);
        let mut legal = Vec::new();
        for k in 1..n - 1 {
            let p = &tr.points[k];
            if p.attached_via >= 0 || p.terminal_pad >= 0 || p.branch_trace >= 0 || p.is_fixed || p.straight_before || tr.points[k + 1].straight_before || referenced[k] { continue; }
            if self.site_fits(b, tr.net, p.pos, dia, drill) { legal.push(k); }
        }
        let mut acc = Vec::new();
        let mut next = Vec::new();
        let mut from = 0;
        for i in 0..legal.len() {
            let add_range = |to: &mut Vec<i32>, lo: usize, hi: usize| {
                for k in lo..hi { to.extend_from_slice(&items[k]); }
                // ADAPTED cpp:225: integer comparator, equal ids tie, then unique.
                to.sort_unstable_by(|a, b| a.cmp(b));
                to.dedup();
            };
            add_range(&mut acc, from, legal[i]);
            next.clear();
            add_range(&mut next, legal[i], if i + 1 < legal.len() { legal[i + 1] } else { n - 1 });
            from = legal[i];
            // std::includes on sorted, unique item vectors.
            if acc.is_empty() || next.is_empty() || next.iter().all(|x| acc.binary_search(x).is_ok()) { continue; }
            out.push(legal[i]);
            acc.clear();
        }
        out
    }

    fn new_unit(&mut self, tr: &Trace) -> usize {
        self.units.push(Unit { net: tr.net, layer: tr.layer, rule: tr.rule, ..Unit::default() });
        self.units.len() - 1
    }
    fn add_piece(&mut self, b: &OBoard, seg_unit: &mut [usize], first_seg: &[usize], u: usize, t: usize, lo: usize, hi: usize) {
        let tr = &b.traces[t];
        let unit = &mut self.units[u];
        unit.pieces.push(Piece { trace: t, lo, hi });
        unit.width = std_max(unit.width, tr.width);
        for k in lo..=hi { unit.box_.expand(tr.points[k].pos); }
        for k in lo..hi { seg_unit[first_seg[t] + k] = u; }
    }

    // cpp:239-501. Local helpers make the reference's lambda captures explicit.
    fn build(&mut self, b: &OBoard, stats: &mut LayerReassignStats, rejected: &BTreeSet<ViaKey>) {
        let n_t = b.traces.len();
        let mut parent: Vec<usize> = (0..n_t).collect();
        fn find(parent: &mut [usize], mut x: usize) -> usize {
            while parent[x] != x { parent[x] = parent[parent[x]]; x = parent[x]; }
            x
        }
        // ADAPTED cpp:249: unordered_map -> BTreeMap, lookup only, never iterated.
        let mut idx = BTreeMap::new();
        for t in 0..n_t { idx.insert(b.traces[t].id, t); }
        let live = |t: usize| b.traces[t].is_routed && b.traces[t].points.len() >= 2;
        // ADAPTED cpp:252: std::map -> BTreeMap; iterated below in (net, layer) key order.
        let mut groups: BTreeMap<(i32, i32), Vec<usize>> = BTreeMap::new();
        let mut box_ = vec![Box2::default(); n_t];
        let mut referenced: Vec<Vec<bool>> = b.traces.iter().map(|t| vec![false; t.points.len()]).collect();
        for t in 0..n_t {
            let tr = &b.traces[t];
            if !live(t) { continue; }
            for p in &tr.points { box_[t].expand(p.pos); }
            groups.entry((tr.net, tr.layer)).or_default().push(t);
            for p in &tr.points {
                if p.branch_trace < 0 { continue; }
                let Some(&host) = idx.get(&p.branch_trace) else { continue; };
                if p.branch_point >= 0 && (p.branch_point as usize) < referenced[host].len() { referenced[host][p.branch_point as usize] = true; }
                if p.attached_via < 0 && b.traces[host].layer == tr.layer {
                    let a = find(&mut parent, t);
                    let c = find(&mut parent, host);
                    parent[a] = c;
                }
            }
        }
        let end_on = |e: &TracePoint, host: &Trace| {
            for k in 0..host.points.len() - 1 {
                if drc::segment_segment_distance(e.pos, e.pos, host.points[k].pos, host.points[k + 1].pos) <= host.width * 0.5f32 { return true; }
            }
            false
        };
        for list in groups.values() {
            for a in 0..list.len() {
                for c in a + 1..list.len() {
                    let ta = list[a];
                    let tb = list[c];
                    if find(&mut parent, ta) == find(&mut parent, tb) || !boxes_touch(&box_[ta], &box_[tb], 0.5f32) { continue; }
                    let joined = |x: &Trace, y: &Trace| {
                        [&x.points[0], x.points.last().unwrap()].iter().any(|e| e.attached_via < 0 && e.terminal_pad < 0 && end_on(e, y))
                    };
                    if joined(&b.traces[ta], &b.traces[tb]) || joined(&b.traces[tb], &b.traces[ta]) {
                        let a = find(&mut parent, ta);
                        let c = find(&mut parent, tb);
                        parent[a] = c;
                    }
                }
            }
        }
        let mut group_size = vec![0; n_t];
        for t in 0..n_t { if live(t) { group_size[find(&mut parent, t)] += 1; } }
        let mut first_seg = vec![0; n_t];
        self.max_half_width = 0.0;
        for t in 0..n_t {
            first_seg[t] = self.segs.len();
            if !live(t) { continue; }
            let tr = &b.traces[t];
            self.max_half_width = std_max(self.max_half_width, tr.width * 0.5f32);
            for k in 0..tr.points.len() - 1 {
                // ADAPTED cpp:83-88,305: node is unused; rule carries the source row.
                self.segs.push(Seg { a: tr.points[k].pos, b: tr.points[k + 1].pos, trace: t, hw: tr.width * 0.5f32, rule: tr.rule });
            }
        }
        for i in 0..self.segs.len() {
            let s = &self.segs[i];
            insert_box(&mut self.seg_grid, Vec2::new(std_min(s.a.x, s.b.x), std_min(s.a.y, s.b.y)), Vec2::new(std_max(s.a.x, s.b.x), std_max(s.a.y, s.b.y)), i);
        }
        for (i, pad) in b.pads.iter().enumerate() {
            let g = std_max(pad.size.x, pad.size.y) * 0.5f32 + b.clr.max + 1.0f32;
            insert_box(&mut self.pad_grid, Vec2::new(pad.center.x - g, pad.center.y - g), Vec2::new(pad.center.x + g, pad.center.y + g), i);
        }
        self.seg_conf = vec![Vec::new(); self.segs.len()];
        self.seg_pads = vec![Vec::new(); self.segs.len()];
        let reach = 2.0f32 * self.max_half_width + b.clr.max + self.opt.clearance_slack;
        for i in 0..self.segs.len() {
            let s = self.segs[i];
            let net = b.traces[s.trace].net;
            let lo = Vec2::new(std_min(s.a.x, s.b.x) - reach, std_min(s.a.y, s.b.y) - reach);
            let hi = Vec2::new(std_max(s.a.x, s.b.x) + reach, std_max(s.a.y, s.b.y) + reach);
            query(&self.seg_grid, lo, hi, |j| {
                if j <= i { return; }
                let r = self.segs[j];
                if b.traces[r.trace].net == net || self.seg_conf[i].contains(&j) { return; }
                // ADAPTED cpp:332: NetClearances::get (Board.h:1243-1248) is layer-independent.
                // pick_sites (cpp:196,223-232) asks for conflicts on any layer before a
                // destination exists; unit edges below also have no candidate layer.
                // Keep -1: the maximum over Fab's layers is the conservative form.
                if drc::segment_segment_distance(s.a, s.b, r.a, r.b) < b.clr.get(s.rule, r.rule, -1) + s.hw + r.hw + self.opt.clearance_slack {
                    self.seg_conf[i].push(j);
                    self.seg_conf[j].push(i);
                }
            });
            query(&self.pad_grid, lo, hi, |p| {
                let pad = &b.pads[p];
                if pad.net == net || !pad.is_smd || self.seg_pads[i].contains(&p) { return; }
                // ADAPTED cpp:342: pick_sites (cpp:197,223-232) also asks for any-layer
                // pad signatures, with no destination layer. The reference is layer-independent;
                // keep -1, the conservative maximum over Fab's layers, as for seg_conf above.
                if drc::segment_pad_distance(s.a, s.b, pad) < b.clr.get(s.rule, pad.rule, -1) + s.hw + self.opt.clearance_slack { self.seg_pads[i].push(p); }
            });
        }
        let pair_net: Vec<bool> = (0..b.nets.len() as i32).map(|n| diffpairs::partner_of(b, n) >= 0).collect();
        let frozen_net = |net: i32| net >= 0 && (net as usize) < pair_net.len() && pair_net[net as usize];
        let n_v = b.vias.len();
        self.vias = (0..n_v).map(|_| ViaInfo::default()).collect();
        let mut seg_unit = vec![usize::MAX; self.segs.len()];
        // ADAPTED cpp:355: std::map -> BTreeMap; insertion/lookup only, never iterated.
        let mut unit_of_root = BTreeMap::new();
        for t in 0..n_t {
            if !live(t) { continue; }
            let tr = &b.traces[t];
            let last = tr.points.len() - 1;
            let root = find(&mut parent, t);
            let sites = if self.opt.split && group_size[root] == 1 && self.trace_editable(tr) && !frozen_net(tr.net) {
                self.pick_sites(b, t, first_seg[t], &referenced[t])
            } else { Vec::new() };
            if sites.is_empty() {
                let u = *unit_of_root.entry(root).or_insert_with(|| self.new_unit(tr));
                self.add_piece(b, &mut seg_unit, &first_seg, u, t, 0, last);
                continue;
            }
            let mut lo = 0;
            let mut prev = None;
            for k in sites {
                let u = self.new_unit(tr);
                self.add_piece(b, &mut seg_unit, &first_seg, u, t, lo, k);
                if let Some(prev) = prev { self.add_site(t, lo, prev, u); }
                prev = Some(u);
                lo = k;
            }
            let u = self.new_unit(tr);
            self.add_piece(b, &mut seg_unit, &first_seg, u, t, lo, last);
            self.add_site(t, lo, prev.unwrap(), u);
        }
        let mut ends = vec![0; n_v];
        let mut interior = vec![0; n_v];
        let mut awkward = vec![false; n_v];
        for t in 0..n_t {
            if !live(t) { continue; }
            let tr = &b.traces[t];
            let n = tr.points.len();
            for k in 0..n {
                let p = &tr.points[k];
                if p.attached_via < 0 || p.attached_via as usize >= n_v { continue; }
                let v = p.attached_via as usize;
                let u = seg_unit[first_seg[t] + std_min(k, n - 2)];
                if !self.vias[v].units.contains(&u) { self.vias[v].units.push(u); }
                if k == 0 || k == n - 1 {
                    ends[v] += 1;
                    if p.terminal_pad >= 0 { awkward[v] = true; }
                } else { interior[v] += 1; }
            }
        }
        let fanout = fanout_via_mask(b);
        // ADAPTED cpp:423: std::map -> BTreeMap, cached lookups only, never iterated.
        let mut tap_net = BTreeMap::new();
        let mut is_tap_net = |net| {
            *tap_net.entry(net).or_insert_with(|| {
                let mut tap = drc::plane_layer_of_net(b, net) >= 0;
                for z in &b.zones { if z.is_copper() && z.net == net { tap = true; } }
                tap
            })
        };
        for v in 0..n_v {
            let via = &b.vias[v];
            let mut fixed = via.locked || via.is_fixed || (via.imported && !self.opt.edit_imported) || awkward[v]
                || (v < fanout.len() && fanout[v]) || is_tap_net(via.net) || frozen_net(via.net) || rejected.contains(&via_key(via));
            if interior[v] > 1 || ends[v] + interior[v] < 2 { fixed = true; }
            if !fixed {
                query(&self.pad_grid, via.pos, via.pos, |i| {
                    let pad = &b.pads[i];
                    if pad.net == via.net && drc::point_pad_distance(via.pos, pad) < via.diameter * 0.5f32 { fixed = true; }
                });
            }
            self.vias[v].fixed = fixed;
        }
        for v in 0..self.vias.len() { for &u in &self.vias[v].units { self.units[u].vias.push(v); } }
        let mut signal = 0u32;
        for l in 0..std_min(self.n_l, 31) {
            // ADAPTED cpp:452: every layer in OBoard is enabled (there is no enabled field).
            if !drc::is_plane_layer(b, l) { signal |= 1u32 << l; }
        }
        for u in &mut self.units {
            u.allowed = signal | if u.layer < 31 { 1u32 << u.layer } else { 0 };
            // ADAPTED cpp:457-459: Fab has neither routing intents nor KiCad DRU;
            // both allowed-layer lookups give zero, so this branch never narrows.
            let allow: u32 = 0;
            if allow != 0 {
                for l in 0..std_min(self.n_l, 31) { if (allow >> l & 1) == 0 && l != u.layer { u.allowed &= !(1u32 << l); } }
            }
            for &v in &u.vias {
                if v < n_v {
                    for l in 0..std_min(self.n_l, 31) {
                        // ADAPTED cpp:463: viaThrough is false; retain every span check.
                        if !drc::via_on_layer(&b.vias[v], l) { u.allowed &= !(1u32 << l); }
                    }
                }
            }
            // ADAPTED cpp:464-466: no per-layer track width/width intents;
            // every layer has the same width, so this test removes no layer.
            for pc in &u.pieces {
                let tr = &b.traces[pc.trace];
                if !(tr.is_routed && !tr.locked && (self.opt.edit_imported || !tr.imported)) { u.pinned = true; }
            }
            if frozen_net(u.net) { u.pinned = true; }
            let mut seen = Vec::new();
            query(&self.pad_grid, u.box_.min, u.box_.max, |i| {
                let pad = &b.pads[i];
                if pad.net != u.net || !drc::pad_on_layer(pad, u.layer, self.n_l) || seen.contains(&i) { return; }
                seen.push(i);
                let mut touches = false;
                for pc in &u.pieces {
                    let tr = &b.traces[pc.trace];
                    let mut k = pc.lo;
                    while k < pc.hi && !touches {
                        touches = drc::segment_pad_distance(tr.points[k].pos, tr.points[k + 1].pos, pad) <= tr.width * 0.5f32;
                        k += 1;
                    }
                }
                if !touches { return; }
                for l in 0..std_min(self.n_l, 31) { if !drc::pad_on_layer(pad, l, self.n_l) { u.allowed &= !(1u32 << l); } }
            });
            if u.allowed & u.allowed.wrapping_sub(1) == 0 { u.pinned = true; }
        }
        // ADAPTED cpp:491: std::set -> BTreeSet; iterated in sorted (unit, unit) key order.
        let mut edges = BTreeSet::new();
        for i in 0..self.segs.len() {
            for &j in &self.seg_conf[i] {
                if seg_unit[i] != seg_unit[j] { edges.insert((std_min(seg_unit[i], seg_unit[j]), std_max(seg_unit[i], seg_unit[j]))); }
            }
        }
        for &(a, c) in &edges { self.units[a].conflicts.push(c); self.units[c].conflicts.push(a); }
        if stats.conflicts == 0 { stats.conflicts = edges.len() as i32; }
    }

    // cpp:502-510.
    fn add_site(&mut self, t: usize, node: usize, a: usize, b: usize) {
        self.vias.push(ViaInfo { units: vec![a, b], fixed: false, trace: t as i32, node: node as i32 });
    }

    // cpp:512-559.
    fn compute_blocked(&self, b: &OBoard, u: &Unit, layer: i32) -> bool {
        let wide = u.box_;
        let g = u.width + 2.0f32;
        let visit = |seg_test: &mut dyn FnMut(Vec2, Vec2, f32) -> bool| {
            for pc in &u.pieces {
                let tr = &b.traces[pc.trace];
                for k in pc.lo..pc.hi {
                    if seg_test(tr.points[k].pos, tr.points[k + 1].pos, tr.width * 0.5f32 + self.opt.clearance_slack) { return true; }
                }
            }
            false
        };
        for pad in &b.pads {
            if pad.net == u.net && u.net >= 0 { continue; }
            if !drc::pad_on_layer(pad, layer, self.n_l) || drc::pad_on_layer(pad, u.layer, self.n_l) { continue; }
            let ext = std_max(pad.size.x, pad.size.y) * 0.5f32 + g;
            if pad.center.x + ext < wide.min.x || pad.center.x - ext > wide.max.x || pad.center.y + ext < wide.min.y || pad.center.y - ext > wide.max.y { continue; }
            // ADAPTED cpp:533: the tested unit's row, pad row and destination layer.
            let clr = b.clr.get(u.rule, pad.rule, layer);
            if visit(&mut |a, c, h| drc::segment_pad_distance(a, c, pad) < clr + h) { return true; }
        }
        for via in &b.vias {
            if via.net == u.net || !drc::via_on_layer(via, layer) || drc::via_on_layer(via, u.layer) { continue; }
            let clr = b.clr.get(u.rule, via.rule, layer) + via.diameter * 0.5f32;
            if visit(&mut |a, c, h| drc::segment_segment_distance(a, c, via.pos, via.pos) < clr + h) { return true; }
        }
        for k in &b.keepouts {
            if !k.no_tracks || !drc::keepout_on_layer(b, k, layer) { continue; }
            if visit(&mut |a, c, h| drc::segment_keepout_distance(k, a, c) < drc::keepout_margin(k) + h) { return true; }
        }
        // ADAPTED cpp:547-552: OBoard has no free copper shapes; Fab's copper
        // graphics/texts are outside this model and are checked by Fab's DRC (Guard).
        for z in &b.zones {
            // ADAPTED cpp:555: Zone's mask already holds copper stack-index bits.
            if !z.is_copper() || z.net == u.net || z.layer_mask & drc::layer_mask_for_index(layer, self.n_l) == 0 { continue; }
            if visit(&mut |a, c, _| drc::segment_polygon_distance(&z.outline, a, c) <= 0.0f32) { return true; }
        }
        false
    }

    // cpp:560-565.
    fn blocked(&mut self, b: &OBoard, u: usize, layer: i32) -> bool {
        if self.blocked_at[u][layer as usize] < 0 {
            self.blocked_at[u][layer as usize] = self.compute_blocked(b, &self.units[u], layer) as i8;
        }
        self.blocked_at[u][layer as usize] != 0
    }

    // cpp:567-595.
    fn via_needed(&self, v: usize) -> bool {
        let info = &self.vias[v];
        if info.fixed { return true; }
        for i in 1..info.units.len() { if self.cur[info.units[i]] != self.cur[info.units[0]] { return true; } }
        false
    }
    fn set_layer(&mut self, u: usize, layer: i32) {
        self.cur[u] = layer;
        for i in 0..self.units[u].vias.len() {
            let v = self.units[u].vias[i];
            let n = self.via_needed(v);
            self.needed_count += n as i32 - self.needed[v] as i32;
            self.needed[v] = n;
        }
    }
    fn undo_to(&mut self, log: &mut Vec<Change>, mark: usize) {
        while log.len() > mark {
            let change = log.last().unwrap();
            self.set_layer(change.unit, change.from);
            self.stamp[change.unit] = 0;
            log.pop();
        }
    }

    // cpp:596-633. Everything stamped this move stays where it was placed.
    fn place(&mut self, b: &OBoard, u: usize, layer: i32, log: &mut Vec<Change>, move_start: usize) -> bool {
        self.work += 1;
        if self.work > self.work_stop { return false; }
        if self.stamp[u] == self.epoch { return self.cur[u] == layer; }
        if self.cur[u] == layer {
            self.stamp[u] = self.epoch;
            log.push(Change { unit: u, from: layer });
            return true;
        }
        if self.units[u].pinned || self.units[u].allowed >> layer & 1 == 0 || self.blocked(b, u, layer) { return false; }
        if log.len() - move_start >= self.opt.max_closure as usize { return false; }
        let from = self.cur[u];
        self.stamp[u] = self.epoch;
        log.push(Change { unit: u, from });
        self.set_layer(u, layer);
        for i in 0..self.units[u].conflicts.len() {
            let w = self.units[u].conflicts[i];
            if self.cur[w] != layer { continue; }
            if self.stamp[w] == self.epoch { return false; }
            let mut ok = false;
            let mark = log.len();
            for pass in 0..2 {
                if ok { break; }
                for l in 0..std_min(self.n_l, 31) {
                    if ok { break; }
                    if l == layer || (pass == 0) != (l == from) { continue; }
                    if self.place_run(b, w, l, log, move_start) { ok = true; }
                    else { self.undo_to(log, mark); }
                }
            }
            if !ok { return false; }
        }
        true
    }

    // cpp:635-666: follow a wire up to the next real via only when cheaper.
    fn place_run(&mut self, b: &OBoard, u: usize, layer: i32, log: &mut Vec<Change>, move_start: usize) -> bool {
        if !self.place(b, u, layer, log, move_start) { return false; }
        for i in 0..self.units[u].vias.len() {
            let s = self.units[u].vias[i];
            if !self.is_site(s) || !self.needed[s] { continue; }
            let mark = log.len();
            let cost = self.needed_count;
            let mut at = u;
            let mut via = s;
            let mut ok = true;
            loop {
                let pair = &self.vias[via].units;
                let next = if pair[0] == at { pair[1] } else { pair[0] };
                if !self.place(b, next, layer, log, move_start) { ok = false; break; }
                let mut on = None;
                for &w in &self.units[next].vias { if self.is_site(w) && w != via { on = Some(w); } }
                let Some(on) = on else { break; };
                at = next;
                via = on;
            }
            if !ok || self.needed_count >= cost { self.undo_to(log, mark); }
        }
        true
    }

    // cpp:668-680.
    fn move_via(&mut self, b: &OBoard, v: usize, layer: i32, log: &mut Vec<Change>) -> bool {
        self.epoch += 1;
        let start = log.len();
        for i in 0..self.vias[v].units.len() {
            let u = self.vias[v].units[i];
            if !self.place_run(b, u, layer, log, start) { self.undo_to(log, start); return false; }
        }
        true
    }

    // cpp:682-688.
    fn frontier(&self, log: &[Change], start: usize, out: &mut Vec<usize>) {
        for change in &log[start..] {
            for &v in &self.units[change.unit].vias {
                if self.needed[v] && !self.vias[v].fixed && !out.contains(&v) { out.push(v); }
            }
        }
    }

    // cpp:690-727: state stays on success.
    fn improve(&mut self, b: &OBoard, v: usize, depth: i32, goal: i32, log: &mut Vec<Change>, path: &mut Vec<usize>) -> bool {
        let mut layers = Vec::new();
        for &u in &self.vias[v].units { if !layers.contains(&self.cur[u]) { layers.push(self.cur[u]); } }
        // ADAPTED cpp:697: same integer comparator, distinct layer indices (no ties).
        layers.sort_unstable_by(|a, b| a.cmp(b));
        path.push(v);
        for layer in layers {
            if self.work > self.work_stop { break; }
            let mark = log.len();
            if !self.move_via(b, v, layer, log) { continue; }
            if self.needed_count < goal { path.pop(); return true; }
            if depth > 1 && self.needed_count <= goal + 1 && log.len() > mark {
                let mut next = Vec::new();
                self.frontier(log, mark, &mut next);
                for w in next {
                    if path.contains(&w) { continue; }
                    if self.improve(b, w, depth - 1, goal, log, path) { path.pop(); return true; }
                }
            }
            self.undo_to(log, mark);
        }
        path.pop();
        false
    }

    // cpp:729-743.
    fn reset(&mut self) {
        self.cur = self.units.iter().map(|u| u.layer).collect();
        self.blocked_at = vec![vec![-1; self.n_l as usize]; self.units.len()];
        self.stamp = vec![0; self.units.len()];
        self.epoch = 0;
        self.needed = vec![false; self.vias.len()];
        self.needed_count = 0;
        for v in 0..self.vias.len() { self.needed[v] = self.via_needed(v); self.needed_count += self.needed[v] as i32; }
    }
}

// cpp:586.
struct Change { unit: usize, from: i32 }

fn touch(t: &mut Trace) {
    t.smoothed.clear();
    t.point_widths.clear();
    t.teardrop_front.clear();
    t.teardrop_back.clear();
}

fn trace_of(b: &OBoard, id: i32) -> usize { b.traces.iter().position(|t| t.id == id).unwrap() }

// cpp:746-848: two ends merge, further ends branch onto their joint.
fn remove_via(b: &mut OBoard, v: usize, dead: &mut [bool]) -> bool {
    let mut ends = Vec::new();
    let mut host_id = -1;
    let mut host_node = -1;
    for t in &mut b.traces {
        let n = t.points.len();
        for k in 0..n {
            if t.points[k].attached_via == v as i32 {
                t.points[k].branch_trace = -1;
                t.points[k].branch_point = -1;
                if k == 0 || k == n - 1 { ends.push((t.id, if k == 0 { 0 } else { 1 })); }
                else { host_id = t.id; host_node = k as i32; }
            }
        }
    }
    let at = b.vias[v].pos;
    if host_id < 0 {
        let mut pair = None;
        for i in 0..ends.len() {
            if pair.is_some() { break; }
            for j in i + 1..ends.len() {
                let p = &b.traces[trace_of(b, ends[i].0)];
                let q = &b.traces[trace_of(b, ends[j].0)];
                if p.id == q.id || (p.width - q.width).abs() > 1e-4f32 { continue; }
                let mut cross_ref = false;
                for pt in &p.points { cross_ref = cross_ref || pt.branch_trace == q.id; }
                for pt in &q.points { cross_ref = cross_ref || pt.branch_trace == p.id; }
                if cross_ref { continue; }
                pair = Some((i, j));
                break;
            }
        }
        let Some((a, c)) = pair else { return false; };
        let p = trace_of(b, ends[a].0);
        let q = trace_of(b, ends[c].0);
        let pid = b.traces[p].id;
        let qid = b.traces[q].id;
        let np = b.traces[p].points.len() as i32;
        let nq = b.traces[q].points.len() as i32;
        let rev_p = ends[a].1 == 0;
        let rev_q = ends[c].1 == 1;
        if rev_p { b.traces[p].points = reversed_points(&b.traces[p].points); }
        if rev_q { b.traces[q].points = reversed_points(&b.traces[q].points); }
        let mut merged = b.traces[p].points.clone();
        let j = merged.last_mut().unwrap();
        j.attached_via = -1;
        j.is_fixed = false;
        j.pos = at;
        merged.extend_from_slice(&b.traces[q].points[1..]);
        b.traces[p].points = merged;
        host_id = pid;
        host_node = np - 1;
        touch(&mut b.traces[p]);
        for t in &mut b.traces {
            for pt in &mut t.points {
                if pt.branch_trace == pid && rev_p { pt.branch_point = np - 1 - pt.branch_point; }
                if pt.branch_trace == qid {
                    pt.branch_trace = pid;
                    pt.branch_point = (np - 1) + if rev_q { nq - 1 - pt.branch_point } else { pt.branch_point };
                }
            }
        }
        b.traces.remove(trace_of(b, qid));
        ends = ends.into_iter().enumerate().filter_map(|(i, e)| if i != a && i != c { Some(e) } else { None }).collect();
    } else {
        let h = trace_of(b, host_id);
        b.traces[h].points[host_node as usize].attached_via = -1;
        touch(&mut b.traces[h]);
    }
    for (id, e) in ends {
        let t = trace_of(b, id);
        let n = b.traces[t].points.len();
        let p = &mut b.traces[t].points[if e == 0 { 0 } else { n - 1 }];
        p.attached_via = -1;
        p.is_fixed = false;
        p.pos = at;
        p.branch_trace = host_id;
        p.branch_point = host_node;
        touch(&mut b.traces[t]);
    }
    dead[v] = true;
    true
}

// cpp:850-951: split only at sites that now need a via.
fn apply_layers(b: &mut OBoard, m: &Model<'_>, dead: &mut Vec<bool>) {
    let n_l = b.layers.len() as i32;
    // ADAPTED cpp:854,857: std::map -> BTreeMap. pieces is iterated in trace-index
    // key order; cuts is lookup-only, never iterated. Vectors retain insertion order.
    let mut pieces: BTreeMap<usize, Vec<(usize, i32)>> = BTreeMap::new();
    for u in 0..m.units.len() { for pc in &m.units[u].pieces { pieces.entry(pc.trace).or_default().push((pc.lo, m.cur[u])); } }
    let mut cuts: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let n_real = b.vias.len();
    for v in n_real..m.vias.len() { if m.needed[v] { cuts.entry(m.vias[v].trace as usize).or_default().push(m.vias[v].node as usize); } }
    let mut next_id = 0;
    for t in &b.traces { next_id = std_max(next_id, t.id + 1); }
    let retarget = |t: &mut Trace, layer: i32| {
        // ADAPTED cpp:866-867: Trace stores only the copper stack index (no named mask).
        if t.layer != layer { t.layer = layer; }
        touch(t);
    };
    for (t, mut list) in pieces {
        // ADAPTED cpp:876: same lexicographic (first node, layer) comparator;
        // distinct piece starts, no ties.
        list.sort_unstable_by(|a, b| a.cmp(b));
        let layer_at = |node: usize| {
            let mut l = list[0].1;
            for &(lo, layer) in &list { if lo <= node { l = layer; } }
            l
        };
        let Some(cut) = cuts.get(&t) else {
            if b.traces[t].layer != list[0].1 { retarget(&mut b.traces[t], list[0].1); }
            continue;
        };
        let mut nodes = cut.clone();
        // ADAPTED cpp:890: same integer comparator; each site is unique, no ties.
        nodes.sort_unstable_by(|a, b| a.cmp(b));
        let proto = b.traces[t].clone();
        let pts = &proto.points;
        let mut remap = vec![(0, proto.id)];
        let mut prev_via = -1;
        for c in 0..=nodes.len() {
            let lo = if c == 0 { 0 } else { nodes[c - 1] };
            let hi = if c == nodes.len() { pts.len() - 1 } else { nodes[c] };
            let mut via = -1;
            if c < nodes.len() {
                // ADAPTED cpp:902-910: viaSizeFor -> adapter-resolved net-class sizes;
                // the net's Via row describes the new copper. OBoard's layer_start/end
                // are stack indices, so there are no separate named-mask fields to set
                // with layerMaskForIndex. Via's reference-default kind is Auto.
                let (diameter, drill) = via_size_for(b, proto.net);
                let nv = Via {
                    id: b.vias.len() as i32, net: proto.net, pos: pts[hi].pos,
                    diameter, drill, layer_start: 0, layer_end: std_max(0, n_l - 1),
                    rule: b.nets.get(proto.net as usize).map_or(0, |n| n.via_rule),
                    ..Via::default()
                };
                via = nv.id;
                b.vias.push(nv);
                dead.push(false);
            }
            let mut part = pts[lo..=hi].to_vec();
            if c > 0 { part[0].attached_via = prev_via; part[0].straight_before = false; }
            if c < nodes.len() { part.last_mut().unwrap().attached_via = via; }
            if c == 0 { b.traces[t].points = part; retarget(&mut b.traces[t], layer_at(lo)); }
            else {
                let mut nt = proto.clone();
                nt.id = next_id;
                next_id += 1;
                nt.points = part;
                remap.push((lo, nt.id));
                retarget(&mut nt, layer_at(lo));
                b.traces.push(nt);
            }
            prev_via = via;
        }
        for tr in &mut b.traces {
            for p in &mut tr.points {
                if p.branch_trace != proto.id { continue; }
                for &(lo, id) in remap.iter().rev() {
                    if p.branch_point >= lo as i32 { p.branch_trace = id; p.branch_point -= lo as i32; break; }
                }
            }
        }
    }
}

// cpp:953-968.
fn compact_vias(b: &mut OBoard, dead: &[bool]) {
    let mut remap = vec![-1; b.vias.len()];
    let mut kept = Vec::new();
    for i in 0..b.vias.len() {
        if dead[i] { continue; }
        remap[i] = kept.len() as i32;
        kept.push(b.vias[i].clone());
        let id = kept.len() as i32 - 1;
        kept.last_mut().unwrap().id = id;
    }
    b.vias = kept;
    for tr in &mut b.traces {
        for p in &mut tr.points {
            if p.attached_via >= 0 { p.attached_via = if (p.attached_via as usize) < remap.len() { remap[p.attached_via as usize] } else { -1 }; }
        }
    }
}

/// cpp:983-1088: every kept change removes a via and passes the hop check and judge.
pub fn run(board: &mut OBoard, opt: &LayerReassignOptions, stats: &mut LayerReassignStats, guard: &mut dyn Guard) -> bool {
    // ADAPTED cpp:985,1086: no clock/time. cpp:989-992: borrowed options, points
    // throughout; no DRC options/report or initial judge call: Guard holds that state.
    *stats = LayerReassignStats::default();
    stats.vias_before = board.vias.len() as i32;
    stats.vias_after = board.vias.len() as i32;
    if opt.depth <= 0 { return true; }
    // ADAPTED cpp:993: std::set -> BTreeSet; membership only, never iterated.
    let mut rejected = BTreeSet::new();
    let mut first = true;
    let mut work = 0;
    let mut resume = 0;
    // ADAPTED h:70,72,92; cpp:1028-1034,1039-1043: no maxWork/maxDrcRuns or
    // budgetHit. The loop ends: a kept change removes a via; a refused seed enters
    // rejected and is never retried. A full sweep without an improvement ends it.
    loop {
        let mut m = Model::new(board, opt);
        m.work = work;
        m.build(board, stats, &rejected);
        m.reset();
        if first {
            first = false;
            stats.units = m.units.len() as i32;
            for u in &m.units { stats.pinned_units += u.pinned as i32; }
            for v in 0..board.vias.len() { stats.vias_movable += (!m.vias[v].fixed) as i32; }
            stats.sites = m.vias.len() as i32 - board.vias.len() as i32;
        }
        let mut log = Vec::new();
        let start = m.needed_count;
        let mut found = false;
        let mut seed = 0;
        let n_v = board.vias.len();
        for i in 0..n_v {
            if found { break; }
            let v = (resume + i) % n_v;
            if m.vias[v].fixed { continue; }
            seed = v;
            if !m.needed[v] { found = true; break; }
            let mut path = Vec::new();
            // ADAPTED cpp:1028-1034: bound this seed alone; work is an accumulating
            // counter, never a whole-pass stopping decision. Exhaustion skips the seed.
            m.work_stop = m.work + opt.max_seed_work;
            found = m.improve(board, v, std_max(1, opt.depth), start, &mut log, &mut path);
            if m.work > m.work_stop {
                m.undo_to(&mut log, 0);
                found = false;
                stats.seed_limit += 1;
            }
        }
        work = m.work;
        stats.work = work;
        if !found { break; }
        stats.found += 1;
        resume = seed;
        let traces0 = board.traces.clone();
        let vias0 = board.vias.clone();
        let seed_via = board.vias[seed].clone();
        let mut moved = 0;
        let mut nets = BTreeSet::new();
        for u in 0..m.units.len() {
            if m.cur[u] != m.units[u].layer { moved += 1; nets.insert(m.units[u].net); }
        }
        let hops0 = count_hops(board, 3.0f32, false, None, None, false).real;
        let mut dead = vec![false; board.vias.len()];
        let mut applied = true;
        apply_layers(board, &m, &mut dead);
        for v in 0..n_v {
            if !m.vias[v].fixed && !m.needed[v] && !remove_via(board, v, &mut dead) { applied = false; }
        }
        // ADAPTED cpp:1063-1065: this is exactly the copper differing from traces0 /
        // vias0: changed-layer units (including split pieces), removed real vias and
        // added site vias. Merges/branches belong to the removed via's net; compaction
        // changes indices, not copper. BTreeSet iterates once in sorted net order, so a
        // multi-net closure is judged together as one change, accepted or refused together.
        for v in 0..n_v { if dead[v] { nets.insert(vias0[v].net); } }
        for v in n_v..board.vias.len() { nets.insert(board.vias[v].net); }
        let nets: Vec<i32> = nets.into_iter().collect();
        compact_vias(board, &dead);
        let mut ok = false;
        if applied && board.vias.len() < vias0.len() {
            // ADAPTED cpp:1061-1066: as in ViaMerge, hops run FIRST. An accepting
            // judge takes this state as its own, so nothing may refuse it afterwards.
            ok = count_hops(board, 3.0f32, false, None, None, false).real <= hops0;
            if ok { ok = guard.judge(board, &nets); stats.judge_calls += 1; }
        }
        // ADAPTED cpp:1067-1069,1074: no printing or retained DRC report.
        if ok { stats.kept += 1; stats.units_moved += moved; }
        else {
            stats.rolled_back += 1;
            board.traces = traces0;
            board.vias = vias0;
            rejected.insert(via_key(&seed_via));
        }
    }
    stats.vias_after = board.vias.len() as i32;
    stats.removed = stats.vias_before - stats.vias_after;
    // ADAPTED cpp:1087-1088: true; every kept change was judged not worse,
    // and every refused application was restored bit for bit.
    true
}

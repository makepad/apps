// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/NeckdownTeardrop.h and .cpp:1-1538.
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Neckdown obstacle indices, polygon/fillet helpers and exported trace width profiles.
//!
//! ADAPTED: one serial `Ctx`, no WorkerQ/copy constructor.
//! Scratch fields are borrowed separately rather than aliased through `X`. ClearanceTable
//! replaces NetClearances; `cur_rule` must be set to the processed trace's rule before collect.
//! BTreeMaps replace unordered maps (their consumers iterate in ascending key order).
//! Board width policy is pre-resolved data; no KiCad rule evaluator is introduced here.
//! The teardrop half lives in `neckdown_td`; its combined pass is re-exported here.

use std::collections::BTreeMap;

use super::copper::{pad_model, pad_on_layer, seg_hole, seg_pad, seg_seg, PadModel, K_BIG};
use super::geom::{std_clamp, std_max, std_min, Box2, Vec2};
use super::grid::{UniformGrid, UNIFORM_GRID_MAX_CELLS};
use super::model::{ClearanceTable, NeckdownIssue, OBoard, Pad, Trace};
use super::opt::drcgeom;
use super::padgeom;
use super::settings::OrganicSettings;

pub use super::neckdown_td::neckdowns_and_teardrops;

pub(crate) fn finite_v(p: Vec2) -> bool { p.x.is_finite() && p.y.is_finite() }

/// Even-odd test; boundary classification follows the reference's strict ray inequalities.
pub fn point_in_poly(poly: &[Vec2], p: Vec2) -> bool {
    if poly.len() < 3 { return false; }
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[j];
        if (a.y > p.y) != (b.y > p.y) {
            let xi = (b.x - a.x) as f64 * (p.y - a.y) as f64 / (b.y - a.y) as f64 + a.x as f64;
            if (p.x as f64) < xi { inside = !inside; }
        }
        j = i;
    }
    inside
}

pub fn poly_seg_dist(poly: &[Vec2], a: Vec2, b: Vec2) -> f64 {
    if point_in_poly(poly, a) || point_in_poly(poly, b) { return 0.0; }
    let mut d = K_BIG;
    for i in 0..poly.len() { d = std_min(d, seg_seg(poly[i], poly[(i + 1) % poly.len()], a, b).d); }
    d
}

pub fn proper_cross(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> bool {
    let o = |p: Vec2, q: Vec2, r: Vec2| {
        (q.x - p.x) as f64 * (r.y - p.y) as f64 - (q.y - p.y) as f64 * (r.x - p.x) as f64
    };
    let d1 = o(a, b, c);
    let d2 = o(a, b, d);
    let d3 = o(c, d, a);
    let d4 = o(c, d, b);
    ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0)) && d1 != 0.0 && d2 != 0.0 && d3 != 0.0 && d4 != 0.0
}

pub fn simple_polygon(p: &[Vec2]) -> bool {
    let n = p.len();
    if n < 3 { return false; }
    for i in 0..n {
        for j in i + 2..n {
            if i == 0 && j == n - 1 { continue; }
            if proper_cross(p[i], p[(i + 1) % n], p[j], p[(j + 1) % n]) { return false; }
        }
    }
    true
}

#[derive(Clone, Debug)]
pub(crate) struct Seg {
    pub a: Vec2, pub b: Vec2, pub h: f32, pub net: i32, pub layer: i32, pub trace: i32,
}
#[derive(Clone, Debug)]
pub(crate) struct PadE { pub m: PadModel, pub id: i32 }
#[derive(Clone, Debug)]
pub(crate) struct ViaE {
    pub c: Vec2, pub r: f32, pub drill: f32, pub net: i32,
    /// ADAPTED: obstacle rule row for ClearanceTable (the reference uses its net).
    pub rule: u16,
}
#[derive(Clone, Debug, Default)]
pub(crate) struct Poly {
    pub pts: Vec<Vec2>, pub bb: Box2, pub net: i32, pub layer: i32, pub trace: i32,
}

pub(crate) fn exported_path(tr: &Trace, pts: &mut Vec<Vec2>, w: &mut Vec<f32>) {
    pts.clear();
    w.clear();
    if tr.smoothed.len() >= 2 {
        pts.clone_from(&tr.smoothed);
        if tr.point_widths.len() == pts.len() { w.clone_from(&tr.point_widths); }
    } else {
        for p in &tr.points { pts.push(p.pos); }
    }
    if w.len() != pts.len() { w.clear(); w.resize(pts.len(), tr.width); }
}

/// `NeckdownCache::Scratch`: reusable serial obstacle-index buffers.
#[derive(Default)]
pub struct Scratch {
    pub(crate) segs: Vec<Seg>, pub(crate) pads: Vec<PadE>, pub(crate) vias: Vec<ViaE>,
    pub(crate) edges: Vec<(Vec2, Vec2)>,
    pub(crate) grid: UniformGrid, pub(crate) pgrid: UniformGrid,
    pub(crate) stamp: Vec<u32>, pub(crate) pstamp: Vec<u32>,
    pub(crate) polys: Vec<Poly>, pub(crate) late: Vec<i32>,
    pub(crate) rbits: Vec<u8>, pub(crate) rsum: Vec<i32>,
}

#[derive(Default)]
pub struct NeckdownCache {
    pub(crate) path_box: Vec<Box2>, pub(crate) td_box: Vec<Box2>,
    pub(crate) stamp: Vec<u64>, pub(crate) nominal: Vec<Vec<Vec2>>, pub(crate) ids: Vec<i32>,
    pub(crate) index: BTreeMap<i32, i32>,
    pub(crate) ncl: ClearanceTable, pub(crate) ncl_valid: bool,
    pub(crate) fixed_width: Vec<u8>, pub(crate) scratch: Option<Box<Scratch>>,
}

impl Clone for NeckdownCache {
    fn clone(&self) -> Self {
        Self {
            path_box: self.path_box.clone(), td_box: self.td_box.clone(), stamp: self.stamp.clone(),
            nominal: self.nominal.clone(), ids: self.ids.clone(), index: self.index.clone(),
            ncl: self.ncl.clone(), ncl_valid: self.ncl_valid,
            // Reference copy constructor omits fixedWidth and scratch.
            fixed_width: Vec::new(), scratch: None,
        }
    }
    fn clone_from(&mut self, o: &Self) {
        self.path_box.clone_from(&o.path_box);
        self.td_box.clone_from(&o.td_box);
        self.stamp.clone_from(&o.stamp);
        self.nominal.clone_from(&o.nominal);
        self.ids.clone_from(&o.ids);
        self.index.clone_from(&o.index);
        self.ncl.clone_from(&o.ncl);
        self.ncl_valid = o.ncl_valid;
        self.scratch = None;
        // Reference copy assignment leaves the destination's fixedWidth unchanged.
    }
}

/// Nonzero sampled stamp of exported outputs, not a complete path hash (C++:206-244).
pub fn output_stamp(tr: &Trace) -> u64 {
    fn mix(h: &mut u64, v: u64) {
        *h = (*h ^ v).wrapping_mul(0xff51afd7ed558ccd);
        *h ^= *h >> 33;
    }
    fn sampled(h: &mut u64, n: usize, at: impl Fn(usize) -> u64) {
        mix(h, n as u64);
        if n == 0 { return; }
        let step = std_max(1, n / 16);
        for i in (0..n).step_by(step) { mix(h, at(i)); }
        mix(h, at(n - 1));
    }
    let mix_v = |v: Vec2| ((v.x.to_bits() as u64) << 32) | v.y.to_bits() as u64;
    let mut h = 0x9E3779B97F4A7C15;
    mix(&mut h, tr.id as i64 as u64);
    mix(&mut h, tr.net as i64 as u64);
    mix(&mut h, tr.layer as i64 as u64);
    mix(&mut h, tr.width.to_bits() as u64);
    mix(&mut h, tr.is_routed as u64);
    sampled(&mut h, tr.smoothed.len(), |i| mix_v(tr.smoothed[i]));
    sampled(&mut h, tr.point_widths.len(), |i| tr.point_widths[i].to_bits() as u64);
    sampled(&mut h, tr.teardrop_front.len(), |i| mix_v(tr.teardrop_front[i]));
    sampled(&mut h, tr.teardrop_back.len(), |i| mix_v(tr.teardrop_back[i]));
    if tr.smoothed.len() < 2 { sampled(&mut h, tr.points.len(), |i| mix_v(tr.points[i].pos)); }
    else { mix(&mut h, tr.points.len() as u64); }
    if h != 0 { h } else { 1 }
}

pub(crate) fn path_near_edges(p: &[Vec2], n_e: usize, edge_at: impl Fn(usize) -> (Vec2, Vec2), half_w: f64, clr: f64) -> bool {
    if p.len() < 2 || n_e == 0 { return false; }
    let thr = half_w + clr + 1e-4;
    let mut pb = Box2::default();
    for &v in p { pb.expand(v); }
    let gap = |a: &Box2, x0: f32, y0: f32, x1: f32, y1: f32| {
        let dx = std_max(std_max(0.0, x0 as f64 - a.max.x as f64), a.min.x as f64 - x1 as f64);
        let dy = std_max(std_max(0.0, y0 as f64 - a.max.y as f64), a.min.y as f64 - y1 as f64);
        (dx * dx + dy * dy).sqrt()
    };
    for e in 0..n_e {
        let a = edge_at(e).0;
        let b = edge_at(e).1;
        let (x0, y0, x1, y1) = (std_min(a.x, b.x), std_min(a.y, b.y), std_max(a.x, b.x), std_max(a.y, b.y));
        if pb.is_valid() && gap(&pb, x0, y0, x1, y1) > thr { continue; }
        for i in 0..p.len() - 1 {
            let mut sb = Box2::default();
            sb.expand(p[i]); sb.expand(p[i + 1]);
            if sb.is_valid() && gap(&sb, x0, y0, x1, y1) > thr { continue; }
            if seg_seg(p[i], p[i + 1], a, b).d - half_w < clr { return true; }
        }
    }
    false
}

pub(crate) fn exported_box(tr: &Trace) -> Box2 {
    let mut bb = Box2::default();
    if tr.smoothed.len() >= 2 {
        for &p in &tr.smoothed { if finite_v(p) { bb.expand(p); } }
    } else {
        for p in &tr.points { if finite_v(p.pos) { bb.expand(p.pos); } }
    }
    bb
}

pub(crate) fn teardrop_box(tr: &Trace) -> Box2 {
    let mut bb = Box2::default();
    for td in [&tr.teardrop_front, &tr.teardrop_back] {
        for &p in td { if finite_v(p) { bb.expand(p); } }
    }
    bb
}

/// Serial context. Public construction/build supports the hand-built neckdown integration test.
pub struct Ctx<'a> {
    pub(crate) b: &'a mut OBoard,
    pub(crate) s: &'a OrganicSettings,
    pub(crate) n_l: i32,
    pub(crate) clr: f64, pub(crate) edge_clr: f64, pub(crate) hole_clr: f64,
    pub(crate) ncl: ClearanceTable, pub(crate) cur_net: i32,
    /// ADAPTED: the current trace's rule row and the layer of the last collect.
    pub(crate) cur_rule: u16, pub(crate) cur_layer: i32,
    pub(crate) region: Box2, pub(crate) all: bool,
    pub(crate) segs: &'a mut Vec<Seg>, pub(crate) pads: &'a mut Vec<PadE>, pub(crate) vias: &'a mut Vec<ViaE>,
    pub(crate) edges: &'a mut Vec<(Vec2, Vec2)>, pub(crate) grid: &'a mut UniformGrid,
    pub(crate) stamp: &'a mut Vec<u32>, pub(crate) cur: u32,
    pub(crate) max_h: f64, pub(crate) grid_box: Box2,
    pub(crate) polys: &'a mut Vec<Poly>, pub(crate) pgrid: &'a mut UniformGrid,
    pub(crate) late: &'a mut Vec<i32>, pub(crate) pstamp: &'a mut Vec<u32>,
    pub(crate) pcur: u32, pub(crate) pgrid_built: bool,
    pub(crate) path_box: Option<&'a [Box2]>,
    pub(crate) nominal: Option<&'a [Option<&'a [Vec2]>]>,
    pub(crate) fixed_width: Option<&'a [u8]>,
    pub(crate) branch_ends: Option<&'a BTreeMap<i32, Vec<Vec2>>>,
    pub(crate) junction_conns: Option<&'a BTreeMap<u64, Vec<(i32, i32)>>>,
    pub(crate) sink: Option<&'a mut Vec<Poly>>,
    pub(crate) rbits: &'a mut Vec<u8>, pub(crate) rsum: &'a mut Vec<i32>,
    pub(crate) rnx: i32, pub(crate) rny: i32,
}

/// `Ctx::Cands` (Rust has no nested struct declarations).
#[derive(Default)]
pub(crate) struct Cands { pub ids: Vec<i32>, pub polys: Vec<i32>, pub r#box: Box2 }

impl<'a> Ctx<'a> {
    pub fn new(b: &'a mut OBoard, s: &'a OrganicSettings, x: &'a mut Scratch, nc: Option<&ClearanceTable>) -> Self {
        x.polys.clear(); x.late.clear();
        let n_l = std_max(1, b.layers.len() as i32);
        let slack = std_max(0.0f32, s.guard_clearance_slack) as f64;
        let clr = b.rules.min_clearance as f64 + slack;
        let ncl = nc.unwrap_or(&b.clr).clone();
        let edge_clr = b.rules.board_edge_clearance as f64 + slack;
        let hole_clr = std_max(b.rules.hole_clearance, 0.0f32) as f64 + slack;
        Self {
            b, s, n_l, clr, edge_clr, hole_clr, ncl, cur_net: -1, cur_rule: 0, cur_layer: 0,
            region: Box2::default(), all: true,
            segs: &mut x.segs, pads: &mut x.pads, vias: &mut x.vias, edges: &mut x.edges,
            grid: &mut x.grid, stamp: &mut x.stamp, cur: 0, max_h: 0.125, grid_box: Box2::default(),
            polys: &mut x.polys, pgrid: &mut x.pgrid, late: &mut x.late, pstamp: &mut x.pstamp,
            pcur: 0, pgrid_built: false, path_box: None, nominal: None, fixed_width: None,
            branch_ends: None, junction_conns: None, sink: None,
            rbits: &mut x.rbits, rsum: &mut x.rsum, rnx: 0, rny: 0,
        }
    }

    /// ADAPTED: `other_rule` is an object's ClearanceTable row, not a net id.
    pub(crate) fn cf(&self, other_rule: u16, _tracks: bool) -> f64 {
        self.clr + (self.ncl.get(self.cur_rule, other_rule, self.cur_layer) as f64 - self.b.rules.min_clearance as f64)
    }

    pub(crate) fn cf_pad(&self, p: &Pad) -> f64 {
        // ADAPTED: padRuleClearance (C++:316), as engine/pair.rs:298's pad pair
        // and engine/mod.rs:528's pair_shift: current trace rule, pad rule, layer.
        self.clr + (self.b.clr.get(self.cur_rule, p.rule, self.cur_layer) as f64 - self.b.rules.min_clearance as f64)
    }

    pub(crate) fn set_regions(&mut self, boxes: &[Box2]) {
        self.rnx = std_max(1, (self.region.max.x - self.region.min.x).ceil() as i32 + 1);
        self.rny = std_max(1, (self.region.max.y - self.region.min.y).ceil() as i32 + 1);
        if self.rnx as f64 * self.rny as f64 > 4e6 { self.all = true; return; }
        self.rbits.clear(); self.rbits.resize(self.rnx as usize * self.rny as usize, 0);
        for r in boxes {
            let x0 = std_max(0, (r.min.x - self.region.min.x).floor() as i32);
            let x1 = std_min(self.rnx - 1, (r.max.x - self.region.min.x).floor() as i32);
            let y0 = std_max(0, (r.min.y - self.region.min.y).floor() as i32);
            let y1 = std_min(self.rny - 1, (r.max.y - self.region.min.y).floor() as i32);
            for y in y0..=y1 {
                for x in x0..=x1 { self.rbits[y as usize * self.rnx as usize + x as usize] = 1; }
            }
        }
        self.rsum.clear(); self.rsum.resize((self.rnx + 1) as usize * (self.rny + 1) as usize, 0);
        for y in 0..self.rny {
            let mut row = 0;
            for x in 0..self.rnx {
                row += self.rbits[y as usize * self.rnx as usize + x as usize] as i32;
                self.rsum[(y + 1) as usize * (self.rnx + 1) as usize + x as usize + 1] =
                    self.rsum[y as usize * (self.rnx + 1) as usize + x as usize + 1] + row;
            }
        }
    }

    pub(crate) fn in_region(&self, b: &Box2, m: f32) -> bool {
        if self.all || !b.is_valid() { return true; }
        if b.min.x - m > self.region.max.x || self.region.min.x - m > b.max.x || b.min.y - m > self.region.max.y || self.region.min.y - m > b.max.y { return false; }
        let x0 = std_max(0, (b.min.x - m - self.region.min.x).floor() as i32);
        let x1 = std_min(self.rnx - 1, (b.max.x + m - self.region.min.x).floor() as i32);
        let y0 = std_max(0, (b.min.y - m - self.region.min.y).floor() as i32);
        let y1 = std_min(self.rny - 1, (b.max.y + m - self.region.min.y).floor() as i32);
        if x0 > x1 || y0 > y1 { return false; }
        let w = self.rnx as usize + 1;
        self.rsum[(y1 + 1) as usize * w + x1 as usize + 1] - self.rsum[y0 as usize * w + x1 as usize + 1]
            - self.rsum[(y1 + 1) as usize * w + x0 as usize] + self.rsum[y0 as usize * w + x0 as usize] > 0
    }

    /// Build nominal or exported-width obstacles, preserving reference insertion order.
    pub fn build(&mut self, exported: bool) {
        self.segs.clear(); self.pads.clear(); self.vias.clear(); self.edges.clear();
        self.max_h = 0.0;
        let mut pts = Vec::new();
        let mut w = Vec::new();
        let mut all2 = Box2::default();
        for t in 0..self.b.traces.len() {
            let tr = &self.b.traces[t];
            if !tr.is_routed { continue; }
            if !self.all {
                let bb = self.path_box.and_then(|p| p.get(t)).copied().unwrap_or_else(|| exported_box(tr));
                if !self.in_region(&bb, 0.5f32) { continue; }
            }
            let nominal = self.nominal.and_then(|p| p.get(t)).copied().flatten();
            let q: &[Vec2] = if !exported && nominal.is_some() {
                nominal.unwrap()
            } else if !exported && tr.smoothed.len() >= 2 {
                &tr.smoothed
            } else {
                exported_path(tr, &mut pts, &mut w);
                &pts
            };
            let h_max = if exported {
                let mw = if w.is_empty() { 0.0f32 } else { w[1..].iter().fold(w[0], |a, &b| std_max(a, b)) };
                0.5f32 * std_max(tr.width, mw)
            } else { 0.5f32 * tr.width };
            let mut chunk_end = 0;
            let mut i = 0;
            while i + 1 < q.len() {
                if !self.all && i >= chunk_end {
                    let e = std_min(q.len() - 1, i + 8);
                    let mut cb = Box2::default();
                    let mut fin = true;
                    for k in i..=e { if finite_v(q[k]) { cb.expand(q[k]); } else { fin = false; } }
                    chunk_end = e;
                    if fin && cb.is_valid() && !self.in_region(&cb, h_max + 0.05f32) { i = e; continue; }
                }
                if !finite_v(q[i]) || !finite_v(q[i + 1]) { i += 1; continue; }
                let mut h = if exported { 0.5f32 * std_min(w[i], w[i + 1]) } else { 0.5f32 * tr.width };
                if h <= 0.0f32 { h = 0.5f32 * tr.width; }
                if !self.all {
                    let mut sb = Box2::default(); sb.expand(q[i]); sb.expand(q[i + 1]);
                    if !self.in_region(&sb, h + 0.05f32) { i += 1; continue; }
                }
                self.segs.push(Seg { a: q[i], b: q[i + 1], h, net: tr.net, layer: tr.layer, trace: t as i32 });
                self.max_h = std_max(self.max_h, h as f64);
                all2.expand(q[i]); all2.expand(q[i + 1]);
                i += 1;
            }
        }
        for p in &self.b.pads {
            let e0 = std_max(p.size.x, p.size.y) + 0.5f32;
            if !finite_v(p.center) || !self.in_region(&Box2::new(p.center - Vec2::new(e0, e0), p.center + Vec2::new(e0, e0)), 0.5f32) { continue; }
            let m = pad_model(p);
            let bb = Box2::new(m.center - Vec2::new(m.ext as f32, m.ext as f32), m.center + Vec2::new(m.ext as f32, m.ext as f32));
            if !finite_v(p.center) || !self.in_region(&bb, 0.5f32) { continue; }
            self.pads.push(PadE { m, id: p.id });
            all2.expand_box(&bb);
        }
        for v in &self.b.vias {
            if !finite_v(v.pos) { continue; }
            let bb = Box2::new(v.pos - Vec2::new(v.diameter, v.diameter), v.pos + Vec2::new(v.diameter, v.diameter));
            if !self.in_region(&bb, 0.5f32) { continue; }
            self.vias.push(ViaE { c: v.pos, r: v.diameter * 0.5f32, drill: v.drill, net: v.net, rule: v.rule });
            all2.expand_box(&bb);
        }
        if self.b.outline.len() >= 3 {
            for i in 0..self.b.outline.len() {
                let a = self.b.outline[i]; let b = self.b.outline[(i + 1) % self.b.outline.len()];
                let mut bb = Box2::default(); bb.expand(a); bb.expand(b);
                self.edges.push((a, b)); all2.expand_box(&bb);
            }
        }
        if !all2.is_valid() { all2 = Box2::new(Vec2::new(0.0, 0.0), Vec2::new(1.0, 1.0)); }
        all2.pad(2.0f32);
        if !self.all && self.region.is_valid() {
            let mut r = self.region; r.pad(2.0f32);
            let g = Box2::new(Vec2::new(std_max(all2.min.x, r.min.x), std_max(all2.min.y, r.min.y)), Vec2::new(std_min(all2.max.x, r.max.x), std_min(all2.max.y, r.max.y)));
            if g.is_valid() && g.max.x > g.min.x && g.max.y > g.min.y { all2 = g; }
        }
        self.grid_box = all2;
        self.grid.init(&all2, 1.0f32, UNIFORM_GRID_MAX_CELLS);
        let mut id = 0;
        for s in self.segs.iter() {
            self.grid.add(id, std_min(s.a.x, s.b.x) - s.h, std_min(s.a.y, s.b.y) - s.h, std_max(s.a.x, s.b.x) + s.h, std_max(s.a.y, s.b.y) + s.h); id += 1;
        }
        for p in self.pads.iter() {
            let e = p.m.ext as f32;
            self.grid.add(id, p.m.center.x - e, p.m.center.y - e, p.m.center.x + e, p.m.center.y + e); id += 1;
        }
        for v in self.vias.iter() { self.grid.add(id, v.c.x - v.r, v.c.y - v.r, v.c.x + v.r, v.c.y + v.r); id += 1; }
        let em = std_max(0.0, self.edge_clr - self.clr) as f32 + 0.01f32;
        for &(a, b) in self.edges.iter() {
            let eb = Box2::new(Vec2::new(std_min(a.x, b.x) - em, std_min(a.y, b.y) - em), Vec2::new(std_max(a.x, b.x) + em, std_max(a.y, b.y) + em));
            if self.all || !finite_v(a) || !finite_v(b) || self.in_region(&eb, 0.05f32) { self.grid.add(id, eb.min.x, eb.min.y, eb.max.x, eb.max.y); }
            id += 1;
        }
        self.grid.finalize(); self.stamp.clear(); self.stamp.resize(id as usize, 0); self.cur = 0;
    }

    pub(crate) fn query(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, mut f: impl FnMut(i32)) {
        self.cur = self.cur.wrapping_add(1);
        if self.cur == 0 { self.stamp.fill(0); self.cur = 1; }
        let cur = self.cur;
        let stamp = &mut self.stamp;
        self.grid.query(x0, y0, x1, y1, |id| {
            if stamp[id as usize] == cur { return; }
            stamp[id as usize] = cur;
            f(id);
        });
    }

    pub(crate) fn add_poly(&mut self, p: Poly) {
        if let Some(sink) = self.sink.as_deref_mut() { sink.push(p); return; }
        if self.pgrid_built { self.late.push(self.polys.len() as i32); }
        self.polys.push(p);
    }

    pub(crate) fn index_polys(&mut self) {
        self.pgrid.init(&if self.grid_box.is_valid() { self.grid_box } else { Box2::new(Vec2::new(0.0, 0.0), Vec2::new(1.0, 1.0)) }, 1.0f32, UNIFORM_GRID_MAX_CELLS);
        for (i, p) in self.polys.iter().enumerate() {
            let b = p.bb; self.pgrid.add(i as i32, b.min.x, b.min.y, b.max.x, b.max.y);
        }
        self.pgrid.finalize(); self.pstamp.clear(); self.pstamp.resize(self.polys.len(), 0);
        self.pcur = 0; self.pgrid_built = true;
    }

    pub(crate) fn query_polys(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, mut f: impl FnMut(i32)) {
        let overlaps = |b: &Box2| !(b.max.x < x0 || b.min.x > x1 || b.max.y < y0 || b.min.y > y1);
        if !self.pgrid_built {
            for (i, p) in self.polys.iter().enumerate() { if overlaps(&p.bb) { f(i as i32); } }
            return;
        }
        self.pcur = self.pcur.wrapping_add(1);
        if self.pcur == 0 { self.pstamp.fill(0); self.pcur = 1; }
        let pcur = self.pcur;
        let pstamp = &mut self.pstamp;
        let polys = &self.polys;
        self.pgrid.query(x0, y0, x1, y1, |id| {
            if pstamp[id as usize] == pcur { return; }
            pstamp[id as usize] = pcur;
            if overlaps(&polys[id as usize].bb) { f(id); }
        });
        for &i in self.late.iter() { if overlaps(&self.polys[i as usize].bb) { f(i); } }
    }

    pub(crate) fn collect(&mut self, mut r#box: Box2, reach: f32, layer: i32, net: i32, c: &mut Cands) {
        c.ids.clear(); c.polys.clear(); self.cur_net = net; self.cur_layer = layer;
        // ADAPTED: padRuleReach (C++:637) uses b.clr.max, as engine/mod.rs:453.
        let extra = self.b.clr.max;
        r#box.pad(reach + std_max(0.0f32, extra - self.b.rules.min_clearance));
        c.r#box = r#box;
        let n_s = self.segs.len() as i32; let n_p = self.pads.len() as i32; let n_v = self.vias.len() as i32;
        let out = |x0: f32, y0: f32, x1: f32, y1: f32| x1 < r#box.min.x || x0 > r#box.max.x || y1 < r#box.min.y || y0 > r#box.max.y;
        // Collect grid ids first to avoid borrowing self through its callback. Order is unchanged.
        let mut ids = Vec::new();
        self.query(r#box.min.x, r#box.min.y, r#box.max.x, r#box.max.y, |id| ids.push(id));
        for id in ids {
            if id < n_s {
                let sg = &self.segs[id as usize];
                if sg.layer != layer || sg.net == net { continue; }
                if out(std_min(sg.a.x, sg.b.x) - sg.h, std_min(sg.a.y, sg.b.y) - sg.h, std_max(sg.a.x, sg.b.x) + sg.h, std_max(sg.a.y, sg.b.y) + sg.h) { continue; }
            } else if id < n_s + n_p {
                let pe = &self.pads[(id - n_s) as usize]; let pad = &self.b.pads[pe.id as usize];
                if !pad_on_layer(pad, layer, self.n_l) || (pad.net >= 0 && pad.net == net) { continue; }
                let e = pe.m.ext as f32 + std_max(0.0, self.hole_clr - self.clr) as f32;
                if out(pe.m.center.x - e, pe.m.center.y - e, pe.m.center.x + e, pe.m.center.y + e) { continue; }
            } else if id < n_s + n_p + n_v {
                let v = &self.vias[(id - n_s - n_p) as usize];
                if v.net == net { continue; }
                let e = v.r + std_max(0.0, self.hole_clr - self.clr) as f32;
                if out(v.c.x - e, v.c.y - e, v.c.x + e, v.c.y + e) { continue; }
            } else {
                let ed = self.edges[(id - n_s - n_p - n_v) as usize];
                let m = std_max(0.0, self.edge_clr - self.clr) as f32;
                if out(std_min(ed.0.x, ed.1.x) - m, std_min(ed.0.y, ed.1.y) - m, std_max(ed.0.x, ed.1.x) + m, std_max(ed.0.y, ed.1.y) + m) { continue; }
            }
            c.ids.push(id);
        }
        let mut polys = Vec::new();
        self.query_polys(r#box.min.x, r#box.min.y, r#box.max.x, r#box.max.y, |i| polys.push(i));
        for i in polys {
            let p = &self.polys[i as usize];
            if p.layer != layer || p.net == net { continue; }
            if p.bb.max.x < r#box.min.x || p.bb.min.x > r#box.max.x || p.bb.max.y < r#box.min.y || p.bb.min.y > r#box.max.y { continue; }
            c.polys.push(i);
        }
    }

    pub(crate) fn box_gap(a: Vec2, b: Vec2, x0: f32, y0: f32, x1: f32, y1: f32) -> f64 {
        let ax0 = std_min(a.x, b.x); let ax1 = std_max(a.x, b.x);
        let ay0 = std_min(a.y, b.y); let ay1 = std_max(a.y, b.y);
        let dx = std_max(std_max(0.0f32, x0 - ax1), ax0 - x1);
        let dy = std_max(std_max(0.0f32, y0 - ay1), ay0 - y1);
        (dx as f64 * dx as f64 + dy as f64 * dy as f64).sqrt()
    }

    pub(crate) fn avail(&self, c: &Cands, a: Vec2, b: Vec2, with_polys: bool, limit: f64) -> f64 {
        let mut best = limit;
        let n_s = self.segs.len() as i32; let n_p = self.pads.len() as i32; let n_v = self.vias.len() as i32;
        for &id in &c.ids {
            if id < n_s {
                let sg = &self.segs[id as usize];
                let c = self.cf(self.b.traces[sg.trace as usize].rule, true);
                if Self::box_gap(a, b, std_min(sg.a.x, sg.b.x), std_min(sg.a.y, sg.b.y), std_max(sg.a.x, sg.b.x), std_max(sg.a.y, sg.b.y)) - sg.h as f64 - c >= best { continue; }
                best = std_min(best, seg_seg(a, b, sg.a, sg.b).d - sg.h as f64 - c);
            } else if id < n_s + n_p {
                let pe = &self.pads[(id - n_s) as usize]; let e = pe.m.ext as f32;
                let c = self.cf_pad(&self.b.pads[pe.id as usize]);
                if Self::box_gap(a, b, pe.m.center.x - e, pe.m.center.y - e, pe.m.center.x + e, pe.m.center.y + e) - c >= best { continue; }
                best = std_min(best, seg_pad(a, b, &pe.m).d - pe.m.r - c);
                if pe.m.hole { best = std_min(best, seg_hole(a, b, &pe.m).d - pe.m.hr - self.hole_clr); }
            } else if id < n_s + n_p + n_v {
                let v = &self.vias[(id - n_s - n_p) as usize]; let c = self.cf(v.rule, false);
                if Self::box_gap(a, b, v.c.x - v.r, v.c.y - v.r, v.c.x + v.r, v.c.y + v.r) - std_max(c, 0.5 * v.drill as f64 + self.hole_clr - v.r as f64) >= best { continue; }
                let d = seg_seg(a, b, v.c, v.c).d;
                best = std_min(best, d - v.r as f64 - c);
                best = std_min(best, d - 0.5 * v.drill as f64 - self.hole_clr);
            } else {
                let e = self.edges[(id - n_s - n_p - n_v) as usize];
                best = std_min(best, seg_seg(a, b, e.0, e.1).d - self.edge_clr);
            }
        }
        if with_polys {
            for &i in &c.polys {
                let p = &self.polys[i as usize]; let c = self.cf(self.b.traces[p.trace as usize].rule, false);
                if Self::box_gap(a, b, p.bb.min.x, p.bb.min.y, p.bb.max.x, p.bb.max.y) - c >= best { continue; }
                best = std_min(best, poly_seg_dist(&p.pts, a, b) - c);
            }
        }
        best
    }

    pub(crate) fn poly_clear(&mut self, poly: &[Vec2], layer: i32, net: i32, c: &mut Cands) -> bool {
        let mut bb = Box2::default(); for &p in poly { bb.expand(p); }
        self.collect(bb, (self.clr + 0.02) as f32, layer, net, c);
        for i in 0..poly.len() { if self.avail(c, poly[i], poly[(i + 1) % poly.len()], true, 1e-6) < -1e-9 { return false; } }
        if self.b.outline.len() >= 3 {
            for &p in poly { if !point_in_poly(&self.b.outline, p) { return false; } }
        }
        for k in &self.b.keepouts {
            // ADAPTED: same stack-mask and margin policy as engine/build.rs.
            if !k.no_tracks || k.poly.len() < 3 || layer < 0 || layer >= 64 || k.layer_mask & (1u64 << layer) == 0 { continue; }
            let m = std_max(k.clearance, 0.0f32) + (self.clr - self.b.rules.min_clearance as f64) as f32;
            let mut kb = Box2::default(); for &q in &k.poly { kb.expand(q); }
            if kb.min.x - m > bb.max.x || bb.min.x - m > kb.max.x || kb.min.y - m > bb.max.y || bb.min.y - m > kb.max.y { continue; }
            for i in 0..poly.len() {
                if drcgeom::segment_keepout_distance(k, poly[i], poly[(i + 1) % poly.len()]) < m { return false; }
            }
            if point_in_poly(poly, k.poly[0]) { return false; }
        }
        let n_s = self.segs.len() as i32; let n_p = self.pads.len() as i32; let n_v = self.vias.len() as i32;
        for &id in &c.ids {
            if id < n_s { if point_in_poly(poly, self.segs[id as usize].a) { return false; } }
            else if id < n_s + n_p { if point_in_poly(poly, self.b.pads[self.pads[(id - n_s) as usize].id as usize].center) { return false; } }
            else if id < n_s + n_p + n_v && point_in_poly(poly, self.vias[(id - n_s - n_p) as usize].c) { return false; }
        }
        for &i in &c.polys { if point_in_poly(poly, self.polys[i as usize].pts[0]) { return false; } }
        true
    }
}

/// Arc-length parametrised polyline; callers provide at least two points and matching widths.
pub(crate) struct PathView<'a> { pub p: &'a [Vec2], pub w: &'a [f32], pub cum: Vec<f64> }
impl<'a> PathView<'a> {
    pub fn new(p: &'a [Vec2], w: &'a [f32]) -> Self {
        let mut cum = vec![0.0; p.len()];
        for i in 1..p.len() { cum[i] = cum[i - 1] + p[i - 1].distance_to(p[i]) as f64; }
        Self { p, w, cum }
    }
    pub fn length(&self) -> f64 { self.cum.last().copied().unwrap_or(0.0) }
    pub fn seg_at(&self, s: f64) -> usize {
        let i = self.cum.partition_point(|&v| !(s < v));
        if i == 0 { 0 } else { std_min(i - 1, self.p.len() - 2) }
    }
    pub fn at(&self, s: f64) -> Vec2 {
        let i = self.seg_at(s);
        let l = self.cum[i + 1] - self.cum[i];
        let u = if l > 1e-12 { std_clamp((s - self.cum[i]) / l, 0.0, 1.0) as f32 } else { 0.0f32 };
        Vec2::lerp(self.p[i], self.p[i + 1], u)
    }
    pub fn width_at(&self, s: f64) -> f32 { let i = self.seg_at(s); std_min(self.w[i], self.w[i + 1]) }
    pub fn tangent(&self, s: f64) -> Vec2 {
        let a = std_max(0.0, s - 0.05); let b = std_min(self.length(), s + 0.05);
        let mut d = (self.at(b) - self.at(a)).normalized();
        if d.length_squared() < 0.5f32 { let i = self.seg_at(s); d = (self.p[i + 1] - self.p[i]).normalized(); }
        d
    }
}

pub(crate) fn extent(p: Vec2, dir: Vec2, cap: f64, inside: impl Fn(Vec2) -> bool) -> f64 {
    if cap <= 0.0 || !inside(p) { return 0.0; }
    if inside(p + dir * cap as f32) { return cap; }
    let mut lo = 0.0; let mut hi = cap;
    for _it in 0..20 {
        let m = 0.5 * (lo + hi);
        if inside(p + dir * m as f32) { lo = m; } else { hi = m; }
    }
    lo
}

pub(crate) fn smoothstep01(x: f64) -> f64 {
    if x <= 0.0 { return 0.0; }
    if x >= 1.0 { return 1.0; }
    x * x * (3.0 - 2.0 * x)
}

pub(crate) fn pad_cross(pad: &Pad, e: Vec2, d: Vec2) -> f64 {
    let f = padgeom::frame(pad);
    let inside = |q: Vec2| padgeom::point_distance(&f, q.x as f64, q.y as f64) <= 1e-9;
    let n = d.perpendicular();
    let cap = std_max(pad.size.x, pad.size.y) as f64 + 1.0;
    extent(e, n, cap, inside) + extent(e, n * -1.0f32, cap, inside)
}

#[derive(Default)]
pub(crate) struct Ring { pub p: Vec<Vec2>, pub cum: Vec<f64>, pub len: f64 }
impl Ring {
    pub fn finish(&mut self) {
        self.cum.clear(); self.cum.resize(self.p.len() + 1, 0.0);
        for i in 0..self.p.len() { self.cum[i + 1] = self.cum[i] + self.p[i].distance_to(self.p[(i + 1) % self.p.len()]) as f64; }
        self.len = *self.cum.last().unwrap();
    }
    pub fn at(&self, mut a: f64) -> Vec2 {
        a %= self.len;
        if a < 0.0 { a += self.len; }
        let i = self.cum.partition_point(|&v| !(a < v));
        let i = if i == 0 { 0 } else { std_min(i - 1, self.p.len() - 1) };
        let l = self.cum[i + 1] - self.cum[i];
        Vec2::lerp(self.p[i], self.p[(i + 1) % self.p.len()], if l > 1e-12 { ((a - self.cum[i]) / l) as f32 } else { 0.0f32 })
    }
    pub fn nearest(&self, q: Vec2) -> f64 {
        let mut best = K_BIG; let mut at0 = 0.0;
        for i in 0..self.p.len() {
            let a = self.p[i]; let b = self.p[(i + 1) % self.p.len()];
            let dx = b.x as f64 - a.x as f64; let dy = b.y as f64 - a.y as f64; let l2 = dx * dx + dy * dy;
            let t = if l2 > 1e-18 { std_clamp(((q.x as f64 - a.x as f64) * dx + (q.y as f64 - a.y as f64) * dy) / l2, 0.0, 1.0) } else { 0.0 };
            let ex = a.x as f64 + t * dx - q.x as f64; let ey = a.y as f64 + t * dy - q.y as f64; let d = ex * ex + ey * ey;
            if d < best { best = d; at0 = self.cum[i] + t * (self.cum[i + 1] - self.cum[i]); }
        }
        at0
    }
}

pub(crate) fn pad_ring(f: &padgeom::Frame<'_>, r_out: &mut Ring) {
    r_out.p.clear();
    if let Some(poly) = f.poly {
        for q in poly { r_out.p.push(padgeom::to_board(f, q.x as f64 - f.ox, q.y as f64 - f.oy)); }
    } else {
        let r = std_min(std_min(f.r, f.hx), f.hy); let ix = f.hx - r; let iy = f.hy - r;
        let cx = [ix, -ix, -ix, ix]; let cy = [iy, iy, -iy, -iy];
        let arc = if r > 1e-6 { 12 } else { 0 };
        for c in 0..4 {
            for k in 0..=arc {
                let a = (c as f64 + if arc != 0 { k as f64 / arc as f64 } else { 0.0 }) * 1.57079632679489662;
                let ca = if arc != 0 { a.cos() } else if c == 0 || c == 3 { 1.0 } else { -1.0 };
                let sa = if arc != 0 { a.sin() } else if c < 2 { 1.0 } else { -1.0 };
                let rx = if arc != 0 { r } else { f.hx - ix }; let ry = if arc != 0 { r } else { f.hy - iy };
                r_out.p.push(padgeom::to_board(f, cx[c] + rx * ca, cy[c] + ry * sa));
            }
        }
    }
    r_out.finish();
}

pub(crate) fn circle_ring(c: Vec2, r: f64, r_out: &mut Ring) {
    r_out.p.clear();
    let n = 64;
    for i in 0..n {
        let a = 6.28318530717958648 * i as f64 / n as f64;
        r_out.p.push(Vec2::new((c.x as f64 + r * a.cos()) as f32, (c.y as f64 + r * a.sin()) as f32));
    }
    r_out.finish();
}

#[derive(Default)]
pub(crate) struct TrackFrame {
    pub s: Vec<f64>, pub p: Vec<Vec2>, pub t: Vec<Vec2>, pub raw: Vec<Vec2>, pub off: Vec<f64>, pub max_off: f64,
}
impl TrackFrame {
    pub fn build(&mut self, pv: &PathView<'_>, s0: f64, s1: f64, window: f64) {
        let n = std_clamp(((s1 - s0) / 0.025).ceil() as i32, 8, 240);
        let ds = (s1 - s0) / n as f64;
        self.s.resize(n as usize + 1, 0.0); self.raw.resize(n as usize + 1, Vec2::default());
        for i in 0..=n as usize { self.s[i] = s0 + ds * i as f64; self.raw[i] = pv.at(self.s[i]); }
        self.p.clone_from(&self.raw);
        for _pass in 0..2 {
            let input = self.p.clone();
            for i in 1..n {
                let k = std_min(std_min(window / ds, 0.5 * (n - i) as f64), i as f64) as i32;
                if k <= 0 { continue; }
                let mut sum = Vec2::default();
                for j in i - k..=i + k { sum += input[j as usize]; }
                self.p[i as usize] = sum / (2 * k + 1) as f32;
            }
        }
        self.t.resize(self.p.len(), Vec2::default());
        for i in 0..=n {
            let a = std_max(0, i - 1); let b = std_min(n, i + 1);
            let mut d = (self.p[b as usize] - self.p[a as usize]).normalized();
            if d.length_squared() < 0.5f32 { d = if i > 0 { self.t[i as usize - 1] } else { pv.tangent(self.s[i as usize]) }; }
            self.t[i as usize] = d;
        }
        self.off.resize(self.p.len(), 0.0); self.max_off = 0.0;
        for i in 0..=n as usize {
            let d = self.raw[i] - self.p[i]; let nn = self.t[i].perpendicular();
            self.off[i] = d.x as f64 * nn.x as f64 + d.y as f64 * nn.y as f64;
            self.max_off = std_max(self.max_off, self.off[i].abs());
        }
    }
    pub fn station(&self, sv: f64) -> (usize, f32) {
        let ds = self.s[1] - self.s[0];
        let x = std_clamp((sv - self.s[0]) / ds, 0.0, self.s.len() as f64 - 1.0001);
        let i = x as usize;
        (i, (x - i as f64) as f32)
    }
    pub fn tangent(&self, sv: f64) -> Vec2 {
        let (i, u) = self.station(sv);
        let d = Vec2::lerp(self.t[i], self.t[i + 1], u).normalized();
        if d.length_squared() > 0.5f32 { d } else { self.t[i] }
    }
    pub fn to_world(&self, sv: f64, nv: f64) -> Vec2 {
        let (i, u) = self.station(sv);
        let over = if sv < self.s[0] { sv - self.s[0] } else if sv > *self.s.last().unwrap() { sv - *self.s.last().unwrap() } else { 0.0 };
        let t = self.tangent(sv);
        Vec2::lerp(self.p[i], self.p[i + 1], u) + t * over as f32 + t.perpendicular() * nv as f32
    }
    /// C++ out parameters (including optional tOut) returned together.
    pub fn to_local(&self, q: Vec2) -> (f64, f64, Vec2) {
        let mut b = 0; let mut bd = 1e30f32;
        for i in 0..self.p.len() {
            let d = self.p[i].distance_squared_to(q);
            if d < bd { bd = d; b = i; }
        }
        let d = q - self.p[b]; let t = self.t[b]; let n = t.perpendicular();
        let sv = self.s[b] + d.x as f64 * t.x as f64 + d.y as f64 * t.y as f64;
        let nv = d.x as f64 * n.x as f64 + d.y as f64 * n.y as f64;
        (sv, nv, t)
    }
}

#[derive(Default)]
pub(crate) struct FilletSide { pub curve: Vec<Vec2>, pub s_tip: f64, pub flare: f64 }

pub(crate) struct FilletJob<'a, 'p> {
    pub pv: &'a PathView<'p>, pub fr: &'a TrackFrame, pub ring: &'a Ring, pub inside: &'a dyn Fn(Vec2) -> bool,
    pub s_x: f64, pub s_root: f64, pub l: f64, pub wt: f64, pub curvature: f64,
}
impl FilletJob<'_, '_> {
    pub fn half(&self, s: f64) -> f64 { 0.5 * self.pv.width_at(std_clamp(s, 0.0, self.pv.length())) as f64 }
    pub fn poke(&self, side: i32, cs: &[f64], cn: &[f64], mut r#where: Option<&mut Vec<(f64, f64)>>) -> f64 {
        let fr = self.fr;
        if fr.max_off < 2e-3 { return 0.0; }
        let mut cover = vec![-K_BIG; fr.s.len()];
        let ds = fr.s[1] - fr.s[0];
        for i in 0..cs.len().saturating_sub(1) {
            let a = std_min(cs[i], cs[i + 1]); let b = std_max(cs[i], cs[i + 1]);
            let k0 = std_max(0, ((a - fr.s[0]) / ds).ceil() as i32);
            let k1 = std_min(fr.s.len() as i32 - 1, ((b - fr.s[0]) / ds).floor() as i32);
            for k in k0..=k1 {
                let u = if b - a > 1e-9 { (fr.s[k as usize] - cs[i]) / (cs[i + 1] - cs[i]) } else { 0.0 };
                cover[k as usize] = std_max(cover[k as usize], side as f64 * (cn[i] + (cn[i + 1] - cn[i]) * u));
            }
        }
        let mut worst = 0.0;
        for k in 0..fr.s.len() {
            if cover[k] <= -K_BIG { continue; }
            let h = self.half(fr.s[k]); let want = side as f64 * fr.off[k] + h;
            if want - cover[k] <= if r#where.is_some() { 0.0 } else { worst } { continue; }
            if (self.inside)(fr.raw[k] + fr.t[k].perpendicular() * (side as f64 * h) as f32) { continue; }
            worst = std_max(worst, want - cover[k]);
            if let Some(points) = r#where.as_deref_mut() { points.push((fr.s[k], want - cover[k])); }
        }
        worst
    }

    pub fn build(&self, side: i32, phi: f64, scale: f64, out: &mut FilletSide) -> bool {
        let fr = self.fr; let pv = self.pv; let ring = self.ring; let inside = self.inside;
        let s_t = self.s_x + self.l * scale;
        let tip = fr.to_world(s_t, side as f64 * self.half(s_t));
        let mut s_e = -1.0;
        let edge = |s: f64| fr.to_world(s, side as f64 * self.half(s));
        if !inside(edge(self.s_root)) { s_e = self.s_root; }
        else {
            let mut s = self.s_root + 0.02;
            while s <= s_t {
                if !inside(edge(s)) {
                    let mut lo = s - 0.02; let mut hi = s;
                    for _it in 0..16 {
                        let m = 0.5 * (lo + hi);
                        if inside(edge(m)) { lo = m; } else { hi = m; }
                    }
                    s_e = hi; break;
                }
                s += 0.02;
            }
        }
        if s_e < 0.0 { return false; }
        let e = edge(s_e); let a0 = ring.nearest(e);
        let n_e = fr.tangent(s_e).perpendicular() * side as f32; let axis_e = fr.to_world(s_e, 0.0);
        let lateral = |q: Vec2| (q.x - axis_e.x) as f64 * n_e.x as f64 + (q.y - axis_e.y) as f64 * n_e.y as f64;
        let dir = if lateral(ring.at(a0 + 0.06)) >= lateral(ring.at(a0 - 0.06)) { 1 } else { -1 };
        let cross = |a: Vec2, b: Vec2| a.x as f64 * b.y as f64 - a.y as f64 * b.x as f64;
        let cap = std_min(0.5 * ring.len, 2.5 * self.l + self.wt);
        let walk = |da: f64| {
            let mut amax = 0.0; let mut c0: f64 = 0.0; let mut back = 0.0;
            let t_e = fr.tangent(s_e);
            let a_x = ring.nearest(pv.at(self.s_x)); let d_c = std_max(1.5 * self.half(s_e), 0.25);
            let t_before = (ring.at(a_x - d_c + 0.03) - ring.at(a_x - d_c)).normalized();
            let t_after = (ring.at(a_x + d_c) - ring.at(a_x + d_c - 0.03)).normalized();
            let dot_limit = if t_before.x as f64 * t_after.x as f64 + t_before.y as f64 * (t_after.y as f64) < 0.8 { -0.3 } else { -0.4 };
            let mut a = 0.0;
            while a + da <= cap {
                let p = ring.at(a0 + dir as f64 * a); let q = ring.at(a0 + dir as f64 * (a + da));
                let d = (q - p).normalized(); let c = cross(d, tip - p);
                if c0.abs() < 1e-9 { c0 = c; }
                if c * c0 < 0.0 { break; }
                if lateral(q) > 0.5 * self.wt { break; }
                if d.x as f64 * t_e.x as f64 + d.y as f64 * (t_e.y as f64) < dot_limit {
                    let u = (p - ring.at(a0 + dir as f64 * (a - 0.12))).normalized();
                    let v = (ring.at(a0 + dir as f64 * (a + 0.12)) - p).normalized();
                    if cross(u, v).abs() < 0.03 {
                        back += da;
                        if back > 1.5 * self.half(s_e) { break; }
                    }
                }
                amax = a + da; a += da;
            }
            amax
        };
        let mut amax = walk(0.02);
        if amax < 0.04 { amax = walk(0.004); }
        if amax < 0.008 { return false; }
        let a_a = phi * amax; let a = ring.at(a0 + dir as f64 * a_a);
        let t_a = (ring.at(a0 + dir as f64 * std_max(0.0, a_a - 0.03)) - a).normalized();
        if t_a.length_squared() < 0.5f32 { return false; }
        let (s_a, n_a, t_f) = fr.to_local(a);
        let n_t = side as f64 * self.half(s_t);
        if n_a * (side as f64) < n_t.abs() + 0.004 { return false; }
        let ls = t_a.x as f64 * t_f.x as f64 + t_a.y as f64 * t_f.y as f64;
        let n_f = t_f.perpendicular();
        let ln = t_a.x as f64 * n_f.x as f64 + t_a.y as f64 * n_f.y as f64;
        let chord = (s_t - s_a).hypot(n_t - n_a);
        let kappa = 0.45 + 0.4 * std_clamp(self.curvature, 0.0, 1.0);
        let m = 20;
        let mut cs = vec![0.0; m + 1]; let mut cn = vec![0.0; m + 1];
        let mut bs = Vec::new(); let mut bn = Vec::new(); let mut least = K_BIG;
        for tension in [1.0, 0.7, 0.45, 0.25, 0.12] {
            let mut h_a = chord / 3.0 * tension; let mut h_t = chord / 3.0 * tension;
            if ln.abs() > 1e-4 {
                let t = (n_t - n_a) / ln; let xs = s_a + t * ls;
                if t > 0.0 && xs < s_t { h_a = kappa * tension * t; h_t = kappa * tension * (s_t - xs); }
            }
            let p1s = s_a + h_a * ls; let p1n = n_a + h_a * ln; let p2s = s_t - h_t; let p2n = n_t;
            let mut dips = false;
            for i in 0..=m {
                if dips { break; }
                let u = i as f64 / m as f64; let v = 1.0 - u;
                let b0 = v * v * v; let b1 = 3.0 * v * v * u; let b2 = 3.0 * v * u * u; let b3 = u * u * u;
                cs[i] = b0 * s_a + b1 * p1s + b2 * p2s + b3 * s_t;
                cn[i] = b0 * n_a + b1 * p1n + b2 * p2n + b3 * n_t;
                dips = cn[i] * (side as f64) < self.half(cs[i]) - 2e-3 && i < m;
            }
            if dips { continue; }
            let pk = self.poke(side, &cs, &cn, None);
            if pk < least - 1e-4 { least = pk; bs.clone_from(&cs); bn.clone_from(&cn); }
            if pk <= 5e-3 { break; }
        }
        if bs.is_empty() { return false; }
        if least > 5e-3 {
            let mut pts = vec![(s_a, 0.0)]; let mut hull: Vec<(f64, f64)> = Vec::new();
            self.poke(side, &bs, &bn, Some(&mut pts)); pts.push((s_t, 0.0));
            for q in pts {
                while hull.len() >= 2 {
                    let a = hull[hull.len() - 2]; let b = *hull.last().unwrap();
                    if (b.0 - a.0) * (q.1 - a.1) - (b.1 - a.1) * (q.0 - a.0) < 0.0 { break; }
                    hull.pop();
                }
                hull.push(q);
            }
            for i in 1..m {
                let sv = bs[i];
                for k in 0..hull.len().saturating_sub(1) {
                    if sv >= hull[k].0 && sv <= hull[k + 1].0 {
                        let u = (sv - hull[k].0) / std_max(1e-9, hull[k + 1].0 - hull[k].0);
                        bn[i] += side as f64 * (hull[k].1 + (hull[k + 1].1 - hull[k].1) * u); break;
                    }
                }
            }
        }
        out.curve.clear(); out.flare = 0.0; out.s_tip = s_t;
        for i in 0..=m {
            out.flare = std_max(out.flare, bn[i] * side as f64);
            out.curve.push(if i == 0 { a } else if i == m { tip } else { fr.to_world(bs[i], bn[i]) });
        }
        if fr.max_off > 0.03 {
            for _pass in 0..4 {
                let c = out.curve.clone();
                for i in 1..m { out.curve[i] = c[i] * 0.5f32 + (c[i - 1] + c[i + 1]) * 0.25f32; }
            }
        }
        true
    }

    pub fn flat(&self, s_to: f64, out: &mut FilletSide) {
        let m = 20;
        out.curve.clear(); out.s_tip = s_to; out.flare = 0.0;
        for i in 0..=m { out.curve.push(self.pv.at(self.s_root + (s_to - self.s_root) * i as f64 / m as f64)); }
    }
    pub fn tail(&self, f: &FilletSide, s: f64, pts: &mut Vec<Vec2>) {
        pts.clone_from(&f.curve);
        let n = ((s - f.s_tip) / 0.05).ceil() as i32;
        if n > 0 { for i in 0..=n { pts.push(self.pv.at(f.s_tip + (s - f.s_tip) * i as f64 / n as f64)); } }
    }
    pub fn assemble(&self, l: &FilletSide, r: &FilletSide, poly: &mut Vec<Vec2>) {
        let s = std_max(l.s_tip, r.s_tip); let mut a = Vec::new(); let mut b = Vec::new();
        self.tail(r, s, &mut a); self.tail(l, s, &mut b);
        poly.clear();
        for q in a { if poly.is_empty() || poly.last().unwrap().distance_squared_to(q) > 1e-10f32 { poly.push(q); } }
        for i in (0..b.len()).rev() { if poly.last().unwrap().distance_squared_to(b[i]) > 1e-10f32 { poly.push(b[i]); } }
        let root = self.pv.at(self.s_root);
        if poly.last().unwrap().distance_squared_to(root) > 1e-10f32 && poly[0].distance_squared_to(root) > 1e-10f32 { poly.push(root); }
        if poly.len() >= 2 && poly[0].distance_squared_to(*poly.last().unwrap()) < 1e-10f32 { poly.pop(); }
    }
    pub fn half_poly(&self, _side: i32, f: &FilletSide, poly: &mut Vec<Vec2>) {
        poly.clone_from(&f.curve);
        let n = 16;
        for i in (0..=n).rev() { poly.push(self.pv.at(self.s_root + (f.s_tip - self.s_root) * i as f64 / n as f64)); }
    }
}

/// Neckdown of one trace. Appends issues; optionally returns the path before resampling.
/// `Ctx::build(false)` must have indexed nominal obstacle widths first.
pub fn neck_trace(c: &mut Ctx<'_>, t: i32, issues: &mut Vec<NeckdownIssue>, chords_clear: bool, nominal_out: Option<&mut Vec<Vec2>>) {
    let settings = c.s;
    let tr = &c.b.traces[t as usize];
    let w = tr.width;
    let min_w = std_max(settings.min_neckdown_width, c.b.min_track_width_for.get(&(tr.net, tr.layer)).copied().unwrap_or(c.b.rules.min_trace_width));
    if !settings.enable_dynamic_neckdown || w <= min_w + 1e-6f32 || tr.smoothed.len() < 2 { return; }
    if let Some(fixed_width) = c.fixed_width {
        if tr.net >= 0 && tr.net < fixed_width.len() as i32 && fixed_width[tr.net as usize] != 0 { return; }
    }
    let p = tr.smoothed.clone();
    let _reach = 0.5 * w as f64 + std_max(c.clr + 0.25, c.edge_clr) + 0.05; // Also unused in C++:1289.
    let taper = std_max(0.05f32, settings.neckdown_taper_length);
    let trace_id = tr.id; let net = tr.net; let layer = tr.layer;
    c.cur_rule = tr.rule;
    // Copy the two endpoints and their pad data so mutable query stamps do not alias the board.
    // Geometry calculations below retain the reference's operation order and Frame borrows.
    let ends = [tr.points.first().unwrap().clone(), tr.points.last().unwrap().clone()];
    let end_pad_storage: [Option<Pad>; 2] = std::array::from_fn(|side| {
        let id = ends[side].terminal_pad;
        if id < 0 || id >= c.b.pads.len() as i32 { None } else { Some(c.b.pads[id as usize].clone()) }
    });
    struct EndPad<'a> { f: padgeom::Frame<'a>, cross: f64 }
    let mut end_pads = Vec::new();
    for side in 0..2 {
        let Some(pad) = end_pad_storage[side].as_ref() else { continue; };
        if !pad_on_layer(pad, layer, c.n_l) { continue; }
        let e = if side == 0 { p[0] } else { *p.last().unwrap() };
        let mut nx = if side == 0 { p[1] } else { p[p.len() - 2] };
        for k in 1..p.len() {
            let q = if side == 0 { p[k] } else { p[p.len() - 1 - k] };
            nx = q;
            if q.distance_to(e) > 0.3f32 { break; }
        }
        let d = (nx - e).normalized();
        if d.length_squared() < 0.5f32 { continue; }
        let cross = pad_cross(pad, e, d);
        if cross > 1e-4 && cross < w as f64 - 1e-5 { end_pads.push(EndPad { f: padgeom::frame(pad), cross }); }
    }
    let mut cands = Cands::default();
    let mut cand_box = Box2::default();
    if chords_clear && end_pads.is_empty() {
        if !path_near_edges(&p, c.edges.len(), |e| c.edges[e], 0.5 * w as f64, c.edge_clr) { return; }
    }
    let mut allowed = |a: Vec2, b: Vec2| {
        // `ensure`: candidates per approximately 1 mm path chunk.
        if !(cand_box.is_valid() && a.x >= cand_box.min.x && a.x <= cand_box.max.x && a.y >= cand_box.min.y && a.y <= cand_box.max.y
            && b.x >= cand_box.min.x && b.x <= cand_box.max.x && b.y >= cand_box.min.y && b.y <= cand_box.max.y) {
            cand_box = Box2::default(); cand_box.expand(a); cand_box.expand(b); cand_box.pad(0.4f32);
            c.collect(cand_box, (0.5 * w as f64 + c.clr + 0.02) as f32, layer, net, &mut cands);
        }
        let av = c.avail(&cands, a, b, false, 0.5 * w as f64 + 1e-6);
        let mut width = std_min(w as f64, 2.0 * av);
        let mid = (a + b) * 0.5f32;
        for ep in &end_pads {
            if padgeom::point_distance(&ep.f, mid.x as f64, mid.y as f64) <= 0.0 { width = std_min(width, std_max(ep.cross, min_w as f64)); }
        }
        width
    };
    let n0 = p.len();
    let mut a0 = vec![0.0; n0 - 1]; let mut any = false;
    for i in 0..n0 - 1 {
        a0[i] = allowed(p[i], p[i + 1]);
        if a0[i] < w as f64 - 1e-5 { any = true; }
    }
    if !any { return; }
    let mut cum = vec![0.0; n0];
    for i in 1..n0 { cum[i] = cum[i - 1] + p[i - 1].distance_to(p[i]) as f64; }
    let mut region = vec![0u8; n0 - 1];
    {
        let mut last = -K_BIG;
        for i in 0..n0 - 1 {
            if a0[i] < w as f64 - 1e-5 { last = cum[i + 1]; }
            if cum[i] - last <= taper as f64 + 0.1 { region[i] = 1; }
        }
        last = K_BIG;
        for i in (0..n0 - 1).rev() {
            if a0[i] < w as f64 - 1e-5 { last = cum[i]; }
            if last - cum[i + 1] <= taper as f64 + 0.1 { region[i] = 1; }
        }
    }
    let mut q = vec![p[0]]; let mut a = Vec::new();
    for i in 0..n0 - 1 {
        let len = cum[i + 1] - cum[i];
        let k = if region[i] != 0 { std_max(1, (len / 0.05 - 1e-6).ceil() as i32) } else { 1 };
        for j in 1..=k {
            let qi = if j == k { p[i + 1] } else { Vec2::lerp(p[i], p[i + 1], j as f32 / k as f32) };
            let pr = *q.last().unwrap();
            a.push(if k == 1 { a0[i] } else { allowed(pr, qi) });
            q.push(qi);
        }
    }
    let n = q.len(); let mut s = vec![0.0; n - 1];
    {
        let mut acc = 0.0;
        for i in 0..n - 1 {
            let len = q[i].distance_to(q[i + 1]) as f64;
            s[i] = acc + 0.5 * len; acc += len;
        }
    }
    let mut i = 0;
    while i + 1 < n {
        if a[i] >= min_w as f64 - 1e-6 { i += 1; continue; }
        let mut j = i; let mut worst = i;
        while j + 1 < n && a[j] < min_w as f64 - 1e-6 {
            if a[j] < a[worst] { worst = j; }
            j += 1;
        }
        issues.push(NeckdownIssue {
            trace_id, net_id: net, layer, pos: (q[worst] + q[worst + 1]) * 0.5f32,
            required_width: std_max(0.0, a[worst]) as f32, min_width: min_w,
        });
        i = j;
    }
    let mut cons = Vec::new();
    for i in 0..n - 1 {
        a[i] = std_max(a[i], min_w as f64);
        if a[i] < w as f64 - 1e-5 { cons.push(i); }
    }
    let mut stub_w = Vec::new();
    if settings.neckdown_pad_stub && !cons.is_empty() {
        const K_ENTRY: f64 = 0.3; const K_STUB_OUT: f64 = 1.0; const K_STUB_MAX: f64 = 3.0; const K_PIECE: f64 = 2.0;
        let k_gap = std_max(0.3, taper as f64);
        let total = s[n - 2] + 0.5 * q[n - 2].distance_to(q[n - 1]) as f64;
        let mut stub_end = [-1.0, -1.0];
        let at = |side: usize, k: usize| if side == 0 { k } else { n - 2 - k };
        let dist = |side: usize, k: usize| if side == 0 { s[k] } else { total - s[n - 2 - k] };
        let half = |side: usize, k: usize| 0.5 * q[at(side, k)].distance_to(q[at(side, k) + 1]) as f64;
        for side in 0..2 {
            let Some(pad) = end_pad_storage[side].as_ref() else { continue; };
            if !pad_on_layer(pad, layer, c.n_l) { continue; }
            let pf = padgeom::frame(pad);
            let mut exit_d = 0.0;
            let pad_reach = 0.5 * (pad.size.x as f64).hypot(pad.size.y as f64) + 0.1;
            for k in 0..n - 1 {
                if !(dist(side, k) <= pad_reach) { break; }
                let mid = (q[at(side, k)] + q[at(side, k) + 1]) * 0.5f32;
                if padgeom::point_distance(&pf, mid.x as f64, mid.y as f64) <= 0.0 { exit_d = dist(side, k) + half(side, k); }
            }
            let mut cons_end = -1.0; let mut runs_on = false;
            for k in 0..n - 1 {
                let d = dist(side, k); let h = half(side, k);
                if d - h > if cons_end < 0.0 { exit_d + K_ENTRY } else { cons_end + k_gap } { break; }
                if a[at(side, k)] >= w as f64 - 1e-5 { continue; }
                if d + h > K_STUB_MAX { runs_on = cons_end < 0.0 || d - h - cons_end <= 0.1; break; }
                cons_end = d + h;
            }
            if cons_end < 0.0 || runs_on { continue; }
            stub_end[side] = std_max(cons_end, std_min(exit_d + K_STUB_OUT * w as f64, K_STUB_MAX));
            let far = &ends[1 - side];
            if far.terminal_pad < 0 && far.attached_via < 0 && total - stub_end[side] <= K_PIECE { stub_end[side] = total; }
        }
        if stub_end[0] >= 0.0 && stub_end[1] >= 0.0 && stub_end[0] + stub_end[1] > total - k_gap { stub_end = [total, total]; }
        for side in 0..2 {
            if stub_end[side] < 0.0 { continue; }
            let mut neck_w = w as f64;
            for k in 0..n - 1 { if !(dist(side, k) <= stub_end[side]) { break; } neck_w = std_min(neck_w, a[at(side, k)]); }
            neck_w = std_max((neck_w * 200.0 + 1e-3).floor() / 200.0, min_w as f64);
            if neck_w >= w as f64 - 1e-5 { continue; }
            if stub_w.is_empty() { stub_w.resize(n - 1, K_BIG); }
            for k in 0..n - 1 { if !(dist(side, k) <= stub_end[side]) { break; } let idx = at(side, k); stub_w[idx] = std_min(stub_w[idx], neck_w); }
        }
        if !stub_w.is_empty() { cons.retain(|&j| stub_w[j] >= K_BIG); }
    }
    let mut ws = vec![w as f64; n - 1]; let mut lo = 0;
    for i in 0..n - 1 {
        while lo < cons.len() && s[cons[lo]] < s[i] - taper as f64 { lo += 1; }
        let mut width = w as f64;
        let mut ci = lo;
        while ci < cons.len() && s[cons[ci]] <= s[i] + taper as f64 {
            let j = cons[ci];
            width = std_min(width, a[j] + (w as f64 - a[j]) * smoothstep01((s[i] - s[j]).abs() / taper as f64));
            ci += 1;
        }
        if !stub_w.is_empty() { width = std_min(width, stub_w[i]); }
        ws[i] = width;
    }
    let mut pw = vec![0.0f32; n];
    for k in 0..n {
        let width = if k == 0 { ws[0] } else if k + 1 == n { ws[n - 2] } else { std_min(ws[k - 1], ws[k]) };
        pw[k] = width as f32;
    }
    if let Some(nominal_out) = nominal_out { *nominal_out = p; }
    let tr = &mut c.b.traces[t as usize];
    tr.smoothed = q; tr.point_widths = pw;
    let mut cursor = 0;
    for tp in &mut tr.points {
        let mut best = cursor; let mut bd = 1e30f32;
        for k in cursor..tr.smoothed.len() {
            let d = tr.smoothed[k].distance_squared_to(tp.pos);
            if d < bd { bd = d; best = k; }
            if d < 1e-10f32 { break; }
        }
        tp.width = tr.point_widths[best]; cursor = best;
    }
}

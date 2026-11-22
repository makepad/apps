// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.cpp (Engine).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Bounds, static and dynamic copper indices, and deduplicated obstacle queries.

use super::{finite_vec, Engine, EngineScratch, KoEdge, Ob, ObKind};
use crate::organic::copper::{pad_model, PadModel};
use crate::organic::geom::{std_max, std_min, Box2, Vec2};
use crate::organic::grid::{UniformGrid, UNIFORM_GRID_MAX_CELLS};
use crate::organic::model::OBoard;

impl Engine {
    /// `computeBounds` (OrganicRelaxer.cpp:510-528).
    pub fn compute_bounds(&mut self, b: &OBoard) {
        let mut bb = Box2::default();
        for &p in &b.outline {
            if finite_vec(p) { bb.expand(p); }
        }
        for p in &b.pads {
            if finite_vec(p.center) { bb.expand(p.center); }
        }
        for v in &b.vias {
            if finite_vec(v.pos) { bb.expand(v.pos); }
        }
        for t in &b.traces {
            for p in &t.points {
                if finite_vec(p.pos) { bb.expand(p.pos); }
            }
        }
        if !bb.is_valid() { bb = Box2::new(Vec2::new(0.0, 0.0), Vec2::new(1.0, 1.0)); }
        bb.pad(2.0);
        self.c.bounds = bb;
        self.c.board_center = bb.center();
        if b.outline.len() >= 3 {
            let mut ob = Box2::default();
            for &p in &b.outline { ob.expand(p); }
            self.c.board_center = ob.center();
        }
    }

    /// `buildStatic` (OrganicRelaxer.cpp:533-656).
    pub fn build_static(&mut self, b: &OBoard, recompute_bounds: bool) {
        if recompute_bounds { self.compute_bounds(b); }
        let n_p = b.pads.len();
        self.c.pad_m.resize(n_p, PadModel::default());
        self.c.edges.clear();
        if b.outline.len() >= 3 {
            for i in 0..b.outline.len() {
                self.c.edges.push((b.outline[i], b.outline[(i + 1) % b.outline.len()]));
            }
        }
        for c in &b.cutouts {
            if c.len() >= 3 {
                for i in 0..c.len() { self.c.edges.push((c[i], c[(i + 1) % c.len()])); }
            }
        }
        self.c.sgrid.init(&self.c.bounds, 1.0, UNIFORM_GRID_MAX_CELLS);
        for i in 0..n_p {
            let p = &b.pads[i];
            self.c.pad_m[i] = pad_model(p);
            let ex = self.c.pad_m[i].ext as f32;
            let pc = self.c.pad_m[i].center;
            self.c.sgrid.add(i as i32, pc.x - ex, pc.y - ex, pc.x + ex, pc.y + ex);
        }
        for (e, &(a, b)) in self.c.edges.iter().enumerate() {
            self.c.sgrid.add((n_p + e) as i32, std_min(a.x, b.x), std_min(a.y, b.y), std_max(a.x, b.x), std_max(a.y, b.y));
        }
        self.c.ko_edges.clear();
        self.c.ko_max_g = 0.0;
        for k in &b.keepouts {
            if k.poly.len() < 3 || !(k.no_tracks || k.no_vias) { continue; }
            // ADAPTED: pour hook 1, the reference's avoid-area net filter on ordinary polygon edges.
            let mut ke = KoEdge { only_net: k.only_net, ..KoEdge::default() };
            for l in 0..std::cmp::min(self.n_l, 64) {
                // ADAPTED: OrganicRelaxer.cpp:568 uses stack-index bits for keepoutOnLayer.
                if !(k.poly.len() >= 3 && l < 64 && (k.layer_mask >> l) & 1 != 0) { continue; }
                if k.no_tracks { ke.track_layers |= 1u64 << l; }
                if k.no_vias { ke.via_layers |= 1u64 << l; }
            }
            if ke.track_layers == 0 && ke.via_layers == 0 { continue; }
            // ADAPTED: OrganicRelaxer.cpp:574 / Drc.h:337 uses std_max(k.clearance, 0.0) for keepoutMargin.
            ke.thr = (std_max(k.clearance, 0.0f32) + std_max(0.0f32, self.s.guard_clearance_slack)) as f64;
            self.c.ko_max_g = std_max(self.c.ko_max_g, ke.thr);
            let mut j = k.poly.len() - 1;
            for i in 0..k.poly.len() {
                if !(k.poly[i].distance_squared_to(k.poly[j]) < 1e-12f32) {
                    ke.a = k.poly[j];
                    ke.b = k.poly[i];
                    self.c.ko_edges.push(ke);
                }
                j = i;
            }
        }
        // ADAPTED: OrganicRelaxer.cpp:584-600 (brief:586-602) copperShapes are absent from OBoard; omitted.
        // ADAPTED: OrganicRelaxer.cpp:601-619 (brief:603-620): avoid areas arrive as net-filtered
        // keepout rings above, without exempt-zone containment tests (area remains -1).
        self.c.cy_nets.clear();
        // ADAPTED: OrganicRelaxer.cpp:620-648 (brief:621-646) component courtyards are absent; omitted.
        for (e, ke) in self.c.ko_edges.iter().enumerate() {
            self.c.sgrid.add((n_p + self.c.edges.len() + e) as i32,
                std_min(ke.a.x, ke.b.x), std_min(ke.a.y, ke.b.y), std_max(ke.a.x, ke.b.x), std_max(ke.a.y, ke.b.y));
        }
        self.c.sgrid.finalize();
        // ADAPTED: pour hook 6. A net-filtered keepout edge (`only_net`) applies to its own net
        // alone (`eval_pair` passes it for every other), so a query on behalf of one net reads a
        // copy of the grid without the other nets' edges: it finds what the full grid finds, in
        // the same order, less pairs that would not apply. One filtered copy per masked net and
        // one without any masks, built here with the static index; none without such edges.
        self.c.net_sgrids.clear();
        self.c.net_sgrid.clear();
        let owners: std::collections::BTreeSet<i32> =
            self.c.ko_edges.iter().filter(|k| k.only_net >= 0).map(|k| k.only_net).collect();
        if let Some(&last) = owners.last() {
            let ko = &self.c.ko_edges;
            let base = (n_p + self.c.edges.len()) as i32;
            let keep = |net: Option<i32>| move |id: i32| id < base || {
                let only = ko[(id - base) as usize].only_net;
                only < 0 || Some(only) == net
            };
            self.c.net_sgrids.push(self.c.sgrid.filtered(keep(None)));
            self.c.net_sgrid.resize(last as usize + 1, 0);
            for net in owners {
                self.c.net_sgrid[net as usize] = self.c.net_sgrids.len();
                let grid = self.c.sgrid.filtered(keep(Some(net)));
                self.c.net_sgrids.push(grid);
            }
        }
    }

    /// The static grid a query on behalf of `net` reads (pour hook 6): `sgrid` without the other
    /// nets' masks, or `sgrid` itself for `None` (a query for several nets).
    pub fn sgrid_for(&self, net: Option<i32>) -> &UniformGrid {
        match net {
            Some(net) if !self.c.net_sgrids.is_empty() => {
                let i = usize::try_from(net).ok().and_then(|n| self.c.net_sgrid.get(n)).copied().unwrap_or(0);
                &self.c.net_sgrids[i]
            }
            _ => &self.c.sgrid,
        }
    }

    /// `buildDynamic` (OrganicRelaxer.cpp:662-782), including the flagged-trace region.
    pub fn build_dynamic(&mut self, b: &OBoard, only: Option<&[u8]>, only_reach: f64, only_boxes: Option<&[Box2]>) {
        self.c.seg_trace.clear();
        self.c.seg_idx.clear();
        self.c.seg_base.clear();
        self.c.seg_base.resize(b.traces.len() + 1, 0);
        self.c.max_half_w = 0.0;
        for (t, tr) in b.traces.iter().enumerate() {
            self.c.seg_base[t] = self.c.seg_trace.len() as i32;
            self.c.max_half_w = std_max(self.c.max_half_w, tr.width as f64 * 0.5);
            for k in 0..tr.points.len().saturating_sub(1) {
                self.c.seg_trace.push(t as i32);
                self.c.seg_idx.push(k as i32);
            }
        }
        self.c.seg_base[b.traces.len()] = self.c.seg_trace.len() as i32;
        self.c.n_segs = self.c.seg_trace.len() as i32;
        self.c.max_via_r = 0.0;
        for v in &b.vias { self.c.max_via_r = std_max(self.c.max_via_r, v.diameter as f64 * 0.5); }

        let mut n_layers_seen = self.n_l;
        for tr in &b.traces { n_layers_seen = std::cmp::max(n_layers_seen, tr.layer + 1); }
        self.c.lgrid.resize(n_layers_seen as usize, UniformGrid::default());
        let part = only.map_or(false, |only| only.len() == b.traces.len());
        let (mut rnx, mut rny) = (0, 0);
        let bounds = self.c.bounds;
        let cell_range = |x0: f32, y0: f32, x1: f32, y1: f32, rnx: i32, rny: i32| {
            let cl = |v: f32, n: i32| if !(v > 0.0f32) { 0 } else { std::cmp::min(n - 1, v as i32) };
            (cl(x0 - bounds.min.x, rnx), cl(y0 - bounds.min.y, rny),
             cl(x1 - bounds.min.x, rnx), cl(y1 - bounds.min.y, rny))
        };
        let mut rect = vec![Box2::default(); self.c.lgrid.len()];
        if part {
            rnx = std::cmp::max(1, (bounds.max.x - bounds.min.x).ceil() as i32 + 1);
            rny = std::cmp::max(1, (bounds.max.y - bounds.min.y).ceil() as i32 + 1);
            self.c.region_bits.resize(self.c.lgrid.len(), Vec::new());
            for bits in &mut self.c.region_bits { bits.clear(); }
            let r = only_reach as f32;
            for (t, tr) in b.traces.iter().enumerate() {
                if only.unwrap()[t] == 0 || tr.layer < 0 { continue; }
                let bits = &mut self.c.region_bits[tr.layer as usize];
                if bits.is_empty() { bits.resize(rnx as usize * rny as usize, 0); }
                for k in 0..tr.points.len().saturating_sub(1) {
                    let a = tr.points[k].pos;
                    let b = tr.points[k + 1].pos;
                    let (x0, y0, x1, y1) = if !finite_vec(a) || !finite_vec(b) {
                        (0, 0, rnx - 1, rny - 1)
                    } else {
                        cell_range(std_min(a.x, b.x) - r, std_min(a.y, b.y) - r,
                            std_max(a.x, b.x) + r, std_max(a.y, b.y) + r, rnx, rny)
                    };
                    for y in y0..=y1 {
                        let start = y as usize * rnx as usize + x0 as usize;
                        bits[start..start + (x1 - x0 + 1) as usize].fill(1);
                    }
                    rect[tr.layer as usize].expand_box(&Box2::new(
                        Vec2::new(bounds.min.x + x0 as f32, bounds.min.y + y0 as f32),
                        Vec2::new(bounds.min.x + (x1 + 1) as f32, bounds.min.y + (y1 + 1) as f32)));
                }
            }
            for (l, grid) in self.c.lgrid.iter_mut().enumerate() {
                let mut g = rect[l];
                if !g.is_valid() { g = Box2::new(bounds.min, bounds.min + Vec2::new(1.0, 1.0)); }
                g.pad(1.0);
                grid.init(&g, 1.0, UNIFORM_GRID_MAX_CELLS);
            }
        } else {
            for g in &mut self.c.lgrid { g.init(&bounds, 1.0, UNIFORM_GRID_MAX_CELLS); }
        }
        for (t, tr) in b.traces.iter().enumerate() {
            if tr.layer < 0 { continue; }
            let mut bits = None;
            if part {
                let region = &self.c.region_bits[tr.layer as usize];
                bits = Some(region);
                if region.is_empty() { continue; }
                if only.unwrap()[t] == 0 {
                    if let Some(only_boxes) = only_boxes {
                        if t < only_boxes.len() {
                            let tb = &only_boxes[t];
                            let rg = &rect[tr.layer as usize];
                            if tb.is_valid() && rg.is_valid() &&
                                (tb.min.x > rg.max.x || tb.max.x < rg.min.x || tb.min.y > rg.max.y || tb.max.y < rg.min.y) {
                                continue;
                            }
                        }
                    }
                }
            }
            for s in self.c.seg_base[t]..self.c.seg_base[t + 1] {
                let a = tr.points[self.c.seg_idx[s as usize] as usize].pos;
                let b = tr.points[self.c.seg_idx[s as usize] as usize + 1].pos;
                if !finite_vec(a) || !finite_vec(b) { continue; }
                let x0 = std_min(a.x, b.x);
                let y0 = std_min(a.y, b.y);
                let x1 = std_max(a.x, b.x);
                let y1 = std_max(a.y, b.y);
                if let Some(bits) = bits {
                    let (cx0, cy0, cx1, cy1) = cell_range(x0, y0, x1, y1, rnx, rny);
                    let mut hit = false;
                    for y in cy0..=cy1 {
                        if hit { break; }
                        for x in cx0..=cx1 {
                            if bits[y as usize * rnx as usize + x as usize] != 0 { hit = true; break; }
                        }
                    }
                    if !hit { continue; }
                }
                self.c.lgrid[tr.layer as usize].add(s, x0, y0, x1, y1);
            }
        }
        for g in &mut self.c.lgrid { g.finalize(); }
        self.c.vgrid.init(&bounds, 1.0, UNIFORM_GRID_MAX_CELLS);
        for (v, via) in b.vias.iter().enumerate() {
            if !finite_vec(via.pos) { continue; }
            let r = via.diameter * 0.5f32;
            self.c.vgrid.add(self.c.n_segs + v as i32, via.pos.x - r, via.pos.y - r, via.pos.x + r, via.pos.y + r);
        }
        self.c.vgrid.finalize();
    }

    /// The `X0` overload of `gather` (OrganicRelaxer.cpp:787).
    pub fn gather_x0(&mut self, b: &OBoard, minx: f32, miny: f32, maxx: f32, maxy: f32, layer: i32, net: Option<i32>) {
        let mut x = std::mem::take(&mut self.x0);
        self.gather(b, &mut x, minx, miny, maxx, maxy, layer, net);
        self.x0 = x;
    }

    /// `gather` (OrganicRelaxer.cpp:788-869); only segments are filtered by query layer. `net` is
    /// the net every primitive of the query belongs to, if one: its static obstacles come from
    /// `sgrid_for` (ADAPTED: pour hook 6).
    #[allow(clippy::too_many_arguments)]
    pub fn gather(&self, b: &OBoard, x: &mut EngineScratch, minx: f32, miny: f32, maxx: f32, maxy: f32, layer: i32,
        net: Option<i32>) {
        let EngineScratch { obs, stamp_s, stamp_d, stamp, .. } = x;
        obs.clear();
        let n_s = b.pads.len() + self.c.edges.len() + self.c.ko_edges.len();
        let n_d = self.c.n_segs as usize + b.vias.len();
        if stamp_s.len() != n_s { stamp_s.clear(); stamp_s.resize(n_s, 0); }
        if stamp_d.len() != n_d { stamp_d.clear(); stamp_d.resize(n_d, 0); }
        *stamp = stamp.wrapping_add(1);
        if *stamp == 0 {
            stamp_s.fill(0);
            stamp_d.fill(0);
            *stamp = 1;
        }
        let n_p = b.pads.len() as i32;
        self.sgrid_for(net).query(minx, miny, maxx, maxy, |id| {
            if stamp_s[id as usize] == *stamp { return; }
            stamp_s[id as usize] = *stamp;
            let mut o = Ob::default();
            if id < n_p {
                let p = &b.pads[id as usize];
                let ex = self.c.pad_m[id as usize].ext as f32;
                let pc = self.c.pad_m[id as usize].center;
                o.kind = ObKind::Pad;
                o.id = id;
                o.x0 = pc.x - ex; o.y0 = pc.y - ex; o.x1 = pc.x + ex; o.y1 = pc.y + ex;
                o.a = pc; o.b = pc; o.net = p.net;
                // ADAPTED: OrganicRelaxer.cpp:813-818 also carries the pad's clearance-table rule.
                o.rule = p.rule;
            } else if id < n_p + self.c.edges.len() as i32 {
                let (a, b) = self.c.edges[(id - n_p) as usize];
                o.kind = ObKind::Edge; o.id = id - n_p;
                o.x0 = std_min(a.x, b.x); o.y0 = std_min(a.y, b.y); o.x1 = std_max(a.x, b.x); o.y1 = std_max(a.y, b.y);
                o.a = a; o.b = b;
            } else {
                let ke = &self.c.ko_edges[(id - n_p - self.c.edges.len() as i32) as usize];
                o.kind = ObKind::Ko; o.id = id - n_p - self.c.edges.len() as i32;
                o.x0 = std_min(ke.a.x, ke.b.x); o.y0 = std_min(ke.a.y, ke.b.y);
                o.x1 = std_max(ke.a.x, ke.b.x); o.y1 = std_max(ke.a.y, ke.b.y);
                o.a = ke.a; o.b = ke.b;
            }
            // ADAPTED: OrganicRelaxer.cpp:820-832 edge and keepout rules stay at Ob::default()'s 0.
            if o.x0 > maxx || o.x1 < minx || o.y0 > maxy || o.y1 < miny { return; }
            obs.push(o);
        });
        let mut dyn_ob = |id: i32| {
            if stamp_d[id as usize] == *stamp { return; }
            stamp_d[id as usize] = *stamp;
            let mut o = Ob::default();
            if id < self.c.n_segs {
                let tr = &b.traces[self.c.seg_trace[id as usize] as usize];
                let a = tr.points[self.c.seg_idx[id as usize] as usize].pos;
                let b = tr.points[self.c.seg_idx[id as usize] as usize + 1].pos;
                o.kind = ObKind::Seg; o.id = id;
                o.x0 = std_min(a.x, b.x); o.y0 = std_min(a.y, b.y); o.x1 = std_max(a.x, b.x); o.y1 = std_max(a.y, b.y);
                o.a = a; o.b = b; o.h = tr.width * 0.5f32; o.net = tr.net; o.layer = tr.layer;
                o.trace = self.c.seg_trace[id as usize]; o.seg = self.c.seg_idx[id as usize];
                // ADAPTED: OrganicRelaxer.cpp:843-849 also carries the trace's clearance-table rule.
                o.rule = tr.rule;
            } else {
                let v = &b.vias[(id - self.c.n_segs) as usize];
                let r = v.diameter * 0.5f32;
                o.kind = ObKind::Via; o.id = id - self.c.n_segs;
                o.x0 = v.pos.x - r; o.y0 = v.pos.y - r; o.x1 = v.pos.x + r; o.y1 = v.pos.y + r;
                o.a = v.pos; o.b = v.pos; o.h = r; o.net = v.net; o.drill = v.drill;
                // ADAPTED: OrganicRelaxer.cpp:853-857 also carries the via's clearance-table rule.
                o.rule = v.rule;
            }
            if o.x0 > maxx || o.x1 < minx || o.y0 > maxy || o.y1 < miny { return; }
            obs.push(o);
        };
        if layer >= 0 {
            if layer < self.c.lgrid.len() as i32 { self.c.lgrid[layer as usize].query(minx, miny, maxx, maxy, &mut dyn_ob); }
        } else {
            for g in &self.c.lgrid { g.query(minx, miny, maxx, maxy, &mut dyn_ob); }
        }
        self.c.vgrid.query(minx, miny, maxx, maxy, &mut dyn_ob);
    }
}

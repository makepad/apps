// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.cpp (external changes:
// lines 3912-4171).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! External changes: the board is compared with the snapshot of the previous step, what the change
//! invalidates is rebuilt, and the region around it is woken. Everything else keeps sleeping.

use super::nodes::INT_MIN_HALF;
use super::state::{fbits, from_bits, same_pad, same_via, snap_pad, snap_via};
use super::{ExtResult, Relaxation, Snapshot, K_QUIET};
use crate::organic::engine::finite_vec;
use crate::organic::geom::{std_max, Vec2};
use crate::organic::model::OBoard;
use std::collections::{BTreeMap, BTreeSet};

impl Relaxation {
    /// The `dirtyBoth` lambda of `applyExternal`.
    fn dirty_both(&mut self, a: Vec2, b: Vec2, s0: i32) {
        self.mark_dirty(a, s0);
        self.mark_dirty(b, s0);
        self.mark_focus(a);
        self.mark_focus(b);
    }

    /// `applyExternal`: compares the board with the snapshot of the previous step, rebuilds what
    /// the change invalidates and wakes the region around it (dirty cells, the changed nodes
    /// themselves and their topological neighbours via last_move).
    pub fn apply_external(&mut self, b: &OBoard, sn: &Snapshot, valid: bool, gh: u64) -> ExtResult {
        let mut x = ExtResult::default();
        if !valid || gh != sn.global || b.pads.len() != sn.pd.len() {
            x.full = true;
            x.topo = true;
            x.any = true;
            self.e.build_static(b, true);
            self.init_cells();
            self.build_topology(b);
            self.e.build_dynamic(b, None, 0.0, None);
            self.c.drift = 0.0;
            self.e.compute_essential(b, None, 0.0);
            self.reset_all(b);
            return x;
        }
        let s0 = self.c.sub; // wake-ups for the next substep (sub + 1 checks >= sub)
        let mut nets: Vec<i32> = Vec::new();
        let mut max_disp = 0.0f32;
        // pads
        let mut pad_changed = false;
        for i in 0..b.pads.len() {
            let np = snap_pad(&b.pads[i]);
            if same_pad(&np, &sn.pd[i]) {
                continue;
            }
            pad_changed = true;
            let p = &b.pads[i];
            let oc = from_bits(sn.pd[i].cx, sn.pd[i].cy);
            let os = from_bits(sn.pd[i].sx, sn.pd[i].sy);
            for pass in 0..2 {
                let c = if pass != 0 { p.center } else { oc };
                let sz = (if pass != 0 { p.size } else { os }) * 0.5f32;
                let mut fx = -1.0f32;
                while fx <= 1.01f32 {
                    let mut fy = -1.0f32;
                    while fy <= 1.01f32 {
                        self.dirty_both(c + Vec2::new(sz.x * fx, sz.y * fy), c, s0);
                        fy += 0.5f32;
                    }
                    fx += 0.5f32;
                }
            }
            if p.net >= 0 {
                nets.push(p.net);
            }
            if sn.pd[i].net >= 0 {
                nets.push(sn.pd[i].net);
            }
        }
        if pad_changed {
            self.e.build_static(b, false);
        }

        // traces
        let tn = b.traces.len();
        let mut same_idx = tn == sn.tr.len();
        let mut t = 0;
        while same_idx && t < tn {
            if b.traces[t].id != sn.tr[t].id {
                same_idx = false;
            }
            t += 1;
        }
        let mut old_of = vec![-1i32; tn];
        if same_idx {
            for t in 0..tn {
                old_of[t] = t as i32;
            }
        } else {
            x.topo = true;
            // ADAPTED: the reference's unordered_map id -> old index is a BTreeMap (lookups only;
            // emplace keeps the first index of an id, as or_insert does).
            let mut by_id: BTreeMap<i32, i32> = BTreeMap::new();
            for o in 0..sn.tr.len() {
                by_id.entry(sn.tr[o].id).or_insert(o as i32);
            }
            for t in 0..tn {
                if let Some(&o) = by_id.get(&b.traces[t].id) {
                    old_of[t] = o;
                }
            }
            // move the per-trace state to the new indexing
            let mut q2: Vec<Vec<u8>> = vec![Vec::new(); tn];
            let mut a2: Vec<Vec<Vec2>> = vec![Vec::new(); tn];
            let mut l2: Vec<Vec<i32>> = vec![Vec::new(); tn];
            let mut tt2 = vec![INT_MIN_HALF; tn];
            let mut ta2 = vec![0i32; tn];
            let mut rl2 = vec![0i32; tn];
            let mut rn2 = vec![0i32; tn];
            let mut rf2 = vec![1u8; tn];
            let mut used_old = vec![0u8; sn.tr.len()];
            for t in 0..tn {
                let o = old_of[t];
                if o < 0 || o >= self.c.quiet.len() as i32 {
                    continue;
                }
                let o = o as usize;
                used_old[o] = 1;
                q2[t] = std::mem::take(&mut self.c.quiet[o]);
                a2[t] = std::mem::take(&mut self.c.anchor[o]);
                l2[t] = std::mem::take(&mut self.c.last_move[o]);
                tt2[t] = self.c.tr_touch[o];
                ta2[t] = self.c.tr_awake[o];
                rl2[t] = self.c.resample_level[o];
                rn2[t] = self.c.resample_next[o];
                rf2[t] = self.c.resample_flag[o];
            }
            for o in 0..sn.tr.len() {
                if used_old[o] == 0 {
                    if sn.tr[o].net >= 0 {
                        nets.push(sn.tr[o].net);
                    }
                    for k in 0..sn.tr[o].n {
                        let op = &sn.pt[(sn.tr[o].off + k) as usize];
                        let p = from_bits(op.x, op.y);
                        self.dirty_both(p, p, s0);
                    }
                }
            }
            self.c.quiet = q2;
            self.c.anchor = a2;
            self.c.last_move = l2;
            self.c.tr_touch = tt2;
            self.c.tr_awake = ta2;
            self.c.resample_level = rl2;
            self.c.resample_next = rn2;
            self.c.resample_flag = rf2;
            self.c.resample_tries = vec![0; tn];
        }
        let mut reset_list: Vec<(i32, i32)> = Vec::new(); // (trace, old index or -1)
        for t in 0..tn {
            let tr = &b.traces[t];
            let o = old_of[t];
            let mut same = o >= 0;
            if same {
                let ot = &sn.tr[o as usize];
                // ADAPTED: a changed clearance-table row is a changed trace too.
                same = ot.net == tr.net
                    && ot.layer == tr.layer
                    && ot.width == fbits(tr.width)
                    && ot.routed == tr.is_routed as i32
                    && ot.n == tr.points.len() as i32
                    && self.c.quiet[t].len() == tr.points.len()
                    && ot.rule == tr.rule;
                let mut k = 0;
                while same && k < ot.n {
                    if !sn.pt[(ot.off + k) as usize].same_flags(&tr.points[k as usize]) {
                        same = false;
                    }
                    k += 1;
                }
            }
            if !same {
                x.topo = true;
                reset_list.push((t as i32, o));
                if o >= 0 && sn.tr[o as usize].net >= 0 {
                    nets.push(sn.tr[o as usize].net);
                }
                if tr.net >= 0 {
                    nets.push(tr.net);
                }
                continue;
            }
            let ot = &sn.tr[o as usize];
            let mut net_added = false;
            for k in 0..ot.n {
                let op = &sn.pt[(ot.off + k) as usize];
                let p = &tr.points[k as usize];
                if op.x == fbits(p.pos.x) && op.y == fbits(p.pos.y) {
                    continue;
                }
                let old = from_bits(op.x, op.y);
                self.dirty_both(old, p.pos, s0);
                if finite_vec(old) && finite_vec(p.pos) {
                    max_disp = std_max(max_disp, old.distance_to(p.pos));
                } else {
                    max_disp = 1e9f32;
                }
                self.wake(b, t as i32, k);
                self.c.anchor[t][k as usize] = p.pos;
                self.touch(t as i32, k, s0);
                self.flag_resample(t as i32);
                x.changed_nodes += 1;
                if !net_added && tr.net >= 0 {
                    nets.push(tr.net);
                    net_added = true;
                }
            }
        }
        // vias
        let mut via_topo = false;
        if b.vias.len() == sn.vi.len() && self.c.vquiet.len() == b.vias.len() {
            for v in 0..b.vias.len() {
                let nv = snap_via(&b.vias[v]);
                if same_via(&nv, &sn.vi[v]) {
                    continue;
                }
                let old = from_bits(sn.vi[v].x, sn.vi[v].y);
                let via = &b.vias[v];
                let mut pos_only = sn.vi[v];
                pos_only.x = nv.x;
                pos_only.y = nv.y;
                if !same_via(&pos_only, &nv) {
                    via_topo = true;
                }
                self.dirty_both(old, via.pos, s0);
                if finite_vec(old) && finite_vec(via.pos) {
                    max_disp = std_max(max_disp, old.distance_to(via.pos) + via.diameter);
                } else {
                    max_disp = 1e9f32;
                }
                self.reset_via(b, v as i32);
                if via.net >= 0 {
                    nets.push(via.net);
                }
                if sn.vi[v].net >= 0 {
                    nets.push(sn.vi[v].net);
                }
            }
        } else {
            via_topo = true;
            // match by (position, net); unmatched vias are new (awake), unmatched old ones dirty
            let mut by_key: BTreeMap<(u32, u32, i32), i32> = BTreeMap::new();
            for o in 0..sn.vi.len() {
                by_key.entry((sn.vi[o].x, sn.vi[o].y, sn.vi[o].net)).or_insert(o as i32);
            }
            let mut vq2 = vec![0u8; b.vias.len()];
            let mut used = vec![0u8; sn.vi.len()];
            let mut va2 = vec![Vec2::default(); b.vias.len()];
            for v in 0..b.vias.len() {
                let via = &b.vias[v];
                if let Some(&o) = by_key.get(&(fbits(via.pos.x), fbits(via.pos.y), via.net)) {
                    if used[o as usize] == 0 && o < self.c.vquiet.len() as i32 {
                        used[o as usize] = 1;
                        vq2[v] = self.c.vquiet[o as usize];
                        va2[v] = self.c.vanchor[o as usize];
                        continue;
                    }
                }
                vq2[v] = 0;
                va2[v] = via.pos;
                self.dirty_both(via.pos, via.pos, s0);
                if via.net >= 0 {
                    nets.push(via.net);
                }
            }
            for o in 0..sn.vi.len() {
                if used[o] == 0 {
                    let p = from_bits(sn.vi[o].x, sn.vi[o].y);
                    self.dirty_both(p, p, s0);
                    if sn.vi[o].net >= 0 {
                        nets.push(sn.vi[o].net);
                    }
                }
            }
            self.c.vquiet = vq2;
            self.c.vanchor = va2;
        }
        nets.sort_unstable();
        nets.dedup();
        for &n in &nets {
            self.flag_net(n);
        }
        x.any = pad_changed || x.topo || via_topo || x.changed_nodes > 0 || !nets.is_empty();
        if x.topo || via_topo {
            x.topo = true;
            self.build_topology(b);
            self.e.build_dynamic(b, None, 0.0, None);
            self.c.drift = 0.0;
            self.e.compute_essential(b, None, 0.0);
            // a structural change (edit, deduplication, loop simplification) restarts the
            // resampling backoff of every trace (merges around it become possible again)
            self.c.resample_level.fill(0);
            self.c.resample_next.fill(0);
            for t in 0..self.c.resample_flag.len() {
                self.flag_resample(t as i32);
            }
            // A structurally edited trace (inserted / removed nodes, changed pins) only wakes
            // around the nodes that are new or gone: nodes still at an old node position (same
            // flags) keep sleeping (e.g. the subdivision when a drag grabs a long trace).
            let key = |x: u32, y: u32| ((x as u64) << 32) | y as u64;
            for &(t, o) in &reset_list {
                self.reset_trace(b, t);
                self.flag_resample(t);
                // ADAPTED: unordered_map / unordered_set are a BTreeMap / BTreeSet, so the gone
                // nodes below are marked in key order (deterministic dirty-list order).
                let mut old_at: BTreeMap<u64, i32> = BTreeMap::new();
                if o >= 0 {
                    let ot = &sn.tr[o as usize];
                    for k in 0..ot.n {
                        let op = &sn.pt[(ot.off + k) as usize];
                        old_at.entry(key(op.x, op.y)).or_insert(k);
                    }
                }
                let mut kept: BTreeSet<u64> = BTreeSet::new();
                let pts = &b.traces[t as usize].points;
                for k in 0..pts.len() {
                    let kk = key(fbits(pts[k].pos.x), fbits(pts[k].pos.y));
                    if let Some(&ok) = old_at.get(&kk) {
                        if sn.pt[(sn.tr[o as usize].off + ok) as usize].same_flags(&pts[k]) {
                            kept.insert(kk);
                            if self.c.quiet[t as usize][k] < K_QUIET {
                                self.set_quiet(t, k as i32, K_QUIET);
                            }
                            continue;
                        }
                    }
                    self.mark_dirty(pts[k].pos, s0);
                    self.mark_focus(pts[k].pos);
                    self.touch(t, k as i32, s0);
                }
                for (&kk, &k) in &old_at {
                    if !kept.contains(&kk) {
                        let op = &sn.pt[(sn.tr[o as usize].off + k) as usize];
                        let p = from_bits(op.x, op.y);
                        self.mark_dirty(p, s0);
                        self.mark_focus(p);
                    }
                }
            }
        } else if x.any {
            self.c.drift += max_disp;
            if self.c.drift >= self.e.max_step as f32 {
                self.e.build_dynamic(b, None, 0.0, None);
                self.c.drift = 0.0;
            }
            if !nets.is_empty() {
                self.e.compute_essential(b, Some(&nets), self.c.drift as f64);
            }
        }
        x
    }
}

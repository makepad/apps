// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.cpp (Relaxation: resampling).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Dirty-trace resampling and junction zipping (OrganicRelaxer.cpp:5548-5926).

use super::nodes::is_pinned;
use super::{ops, Relaxation, K_PAIR_NODE_SPACING, K_POUR_NODE_SPACING_SCALE, K_QUIET};
use crate::organic::copper::trace_key;
use crate::organic::engine::{finite_vec, Prim};
use crate::organic::geom::{std_clamp, std_max, std_min, Vec2};
use crate::organic::model::{OBoard, TracePoint};

pub const K_MAX_TUNABLE_SKEW: f64 = 2.0; // mm
pub const K_SKEW_CORRECTIONS: i32 = 6;

impl Relaxation {
    /// `flagTracesNearDirty`.
    pub fn flag_traces_near_dirty(&mut self) {
        self.c.step_dirty.sort_unstable();
        self.c.step_dirty.dedup();
        let resample_flag = &mut self.c.resample_flag;
        for &c in &self.c.step_dirty {
            let cx = c % self.c.dnx;
            let cy = c / self.c.dnx;
            let x0 = self.e.c.bounds.min.x + (cx - 1) as f32 * self.c.dcs;
            let y0 = self.e.c.bounds.min.y + (cy - 1) as f32 * self.c.dcs;
            let x1 = x0 + 3.0f32 * self.c.dcs;
            let y1 = y0 + 3.0f32 * self.c.dcs;
            for g in &self.e.c.lgrid {
                g.query(x0, y0, x1, y1, |id| {
                    if id < self.e.c.n_segs {
                        resample_flag[self.e.c.seg_trace[id as usize] as usize] = 1;
                    }
                });
            }
        }
        self.c.step_dirty.clear();
    }

    /// Cut a branch's overlapping run along its trunk and reattach where it leaves the
    /// trunk. DRC-guarded, with at most one zip per host / branch trace and call.
    pub fn zip_junctions(&mut self, b: &mut OBoard) -> bool {
        const K_ZIP_COS: f32 = 0.966; // first branch leg closer than 15 deg to a trunk leg
        let mut touched: Vec<u8> = Vec::new();
        struct Zip {
            h: i32,
            bt: i32,
            e: i32,
            at: i32, // trunk: insertion index (insert) or node index (attach)
            insert: bool,
            q: Vec2,
            keep_from: i32, // branch: first kept node counted from the junction end (>= 1)
        }
        let mut zips: Vec<Zip> = Vec::new();
        let mut used = vec![0u8; b.traces.len()];
        // host_branches is a BTreeMap: its key order is the C++ hb copy-and-sort order.
        let is_frozen = |t: i32| t < self.c.frozen.len() as i32 && self.c.frozen[t as usize] != 0;
        for (&key, br) in &self.c.host_branches {
            let h = (key >> 32) as i32;
            let k = (key & 0xffff_ffffu64) as i32;
            let ht = &b.traces[h as usize];
            let hn = ht.points.len() as i32;
            // Only crowded junctions (two or more branches on one host node); a plain T
            // settles by itself.
            if used[h as usize] != 0 || k <= 0 || k + 1 >= hn || is_frozen(h) || br.len() < 2 {
                continue;
            }
            let h_pos = ht.points[k as usize].pos;
            for &(bt, e) in br {
                if used[h as usize] != 0 || used[bt as usize] != 0 || is_frozen(bt) || bt == h {
                    continue;
                }
                let btr = &b.traces[bt as usize];
                let bn = btr.points.len() as i32;
                if bn < 3 || btr.layer != ht.layer {
                    continue;
                }
                // i-th node from the junction end.
                let b_idx = |i: i32| if e == 0 { i } else { bn - 1 - i };
                let nb = btr.points[b_idx(1) as usize].pos;
                let ub = nb - h_pos;
                let lb = ub.length();
                if lb < 1e-4f32 || !finite_vec(nb) {
                    continue;
                }
                let tol = 0.5f32 * std_max(0.05f32, std_min(ht.width, btr.width));
                for side in [-1, 1] {
                    if ops::straight_segment(&ht.points, if side < 0 { k - 1 } else { k }) {
                        continue;
                    }
                    let sp = ht.points[(k + side) as usize].pos;
                    let us = sp - h_pos;
                    let ls = us.length();
                    if ls < 1e-4f32 || ub.dot(us) < K_ZIP_COS * lb * ls {
                        continue;
                    }
                    // Walk the branch along the trunk (segment index s from k towards side).
                    let seg = if side < 0 { k - 1 } else { k }; // [seg, seg + 1]
                    let mut last_along = -1.0f32; // polyline parameter: segment + fraction
                    let mut m = 0; // last branch node (from the junction) on the trunk
                    let mut m_seg = seg;
                    let mut m_t = 0.0f32;
                    for i in 1..bn - 1 {
                        let bi = b_idx(i);
                        if is_pinned(&btr.points[bi as usize]) || self.c.branch_target[bt as usize][bi as usize] != 0 {
                            break;
                        }
                        let p = btr.points[bi as usize].pos;
                        // Nearest point on the trunk segments from seg onwards towards side.
                        let mut best = 1e30f32;
                        let mut best_t = 0.0f32;
                        let mut best_along = 0.0f32;
                        let mut best_seg = -1;
                        let mut s = seg;
                        let mut c = 0;
                        while s >= 0 && s + 1 < hn && c < 64 {
                            // P7: never split a pad-exit stub when the pad-exit term is on.
                            if self.s.pad_exit_half_plane > 0.0f32
                                && self.e.partner_of(ht.net) < 0
                                && ops::straight_segment(&ht.points, s)
                            {
                                break;
                            }
                            let a = ht.points[(if side < 0 { s + 1 } else { s }) as usize].pos;
                            let b2 = ht.points[(if side < 0 { s } else { s + 1 }) as usize].pos;
                            let d = b2 - a;
                            let l2 = d.dot(d);
                            let tt = if l2 > 1e-12f32 { std_clamp((p - a).dot(d) / l2, 0.0f32, 1.0f32) } else { 0.0f32 };
                            let dist = p.distance_to(a + d * tt);
                            if dist < best {
                                best = dist;
                                best_t = tt;
                                best_seg = s;
                                best_along = c as f32 + tt;
                            }
                            if dist > 4.0f32 * tol && c > 0 && best <= tol {
                                break;
                            }
                            s += side;
                            c += 1;
                        }
                        let on_trunk = if i == 1 { best <= 1.5f32 * tol } else { best <= tol };
                        if best_seg < 0 || !on_trunk || best_along <= last_along {
                            break;
                        }
                        last_along = best_along;
                        m = i;
                        m_seg = best_seg;
                        m_t = best_t;
                    }
                    if m < 1 {
                        continue;
                    }
                    if m >= bn - 2 {
                        m = bn - 3; // keep at least two branch nodes beyond the new end
                    }
                    if m < 1 {
                        continue;
                    }
                    // Reattachment point: the reference retains m_seg / m_t when capping m.
                    let a = ht.points[(if side < 0 { m_seg + 1 } else { m_seg }) as usize].pos;
                    let b2 = ht.points[(if side < 0 { m_seg } else { m_seg + 1 }) as usize].pos;
                    let mut z = Zip { h, bt, e, at: -1, insert: true, q: Vec2::lerp(a, b2, m_t), keep_from: m + 1 };
                    let ia = if side < 0 { m_seg + 1 } else { m_seg };
                    let ib = if side < 0 { m_seg } else { m_seg + 1 };
                    let attachable = |idx: i32| idx > 0 && idx + 1 < hn && !self.node_pinned(&*b, h, idx) && idx != k;
                    if z.q.distance_to(a) < 0.03f32 && attachable(ia) {
                        z.insert = false;
                        z.at = ia;
                        z.q = a;
                    } else if z.q.distance_to(b2) < 0.03f32 && attachable(ib) {
                        z.insert = false;
                        z.at = ib;
                        z.q = b2;
                    } else if z.q.distance_to(a) < 0.03f32 || z.q.distance_to(b2) < 0.03f32 {
                        continue;
                    } else {
                        z.at = std_max(ia, ib); // insert between ia and ib
                    }
                    if z.q.distance_to(h_pos) < 0.03f32 {
                        continue;
                    }
                    // Guard: branch nodes 0..m on the trunk become q -> node m + 1.
                    let mut p = Prim::default();
                    p.h = btr.width as f64 * 0.5f64;
                    p.layer = btr.layer;
                    p.net = btr.net;
                    p.trace = bt;
                    p.owner = 0;
                    p.obj = trace_key(bt);
                    p.rule = btr.rule; // ADAPTED: the port's per-copper clearance-table row.
                    let mut old_p: Vec<Prim> = Vec::new();
                    let mut new_p: Vec<Prim> = Vec::new();
                    let mut lo = 1 << 30;
                    let mut hi = -1;
                    for i in 0..=m {
                        let s0 = std_min(b_idx(i), b_idx(i + 1));
                        let mut q = p;
                        q.a = btr.points[b_idx(i) as usize].pos;
                        q.b = btr.points[b_idx(i + 1) as usize].pos;
                        lo = std_min(lo, s0);
                        hi = std_max(hi, s0);
                        old_p.push(q);
                    }
                    let mut np = p;
                    np.a = z.q;
                    np.b = btr.points[b_idx(m + 1) as usize].pos;
                    np.ex_lo = lo - 1;
                    np.ex_hi = hi + 1;
                    for q in &mut old_p {
                        q.ex_lo = lo - 1;
                        q.ex_hi = hi + 1;
                    }
                    new_p.push(np);
                    if touched.is_empty() {
                        touched = vec![0; self.e.c.n_segs as usize];
                    }
                    if !self.e.guard_replace(&*b, &old_p, &new_p, 1, &touched, bt, lo - 1, hi + 1) {
                        continue;
                    }
                    for s in lo..=hi {
                        touched[self.e.c.seg_base[bt as usize] as usize + s as usize] = 1;
                    }
                    touched[self.e.c.seg_base[h as usize] as usize + std_min(ia, ib) as usize] = 1;
                    // The C++ PSR_ZIP_DEBUG environment read and diagnostic print are omitted.
                    zips.push(z);
                    used[bt as usize] = 1;
                    used[h as usize] = 1;
                    break;
                }
            }
        }
        if zips.is_empty() {
            return false;
        }
        for z in &zips {
            let target = z.at;
            if z.insert {
                let ht = &mut b.traces[z.h as usize];
                let mut map = vec![0i32; ht.points.len()];
                for i in 0..map.len() as i32 {
                    map[i as usize] = if i < z.at { i } else { i + 1 };
                }
                let mut np = TracePoint::default();
                np.pos = z.q;
                np.width = ht.width;
                ht.points.insert(z.at as usize, np);
                let id = ht.id;
                ops::remap_branches(b, id, &map);
            }
            // Drop the branch nodes on the trunk; its end becomes the new junction.
            let bt = &b.traces[z.bt as usize];
            let bn = bt.points.len() as i32;
            let mut map = vec![-1i32; bn as usize];
            let mut out: Vec<TracePoint> = Vec::new();
            let mut end_pt = bt.points[(if z.e == 0 { 0 } else { bn - 1 }) as usize].clone();
            end_pt.pos = b.traces[z.h as usize].points[target as usize].pos;
            end_pt.branch_trace = b.traces[z.h as usize].id;
            end_pt.branch_point = target;
            if z.e == 0 {
                out.push(end_pt);
                map[0] = 0;
                for i in z.keep_from..bn {
                    map[i as usize] = out.len() as i32;
                    out.push(bt.points[i as usize].clone());
                }
            } else {
                for i in 0..bn - z.keep_from {
                    map[i as usize] = out.len() as i32;
                    out.push(bt.points[i as usize].clone());
                }
                map[(bn - 1) as usize] = out.len() as i32;
                out.push(end_pt);
            }
            b.traces[z.bt as usize].points = out;
            let id = b.traces[z.bt as usize].id;
            ops::remap_branches(b, id, &map);
            self.reset_trace(&*b, z.h);
            self.reset_trace(&*b, z.bt);
            self.mark_dirty(z.q, self.sub_index);
            self.flag_net(b.traces[z.h as usize].net);
            self.flag_resample(z.h);
            self.flag_resample(z.bt);
            for i in 0..b.traces[z.h as usize].points.len() {
                self.touch(z.h, i as i32, self.sub_index);
            }
            for i in 0..b.traces[z.bt as usize].points.len() {
                self.touch(z.bt, i as i32, self.sub_index);
            }
        }
        self.topo_dirty = true;
        true
    }

    /// `resample`: guarded merges followed by geometry-preserving splits.
    pub fn resample(&mut self, b: &mut OBoard, step_no: i32) -> bool {
        if self.zip_junctions(b) {
            return true; // topology changed: resampling continues next step
        }
        self.flag_traces_near_dirty();
        let sp = std_max(0.1f32, self.s.node_spacing);
        let mut touched: Vec<u8> = Vec::new();
        let mut changed = false;
        struct Pending {
            t: i32,
            pts: Vec<TracePoint>,
            map: Vec<i32>,
        }
        let mut pending: Vec<Pending> = Vec::new();
        for t in 0..b.traces.len() {
            if self.c.resample_flag[t] == 0 {
                continue;
            }
            let tr = &b.traces[t];
            let n = tr.points.len() as i32;
            if n < 2 {
                self.c.resample_flag[t] = 0;
                continue;
            }
            // Exponential backoff prevents a persistent split / merge limit cycle.
            if t < self.c.frozen.len() && self.c.frozen[t] != 0 {
                continue;
            }
            // A tiny segment folded back against its neighbour bypasses the backoff:
            // spring energy has no bending term to straighten the hook.
            let mut hook = false;
            for k in 1..n - 1 {
                if hook {
                    break;
                }
                if self.node_anchored(&*b, t as i32, k) {
                    continue;
                }
                let u = tr.points[(k - 1) as usize].pos - tr.points[k as usize].pos;
                let w = tr.points[(k + 1) as usize].pos - tr.points[k as usize].pos;
                let lu = u.length();
                let lw = w.length();
                if std_min(lu, lw) < 0.1f32 && lu > 1e-6f32 && lw > 1e-6f32 && u.dot(w) > 0.5f32 * lu * lw {
                    hook = true;
                }
            }
            if step_no < self.c.resample_next[t] && !hook {
                continue; // stays flagged until due
            }
            self.c.resample_flag[t] = 0;
            let mut rejected = false;
            if touched.is_empty() {
                touched = vec![0; self.e.c.n_segs as usize];
            }
            // The hysteresis band widens with each resample, making events rarer too.
            let widen = 1.0f32 + 0.25f32 * std_min(self.c.resample_level[t], 8) as f32;
            // Diff-pair members use denser nodes so coupling follows the partner closely.
            let sp_t = if self.e.partner_of(tr.net) >= 0 { std_min(sp, K_PAIR_NODE_SPACING) } else { sp };
            // ADAPTED: pour hook 7. A pour path's nodes carry only clearance: wider spacing.
            let sp_t = if b.pour_rules.binary_search(&tr.rule).is_ok() { K_POUR_NODE_SPACING_SCALE * sp_t } else { sp_t };
            let split_len = sp_t * widen;
            let merge_short = 0.45f32 * sp_t / widen;
            let mut drop = vec![0u8; n as usize];
            let mut last_merge = -100;
            for k in 1..n - 1 {
                if self.node_anchored(&*b, t as i32, k) || k - last_merge < 4 {
                    continue;
                }
                if ops::straight_segment(&tr.points, k - 1) || ops::straight_segment(&tr.points, k) {
                    continue; // exit stub
                }
                let a = tr.points[(k - 1) as usize].pos;
                let b_pos = tr.points[k as usize].pos;
                let c = tr.points[(k + 1) as usize].pos;
                let lac = a.distance_to(c);
                if lac > 0.9f32 * sp_t {
                    continue;
                }
                let lab = a.distance_to(b_pos);
                let lbc = b_pos.distance_to(c);
                if !(lab < merge_short || lbc < merge_short) {
                    continue;
                }
                // P4b / P7: removing the first free node after a pad-exit stub must not
                // turn its next neighbour back behind the stub end.
                {
                    let mut folds_back = false;
                    for side in [-1, 1] {
                        let e = k + side;
                        let s0 = if side < 0 { k - 2 } else { k + 1 };
                        let far = k - side;
                        if s0 < 0 || s0 + 1 >= n || !ops::straight_segment(&tr.points, s0) {
                            continue;
                        }
                        let su = tr.points[e as usize].pos - tr.points[(e + side) as usize].pos;
                        let se = tr.points[e as usize].pos;
                        let w_new = tr.points[far as usize].pos - se;
                        let w_old = tr.points[k as usize].pos - se;
                        let l_n = w_new.length();
                        let l_o = w_old.length();
                        let ls = su.length();
                        if l_n < 1e-6f32 || ls < 1e-6f32 {
                            continue;
                        }
                        let c_new = su.dot(w_new) / (ls * l_n);
                        let c_old = if l_o > 1e-6f32 { su.dot(w_old) / (ls * l_o) } else { 1.0f32 };
                        if c_new < 0.0f32 && c_new < c_old {
                            folds_back = true;
                        }
                    }
                    if folds_back {
                        rejected = true;
                        continue;
                    }
                }
                let mut old_p = vec![Prim::default(); 2];
                let mut new_p = vec![Prim::default(); 1];
                let mut p = Prim::default();
                p.h = tr.width as f64 * 0.5f64;
                p.layer = tr.layer;
                p.net = tr.net;
                p.trace = t as i32;
                p.owner = 0;
                p.obj = trace_key(t as i32);
                p.rule = tr.rule; // ADAPTED: the port's per-copper clearance-table row.
                p.a = a;
                p.b = b_pos;
                p.ex_lo = k - 2;
                p.ex_hi = k;
                old_p[0] = p;
                p.a = c;
                p.b = b_pos;
                p.ex_lo = k - 1;
                p.ex_hi = k + 1;
                old_p[1] = p;
                p.a = a;
                p.b = c;
                p.ex_lo = k - 2;
                p.ex_hi = k + 1;
                new_p[0] = p;
                if !self.e.guard_replace(&*b, &old_p, &new_p, 1, &touched, t as i32, k - 2, k + 1) {
                    rejected = true;
                    continue;
                }
                drop[k as usize] = 1;
                last_merge = k;
                let base = self.e.c.seg_base[t];
                touched[base as usize + k as usize - 1] = 1;
                touched[base as usize + k as usize] = 1;
            }
            // Rebuild with merges, then splits (splits do not change geometry).
            let mut out: Vec<TracePoint> = Vec::new();
            let mut map = vec![-1i32; n as usize];
            out.reserve((n + 8) as usize);
            let pair_pinned = self.s.diff_pair_pin_short > 0.0f32 && self.e.partner_of(tr.net) >= 0;
            let mut prev_kept = -1;
            for k in 0..n {
                if drop[k as usize] != 0 {
                    continue;
                }
                let stub = prev_kept == k - 1 && ops::straight_segment(&tr.points, k - 1);
                prev_kept = k;
                if !out.is_empty() && !stub {
                    let pa = out.last().unwrap().pos;
                    let pb = tr.points[k as usize].pos;
                    let len = pa.distance_to(pb);
                    if len > split_len && len.is_finite() {
                        let pieces = std_min(256, (len / sp_t).ceil() as i32);
                        // H6d: splits inside a pinned diff-pair run stay pinned.
                        let pinned_run = pair_pinned && out.last().unwrap().is_fixed && tr.points[k as usize].is_fixed;
                        for i in 1..pieces {
                            let mut np = TracePoint::default();
                            np.pos = Vec2::lerp(pa, pb, i as f32 / pieces as f32);
                            np.width = tr.width;
                            np.is_fixed = pinned_run;
                            out.push(np);
                        }
                    }
                }
                map[k as usize] = out.len() as i32;
                out.push(tr.points[k as usize].clone());
            }
            if out.len() as i32 == n {
                // Retry rejected merges a few times as neighbours move, then wait for motion.
                if rejected && self.c.resample_tries[t] < if hook { 48 } else { 6 } {
                    self.c.resample_flag[t] = 1;
                    self.c.resample_tries[t] += 1;
                }
                continue;
            }
            changed = true;
            for k in 0..n {
                if drop[k as usize] != 0 {
                    self.mark_dirty(tr.points[k as usize].pos, self.sub_index);
                }
            }
            for k in 1..out.len().saturating_sub(1) {
                self.mark_dirty(out[k].pos, self.sub_index);
            }
            // Apply after the pass: later guards still index the current point arrays;
            // merged segments are covered by touched.
            pending.push(Pending { t: t as i32, pts: out, map });
            self.c.resample_flag[t] = 1; // more merges may follow; retried when due
            let lvl = &mut self.c.resample_level[t];
            self.c.resample_next[t] = step_no + (1 << std_min(*lvl, 7));
            *lvl = std_min(*lvl + 1, 7);
        }
        // Replace every point array before remapping: a branch resampled alongside its
        // host still carries old host indices in its new array.
        for pr in &mut pending {
            b.traces[pr.t as usize].points = std::mem::take(&mut pr.pts);
            self.reset_trace(&*b, pr.t);
            for k in 0..b.traces[pr.t as usize].points.len() {
                self.touch(pr.t, k as i32, self.sub_index);
            }
            self.flag_net(b.traces[pr.t as usize].net);
        }
        for pr in &pending {
            let id = b.traces[pr.t as usize].id;
            ops::remap_branches(b, id, &pr.map);
        }
        if changed {
            self.topo_dirty = true;
        }
        changed
    }

    /// `awakeCount`.
    pub fn awake_count(&self, b: &OBoard) -> i64 {
        let mut a = 0i64;
        for &v in &self.c.tr_awake {
            a += v as i64;
        }
        for v in 0..self.c.vquiet.len() {
            if self.c.vquiet[v] < K_QUIET && self.c.via_pinned[v] == 0 && finite_vec(b.vias[v].pos) {
                a += 1;
            }
        }
        a
    }
}

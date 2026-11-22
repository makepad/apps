// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.cpp (topology and spring weights:
// lines 2865-3008; FA25 straight-run memory 3069-3189; hopKeepRun, hopKeepFloors, hopKeepSeen, addFloors
// 3191-3280; wideFactor 3286-3292, wideTrackWeights/addSnap 3299-3412; bundlePrepare's off path
// 3009-3015; skew helpers and updatePairSkew 4192-4622).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Junction topology (branch links, host branches, junction groups), via membership and pins,
//! the spring weights of a step, FA25 straight-run memory (straight-pass relief), via-to-via
//! hop-keep length floors, wide-track spring stiffness, octilinear snap and line pull, and
//! differential-pair skew budgets, anchors and coupling-loss guards.
//!
//! The bundle-keep preparation pass is ported on its off path (the port's board has no
//! bundles).

use super::nodes::is_pinned;
use super::ops::compute_branch_targets;
use super::resample::{K_MAX_TUNABLE_SKEW, K_SKEW_CORRECTIONS};
use super::{BranchLink, HopKeepRun, Relaxation, StraightLine, StraightPull};
use crate::organic::copper::{pad_on_layer, proj_seg, seg_pad_gap};
use crate::organic::diffpairs::{coupled_length, CouplingPoly};
use crate::organic::engine::guard::LocalModel;
use crate::organic::engine::{finite_vec, PairClass, PairEval, Prim};
use crate::organic::geom::{std_clamp, std_max, std_min, Vec2, K_PI};
use crate::organic::model::{OBoard, Trace};
use crate::organic::settings::relax_immutable;
use std::collections::BTreeMap;

impl Relaxation {
    pub fn build_topology(&mut self, b: &OBoard) {
        self.c.branch_target = compute_branch_targets(b);
        self.c.host_branches.clear();
        self.c.linked_traces = vec![Vec::new(); b.traces.len()];
        self.c.branch_links.clear();
        {
            let mut id_to_idx: BTreeMap<i32, usize> = BTreeMap::new();
            for (t, tr) in b.traces.iter().enumerate() {
                id_to_idx.insert(tr.id, t);
            }
            for t in 0..b.traces.len() {
                let pts = &b.traces[t].points;
                let n = pts.len() as i32;
                for k in 0..n {
                    let p = &pts[k as usize];
                    if p.branch_trace < 0 {
                        continue;
                    }
                    let Some(&h) = id_to_idx.get(&p.branch_trace) else { continue };
                    if p.branch_point < 0 || p.branch_point >= b.traces[h].points.len() as i32 {
                        continue;
                    }
                    self.c.branch_links.push(BranchLink { t: t as i32, k, h: h as i32, hk: p.branch_point });
                    let lt = &mut self.c.linked_traces[h];
                    if !lt.contains(&(t as i32)) {
                        lt.push(t as i32);
                    }
                }
                if n < 2 {
                    continue;
                }
                for e in [0, n - 1] {
                    let p = &pts[e as usize];
                    if p.branch_trace < 0 || p.terminal_pad >= 0 || p.attached_via >= 0 {
                        continue;
                    }
                    let Some(&h) = id_to_idx.get(&p.branch_trace) else { continue };
                    if h == t {
                        continue;
                    }
                    if p.branch_point <= 0 || p.branch_point + 1 >= b.traces[h].points.len() as i32 {
                        continue;
                    }
                    if b.traces[h].layer != b.traces[t].layer {
                        continue;
                    }
                    let v = self.c.host_branches.entry(Self::host_key(h as i32, p.branch_point)).or_default();
                    if !v.contains(&(t as i32, e)) {
                        v.push((t as i32, e));
                    }
                }
            }
        }
        // junction groups (union-find over branch links; the root is the smallest index, so the
        // result does not depend on the visiting order)
        {
            let mut par: Vec<i32> = (0..b.traces.len() as i32).collect();
            fn find(par: &mut [i32], mut x: i32) -> i32 {
                while par[x as usize] != x {
                    par[x as usize] = par[par[x as usize] as usize];
                    x = par[x as usize];
                }
                x
            }
            let mut in_group = vec![0u8; b.traces.len()];
            for (&key, br) in &self.c.host_branches {
                let h = (key >> 32) as i32;
                in_group[h as usize] = 1;
                for &(bt, _e) in br {
                    in_group[bt as usize] = 1;
                    let ra = find(&mut par, h);
                    let rb = find(&mut par, bt);
                    if ra != rb {
                        par[std::cmp::max(ra, rb) as usize] = std::cmp::min(ra, rb);
                    }
                }
            }
            self.e.c.jgroup = vec![-1; b.traces.len()];
            for t in 0..b.traces.len() {
                if in_group[t] != 0 {
                    self.e.c.jgroup[t] = find(&mut par, t as i32);
                }
            }
        }
        self.c.via_members = vec![Vec::new(); b.vias.len()];
        self.c.via_pinned = vec![0; b.vias.len()];
        for (v, via) in b.vias.iter().enumerate() {
            if via.is_fixed || via.locked || !self.s.enable_dynamic_vias {
                self.c.via_pinned[v] = 1; // locked: static (K4)
            }
        }
        for t in 0..b.traces.len() {
            let pts = &b.traces[t].points;
            for k in 0..pts.len() {
                let v = pts[k].attached_via;
                if v < 0 || v >= b.vias.len() as i32 {
                    continue;
                }
                if pts[k].is_fixed
                    || pts[k].terminal_pad >= 0
                    || pts[k].branch_trace >= 0
                    || (k < self.c.branch_target[t].len() && self.c.branch_target[t][k] != 0)
                {
                    self.c.via_pinned[v as usize] = 1;
                }
                self.c.via_members[v as usize].push((t as i32, k as i32));
            }
        }
        // the two vias of a diff-pair layer change move together: they stay where the coupled
        // router placed them
        if self.s.enable_diff_pair_coupling {
            let mut pv: Vec<usize> = Vec::new();
            for (v, via) in b.vias.iter().enumerate() {
                if self.e.partner_of(via.net) >= 0 {
                    pv.push(v);
                }
            }
            for i in 0..pv.len() {
                for j in i + 1..pv.len() {
                    let va = &b.vias[pv[i]];
                    let vb = &b.vias[pv[j]];
                    if self.e.partner_of(va.net) != vb.net {
                        continue;
                    }
                    if va.pos.distance_to(vb.pos) > 1.5 {
                        continue;
                    }
                    self.c.via_pinned[pv[i]] = 1;
                    self.c.via_pinned[pv[j]] = 1;
                }
            }
        }
        // an escape via without a copper link to its tethered pad is an input defect
        let n_layers = std::cmp::max(1, b.layers.len() as i32);
        self.c.via_orphan = vec![0; b.vias.len()];
        for (v, via) in b.vias.iter().enumerate() {
            if self.c.via_pinned[v] != 0 || via.connected_pad < 0 || via.connected_pad >= b.pads.len() as i32 {
                continue;
            }
            let pad = &b.pads[via.connected_pad as usize];
            if pad.net != via.net {
                continue;
            }
            let mut linked = seg_pad_gap(via.pos, via.pos, via.diameter as f64 * 0.5, pad) <= 1e-3;
            for &(t, _k) in &self.c.via_members[v] {
                if pad_on_layer(pad, b.traces[t as usize].layer, n_layers) {
                    linked = true;
                }
            }
            if !linked {
                self.c.via_orphan[v] = 1;
            }
        }
    }

    /// Spring weights: zero-rest-length springs of weight 1, except junction legs, which get
    /// c0 / l0 (a constant tension, so a junction settles at its Steiner point).
    pub fn spring_weights(&mut self, b: &OBoard) {
        self.c.seg_w.resize(b.traces.len(), Vec::new());
        for (t, tr) in b.traces.iter().enumerate() {
            let n = if tr.points.len() > 1 { tr.points.len() - 1 } else { 0 };
            self.c.seg_w[t] = vec![1.0f32; n];
        }
        let c0 = 0.75f32 * std_max(0.1f32, self.s.node_spacing);
        let seg_w = &mut self.c.seg_w;
        let mut leg_w = |t: i32, s: i32| {
            let pts = &b.traces[t as usize].points;
            let l = pts[s as usize].pos.distance_to(pts[s as usize + 1].pos);
            if !l.is_finite() {
                return;
            }
            seg_w[t as usize][s as usize] = std_clamp(c0 / std_max(l, 0.05f32), 0.25f32, 8.0f32);
        };
        for (&key, br) in &self.c.host_branches {
            let h = (key >> 32) as i32;
            let k = (key & 0xffff_ffff) as i32;
            leg_w(h, k - 1);
            leg_w(h, k);
            for &(bt, e) in br {
                leg_w(bt, if e == 0 { 0 } else { b.traces[bt as usize].points.len() as i32 - 2 });
            }
        }
        self.wide_track_weights(b);
        self.hop_keep_floors(b);
        self.straight_prepare(b);
        self.bundle_prepare(b);
    }

    /// `wideFactor`: called only for `wide_trace` traces.
    pub fn wide_factor(&self, b: &OBoard, tr: &Trace) -> f32 {
        if self.s.wide_track_straightening <= 0.0f32 {
            return 1.0f32;
        }
        let w0 = if b.rules.default_trace_width > 1e-4f32 { b.rules.default_trace_width } else { 0.25f32 };
        let k = self.s.wide_track_straightening;
        std_min(1.0f32 + k * (tr.width / w0 - 1.0f32), 1.0f32 + 4.0f32 * k)
    }

    /// `wideTrackWeights`: spring stiffness, line-pull stretches and snap axes, fixed for the step.
    pub fn wide_track_weights(&mut self, b: &OBoard) {
        let stiff = self.s.wide_track_straightening > 0.0f32;
        let snap = self.s.wide_track_snap_deg > 0.0f32;
        let pull = self.s.wide_track_line_pull > 0.0f32;
        if !stiff && !snap && !pull {
            self.c.seg_snap.clear();
            self.c.node_line.clear();
            return;
        }
        self.c.seg_snap.resize(b.traces.len(), Vec::new());
        self.c.node_line.resize(b.traces.len(), Vec::new());
        let snap_tol = std_clamp(self.s.wide_track_snap_deg, 0.0f32, 22.0f32) * K_PI / 180.0f32;
        for t in 0..b.traces.len() {
            let tr = &b.traces[t];
            self.c.seg_snap[t].clear();
            self.c.node_line[t].clear();
            if !self.wide_trace(b, tr) {
                continue;
            }
            let n = tr.points.len() as i32;
            if stiff {
                let f = self.wide_factor(b, tr);
                for w in &mut self.c.seg_w[t] {
                    *w *= f;
                }
            }
            if pull && n >= 3 {
                // Anchors (ends, pinned nodes, junction hosts) plus Douglas-Peucker vertices
                // of the stretches between them: deviation > wide_track_deviation is a real bend.
                let mut vtx = vec![0u8; n as usize];
                vtx[n as usize - 1] = 1;
                vtx[0] = 1;
                for k in 1..n - 1 {
                    if self.node_anchored(b, t as i32, k) || !finite_vec(tr.points[k as usize].pos) {
                        vtx[k as usize] = 1;
                    }
                }
                let tol = std_max(0.01f32, self.s.wide_track_deviation);
                let seg_dist = |p: Vec2, a: Vec2, b: Vec2| {
                    let (_tt, qx, qy) = proj_seg(p.x as f64, p.y as f64, a.x as f64, a.y as f64, b.x as f64, b.y as f64);
                    ((p.x as f64 - qx) * (p.x as f64 - qx) + (p.y as f64 - qy) * (p.y as f64 - qy)).sqrt() as f32
                };
                let mut stack: Vec<(i32, i32)> = Vec::new();
                let mut a = 0;
                for b in 1..n {
                    if vtx[b as usize] != 0 {
                        if b - a > 1 {
                            stack.push((a, b));
                        }
                        a = b;
                    }
                }
                while let Some((a, b)) = stack.pop() {
                    let a_pos = tr.points[a as usize].pos;
                    let bp = tr.points[b as usize].pos;
                    let mut worst = -1.0f32;
                    let mut wk = -1;
                    for k in a + 1..b {
                        let d = seg_dist(tr.points[k as usize].pos, a_pos, bp);
                        if d > worst {
                            worst = d;
                            wk = k;
                        }
                    }
                    if wk >= 0 && worst > tol {
                        vtx[wk as usize] = 1;
                        if wk - a > 1 {
                            stack.push((a, wk));
                        }
                        if b - wk > 1 {
                            stack.push((wk, b));
                        }
                    }
                }
                self.c.node_line[t].resize(n as usize, (Vec2::new(0.0f32, 0.0f32), Vec2::new(0.0f32, 0.0f32)));
                let mut a = 0;
                for b in 1..n {
                    if vtx[b as usize] != 0 {
                        let a_pos = tr.points[a as usize].pos;
                        let d = tr.points[b as usize].pos - a_pos;
                        let l = d.length();
                        if l > 1e-4f32 {
                            for k in a + 1..b {
                                self.c.node_line[t][k as usize] = (a_pos, Vec2::new(-d.y / l, d.x / l));
                            }
                        }
                        a = b;
                    }
                }
            }
            if !snap || n < 2 {
                continue;
            }
            self.c.seg_snap[t].resize(n as usize - 1, Vec2::new(0.0f32, 0.0f32));
            for s in 0..n - 1 {
                let a = tr.points[std_max(0, s - 1) as usize].pos;
                let b = tr.points[std_min(n - 1, s + 2) as usize].pos;
                let d = b - a;
                if !finite_vec(d) || d.length() < 1e-3f32 {
                    continue;
                }
                let ang = d.y.atan2(d.x);
                let step = K_PI / 4.0f32;
                let axis = (ang / step).round() * step;
                if (ang - axis).abs() > snap_tol {
                    continue;
                }
                self.c.seg_snap[t][s as usize] = Vec2::new(-axis.sin(), axis.cos());
            }
        }
    }

    /// `addSnap`: ks/2 ((p_k+1 - p_k) . n)^2 for segments k-1 and k.
    /// ADAPTED: OrganicRelaxer.cpp:3394 reads B; pass the live board explicitly, as in the sweeps.
    pub fn add_snap(&self, b: &OBoard, m: &mut LocalModel, t: i32, k: i32, x: Vec2, ks: f64) {
        if t >= self.c.seg_snap.len() as i32 || self.c.seg_snap[t as usize].is_empty() {
            return;
        }
        let sn = &self.c.seg_snap[t as usize];
        let pts = &b.traces[t as usize].points;
        let w = 1.0f64; // the snap stiffness equals the plain spring stiffness
        if k - 1 >= 0 && k - 1 < sn.len() as i32 && (sn[k as usize - 1].x != 0.0f32 || sn[k as usize - 1].y != 0.0f32) {
            let nn = sn[k as usize - 1];
            let e = (x.x - pts[k as usize - 1].pos.x) as f64 * nn.x as f64
                + (x.y - pts[k as usize - 1].pos.y) as f64 * nn.y as f64;
            m.gx += ks * w * e * nn.x as f64;
            m.gy += ks * w * e * nn.y as f64;
            m.add_h(nn.x as f64, nn.y as f64, ks * w);
        }
        if k < sn.len() as i32 && k + 1 < pts.len() as i32 && (sn[k as usize].x != 0.0f32 || sn[k as usize].y != 0.0f32) {
            let nn = sn[k as usize];
            let e = (pts[k as usize + 1].pos.x - x.x) as f64 * nn.x as f64
                + (pts[k as usize + 1].pos.y - x.y) as f64 * nn.y as f64;
            m.gx -= ks * w * e * nn.x as f64;
            m.gy -= ks * w * e * nn.y as f64;
            m.add_h(nn.x as f64, nn.y as f64, ks * w);
        }
    }

    /// `hopKeepRun`: a mutable non-pair trace between two different vias of its own net,
    /// with no interior junction or via. `poly` is its centre polyline length; `ends` is
    /// the mean via radius plus half width (the hop metric excludes the via disc parts).
    pub fn hop_keep_run(&self, b: &OBoard, t: i32, va: &mut i32, vb: &mut i32, poly: &mut f64, ends: &mut f64) -> bool {
        let tr = &b.traces[t as usize];
        let n = tr.points.len() as i32;
        if n < 2 || relax_immutable(tr, &self.s) || self.e.partner_of(tr.net) >= 0 {
            return false;
        }
        *va = tr.points[0].attached_via;
        *vb = tr.points[n as usize - 1].attached_via;
        let v = b.vias.len() as i32;
        if *va < 0 || *vb < 0 || *va >= v || *vb >= v || *va == *vb {
            return false;
        }
        if b.vias[*va as usize].net != tr.net || b.vias[*vb as usize].net != tr.net {
            return false;
        }
        if tr.points[0].terminal_pad >= 0 || tr.points[n as usize - 1].terminal_pad >= 0 {
            return false;
        }
        *poly = 0.0;
        for k in 0..n {
            let p = &tr.points[k as usize];
            if !finite_vec(p.pos) {
                return false;
            }
            if k > 0
                && k + 1 < n
                && ((k < self.c.branch_target[t as usize].len() as i32 && self.c.branch_target[t as usize][k as usize] != 0)
                    || p.branch_trace >= 0
                    || p.attached_via >= 0)
            {
                return false; // a junction / a via on the way: no single run between the two vias
            }
            if k + 1 < n {
                *poly += p.pos.distance_to(tr.points[k as usize + 1].pos) as f64;
            }
        }
        *ends = 0.25 * (b.vias[*va as usize].diameter + b.vias[*vb as usize].diameter) as f64 + 0.5 * tr.width as f64;
        true
    }

    /// `hopKeepFloors`: distribute the run's remembered length floor across its segments.
    pub fn hop_keep_floors(&mut self, b: &OBoard) {
        if !(self.s.hop_keep_dist > 0.0f32) || !(self.s.hop_keep_weight > 0.0f32) {
            self.c.seg_floor.clear();
            self.c.hop_keep.clear();
            return;
        }
        self.c.seg_floor = vec![Vec::new(); b.traces.len()];
        let lim = std_max(0.0f32, self.s.hop_limit) as f64;
        let keep = std_max(self.s.hop_keep_dist, 0.0f32) as f64;
        for t in 0..b.traces.len() {
            let (mut va, mut vb) = (-1, -1);
            let (mut poly, mut ends) = (0.0, 0.0);
            if !self.hop_keep_run(b, t as i32, &mut va, &mut vb, &mut poly, &mut ends) {
                continue;
            }
            let tr = &b.traces[t];
            let h = self.c.hop_keep.entry(tr.id).or_default();
            if h.va != va || h.vb != vb || (h.seen as f64 - poly).abs() > 1e-3 {
                h.va = va;
                h.vb = vb;
                h.l0 = poly as f32; // first seen / edited from outside: (re)start
            }
            h.seen = poly as f32;
            if (h.l0 as f64) < lim || !(poly > 1e-6) {
                continue; // already a hop: not kept
            }
            // The first length, at least the limit + one disc part (the spline cuts
            // corners), at most hop_keep_dist.
            let p = std_min(keep, std_max(h.l0 as f64, lim + ends));
            if poly >= 2.0 * p {
                continue;
            }
            let n = tr.points.len() as i32;
            let f = &mut self.c.seg_floor[t];
            *f = vec![0.0f32; n as usize - 1];
            for k in 0..n - 1 {
                f[k as usize] = (tr.points[k as usize].pos.distance_to(tr.points[k as usize + 1].pos) as f64 * p / poly) as f32;
            }
        }
    }

    /// `hopKeepSeen`: remember the lengths at the end of a step, dropping vanished runs.
    pub fn hop_keep_seen(&mut self, b: &OBoard) {
        if self.c.hop_keep.is_empty() {
            return;
        }
        let mut next: BTreeMap<i32, HopKeepRun> = BTreeMap::new();
        for t in 0..b.traces.len() {
            let Some(h) = self.c.hop_keep.get(&b.traces[t].id) else { continue };
            let (mut va, mut vb) = (-1, -1);
            let (mut poly, mut ends) = (0.0, 0.0);
            if !self.hop_keep_run(b, t as i32, &mut va, &mut vb, &mut poly, &mut ends) {
                continue;
            }
            let mut h = *h;
            h.seen = poly as f32;
            next.insert(b.traces[t].id, h);
        }
        std::mem::swap(&mut self.c.hop_keep, &mut next);
    }

    /// `addFloors`: add the floors of trace t's segments meeting at moving node k.
    pub fn add_floors(&self, b: &OBoard, m: &mut LocalModel, t: i32, k: i32, ks: f64) {
        if t >= self.c.seg_floor.len() as i32 || self.c.seg_floor[t as usize].is_empty() {
            return;
        }
        let f = &self.c.seg_floor[t as usize];
        let pts = &b.traces[t as usize].points;
        let kh = ks * self.s.hop_keep_weight as f64;
        if k >= 1 && k as usize - 1 < f.len() {
            m.add_floor(kh, pts[k as usize - 1].pos, f[k as usize - 1] as f64);
        }
        if (k as usize) < f.len() && k as usize + 1 < pts.len() {
            m.add_floor(kh, pts[k as usize + 1].pos, f[k as usize] as f64);
        }
    }

    /// `straightEligible` (OrganicRelaxer.cpp:3070-3073).
    fn straight_eligible(&self, tr: &Trace) -> bool {
        tr.points.len() >= 2 && !relax_immutable(tr, &self.s) && !(self.e.partner_of(tr.net) >= 0)
    }

    /// `polyLength` (OrganicRelaxer.cpp:3074-3079): f32 distances accumulated in f64.
    fn poly_length(tr: &Trace) -> f32 {
        let mut l = 0.0f64;
        for k in 0..tr.points.len().saturating_sub(1) {
            l += tr.points[k].pos.distance_to(tr.points[k + 1].pos) as f64;
        }
        l as f32
    }

    /// `straightRecord` (OrganicRelaxer.cpp:3080-3114): maximal runs within 0.02 mm of their chord.
    fn straight_record(&self, tr: &Trace, lines: &mut Vec<StraightLine>) {
        lines.clear();
        let n = tr.points.len() as i32;
        let tol = 0.02f32;
        let min_len = std_max(0.5f32, self.s.straight_min_len);
        let p = |k: i32| tr.points[k as usize].pos;
        let mut i = 0;
        while i + 1 < n {
            let mut best = i + 1;
            for j in i + 2..n {
                let d = p(j) - p(i);
                let l = d.length();
                if !(l > 1e-4f32) {
                    break;
                }
                let u = d / l;
                let nn = Vec2::new(-u.y, u.x);
                let mut ok = true;
                let mut prev = 0.0f32;
                let mut m = i + 1;
                while m < j && ok {
                    let q = p(m) - p(i);
                    let s = q.x * u.x + q.y * u.y;
                    ok = (q.x * nn.x + q.y * nn.y).abs() <= tol && s >= prev - 1e-4f32 && s <= l + 1e-4f32;
                    prev = s;
                    m += 1;
                }
                if !ok {
                    break;
                }
                best = j;
            }
            let d = p(best) - p(i);
            let l = d.length();
            if finite_vec(d) && l >= min_len {
                lines.push(StraightLine { a: p(i), u: d / l, l });
            }
            i = best;
        }
    }

    /// `straightPrepare` (OrganicRelaxer.cpp:3115-3172).
    pub fn straight_prepare(&mut self, b: &OBoard) {
        self.e.seg_pass.clear();
        self.e.pass_relief = 0.0;
        if !(self.s.straight_pass_relief > 0.0f32) {
            self.c.straight_mem.clear();
            self.c.straight_pull.clear();
            self.c.seg_pass.clear();
            return;
        }
        self.c.straight_pull = vec![Vec::new(); b.traces.len()];
        self.c.seg_pass = vec![Vec::new(); b.traces.len()];
        let taper = std_max(0.05f32, self.s.straight_taper);
        for (t, tr) in b.traces.iter().enumerate() {
            if !self.straight_eligible(tr) {
                continue;
            }
            let n = tr.points.len() as i32;
            let len = Self::poly_length(tr);
            // ADAPTED: temporarily own mem to call straight_record without a mutable cache borrow.
            let mut mem = std::mem::take(self.c.straight_mem.entry(tr.id).or_default());
            let e0 = tr.points[0].pos;
            let e1 = tr.points[n as usize - 1].pos;
            if mem.seen < 0.0f32
                || (mem.seen - len).abs() > 1e-3f32
                || mem.e0.distance_to(e0) > 1e-3f32
                || mem.e1.distance_to(e1) > 1e-3f32
            {
                self.straight_record(tr, &mut mem.lines); // first seen / edited from outside
            }
            mem.seen = len;
            mem.e0 = e0;
            mem.e1 = e1;
            self.c.straight_mem.insert(tr.id, mem);
            let mem = &self.c.straight_mem[&tr.id];
            if mem.lines.is_empty() || n < 3 {
                continue;
            }
            // ADAPTED: keep this row local while node_anchored reads the cache.
            let mut sp = vec![StraightPull::default(); n as usize];
            let mut any = false;
            for k in 1..n - 1 {
                if self.node_anchored(b, t as i32, k) {
                    continue;
                }
                let p = tr.points[k as usize].pos;
                for ln in &mem.lines {
                    let q = p - ln.a;
                    let nn = Vec2::new(-ln.u.y, ln.u.x);
                    let s = q.x * ln.u.x + q.y * ln.u.y;
                    let l = q.x * nn.x + q.y * nn.y;
                    if s < 0.0f32 || s > ln.l || l.abs() > 0.5f32 {
                        continue;
                    }
                    let d = std_clamp(std_min(s, ln.l - s) / taper, 0.0f32, 1.0f32);
                    let w = d * d * (3.0f32 - 2.0f32 * d);
                    if w > sp[k as usize].w {
                        sp[k as usize] = StraightPull { a: ln.a, n: nn, w };
                        any = true;
                    }
                }
            }
            if !any {
                sp.clear();
                self.c.straight_pull[t] = sp;
                continue;
            }
            let sg = &mut self.c.seg_pass[t];
            *sg = vec![0.0f32; n as usize - 1];
            for k in 0..n - 1 {
                sg[k as usize] = std_min(sp[k as usize].w, sp[k as usize + 1].w);
            }
            self.c.straight_pull[t] = sp;
        }
        if self.s.straight_pass_relief > 0.0f32 {
            // ADAPTED: Engine owns its fixed-per-step copy instead of borrowing segPass.
            self.e.seg_pass.clone_from(&self.c.seg_pass);
            self.e.pass_relief = std_clamp(self.s.straight_pass_relief as f64, 0.0, 1.0);
        }
    }

    /// `bundlePrepare`, off path (bundleKeep: L5; the port's board has no bundles).
    pub fn bundle_prepare(&mut self, _b: &OBoard) {
        self.c.bundle_pull.clear();
    }

    /// `straightSeen` (OrganicRelaxer.cpp:3173-3189): the step's output is not an external edit.
    pub fn straight_seen(&mut self, b: &OBoard) {
        if self.c.straight_mem.is_empty() {
            return;
        }
        // ADAPTED: unordered_map -> BTreeMap (key order if iterated); this loop keeps board order.
        let mut next = BTreeMap::new();
        for tr in &b.traces {
            if tr.points.len() < 2 {
                continue;
            }
            let Some(mut m) = self.c.straight_mem.remove(&tr.id) else { continue };
            m.seen = Self::poly_length(tr);
            m.e0 = tr.points.first().unwrap().pos;
            m.e1 = tr.points.last().unwrap().pos;
            next.entry(tr.id).or_insert(m);
        }
        std::mem::swap(&mut self.c.straight_mem, &mut next);
    }

    /// `crinkleAnchors` (OrganicRelaxer.cpp:4192-4252): anchors around bends of the
    /// uncrinkled longer member, nearest first; H6b drops overlapping anchor windows.
    fn crinkle_anchors(tr: &Trace, dedupe: bool) -> Vec<Vec2> {
        let mut out = Vec::new();
        let n = tr.points.len();
        if n < 3 { return out; }
        let mut s = vec![0.0f32; n];
        for k in 1..n { s[k] = s[k - 1] + tr.points[k].pos.distance_to(tr.points[k - 1].pos); }
        let seg_at = |sq: f32| {
            let mut k = 0;
            while k + 1 < n && s[k + 1] < sq { k += 1; }
            std_clamp(k, 0, n - 2)
        };
        let dir_at = |sq: f32| {
            let k = seg_at(sq);
            (tr.points[k + 1].pos - tr.points[k].pos).normalized()
        };
        let pos_at = |sq: f32| {
            let k = seg_at(sq);
            let l = s[k + 1] - s[k];
            if l > 1e-6f32 {
                Vec2::lerp(tr.points[k].pos, tr.points[k + 1].pos, std_clamp((sq - s[k]) / l, 0.0f32, 1.0f32))
            } else {
                tr.points[k].pos
            }
        };
        let mut bends = Vec::new();
        for k in 1..n - 1 {
            if s[k] < 3.0f32 || s[n - 1] - s[k] < 3.0f32 { continue; }
            let a = dir_at(std_max(0.0f32, s[k] - 1.0f32));
            let z = dir_at(std_min(s[n - 1], s[k] + 1.0f32));
            if a.dot(z) >= 0.94f32 { continue; }
            if bends.last().is_some_and(|last| s[k] - *last < 1.5f32) { continue; }
            bends.push(s[k]);
        }
        let mut used: Vec<f32> = Vec::new();
        for j in 1..=6 {
            for &sb in &bends {
                for sg in [-1, 1] {
                    let sq = sb + sg as f32 * 1.6f32 * j as f32;
                    if sq < 2.0f32 || sq > s[n - 1] - 2.0f32 { continue; }
                    if dedupe {
                        let mut near = false;
                        for &u in &used { near = near || (u - sq).abs() < 1.2f32; }
                        if near { continue; }
                        used.push(sq);
                    }
                    out.push(pos_at(sq));
                }
            }
        }
        out
    }

    /// `anchorCoupled` (OrganicRelaxer.cpp:4253-4257): the metrics band plus slack.
    fn anchor_coupled(bd: f64, gap: f64, h: f64, hl: f64) -> bool {
        let eg = bd - h - hl;
        eg >= 0.75 * gap && eg <= 1.25 * gap
    }

    /// `crinkleTips` (OrganicRelaxer.cpp:4264-4310): full tips plus a fractional last tip,
    /// retaining their positions through resampling; pinned or adjacent tips are skipped.
    fn crinkle_tips(tr: &Trace, anchors: &[Vec2], c: f32, max_amp: f32, mem: &mut Vec<Vec2>) -> Vec<f32> {
        let n = tr.points.len();
        mem.resize(anchors.len(), Vec2::new(1e30f32, 1e30f32));
        if n < 3 || anchors.is_empty() || c <= 0.0f32 { return Vec::new(); }
        let mut a = vec![0.0f32; n];
        let mut used: Vec<i32> = Vec::new();
        let full = c.floor() as i32;
        let mut any = false;
        for i in 0..anchors.len() {
            if i as i32 > full { break; }
            let amp = if (i as i32) < full { max_amp } else { (c - full as f32) * max_amp };
            if amp <= 0.0f32 { break; }
            let mut best = -1;
            let mut bd = 1.0f32;
            if mem[i].x < 1e29f32 {
                let mut md = 0.6f32;
                for k in 1..n - 1 {
                    let d = tr.points[k].pos.distance_to(mem[i]);
                    if d < md && !is_pinned(&tr.points[k]) { md = d; best = k as i32; }
                }
                if best >= 0 { bd = -1.0f32; }
            }
            for k in 1..n - 1 {
                if bd < 0.0f32 { break; }
                let dir = (tr.points[k + 1].pos - tr.points[k - 1].pos).normalized();
                let w = tr.points[k].pos - anchors[i];
                let along = w.dot(dir).abs();
                let across = w.cross(dir).abs();
                if across > 1.5f32 { continue; }
                let d = along + 0.2f32 * across;
                if d < bd && !is_pinned(&tr.points[k]) { bd = d; best = k as i32; }
            }
            if best < 0 { mem[i] = Vec2::new(1e30f32, 1e30f32); continue; }
            let mut clash = false;
            for &u in &used { clash = clash || (u - best).abs() < 2; }
            if clash { continue; }
            used.push(best);
            a[best as usize] = amp;
            mem[i] = tr.points[best as usize].pos;
            any = true;
        }
        if !any { return Vec::new(); }
        a
    }

    /// `pairCoupledLength` (OrganicRelaxer.cpp:4330-4346): both exported member lengths.
    fn pair_coupled_length(b: &OBoard, p: i32, q: i32, gap: f64) -> f64 {
        let mut a = Vec::new();
        let mut z = Vec::new();
        for tr in &b.traces {
            if tr.net != p && tr.net != q { continue; }
            let cp = Self::coupling_poly(tr);
            if tr.net == p { a.push(cp); } else { z.push(cp); }
        }
        coupled_length(&a, &z, gap as f32, true)
    }

    /// `netPolys` / `traceCoupled` / `pairCoupledLength`'s identical polyline construction
    /// (OrganicRelaxer.cpp:4337-4342, 4362-4367, 4380-4385).
    /// ADAPTED: shared helper; smoothed output wins over raw nodes in each caller.
    fn coupling_poly(tr: &Trace) -> CouplingPoly {
        CouplingPoly {
            layer: tr.layer,
            width: tr.width,
            pts: if tr.smoothed.len() >= 2 { tr.smoothed.clone() } else { tr.points.iter().map(|pt| pt.pos).collect() },
        }
    }

    /// `netPolys` (OrganicRelaxer.cpp:4356-4371): exported polylines in board order.
    fn net_polys(b: &OBoard, net: i32) -> Vec<CouplingPoly> {
        let mut v = Vec::new();
        for tr in &b.traces {
            if tr.net != net { continue; }
            v.push(Self::coupling_poly(tr));
        }
        v
    }

    /// `traceCoupled` (OrganicRelaxer.cpp:4373-4389): coupled length of each member trace.
    fn trace_coupled(b: &OBoard, p: i32, q: i32, gap: f64) -> Vec<(i32, f32)> {
        let mut out = Vec::new();
        let pp = Self::net_polys(b, p);
        let qq = Self::net_polys(b, q);
        for tr in &b.traces {
            if tr.net != p && tr.net != q { continue; }
            let cp = Self::coupling_poly(tr);
            out.push((tr.id, coupled_length(&[cp], if tr.net == p { &qq } else { &pp }, gap as f32, false) as f32));
        }
        out
    }

    /// `crinkleGuard` (OrganicRelaxer.cpp:4390-4420): turn crinkles off until an edit when
    /// their pair-wide or per-segment coupling loss exceeds the allowance (at least 0.1 mm).
    fn crinkle_guard(&mut self, b: &OBoard, p: usize, q: i32, gap: f64, was_on: bool, a_s: &mut f32, a_l: &mut f32) {
        const K_CRINKLE_MIN_LOSS: f64 = 0.1;
        if self.skew_reset {
            self.c.crinkle_blocked[p] = 0;
            self.c.crinkle_cpl_ref[p] = -1.0f32;
        }
        if self.c.crinkle_blocked[p] != 0 { *a_s = 0.0f32; *a_l = 0.0f32; return; }
        let on = *a_s > 0.0f32 || *a_l > 0.0f32;
        if !was_on && !on { self.c.crinkle_cpl_ref[p] = -1.0f32; return; }
        let cl = Self::pair_coupled_length(b, p as i32, q, gap);
        if !was_on || self.c.crinkle_cpl_ref[p] < 0.0f32 {
            self.c.crinkle_cpl_ref[p] = cl as f32;
            if self.s.diff_pair_crinkle_segment_guard { self.c.crinkle_trace_ref[p] = Self::trace_coupled(b, p as i32, q, gap); }
            return;
        }
        let pts = 0.01 * std_max(0.0, self.s.diff_pair_crinkle_max_loss as f64);
        let reference = self.c.crinkle_cpl_ref[p] as f64;
        let allow = std_max(K_CRINKLE_MIN_LOSS, pts * reference);
        let mut seg_loss = false;
        if self.s.diff_pair_crinkle_segment_guard && !self.c.crinkle_trace_ref[p].is_empty() {
            for (id, c) in Self::trace_coupled(b, p as i32, q, gap) {
                for &(id0, c0) in &self.c.crinkle_trace_ref[p] {
                    if id0 == id && (c as f64) < c0 as f64 - std_max(K_CRINKLE_MIN_LOSS, pts * c0 as f64) { seg_loss = true; }
                }
            }
        }
        if cl < reference - allow || seg_loss {
            *a_s = 0.0f32;
            *a_l = 0.0f32;
            self.c.crinkle_blocked[p] = 1;
        }
    }

    /// `updatePairSkew` (OrganicRelaxer.cpp:4423-4622): bounded corrections to the shorter
    /// member's tip budget, then anchors, stable tips and wakeups in reference order.
    /// ADAPTED: local amplitude values replace aliased vector references across helper calls;
    /// they are written back before consumers run. kDebugSkew diagnostics are omitted.
    pub fn update_pair_skew(&mut self, b: &OBoard) {
        let nn = b.nets.len();
        let c = &mut self.c;
        c.crinkle_amp.resize(nn, 0.0f32);
        c.crinkle_bends.resize(nn, Vec::new());
        c.crinkle_best.resize(nn, 1e9f32);
        c.crinkle_last_len.resize(nn, -1.0f32);
        c.crinkle_wait.resize(nn, 0);
        c.tip_mem.resize(nn, Vec::new());
        c.trace_tips = vec![Vec::new(); b.traces.len()];
        c.crinkle_stall.resize(nn, 0);
        c.crinkle_cpl_ref.resize(nn, -1.0f32);
        c.crinkle_trace_ref.resize(nn, Vec::new());
        c.crinkle_blocked.resize(nn, 0);
        if !self.s.enable_diff_pair_coupling || !self.s.enable_diff_pair_skew_tuning {
            c.crinkle_amp.fill(0.0f32);
            return;
        }
        let mut len = vec![0.0f64; nn];
        let mut any_pair = false;
        for n in 0..nn { any_pair = any_pair || self.e.partner_of(n as i32) >= 0; }
        if !any_pair { return; }
        for tr in &b.traces {
            if tr.net < 0 || tr.net >= nn as i32 || self.e.partner_of(tr.net) < 0 { continue; }
            if tr.smoothed.len() >= 2 {
                for k in 0..tr.smoothed.len() - 1 { len[tr.net as usize] += tr.smoothed[k].distance_to(tr.smoothed[k + 1]) as f64; }
            } else {
                for k in 0..tr.points.len().saturating_sub(1) { len[tr.net as usize] += tr.points[k].pos.distance_to(tr.points[k + 1].pos) as f64; }
            }
        }
        for p in 0..nn {
            let q = self.e.partner_of(p as i32);
            if q < 0 || q < p as i32 { continue; }
            let q = q as usize;
            let gap = if q < self.e.pair_gap.len() && self.e.pair_gap[p] > 0.0f32 { self.e.pair_gap[p] as f64 } else { 0.2 };
            let tol = std_max(0.01, self.s.diff_pair_skew_tolerance as f64);
            let max_amp = std_max(0.0f32, self.s.diff_pair_skew_amplitude) as f64 * gap;
            let shorter = if len[p] < len[q] { p } else { q };
            let longer = if shorter == p { q } else { p };
            let skew = (len[p] - len[q]).abs();
            let mut a_s = self.c.crinkle_amp[shorter];
            let mut a_l = self.c.crinkle_amp[longer];
            let a0s = a_s;
            let a0l = a_l;
            let mut h = 0.0;
            let mut hn = 0;
            for tr in &b.traces {
                if tr.net != shorter as i32 { continue; }
                for k in 0..tr.points.len().saturating_sub(1) {
                    h += tr.points[k].pos.distance_to(tr.points[k + 1].pos) as f64;
                    hn += 1;
                }
            }
            h = if hn != 0 { std_clamp(h / hn as f64, 0.2, 1.2) } else { 0.5 };
            let gain_of = |a: f64| 2.0 * ((h * h + a * a).sqrt() - h);
            let g_full = std_max(1e-3, gain_of(max_amp));
            let budget_for = |g: f64| {
                let nf = (g / g_full).floor();
                let rem = g - nf * g_full;
                let a = std_max(0.0, (0.5 * rem + h) * (0.5 * rem + h) - h * h).sqrt();
                (nf + std_min(1.0, a / max_amp)) as f32
            };
            let gain_of_budget = |c: f32| {
                let nf = c.floor() as f64;
                nf * g_full + gain_of((c as f64 - nf) * max_amp)
            };
            let reference = &mut self.c.crinkle_best[p];
            let done = &mut self.c.crinkle_stall[p];
            let wait = &mut self.c.crinkle_wait[p];
            let moving = (len[p] - self.c.crinkle_last_len[p] as f64).abs() > 0.004
                || (len[q] - self.c.crinkle_last_len[q] as f64).abs() > 0.004;
            self.c.crinkle_last_len[p] = len[p] as f32;
            self.c.crinkle_last_len[q] = len[q] as f32;
            *wait += 1;
            if self.skew_reset { *reference = 1e9f32; *done = 0; }
            if !self.s.diff_pair_crinkle_coupled && *reference < 1e8f32 && skew > tol
                && skew > *reference as f64 + 0.5 * tol && *done < K_SKEW_CORRECTIONS && !moving && *wait >= 25
            {
                *reference = 1e9f32;
            }
            let cap = std_min(8.0f32, (0.10 * std_min(len[p], len[q]) / 0.8) as f32);
            if len[p] <= 0.0 || len[q] <= 0.0 || max_amp <= 0.0 || skew > K_MAX_TUNABLE_SKEW {
                a_s = 0.0f32;
                a_l = 0.0f32;
                *done = 0;
                *reference = 1e9f32;
            } else if *reference < 1e8f32 || (moving && *wait < 25) || *wait < 8 {
            } else if skew <= tol || *done >= K_SKEW_CORRECTIONS {
                *reference = skew as f32;
            } else {
                if a_l > 0.0f32 { a_l = budget_for(std_max(0.0, gain_of_budget(a_l) - skew)); }
                else { a_s = std_min(cap, budget_for(gain_of_budget(a_s) + skew)); }
                *done += 1;
                *wait = 0;
            }
            if !self.s.diff_pair_crinkle_coupled {
                self.crinkle_guard(b, p, q as i32, gap, a0s > 0.0f32 || a0l > 0.0f32, &mut a_s, &mut a_l);
            }
            self.c.crinkle_amp[shorter] = a_s;
            self.c.crinkle_amp[longer] = a_l;
            let mut bends = Vec::new();
            if a_l > 0.0f32 {
                bends = self.c.crinkle_bends[longer].clone();
            } else if a_s > 0.0f32 {
                for tr in &b.traces {
                    if tr.net == longer as i32 { bends.extend(Self::crinkle_anchors(tr, !self.s.diff_pair_crinkle_coupled)); }
                }
            }
            if !bends.is_empty() && a_l <= 0.0f32 {
                let mut longer_half_w = 0.0;
                for tr in &b.traces {
                    if tr.net == longer as i32 { longer_half_w = std_max(longer_half_w, 0.5 * tr.width as f64); }
                }
                let mut roomy = Vec::new();
                let mut roomy_coupled = Vec::new();
                for a in bends {
                    let mut bd = 1e9f32;
                    let mut q = Vec2::default();
                    let mut layer = 0;
                    let mut h = 0.0;
                    let mut rule = 0;
                    for tr in &b.traces {
                        if tr.net != shorter as i32 { continue; }
                        for k in 0..tr.points.len().saturating_sub(1) {
                            let u = tr.points[k].pos;
                            let v = tr.points[k + 1].pos;
                            let (_, qx, qy) = proj_seg(a.x as f64, a.y as f64, u.x as f64, u.y as f64, v.x as f64, v.y as f64);
                            let at = Vec2::new(qx as f32, qy as f32);
                            let d = at.distance_to(a);
                            if d < bd { bd = d; q = at; layer = tr.layer; h = tr.width as f64 * 0.5; rule = tr.rule; }
                        }
                    }
                    if bd > 3.0f32 || bd < 1e-4f32 { continue; }
                    if !self.s.diff_pair_crinkle_coupled {
                        let mut near_pin = false;
                        for tr in &b.traces {
                            if tr.net != shorter as i32 { continue; }
                            for k in 1..tr.points.len().saturating_sub(1) {
                                if near_pin { break; }
                                near_pin = tr.points[k].is_fixed && tr.points[k].pos.distance_to(q) < 1.0f32;
                            }
                        }
                        if near_pin { continue; }
                    }
                    let on_coupled = !self.s.diff_pair_crinkle_coupled && Self::anchor_coupled(bd as f64, gap, h, longer_half_w);
                    let nrm = (q - a) / bd;
                    // ADAPTED: the probe carries the nearest shorter trace's Fab clearance row;
                    // the reference resolves the same rule through the primitive's net.
                    let pr = Prim {
                        a: q + nrm * 0.1f32, b: q + nrm * max_amp as f32,
                        h, layer, net: shorter as i32, trace: -1, ex_lo: 0, ex_hi: -1, rule,
                        ..Prim::default()
                    };
                    let r = (max_amp + h + self.e.clr_g + self.e.max_extra + self.e.margin + 0.5) as f32;
                    self.e.gather_x0(b, q.x - r, q.y - r, q.x + r, q.y + r, layer, Some(pr.net));
                    let mut ok = true;
                    let mut ev = PairEval::default();
                    for o in &self.e.x0.obs {
                        if o.net == shorter as i32 || o.net == longer as i32 { continue; }
                        if self.e.eval_pair(b, &pr, o, &mut ev) != PairClass::Eval || !ev.repel { continue; }
                        if ev.gap < ev.thr + 0.5 * self.e.margin { ok = false; break; }
                    }
                    if ok {
                        if on_coupled { roomy_coupled.push(a); } else { roomy.push(a); }
                    }
                }
                roomy.extend(roomy_coupled);
                bends = roomy;
            }
            self.c.crinkle_bends[shorter] = bends.clone();
            self.c.crinkle_bends[longer] = bends;
            for net in [shorter, longer] {
                if self.c.crinkle_amp[net] <= 0.0f32 { self.c.tip_mem[net].clear(); continue; }
                let mut first = true;
                for (t, tr) in b.traces.iter().enumerate() {
                    if tr.net != net as i32 { continue; }
                    let mut scratch = Vec::new();
                    let mem = if first { &mut self.c.tip_mem[net] } else { &mut scratch };
                    self.c.trace_tips[t] = Self::crinkle_tips(tr, &self.c.crinkle_bends[net], self.c.crinkle_amp[net], max_amp as f32, mem);
                    first = false;
                }
            }
            for net in [shorter, longer] {
                if (if net == shorter { a0s } else { a0l }) == self.c.crinkle_amp[net] { continue; }
                for (t, tr) in b.traces.iter().enumerate() {
                    if tr.net != net as i32 { continue; }
                    for k in 1..tr.points.len().saturating_sub(1) {
                        if !self.node_pinned(b, t as i32, k as i32) { self.set_quiet(t as i32, k as i32, 0); }
                    }
                }
            }
        }
    }
}

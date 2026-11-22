// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.cpp (serial sweeps: springK and spend
// 4171-4189, crinkleQuiet 4321-4335, wideTrace 3293-3298, the sweep log and sweep parameters 4625-4787, relaxNode
// 4789-5028, nodeGate and sweepTraces 5030-5068, relaxVia 5070-5181, viaGate and sweepVias 5186-5212).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! The serial sweeps of a budgeted step: every awake interior node (then every awake via) in
//! trace order takes one guarded Newton step on the shared energy.
//!
//! The optional terms whose data is built by an unported preparation pass (wide-track snap, hop
//! keep floors, wire spreading) are on their off path here; the step refuses settings that turn
//! them on.

use super::{ForceRec, Relaxation, SweepLog, TraceSweep, K_MIN_APPLY2, K_QUIET, K_SETTLE, K_SETTLE2};
use crate::organic::copper::{trace_key, via_key};
use crate::organic::engine::guard::LocalModel;
use crate::organic::engine::{finite_vec, EngineScratch, Prim};
use crate::organic::geom::{std_clamp, std_max, Vec2, K_PI};
use crate::organic::model::{OBoard, Trace};
use crate::organic::settings::{relax_immutable, ForceTerm, FORCE_TERM_COUNT};

impl Relaxation {
    /// `springK`: zero rest length between neighbouring nodes (contraction and spacing
    /// equalisation; node spacing itself is controlled by resampling).
    pub fn spring_k(&self) -> f64 {
        std_max(0.05, (self.s.tension + self.s.straightening_force + 0.5f32 * self.s.contraction_tension) as f64)
    }

    /// `spend`: takes `units` from the substep's budget (-1: unlimited).
    pub fn spend(&mut self, units: i64) -> bool {
        if self.budget < 0 {
            return true;
        }
        if self.budget <= 0 {
            self.budget_hit = true;
            return false;
        }
        self.budget -= units;
        true
    }

    /// `wideTrace`.
    pub fn wide_trace(&self, b: &OBoard, tr: &Trace) -> bool {
        let w0 = if b.rules.default_trace_width > 1e-4f32 { b.rules.default_trace_width } else { 0.25f32 };
        tr.width >= std_max(1.0f32, self.s.wide_track_ratio) * w0 - 1e-6f32
            && !relax_immutable(tr, &self.s)
            && !(self.e.partner_of(tr.net) >= 0)
    }

    /// `crinkleQuiet`: nodes of the longer pair member near the active anchors (they must not
    /// follow the crinkles).
    pub fn crinkle_quiet(&self, tr: &Trace, anchors: &[Vec2], c: f32) -> Vec<u8> {
        let n = tr.points.len() as i32;
        if n < 3 || anchors.is_empty() || c <= 0.0f32 {
            return Vec::new();
        }
        let mut q = vec![0u8; n as usize];
        let m = std::cmp::min(anchors.len() as i32, c.ceil() as i32);
        let mut any = false;
        let r = 2.0f32;
        for k in 1..n - 1 {
            for i in 0..m {
                if tr.points[k as usize].pos.distance_squared_to(anchors[i as usize]) < r * r {
                    q[k as usize] = 1;
                    any = true;
                }
            }
        }
        if !any {
            return Vec::new();
        }
        q
    }

    // ---- the sweep log -------------------------------------------------------
    pub fn l_set_quiet(&mut self, l: &mut SweepLog, t: i32, k: i32, q: u8) {
        if !l.defer {
            self.set_quiet(t, k, q);
            return;
        }
        let c = &mut self.c.quiet[t as usize][k as usize];
        let was = *c < K_QUIET;
        let now = q < K_QUIET;
        if was != now {
            l.awake.push((t, if now { 1 } else { -1 }));
        }
        *c = q;
    }

    pub fn l_touch(&mut self, l: &mut SweepLog, t: i32, k: i32, s: i32) {
        if !l.defer {
            self.touch(t, k, s);
            return;
        }
        self.c.last_move[t as usize][k as usize] = s;
        l.touch.push((t, s));
    }

    pub fn l_mark_dirty(&mut self, l: &mut SweepLog, p: Vec2, s: i32) {
        if !l.defer {
            self.mark_dirty(p, s);
            return;
        }
        if !finite_vec(p) {
            return;
        }
        let (cx, cy) = self.cell_of(p);
        let c = cy * self.c.dnx + cx;
        if self.c.dirty_stamp[c as usize] == s {
            return;
        }
        self.c.dirty_stamp[c as usize] = s;
        l.dirty.push(c);
    }

    pub fn l_flag_resample(&mut self, l: &mut SweepLog, t: i32) {
        if !l.defer {
            self.flag_resample(t);
        } else {
            l.resample.push(t);
        }
    }

    pub fn l_flag_net(&mut self, l: &mut SweepLog, net: i32) {
        if !l.defer {
            self.flag_net(net);
        } else {
            l.nets.push(net);
        }
    }

    pub fn l_box(&mut self, l: &mut SweepLog, t: i32, p: Vec2) {
        if !l.defer {
            self.tr_box[t as usize].expand(p);
        } else {
            l.boxes.push((t, p));
        }
    }

    /// `applyLog`: applies a log (deferred part and statistics) to the shared state.
    pub fn apply_log(&mut self, l: &mut SweepLog, s: i32) {
        for &(t, d) in &l.awake {
            self.c.tr_awake[t as usize] += d;
        }
        for &(t, st) in &l.touch {
            self.c.tr_touch[t as usize] = std::cmp::max(self.c.tr_touch[t as usize], st);
        }
        for &(t, p) in &l.boxes {
            self.tr_box[t as usize].expand(p);
        }
        for &c in &l.dirty {
            self.c.dirty_list.push((c, s));
            self.c.step_dirty.push(c);
        }
        for &t in &l.resample {
            self.flag_resample(t);
        }
        for &n in &l.nets {
            self.flag_net(n);
        }
        self.sub_max_move = std_max(self.sub_max_move, l.max_move);
        self.eval_nodes += l.eval_nodes;
        self.eval_vias += l.eval_vias;
        for f in &l.forces {
            self.step_forces.push(*f);
        }
        l.clear();
    }

    /// `traceSweepParams`: meander forcing, pair coupling, crinkles of trace t.
    pub fn trace_sweep_params(&self, b: &OBoard, t: i32, p: &mut TraceSweep) {
        let tr = &b.traces[t as usize];
        *p = TraceSweep::default();
        let target_len = if tr.net >= 0 && tr.net < b.nets.len() as i32 { b.nets[tr.net as usize].target_length } else { 0.0f32 };
        p.amp = self.s.meander_amplitude;
        if self.s.enable_target_length_matching && target_len > 0.0f32 {
            if tr.length < target_len * (1.0f32 - self.s.length_matching_tolerance) {
                p.meander = true;
                let deficit = target_len - tr.length;
                p.amp = std_clamp(
                    (deficit / (target_len * 0.15f32 + 1e-3f32)) * self.s.meander_amplitude * 1.5f32,
                    0.2f32,
                    self.s.meander_amplitude * 3.0f32,
                );
            }
        } else if self.s.enable_length_tuning {
            p.meander = true;
        }
        p.couple = self.e.partner_of(tr.net) >= 0;
        // ADAPTED: pour hook 4, a pour path follows (see `relax_node`) and has no length to tune
        p.follow = b.pour_rules.binary_search(&tr.rule).is_ok();
        p.meander &= !p.follow;
        // skew tuning: nodes within the crinkle window of a bend of the shorter member crinkle
        let camp = if p.couple && tr.net < self.c.crinkle_amp.len() as i32 { self.c.crinkle_amp[tr.net as usize] } else { 0.0f32 };
        if camp > 0.0f32 && (t as usize) < self.c.trace_tips.len() && self.c.trace_tips[t as usize].len() == tr.points.len() {
            p.crinkle = Some(t as usize);
        }
        // ... and the longer member does not follow the crinkles there (decoupled near its bends)
        let ptn = if p.couple { self.e.partner_of(tr.net) } else { -1 };
        if ptn >= 0 && ptn < self.c.crinkle_amp.len() as i32 && self.c.crinkle_amp[ptn as usize] > 0.0f32 {
            p.quiet = self.crinkle_quiet(tr, &self.c.crinkle_bends[tr.net as usize], self.c.crinkle_amp[ptn as usize]);
        }
    }

    /// `recSprings`: the spring (split by the shares of tension, contraction and straightening
    /// in springK) and the forcing.
    pub fn rec_springs(&self, x: &mut EngineScratch, m: &LocalModel) {
        x.rec_clear();
        let a = self.s.tension as f64;
        let c = 0.5 * self.s.contraction_tension as f64;
        let st = self.s.straightening_force as f64;
        let sum = a + c + st;
        let fx = -m.gx;
        let fy = -m.gy;
        if sum.abs() > 1e-9 {
            x.rec_add(ForceTerm::Tension, fx * a / sum, fy * a / sum);
            x.rec_add(ForceTerm::Contraction, fx * c / sum, fy * c / sum);
            x.rec_add(ForceTerm::Straightening, fx * st / sum, fy * st / sum);
        } else {
            x.rec_add(ForceTerm::Tension, fx, fy);
        }
        x.rec_add(ForceTerm::Meander, m.fx, m.fy);
    }

    /// `recPush`. ADAPTED: the scratch is passed beside the log (the C++ log holds a pointer).
    #[allow(clippy::too_many_arguments)]
    pub fn rec_push(&self, b: &OBoard, l: &mut SweepLog, x: &EngineScratch, t: i32, k: i32, via: i32, at: Vec2) {
        let mut r = ForceRec::default();
        r.t = t;
        r.k = k;
        r.via = via;
        r.trace_id = if t >= 0 { b.traces[t as usize].id } else { -1 };
        r.at = at;
        for i in 0..FORCE_TERM_COUNT {
            r.f[i] = if (self.rec_mask >> i) & 1u32 != 0 {
                Vec2::new(x.rec_x[i] as f32, x.rec_y[i] as f32)
            } else {
                Vec2::new(0.0f32, 0.0f32)
            };
        }
        l.forces.push(r);
    }

    /// `relaxNode`: guarded move of interior node k of trace t (the node passed the sweep's
    /// gate). ADAPTED: the scratch `x` is passed beside the log; `tp.force` (a display field the
    /// port's board does not have) is not written.
    #[allow(clippy::too_many_arguments)]
    pub fn relax_node(
        &mut self,
        b: &mut OBoard,
        x: &mut EngineScratch,
        l: &mut SweepLog,
        t: i32,
        k: i32,
        p: &TraceSweep,
        dt: f32,
        ks: f64,
        mut omega: f32,
    ) {
        let tu = t as usize;
        let ku = k as usize;
        let n = b.traces[tu].points.len() as i32;
        let xp = b.traces[tu].points[ku].pos;
        let pa = b.traces[tu].points[ku - 1].pos;
        let pc = b.traces[tu].points[ku + 1].pos;
        l.eval_nodes += 1;
        let mut m = LocalModel::default();
        let w_a = self.c.seg_w[tu][ku - 1] as f64;
        let w_c = self.c.seg_w[tu][ku] as f64;
        m.gx = ks * (w_a * (xp.x - pa.x) as f64 + w_c * (xp.x - pc.x) as f64);
        m.gy = ks * (w_a * (xp.y - pa.y) as f64 + w_c * (xp.y - pc.y) as f64);
        m.hxx = ks * (w_a + w_c);
        m.hyy = m.hxx;
        let mut settle = K_SETTLE; // sleep threshold of this node (FA25 wideBendSettle)
        if (self.s.layer_crossing_perpendicularity > 0.0f32 || self.s.straight_bend > 0.0f32) && !p.couple {
            // P4e: bending energy kb/2 sum_j |p(j-1) - 2 p(j) + p(j+1)|^2, kb = K x spring, over
            // interior nodes only and none centred at a junction host. Not on diff-pair members.
            let kb = (self.s.layer_crossing_perpendicularity as f64 + self.s.straight_bend as f64) * ks;
            let tr = &b.traces[tu];
            let pt = |j: i32| tr.points[j as usize].pos;
            let mut gx = 0.0f64;
            let mut gy = 0.0f64;
            let mut h = 0.0f64;
            if k >= 3 && self.branches_of(t, k - 1).is_none() {
                let bb = pt(k - 2) - pt(k - 1) * 2.0f32 + xp;
                gx += bb.x as f64;
                gy += bb.y as f64;
                h += 1.0;
            }
            if k >= 2 && k + 2 < n && self.branches_of(t, k).is_none() {
                let bb = pa - xp * 2.0f32 + pc;
                gx += -2.0 * bb.x as f64;
                gy += -2.0 * bb.y as f64;
                h += 4.0;
            }
            if k + 3 < n && self.branches_of(t, k + 1).is_none() {
                let bb = xp - pc * 2.0f32 + pt(k + 2);
                gx += bb.x as f64;
                gy += bb.y as f64;
                h += 1.0;
            }
            m.gx += kb * gx;
            m.gy += kb * gy;
            m.hxx += kb * h;
            m.hyy += kb * h;
            // v16: a straightBend node gets its own SOR factor (not a junction host)
            if self.s.straight_bend > 0.0f32 && h > 0.0 && self.branches_of(t, k).is_none() {
                omega = std_clamp(self.s.bend_over_relaxation, 1.0f32, 1.9f32);
            }
            // FA25 wideBendSettle: on a wide track the sleep test scales with the spring share
            if self.s.straight_bend > 0.0f32 && self.s.wide_bend_settle && h > 0.0 && self.wide_trace(b, tr) {
                let hs = ks * (w_a + w_c);
                settle = (K_SETTLE as f64 * std_max(0.02, hs / (hs + kb * h))) as f32;
            }
        }
        if !self.c.seg_snap.is_empty() {
            self.add_snap(b, &mut m, t, k, xp, ks); // P3 wide-track octilinear snap (recorded as straightening)
        }
        if !self.c.seg_floor.is_empty() {
            self.add_floors(b, &mut m, t, k, ks); // O8b hop keep (recorded as tether)
        }
        if tu < self.c.bundle_pull.len() && ku < self.c.bundle_pull[tu].len() {
            for bp in &self.c.bundle_pull[tu][ku] {
                if bp.w > 0.0f32 {
                    // FA25b bundle keep: kk/2 ((q - x) . u - pitch)^2 (recorded as tether)
                    let kk = ks * self.s.bundle_keep as f64 * bp.w as f64;
                    let sdev = (bp.q.x - xp.x) as f64 * bp.u.x as f64 + (bp.q.y - xp.y) as f64 * bp.u.y as f64 - bp.pitch as f64;
                    m.gx -= kk * sdev * bp.u.x as f64;
                    m.gy -= kk * sdev * bp.u.y as f64;
                    m.add_h(bp.u.x as f64, bp.u.y as f64, kk);
                }
            }
        }
        // ADAPTED: spreadPull (P1d wire spreading, addSpread) is built by an unported pass (L5):
        // the step refuses wireSpread > 0, so it is always empty here.
        if tu < self.c.node_line.len() && !self.c.node_line[tu].is_empty() {
            // P3 wide-track line pull: kl/2 ((x - a) . n)^2 towards the node's polyline stretch
            let (la, ln) = self.c.node_line[tu][ku];
            if ln.x != 0.0f32 || ln.y != 0.0f32 {
                let kl = ks * self.s.wide_track_line_pull as f64;
                let e = (xp.x - la.x) as f64 * ln.x as f64 + (xp.y - la.y) as f64 * ln.y as f64;
                m.gx += kl * e * ln.x as f64;
                m.gy += kl * e * ln.y as f64;
                m.add_h(ln.x as f64, ln.y as f64, kl);
            }
        }
        if self.s.pad_exit_half_plane > 0.0f32 && !p.couple {
            // P7: the first two free nodes after a pad-exit stub stay beyond the stub end (the
            // first one also within the cone padExitMaxTurnDeg): slot 0 = front stub, 1 = back.
            let tr = &b.traces[tu];
            for i in 0..2usize {
                let front = i == 0;
                let e0 = if front { 1 } else { n - 2 };
                let e1 = if front { 0 } else { n - 1 }; // stub end, stub start (pad side)
                let off = if front { k - e0 } else { e0 - k }; // 1 = first free node
                if n < 4
                    || off < 1
                    || !tr.points[(if front { 1 } else { n - 1 }) as usize].straight_before
                    || tr.points[e1 as usize].terminal_pad < 0
                {
                    continue;
                }
                let se = tr.points[e0 as usize].pos;
                let su = se - tr.points[e1 as usize].pos;
                let sl = su.length();
                if !(sl > 1e-4f32) || !finite_vec(se) {
                    continue;
                }
                if off > 2 {
                    // later nodes: the own-pad box
                    let pi = tr.points[e1 as usize].terminal_pad;
                    if pi >= b.pads.len() as i32 {
                        continue;
                    }
                    let pd = &b.pads[pi as usize];
                    let ux = (su.x / sl) as f64;
                    let uy = (su.y / sl) as f64;
                    let has_local = pd.local_size.x > 0.0f32 && pd.local_size.y > 0.0f32;
                    let ls = if has_local { pd.local_size } else { pd.size };
                    let rot = (if has_local { pd.rotation } else { 0.0f32 }) as f64 * K_PI as f64 / 180.0;
                    let ex = rot.cos();
                    let ey = -rot.sin(); // pad local x on the board (y down)
                    let ext = |wx: f64, wy: f64| {
                        0.5 * ls.x as f64 * (wx * ex + wy * ey).abs() + 0.5 * ls.y as f64 * (-wx * ey + wy * ex).abs()
                    };
                    let mg = 0.5 * tr.width as f64 + 0.05;
                    m.sk[i] = ks * self.s.pad_exit_half_plane as f64;
                    m.sc[i] = 0.0;
                    m.sb[i] = ext(-uy, ux) + mg;
                    m.sd[i] = sl as f64 + ext(ux, uy) + mg;
                    m.sx[i] = se.x as f64;
                    m.sy[i] = se.y as f64;
                    m.sux[i] = ux;
                    m.suy[i] = uy;
                    continue;
                }
                m.sk[i] = ks * self.s.pad_exit_half_plane as f64;
                m.sc[i] = if off == 1 {
                    (std_clamp(self.s.pad_exit_max_turn_deg, 0.0f32, 90.0f32) as f64 * K_PI as f64 / 180.0).cos()
                } else {
                    0.0
                };
                m.sx[i] = se.x as f64;
                m.sy[i] = se.y as f64;
                m.sux[i] = (su.x / sl) as f64;
                m.suy[i] = (su.y / sl) as f64;
            }
        }
        if p.follow {
            // ADAPTED: pour hook 4. A pour path follows and never pulls: any curve between its ends
            // at clearance keeps the pour whole, so no shape term (spring, bend, snap, keeps) moves
            // it. Their curvature stays as damping, so the node takes a damped step on the
            // clearance terms alone: out from under a threshold or a neighbour's soft margin, and
            // where nothing is near it does not move and sleeps.
            m.gx = 0.0;
            m.gy = 0.0;
        }
        let chord = pc - pa;
        let cl = chord.length();
        if p.meander && k > 1 && k + 2 < n && cl > 1e-5f32 {
            let normal = (chord / cl).perpendicular();
            let osc = (k as f32 * self.s.meander_frequency * K_PI).sin();
            m.fx += (normal.x * osc * p.amp) as f64 * 0.1 * ks;
            m.fy += (normal.y * osc * p.amp) as f64 * 0.1 * ks;
        }
        x.rec_mask = self.rec_mask;
        x.prims.clear();
        let tr = &b.traces[tu];
        let mut p0 = Prim::default();
        p0.a = pa;
        p0.h = tr.width as f64 * 0.5;
        p0.layer = tr.layer;
        p0.net = tr.net;
        p0.trace = t;
        p0.ex_lo = k - 2;
        p0.ex_hi = k;
        p0.owner = 0;
        p0.obj = trace_key(t);
        p0.rule = tr.rule; // ADAPTED: the trace's clearance-table row
        let mut p1 = p0;
        p1.a = pc;
        p1.ex_lo = k - 1;
        p1.ex_hi = k + 1;
        if tu < self.c.seg_pass.len() && !self.c.seg_pass[tu].is_empty() {
            p0.pass = self.c.seg_pass[tu][ku - 1];
            p1.pass = self.c.seg_pass[tu][ku];
        }
        x.prims.push(p0);
        x.prims.push(p1);
        let mut n_owners = 1;
        // Junction host: every branch leg moves with the node, so its spring is part of the
        // node's energy (a Steiner-like equilibrium) and its segment is checked by the guard.
        let branches: Option<Vec<(i32, i32)>> = self.branches_of(t, k).cloned();
        x.co_moving.clear();
        if let Some(br) = &branches {
            let base = self.e.c.seg_base[tu];
            x.co_moving.push(base + k - 1);
            x.co_moving.push(base + k);
            for &(bt, e) in br {
                let btr = &b.traces[bt as usize];
                let bn = btr.points.len() as i32;
                let j = if e == 0 { 1 } else { bn - 2 };
                let seg = if e == 0 { 0 } else { bn - 2 };
                let pj = btr.points[j as usize].pos;
                if !finite_vec(pj) {
                    continue;
                }
                let wb = self.c.seg_w[bt as usize][seg as usize] as f64;
                m.gx += ks * wb * (xp.x - pj.x) as f64;
                m.gy += ks * wb * (xp.y - pj.y) as f64;
                m.hxx += ks * wb;
                m.hyy += ks * wb;
                let mut pb = Prim::default();
                pb.a = pj;
                pb.h = btr.width as f64 * 0.5;
                pb.layer = btr.layer;
                pb.net = btr.net;
                pb.trace = bt;
                pb.ex_lo = seg - 1;
                pb.ex_hi = seg + 1;
                pb.owner = n_owners;
                n_owners += 1;
                pb.obj = trace_key(bt);
                pb.rule = btr.rule; // ADAPTED: the trace's clearance-table row
                x.prims.push(pb);
                x.co_moving.push(self.e.c.seg_base[bt as usize] + seg);
            }
        }
        x.contacts.clear();
        x.coupling_scale = 1.0;
        x.crinkle_f = 0.0;
        let tip = p.crinkle.map_or(0.0f32, |ct| self.c.trace_tips[ct][ku]);
        if p.crinkle.is_some() && tip > 0.0f32 {
            // every other node near a bend: relaxed coupling and contraction and a forcing away
            // from the partner -> a crinkle tip; its neighbours stay coupled
            x.coupling_scale = 0.1;
            m.gx *= 0.5;
            m.gy *= 0.5;
            m.hxx *= 0.5;
            m.hyy *= 0.5;
            m.hxy *= 0.5;
            x.crinkle_f = tip as f64 * (m.hxx + 0.1 * 16.0 * self.s.diff_pair_coupling_strength as f64);
            x.crinkle_max = tip as f64;
        } else if !p.quiet.is_empty() && p.quiet[ku] != 0 {
            x.coupling_repel_only = true;
        }
        if self.rec_mask != 0 {
            self.rec_springs(x, &m); // spring (incl. branch legs) + meander forcing at x
        }
        // layer-crossing perpendicularity / disentanglement: plain interior nodes only
        x.crossing_forces = !p.follow
            && branches.is_none()
            && k >= 2
            && k + 2 < n
            && self.branches_of(t, k - 1).is_none()
            && self.branches_of(t, k + 1).is_none();
        let mut prims = std::mem::take(&mut x.prims);
        let contacts = std::mem::take(&mut x.contacts);
        let mut r = self.e.try_move(&*b, x, xp, &mut prims, n_owners, m, &contacts, 0.0, p.couple, omega);
        x.prims = prims;
        x.contacts = contacts;
        x.crossing_forces = false;
        // (the RELAX_DEBUG_TIP print of the reference is left out: no environment reads)
        x.coupling_scale = 1.0;
        x.crinkle_f = 0.0;
        x.coupling_repel_only = false;
        x.co_moving.clear();
        if r.moved && r.pos.distance_squared_to(xp) < K_MIN_APPLY2 {
            r.moved = false;
        }
        if self.rec_mask != 0 {
            let at = if r.moved && !self.rec_at_eval { r.pos } else { xp };
            self.rec_push(&*b, l, x, t, k, -1, at);
        }
        let mut big = false;
        if r.moved {
            {
                let tp = &mut b.traces[tu].points[ku];
                tp.velocity = (r.pos - xp) / dt;
                l.max_move = std_max(l.max_move, r.pos.distance_to(xp));
                tp.pos = r.pos;
            }
            self.l_box(l, t, r.pos);
            // (a junction host also drags its branch legs: any displacement counts)
            big = if branches.is_some() {
                r.pos.distance_squared_to(self.c.anchor[tu][ku]) > K_SETTLE2
            } else {
                Self::shape_moved(self.c.anchor[tu][ku], r.pos, pa, pc, settle)
            };
            // ADAPTED: a restoration step keeps the node awake until its deficit is cured or boxed.
            big |= r.restored;
            if let Some(br) = &branches {
                for &(bt, e) in br {
                    b.traces[bt as usize].points[e as usize].pos = r.pos;
                    self.l_box(l, bt, r.pos);
                    if big {
                        self.l_touch(l, bt, e, self.sub_index);
                    }
                }
            }
        } else {
            b.traces[tu].points[ku].velocity = Vec2::new(0.0f32, 0.0f32);
        }
        if big {
            self.l_set_quiet(l, t, k, 0);
            self.c.anchor[tu][ku] = r.pos;
            self.l_touch(l, t, k, self.sub_index);
            self.l_mark_dirty(l, xp, self.sub_index);
            self.l_mark_dirty(l, r.pos, self.sub_index);
            self.l_flag_resample(l, t);
            let net = b.traces[tu].net;
            self.l_flag_net(l, net);
            if let Some(br) = &branches {
                for &(bt, _e) in br {
                    self.l_flag_resample(l, bt);
                }
            }
        } else if self.c.quiet[tu][ku] < 255 {
            let q = self.c.quiet[tu][ku] + 1;
            self.l_set_quiet(l, t, k, q);
        }
    }

    /// `relaxVia`: guarded move of via v (passed should_evaluate_via).
    // ADAPTED: OrganicRelaxer.cpp:5073-5076 passes b and scratch x explicitly; the position x is xp, as in relax_node.
    // ADAPTED: OrganicRelaxer.cpp:5081,5092,5124-5126,5140 maps netId to net, layerIndex to layer and connectedPadId to connected_pad.
    pub fn relax_via(&mut self, b: &mut OBoard, x: &mut EngineScratch, l: &mut SweepLog, v: i32, dt: f32, kv: f64) {
        let vu = v as usize;
        let via = &b.vias[vu];
        let xp = via.pos;
        l.eval_vias += 1;
        x.prims.clear();
        let mut d = Prim::default();
        d.disc = true;
        d.a = xp;
        d.b = xp;
        d.h = via.diameter as f64 * 0.5;
        d.layer = -1;
        d.net = via.net;
        d.via_id = v;
        d.owner = 0;
        d.obj = via_key(v);
        d.drill = via.drill as f64;
        d.rule = via.rule; // ADAPTED: OrganicRelaxer.cpp:5081-5083 carries the via's clearance-table row.
        x.prims.push(d);
        let mut m = LocalModel::default();
        let mut owner = 1;
        for &(t, k) in &self.c.via_members[vu] {
            let tu = t as usize;
            let tr = &b.traces[tu];
            let n = tr.points.len() as i32;
            let mut sp = Prim::default();
            sp.h = tr.width as f64 * 0.5;
            sp.layer = tr.layer;
            sp.net = tr.net;
            sp.trace = t;
            sp.owner = owner;
            sp.obj = trace_key(t);
            sp.rule = tr.rule; // ADAPTED: OrganicRelaxer.cpp:5092-5093 carries the trace's clearance-table row.
            for side in 0..2 {
                let j = if side != 0 { k + 1 } else { k - 1 };
                if j < 0 || j >= n {
                    continue;
                }
                sp.a = tr.points[j as usize].pos;
                sp.ex_lo = if side != 0 { k - 1 } else { k - 2 };
                sp.ex_hi = if side != 0 { k + 1 } else { k };
                x.prims.push(sp);
                let w = self.c.seg_w[tu][(if side != 0 { k } else { k - 1 }) as usize] as f64;
                m.gx += kv * w * (xp.x - sp.a.x) as f64;
                m.gy += kv * w * (xp.y - sp.a.y) as f64;
                m.hxx += kv * w;
                m.hyy += kv * w;
            }
            if !self.c.seg_snap.is_empty() {
                self.add_snap(b, &mut m, t, k, xp, kv); // P3: the via slides onto a wide track's axis
            }
            if !self.c.seg_floor.is_empty() {
                self.add_floors(b, &mut m, t, k, kv); // O8b: a via-to-via run keeps its length
            }
            // ADAPTED: OrganicRelaxer.cpp:5110-5119 asserts spread_via_home empty, falsifying the P1d condition's first operand (L5).
            debug_assert!(self.c.spread_via_home.is_empty(), "wire spreading is not ported yet (L5)");
            owner += 1;
        }
        if m.hxx <= 0.0 {
            m.hyy = kv;
            m.hxx = m.hyy; // dangling via: only clearance terms
        }
        // Escape (dog-bone) via: stay within the escape radius of its pad.
        if via.connected_pad >= 0 && via.connected_pad < b.pads.len() as i32 {
            let pad = &b.pads[via.connected_pad as usize];
            m.tk = 4.0 * kv;
            m.tcx = pad.center.x as f64;
            m.tcy = pad.center.y as f64;
            m.td0 = if self.c.via_orphan[vu] != 0 {
                0.0
            } else {
                std_max(pad.size.x, pad.size.y) as f64 * 0.5 + via.diameter as f64 * 0.5 + b.rules.min_clearance as f64 + 0.1
            };
        }
        // Same-net pads currently touched by the via copper must stay touched.
        x.contacts.clear();
        let rv = via.diameter as f64 * 0.5;
        let n_p = b.pads.len() as i32;
        let qr = rv as f32 + 0.05f32;
        self.e.c.sgrid.query(xp.x - qr, xp.y - qr, xp.x + qr, xp.y + qr, |id| {
            if id >= n_p {
                return;
            }
            let pad = &b.pads[id as usize];
            if pad.net < 0 || pad.net != via.net {
                return;
            }
            if x.contacts.contains(&id) {
                return;
            }
            if self.e.point_pad_dist(&*b, xp, id) <= rv + 1e-3 {
                x.contacts.push(id);
            }
        });
        x.rec_mask = self.rec_mask;
        if self.rec_mask != 0 {
            self.rec_springs(x, &m);
        }
        // ADAPTED: OrganicRelaxer.cpp:5146 takes and restores the scratch vectors, as in relax_node.
        let mut prims = std::mem::take(&mut x.prims);
        let contacts = std::mem::take(&mut x.contacts);
        let mut r = self.e.try_move(&*b, x, xp, &mut prims, owner, m, &contacts, rv, false, 1.0f32);
        x.prims = prims;
        x.contacts = contacts;
        if r.moved && r.pos.distance_squared_to(xp) < K_MIN_APPLY2 {
            r.moved = false;
        }
        if self.rec_mask != 0 {
            let at = if r.moved && !self.rec_at_eval { r.pos } else { xp };
            self.rec_push(&*b, l, x, -1, -1, v, at);
        }
        let mut big = false;
        if r.moved {
            l.max_move = std_max(l.max_move, r.pos.distance_to(xp));
            b.vias[vu].velocity = (r.pos - xp) / dt;
            b.vias[vu].pos = r.pos;
            // ADAPTED: a restoration step keeps the via awake until its deficit is cured or boxed.
            big = r.restored || r.pos.distance_squared_to(self.c.vanchor[vu]) > K_SETTLE2;
            if big {
                self.l_mark_dirty(l, xp, self.sub_index);
                self.l_mark_dirty(l, r.pos, self.sub_index);
                self.c.vanchor[vu] = r.pos;
                self.l_flag_net(l, b.vias[vu].net);
            }
            for i in 0..self.c.via_members[vu].len() {
                let (t, k) = self.c.via_members[vu][i];
                b.traces[t as usize].points[k as usize].pos = r.pos;
                b.traces[t as usize].points[k as usize].velocity = b.vias[vu].velocity;
                self.l_box(l, t, r.pos);
                if big {
                    self.l_touch(l, t, k, self.sub_index);
                    self.l_flag_resample(l, t);
                }
            }
        } else {
            b.vias[vu].velocity = Vec2::new(0.0f32, 0.0f32);
            for &(t, k) in &self.c.via_members[vu] {
                b.traces[t as usize].points[k as usize].velocity = Vec2::new(0.0f32, 0.0f32);
            }
        }
        if big {
            self.c.vquiet[vu] = 0;
        } else if self.c.vquiet[vu] < 255 {
            self.c.vquiet[vu] += 1;
        }
        // ADAPTED: OrganicRelaxer.cpp:5182 (5180 in brief) omits via.force = {0, 0}; the port's board has no force field.
    }

    /// `nodeGate`: false when the node is not evaluated (pinned, asleep or broken).
    /// ADAPTED: `tp.force` is not written (the port's board has no display force).
    pub fn node_gate(&mut self, b: &mut OBoard, l: &mut SweepLog, t: i32, k: i32) -> bool {
        if self.node_pinned(&*b, t, k) {
            b.traces[t as usize].points[k as usize].velocity = Vec2::new(0.0f32, 0.0f32);
            return false;
        }
        if !self.should_evaluate(&*b, t, k) {
            b.traces[t as usize].points[k as usize].velocity = Vec2::new(0.0f32, 0.0f32);
            return false;
        }
        let pts = &b.traces[t as usize].points;
        let ku = k as usize;
        if !finite_vec(pts[ku].pos) || !finite_vec(pts[ku - 1].pos) || !finite_vec(pts[ku + 1].pos) {
            self.l_set_quiet(l, t, k, K_QUIET);
            return false;
        }
        true
    }

    /// `sweepTraces`: serial sweep in trace order (budgeted steps: resumable at cursor_t).
    pub fn sweep_traces(&mut self, b: &mut OBoard) {
        let dt = std_max(1e-4f32, self.s.dt);
        let ks = self.spring_k();
        let omega = std_clamp(self.s.over_relaxation, 1.0f32, 1.9f32);
        let tn = b.traces.len() as i32;
        if tn == 0 {
            return;
        }
        let mut l = SweepLog::default();
        // the log's scratch is the Engine's X0 (taken for the sweep, put back after)
        let mut x = std::mem::take(&mut self.e.x0);
        let mut p = TraceSweep::default();
        let start = if self.budget >= 0 { self.c.cursor_t % tn } else { 0 };
        for i in 0..tn {
            let t = (start + i) % tn;
            let n = b.traces[t as usize].points.len() as i32;
            if n < 3 {
                continue;
            }
            if !self.trace_may_wake(t, self.sub_index - 1) {
                continue;
            }
            self.trace_sweep_params(&*b, t, &mut p);
            for k in 1..n - 1 {
                if !self.node_gate(b, &mut l, t, k) {
                    continue;
                }
                if !self.spend(1) {
                    self.c.cursor_t = t;
                    self.apply_log(&mut l, self.sub_index);
                    self.e.x0 = x;
                    return;
                }
                self.relax_node(b, &mut x, &mut l, t, k, &p, dt, ks, omega);
            }
        }
        self.apply_log(&mut l, self.sub_index);
        if self.budget >= 0 {
            self.c.cursor_t = 0;
        }
        self.e.x0 = x;
    }

    /// `viaGate`. ADAPTED: `via.force` is not written (no display force on the port's board).
    pub fn via_gate(&mut self, b: &mut OBoard, v: i32) -> bool {
        let vu = v as usize;
        if self.c.via_pinned[vu] != 0 {
            b.vias[vu].velocity = Vec2::new(0.0f32, 0.0f32);
            self.c.vquiet[vu] = K_QUIET;
            return false;
        }
        if !finite_vec(b.vias[vu].pos) {
            self.c.vquiet[vu] = K_QUIET;
            return false;
        }
        if !self.should_evaluate_via(&*b, v) {
            b.vias[vu].velocity = Vec2::new(0.0f32, 0.0f32);
            return false;
        }
        true
    }

    /// `sweepVias`: serial sweep in via order (budgeted steps: resumable at cursor_v).
    pub fn sweep_vias(&mut self, b: &mut OBoard) {
        let dt = std_max(1e-4f32, self.s.dt);
        let kv = self.spring_k(); // same springs as between trace nodes (shared energy)
        let vn = b.vias.len() as i32;
        if vn == 0 {
            return;
        }
        let mut l = SweepLog::default();
        // ADAPTED: OrganicRelaxer.cpp:5203 takes X0 for the sweep and restores it on both exits, as in sweep_traces.
        let mut x = std::mem::take(&mut self.e.x0);
        let start = if self.budget >= 0 { self.c.cursor_v % vn } else { 0 };
        for i in 0..vn {
            let v = (start + i) % vn;
            if !self.via_gate(b, v) {
                continue;
            }
            if !self.spend(3) {
                self.c.cursor_v = v;
                self.apply_log(&mut l, self.sub_index);
                self.e.x0 = x;
                return;
            }
            self.relax_via(b, &mut x, &mut l, v, dt, kv);
        }
        self.apply_log(&mut l, self.sub_index);
        if self.budget >= 0 {
            self.c.cursor_v = 0;
        }
        self.e.x0 = x;
    }
}

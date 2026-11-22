// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.cpp (Engine: guarded moves).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Guarded Newton moves, fixed-geometry replacements, and segment clearance queries.
//!
//! ADAPTED: the restoration step (not in the reference). A guarded move never lets a pair end
//! under its threshold `thr` and closer than it started, so `min(gap, thr)` of every pair never
//! falls. But a pair under `thr` is only a soft penalty in the energy: soft margins can outweigh
//! it, and the Newton step then points into it and every halving is refused. When the ordinary
//! step does not move and a pair the step pushes is more than `K_RESTORE_DELTA` under `thr`, one
//! restoration try follows. It steps along the deficit- and lever-weighted sum of those pairs'
//! pushes, far enough for the deepest to reach its threshold, capped at `max_step`, with the same
//! three halvings. It takes the first that keeps the gap clause, the contacts and the cones as an
//! ordinary step must, does not raise the tether energy (tether, half-planes, hop floors), and
//! lowers the node's summed deficit by at least `K_RESTORE_DELTA`. The energy test is waived for
//! this step only. A sum that cancels (pushes from opposite sides) means the node is boxed in, and
//! it stays. Each restoration lowers the board's summed deficit by `K_RESTORE_DELTA` or more and
//! no move raises it, so there are at most that sum over `K_RESTORE_DELTA` of them. Cost: nothing
//! on a step that moves; a refused step with a deficit adds up to three guard checks over the
//! obstacles it already gathered.
//!
//! ADAPTED: two levels on the same-net via and pad pair (`PairEval::hard_extra`). Ordinary steps
//! keep its ring a full `thr` off the pad, as the reference does; the hard level is the rule DRC
//! judges, the drill wall at the pad's `via_drill_gap`. A restoration step's gap clause tests the
//! hard level, so a via may go inside its ring wall only to cure another pair's deficit, and never
//! past the drill rule; the pad pair does not push, so it never drives a restoration itself. So
//! `min(gap_hard, thr)` of every pair never falls under any step, and `min(gap, thr)` never falls
//! under an ordinary step.

use super::{finite_vec, Deficit, Engine, EngineScratch, ObKind, PairClass, PairEval, PenPair, Prim};
use crate::organic::copper::{proj_seg, K_BIG};
use crate::organic::geom::{std_clamp, std_max, std_min, Vec2};
use crate::organic::model::OBoard;
use crate::organic::settings::ForceTerm;

/// `MoveResult` (OrganicRelaxer.cpp:1282-1286).
#[derive(Copy, Clone, Debug, Default)]
pub struct MoveResult {
    pub moved: bool,
    pub pos: Vec2,
    /// ADAPTED: the move is a restoration step (see the module doc); the caller keeps the node
    /// awake, so it cures the rest of its deficit before it sleeps.
    pub restored: bool,
}

/// ADAPTED: the restoration step's unit of progress (mm). Positions are `f32`: at 100 mm one ulp
/// is 7.6e-6 mm, so a smaller unit would count rounding as progress.
pub const K_RESTORE_DELTA: f64 = 1e-4;

/// Caller-supplied local quadratic energy, forcing, and one-sided constraints.
/// `LocalModel` (OrganicRelaxer.cpp:1290-1374).
#[derive(Copy, Clone, Debug, Default)]
pub struct LocalModel {
    pub gx: f64,
    pub gy: f64,
    pub hxx: f64,
    pub hxy: f64,
    pub hyy: f64,
    pub fx: f64,
    pub fy: f64,
    pub tk: f64,
    pub tcx: f64,
    pub tcy: f64,
    pub td0: f64,
    pub sk: [f64; 2],
    pub sx: [f64; 2],
    pub sy: [f64; 2],
    pub sux: [f64; 2],
    pub suy: [f64; 2],
    pub sc: [f64; 2],
    pub sb: [f64; 2],
    pub sd: [f64; 2],
    pub nf: i32,
    pub fk: [f64; LocalModel::K_MAX_FLOORS],
    pub fqx: [f64; LocalModel::K_MAX_FLOORS],
    pub fqy: [f64; LocalModel::K_MAX_FLOORS],
    pub fr: [f64; LocalModel::K_MAX_FLOORS],
}

impl LocalModel {
    pub const K_MAX_FLOORS: usize = 8;

    pub fn add_floor(&mut self, k: f64, q: Vec2, r: f64) {
        if self.nf >= Self::K_MAX_FLOORS as i32 || !(k > 0.0) || !(r > 0.0) { return; }
        let i = self.nf as usize;
        self.fk[i] = k;
        self.fqx[i] = q.x as f64;
        self.fqy[i] = q.y as f64;
        self.fr[i] = r;
        self.nf += 1;
    }

    pub fn floor_e(&self, x: f64, y: f64) -> f64 {
        let mut e = 0.0;
        for i in 0..self.nf {
            let i = i as usize;
            let d = self.fr[i] - ((x - self.fqx[i]) * (x - self.fqx[i]) + (y - self.fqy[i]) * (y - self.fqy[i])).sqrt();
            if d > 0.0 { e += 0.5 * self.fk[i] * d * d; }
        }
        e
    }

    pub fn add_h(&mut self, nx: f64, ny: f64, k: f64) {
        self.hxx += k * nx * nx;
        self.hxy += k * nx * ny;
        self.hyy += k * ny * ny;
    }

    pub fn along(&self, i: usize, x: f64, y: f64) -> f64 {
        (x - self.sx[i]) * self.sux[i] + (y - self.sy[i]) * self.suy[i]
    }

    pub fn lateral(&self, i: usize, x: f64, y: f64) -> f64 {
        -(x - self.sx[i]) * self.suy[i] + (y - self.sy[i]) * self.sux[i]
    }

    /// Depth inside the own-pad box and its gradient direction (zero outside).
    pub fn box_depth(&self, i: usize, x: f64, y: f64) -> (f64, f64, f64) {
        let d = self.along(i, x, y);
        let l = self.lateral(i, x, y);
        let back = -d;
        let front = self.sd[i] + d;
        let side = self.sb[i] - l.abs();
        let p = std_min(std_min(back, front), side);
        if !(p > 0.0) { return (0.0, 0.0, 0.0); }
        // ADAPTED: OrganicRelaxer.cpp:1331-1349 returns the optional pointer outputs as a tuple.
        let (gpx, gpy) = if p == back {
            (-self.sux[i], -self.suy[i])
        } else if p == front {
            (self.sux[i], self.suy[i])
        } else {
            let sg = if l >= 0.0 { -1.0 } else { 1.0 };
            (sg * -self.suy[i], sg * self.sux[i])
        };
        (p, gpx, gpy)
    }

    pub fn cone_violation(&self, i: usize, x: f64, y: f64) -> f64 {
        if self.sb[i] > 0.0 { return self.box_depth(i, x, y).0; }
        let vx = x - self.sx[i];
        let vy = y - self.sy[i];
        self.sc[i] * (vx * vx + vy * vy).sqrt() - (vx * self.sux[i] + vy * self.suy[i])
    }

    pub fn half_plane_e(&self, x: f64, y: f64) -> f64 {
        let mut e = 0.0;
        for i in 0..2 {
            if self.sk[i] <= 0.0 { continue; }
            let d = if self.sb[i] > 0.0 { -self.box_depth(i, x, y).0 } else { self.along(i, x, y) };
            if d < 0.0 { e += 0.5 * self.sk[i] * d * d; }
        }
        e
    }

    /// Tether, half-plane, and hop-floor energy.
    pub fn tether_e(&self, x: f64, y: f64) -> f64 {
        let e = self.half_plane_e(x, y) + if self.nf != 0 { self.floor_e(x, y) } else { 0.0 };
        if self.tk <= 0.0 { return e; }
        let d = ((x - self.tcx) * (x - self.tcx) + (y - self.tcy) * (y - self.tcy)).sqrt() - self.td0;
        if d > 0.0 { e + 0.5 * self.tk * d * d } else { e }
    }
}

impl Engine {
    /// Single-threaded `tryMove` (OrganicRelaxer.cpp:1382-1386).
    pub fn try_move_x0(&mut self, b: &OBoard, pos: Vec2, prims: &mut Vec<Prim>, n_owners: i32,
        m: LocalModel, contact_pads: &[i32], contact_r: f64, couple: bool, omega: f32) -> MoveResult {
        // ADAPTED: OrganicRelaxer.cpp:1385 takes X0 out and restores it after the shared-scratch call.
        let mut x = std::mem::take(&mut self.x0);
        let r = self.try_move(b, &mut x, pos, prims, n_owners, m, contact_pads, contact_r, couple, omega);
        self.x0 = x;
        r
    }

    /// Guarded step, retried without crossing forces if they sharpen the node too far.
    /// `tryMove` (OrganicRelaxer.cpp:1387-1398).
    pub fn try_move(&self, b: &OBoard, x: &mut EngineScratch, pos: Vec2, prims: &mut Vec<Prim>, n_owners: i32,
        m: LocalModel, contact_pads: &[i32], contact_r: f64, couple: bool, omega: f32) -> MoveResult {
        let mut perp_tripped = false;
        let crossing_forces = x.crossing_forces;
        let mut r = self.try_move_impl(b, x, pos, prims, n_owners, m, contact_pads, contact_r, couple, omega,
            crossing_forces, &mut perp_tripped);
        if perp_tripped {
            r = self.try_move_impl(b, x, pos, prims, n_owners, m, contact_pads, contact_r, couple, omega,
                false, &mut perp_tripped);
        }
        r
    }

    /// One guarded Newton / coordinate-descent step (OrganicRelaxer.cpp:1399-1874).
    fn try_move_impl(&self, b: &OBoard, x: &mut EngineScratch, pos: Vec2, prims: &mut Vec<Prim>, n_owners: i32,
        mut m: LocalModel, contact_pads: &[i32], contact_r: f64, couple: bool, omega: f32, perp: bool,
        perp_tripped: &mut bool) -> MoveResult {
        // ADAPTED: OrganicRelaxer.cpp:1403-1411,1537 uses x fields directly instead of simultaneous scratch aliases.
        let coupling_scale = x.coupling_scale;
        let crinkle_f = x.crinkle_f;
        let crinkle_max = x.crinkle_max;
        let coupling_repel_only = x.coupling_repel_only;
        let rec = x.rec_mask != 0;
        // ADAPTED: OrganicRelaxer.cpp:1415 uses EngineScratch::rec_clear_move.
        if rec { x.rec_clear_move(); }
        let mut res = MoveResult { pos, ..MoveResult::default() };
        let (mut minx, mut miny, mut maxx, mut maxy) = (pos.x, pos.y, pos.x, pos.y);
        let mut hmax = 0.0;
        for p in prims.iter_mut() {
            p.b = pos;
            if p.disc { p.a = pos; }
            minx = std_min(minx, p.a.x);
            maxx = std_max(maxx, p.a.x);
            miny = std_min(miny, p.a.y);
            maxy = std_max(maxy, p.a.y);
            hmax = std_max(hmax, p.h);
        }
        // Pads / vias carry their full extent; segments carry their centreline bounds.
        let mut r = (hmax + self.c.max_half_w + std_max(std_max(
            self.clr_g + self.max_extra + self.margin_wide + self.hole_extra + self.via_extra,
            self.edge_g + self.margin), self.c.ko_max_g + self.margin) + 3.0 * self.max_step) as f32;
        if couple { r = std_max(r, (3.0 * std_max(0.1f32, self.s.diff_pair_distance) as f64 + 3.0 * self.max_step) as f32); }
        let mut q_layer = -2;
        let mut q_net = prims.first().map(|p| p.net);
        for p in prims.iter() {
            let l = if p.disc { -1 } else { p.layer };
            q_layer = if q_layer == -2 || q_layer == l { l } else { -1 };
            if q_net != Some(p.net) { q_net = None; }
        }
        // Crossing energy reads other layers, within the same gather reach.
        if self.s.layer_crossing_perpendicularity > 0.0f32 && !prims.is_empty() && !prims[0].disc { q_layer = -1; }
        self.gather(b, x, minx - r, miny - r, maxx + r, maxy + r, if q_layer < -1 { -1 } else { q_layer }, q_net);
        if !x.co_moving.is_empty() {
            let co_moving = &x.co_moving;
            x.obs.retain(|o| !(o.kind == ObKind::Seg && co_moving.contains(&o.id)));
        }
        let n_o = x.obs.len();
        x.g_old.clear();
        x.g_old.resize(n_owners as usize * n_o, K_BIG);
        x.g_old_h.clear();
        x.g_old_h.resize(n_owners as usize * n_o, K_BIG);
        x.g_thr.clear();
        x.g_thr.resize(n_owners as usize * n_o, 0.0);
        let mut any_ess = false;
        for p in prims.iter() { if self.obj_has_essential(p.obj) { any_ess = true; } }
        if any_ess {
            x.c_old.clear();
            x.c_old.resize(n_owners as usize * n_o, K_BIG);
        }

        let m0 = m;
        let mut active = false;
        let e_tether_old = m.tether_e(pos.x as f64, pos.y as f64);
        if m.tk > 0.0 {
            let dx = pos.x as f64 - m.tcx;
            let dy = pos.y as f64 - m.tcy;
            let d = (dx * dx + dy * dy).sqrt();
            if d > m.td0 && d > 1e-9 {
                let nx = dx / d;
                let ny = dy / d;
                let ex = d - m.td0;
                m.gx += m.tk * ex * nx;
                m.gy += m.tk * ex * ny;
                // ADAPTED: OrganicRelaxer.cpp:1470 uses EngineScratch::rec_add and settings::ForceTerm.
                if rec { x.rec_add(ForceTerm::Tether, -m.tk * ex * nx, -m.tk * ex * ny); }
                m.add_h(nx, ny, m.tk);
                m.add_h(-ny, nx, m.tk * ex / d);
                active = true;
            }
        }
        for i in 0..m.nf {
            let i = i as usize;
            let dx = pos.x as f64 - m.fqx[i];
            let dy = pos.y as f64 - m.fqy[i];
            let sl = (dx * dx + dy * dy).sqrt();
            let d = m.fr[i] - sl;
            if !(d > 0.0) || sl < 1e-9 { continue; }
            let ux = dx / sl;
            let uy = dy / sl;
            m.gx -= m.fk[i] * d * ux;
            m.gy -= m.fk[i] * d * uy;
            // ADAPTED: OrganicRelaxer.cpp:1485 uses EngineScratch::rec_add and settings::ForceTerm.
            if rec { x.rec_add(ForceTerm::Tether, m.fk[i] * d * ux, m.fk[i] * d * uy); }
            m.add_h(ux, uy, m.fk[i]);
            active = true;
        }
        let mut cone_old = [0.0; 2];
        for i in 0..2 {
            if m.sk[i] <= 0.0 { continue; }
            cone_old[i] = m.cone_violation(i, pos.x as f64, pos.y as f64);
            if m.sb[i] > 0.0 {
                let (p, gpx, gpy) = m.box_depth(i, pos.x as f64, pos.y as f64);
                if p > 0.0 {
                    m.gx += m.sk[i] * p * gpx;
                    m.gy += m.sk[i] * p * gpy;
                    // ADAPTED: OrganicRelaxer.cpp:1502 uses EngineScratch::rec_add and settings::ForceTerm.
                    if rec { x.rec_add(ForceTerm::PadExit, -m.sk[i] * p * gpx, -m.sk[i] * p * gpy); }
                    m.add_h(gpx, gpy, m.sk[i]);
                    active = true;
                }
                continue;
            }
            let d = m.along(i, pos.x as f64, pos.y as f64);
            if d < 0.0 {
                m.gx += m.sk[i] * d * m.sux[i];
                m.gy += m.sk[i] * d * m.suy[i];
                // ADAPTED: OrganicRelaxer.cpp:1513 uses EngineScratch::rec_add and settings::ForceTerm.
                if rec { x.rec_add(ForceTerm::PadExit, -m.sk[i] * d * m.sux[i], -m.sk[i] * d * m.suy[i]); }
                m.add_h(m.sux[i], m.suy[i], m.sk[i]);
                active = true;
            }
        }
        let mut cpl_best = K_BIG;
        let mut cpl_h = 0.0;
        let (mut cfx, mut cfy) = (0.0, 0.0);
        let mut cpl_dir = Vec2::new(0.0, 0.0);
        let mut e = PairEval::default();
        x.pen_pairs.clear();
        x.deficits.clear();
        x.partner_obs.clear();
        let mut e_old = e_tether_old;
        let is_trace_node = !prims.is_empty() && !prims[0].disc;
        let mut n_hat = Vec2::new(0.0f32, 1.0f32);
        let (mut trace_layer, mut trace_net) = (-1, -1);
        if is_trace_node {
            trace_layer = prims[0].layer;
            trace_net = prims[0].net;
            let chord = if prims.len() >= 2 { prims[1].a - prims[0].a } else { pos - prims[0].a };
            let cl = chord.length();
            if cl > 1e-4f32 { n_hat = (chord / cl).perpendicular(); }
        }
        x.perp_cand.clear();
        let mut cross_terms = false;
        // All trace nodes account for crossing energy; only perp nodes are driven by it.
        let k_perp = if is_trace_node { self.s.layer_crossing_perpendicularity as f64 } else { 0.0 };

        for o in 0..n_o {
            let ob = x.obs[o];
            if ob.kind == ObKind::Seg && is_trace_node && ob.net != trace_net && ob.layer >= 0 && trace_layer >= 0 {
                let d2 = ob.b - ob.a;
                if d2.length() > 1e-4f32 {
                    let mut drift_applied = false;
                    for (pi, p) in prims.iter().enumerate() {
                        if p.disc || p.layer != trace_layer { continue; }
                        let seg_a = p.a;
                        let seg_b = pos;
                        let d1 = seg_b - seg_a;
                        if d1.length() < 1e-4f32 { continue; }
                        let cross1 = d1.cross(ob.a - seg_a);
                        let cross2 = d1.cross(ob.b - seg_a);
                        let cross3 = d2.cross(seg_a - ob.a);
                        let cross4 = d2.cross(seg_b - ob.a);
                        let intersects = ((cross1 > 0.0f32) != (cross2 > 0.0f32)) && ((cross3 > 0.0f32) != (cross4 > 0.0f32));
                        if k_perp > 0.0 && ob.layer != trace_layer {
                            x.perp_cand.push((pi as i32, o as i32));
                            let w = Self::perp_weight(seg_a, pos, &ob);
                            if w > 0.0 {
                                if let Some((c, gx, gy)) = Self::perp_cos(seg_a, pos, &ob) {
                                    e_old += 0.5 * k_perp * w * c * c;
                                    if perp {
                                        m.gx += k_perp * w * c * gx;
                                        m.gy += k_perp * w * c * gy;
                                        // ADAPTED: OrganicRelaxer.cpp:1588 uses EngineScratch::rec_add and settings::ForceTerm.
                                        if rec { x.rec_add(ForceTerm::Perpendicularity, -k_perp * w * c * gx, -k_perp * w * c * gy); }
                                        m.add_h(gx, gy, k_perp * w);
                                        active = true;
                                        cross_terms = true;
                                    }
                                }
                            }
                        }
                        if !intersects { continue; }
                        if perp && !drift_applied && self.s.crossing_disentanglement.abs() > 1e-4f32
                            && ob.layer == trace_layer && ob.net >= 0 && trace_net >= 0 {
                            let drift_mag = -self.s.crossing_disentanglement;
                            let fdrift = n_hat * (drift_mag * 15.0f32);
                            m.fx += fdrift.x as f64;
                            m.fy += fdrift.y as f64;
                            cfx += fdrift.x as f64;
                            cfy += fdrift.y as f64;
                            // ADAPTED: OrganicRelaxer.cpp:1610 uses EngineScratch::rec_add and settings::ForceTerm.
                            if rec { x.rec_add(ForceTerm::Disentanglement, fdrift.x as f64, fdrift.y as f64); }
                            active = true;
                            drift_applied = true;
                            cross_terms = true;
                        }
                    }
                }
            }

            for (pi, p) in prims.iter().enumerate() {
                if any_ess && self.is_essential(p, &x.obs[o]) {
                    if let Some(cg) = self.contact_gap(b, p, &x.obs[o]) {
                        let idx = p.owner as usize * n_o + o;
                        x.c_old[idx] = std_min(x.c_old[idx], cg);
                    }
                }
                let pc = self.eval_pair(b, p, &x.obs[o], &mut e);
                if pc != PairClass::Eval { continue; }
                let idx = p.owner as usize * n_o + o;
                if e.gap < x.g_old[idx] { x.g_old[idx] = e.gap; x.g_thr[idx] = e.thr; }
                x.g_old_h[idx] = std_min(x.g_old_h[idx], e.gap_hard());
                if e.repel {
                    let w = if p.disc { 1.0 } else { e.t };
                    let pen = std_max(0.0, e.target - e.gap);
                    let pen_w = std_max(0.0, e.target_wide - e.gap);
                    let k = if x.obs[o].kind == ObKind::Seg && !p.disc { self.k_rep_trace }
                        else if p.disc && x.obs[o].kind == ObKind::Via { self.k_rep_via } else { self.k_rep_pad };
                    if e.gap < e.target_wide + 2.0 * self.max_step {
                        x.pen_pairs.push(PenPair { prim: pi as i32, ob: o as i32, t: e.target, tw: e.target_wide, k });
                        e_old += Self::pen_energy(e.gap, e.target, e.target_wide, k);
                    }
                    if (pen > 0.0 || pen_w > 0.0) && w > 1e-6 {
                        let vl = (e.vx * e.vx + e.vy * e.vy).sqrt();
                        let dir = if vl > 1e-9 { Vec2::new((e.vx / vl) as f32, (e.vy / vl) as f32) }
                            else { self.fallback_dir(b, p, &x.obs[o]) };
                        let kk = (if pen > 0.0 { k } else { 0.0 }) + if pen_w > 0.0 { 0.25 * k } else { 0.0 };
                        let f = k * pen + 0.25 * k * pen_w;
                        m.gx -= f * w * dir.x as f64;
                        m.gy -= f * w * dir.y as f64;
                        m.add_h(dir.x as f64, dir.y as f64, kk * w * w);
                        // ADAPTED: restoration step. A pair this far under its threshold is a deficit
                        // the step pushes; the restoration try weighs its push the same way.
                        if e.thr - e.gap > K_RESTORE_DELTA {
                            x.deficits.push(Deficit { prim: pi as i32, ob: o as i32, deficit: e.thr - e.gap, lever: w, dir });
                        }
                        if rec {
                            // ADAPTED: OrganicRelaxer.cpp:1657-1660 uses EngineScratch::rec_add and settings::ForceTerm.
                            x.rec_add(if x.obs[o].kind == ObKind::Seg && !p.disc { ForceTerm::TraceRepulsion }
                                else if p.disc && x.obs[o].kind == ObKind::Via { ForceTerm::ViaRepulsion }
                                else { ForceTerm::PadRepulsion }, f * w * dir.x as f64, f * w * dir.y as f64);
                        }
                        active = true;
                    }
                }
                if couple && e.partner {
                    x.partner_obs.push(o as i32);
                    let a = x.obs[o].a;
                    let bp = x.obs[o].b;
                    let (_, qx, qy) = proj_seg(pos.x as f64, pos.y as f64, a.x as f64, a.y as f64, bp.x as f64, bp.y as f64);
                    let dx = qx - pos.x as f64;
                    let dy = qy - pos.y as f64;
                    let d = (dx * dx + dy * dy).sqrt();
                    if d < cpl_best && d > 1e-6 {
                        cpl_best = d;
                        cpl_h = x.obs[o].h as f64;
                        cpl_dir = Vec2::new((dx / d) as f32, (dy / d) as f32);
                    }
                }
            }
        }
        let (mut cpl_target, mut kc) = (0.0, 0.0);
        if couple && cpl_best < K_BIG {
            let mut hw = 0.0;
            let mut pnet = -1;
            for p in prims.iter() {
                hw = std_max(hw, p.h);
                if pnet < 0 { pnet = p.net; }
            }
            let pg = if pnet >= 0 && (pnet as usize) < self.pair_gap.len() { self.pair_gap[pnet as usize] as f64 } else { 0.0 };
            let target = if pg > 0.0 { pg + hw + cpl_h + 0.004 }
                else { std_max(self.s.diff_pair_distance as f64, 2.0 * hw + self.clr_g + 0.01) };
            cpl_target = target;
            kc = std_clamp(16.0 * self.s.diff_pair_coupling_strength as f64 * coupling_scale, 0.0, 400.0);
            if self.s.diff_pair_rigid && (cpl_best - target).abs() < 0.25 * target { return res; }
            e_old += Self::cpl_energy(cpl_best, target, kc, coupling_repel_only);
            if cpl_best < if coupling_repel_only { target } else { 3.0 * target } {
                m.gx -= kc * (cpl_best - target) * cpl_dir.x as f64;
                m.gy -= kc * (cpl_best - target) * cpl_dir.y as f64;
                // ADAPTED: OrganicRelaxer.cpp:1707 uses EngineScratch::rec_add and settings::ForceTerm.
                if rec { x.rec_add(ForceTerm::Coupling, kc * (cpl_best - target) * cpl_dir.x as f64, kc * (cpl_best - target) * cpl_dir.y as f64); }
                m.add_h(cpl_dir.x as f64, cpl_dir.y as f64, kc);
                active = true;
                if crinkle_f > 0.0 {
                    m.fx -= crinkle_f * cpl_dir.x as f64;
                    m.fy -= crinkle_f * cpl_dir.y as f64;
                    cfx -= crinkle_f * cpl_dir.x as f64;
                    cfy -= crinkle_f * cpl_dir.y as f64;
                    // ADAPTED: OrganicRelaxer.cpp:1718 uses EngineScratch::rec_add and settings::ForceTerm.
                    if rec { x.rec_add(ForceTerm::Crinkle, -crinkle_f * cpl_dir.x as f64, -crinkle_f * cpl_dir.y as f64); }
                }
            }
        }
        // Newton step, over-relaxed only when no constraint is active.
        let hxx = m.hxx + 1e-6;
        let hyy = m.hyy + 1e-6;
        let hxy = m.hxy;
        let det = hxx * hyy - hxy * hxy;
        if !(det > 1e-12) { return self.restore(b, x, pos, prims, n_owners, &m0, &cone_old, contact_pads, contact_r, any_ess); }
        let rx = -m.gx + m.fx;
        let ry = -m.gy + m.fy;
        let mut r#move = Vec2::new(((hyy * rx - hxy * ry) / det) as f32, ((hxx * ry - hxy * rx) / det) as f32);
        if !active { r#move = r#move * omega; }
        if !finite_vec(r#move) { return self.restore(b, x, pos, prims, n_owners, &m0, &cone_old, contact_pads, contact_r, any_ess); }
        let mut ml = r#move.length();
        if ml > self.max_step as f32 {
            r#move = r#move * (self.max_step as f32 / ml);
            ml = self.max_step as f32;
        }
        if ml < 1e-7f32 { return self.restore(b, x, pos, prims, n_owners, &m0, &cone_old, contact_pads, contact_r, any_ess); }

        let mut contact_old = Vec::new();
        for &pid in contact_pads { contact_old.push(self.point_pad_dist(b, pos, pid)); }
        for attempt in 0..3 {
            let f = 1.0f32 / (1 << attempt) as f32;
            let q = pos + r#move * f;
            if !self.s.enable_drc_guard {
                res.moved = true;
                res.pos = q;
                return res;
            }
            let move_len = (ml * f) as f64;
            for p in prims.iter_mut() {
                p.b = q;
                if p.disc { p.a = q; }
            }
            // Strict descent of the shared energy, including all candidate crossings at q.
            {
                let dx = (q.x - pos.x) as f64;
                let dy = (q.y - pos.y) as f64;
                let d_e = m0.gx * dx + m0.gy * dy + 0.5 * (m0.hxx * dx * dx + 2.0 * m0.hxy * dx * dy + m0.hyy * dy * dy)
                    - (m0.fx * dx + m0.fy * dy) - (cfx * dx + cfy * dy);
                let mut e_new = m0.tether_e(q.x as f64, q.y as f64);
                x.g_new.clear();
                x.g_new.resize(n_owners as usize * n_o, K_BIG);
                x.done_new.clear();
                x.done_new.resize(prims.len() * n_o, 0);
                for pp in &x.pen_pairs {
                    if self.eval_pair(b, &prims[pp.prim as usize], &x.obs[pp.ob as usize], &mut e) == PairClass::Eval {
                        e_new += Self::pen_energy(e.gap, pp.t, pp.tw, pp.k);
                        let idx = prims[pp.prim as usize].owner as usize * n_o + pp.ob as usize;
                        x.g_new[idx] = std_min(x.g_new[idx], e.gap);
                    }
                    x.done_new[pp.prim as usize * n_o + pp.ob as usize] = 1;
                }
                for pp in &x.perp_cand {
                    let seg_a = prims[pp.0 as usize].a;
                    let ob = &x.obs[pp.1 as usize];
                    let w = Self::perp_weight(seg_a, q, ob);
                    if w > 0.0 {
                        if let Some((c, _, _)) = Self::perp_cos(seg_a, q, ob) {
                            e_new += 0.5 * k_perp * w * c * c;
                        }
                    }
                }
                if kc > 0.0 {
                    let mut best = K_BIG;
                    for &o in &x.partner_obs {
                        let a = x.obs[o as usize].a;
                        let bp = x.obs[o as usize].b;
                        let (_, qx, qy) = proj_seg(q.x as f64, q.y as f64, a.x as f64, a.y as f64, bp.x as f64, bp.y as f64);
                        best = std_min(best, ((qx - q.x as f64) * (qx - q.x as f64) + (qy - q.y as f64) * (qy - q.y as f64)).sqrt());
                    }
                    if best < K_BIG { e_new += Self::cpl_energy(best, cpl_target, kc, coupling_repel_only); }
                    if crinkle_f > 0.0 && best < K_BIG && best > cpl_target + crinkle_max && best > cpl_best { continue; }
                }
                if d_e + e_new > e_old + 1e-12 { continue; }
                let mut cone_deeper = false;
                for i in 0..2 {
                    if cone_deeper { break; }
                    if m0.sk[i] <= 0.0 { continue; }
                    let c_new = m0.cone_violation(i, q.x as f64, q.y as f64);
                    cone_deeper = c_new > 1e-6 && c_new > cone_old[i] + 1e-9;
                }
                if cone_deeper { continue; }
                if cross_terms && prims.len() == 2 {
                    let turn_cos = |y: Vec2| {
                        let ax = (y.x - prims[0].a.x) as f64;
                        let ay = (y.y - prims[0].a.y) as f64;
                        let cx = (prims[1].a.x - y.x) as f64;
                        let cy = (prims[1].a.y - y.y) as f64;
                        let la = (ax * ax + ay * ay).sqrt();
                        let lc = (cx * cx + cy * cy).sqrt();
                        if la > 1e-9 && lc > 1e-9 { (ax * cx + ay * cy) / (la * lc) } else { 1.0 }
                    };
                    let c_new = turn_cos(q);
                    if c_new < Self::K_PERP_MAX_TURN_COS && c_new < turn_cos(pos) - 1e-9 {
                        *perp_tripped = true;
                        return res;
                    }
                }
            }
            let mut ok = true;
            for k in 0..contact_pads.len() {
                if !ok { break; }
                let d_new = self.point_pad_dist(b, q, contact_pads[k]);
                if d_new > std_max(contact_old[k], contact_r - 0.002) { ok = false; }
            }
            if any_ess && ok && !self.contacts_kept(b, x, prims, n_owners, n_o) { ok = false; }
            for o in 0..n_o {
                if !ok { break; }
                // Farther pairs cannot reach the threshold within this move.
                let mut need = false;
                for w in 0..n_owners {
                    let idx = w as usize * n_o + o;
                    if x.g_old[idx] < K_BIG && x.g_old[idx] < x.g_thr[idx] + move_len + 1e-9 { need = true; }
                }
                if !need { continue; }
                for (pi, p) in prims.iter().enumerate() {
                    let idx = p.owner as usize * n_o + o;
                    if !(x.g_old[idx] < x.g_thr[idx] + move_len + 1e-9) { continue; }
                    if x.done_new[pi * n_o + o] != 0 { continue; }
                    let pc = self.eval_pair(b, p, &x.obs[o], &mut e);
                    if pc != PairClass::Eval { continue; }
                    x.g_new[idx] = std_min(x.g_new[idx], e.gap);
                }
                for w in 0..n_owners {
                    if !ok { break; }
                    let idx = w as usize * n_o + o;
                    if x.g_new[idx] < K_BIG && x.g_new[idx] < x.g_thr[idx] && x.g_new[idx] < x.g_old[idx] { ok = false; }
                }
            }
            if ok {
                res.moved = true;
                res.pos = q;
                return res;
            }
        }
        for p in prims.iter_mut() {
            p.b = pos;
            if p.disc { p.a = pos; }
        }
        self.restore(b, x, pos, prims, n_owners, &m0, &cone_old, contact_pads, contact_r, any_ess)
    }

    /// ADAPTED: the restoration try after an ordinary step that did not move (see the module doc).
    /// `x` holds the gathered obstacles, `g_old_h`/`g_thr` and the deficits of that step; `m0` is
    /// the caller's model. Leaves `prims` at `pos` unless the try moves them to its new position.
    fn restore(&self, b: &OBoard, x: &mut EngineScratch, pos: Vec2, prims: &mut [Prim], n_owners: i32,
        m0: &LocalModel, cone_old: &[f64; 2], contact_pads: &[i32], contact_r: f64, any_ess: bool) -> MoveResult {
        let res = MoveResult { pos, ..MoveResult::default() };
        if !self.s.enable_drc_guard || x.deficits.is_empty() { return res; }
        let (mut dx, mut dy, mut weight, mut before) = (0.0, 0.0, 0.0, 0.0);
        let mut deepest = x.deficits[0];
        for d in &x.deficits {
            dx += d.deficit * d.lever * d.dir.x as f64;
            dy += d.deficit * d.lever * d.dir.y as f64;
            weight += d.deficit * d.lever;
            before += d.deficit;
            if d.deficit > deepest.deficit { deepest = *d; }
        }
        let len = (dx * dx + dy * dy).sqrt();
        // pushes that cancel: the node is boxed in
        if !(len > 1e-6 * weight) { return res; }
        let (ux, uy) = (dx / len, dy / len);
        // the deepest pair's gap grows at this rate along the direction
        let rate = deepest.lever * (deepest.dir.x as f64 * ux + deepest.dir.y as f64 * uy);
        let reach = if rate > 1e-9 { (deepest.deficit + 2.0 * K_RESTORE_DELTA) / rate } else { self.max_step };
        let reach = std_min(reach, self.max_step);
        let tether_old = m0.tether_e(pos.x as f64, pos.y as f64);
        let contact_old: Vec<f64> = contact_pads.iter().map(|&pid| self.point_pad_dist(b, pos, pid)).collect();
        let n_o = x.obs.len();
        let mut e = PairEval::default();
        for attempt in 0..3 {
            let step = reach / (1 << attempt) as f64;
            let q = Vec2::new((pos.x as f64 + ux * step) as f32, (pos.y as f64 + uy * step) as f32);
            for p in prims.iter_mut() {
                p.b = q;
                if p.disc { p.a = q; }
            }
            if m0.tether_e(q.x as f64, q.y as f64) > tether_old + 1e-12 { continue; }
            if (0..2).any(|i| m0.sk[i] > 0.0 && {
                let c_new = m0.cone_violation(i, q.x as f64, q.y as f64);
                c_new > 1e-6 && c_new > cone_old[i] + 1e-9
            }) { continue; }
            if contact_pads.iter().zip(&contact_old)
                .any(|(&pid, &old)| self.point_pad_dist(b, q, pid) > std_max(old, contact_r - 0.002)) { continue; }
            if any_ess && !self.contacts_kept(b, x, prims, n_owners, n_o) { continue; }
            // the node's summed deficit falls by at least the unit
            let mut after = 0.0;
            for d in &x.deficits {
                if self.eval_pair(b, &prims[d.prim as usize], &x.obs[d.ob as usize], &mut e) == PairClass::Eval {
                    after += std_max(0.0, e.thr - e.gap);
                }
            }
            if after > before - K_RESTORE_DELTA { continue; }
            // the gap clause, as for an ordinary step but at each pair's hard level
            let move_len = q.distance_to(pos) as f64;
            x.g_new.clear();
            x.g_new.resize(n_owners as usize * n_o, K_BIG);
            let mut ok = true;
            for o in 0..n_o {
                if !ok { break; }
                for p in prims.iter() {
                    let idx = p.owner as usize * n_o + o;
                    if !(x.g_old_h[idx] < x.g_thr[idx] + move_len + 1e-9) { continue; }
                    if self.eval_pair(b, p, &x.obs[o], &mut e) != PairClass::Eval { continue; }
                    x.g_new[idx] = std_min(x.g_new[idx], e.gap_hard());
                }
                for w in 0..n_owners {
                    let idx = w as usize * n_o + o;
                    if x.g_new[idx] < K_BIG && x.g_new[idx] < x.g_thr[idx] && x.g_new[idx] < x.g_old_h[idx] { ok = false; }
                }
            }
            if ok {
                return MoveResult { moved: true, pos: q, restored: true };
            }
        }
        for p in prims.iter_mut() {
            p.b = pos;
            if p.disc { p.a = pos; }
        }
        res
    }

    /// Guard replacement of fixed primitives (OrganicRelaxer.cpp:1878-1952).
    /// Any interaction with another segment touched earlier in the pass rejects it.
    pub fn guard_replace(&mut self, b: &OBoard, old_prims: &[Prim], new_prims: &[Prim], n_owners: i32,
        touched: &[u8], own_trace: i32, ignore_lo: i32, ignore_hi: i32) -> bool {
        // ADAPTED: OrganicRelaxer.cpp:1881-1886 uses x fields; taking X0 around a closure preserves early returns.
        let mut x = std::mem::take(&mut self.x0);
        let result = (|| {
            let (mut minx, mut miny, mut maxx, mut maxy) = (1e30f32, 1e30f32, -1e30f32, -1e30f32);
            let mut hmax = 0.0;
            for v in [old_prims, new_prims] {
                for p in v {
                    minx = std_min(std_min(minx, p.a.x), p.b.x);
                    maxx = std_max(std_max(maxx, p.a.x), p.b.x);
                    miny = std_min(std_min(miny, p.a.y), p.b.y);
                    maxy = std_max(std_max(maxy, p.a.y), p.b.y);
                    hmax = std_max(hmax, p.h);
                }
            }
            let r = (hmax + self.c.max_half_w + std_max(std_max(
                self.clr_g + self.max_extra + self.margin_wide + self.hole_extra + self.via_extra,
                self.edge_g + self.margin), self.c.ko_max_g + self.margin) + 3.0 * self.max_step) as f32;
            let mut q_layer = -2;
            let mut q_net = old_prims.first().or(new_prims.first()).map(|p| p.net);
            for v in [old_prims, new_prims] {
                for p in v {
                    let l = if p.disc { -1 } else { p.layer };
                    q_layer = if q_layer == -2 || q_layer == l { l } else { -1 };
                    if q_net != Some(p.net) { q_net = None; }
                }
            }
            self.gather(b, &mut x, minx - r, miny - r, maxx + r, maxy + r, if q_layer < -1 { -1 } else { q_layer }, q_net);
            let n_o = x.obs.len();
            x.g_old.clear();
            x.g_old.resize(n_owners as usize * n_o, K_BIG);
            x.g_new.clear();
            x.g_new.resize(n_owners as usize * n_o, K_BIG);
            x.g_thr.clear();
            x.g_thr.resize(n_owners as usize * n_o, 0.0);
            let mut any_ess = false;
            for p in old_prims { if self.obj_has_essential(p.obj) { any_ess = true; } }
            if any_ess {
                x.c_old.clear();
                x.c_old.resize(n_owners as usize * n_o, K_BIG);
                for o in 0..n_o {
                    for p in old_prims {
                        if self.is_essential(p, &x.obs[o]) {
                            if let Some(cg) = self.contact_gap(b, p, &x.obs[o]) {
                                let idx = p.owner as usize * n_o + o;
                                x.c_old[idx] = std_min(x.c_old[idx], cg);
                            }
                        }
                    }
                }
                if !self.contacts_kept(b, &mut x, new_prims, n_owners, n_o) { return false; }
            }
            let mut e = PairEval::default();
            for o in 0..n_o {
                if x.obs[o].kind == ObKind::Seg && !touched.is_empty() && touched[x.obs[o].id as usize] != 0 {
                    let t2 = self.c.seg_trace[x.obs[o].id as usize];
                    let s2 = self.c.seg_idx[x.obs[o].id as usize];
                    if !(t2 == own_trace && s2 >= ignore_lo && s2 <= ignore_hi) { return false; }
                }
                for p in old_prims {
                    if self.eval_pair(b, p, &x.obs[o], &mut e) == PairClass::Eval {
                        let idx = p.owner as usize * n_o + o;
                        if e.gap < x.g_old[idx] { x.g_old[idx] = e.gap; x.g_thr[idx] = e.thr; }
                    }
                }
                for p in new_prims {
                    if self.eval_pair(b, p, &x.obs[o], &mut e) == PairClass::Eval {
                        let idx = p.owner as usize * n_o + o;
                        x.g_new[idx] = std_min(x.g_new[idx], e.gap);
                        if x.g_thr[idx] == 0.0 { x.g_thr[idx] = e.thr; }
                    }
                }
            }
            if !self.s.enable_drc_guard { return true; }
            for idx in 0..x.g_new.len() {
                if x.g_new[idx] < K_BIG && x.g_new[idx] < x.g_thr[idx] && x.g_new[idx] < x.g_old[idx] { return false; }
            }
            true
        })();
        self.x0 = x;
        result
    }

    /// Single-threaded `segmentClearance` (OrganicRelaxer.cpp:1960-1963).
    pub fn segment_clearance_x0(&mut self, b: &OBoard, t: i32, k: i32, cap: f64, reach_extra: f64) -> (f64, f64) {
        // ADAPTED: OrganicRelaxer.cpp:1962 takes X0 out and restores it after the shared-scratch call.
        let mut x = std::mem::take(&mut self.x0);
        let result = self.segment_clearance(b, &mut x, t, k, cap, reach_extra);
        self.x0 = x;
        result
    }

    /// Allowed spline deviation and smallest foreign copper gap (OrganicRelaxer.cpp:1964-1995).
    pub fn segment_clearance(&self, b: &OBoard, x: &mut EngineScratch, t: i32, k: i32, cap: f64, reach_extra: f64) -> (f64, f64) {
        let tr = &b.traces[t as usize];
        let mut p = Prim::default();
        p.a = tr.points[k as usize].pos;
        p.b = tr.points[(k + 1) as usize].pos;
        p.h = tr.width as f64 * 0.5;
        p.layer = tr.layer;
        p.net = tr.net;
        p.trace = t;
        p.ex_lo = k - 1;
        p.ex_hi = k + 1;
        // ADAPTED: OrganicRelaxer.cpp:1967-1975 carries tr.rule so eval_pair uses pair_shift with this segment's layer.
        p.rule = tr.rule;
        let mut slack = cap;
        let mut copper_gap = K_BIG;
        // ADAPTED: OrganicRelaxer.cpp:1964,1976-1978,1995 returns the two output references as (slack, copper_gap).
        if !finite_vec(p.a) || !finite_vec(p.b) { slack = 0.0; return (slack, copper_gap); }
        let reach = std_max(2.0 * cap, reach_extra)
            + std_max(std_max(self.clr_g + self.max_extra + self.hole_extra, self.edge_g), self.c.ko_max_g) + 0.02;
        let r = (p.h + self.c.max_half_w + reach) as f32;
        self.gather(b, x, std_min(p.a.x, p.b.x) - r, std_min(p.a.y, p.b.y) - r,
            std_max(p.a.x, p.b.x) + r, std_max(p.a.y, p.b.y) + r, p.layer, Some(p.net));
        let mut e = PairEval::default();
        for o in &x.obs {
            if o.kind == ObKind::Seg && o.trace != t && o.net == p.net { continue; }
            let pre = Self::bbox_gap2(&p, o).sqrt() - if o.kind == ObKind::Seg && o.trace == t { 0.0 } else { p.h + o.h as f64 };
            if pre > reach { continue; }
            if self.eval_pair(b, &p, o, &mut e) != PairClass::Eval { continue; }
            if e.repel && o.kind != ObKind::Edge && o.kind != ObKind::Ko { copper_gap = std_min(copper_gap, e.gap); }
            let mut allowed = e.gap - e.thr;
            if o.kind == ObKind::Seg { allowed *= 0.5; }
            slack = std_min(slack, allowed);
        }
        slack = std_max(0.0, slack);
        (slack, copper_gap)
    }
}

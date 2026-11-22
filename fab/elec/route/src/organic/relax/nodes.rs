// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.cpp (node state, dirty and focus cells,
// evaluation gates: lines 1998-2001, 2660-2855, 3888-3910).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Sleep state per node, the coarse dirty / focus cells, and the tests that decide whether a
//! node or a via is evaluated in a substep.

use super::{Relaxation, K_DIRTY_LIST_MAX, K_FOCUS_RADIUS, K_QUIET, K_SETTLE};
use crate::organic::engine::finite_vec;
use crate::organic::geom::{std_max, std_min, Segment2, Vec2};
use crate::organic::model::{OBoard, TracePoint};

/// `isPinned`: a node that may not move (fixed, on a pad, on a via, or a branch end).
#[inline]
pub fn is_pinned(p: &TracePoint) -> bool {
    p.is_fixed || p.terminal_pad >= 0 || p.attached_via >= 0 || p.branch_trace >= 0
}

pub(super) const INT_MIN_HALF: i32 = i32::MIN / 2;

impl Relaxation {
    // ---- node state -------------------------------------------------------
    pub fn movable(&self, b: &OBoard, t: i32, k: i32) -> bool {
        let pts = &b.traces[t as usize].points;
        k > 0 && k + 1 < pts.len() as i32 && !is_pinned(&pts[k as usize])
    }

    pub fn set_quiet(&mut self, t: i32, k: i32, q: u8) {
        let c = &mut self.c.quiet[t as usize][k as usize];
        let was = *c < K_QUIET;
        let now = q < K_QUIET;
        *c = q;
        if was != now {
            self.c.tr_awake[t as usize] += if now { 1 } else { -1 };
        }
    }

    pub fn wake(&mut self, b: &OBoard, t: i32, k: i32) {
        if self.movable(b, t, k) {
            self.set_quiet(t, k, 0);
        }
    }

    pub fn touch(&mut self, t: i32, k: i32, s: i32) {
        self.c.last_move[t as usize][k as usize] = s;
        self.c.tr_touch[t as usize] = std::cmp::max(self.c.tr_touch[t as usize], s);
    }

    pub fn reset_trace(&mut self, b: &OBoard, t: i32) {
        let tu = t as usize;
        let n = b.traces[tu].points.len();
        self.c.quiet[tu] = vec![K_QUIET; n];
        self.c.anchor[tu].resize(n, Vec2::default());
        self.c.last_move[tu] = vec![INT_MIN_HALF; n];
        self.c.tr_awake[tu] = 0;
        for k in 0..n {
            let pos = b.traces[tu].points[k].pos;
            self.c.anchor[tu][k] = pos;
            if self.movable(b, t, k as i32) && finite_vec(pos) {
                self.c.quiet[tu][k] = 0;
                self.c.tr_awake[tu] += 1;
            }
        }
        self.c.tr_touch[tu] = INT_MIN_HALF;
    }

    pub fn reset_via(&mut self, b: &OBoard, v: i32) {
        self.c.vquiet[v as usize] = 0;
        self.c.vanchor[v as usize] = b.vias[v as usize].pos;
    }

    pub fn resize_trace_state(&mut self, b: &OBoard) {
        let t = b.traces.len();
        let c = &mut self.c;
        c.quiet.resize(t, Vec::new());
        c.anchor.resize(t, Vec::new());
        c.last_move.resize(t, Vec::new());
        c.tr_touch.resize(t, 0);
        c.tr_awake.resize(t, 0);
        c.resample_level.resize(t, 0);
        c.resample_next.resize(t, 0);
        c.resample_flag.resize(t, 1);
        c.resample_tries.resize(t, 0);
    }

    pub fn reset_all(&mut self, b: &OBoard) {
        let t = b.traces.len();
        self.c.quiet = vec![Vec::new(); t];
        self.c.anchor = vec![Vec::new(); t];
        self.c.last_move = vec![Vec::new(); t];
        self.c.tr_touch = vec![INT_MIN_HALF; t];
        self.c.tr_awake = vec![0; t];
        for i in 0..t {
            self.reset_trace(b, i as i32);
        }
        self.c.vquiet = vec![0; b.vias.len()];
        self.c.vanchor.resize(b.vias.len(), Vec2::default());
        for v in 0..b.vias.len() {
            self.reset_via(b, v as i32);
        }
        self.c.resample_level = vec![0; t];
        self.c.resample_next = vec![0; t];
        self.c.resample_flag = vec![1; t];
        self.c.resample_tries = vec![0; t];
        self.c.net_dirty = vec![1; b.nets.len() + 1];
        self.c.cursor_t = 0;
        self.c.cursor_v = 0;
    }

    pub fn flag_resample(&mut self, t: i32) {
        self.c.resample_flag[t as usize] = 1;
        self.c.resample_tries[t as usize] = 0;
    }

    pub fn flag_net(&mut self, net: i32) {
        if net >= 0 && (net as usize) < self.c.net_dirty.len() {
            self.c.net_dirty[net as usize] = 1;
        }
    }

    // ---- coarse dirty / focus cells ----------------------------------------
    pub fn init_cells(&mut self) {
        let sz = self.e.c.bounds.size();
        let c = &mut self.c;
        c.dnx = std::cmp::max(1, (sz.x / c.dcs).ceil() as i32 + 1);
        c.dny = std::cmp::max(1, (sz.y / c.dcs).ceil() as i32 + 1);
        c.dirty_stamp = vec![INT_MIN_HALF; c.dnx as usize * c.dny as usize];
        c.focus_stamp = vec![INT_MIN_HALF; c.dnx as usize * c.dny as usize];
        c.dirty_list.clear();
    }

    pub fn cell_of(&self, p: Vec2) -> (i32, i32) {
        let bmin = self.e.c.bounds.min;
        let fx = (p.x - bmin.x) / self.c.dcs;
        let fy = (p.y - bmin.y) / self.c.dcs;
        let cx = if fx > 0.0 { std::cmp::min(self.c.dnx - 1, fx as i32) } else { 0 };
        let cy = if fy > 0.0 { std::cmp::min(self.c.dny - 1, fy as i32) } else { 0 };
        (cx, cy)
    }

    pub fn mark_dirty(&mut self, p: Vec2, s: i32) {
        if !finite_vec(p) {
            return;
        }
        let (cx, cy) = self.cell_of(p);
        let c = cy * self.c.dnx + cx;
        if self.c.dirty_stamp[c as usize] == s {
            return;
        }
        self.c.dirty_stamp[c as usize] = s;
        self.c.dirty_list.push((c, s));
        self.c.step_dirty.push(c);
    }

    pub fn dirty_around(&self, p: Vec2, since_sub: i32, r: i32) -> bool {
        let (cx, cy) = self.cell_of(p);
        let c = &self.c;
        for y in std::cmp::max(0, cy - r)..=std::cmp::min(c.dny - 1, cy + r) {
            for x in std::cmp::max(0, cx - r)..=std::cmp::min(c.dnx - 1, cx + r) {
                if c.dirty_stamp[(y * c.dnx + x) as usize] >= since_sub {
                    return true;
                }
            }
        }
        false
    }

    pub fn mark_focus(&mut self, p: Vec2) {
        if self.c.focus_since < 0 || !finite_vec(p) {
            return;
        }
        let (cx, cy) = self.cell_of(p);
        let r = (K_FOCUS_RADIUS / self.c.dcs).ceil() as i32;
        let c = &mut self.c;
        for y in std::cmp::max(0, cy - r)..=std::cmp::min(c.dny - 1, cy + r) {
            for x in std::cmp::max(0, cx - r)..=std::cmp::min(c.dnx - 1, cx + r) {
                c.focus_stamp[(y * c.dnx + x) as usize] = c.sub;
            }
        }
    }

    pub fn in_focus(&self, p: Vec2) -> bool {
        if self.c.focus_since < 0 {
            return true;
        }
        let (cx, cy) = self.cell_of(p);
        self.c.focus_stamp[(cy * self.c.dnx + cx) as usize] >= self.c.focus_since
    }

    pub fn prune_dirty_list(&mut self, since_sub: i32) {
        self.c.dirty_list.retain(|d| d.1 >= since_sub);
    }

    /// Can any node of trace t be woken in this substep? (cheap whole-trace skip; an exact
    /// superset of the per-node `should_evaluate` conditions)
    pub fn trace_may_wake(&self, t: i32, since_sub: i32) -> bool {
        let tu = t as usize;
        let c = &self.c;
        if c.tr_awake[tu] > 0 || c.tr_touch[tu] >= since_sub {
            return true;
        }
        for &bt in &c.linked_traces[tu] {
            if c.tr_touch[bt as usize] >= since_sub {
                return true;
            }
        }
        if c.dirty_list.len() as i32 > K_DIRTY_LIST_MAX {
            return true;
        }
        if c.dirty_list.is_empty() {
            return false;
        }
        let bx = &self.tr_box[tu];
        if !bx.is_valid() {
            return true;
        }
        let (x0, y0) = self.cell_of(bx.min);
        let (x1, y1) = self.cell_of(bx.max);
        for &(cell, s) in &c.dirty_list {
            if s < since_sub {
                continue;
            }
            let cx = cell % c.dnx;
            let cy = cell / c.dnx;
            if cx >= x0 - 1 && cx <= x1 + 1 && cy >= y0 - 1 && cy <= y1 + 1 {
                return true;
            }
        }
        false
    }

    /// Did moving a node from its anchor A to q (neighbours pa, pc) change the copper shape by
    /// more than `settle`? Sliding along its own segments (spacing equalisation) does not.
    pub fn shape_moved(a: Vec2, q: Vec2, pa: Vec2, pc: Vec2, settle: f32) -> bool {
        if q.distance_squared_to(a) <= settle * settle {
            return false;
        }
        let d = |p: Vec2, s0: Vec2, s1: Vec2| Segment2::new(s0, s1).distance_to_point(p);
        let d1 = std_min(d(q, pa, a), d(q, a, pc));
        let d2 = std_min(d(a, pa, q), d(a, q, pc));
        std_max(d1, d2) > (settle * settle).sqrt()
    }

    /// `shapeMoved` with its default settle (`kSettle`).
    #[inline]
    pub fn shape_moved_default(a: Vec2, q: Vec2, pa: Vec2, pc: Vec2) -> bool {
        Self::shape_moved(a, q, pa, pc, K_SETTLE)
    }

    pub fn should_evaluate(&self, b: &OBoard, t: i32, k: i32) -> bool {
        let p = b.traces[t as usize].points[k as usize].pos;
        if !self.in_focus(p) {
            return false;
        }
        if self.c.quiet[t as usize][k as usize] < K_QUIET {
            return true;
        }
        let lm = &self.c.last_move[t as usize];
        let n = lm.len() as i32;
        for i in std::cmp::max(0, k - 2)..=std::cmp::min(n - 1, k + 2) {
            if lm[i as usize] >= self.sub_index - 1 {
                return true;
            }
        }
        if let Some(br) = self.branches_of(t, k) {
            for &(bt, e) in br {
                let bl = &self.c.last_move[bt as usize];
                let bn = bl.len() as i32;
                for i in std::cmp::max(0, e - 2)..=std::cmp::min(bn - 1, e + 2) {
                    if bl[i as usize] >= self.sub_index - 1 {
                        return true;
                    }
                }
            }
        }
        self.dirty_around(p, self.sub_index - 1, 1)
    }

    pub fn should_evaluate_via(&self, b: &OBoard, v: i32) -> bool {
        let p = b.vias[v as usize].pos;
        if !self.in_focus(p) {
            return false;
        }
        if self.c.vquiet[v as usize] < K_QUIET {
            return true;
        }
        for &(t, k) in &self.c.via_members[v as usize] {
            let lm = &self.c.last_move[t as usize];
            for i in std::cmp::max(0, k - 1)..=std::cmp::min(lm.len() as i32 - 1, k + 1) {
                if lm[i as usize] >= self.sub_index - 1 {
                    return true;
                }
            }
        }
        self.dirty_around(p, self.sub_index - 1, 2)
    }

    // ---- pins and branches (3902-3910) ---------------------------------------
    /// A node that may not move. Junction (branch host) nodes move.
    #[inline]
    pub fn node_pinned(&self, b: &OBoard, t: i32, k: i32) -> bool {
        is_pinned(&b.traces[t as usize].points[k as usize])
    }

    /// A node that may not be removed by resampling (pinned, or a junction host).
    #[inline]
    pub fn node_anchored(&self, b: &OBoard, t: i32, k: i32) -> bool {
        self.node_pinned(b, t, k) || self.c.branch_target[t as usize][k as usize] != 0
    }

    pub fn branches_of(&self, t: i32, k: i32) -> Option<&Vec<(i32, i32)>> {
        if self.c.branch_target[t as usize][k as usize] == 0 {
            return None;
        }
        self.c.host_branches.get(&Self::host_key(t, k))
    }

    /// `syncBranchLinks`: every branch end sits on its host node.
    pub fn sync_branch_links(&self, b: &mut OBoard) {
        for l in &self.c.branch_links {
            let (lk, lhk) = (l.k as usize, l.hk as usize);
            if lk >= b.traces[l.t as usize].points.len() || lhk >= b.traces[l.h as usize].points.len() {
                continue;
            }
            let hp = b.traces[l.h as usize].points[lhk].pos;
            let p = &mut b.traces[l.t as usize].points[lk];
            p.pos = hp;
            p.velocity = Vec2::new(0.0, 0.0);
        }
    }
}

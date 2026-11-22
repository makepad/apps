// Ported from TiNRS Psychedelic Router 0.1.0: src/optimize/PairGuard.h (1-139).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in the parent folder.

//! Guard finish-pass edits against losing a differential pair's coupled length.
//! Callers supply only replaced copper; partner copper uses current Trace::points.
//!
//! ADAPTED: the three poly overloads are poly_trace_points, poly_points and
//! poly_trace. Default arguments are explicit; the optional lost pointer is
//! Option<&mut f64>. Vectors keep reference insertion order; no map is iterated.

use super::super::diffpairs::{self, CouplingPoly};
use super::super::geom::{std_max, std_min, Box2, Vec2};
use super::super::model::{OBoard, Trace, TracePoint};

/// PairGuard.h:33-137: cache pair membership, then compare old and new coupling.
pub struct PairGuard {
    /// PairGuard.h:36: one metrics sample, in mm.
    pub tol_mm: f32,
    on: bool,
    partner_net: Vec<i32>,
    gap_of: Vec<f32>,
}

impl Default for PairGuard {
    fn default() -> Self {
        Self { tol_mm: 0.05f32, on: false, partner_net: Vec::new(), gap_of: Vec::new() }
    }
}

impl PairGuard {
    /// PairGuard.h:38-52.
    pub fn init(&mut self, b: &OBoard, enabled: bool) {
        self.on = false;
        self.partner_net.clear();
        self.partner_net.resize(b.nets.len(), -1);
        self.gap_of.clear();
        self.gap_of.resize(b.nets.len(), 0.0f32);
        if !enabled { return; }
        for n in 0..b.nets.len() as i32 {
            let q = diffpairs::partner_of(b, n);
            if q < 0 { continue; }
            self.partner_net[n as usize] = q;
            self.gap_of[n as usize] = diffpairs::member_gap(b, n);
            self.on = true;
        }
    }

    /// PairGuard.h:53.
    pub fn active(&self) -> bool {
        self.on
    }

    /// PairGuard.h:54.
    pub fn member(&self, net: i32) -> bool {
        self.on && net >= 0 && net < self.partner_net.len() as i32 && self.partner_net[net as usize] >= 0 && self.gap_of[net as usize] > 0.0f32
    }

    /// PairGuard.h:55.
    pub fn partner(&self, net: i32) -> i32 {
        if self.member(net) { self.partner_net[net as usize] } else { -1 }
    }

    /// PairGuard.h:57-67: inclusive bounds; C++ defaults are lo = 0, hi = -1.
    pub fn poly_trace_points(pts: &[TracePoint], layer: i32, width: f32, mut lo: i32, mut hi: i32) -> CouplingPoly {
        let mut p = CouplingPoly { layer, width, ..CouplingPoly::default() };
        if hi < 0 { hi = pts.len() as i32 - 1; }
        lo = std_max(lo, 0);
        hi = std_min(hi, pts.len() as i32 - 1);
        for k in lo..=hi { p.pts.push(pts[k as usize].pos); }
        p
    }

    /// PairGuard.h:68-78: inclusive bounds; C++ defaults are lo = 0, hi = -1.
    pub fn poly_points(pts: &[Vec2], layer: i32, width: f32, mut lo: i32, mut hi: i32) -> CouplingPoly {
        let mut p = CouplingPoly { layer, width, ..CouplingPoly::default() };
        if hi < 0 { hi = pts.len() as i32 - 1; }
        lo = std_max(lo, 0);
        hi = std_min(hi, pts.len() as i32 - 1);
        for k in lo..=hi { p.pts.push(pts[k as usize]); }
        p
    }

    /// PairGuard.h:79: current points, not the smoothed export.
    pub fn poly_trace(t: &Trace) -> CouplingPoly {
        Self::poly_trace_points(&t.points, t.layer, t.width, 0, -1)
    }

    /// PairGuard.h:81-99: current partner copper near box_, or all if it is invalid.
    pub fn partner_copper(&self, b: &OBoard, net: i32, box_: Box2) -> Vec<CouplingPoly> {
        let mut out = Vec::new();
        let q = self.partner(net);
        if q < 0 { return out; }
        for t in &b.traces {
            if t.net != q || !t.is_routed || t.points.len() < 2 { continue; }
            if box_.is_valid() {
                let mut tb = Box2::default();
                for p in &t.points { tb.expand(p.pos); }
                if tb.max.x < box_.min.x || tb.min.x > box_.max.x || tb.max.y < box_.min.y || tb.min.y > box_.max.y { continue; }
            }
            out.push(Self::poly_trace(t));
        }
        out
    }

    /// PairGuard.h:101-106: coupled mm in both directions; net must be a member.
    pub fn coupled(&self, _b: &OBoard, net: i32, polys: &[CouplingPoly], partner_polys: &[CouplingPoly]) -> f64 {
        diffpairs::coupled_length(polys, partner_polys, self.gap_of[net as usize], true)
    }

    /// PairGuard.h:108-131: keep edits within tol_mm; always keep nonmember edits.
    /// lost is zero unless refused, then before - after (C++ default: None).
    pub fn keeps(&self, b: &OBoard, net: i32, old_p: &[CouplingPoly], new_p: &[CouplingPoly], mut lost: Option<&mut f64>) -> bool {
        if let Some(out) = lost.as_deref_mut() { *out = 0.0; }
        if !self.member(net) { return true; }
        let mut box_ = Box2::default();
        let mut w = 0.0f32;
        for v in [old_p, new_p] {
            for p in v {
                for &x in &p.pts { box_.expand(x); }
                w = std_max(w, p.width);
            }
        }
        if !box_.is_valid() { return true; }
        box_.pad(self.gap_of[net as usize] * 1.2f32 + 2.0f32 * w + 0.5f32);
        let partner_polys = self.partner_copper(b, net, box_);
        if partner_polys.is_empty() { return true; }
        let before = self.coupled(b, net, old_p, &partner_polys);
        let after = self.coupled(b, net, new_p, &partner_polys);
        if after >= before - self.tol_mm as f64 { return true; }
        if let Some(out) = lost { *out = before - after; }
        false
    }
}

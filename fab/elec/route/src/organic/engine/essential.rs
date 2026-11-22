// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.cpp (Engine).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Essential copper contacts and the contact-preservation guard.

use super::{finite_vec, Engine, EngineScratch, Ob, ObKind, Prim};
use crate::organic::copper::{self, pad_on_layer, seg_pad, seg_seg, K_BIG, K_PAD_BASE, K_VIA_BASE};
use crate::organic::geom::{std_max, std_min};
use crate::organic::model::{OBoard, Pad};

impl Engine {
    /// `computeEssential` (OrganicRelaxer.cpp:320-356); `nets`, when present, is sorted.
    pub fn compute_essential(&mut self, b: &OBoard, nets: Option<&[i32]>, slack: f64) {
        let wanted = |net: i32| nets.map_or(true, |nets| nets.binary_search(&net).is_ok());
        if let Some(nets) = nets {
            for n in nets { self.c.ess_by_net.remove(n); }
        } else {
            self.c.ess_by_net.clear();
        }
        let mut cs = Vec::new();
        for net in &b.nets {
            if net.id < 0 || !wanted(net.id) { continue; }
            cs.clear();
            copper::net_pad_groups(b, net.id, Some(&mut cs), 1e-3);
            let v = self.c.ess_by_net.entry(net.id).or_default();
            for c in &cs { v.push((c.a, c.b)); }
        }
        // Keep redundant via/trace contacts too, so a via never becomes dangling.
        let mut x = std::mem::take(&mut self.x0);
        for (v, via) in b.vias.iter().enumerate() {
            if !finite_vec(via.pos) || !wanted(via.net) { continue; }
            let rv = via.diameter as f64 * 0.5;
            let r = (rv + self.c.max_half_w + 0.01 + slack) as f32;
            self.gather(b, &mut x, via.pos.x - r, via.pos.y - r, via.pos.x + r, via.pos.y + r, -1, Some(via.net));
            for o in &x.obs {
                if o.kind != ObKind::Seg || o.net != via.net { continue; }
                if seg_seg(o.a, o.b, via.pos, via.pos).d <= o.h as f64 + rv + 1e-3 {
                    self.c.ess_by_net.entry(via.net).or_default().push((copper::via_key(v as i32), copper::trace_key(o.trace)));
                }
            }
        }
        self.x0 = x;
        self.c.essential.clear();
        self.c.trace_ess.clear();
        self.c.trace_ess.resize(b.traces.len(), 0);
        self.c.via_ess.clear();
        self.c.via_ess.resize(b.vias.len(), 0);
        // Temporarily move the map so add_essential can mutate the cache in key order.
        let ess_by_net = std::mem::take(&mut self.c.ess_by_net);
        for v in ess_by_net.values() {
            for &(a, b) in v { self.add_essential(a, b); }
        }
        self.c.ess_by_net = ess_by_net;
    }

    /// `addEssential` (OrganicRelaxer.cpp:358-366).
    pub fn add_essential(&mut self, a: i64, b: i64) {
        self.c.essential.insert(copper::pair_key(a, b));
        for k in [a, b] {
            if k < K_VIA_BASE {
                if k < self.c.trace_ess.len() as i64 { self.c.trace_ess[k as usize] = 1; }
            } else if k < K_PAD_BASE {
                if k - K_VIA_BASE < self.c.via_ess.len() as i64 { self.c.via_ess[(k - K_VIA_BASE) as usize] = 1; }
            }
        }
    }

    /// `objHasEssential` (OrganicRelaxer.cpp:368-374).
    pub fn obj_has_essential(&self, k: i64) -> bool {
        if self.c.essential.is_empty() || k < 0 { return false; }
        if k < K_VIA_BASE { return k < self.c.trace_ess.len() as i64 && self.c.trace_ess[k as usize] != 0; }
        if k < K_PAD_BASE { return k - K_VIA_BASE < self.c.via_ess.len() as i64 && self.c.via_ess[(k - K_VIA_BASE) as usize] != 0; }
        false
    }

    /// `obKey` (OrganicRelaxer.cpp:376-385).
    pub fn ob_key(&self, o: &Ob) -> i64 {
        match o.kind {
            ObKind::Seg => copper::trace_key(self.c.seg_trace[o.id as usize]),
            ObKind::Pad => copper::pad_key(o.id),
            ObKind::Via => copper::via_key(o.id),
            _ => -1,
        }
    }

    /// `isEssential` (OrganicRelaxer.cpp:387-393).
    pub fn is_essential(&self, p: &Prim, o: &Ob) -> bool {
        if !self.obj_has_essential(p.obj) { return false; }
        let k = self.ob_key(o);
        if k < 0 || k == p.obj { return false; }
        self.c.essential.contains(&copper::pair_key(p.obj, k))
    }

    /// `contactGap` (OrganicRelaxer.cpp:396-424), regardless of net; false becomes None.
    pub fn contact_gap(&self, b: &OBoard, p: &Prim, o: &Ob) -> Option<f64> {
        match o.kind {
            ObKind::Seg => {
                let tr2 = &b.traces[self.c.seg_trace[o.id as usize] as usize];
                if !p.disc && tr2.layer != p.layer { return None; }
                let a = tr2.points[self.c.seg_idx[o.id as usize] as usize].pos;
                let bp = tr2.points[self.c.seg_idx[o.id as usize] as usize + 1].pos;
                Some(seg_seg(p.a, p.b, a, bp).d - p.h - tr2.width as f64 * 0.5)
            }
            ObKind::Pad => {
                let pad = &b.pads[o.id as usize];
                if !p.disc && !pad_on_layer(pad, p.layer, self.n_l) { return None; }
                let m = &self.c.pad_m[o.id as usize];
                Some(seg_pad(p.a, p.b, m).d - m.r - p.h)
            }
            ObKind::Via => {
                let v = &b.vias[o.id as usize];
                Some(seg_seg(p.a, p.b, v.pos, v.pos).d - v.diameter as f64 * 0.5 - p.h)
            }
            _ => None,
        }
    }

    /// `discOnPad` (OrganicRelaxer.cpp:496-508).
    pub fn disc_on_pad(&self, b: &OBoard, p: &Prim, pad: &Pad) -> bool {
        let (mut lo, mut hi) = (0, self.n_l - 1);
        if p.via_id >= 0 && p.via_id < b.vias.len() as i32 {
            let v = &b.vias[p.via_id as usize];
            lo = std::cmp::max(0, std::cmp::min(v.layer_start, v.layer_end));
            hi = std::cmp::min(self.n_l - 1, std::cmp::max(v.layer_start, v.layer_end));
        }
        for l in lo..=hi {
            if pad_on_layer(pad, l, self.n_l) { return true; }
        }
        false
    }

    /// `contactsKept` (OrganicRelaxer.cpp:1263-1280).
    pub fn contacts_kept(&self, b: &OBoard, x: &mut EngineScratch, prims: &[Prim], n_owners: i32, n_o: usize) -> bool {
        let EngineScratch { c_old, c_new, obs, .. } = x;
        c_new.clear();
        c_new.resize(n_owners as usize * n_o, K_BIG);
        for o in 0..n_o {
            for p in prims {
                let idx = p.owner as usize * n_o + o;
                if !(c_old[idx] <= 1e-3) { continue; }
                if let Some(cg) = self.contact_gap(b, p, &obs[o]) { c_new[idx] = std_min(c_new[idx], cg); }
            }
        }
        for idx in 0..c_new.len() {
            if c_old[idx] <= 1e-3 && c_new[idx] > std_max(c_old[idx], -0.002) { return false; }
        }
        true
    }
}

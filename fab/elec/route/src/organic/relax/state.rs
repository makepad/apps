// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.cpp (change detection: lines 2500-2633,
// 5929-6000).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! The change-detection snapshot of everything the relaxer reads, the global input hash, and the
//! per-trace frozen mask.
//!
//! ADAPTED: the global hash mixes the port's clearance table in place of the reference's
//! design-rule floats it does not have (diff-pair spacing, SMD clearance), and the snapshots carry
//! each item's rule row. Copper shapes, courtyards, bundles and routing intents are absent from
//! the port's board, so their parts of the hash are left out (the reference mixes them only when
//! present or on).

use super::{SnapPd, SnapPt, SnapTr, SnapVi, Snapshot};
use crate::organic::geom::Vec2;
use crate::organic::model::{OBoard, Pad, Trace, TracePoint, Via};
use crate::organic::settings::{relax_immutable, OrganicSettings};

/// `fbits`.
#[inline]
pub fn fbits(f: f32) -> u32 {
    f.to_bits()
}

/// `fromBits`.
#[inline]
pub fn from_bits(x: u32, y: u32) -> Vec2 {
    Vec2::new(f32::from_bits(x), f32::from_bits(y))
}

pub fn snap_pt(p: &TracePoint) -> SnapPt {
    SnapPt {
        x: fbits(p.pos.x),
        y: fbits(p.pos.y),
        term: p.terminal_pad,
        via: p.attached_via,
        br_t: p.branch_trace,
        br_i: p.branch_point,
        fixed: p.is_fixed as u8,
        straight: p.straight_before as u8,
    }
}

pub fn snap_via(v: &Via) -> SnapVi {
    SnapVi {
        x: fbits(v.pos.x),
        y: fbits(v.pos.y),
        dia: fbits(v.diameter),
        drill: fbits(v.drill),
        id: v.id,
        net: v.net,
        pad: v.connected_pad,
        l0: v.layer_start,
        l1: v.layer_end,
        fixed: v.is_fixed as u8,
        rule: v.rule,
    }
}

pub fn snap_pad(p: &Pad) -> SnapPd {
    SnapPd {
        cx: fbits(p.center.x),
        cy: fbits(p.center.y),
        sx: fbits(p.size.x),
        sy: fbits(p.size.y),
        drill: fbits(p.drill_diameter),
        rot: fbits(p.rotation),
        lx: fbits(p.local_size.x),
        ly: fbits(p.local_size.y),
        rr: fbits(p.round_rect_ratio),
        dx: fbits(p.drill_size.x),
        dy: fbits(p.drill_size.y),
        shape: p.shape as i32,
        net: p.net,
        mask: p.layer_mask,
        smd: p.is_smd as u8,
        rule: p.rule,
    }
}

/// `sameVia` (field-wise).
#[inline]
pub fn same_via(a: &SnapVi, b: &SnapVi) -> bool {
    a == b
}

/// `samePad` (field-wise).
#[inline]
pub fn same_pad(a: &SnapPd, b: &SnapPd) -> bool {
    a == b
}

/// `samePt`.
#[inline]
pub fn same_pt(a: &SnapPt, b: &SnapPt) -> bool {
    a == b
}

/// `globalHash`: every input that changes the physics globally; a change wakes the whole board.
pub fn global_hash(b: &OBoard, s: &OrganicSettings) -> u64 {
    let mut h: u64 = 1469598103934665603;
    let mut mix_u = |v: u64| h = (h ^ v).wrapping_mul(1099511628211);
    let fb = |f: f32| fbits(f) as u64;
    mix_u(b.outline.len() as u64);
    for p in &b.outline {
        mix_u(fb(p.x));
        mix_u(fb(p.y));
    }
    mix_u(b.cutouts.len() as u64);
    for c in &b.cutouts {
        for p in c {
            mix_u(fb(p.x));
            mix_u(fb(p.y));
        }
    }
    mix_u(b.layers.len() as u64);
    for l in &b.layers {
        mix_u(l.intent as u64);
    }
    // copper shapes: absent from the port's board (the reference mixes their count, 0)
    mix_u(0);
    mix_u(b.keepouts.len() as u64);
    for k in &b.keepouts {
        mix_u(k.layer_mask);
        mix_u(fb(k.clearance));
        mix_u(k.no_tracks as u64);
        mix_u(k.no_vias as u64);
        mix_u(k.poly.len() as u64);
        for p in &k.poly {
            mix_u(fb(p.x));
            mix_u(fb(p.y));
        }
    }
    let r = &b.rules;
    for f in [
        r.default_trace_width,
        r.min_trace_width,
        r.min_clearance,
        r.via_diameter,
        r.via_drill,
        r.board_edge_clearance,
        r.hole_clearance,
        r.hole_to_hole,
    ] {
        mix_u(fb(f));
    }
    // ADAPTED: the clearance table stands for the per-pair rules
    mix_u(b.clr.rows as u64);
    mix_u(b.clr.layers as u64);
    for &v in &b.clr.mm {
        mix_u(fb(v));
    }
    mix_u(b.nets.len() as u64);
    for n in &b.nets {
        mix_u(n.id as i64 as u64);
        mix_u(n.diff_pair_partner as i64 as u64);
        mix_u(n.is_diff_pair as u64);
        mix_u(fb(n.target_length));
    }
    for f in [
        s.tension,
        s.contraction_tension,
        s.straightening_force,
        s.pad_repulsion,
        s.trace_repulsion,
        s.clearance_margin,
        s.dt,
        s.diff_pair_distance,
        s.diff_pair_coupling_strength,
        s.meander_frequency,
        s.meander_amplitude,
        s.length_matching_tolerance,
        s.medial_axis_centering_strength,
        s.max_node_step,
        s.over_relaxation,
        s.node_spacing,
        s.guard_clearance_slack,
        s.diff_pair_skew_tolerance,
        s.diff_pair_skew_amplitude,
        s.layer_crossing_perpendicularity,
        s.crossing_disentanglement,
        s.via_via_repulsion,
    ] {
        mix_u(fb(f));
    }
    for v in [
        s.enable_diff_pair_coupling,
        s.enable_length_tuning,
        s.enable_dynamic_vias,
        s.enable_target_length_matching,
        s.enable_medial_axis_centering,
        s.enable_loop_simplification,
        s.enable_drc_guard,
        s.diff_pair_rigid,
        s.enable_diff_pair_skew_tuning,
    ] {
        mix_u(v as u64);
    }
    mix_u(s.substeps_per_frame as i64 as u64);
    mix_u(fb(s.diff_pair_partner_margin));
    mix_u(s.diff_pair_crinkle_coupled as u64);
    mix_u(fb(s.diff_pair_crinkle_max_loss));
    if s.diff_pair_crinkle_segment_guard {
        mix_u(1);
    }
    mix_u(s.diff_pair_pin_approach as u64);
    if s.wide_track_straightening != 0.0 || s.wide_track_snap_deg != 0.0 || s.wide_track_line_pull != 0.0 {
        for f in [s.wide_track_straightening, s.wide_track_ratio, s.wide_track_snap_deg, s.wide_track_line_pull, s.wide_track_deviation] {
            mix_u(fb(f));
        }
    }
    if s.pad_exit_half_plane != 0.0 {
        mix_u(fb(s.pad_exit_half_plane));
        mix_u(fb(s.pad_exit_max_turn_deg));
    }
    if s.hop_keep_dist != 0.0 {
        mix_u(fb(s.hop_keep_dist));
        mix_u(fb(s.hop_limit));
        mix_u(fb(s.hop_keep_weight));
    }
    if s.via_courtyard_guard {
        // courtyards: absent from the port's board
        mix_u(0x5a5b);
    }
    if s.bundle_keep != 0.0 {
        // bundles: absent from the port's board
        mix_u(fb(s.bundle_keep));
    }
    // routing intents (RoutingIntents::relaxKeyFor): none in the port's board, so nothing mixed
    if s.straight_pass_relief != 0.0 || s.straight_bend != 0.0 {
        for f in [s.straight_pass_relief, s.straight_bend, s.straight_taper, s.straight_min_len, s.bend_over_relaxation] {
            mix_u(fb(f));
        }
        if s.wide_bend_settle {
            mix_u(1);
        }
    }
    if s.wire_spread != 0.0 {
        for f in [
            s.wire_spread,
            s.wire_spread_reach,
            s.wire_spread_link,
            s.wire_spread_smooth,
            s.wire_spread_slope,
            s.wire_spread_max_shift,
            s.wire_spread_taper,
            s.wire_spread_pitch,
            s.wire_spread_lone_reach,
        ] {
            mix_u(fb(f));
        }
        mix_u(s.wire_spread_every as i64 as u64);
        mix_u(s.wire_spread_start as i64 as u64);
        mix_u(s.wire_spread_updates as i64 as u64);
    }
    h
}

/// `takeSnapshot`.
pub fn take_snapshot(sn: &mut Snapshot, b: &OBoard, gh: u64) {
    sn.global = gh;
    sn.tr.resize(b.traces.len(), SnapTr::default());
    let np: usize = b.traces.iter().map(|t| t.points.len()).sum();
    sn.pt.resize(np, SnapPt::default());
    let mut off = 0usize;
    for (t, tr) in b.traces.iter().enumerate() {
        sn.tr[t] = SnapTr {
            id: tr.id,
            net: tr.net,
            layer: tr.layer,
            width: fbits(tr.width),
            routed: tr.is_routed as i32,
            off: off as i32,
            n: tr.points.len() as i32,
            rule: tr.rule,
        };
        for p in &tr.points {
            sn.pt[off] = snap_pt(p);
            off += 1;
        }
    }
    sn.vi.resize(b.vias.len(), SnapVi::default());
    for (v, via) in b.vias.iter().enumerate() {
        sn.vi[v] = snap_via(via);
    }
    sn.pd.resize(b.pads.len(), SnapPd::default());
    for (p, pad) in b.pads.iter().enumerate() {
        sn.pd[p] = snap_pad(pad);
    }
}

/// `traceMetaHash`: the trace-level inputs of the spline rebuild (everything but the nodes).
pub fn trace_meta_hash(tr: &Trace) -> u64 {
    let mut h: u64 = 1469598103934665603;
    let mut mix_u = |v: u64| h = (h ^ v).wrapping_mul(1099511628211);
    mix_u(tr.id as i64 as u64);
    mix_u(tr.net as i64 as u64);
    mix_u(tr.layer as u64);
    mix_u(fbits(tr.width) as u64);
    mix_u(tr.is_routed as u64);
    mix_u(tr.locked as u64);
    mix_u(tr.imported as u64);
    h
}

/// `differsFromSnapshot`: does the board differ from the snapshot in anything the relaxer reads?
pub fn differs_from_snapshot(sn: &Snapshot, b: &OBoard, gh: u64) -> bool {
    if gh != sn.global || b.traces.len() != sn.tr.len() || b.vias.len() != sn.vi.len() || b.pads.len() != sn.pd.len() {
        return true;
    }
    for (t, tr) in b.traces.iter().enumerate() {
        let o = &sn.tr[t];
        if o.id != tr.id
            || o.net != tr.net
            || o.layer != tr.layer
            || o.width != fbits(tr.width)
            || o.routed != tr.is_routed as i32
            || o.n != tr.points.len() as i32
            || o.rule != tr.rule
        {
            return true;
        }
        let op = &sn.pt[o.off as usize..];
        for k in 0..o.n as usize {
            let p = &tr.points[k];
            if op[k].x != fbits(p.pos.x) || op[k].y != fbits(p.pos.y) || !op[k].same_flags(p) {
                return true;
            }
        }
    }
    for (v, via) in b.vias.iter().enumerate() {
        if !same_via(&snap_via(via), &sn.vi[v]) {
            return true;
        }
    }
    for (p, pad) in b.pads.iter().enumerate() {
        if !same_pad(&snap_pad(pad), &sn.pd[p]) {
            return true;
        }
    }
    false
}

/// `frozenMaskOf`: per trace, 1 when it is immutable (locked / kept imported) or on a frozen net.
pub fn frozen_mask_of(b: &OBoard, frozen_net_ids: &[i32], s: &OrganicSettings) -> Vec<u8> {
    let mut f = vec![0u8; b.traces.len()];
    for (t, tr) in b.traces.iter().enumerate() {
        if relax_immutable(tr, s) || (!frozen_net_ids.is_empty() && frozen_net_ids.contains(&tr.net)) {
            f[t] = 1;
        }
    }
    f
}

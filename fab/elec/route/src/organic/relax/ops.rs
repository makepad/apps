// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.cpp (board operations of the step:
// lines 2003-2304, 2322-2325; TraceEdit::straightSegment from src/core/TraceEdit.cpp 441-444).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Board operations the step runs before and after the sweeps: via ids, branch references,
//! duplicate nodes and paired pad approaches. The reference's id lookups (`unordered_map`,
//! lookups only) are `BTreeMap`s.

use super::nodes::is_pinned;
use crate::organic::copper::{proj_seg, seg_seg};
use crate::organic::geom::{std_min, Vec2};
use crate::organic::model::{OBoard, TracePoint};
use crate::organic::settings::{relax_immutable, OrganicSettings};
use std::collections::BTreeMap;

/// `TraceEdit::straightSegment`: segment i (nodes i, i + 1) is a fixed straight stub.
#[inline]
pub fn straight_segment(pts: &[TracePoint], i: i32) -> bool {
    i >= 0 && i + 1 < pts.len() as i32 && pts[(i + 1) as usize].straight_before
}

/// `normalizeVias`: via ids become their indices (attachments follow), and a free node attached
/// to a via sits on it.
pub fn normalize_vias(b: &mut OBoard) {
    let mismatch = b.vias.iter().enumerate().any(|(i, v)| v.id != i as i32);
    if mismatch {
        let mut id_to_index: BTreeMap<i32, i32> = BTreeMap::new();
        for (i, v) in b.vias.iter().enumerate() {
            id_to_index.entry(v.id).or_insert(i as i32);
        }
        for tr in &mut b.traces {
            for p in &mut tr.points {
                if p.attached_via >= 0 {
                    p.attached_via = id_to_index.get(&p.attached_via).copied().unwrap_or(-1);
                }
            }
        }
        for (i, v) in b.vias.iter_mut().enumerate() {
            v.id = i as i32;
        }
    }
    let nv = b.vias.len() as i32;
    for t in 0..b.traces.len() {
        for k in 0..b.traces[t].points.len() {
            let p = &mut b.traces[t].points[k];
            if p.attached_via >= nv {
                p.attached_via = -1;
            }
            if p.attached_via >= 0 && !p.is_fixed && p.terminal_pad < 0 {
                p.pos = b.vias[p.attached_via as usize].pos;
            }
        }
    }
}

/// `pinPairApproaches` (OrganicRelaxer.cpp:2003-2183): keep paired pad approaches and
/// short pad-to-pad segments that are part of a longer pair (H6b/H6d).
/// ADAPTED: Board::diffPairGap is the adapter-resolved Net::pair_gap; debug prints omitted.
pub fn pin_pair_approaches(b: &mut OBoard, s: &OrganicSettings) -> i32 {
    if !s.enable_diff_pair_coupling || !s.diff_pair_pin_approach { return 0; }
    const K_REACH: f32 = 2.5;
    const K_RUN: f32 = 0.6;
    const K_HOLD: f32 = 1.2;
    const K_PAIR_ENDS: f32 = 3.0;
    struct End {
        t: usize,
        e: i32,
        dir: i32,
        c: i32,
        net: i32,
        at: Vec2,
        ok: bool,
    }
    let mut ends = Vec::new();
    for (ti, tr) in b.traces.iter().enumerate() {
        if tr.net < 0 || tr.net >= b.nets.len() as i32 || relax_immutable(tr, s) { continue; }
        let net = &b.nets[tr.net as usize];
        let q = net.diff_pair_partner;
        if !net.is_diff_pair || q < 0 || q >= b.nets.len() as i32 || q == tr.net { continue; }
        let n = tr.points.len() as i32;
        if n < 3 { continue; }
        let g = if net.pair_gap > 0.0f32 { net.pair_gap } else { b.rules.diff_pair_spacing };
        if !(g > 0.0f32) { continue; }
        let edge_gap = |x: Vec2| {
            let mut best = 1e9;
            for o in &b.traces {
                if o.net != q || o.layer != tr.layer { continue; }
                for k in 0..o.points.len().saturating_sub(1) {
                    let a = o.points[k].pos;
                    let z = o.points[k + 1].pos;
                    let (_, qx, qy) = proj_seg(x.x as f64, x.y as f64, a.x as f64, a.y as f64, z.x as f64, z.y as f64);
                    let dx = qx - x.x as f64;
                    let dy = qy - x.y as f64;
                    best = std_min(best, (dx * dx + dy * dy).sqrt() - 0.5 * (tr.width + o.width) as f64);
                }
            }
            best
        };
        let coupled = |x: Vec2| {
            let eg = edge_gap(x);
            eg >= 0.8 * g as f64 && eg <= 1.2 * g as f64
        };
        for end in 0..2 {
            let e = if end == 0 { 0 } else { n - 1 };
            let dir = if end == 0 { 1 } else { -1 };
            if tr.points[e as usize].terminal_pad < 0 { continue; }
            let mut arc = 0.0f32;
            let mut c = -1;
            let run_coupled = |k0: i32| {
                let mut run = 0.0f32;
                let mut k = k0;
                while k > 0 && k < n - 1 {
                    if k != k0 { run += tr.points[k as usize].pos.distance_to(tr.points[(k - dir) as usize].pos); }
                    if !coupled(tr.points[k as usize].pos) { return false; }
                    if run >= K_RUN { return true; }
                    k += dir;
                }
                true // coupled up to the other end
            };
            let mut k = e + dir;
            while k > 0 && k < n - 1 {
                arc += tr.points[k as usize].pos.distance_to(tr.points[(k - dir) as usize].pos);
                if arc > K_REACH { break; }
                if coupled(tr.points[k as usize].pos) && run_coupled(k) { c = k; break; }
                k += dir;
            }
            if c < 0 {
                ends.push(End { t: ti, e, dir, c: -1, net: tr.net, at: tr.points[e as usize].pos, ok: false });
                continue;
            }
            // Keep at least K_HOLD from the pad and diff_pair_pin_past beyond the first
            // coupled node. The reference measures through c + dir before advancing c.
            let c0 = c;
            while c + dir > 0 && c + dir < n - 1 {
                let mut a2 = 0.0f32;
                let mut k = e + dir;
                while k != c + dir + dir {
                    a2 += tr.points[k as usize].pos.distance_to(tr.points[(k - dir) as usize].pos);
                    k += dir;
                }
                let mut past = 0.0f32;
                let mut k = c0 + dir;
                while k != c + dir + dir {
                    past += tr.points[k as usize].pos.distance_to(tr.points[(k - dir) as usize].pos);
                    k += dir;
                }
                if a2 > K_HOLD && past > s.diff_pair_pin_past { break; }
                c += dir;
            }
            let mut spike = false;
            let mut k = e + dir;
            while k != c + dir && !spike {
                if k > 0 && k < n - 1 {
                    if edge_gap(tr.points[k as usize].pos) < 0.8 * g as f64 { spike = true; break; }
                    let u = tr.points[k as usize].pos - tr.points[(k - 1) as usize].pos;
                    let v = tr.points[(k + 1) as usize].pos - tr.points[k as usize].pos;
                    let lu = u.length();
                    let lv = v.length();
                    spike = lu > 1e-6f32 && lv > 1e-6f32 && u.dot(v) < -0.7071f32 * lu * lv;
                }
                k += dir;
            }
            ends.push(End { t: ti, e, dir, c, net: tr.net, at: tr.points[e as usize].pos, ok: !spike });
        }
    }
    // ADAPTED: the two identical nearest-mate searches share a closure, preserving end order.
    let nets = &b.nets;
    let has_mate = |en: &End| {
        let q = nets[en.net as usize].diff_pair_partner;
        let mut mate = None;
        let mut md = K_PAIR_ENDS;
        for o in &ends {
            if o.net == q && o.ok && o.at.distance_to(en.at) < md {
                md = o.at.distance_to(en.at);
                mate = Some(o);
            }
        }
        mate.is_some_and(|o| o.ok)
    };
    let mut pinned = 0;
    for en in &ends {
        if !en.ok || !has_mate(en) { continue; }
        let tr = &mut b.traces[en.t];
        let mut k = en.e + en.dir;
        while k != en.c + en.dir {
            let p = &mut tr.points[k as usize];
            if !p.is_fixed && p.attached_via < 0 && p.branch_trace < 0 {
                p.is_fixed = true;
                pinned += 1;
            }
            k += en.dir;
        }
    }
    if s.diff_pair_pin_short > 0.0f32 {
        let kept_end = |t: usize, e: i32| {
            for en in &ends {
                if en.t == t && en.e == e { return en.ok && has_mate(en); }
            }
            false
        };
        for ti in 0..b.traces.len() {
            let tr = &b.traces[ti];
            let n = tr.points.len() as i32;
            if n < 3 || tr.points[0].terminal_pad < 0 || tr.points[(n - 1) as usize].terminal_pad < 0 { continue; }
            let mut l = 0.0f32;
            for k in 0..n - 1 { l += tr.points[k as usize].pos.distance_to(tr.points[(k + 1) as usize].pos); }
            if l > s.diff_pair_pin_short || !kept_end(ti, 0) || !kept_end(ti, n - 1) { continue; }
            // Leave a free stretch for skew tuning when this is the pair's only trace.
            let mut more = false;
            for o in 0..b.traces.len() {
                if more { break; }
                more = o != ti && b.traces[o].net == tr.net && b.traces[o].points.len() >= 2;
            }
            if !more { continue; }
            for k in 1..n - 1 {
                let p = &mut b.traces[ti].points[k as usize];
                if !p.is_fixed && p.attached_via < 0 && p.branch_trace < 0 {
                    p.is_fixed = true;
                    pinned += 1;
                }
            }
        }
    }
    pinned
}

/// `remapBranches`: branch references to trace `trace_id` follow its rebuilt points.
pub fn remap_branches(b: &mut OBoard, trace_id: i32, old_to_new: &[i32]) {
    for tr in &mut b.traces {
        for p in &mut tr.points {
            if p.branch_trace == trace_id && p.branch_point >= 0 {
                let i = p.branch_point as usize;
                if i < old_to_new.len() && old_to_new[i] >= 0 {
                    p.branch_point = old_to_new[i];
                }
            }
        }
    }
}

/// `removeDuplicateNodes`: consecutive duplicate nodes (zero-length segments) are removed;
/// geometry is unchanged. A special node (pinned, a stub end, a junction host, an end) replaces
/// a plain duplicate in its slot.
pub fn remove_duplicate_nodes(b: &mut OBoard, branch_target: &[Vec<u8>], frozen: &[u8]) {
    for t in 0..b.traces.len() {
        let pts = &b.traces[t].points;
        if pts.len() < 3 || (t < frozen.len() && frozen[t] != 0) {
            continue;
        }
        let mut any = false;
        for k in 0..pts.len() - 1 {
            if pts[k].pos.distance_squared_to(pts[k + 1].pos) < 1e-12f32 {
                any = true;
                break;
            }
        }
        if !any {
            continue;
        }
        let mut out: Vec<TracePoint> = Vec::new();
        let mut out_special: Vec<u8> = Vec::new();
        let mut map = vec![-1i32; pts.len()];
        for k in 0..pts.len() {
            let special = is_pinned(&pts[k])
                || pts[k].straight_before
                || (k < branch_target[t].len() && branch_target[t][k] != 0)
                || k == 0
                || k + 1 == pts.len();
            if let Some(last) = out.last() {
                if last.pos.distance_squared_to(pts[k].pos) < 1e-12f32 {
                    if !special {
                        map[k] = out.len() as i32 - 1;
                        continue;
                    }
                    if *out_special.last().unwrap() == 0 {
                        *out.last_mut().unwrap() = pts[k].clone();
                        *out_special.last_mut().unwrap() = 1;
                        map[k] = out.len() as i32 - 1;
                        continue;
                    }
                }
            }
            map[k] = out.len() as i32;
            out.push(pts[k].clone());
            out_special.push(special as u8);
        }
        if out.len() < 2 || out.len() == pts.len() {
            continue;
        }
        b.traces[t].points = out;
        let id = b.traces[t].id;
        remap_branches(b, id, &map);
    }
}

/// `computeBranchTargets`: per trace node, 1 when a branch end follows it.
pub fn compute_branch_targets(b: &OBoard) -> Vec<Vec<u8>> {
    let mut bt: Vec<Vec<u8>> = Vec::with_capacity(b.traces.len());
    let mut id_to_idx: BTreeMap<i32, usize> = BTreeMap::new();
    for (t, tr) in b.traces.iter().enumerate() {
        bt.push(vec![0; tr.points.len()]);
        id_to_idx.insert(tr.id, t);
    }
    for tr in &b.traces {
        for p in &tr.points {
            if p.branch_trace >= 0 {
                if let Some(&h) = id_to_idx.get(&p.branch_trace) {
                    if p.branch_point >= 0 && (p.branch_point as usize) < bt[h].len() {
                        bt[h][p.branch_point as usize] = 1;
                    }
                }
            }
        }
    }
    bt
}

/// `syncBranches`: every branch end sits on its host node (in trace and node order, reading the
/// host as it stands at that moment, as the reference).
pub fn sync_branches(b: &mut OBoard) {
    let mut trace_map: BTreeMap<i32, usize> = BTreeMap::new();
    for (t, tr) in b.traces.iter().enumerate() {
        trace_map.insert(tr.id, t);
    }
    for t in 0..b.traces.len() {
        for k in 0..b.traces[t].points.len() {
            let pt = &b.traces[t].points[k];
            if pt.branch_trace < 0 {
                continue;
            }
            let Some(&h) = trace_map.get(&pt.branch_trace) else { continue };
            let bp = pt.branch_point;
            if bp >= 0 && (bp as usize) < b.traces[h].points.len() {
                let pos = b.traces[h].points[bp as usize].pos;
                let pt = &mut b.traces[t].points[k];
                pt.pos = pos;
                pt.velocity = Vec2::new(0.0, 0.0);
            }
        }
    }
}

/// `segsTouch`.
#[inline]
pub fn segs_touch(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> bool {
    seg_seg(a, b, c, d).d < 1e-7
}

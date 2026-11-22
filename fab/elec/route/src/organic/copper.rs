// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/CopperGeometry.h.
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Copper geometry and connectivity shared by the relaxer and net graph simplifier.
//! Traces are capsules, pads are rounded boxes, vias are discs, and planes join their net.
//!
//! ADAPTED: pad masks use copper stack indices, planes use stored net membership, and
//! pad pieces are joined during attachments.

use super::geom::{std_clamp, std_max, std_min, Vec2};
use super::model::{LayerIntent, OBoard, Pad, Via};
use super::padgeom;

pub const K_BIG: f64 = 1e30;

#[derive(Copy, Clone, Debug)]
pub struct Closest {
    pub d: f64,
    /// Parameter on the first segment.
    pub t: f64,
    /// Point on first minus point on second.
    pub vx: f64,
    pub vy: f64,
}

impl Default for Closest {
    fn default() -> Self { Self { d: K_BIG, t: 0.0, vx: 0.0, vy: 0.0 } }
}

pub fn proj_seg(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> (f64, f64, f64) {
    let dx = bx - ax;
    let dy = by - ay;
    let l2 = dx * dx + dy * dy;
    let mut t = if l2 > 1e-24 { ((px - ax) * dx + (py - ay) * dy) / l2 } else { 0.0 };
    t = if t < 0.0 { 0.0 } else if t > 1.0 { 1.0 } else { t };
    let qx = ax + dx * t;
    let qy = ay + dy * t;
    (t, qx, qy)
}

/// Proper crossings use orientations; otherwise take the four endpoint-to-segment minima.
pub fn seg_seg_xy(ax: f64, ay: f64, bx: f64, by: f64, cx: f64, cy: f64, dx: f64, dy: f64) -> Closest {
    let mut r = Closest::default();
    let rx = bx - ax;
    let ry = by - ay;
    let sx = dx - cx;
    let sy = dy - cy;
    let d1 = sx * (ay - cy) - sy * (ax - cx);
    let d2 = sx * (by - cy) - sy * (bx - cx);
    let d3 = rx * (cy - ay) - ry * (cx - ax);
    let d4 = rx * (dy - ay) - ry * (dx - ax);
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0)) && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0)) {
        r.d = 0.0;
        r.t = d1 / (d1 - d2);
        return r;
    }
    let rr = rx * rx + ry * ry;
    let ss = sx * sx + sy * sy;
    let irr = if rr > 1e-24 { 1.0 / rr } else { 0.0 };
    let iss = if ss > 1e-24 { 1.0 / ss } else { 0.0 };
    let clamp01 = |v: f64| if v < 0.0 { 0.0 } else if v > 1.0 { 1.0 } else { v };
    // A vs CD.
    let mut u = clamp01(((ax - cx) * sx + (ay - cy) * sy) * iss);
    let mut ex = ax - (cx + sx * u);
    let mut ey = ay - (cy + sy * u);
    let mut best = ex * ex + ey * ey;
    r.t = 0.0;
    r.vx = ex;
    r.vy = ey;
    // B vs CD.
    u = clamp01(((bx - cx) * sx + (by - cy) * sy) * iss);
    ex = bx - (cx + sx * u);
    ey = by - (cy + sy * u);
    let mut d2v = ex * ex + ey * ey;
    if d2v < best { best = d2v; r.t = 1.0; r.vx = ex; r.vy = ey; }
    // C vs AB.
    u = clamp01(((cx - ax) * rx + (cy - ay) * ry) * irr);
    ex = (ax + rx * u) - cx;
    ey = (ay + ry * u) - cy;
    d2v = ex * ex + ey * ey;
    if d2v < best { best = d2v; r.t = u; r.vx = ex; r.vy = ey; }
    // D vs AB.
    u = clamp01(((dx - ax) * rx + (dy - ay) * ry) * irr);
    ex = (ax + rx * u) - dx;
    ey = (ay + ry * u) - dy;
    d2v = ex * ex + ey * ey;
    if d2v < best { best = d2v; r.t = u; r.vx = ex; r.vy = ey; }
    r.d = best.sqrt();
    r
}

pub fn seg_seg(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> Closest {
    seg_seg_xy(a.x as f64, a.y as f64, b.x as f64, b.y as f64, c.x as f64, c.y as f64, d.x as f64, d.y as f64)
}

/// Segment to axis-aligned box core around c, with half extents hx and hy.
pub fn seg_box(a: Vec2, b: Vec2, c: Vec2, hx: f64, hy: f64) -> Closest {
    if hx <= 0.0 && hy <= 0.0 {
        return seg_seg_xy(a.x as f64, a.y as f64, b.x as f64, b.y as f64, c.x as f64, c.y as f64, c.x as f64, c.y as f64);
    }
    if hx <= 0.0 || hy <= 0.0 {
        return seg_seg_xy(a.x as f64, a.y as f64, b.x as f64, b.y as f64, c.x as f64 - hx, c.y as f64 - hy, c.x as f64 + hx, c.y as f64 + hy);
    }
    let mut best = Closest::default();
    let mut point_box = |p: Vec2, t: f64| {
        let qx = std_clamp(p.x as f64, c.x as f64 - hx, c.x as f64 + hx);
        let qy = std_clamp(p.y as f64, c.y as f64 - hy, c.y as f64 + hy);
        let ex = p.x as f64 - qx;
        let ey = p.y as f64 - qy;
        let d = (ex * ex + ey * ey).sqrt();
        if d < best.d { best.d = d; best.t = t; best.vx = ex; best.vy = ey; }
    };
    point_box(a, 0.0);
    point_box(b, 1.0);
    if best.d <= 0.0 { return best; }
    let x0 = c.x as f64 - hx;
    let x1 = c.x as f64 + hx;
    let y0 = c.y as f64 - hy;
    let y1 = c.y as f64 + hy;
    let ex = [x0, x1, x1, x0, x0];
    let ey = [y0, y0, y1, y1, y0];
    for i in 0..4 {
        let e = seg_seg_xy(a.x as f64, a.y as f64, b.x as f64, b.y as f64, ex[i], ey[i], ex[i + 1], ey[i + 1]);
        if e.d < best.d { best = e; }
    }
    best
}

/// Rotated inner box grown by r, plus a drill capsule. Polygon centre is its box centre.
#[derive(Clone, Debug)]
pub struct PadModel {
    pub center: Vec2,
    pub c: f64,
    pub s: f64,
    pub hx: f64,
    pub hy: f64,
    pub r: f64,
    /// Half side of a board-frame square containing the copper.
    pub ext: f64,
    pub hole: bool,
    /// Drill axis; coincident endpoints describe a round hole.
    pub ha: Vec2,
    pub hb: Vec2,
    pub hr: f64,
}

impl Default for PadModel {
    fn default() -> Self {
        Self { center: Vec2::default(), c: 1.0, s: 0.0, hx: 0.0, hy: 0.0, r: 0.0, ext: 0.0, hole: false, ha: Vec2::default(), hb: Vec2::default(), hr: 0.0 }
    }
}

impl PadModel {
    pub fn rotated(&self) -> bool { self.s != 0.0 || self.c != 1.0 }
}

pub fn pad_model(p: &Pad) -> PadModel {
    let mut m = PadModel::default();
    let f = padgeom::frame(p);
    m.center = Vec2::new(f.cx as f32, f.cy as f32);
    m.c = f.c;
    m.s = f.s;
    m.r = f.r;
    m.hx = std_max(0.0, f.hx - f.r);
    m.hy = std_max(0.0, f.hy - f.r);
    m.ext = (if m.rotated() { (m.hx * m.hx + m.hy * m.hy).sqrt() } else { std_max(m.hx, m.hy) }) + m.r;
    if let Some(h) = padgeom::hole(p) {
        m.hole = true;
        m.ha = h.a;
        m.hb = h.b;
        m.hr = h.r as f64;
    }
    m
}

/// Segment to pad inner box: subtract m.r for copper gap; vector is in the board frame.
pub fn seg_pad(a: Vec2, b: Vec2, m: &PadModel) -> Closest {
    if !m.rotated() { return seg_box(a, b, m.center, m.hx, m.hy); }
    let loc = |q: Vec2| {
        let dx = (q.x - m.center.x) as f64;
        let dy = (q.y - m.center.y) as f64;
        Vec2::new((dx * m.c - dy * m.s) as f32, (dx * m.s + dy * m.c) as f32)
    };
    let mut e = seg_box(loc(a), loc(b), Vec2::new(0.0_f32, 0.0_f32), m.hx, m.hy);
    let vx = e.vx * m.c + e.vy * m.s;
    let vy = -e.vx * m.s + e.vy * m.c;
    e.vx = vx;
    e.vy = vy;
    e
}

/// Segment to drill axis; subtract m.hr for distance to the wall.
pub fn seg_hole(a: Vec2, b: Vec2, m: &PadModel) -> Closest { seg_seg(a, b, m.ha, m.hb) }

/// ADAPTED: layer_mask stores copper stack-index bits for every pad kind.
pub fn pad_on_layer(pad: &Pad, layer: i32, _n_layers: i32) -> bool {
    layer >= 0 && layer < 64 && pad.layer_mask & (1_u64 << layer) != 0
}

/// Copper gap of segment ab (half width h) to pad; negative means overlap.
pub fn seg_pad_gap(a: Vec2, b: Vec2, h: f64, pad: &Pad) -> f64 {
    let m = pad_model(pad);
    seg_pad(a, b, &m).d - m.r - h
}

// Object keys use trace, via and pad indices in separate ranges.
pub const K_VIA_BASE: i64 = 1_i64 << 32;
pub const K_PAD_BASE: i64 = 2_i64 << 32;
pub fn trace_key(t: i32) -> i64 { t as i64 }
pub fn via_key(v: i32) -> i64 { K_VIA_BASE + v as i64 }
pub fn pad_key(p: i32) -> i64 { K_PAD_BASE + p as i64 }

pub fn pair_key(mut a: i64, mut b: i64) -> u64 {
    if a > b { std::mem::swap(&mut a, &mut b); }
    (a as u64).wrapping_mul(0x9E3779B97F4A7C15_u64)
        ^ (b as u64).wrapping_add(0x632BE59BD9B4E019_u64).wrapping_add((a as u64) << 7)
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Contact {
    pub a: i64,
    pub b: i64,
}

pub fn via_on_layer(v: &Via, layer: i32) -> bool {
    let a = std_min(v.layer_start, v.layer_end);
    let b = std_max(v.layer_start, v.layer_end);
    layer >= a && layer <= b
}

/// Plane layers owned by net, joining its pads and spanning vias.
pub fn plane_layers_of(board: &OBoard, net: i32, out: &mut Vec<i32>) {
    out.clear();
    if net < 0 || net >= board.nets.len() as i32 { return; }
    // ADAPTED: plane ownership is data, with no net-name heuristic.
    for l in 0..board.layers.len() as i32 {
        let layer = &board.layers[l as usize];
        if (layer.intent == LayerIntent::GroundPlane || layer.intent == LayerIntent::PowerPlane) && layer.plane_net == net {
            out.push(l);
        }
    }
}

/// Disconnected pad groups minus one, using exporter/oracle contacts with tolerance tol.
/// Attachments come first; essential appends geometric merges in deterministic order.
pub fn net_pad_groups(board: &OBoard, net: i32, mut essential: Option<&mut Vec<Contact>>, tol: f64) -> i32 {
    let n_l = std_max(1, board.layers.len() as i32);
    let (mut pads, mut vias, mut trs) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..board.pads.len() {
        if board.pads[i].net == net { pads.push(i as i32); }
    }
    if pads.len() < 2 { return 0; }
    for i in 0..board.vias.len() {
        if board.vias[i].net == net { vias.push(i as i32); }
    }
    for i in 0..board.traces.len() {
        if board.traces[i].net == net && board.traces[i].is_routed && board.traces[i].points.len() >= 2 {
            trs.push(i as i32);
        }
    }
    let n_p = pads.len() as i32;
    let n_v = vias.len() as i32;
    let n_t = trs.len() as i32;
    let mut par: Vec<i32> = (0..n_p + n_v + n_t).collect();
    // The reference's local helpers take par explicitly to keep mutable borrows disjoint.
    fn find(par: &mut [i32], mut x: i32) -> i32 {
        while par[x as usize] != x {
            par[x as usize] = par[par[x as usize] as usize];
            x = par[x as usize];
        }
        x
    }
    fn unite(par: &mut [i32], a: i32, b: i32) -> bool {
        let a = find(par, a);
        let b = find(par, b);
        if a == b { return false; }
        par[a as usize] = b;
        true
    }
    let p = |k: i32| k;
    let v = |k: i32| n_p + k;
    let t = |k: i32| n_p + n_v + k;
    let pad_slot = |pad_idx: i32| {
        for k in 0..n_p { if pads[k as usize] == pad_idx { return k; } }
        -1
    };
    let via_slot = |via_idx: i32| {
        for k in 0..n_v { if vias[k as usize] == via_idx { return k; } }
        -1
    };
    let trace_slot_by_id = |trace_id: i32| {
        for k in 0..n_t { if board.traces[trs[k as usize] as usize].id == trace_id { return k; } }
        -1
    };
    let groups = |par: &mut [i32]| {
        let mut roots = Vec::new();
        for k in 0..n_p { roots.push(find(par, p(k))); }
        roots.sort();
        roots.dedup();
        roots.len() as i32
    };

    // 1. Attachments, exact by construction.
    // ADAPTED: pieces referring to another pad slot of this net are one pad.
    for pi in 0..n_p {
        let s = pad_slot(board.pads[pads[pi as usize] as usize].piece_of);
        if s >= 0 { unite(&mut par, p(pi), p(s)); }
    }
    for ti in 0..n_t {
        let tr = &board.traces[trs[ti as usize] as usize];
        for k in 0..tr.points.len() {
            let point = &tr.points[k];
            if (k == 0 || k + 1 == tr.points.len()) && point.terminal_pad >= 0 && point.terminal_pad < board.pads.len() as i32 {
                let pad = &board.pads[point.terminal_pad as usize];
                let s = pad_slot(point.terminal_pad);
                if s >= 0 && pad_on_layer(pad, tr.layer, n_l) && seg_pad_gap(point.pos, point.pos, tr.width as f64 * 0.5, pad) <= tol {
                    unite(&mut par, t(ti), p(s));
                }
            }
            if point.attached_via >= 0 && point.attached_via < board.vias.len() as i32 {
                let s = via_slot(point.attached_via);
                if s >= 0 && point.pos.distance_to(board.vias[point.attached_via as usize].pos) as f64 <= tol {
                    unite(&mut par, t(ti), v(s));
                }
            }
            if point.branch_trace >= 0 {
                let s = trace_slot_by_id(point.branch_trace);
                if s >= 0 { unite(&mut par, t(ti), t(s)); }
            }
        }
    }
    // 1b. Plane zones of this net, using modelled via spans and pad layers.
    {
        let mut planes = Vec::new();
        plane_layers_of(board, net, &mut planes);
        for l in planes {
            let mut anchor = -1;
            let mut join = |node: i32| {
                if anchor < 0 { anchor = node; } else { unite(&mut par, anchor, node); }
            };
            for k in 0..n_p {
                if pad_on_layer(&board.pads[pads[k as usize] as usize], l, n_l) { join(p(k)); }
            }
            for k in 0..n_v {
                if via_on_layer(&board.vias[vias[k as usize] as usize], l) { join(v(k)); }
            }
        }
    }
    if groups(&mut par) <= 1 { return 0; }

    // 2. Geometric contacts, only between different components.
    #[derive(Copy, Clone, Default)]
    struct Tb {
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        _hw: f64, // The reference stores hw but never reads it.
    }
    let mut tb = vec![Tb::default(); n_t as usize];
    for ti in 0..n_t {
        let tr = &board.traces[trs[ti as usize] as usize];
        let mut b = Tb { x0: 1e300, y0: 1e300, x1: -1e300, y1: -1e300, _hw: tr.width as f64 * 0.5 };
        for point in &tr.points {
            b.x0 = std_min(b.x0, point.pos.x as f64);
            b.x1 = std_max(b.x1, point.pos.x as f64);
            b.y0 = std_min(b.y0, point.pos.y as f64);
            b.y1 = std_max(b.y1, point.pos.y as f64);
        }
        tb[ti as usize] = b;
    }
    let near = |b: &Tb, x: f64, y: f64, r: f64| {
        x >= b.x0 - r && x <= b.x1 + r && y >= b.y0 - r && y <= b.y1 + r
    };
    let mut record = |par: &mut [i32], a: i32, b: i32, ka: i64, kb: i64| {
        if unite(par, a, b) {
            if let Some(out) = essential.as_deref_mut() { out.push(Contact { a: ka, b: kb }); }
        }
    };
    let pad_ext = |pad: &Pad| std_max(pad.size.x, pad.size.y) as f64 * 0.5;

    for ti in 0..n_t {
        let tr = &board.traces[trs[ti as usize] as usize];
        let h = tr.width as f64 * 0.5;
        // Trace - pad.
        for pi in 0..n_p {
            if find(&mut par, t(ti)) == find(&mut par, p(pi)) { continue; }
            let pad = &board.pads[pads[pi as usize] as usize];
            if !pad_on_layer(pad, tr.layer, n_l) || !near(&tb[ti as usize], pad.center.x as f64, pad.center.y as f64, pad_ext(pad) + h + tol) { continue; }
            for k in 0..tr.points.len() - 1 {
                if seg_pad_gap(tr.points[k].pos, tr.points[k + 1].pos, h, pad) <= tol {
                    record(&mut par, t(ti), p(pi), trace_key(trs[ti as usize]), pad_key(pads[pi as usize]));
                    break;
                }
            }
        }
        // Trace - via.
        for vi in 0..n_v {
            if find(&mut par, t(ti)) == find(&mut par, v(vi)) { continue; }
            let via = &board.vias[vias[vi as usize] as usize];
            let rv = via.diameter as f64 * 0.5;
            if !near(&tb[ti as usize], via.pos.x as f64, via.pos.y as f64, rv + h + tol) { continue; }
            for k in 0..tr.points.len() - 1 {
                if seg_seg(tr.points[k].pos, tr.points[k + 1].pos, via.pos, via.pos).d <= h + rv + tol {
                    record(&mut par, t(ti), v(vi), trace_key(trs[ti as usize]), via_key(vias[vi as usize]));
                    break;
                }
            }
        }
        // Trace - trace, on the same layer.
        for tj in ti + 1..n_t {
            if find(&mut par, t(ti)) == find(&mut par, t(tj)) { continue; }
            let t2 = &board.traces[trs[tj as usize] as usize];
            if t2.layer != tr.layer { continue; }
            let r = h + t2.width as f64 * 0.5 + tol;
            if tb[ti as usize].x0 - r > tb[tj as usize].x1 || tb[tj as usize].x0 - r > tb[ti as usize].x1
                || tb[ti as usize].y0 - r > tb[tj as usize].y1 || tb[tj as usize].y0 - r > tb[ti as usize].y1 { continue; }
            let mut touch = false;
            let mut a = 0;
            while a + 1 < tr.points.len() && !touch {
                let pa = tr.points[a].pos;
                let pb = tr.points[a + 1].pos;
                if !near(&tb[tj as usize], std_min(pa.x, pb.x) as f64, std_min(pa.y, pb.y) as f64,
                    r + (pa.x - pb.x).abs() as f64 + (pa.y - pb.y).abs() as f64) {
                    a += 1;
                    continue;
                }
                for b in 0..t2.points.len() - 1 {
                    if seg_seg(pa, pb, t2.points[b].pos, t2.points[b + 1].pos).d <= r { touch = true; break; }
                }
                a += 1;
            }
            if touch { record(&mut par, t(ti), t(tj), trace_key(trs[ti as usize]), trace_key(trs[tj as usize])); }
        }
    }
    for vi in 0..n_v {
        let via = &board.vias[vias[vi as usize] as usize];
        let rv = via.diameter as f64 * 0.5;
        for pi in 0..n_p {
            if find(&mut par, v(vi)) == find(&mut par, p(pi)) { continue; }
            let pad = &board.pads[pads[pi as usize] as usize];
            if seg_pad_gap(via.pos, via.pos, rv, pad) <= tol {
                record(&mut par, v(vi), p(pi), via_key(vias[vi as usize]), pad_key(pads[pi as usize]));
            }
        }
        for vj in vi + 1..n_v {
            if find(&mut par, v(vi)) == find(&mut par, v(vj)) { continue; }
            let w = &board.vias[vias[vj as usize] as usize];
            if via.pos.distance_to(w.pos) as f64 <= rv + w.diameter as f64 * 0.5 + tol {
                record(&mut par, v(vi), v(vj), via_key(vias[vi as usize]), via_key(vias[vj as usize]));
            }
        }
    }
    groups(&mut par) - 1
}

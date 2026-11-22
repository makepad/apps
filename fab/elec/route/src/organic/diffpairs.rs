// Ported from TiNRS Psychedelic Router 0.1.0: src/core/DiffPairs.h (87-93, 121-130), DiffPairs.cpp (242-260, 279-344, 401-437).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Sampled coupled length and pair membership, shared by the relaxer and finish passes.
//!
//! ADAPTED: member_gap reads the adapter-resolved Net::pair_gap in place of
//! Board::diffPairGap. C++ default arguments are explicit. netSegs (cpp:262-277)
//! is omitted: coupledLength constructs its segments from CouplingPoly instead.
//! Determinism: coupling's unordered_map becomes BTreeMap<i64, Vec<i32>>.
//! The map is only inserted into and looked up, never iterated; each cell's
//! vector is visited in insertion order, preserving the first coupled hit.

use super::geom::{std_clamp, std_max, std_min, Vec2};
use super::model::OBoard;
use std::collections::BTreeMap;

/// DiffPairs.h:87-93: one polyline of copper (layer index and track width).
#[derive(Clone, Debug, Default)]
pub struct CouplingPoly {
    pub pts: Vec<Vec2>,
    pub layer: i32,
    pub width: f32,
}

// DiffPairs.cpp:244-250.
struct MSeg {
    a: Vec2,
    b: Vec2,
    w: f32,
    layer: i32,
    trace: i32,
}

// DiffPairs.cpp:252-260. Coordinate differences are float before promotion.
fn pt_seg(p: &Vec2, a: &Vec2, b: &Vec2) -> f64 {
    let dx = (b.x - a.x) as f64;
    let dy = (b.y - a.y) as f64;
    let l2 = dx * dx + dy * dy;
    let mut t = if l2 > 0.0 { ((p.x - a.x) as f64 * dx + (p.y - a.y) as f64 * dy) / l2 } else { 0.0 };
    t = std_clamp(t, 0.0, 1.0);
    let qx = a.x as f64 + t * dx - p.x as f64;
    let qy = a.y as f64 + t * dy - p.y as f64;
    (qx * qx + qy * qy).sqrt()
}

// DiffPairs.cpp:279-343: coupled / total length and longest uncoupled run.
fn coupling(a: &[MSeg], b: &[MSeg], gap: f64, total: &mut f64, coupled: &mut f64, max_run: &mut f64) {
    *total = 0.0;
    *coupled = 0.0;
    *max_run = 0.0;
    let step = 0.05f64;
    let cos_tol = (15.0f64 * 3.14159265358979 / 180.0).cos();
    let cs = 1.0f64;
    // Lookup/append only, never map iteration; cell vectors retain b's order.
    let mut grid: BTreeMap<i64, Vec<i32>> = BTreeMap::new();
    let key = |x: i64, y: i64| (x << 32) ^ (y & 0xffffffffi64);
    for j in 0..b.len() {
        let s = &b[j];
        let x0 = (std_min(s.a.x, s.b.x) as f64 / cs).floor() as i64;
        let x1 = (std_max(s.a.x, s.b.x) as f64 / cs).floor() as i64;
        let y0 = (std_min(s.a.y, s.b.y) as f64 / cs).floor() as i64;
        let y1 = (std_max(s.a.y, s.b.y) as f64 / cs).floor() as i64;
        for y in y0..=y1 {
            for x in x0..=x1 {
                grid.entry(key(x, y)).or_default().push(j as i32);
            }
        }
    }
    let mut cur_trace = -1;
    let mut run = 0.0;
    let mut last_end = Vec2::new(1e30f32, 1e30f32);
    for s in a {
        if s.trace != cur_trace || s.a.distance_squared_to(last_end) > 1e-8f32 { run = 0.0; }
        cur_trace = s.trace;
        last_end = s.b;
        let l = s.a.distance_to(s.b) as f64;
        let n = std_max(1, (l / step).ceil() as i32);
        let dl = l / n as f64;
        let ux = (s.b.x - s.a.x) as f64 / l;
        let uy = (s.b.y - s.a.y) as f64 / l;
        for i in 0..n {
            let t = (i as f64 + 0.5) / n as f64;
            let p = Vec2::new(
                (s.a.x as f64 + (s.b.x - s.a.x) as f64 * t) as f32,
                (s.a.y as f64 + (s.b.y - s.a.y) as f64 * t) as f32,
            );
            let mut ok = false;
            let cx = (p.x as f64 / cs).floor() as i64;
            let cy = (p.y as f64 / cs).floor() as i64;
            for y in cy - 1..=cy + 1 {
                if ok { break; }
                for x in cx - 1..=cx + 1 {
                    if ok { break; }
                    let Some(cell) = grid.get(&key(x, y)) else { continue; };
                    for &j in cell {
                        let q = &b[j as usize];
                        if q.layer != s.layer { continue; }
                        let lq = q.a.distance_to(q.b) as f64;
                        let vx = (q.b.x - q.a.x) as f64 / lq;
                        let vy = (q.b.y - q.a.y) as f64 / lq;
                        if (ux * vx + uy * vy).abs() < cos_tol { continue; }
                        let eg = pt_seg(&p, &q.a, &q.b) - 0.5 * (s.w + q.w) as f64;
                        if (eg - gap).abs() <= 0.2 * gap {
                            ok = true;
                            break;
                        }
                    }
                }
            }
            *total += dl;
            if ok {
                *coupled += dl;
                run = 0.0;
            } else {
                run += dl;
                *max_run = std_max(*max_run, run);
            }
        }
    }
}

/// DiffPairs.cpp:401-421, DiffPairs.h:122-125: coupled mm of a against b;
/// also b against a when both is true (the C++ default).
pub fn coupled_length(a: &[CouplingPoly], b: &[CouplingPoly], gap: f32, both: bool) -> f64 {
    let segs = |ps: &[CouplingPoly]| {
        let mut out = Vec::new();
        for t in 0..ps.len() {
            for i in 0..ps[t].pts.len().saturating_sub(1) {
                if ps[t].pts[i].distance_squared_to(ps[t].pts[i + 1]) > 1e-12f32 {
                    out.push(MSeg { a: ps[t].pts[i], b: ps[t].pts[i + 1], w: ps[t].width, layer: ps[t].layer, trace: t as i32 });
                }
            }
        }
        out
    };
    let a = segs(a);
    let b = segs(b);
    if a.is_empty() || b.is_empty() || !(gap > 0.0f32) { return 0.0; }
    let (mut tot, mut cpl, mut run, mut sum) = (0.0, 0.0, 0.0, 0.0);
    coupling(&a, &b, gap as f64, &mut tot, &mut cpl, &mut run);
    sum += cpl;
    if both {
        coupling(&b, &a, gap as f64, &mut tot, &mut cpl, &mut run);
        sum += cpl;
    }
    sum
}

/// DiffPairs.cpp:423-430, DiffPairs.h:129-130: partner net, or -1.
pub fn partner_of(board: &OBoard, net: i32) -> i32 {
    if net < 0 || net >= board.nets.len() as i32 { return -1; }
    let n = &board.nets[net as usize];
    let q = n.diff_pair_partner;
    if !n.is_diff_pair || q < 0 || q >= board.nets.len() as i32 || q == net { return -1; }
    q
}

/// DiffPairs.cpp:432-437, DiffPairs.h:126-128: resolved pair gap, or 0 for a nonmember.
pub fn member_gap(board: &OBoard, net: i32) -> f32 {
    if partner_of(board, net) < 0 { return 0.0f32; }
    // ADAPTED: Board.cpp:236-247 is resolved by the adapter, not queried here.
    let g = board.nets[net as usize].pair_gap;
    if g > 0.0f32 { g } else { board.rules.diff_pair_spacing }
}

// Ported from TiNRS Psychedelic Router 0.1.0: src/optimize/ViaHops.cpp (1841-2076), ViaHops.h (229-245).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in the parent folder.

//! Count real and pseudo via hops on rounded export primitives, without changing the board.
//!
//! ADAPTED: nodes also retain their copper stack index for copper::pad_on_layer /
//! via_on_layer. Pads still attach only on the outer layers, as in the reference.
//! The smoothed path uses ArcFit's default options with no injected DRC counter:
//! its fitting and junction breaks run, but its DRC refit/fallback guard does not.
//! C++ default arguments are explicit; optional output vectors are cleared on entry.
//!
//! Determinism: index, pads_of_net, vias_of_net, seen and visited use BTreeMap /
//! BTreeSet (the reference already uses std::map / std::set here). None is iterated:
//! only the per-net pad/via vectors are iterated, in board order. Nodes, adjacency,
//! primitives and the ends_on stack keep insertion order; no hash order is involved.

use super::super::arcfit::{self, ArcFitOptions, TrackStyle};
use super::super::copper::{pad_on_layer, via_on_layer};
use super::super::geom::{std_max, std_min, Vec2};
use super::super::model::{OBoard, Pad, PadShape};
use std::collections::{BTreeMap, BTreeSet};

/// ViaHops.h:229-233: real hops and pass-over (pseudo) hops counted separately.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct HopCount {
    pub real: i32,
    pub pseudo: i32,
}

impl HopCount {
    pub fn all(&self) -> i32 {
        self.real + self.pseudo
    }
}

/// ViaHops.h:234-239: one counted chain, with its ordered board via indices.
#[derive(Copy, Clone, Debug)]
pub struct CountedHop {
    pub via_a: i32,
    pub via_b: i32,
    /// Midpoint of the two vias, in mm.
    pub at: Vec2,
    pub pseudo: bool,
}

impl Default for CountedHop {
    fn default() -> Self {
        Self { via_a: -1, via_b: -1, at: Vec2::default(), pseudo: false }
    }
}

// ViaHops.cpp:1844-1869: exported start/mid/end, chorded in 10-degree steps
// (at least two pieces), after rounding the three points to four decimals.
fn chord_arc(p0: &[f64; 2], p1: &[f64; 2], p2: &[f64; 2], out: &mut Vec<[f64; 2]>) {
    out.clear();
    let (ax, ay, bx, by, cx, cy) = (p0[0], p0[1], p1[0], p1[1], p2[0], p2[1]);
    let d = 2.0 * (ax * (by - cy) + bx * (cy - ay) + cx * (ay - by));
    if d.abs() < 1e-12 {
        out.push([ax, ay]);
        out.push([cx, cy]);
        return;
    }
    let ux = ((ax * ax + ay * ay) * (by - cy) + (bx * bx + by * by) * (cy - ay) + (cx * cx + cy * cy) * (ay - by)) / d;
    let uy = ((ax * ax + ay * ay) * (cx - bx) + (bx * bx + by * by) * (ax - cx) + (cx * cx + cy * cy) * (bx - ax)) / d;
    let r = (ax - ux).hypot(ay - uy);
    let a0 = (ay - uy).atan2(ax - ux);
    let a1 = (by - uy).atan2(bx - ux);
    let a2 = (cy - uy).atan2(cx - ux);
    let pi: f64 = 3.14159265358979323846;
    let tau = 2.0 * pi;
    let norm = |a: f64| {
        // Rust remainder is C++ fmod; add tau only for a negative remainder.
        let mut m = a % tau;
        if m < 0.0 { m += tau; }
        m
    };
    let ccw = norm(a1 - a0) < norm(a2 - a0);
    let sweep = if ccw { norm(a2 - a0) } else { -norm(a0 - a2) };
    let n = std_max(2, ((sweep * 180.0 / pi).abs() / 10.0) as i32 + 1);
    for i in 0..=n {
        out.push([
            ux + r * (a0 + sweep * i as f64 / n as f64).cos(),
            uy + r * (a0 + sweep * i as f64 / n as f64).sin(),
        ]);
    }
}

// ViaHops.cpp:1871-1875: KicadExporter num(), four decimals.
fn r4(v: f32) -> f64 {
    if v.abs() < 5e-5f32 { return 0.0; }
    // std::round rounds halfway away from zero, independent of the current
    // floating-point rounding mode, as does f64::round.
    (v as f64 * 1e4).round() / 1e4
}

/// ViaHops.cpp:1878-2076: chains between attached or non-degree-two graph nodes.
///
/// The C++ defaults are max_hop_length = 3.0, use_smoothed = true, where_ = None,
/// detail = None and via_centre_length = false. With via_centre_length, include
/// the distances from both chain ends to their via centres in the length test.
pub fn count_hops(
    board: &OBoard,
    max_hop_length: f32,
    use_smoothed: bool,
    mut where_: Option<&mut Vec<Vec2>>,
    mut detail: Option<&mut Vec<CountedHop>>,
    via_centre_length: bool,
) -> HopCount {
    if let Some(out) = where_.as_deref_mut() { out.clear(); }
    if let Some(out) = detail.as_deref_mut() { out.clear(); }
    let n_cu = std_max(2, board.layers.len() as i32);
    let prims = if use_smoothed {
        // ADAPTED: the ArcFit port requires explicit options, stats and counter.
        arcfit::board_primitives(board, TrackStyle::Arcs, &ArcFitOptions::default(), None, None)
    } else {
        let mut prims = vec![Vec::new(); board.traces.len()];
        for t in 0..board.traces.len() {
            if !board.traces[t].is_routed { continue; }
            let mut raw = board.traces[t].clone();
            raw.smoothed.clear();
            raw.point_widths.clear();
            prims[t] = arcfit::segments_of(&raw);
        }
        prims
    };
    struct Node {
        net: i32,
        // Layer class: 0 front, 1 inner, 2 back.
        layer: i32,
        // ADAPTED: the actual copper stack index for layer membership.
        layer_index: i32,
        x: f64,
        y: f64,
        hw: f64,
        adj: Vec<i32>,
        // -1 none, -2 pad, >= 0 board via index.
        att: i32,
    }
    let mut nodes: Vec<Node> = Vec::new();
    // (net, layer class, inner index, qx, qy); lookup/insert only, never iterated.
    let mut index: BTreeMap<(i32, i32, i32, i64, i64), i32> = BTreeMap::new();
    // Explicit nodes argument replaces the C++ lambda's mutable capture.
    let mut node_of = |nodes: &mut Vec<Node>, net: i32, li: i32, x: f64, y: f64, hw: f64| {
        let lc = if li <= 0 { 0 } else if li >= n_cu - 1 { 2 } else { 1 };
        // std::llround is also halfway away from zero, independent of the
        // rounding mode: f64::round then as i64, for representable board coords.
        let key = (net, lc, if lc == 1 { li } else { 0 }, (x * 10000.0).round() as i64, (y * 10000.0).round() as i64);
        let id;
        if let Some(&existing) = index.get(&key) {
            id = existing;
        } else {
            id = nodes.len() as i32;
            index.insert(key, id);
            nodes.push(Node { net, layer: lc, layer_index: li, x, y, hw: 0.0, adj: Vec::new(), att: -1 });
        }
        nodes[id as usize].hw = std_max(nodes[id as usize].hw, hw);
        id
    };
    let mut poly = Vec::new();
    for t in 0..std_min(board.traces.len(), prims.len()) {
        let tr = &board.traces[t];
        if !tr.is_routed || tr.net < 0 { continue; }
        for p in &prims[t] {
            if p.a.distance_squared_to(p.b) < 1e-8f32 { continue; }
            let w = if p.width > 0.0f32 { p.width } else { tr.width };
            let hw = r4(w) / 2.0;
            let a = [r4(p.a.x), r4(p.a.y)];
            let b = [r4(p.b.x), r4(p.b.y)];
            poly.clear();
            if p.is_arc {
                let m = [r4(p.mid.x), r4(p.mid.y)];
                chord_arc(&a, &m, &b, &mut poly);
            } else {
                poly.push([a[0], a[1]]);
                poly.push([b[0], b[1]]);
            }
            for k in 0..poly.len() - 1 {
                let u = node_of(&mut nodes, tr.net, tr.layer, poly[k][0], poly[k][1], hw);
                let v = node_of(&mut nodes, tr.net, tr.layer, poly[k + 1][0], poly[k + 1][1], hw);
                if u == v { continue; }
                nodes[u as usize].adj.push(v);
                nodes[v as usize].adj.push(u);
            }
        }
    }
    // Attachment: pad geometry first, then via discs. Neither map is iterated;
    // the per-net vectors below preserve board order and first-match precedence.
    let mut pads_of_net: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    let mut vias_of_net: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    for i in 0..board.pads.len() {
        if board.pads[i].net >= 0 { pads_of_net.entry(board.pads[i].net).or_default().push(i as i32); }
    }
    for i in 0..board.vias.len() {
        if board.vias[i].net >= 0 { vias_of_net.entry(board.vias[i].net).or_default().push(i as i32); }
    }
    let in_pad = |p: &Pad, layer_class: i32, li: i32, x: f64, y: f64, tol: f64| {
        if layer_class == 1 { return false; }
        // ADAPTED: copper stack masks replace KiCad's SMD/front/back mask test.
        if !pad_on_layer(p, li, n_cu) { return false; }
        let have_local = p.local_size.x > 1e-4f32 && p.local_size.y > 1e-4f32;
        let mut size = if have_local { p.local_size } else { p.size };
        let mut rot = if have_local { ((p.rotation as f64 % 360.0) + 360.0) % 360.0 } else { 0.0 };
        if rot > 359.9995 { rot = 0.0; }
        if p.shape == PadShape::Polygon && p.polygon.len() >= 3 { size = Vec2::new(0.01f32, 0.01f32); }
        let dx = x - p.center.x as f64;
        let dy = y - p.center.y as f64;
        if p.shape == PadShape::Circle { return dx.hypot(dy) <= std_max(r4(size.x), r4(size.y)) / 2.0 + tol; }
        let a = rot * 3.14159265358979323846 / 180.0;
        let lx = dx * a.cos() - dy * a.sin();
        let ly = dx * a.sin() + dy * a.cos();
        lx.abs() <= r4(size.x) / 2.0 + tol && ly.abs() <= r4(size.y) / 2.0 + tol
    };
    for n in &mut nodes {
        for &pi in pads_of_net.entry(n.net).or_default().iter() {
            if in_pad(&board.pads[pi as usize], n.layer, n.layer_index, n.x, n.y, n.hw) {
                n.att = -2;
                break;
            }
        }
        if n.att != -1 { continue; }
        for &vi in vias_of_net.entry(n.net).or_default().iter() {
            let v = &board.vias[vi as usize];
            // ADAPTED: unlike the reference's all-layer discs, respect via spans.
            if !via_on_layer(v, n.layer_index) { continue; }
            if (n.x - r4(v.pos.x)).hypot(n.y - r4(v.pos.y)) <= r4(v.diameter) / 2.0 + n.hw {
                n.att = vi;
                break;
            }
        }
    }
    // Chains between breakers (degree != 2 or attached). seen is membership-only.
    let breaker = |k: i32| nodes[k as usize].adj.len() != 2 || nodes[k as usize].att != -1;
    let mut seen: BTreeSet<(i32, i32)> = BTreeSet::new();
    let mut hops = HopCount::default();
    for s in 0..nodes.len() as i32 {
        if !breaker(s) { continue; }
        for ai in 0..nodes[s as usize].adj.len() {
            let nb = nodes[s as usize].adj[ai];
            if seen.contains(&(s, nb)) { continue; }
            seen.insert((s, nb));
            seen.insert((nb, s));
            let mut prev = s;
            let mut cur = nb;
            let mut l = (nodes[cur as usize].x - nodes[s as usize].x).hypot(nodes[cur as usize].y - nodes[s as usize].y);
            while !breaker(cur) {
                let mut nxt = -1;
                for xi in 0..nodes[cur as usize].adj.len() {
                    if !seen.contains(&(cur, nodes[cur as usize].adj[xi])) {
                        nxt = nodes[cur as usize].adj[xi];
                        break;
                    }
                }
                if nxt < 0 { break; }
                seen.insert((cur, nxt));
                seen.insert((nxt, cur));
                l += (nodes[nxt as usize].x - nodes[cur as usize].x).hypot(nodes[nxt as usize].y - nodes[cur as usize].y);
                prev = cur;
                cur = nxt;
            }
            let a = nodes[s as usize].att;
            let b = nodes[cur as usize].att;
            if via_centre_length && a >= 0 && b >= 0 && a != b {
                l += (nodes[s as usize].x - r4(board.vias[a as usize].pos.x)).hypot(nodes[s as usize].y - r4(board.vias[a as usize].pos.y))
                    + (nodes[cur as usize].x - r4(board.vias[b as usize].pos.x)).hypot(nodes[cur as usize].y - r4(board.vias[b as usize].pos.y));
            }
            if a >= 0 && b >= 0 && a != b && l < max_hop_length as f64 {
                // Walk away from this chain through the via's attached nodes:
                // a centre, track end or junction makes it real; only leaving
                // the disc makes it pseudo. visited is membership-only.
                let ends_on = |end: i32, inward: i32, via: i32| {
                    let vx = r4(board.vias[via as usize].pos.x);
                    let vy = r4(board.vias[via as usize].pos.y);
                    let mut stack = vec![end];
                    let mut visited = BTreeSet::from([end, inward]);
                    while let Some(nd) = stack.pop() {
                        if nodes[nd as usize].adj.len() != 2 || (nodes[nd as usize].x - vx).hypot(nodes[nd as usize].y - vy) < 1e-3 {
                            return true;
                        }
                        for &x in &nodes[nd as usize].adj {
                            if !visited.contains(&x) && nodes[x as usize].att == via {
                                visited.insert(x);
                                stack.push(x);
                            }
                        }
                    }
                    false
                };
                let pseudo = !ends_on(s, nb, a) || !ends_on(cur, prev, b);
                if pseudo { hops.pseudo += 1; } else { hops.real += 1; }
                if let Some(out) = where_.as_deref_mut() {
                    out.push((board.vias[a as usize].pos + board.vias[b as usize].pos) * 0.5f32);
                }
                if let Some(out) = detail.as_deref_mut() {
                    let h = CountedHop {
                        via_a: std_min(a, b),
                        via_b: std_max(a, b),
                        at: (board.vias[a as usize].pos + board.vias[b as usize].pos) * 0.5f32,
                        pseudo,
                    };
                    out.push(h);
                }
            }
        }
    }
    hops
}

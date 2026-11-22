// Ported from TiNRS Psychedelic Router 0.1.0: src/topology/NetGraphSimplifier.cpp (with TraceEdit::reversePoints,
// Drc::planeLayerOfNet, Board::layerIndexForMask).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Split net crossings, remove graph cycles, rebuild chains, and prune dangling vias.

use super::copper;
use super::geom::{std_clamp, std_max, std_min, Segment2, Vec2};
use super::model::{LayerIntent, OBoard, Trace, TracePoint};
use super::relax::ops::straight_segment;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

// NetGraphSimplifier.cpp:21-43.
struct Dsu {
    parent: Vec<i32>,
}

impl Dsu {
    fn new(n: i32) -> Self {
        Self { parent: (0..n).collect() }
    }

    fn find(&mut self, i: i32) -> i32 {
        if self.parent[i as usize] == i {
            i
        } else {
            let root = self.find(self.parent[i as usize]);
            self.parent[i as usize] = root;
            root
        }
    }

    fn unite(&mut self, i: i32, j: i32) -> bool {
        let root_i = self.find(i);
        let root_j = self.find(j);
        if root_i != root_j {
            self.parent[root_i as usize] = root_j;
            return true;
        }
        false
    }
}

// NetGraphSimplifier.cpp:45-56. The comparator is used at the split sort below.
struct SplitMarker {
    segment_index: i32,
    param_t: f32,
    pos: Vec2,
}

// NetGraphSimplifier.cpp:58-73. The stable edge sort compares length only.
#[derive(Clone)]
struct GraphEdge {
    u: i32,
    v: i32,
    length: f32,
    layer_index: i32,
    width: f32,
    orig_trace_id: i32,
    points: Vec<TracePoint>,
    rule: u16,
}

const K_STUB_JUNCTION_SNAP: f32 = 0.05;

// NetGraphSimplifier.cpp:82-87.
fn stub_end_node(pts: &[TracePoint], seg: i32, hit: Vec2) -> i32 {
    let s = seg as usize;
    if pts[s].terminal_pad >= 0 && pts[s + 1].terminal_pad < 0 {
        return seg + 1;
    }
    if pts[s + 1].terminal_pad >= 0 && pts[s].terminal_pad < 0 {
        return seg;
    }
    if hit.distance_to(pts[s].pos) < hit.distance_to(pts[s + 1].pos) {
        seg
    } else {
        seg + 1
    }
}

// NetGraphSimplifier.cpp:90. Rust's set needs Ord: use the C++ tuple's lexicographic
// comparisons, including equal signed zeros, rather than f32::total_cmp. As in the
// C++ ordered set, coordinates must have a strict weak order (no NaNs).
#[derive(Clone, Copy)]
struct SegKey(i32, f32, f32, f32, f32);

impl Ord for SegKey {
    fn cmp(&self, other: &Self) -> Ordering {
        let layer = self.0.cmp(&other.0);
        if layer != Ordering::Equal {
            return layer;
        }
        for (a, b) in [(self.1, other.1), (self.2, other.2), (self.3, other.3), (self.4, other.4)] {
            if a < b {
                return Ordering::Less;
            }
            if b < a {
                return Ordering::Greater;
            }
        }
        Ordering::Equal
    }
}

impl PartialOrd for SegKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for SegKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for SegKey {}

fn seg_key(layer: i32, a: Vec2, b: Vec2) -> SegKey {
    if b.x < a.x || (b.x == a.x && b.y < a.y) {
        return SegKey(layer, b.x, b.y, a.x, a.y);
    }
    SegKey(layer, a.x, a.y, b.x, b.y)
}

fn net_segments(board: &OBoard, net_id: i32) -> BTreeSet<SegKey> {
    let mut s = BTreeSet::new();
    for tr in &board.traces {
        if tr.net == net_id {
            for k in 0..tr.points.len().saturating_sub(1) {
                s.insert(seg_key(tr.layer, tr.points[k].pos, tr.points[k + 1].pos));
            }
        }
    }
    s
}

// NetGraphSimplifier.cpp:109-153. Check only copper newly introduced by vertex snapping.
fn new_segment_too_close(board: &OBoard, net_id: i32, old_segs: &BTreeSet<SegKey>) -> bool {
    let n_l = std_max(1, board.layers.len() as i32);
    const K_TOL: f32 = 1e-4;
    for tr in &board.traces {
        if tr.net != net_id {
            continue;
        }
        let h: f32 = 0.5 * tr.width;
        for k in 0..tr.points.len().saturating_sub(1) {
            let a = tr.points[k].pos;
            let b = tr.points[k + 1].pos;
            if old_segs.contains(&seg_key(tr.layer, a, b)) {
                continue;
            }
            let x0 = std_min(a.x, b.x);
            let x1 = std_max(a.x, b.x);
            let y0 = std_min(a.y, b.y);
            let y1 = std_max(a.y, b.y);
            let far = |c: Vec2, reach: f32| {
                c.x < x0 - reach || c.x > x1 + reach || c.y < y0 - reach || c.y > y1 + reach
            };
            for pad in &board.pads {
                if pad.net == net_id || !copper::pad_on_layer(pad, tr.layer, n_l) {
                    continue;
                }
                // ADAPTED: NetGraphSimplifier.cpp:126 uses trace/pad rule rows on the trace layer; get already returns f32.
                let need: f32 = board.clr.get(tr.rule, pad.rule, tr.layer);
                if far(pad.center, 0.5 * std_max(pad.size.x, pad.size.y) + h + need + 0.5) {
                    continue;
                }
                if copper::seg_pad_gap(a, b, h as f64, pad) < (need - K_TOL) as f64 {
                    return true;
                }
            }
            for v in &board.vias {
                // ADAPTED: NetGraphSimplifier.cpp:132 (also 379, 737): Drc::viaOnLayer is copper::via_on_layer.
                if v.net == net_id || !copper::via_on_layer(v, tr.layer) {
                    continue;
                }
                // ADAPTED: NetGraphSimplifier.cpp:133 uses trace/via rule rows on the trace layer.
                let need: f32 = board.clr.get(tr.rule, v.rule, tr.layer);
                if far(v.pos, 0.5 * v.diameter + h + need) {
                    continue;
                }
                if copper::seg_seg(a, b, v.pos, v.pos).d - (0.5f32 * v.diameter) as f64 - (h as f64)
                    < (need - K_TOL) as f64
                {
                    return true;
                }
            }
            for o in &board.traces {
                if o.net == net_id || o.layer != tr.layer {
                    continue;
                }
                // ADAPTED: NetGraphSimplifier.cpp:140 uses trace/trace rule rows on the trace layer.
                let need: f32 = board.clr.get(tr.rule, o.rule, tr.layer);
                let reach: f32 = h + 0.5 * o.width + need;
                for j in 0..o.points.len().saturating_sub(1) {
                    let c = o.points[j].pos;
                    let d = o.points[j + 1].pos;
                    if std_max(c.x, d.x) < x0 - reach
                        || std_min(c.x, d.x) > x1 + reach
                        || std_max(c.y, d.y) < y0 - reach
                        || std_min(c.y, d.y) > y1 + reach
                    {
                        continue;
                    }
                    if copper::seg_seg(a, b, c, d).d - (h as f64) - ((0.5f32 * o.width) as f64)
                        < (need - K_TOL) as f64
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// `TraceEdit.cpp:446-454`: reversal carries each straight flag with its segment.
pub fn reverse_points(pts: &mut [TracePoint]) {
    let n = pts.len() as i32;
    let mut flag = vec![0u8; n as usize];
    for k in 0..n {
        flag[k as usize] = pts[k as usize].straight_before as u8;
    }
    pts.reverse();
    for j in 0..n {
        pts[j as usize].straight_before = j >= 1 && flag[(n - j) as usize] != 0;
    }
}

// Drc.cpp:418-424.
/// `Board::layerIndexForMask` (Board.cpp:193-198): the first board layer whose bit is in `mask`,
/// -1 for none.
fn layer_index_for_mask(board: &OBoard, mask: u64) -> i32 {
    for i in 0..board.layers.len() {
        if i < 64 && mask & (1u64 << i) != 0 {
            return i as i32;
        }
    }
    -1
}

fn plane_layer_of_net(board: &OBoard, net: i32) -> i32 {
    if net < 0 {
        return -1;
    }
    for l in 0..board.layers.len() as i32 {
        let layer = &board.layers[l as usize];
        // ADAPTED: Drc.cpp:422 planeNet is plane_net for GroundPlane/PowerPlane intent, otherwise -1.
        let plane_net = if layer.intent == LayerIntent::GroundPlane || layer.intent == LayerIntent::PowerPlane {
            layer.plane_net
        } else {
            -1
        };
        if plane_net == net {
            return l;
        }
    }
    -1
}

/// `NetGraphSimplifier.cpp:157-212`: interior crossings, excluding shared ends.
pub fn has_self_intersections(net_id: i32, board: &OBoard) -> bool {
    let mut net_traces = Vec::new();
    for tr in &board.traces {
        if tr.net == net_id && tr.points.len() >= 2 {
            net_traces.push(tr);
        }
    }
    if net_traces.is_empty() {
        return false;
    }
    for t_a in 0..net_traces.len() {
        let tr_a = net_traces[t_a];
        for i in 0..tr_a.points.len() - 1 {
            let s_a = Segment2::new(tr_a.points[i].pos, tr_a.points[i + 1].pos);
            for t_b in t_a..net_traces.len() {
                let tr_b = net_traces[t_b];
                if tr_a.layer != tr_b.layer {
                    continue;
                }
                let start_j = if t_a == t_b { i + 2 } else { 0 };
                for j in start_j..tr_b.points.len() - 1 {
                    let s_b = Segment2::new(tr_b.points[j].pos, tr_b.points[j + 1].pos);
                    if s_a.p0.distance_to(s_b.p0) < 0.1
                        || s_a.p0.distance_to(s_b.p1) < 0.1
                        || s_a.p1.distance_to(s_b.p0) < 0.1
                        || s_a.p1.distance_to(s_b.p1) < 0.1
                    {
                        continue;
                    }
                    if let Some(hit) = s_a.intersects(&s_b) {
                        let min_end_dist = std_min(
                            std_min(std_min(hit.distance_to(s_a.p0), hit.distance_to(s_a.p1)), hit.distance_to(s_b.p0)),
                            hit.distance_to(s_b.p1),
                        );
                        if min_end_dist > 0.03 {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// `NetGraphSimplifier.cpp:214-807`: accept the rebuilt tree only if its copper remains valid.
pub fn simplify_net(net_id: i32, board: &mut OBoard) -> bool {
    if net_id < 0 || net_id >= board.nets.len() as i32 {
        return false;
    }
    // ADAPTED: NetGraphSimplifier.cpp:217-218: Net has no padIds; count the net's pads, each once
    // (the extra copper pieces of one pad link to their first piece through `piece_of`).
    if board.pads.iter().filter(|pad| pad.net == net_id && pad.piece_of < 0).count() < 2 {
        return false;
    }

    let mut trace_indices = Vec::new();
    for i in 0..board.traces.len() {
        if board.traces[i].net == net_id && board.traces[i].points.len() >= 2 {
            trace_indices.push(i as i32);
        }
    }
    if trace_indices.is_empty() {
        return false;
    }

    // 1. Detect all intersection points among the net's traces.
    let mut trace_splits: BTreeMap<i32, Vec<SplitMarker>> = BTreeMap::new();
    let mut stub_junctions = Vec::new();
    let mut found_intersection = false;
    for idx_a in 0..trace_indices.len() {
        let t_a = trace_indices[idx_a];
        let tr_a = &board.traces[t_a as usize];
        for i in 0..tr_a.points.len() - 1 {
            let s_a = Segment2::new(tr_a.points[i].pos, tr_a.points[i + 1].pos);
            for idx_b in idx_a..trace_indices.len() {
                let t_b = trace_indices[idx_b];
                let tr_b = &board.traces[t_b as usize];
                if tr_a.layer != tr_b.layer {
                    continue;
                }
                let start_j = if t_a == t_b { i + 2 } else { 0 };
                for j in start_j..tr_b.points.len() - 1 {
                    let s_b = Segment2::new(tr_b.points[j].pos, tr_b.points[j + 1].pos);
                    if s_a.p0.distance_to(s_b.p0) < 0.1
                        || s_a.p0.distance_to(s_b.p1) < 0.1
                        || s_a.p1.distance_to(s_b.p0) < 0.1
                        || s_a.p1.distance_to(s_b.p1) < 0.1
                    {
                        continue;
                    }
                    if let Some(hit) = s_a.intersects(&s_b) {
                        let min_end_dist = std_min(
                            std_min(std_min(hit.distance_to(s_a.p0), hit.distance_to(s_a.p1)), hit.distance_to(s_b.p0)),
                            hit.distance_to(s_b.p1),
                        );
                        if min_end_dist > 0.03 {
                            // Never split the inside of a straight pad-exit stub.
                            let st_a = straight_segment(&tr_a.points, i as i32);
                            let st_b = straight_segment(&tr_b.points, j as i32);
                            if st_a || st_b {
                                if st_a && st_b {
                                    continue;
                                }
                                let t_s = if st_a { t_a } else { t_b };
                                let t_o = if st_a { t_b } else { t_a };
                                let seg_s = if st_a { i as i32 } else { j as i32 };
                                let seg_o = if st_a { j as i32 } else { i as i32 };
                                let tr_s = &board.traces[t_s as usize];
                                let s_o = if st_a { s_b } else { s_a };
                                let end_idx = stub_end_node(&tr_s.points, seg_s, hit);
                                let e = tr_s.points[end_idx as usize].pos;
                                let d = s_o.p1 - s_o.p0;
                                let len2 = d.dot(d);
                                let t_o_ = if len2 > 1e-12 {
                                    std_clamp((e - s_o.p0).dot(d) / len2, 0.0, 1.0)
                                } else {
                                    0.0
                                };
                                let foot = s_o.p0 + d * t_o_;
                                if foot.distance_to(e) > K_STUB_JUNCTION_SNAP {
                                    continue;
                                }
                                if std_min(e.distance_to(s_o.p0), e.distance_to(s_o.p1)) <= 0.03 {
                                    continue;
                                }
                                found_intersection = true;
                                stub_junctions.push((t_s, end_idx));
                                trace_splits.entry(t_o).or_default().push(SplitMarker {
                                    segment_index: seg_o, param_t: t_o_, pos: e,
                                });
                                continue;
                            }
                            found_intersection = true;
                            let t_param_a = (hit - s_a.p0).length() / (s_a.length() + 1e-4);
                            let t_param_b = (hit - s_b.p0).length() / (s_b.length() + 1e-4);
                            trace_splits.entry(t_a).or_default().push(SplitMarker {
                                segment_index: i as i32, param_t: t_param_a, pos: hit,
                            });
                            trace_splits.entry(t_b).or_default().push(SplitMarker {
                                segment_index: j as i32, param_t: t_param_b, pos: hit,
                            });
                        }
                    }
                }
            }
        }
    }

    let mut legacy_junction = false;
    for &t_idx in &trace_indices {
        let tr = &board.traces[t_idx as usize];
        for p in [&tr.points[0], tr.points.last().unwrap()] {
            if p.is_fixed && p.terminal_pad < 0 && p.attached_via < 0 && p.branch_trace < 0 {
                legacy_junction = true;
            }
        }
    }
    found_intersection = found_intersection || legacy_junction;
    if !found_intersection && trace_indices.len() <= 1 {
        return false;
    }

    // 2. Build vertices (pads, vias, intersections).
    let mut vertices = Vec::new();
    let get_or_add_vertex = |vertices: &mut Vec<Vec2>, p: Vec2| -> i32 {
        for v in 0..vertices.len() {
            if vertices[v].distance_to(p) < 0.1 {
                return v as i32;
            }
        }
        let id = vertices.len() as i32;
        vertices.push(p);
        id
    };
    let mut touched_pad_ids = BTreeSet::new();
    for &tid in &trace_indices {
        let tr = &board.traces[tid as usize];
        if !tr.points.is_empty() {
            if tr.points[0].terminal_pad >= 0 {
                touched_pad_ids.insert(tr.points[0].terminal_pad);
            }
            if tr.points.last().unwrap().terminal_pad >= 0 {
                touched_pad_ids.insert(tr.points.last().unwrap().terminal_pad);
            }
        }
    }
    let mut pad_vertex_ids = Vec::new();
    let mut pad_to_vertex = BTreeMap::new();
    for pid in touched_pad_ids {
        if pid >= 0 && pid < board.pads.len() as i32 {
            let vid = get_or_add_vertex(&mut vertices, board.pads[pid as usize].center);
            pad_vertex_ids.push(vid);
            pad_to_vertex.insert(pid, vid);
        }
    }
    let mut via_to_vertex = BTreeMap::new();
    let mut planes = Vec::new();
    copper::plane_layers_of(board, net_id, &mut planes);
    for via in &board.vias {
        if via.net == net_id {
            let vid = get_or_add_vertex(&mut vertices, via.pos);
            via_to_vertex.insert(via.id, vid);
            for &l in &planes {
                if copper::via_on_layer(via, l) {
                    if !pad_vertex_ids.contains(&vid) {
                        pad_vertex_ids.push(vid);
                    }
                    break;
                }
            }
        }
    }

    // 3. Split traces at markers, junction host nodes, and endpoints.
    let mut all_edges = Vec::new();
    let mut host_node = vec![Vec::<u8>::new(); board.traces.len()];
    for &t_idx in &trace_indices {
        host_node[t_idx as usize] = vec![0; board.traces[t_idx as usize].points.len()];
    }
    for &(t_idx, k) in &stub_junctions {
        host_node[t_idx as usize][k as usize] = 1;
    }
    for &t_idx in &trace_indices {
        let tr = &board.traces[t_idx as usize];
        for e in [0, tr.points.len() as i32 - 1] {
            let p = &tr.points[e as usize];
            if p.terminal_pad >= 0 || p.attached_via >= 0 {
                continue;
            }
            for &h_idx in &trace_indices {
                if h_idx == t_idx {
                    continue;
                }
                let h = &board.traces[h_idx as usize];
                if h.layer != tr.layer {
                    continue;
                }
                let hn = h.points.len() as i32;
                if p.branch_trace == h.id && p.branch_point > 0 && p.branch_point < hn - 1 {
                    host_node[h_idx as usize][p.branch_point as usize] = 1;
                    continue;
                }
                for k in 1..hn - 1 {
                    if h.points[k as usize].pos.distance_to(p.pos) < 1e-3 {
                        host_node[h_idx as usize][k as usize] = 1;
                    }
                }
            }
        }
    }
    let mut make_edge = |tr: &Trace, u: i32, v: i32, pts: &[TracePoint]| {
        // ADAPTED: NetGraphSimplifier.cpp:424, 645, 665, 775: drop the unused layer mask; layer_index carries the layer.
        // ADAPTED: NetGraphSimplifier.cpp:425-427, 666, 776: carry the trace rule through edges and the chain's first edge.
        let mut e = GraphEdge {
            u, v, length: 0.0, layer_index: tr.layer, width: tr.width,
            orig_trace_id: tr.id, points: pts.to_vec(), rule: tr.rule,
        };
        for k in 0..pts.len().saturating_sub(1) {
            e.length += pts[k].pos.distance_to(pts[k + 1].pos);
        }
        all_edges.push(e);
    };
    for &t_idx in &trace_indices {
        let tr = &board.traces[t_idx as usize];
        let splits = trace_splits.entry(t_idx).or_default();
        // ADAPTED: NetGraphSimplifier.cpp:436 std::sort leaves the order of equal (segment, t) keys
        // to the library; the stable sort keeps them in insertion order, the same on every platform.
        splits.sort_by(|a, b| {
            a.segment_index.cmp(&b.segment_index)
                .then_with(|| a.param_t.partial_cmp(&b.param_t).unwrap_or(Ordering::Equal))
        });
        let mut last_v = get_or_add_vertex(&mut vertices, tr.points[0].pos);
        let mut current_sub = vec![tr.points[0].clone()];
        let mut split_idx = 0;
        for s in 0..tr.points.len() - 1 {
            while split_idx < splits.len() && splits[split_idx].segment_index == s as i32 {
                let sp = &splits[split_idx];
                split_idx += 1;
                let split_pt = TracePoint { pos: sp.pos, width: tr.points[s].width, ..TracePoint::default() };
                current_sub.push(split_pt.clone());
                let v_branch = get_or_add_vertex(&mut vertices, sp.pos);
                if last_v != v_branch && current_sub.len() >= 2 {
                    make_edge(tr, last_v, v_branch, &current_sub);
                }
                current_sub.clear();
                current_sub.push(split_pt);
                last_v = v_branch;
            }
            current_sub.push(tr.points[s + 1].clone());
            if s + 2 < tr.points.len() && host_node[t_idx as usize][s + 1] != 0 {
                let v_host = get_or_add_vertex(&mut vertices, tr.points[s + 1].pos);
                if last_v != v_host && current_sub.len() >= 2 {
                    make_edge(tr, last_v, v_host, &current_sub);
                }
                current_sub.clear();
                current_sub.push(tr.points[s + 1].clone());
                last_v = v_host;
            }
        }
        let v_end = get_or_add_vertex(&mut vertices, tr.points.last().unwrap().pos);
        if last_v != v_end && current_sub.len() >= 2 {
            make_edge(tr, last_v, v_end, &current_sub);
        }
    }
    if all_edges.is_empty() {
        return false;
    }

    // 4. Kruskal's minimum spanning tree: preserve insertion order at equal lengths.
    all_edges.sort_by(|a, b| a.length.partial_cmp(&b.length).unwrap_or(Ordering::Equal));
    let mut dsu = Dsu::new(vertices.len() as i32);
    let mut mst_edges = Vec::new();
    let mut redundant_cycles_removed = 0;
    for e in &all_edges {
        if dsu.find(e.u) != dsu.find(e.v) {
            dsu.unite(e.u, e.v);
            mst_edges.push(e.clone());
        } else {
            redundant_cycles_removed += 1;
        }
    }
    if !found_intersection && redundant_cycles_removed == 0 {
        return false;
    }
    let mut orig_dsu = Dsu::new(vertices.len() as i32);
    for e in &all_edges {
        orig_dsu.unite(e.u, e.v);
    }
    for k in 0..pad_vertex_ids.len() {
        for m in k + 1..pad_vertex_ids.len() {
            let p_a = pad_vertex_ids[k];
            let p_b = pad_vertex_ids[m];
            if orig_dsu.find(p_a) == orig_dsu.find(p_b) && dsu.find(p_a) != dsu.find(p_b) {
                return false;
            }
        }
    }

    // 5. Prune degree-one vertices that are not terminals.
    let mut pruned_any = true;
    while pruned_any {
        pruned_any = false;
        let mut degree = vec![0i32; vertices.len()];
        for e in &mst_edges {
            degree[e.u as usize] += 1;
            degree[e.v as usize] += 1;
        }
        let mut next_edges = Vec::new();
        for e in &mst_edges {
            let u_is_pad = pad_vertex_ids.contains(&e.u);
            let v_is_pad = pad_vertex_ids.contains(&e.v);
            if (!u_is_pad && degree[e.u as usize] <= 1) || (!v_is_pad && degree[e.v as usize] <= 1) {
                pruned_any = true;
            } else {
                next_edges.push(e.clone());
            }
        }
        mst_edges = next_edges;
    }
    if mst_edges.is_empty() {
        return false;
    }

    // 6. Rebuild chains; retain a backup for copper connectivity and clearance checks.
    // The Rust copper API makes the reference's default contact tolerance explicit.
    let groups_before = copper::net_pad_groups(board, net_id, None, 1e-3);
    let old_segs = net_segments(board, net_id);
    let backup_traces = board.traces.clone();
    let mut e = Vec::new();
    for edge in &mst_edges {
        if edge.points.len() >= 2 {
            e.push(edge.clone());
        }
    }
    if e.is_empty() {
        return false;
    }
    let n_e = e.len() as i32;
    let n_vert = vertices.len() as i32;
    let mut terminal = vec![0u8; n_vert as usize];
    for &v in &pad_vertex_ids {
        terminal[v as usize] = 1;
    }
    for &vv in via_to_vertex.values() {
        terminal[vv as usize] = 1;
    }
    let mut inc: Vec<Vec<(i32, i32)>> = vec![Vec::new(); n_vert as usize];
    // The reference's unused endPoint lambda (593) is used for these read-only endpoints.
    let end_point = |edge: i32, side: i32| {
        if side != 0 { e[edge as usize].points.last().unwrap() } else { &e[edge as usize].points[0] }
    };
    for i in 0..n_e {
        inc[e[i as usize].u as usize].push((i, 0));
        inc[e[i as usize].v as usize].push((i, 1));
        for s in 0..2 {
            let p = end_point(i, s);
            if p.terminal_pad >= 0 || p.attached_via >= 0 {
                terminal[if s != 0 { e[i as usize].v } else { e[i as usize].u } as usize] = 1;
            }
        }
    }
    let leave_dir = |edge: i32, side: i32| {
        let pts = &e[edge as usize].points;
        let n = pts.len() as i32;
        let o = if side != 0 { pts[(n - 1) as usize].pos } else { pts[0].pos };
        let want = std_min(1.0f32, 0.5 * e[edge as usize].length);
        let mut acc = 0.0f32;
        let mut q = if side != 0 { pts[0].pos } else { pts[(n - 1) as usize].pos };
        for k in 1..n {
            let a = if side != 0 { pts[(n - k) as usize].pos } else { pts[(k - 1) as usize].pos };
            let b = if side != 0 { pts[(n - 1 - k) as usize].pos } else { pts[k as usize].pos };
            let l = a.distance_to(b);
            if acc + l >= want && l > 0.0 {
                q = Vec2::lerp(a, b, (want - acc) / l);
                break;
            }
            acc += l;
        }
        (q - o).normalized()
    };
    let mut link = vec![[(-1i32, -1i32); 2]; n_e as usize];
    let mut trunk_pair = vec![(-1i32, -1i32); n_vert as usize];
    for v in 0..n_vert as usize {
        if terminal[v] != 0 || inc[v].len() < 2 {
            continue;
        }
        let mut best = 2.0f32;
        let mut bi = -1;
        let mut bj = -1;
        for i in 0..inc[v].len() {
            for j in i + 1..inc[v].len() {
                let (ei, si) = inc[v][i];
                let (ej, sj) = inc[v][j];
                if ei == ej {
                    continue;
                }
                if e[ei as usize].layer_index != e[ej as usize].layer_index
                    || (e[ei as usize].width - e[ej as usize].width).abs() > 1e-4
                {
                    continue;
                }
                let d = leave_dir(ei, si).dot(leave_dir(ej, sj));
                if d < best {
                    best = d;
                    bi = i as i32;
                    bj = j as i32;
                }
            }
        }
        if bi < 0 {
            continue;
        }
        trunk_pair[v] = (bi, bj);
        let (ei, si) = inc[v][bi as usize];
        let (ej, sj) = inc[v][bj as usize];
        link[ei as usize][si as usize] = (ej, sj);
        link[ej as usize][sj as usize] = (ei, si);
    }

    // Walk chains (NetGraphSimplifier.cpp:638-694).
    struct Chain {
        pts: Vec<TracePoint>,
        joint_index: BTreeMap<i32, i32>,
        v_start: i32,
        v_end: i32,
        layer_index: i32,
        width: f32,
        orig_count: BTreeMap<i32, i32>,
        id: i32,
        rule: u16,
    }
    let mut chains: Vec<Chain> = Vec::new();
    let mut edge_chain = vec![-1i32; n_e as usize];
    for e0 in 0..n_e {
        if edge_chain[e0 as usize] >= 0 {
            continue;
        }
        let mut cur = e0;
        let mut free_side = 0;
        let mut guard = 0;
        while guard <= n_e && link[cur as usize][free_side as usize].0 >= 0 {
            let (f, t) = link[cur as usize][free_side as usize];
            cur = f;
            free_side = 1 - t;
            guard += 1;
        }
        let mut c = Chain {
            pts: Vec::new(), joint_index: BTreeMap::new(),
            v_start: if free_side != 0 { e[cur as usize].v } else { e[cur as usize].u },
            v_end: -1, layer_index: e[cur as usize].layer_index, width: e[cur as usize].width,
            orig_count: BTreeMap::new(), id: -1, rule: e[cur as usize].rule,
        };
        let mut side = free_side;
        let mut guard = 0;
        while guard <= n_e && cur >= 0 && edge_chain[cur as usize] < 0 {
            edge_chain[cur as usize] = chains.len() as i32;
            let mut pts = e[cur as usize].points.clone();
            if side != 0 {
                reverse_points(&mut pts);
            }
            if !c.pts.is_empty() {
                let stub_joint = straight_segment(&c.pts, c.pts.len() as i32 - 2) || straight_segment(&pts, 0);
                let j = c.pts.last_mut().unwrap();
                if !stub_joint {
                    j.is_fixed = false;
                }
                j.branch_trace = -1;
                j.branch_point = -1;
                c.joint_index.insert(if side != 0 { e[cur as usize].v } else { e[cur as usize].u }, c.pts.len() as i32 - 1);
                pts.remove(0);
            }
            *c.orig_count.entry(e[cur as usize].orig_trace_id).or_default() += pts.len() as i32;
            c.pts.extend(pts);
            c.v_end = if side != 0 { e[cur as usize].u } else { e[cur as usize].v };
            let nx = link[cur as usize][(1 - side) as usize];
            cur = nx.0;
            side = nx.1;
            guard += 1;
        }
        chains.push(c);
    }

    // Reuse original trace ids, largest share first.
    let mut next_trace_id = 0;
    for tr in &backup_traces {
        next_trace_id = std_max(next_trace_id, tr.id + 1);
    }
    {
        struct Claim { count: i32, chain: i32, orig: i32 }
        let mut claims = Vec::new();
        for ci in 0..chains.len() {
            for (&orig, &cnt) in &chains[ci].orig_count {
                if orig >= 0 {
                    claims.push(Claim { count: cnt, chain: ci as i32, orig });
                }
            }
        }
        claims.sort_unstable_by(|a, b| {
            b.count.cmp(&a.count).then(a.chain.cmp(&b.chain)).then(a.orig.cmp(&b.orig))
        });
        let mut used_orig = BTreeSet::new();
        for cl in claims {
            if chains[cl.chain as usize].id >= 0 || used_orig.contains(&cl.orig) {
                continue;
            }
            chains[cl.chain as usize].id = cl.orig;
            used_orig.insert(cl.orig);
        }
        for c in &mut chains {
            if c.id < 0 {
                c.id = next_trace_id;
                next_trace_id += 1;
            }
        }
    }

    // Chain ends become trunk branches or pinned fallback nodes. Index-based borrows
    // read each host as it stands at this point, just as the reference's references do.
    for ci in 0..chains.len() {
        for s in 0..2 {
            let c = &chains[ci];
            let v = if s != 0 { c.v_end } else { c.v_start };
            let pi = if s != 0 { c.pts.len() - 1 } else { 0 };
            let layer_index = c.layer_index;
            let p = &mut chains[ci].pts[pi];
            if terminal[v as usize] != 0 || p.terminal_pad >= 0 || p.attached_via >= 0 {
                if p.terminal_pad < 0 && p.attached_via < 0 {
                    p.branch_trace = -1;
                    p.branch_point = -1;
                    for (&via_id, &vv) in &via_to_vertex {
                        if vv == v && via_id >= 0 && via_id < board.vias.len() as i32
                            && copper::via_on_layer(&board.vias[via_id as usize], layer_index)
                        {
                            p.attached_via = via_id;
                            p.pos = board.vias[via_id as usize].pos;
                            p.is_fixed = false;
                            break;
                        }
                    }
                }
                continue;
            }
            p.branch_trace = -1;
            p.branch_point = -1;
            p.is_fixed = true;
            if trunk_pair[v as usize].0 < 0 {
                continue;
            }
            let host_chain = edge_chain[inc[v as usize][trunk_pair[v as usize].0 as usize].0 as usize] as usize;
            let h = &chains[host_chain];
            let it = h.joint_index.get(&v);
            if host_chain == ci || it.is_none() {
                continue;
            }
            let joint = *it.unwrap();
            let id = h.id;
            let pos = h.pts[joint as usize].pos;
            let p = &mut chains[ci].pts[pi];
            p.is_fixed = false;
            p.branch_trace = id;
            p.branch_point = joint;
            p.pos = pos;
        }
    }

    trace_indices.sort_unstable_by(|a, b| b.cmp(a));
    for tid in trace_indices {
        board.traces.remove(tid as usize);
    }
    for c in chains {
        if c.pts.len() < 2 {
            continue;
        }
        board.traces.push(Trace {
            id: c.id, net: net_id, layer: c.layer_index, width: c.width,
            points: c.pts, is_routed: true, rule: c.rule, ..Trace::default()
        });
    }

    // Every branch link must name an interior node of a trace of this net.
    {
        let mut id_to_idx = BTreeMap::new();
        for t in 0..board.traces.len() {
            id_to_idx.insert(board.traces[t].id, t);
        }
        for t in 0..board.traces.len() {
            if board.traces[t].net != net_id {
                continue;
            }
            for k in 0..board.traces[t].points.len() {
                let p = &board.traces[t].points[k];
                if p.branch_trace < 0 {
                    continue;
                }
                let h = id_to_idx.get(&p.branch_trace).map(|&idx| &board.traces[idx]);
                if h.map_or(true, |h| h.net != net_id || p.branch_point <= 0 || p.branch_point + 1 >= h.points.len() as i32) {
                    let p = &mut board.traces[t].points[k];
                    p.branch_trace = -1;
                    p.branch_point = -1;
                }
            }
        }
    }
    if copper::net_pad_groups(board, net_id, None, 1e-3) > groups_before
        || new_segment_too_close(board, net_id, &old_segs)
    {
        board.traces = backup_traces;
        return false;
    }
    true
}

/// `NetGraphSimplifier.cpp:812-820`: even-odd test, last vertex joins the first.
/// `pointInRing` (NetGraphSimplifier.cpp:812-820); public for the integration test.
pub fn point_in_ring(ring: &[Vec2], p: Vec2) -> bool {
    let mut inside = false;
    let mut j = ring.len().wrapping_sub(1);
    for i in 0..ring.len() {
        if (ring[i].y > p.y) != (ring[j].y > p.y)
            && p.x < (ring[j].x - ring[i].x) * (p.y - ring[i].y) / (ring[j].y - ring[i].y) + ring[i].x
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// `NetGraphSimplifier.cpp:823-911`: remove unused vias and remap index attachments.
pub fn prune_dangling_vias(board: &mut OBoard) -> i32 {
    let n_l = std::cmp::max(1, board.layers.len() as i32);
    let mut remap = vec![-1i32; board.vias.len()];
    let mut kept = Vec::new();
    let mut removed = 0;
    for i in 0..board.vias.len() {
        let v = &board.vias[i];
        let rv: f64 = v.diameter as f64 * 0.5;
        let mut used = v.is_fixed || (v.net >= 0 && plane_layer_of_net(board, v.net) >= 0);
        let mut items = 0;
        let mut first_layer = -1;
        let mut multi_layer = false;
        // Explicit state arguments let the loops inspect multi_layer between calls.
        let touch = |layer: i32, items: &mut i32, first_layer: &mut i32, multi_layer: &mut bool| {
            *items += 1;
            if layer < 0 || (*first_layer >= 0 && layer != *first_layer) {
                *multi_layer = true;
            }
            if *first_layer < 0 {
                *first_layer = layer;
            }
        };
        for tr in &board.traces {
            if used || multi_layer {
                break;
            }
            if tr.net != v.net {
                continue;
            }
            let mut t = false;
            let mut k = 0;
            while k < tr.points.len() && !t {
                if tr.points[k].attached_via == i as i32 {
                    t = true;
                }
                if k + 1 < tr.points.len()
                    && copper::seg_seg(tr.points[k].pos, tr.points[k + 1].pos, v.pos, v.pos).d
                        <= tr.width as f64 * 0.5 + rv + 1e-3
                {
                    t = true;
                }
                k += 1;
            }
            if t {
                touch(tr.layer, &mut items, &mut first_layer, &mut multi_layer);
            }
        }
        for pad in &board.pads {
            if used || multi_layer {
                break;
            }
            if pad.net >= 0 && pad.net == v.net && copper::seg_pad_gap(v.pos, v.pos, rv, pad) <= 1e-3 {
                // NetGraphSimplifier.cpp:864, Board.cpp:193-198 layerIndexForMask: the first board layer in
                // the mask, -1 for none (the port's mask holds stack-index bits).
                let layer = if pad.is_smd { layer_index_for_mask(board, pad.layer_mask) } else { -1 };
                touch(layer, &mut items, &mut first_layer, &mut multi_layer);
            }
        }
        // NetGraphSimplifier.cpp:866-876 (C5e): a copper zone of the via's net whose outline contains it
        // is copper on the zone's layers.
        for z in &board.zones {
            if used || multi_layer {
                break;
            }
            if !z.is_copper() || z.net != v.net || !point_in_ring(&z.outline, v.pos) {
                continue;
            }
            let mut l = 0;
            while l < n_l && !multi_layer {
                if z.layer_mask & (1u64 << l) != 0 {
                    touch(l, &mut items, &mut first_layer, &mut multi_layer);
                }
                l += 1;
            }
        }
        if !used && !multi_layer && items >= 2 {
            let before = copper::net_pad_groups(board, v.net, None, 1e-3);
            let net = v.net;
            board.vias[i].net = -1;
            let without = copper::net_pad_groups(board, net, None, 1e-3);
            board.vias[i].net = net;
            used = without > before;
            if !used {
                board.vias[i].net = -1;
            }
        }
        used = used || multi_layer;
        if used {
            remap[i] = kept.len() as i32;
            kept.push(board.vias[i].clone());
            let id = kept.len() as i32 - 1;
            kept.last_mut().unwrap().id = id;
        } else {
            removed += 1;
        }
    }
    let mut id_mismatch = false;
    for i in 0..board.vias.len() {
        if board.vias[i].id != i as i32 {
            id_mismatch = true;
        }
    }
    if removed == 0 && !id_mismatch {
        return 0;
    }
    for tr in &mut board.traces {
        for pt in &mut tr.points {
            if pt.attached_via >= 0 {
                pt.attached_via = if pt.attached_via < remap.len() as i32 { remap[pt.attached_via as usize] } else { -1 };
            }
        }
    }
    board.vias = kept;
    removed
}

/// `NetGraphSimplifier.cpp:913-926`: visit nets in index order, skipping frozen nets.
pub fn simplify_all_nets(board: &mut OBoard, frozen_nets: Option<&[i32]>) -> i32 {
    let mut simplified_count = 0;
    for net_id in 0..board.nets.len() {
        if frozen_nets.is_some_and(|frozen| frozen.contains(&(net_id as i32))) {
            continue;
        }
        if simplify_net(net_id as i32, board) {
            simplified_count += 1;
        }
    }
    if simplified_count > 0 {
        prune_dangling_vias(board);
    }
    simplified_count
}

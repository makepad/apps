//! Bead counts, layouts and lines on a skeleton.
//!
//! Counts are decided on the central skeleton (where the two sides run
//! roughly parallel) and carried down into the corners. Each node then has a
//! layout: the beads one side sees (location from that side's outline,
//! width), the width of a middle bead on the skeleton itself, and the
//! marker, the inner edge of the walls where infill starts. A side bead or
//! the marker is traced face by face: it crosses a rib where its location
//! is below the rib's node, and a skeleton edge where it climbs past it.

use super::beading::{Beading, Strategy};
use super::delaunay::NONE;
use super::skeleton::{Corner, Skeleton, P};
use super::{Config, BeadLine};
use makepad_fab_slicer::geom::*;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// One side's beads at a node, in mm from that side's outline.
#[derive(Clone, Default, Debug)]
pub(crate) struct Half {
    pub side: Vec<(f64, f64)>,
    /// The middle bead's width (an odd count), 0 when none.
    pub mid: f64,
    /// Where the infill starts; NaN when the beads fill the whole width.
    pub marker: f64,
}

fn half_of(st: &Strategy, t: f64, n: usize, shift: f64, b: &mut Beading) -> Half {
    st.compute_into(t, n, b);
    let c = 0.5 * t;
    let mut h = Half { side: Vec::new(), mid: 0.0, marker: f64::NAN };
    for (&l, &w) in b.locations.iter().zip(&b.widths) {
        if w <= 0.0 {
            if l < c - 1e-9 && h.marker.is_nan() {
                h.marker = l;
            }
        } else if (l - c).abs() <= 1e-9 {
            h.mid = w;
        } else if l < c {
            h.side.push((l, w));
        }
    }
    // Precise outer wall: the outline was shrunk by a whole bead overlap;
    // the outer bead moves back out by half of it where it has a neighbour.
    if shift > 0.0 && 2 * h.side.len() + usize::from(h.mid > 0.0) >= 2 {
        h.side[0].0 = (h.side[0].0 - shift).max(0.0);
    }
    h
}

fn blend(a: &Half, b: &Half, t: f64, c: f64) -> Half {
    let n = a.side.len().max(b.side.len());
    let get = |h: &Half, i: usize| h.side.get(i).copied().unwrap_or((c, 0.0));
    let side = (0..n).map(|i| {
        let (x, y) = (get(a, i), get(b, i));
        (x.0 + (y.0 - x.0) * t, x.1 + (y.1 - x.1) * t)
    });
    let m = |v: f64| if v.is_nan() { c } else { v };
    let marker = if a.marker.is_nan() && b.marker.is_nan() { f64::NAN } else { m(a.marker) + (m(b.marker) - m(a.marker)) * t };
    Half { side: side.collect(), mid: a.mid + (b.mid - a.mid) * t, marker }
}

fn layout(st: &Strategy, q: f64, t: f64, shift: f64, b: &mut Beading) -> Half {
    let top = st.cap + 1;
    let q = q.clamp(0.0, top as f64);
    let n = (q.floor() as usize).min(top);
    let a = q - n as f64;
    let lo = half_of(st, t, n, shift, b);
    if a <= 1e-9 || n == top {
        return lo;
    }
    let hi = half_of(st, t, n + 1, shift, b);
    blend(&lo, &hi, a, 0.5 * t)
}

/// Make short non-central stretches between two central regions whose
/// counts differ by at most one central too (a slight bend in a thin wall).
fn gaps(sk: &mut Skeleton, st: &Strategy, length: f64) {
    let count: Vec<usize> = sk.nodes.iter().map(|n| st.optimal_count(2.0 * n.r / SCALE)).collect();
    let central: Vec<bool> = (0..sk.nodes.len()).map(|i| sk.incident(i).iter().any(|&e| sk.edges[e as usize].central)).collect();
    let mut seen = vec![false; sk.edges.len()];
    let mut mark = Vec::new();
    let mut path = Vec::new();
    for start in 0..sk.nodes.len() {
        if !central[start] {
            continue;
        }
        for &first in sk.incident(start) {
            let first = first as usize;
            if seen[first] || sk.edges[first].central {
                continue;
            }
            path.clear();
            let (mut n, mut e, mut walked) = (start, first, 0.0);
            loop {
                seen[e] = true;
                path.push(e);
                walked += sk.length(e);
                n = sk.other(e, n);
                if walked >= length * SCALE {
                    break;
                }
                if central[n] {
                    if count[start].abs_diff(count[n]) <= 1 {
                        mark.extend_from_slice(&path);
                    }
                    break;
                }
                let inc = sk.incident(n);
                if inc.len() != 2 {
                    break;
                }
                e = if inc[0] as usize == e { inc[1] as usize } else { inc[0] as usize };
                if seen[e] {
                    break;
                }
            }
        }
    }
    for e in mark {
        sk.edges[e].central = true;
    }
}

struct Event {
    x: f64,
    /// Index of the path node before it.
    k: usize,
    lower: usize,
    up: bool,
}

/// Fractional bead counts along one central path (node, distance in mm):
/// transitions between counts ramp over the transition length, short
/// up-and-down flickers are filtered out.
fn path_q(path: &[(usize, f64)], free: [bool; 2], sk: &Skeleton, cfg: &Config, st: &Strategy, q: &mut [f64]) {
    let r: Vec<f64> = path.iter().map(|&(n, _)| sk.nodes[n].r / SCALE).collect();
    let count: Vec<usize> = r.iter().map(|&r| st.optimal_count(2.0 * r)).collect();
    let total = path.last().map_or(0.0, |p| p.1);
    let mut events: Vec<Event> = Vec::new();
    for k in 0..path.len() - 1 {
        let (a, b) = (count[k], count[k + 1]);
        let steps: Vec<usize> = if b > a { (a..b).collect() } else { (b..a).rev().collect() };
        for lower in steps {
            let t = 0.5 * st.transition_thickness(lower);
            let f = if r[k + 1] == r[k] { 0.5 } else { ((t - r[k]) / (r[k + 1] - r[k])).clamp(0.0, 1.0) };
            events.push(Event { x: path[k].1 + (path[k + 1].1 - path[k].1) * f, k, lower, up: b > a });
        }
    }
    // Filter: an up and a down of the same step close together, with the
    // thickness between them never far from the step's thickness.
    let mut keep: Vec<usize> = Vec::new();
    for i in 0..events.len() {
        if let Some(&j) = keep.last() {
            let (a, b) = (&events[j], &events[i]);
            if a.lower == b.lower && a.up != b.up && b.x - a.x <= 100.0 {
                let t = st.transition_thickness(a.lower);
                if (a.k + 1..=b.k).all(|k| (2.0 * r[k] - t).abs() <= cfg.transition_filter_deviation) {
                    keep.pop();
                    continue;
                }
            }
        }
        keep.push(i);
    }
    let mut base = count[0] as f64;
    let mut ramps: Vec<(f64, f64, f64)> = Vec::new();
    for &i in &keep {
        let e = &events[i];
        let length = if e.lower == 0 { 0.01 } else { cfg.transition_length };
        let anchor = st.transition_anchor(e.lower);
        let before = if e.up { anchor } else { 1.0 - anchor } * length;
        let dir = if e.up { 1.0 } else { -1.0 };
        let (mut lo, mut hi) = (e.x - before, e.x - before + length);
        if lo < 0.0 {
            if free[0] {
                base += dir;
                continue;
            }
            hi -= lo;
            lo = 0.0;
        }
        if hi > total {
            if free[1] {
                continue;
            }
            lo = (lo - (hi - total)).max(0.0);
            hi = total;
        }
        ramps.push((lo, hi, dir));
    }
    for (k, &(n, s)) in path.iter().enumerate() {
        let mut v = base;
        for &(lo, hi, dir) in &ramps {
            v += dir * if hi > lo { ((s - lo) / (hi - lo)).clamp(0.0, 1.0) } else if s >= lo { 1.0 } else { 0.0 };
        }
        // A branch node keeps its own count: every path through it agrees.
        let ends = k == 0 && !free[0] || k + 1 == path.len() && !free[1];
        q[n] = if ends { count[k] as f64 } else { v };
    }
}

#[derive(Clone, Copy)]
struct Visit {
    d: f64,
    node: u32,
}
impl PartialEq for Visit {
    fn eq(&self, o: &Self) -> bool { self.cmp(o) == Ordering::Equal }
}
impl Eq for Visit {}
impl PartialOrd for Visit {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> { Some(self.cmp(o)) }
}
impl Ord for Visit {
    fn cmp(&self, o: &Self) -> Ordering { o.d.total_cmp(&self.d).then(o.node.cmp(&self.node)) }
}

/// Layouts per node: (the layouts, each node's layout index, the nodes that
/// own a layout of their own).
pub(crate) fn layouts(sk: &mut Skeleton, cfg: &Config, st: &Strategy) -> (Vec<Half>, Vec<u32>) {
    gaps(sk, st, cfg.transition_length);
    let n = sk.nodes.len();
    let deg: Vec<usize> = (0..n).map(|i| sk.incident(i).iter().filter(|&&e| sk.edges[e as usize].central).count()).collect();
    let mut q = vec![f64::NAN; n];
    let mut seen = vec![false; sk.edges.len()];
    let mut path: Vec<(usize, f64)> = Vec::new();
    for cycles in [false, true] {
        for start in 0..n {
            if deg[start] == 0 || (deg[start] == 2) != cycles {
                continue;
            }
            for &e0 in sk.incident(start) {
                let e0 = e0 as usize;
                if !sk.edges[e0].central || seen[e0] {
                    continue;
                }
                path.clear();
                path.push((start, 0.0));
                let (mut node, mut e, mut s) = (start, e0, 0.0);
                loop {
                    seen[e] = true;
                    s += sk.length(e) / SCALE;
                    node = sk.other(e, node);
                    path.push((node, s));
                    if deg[node] != 2 || node == start {
                        break;
                    }
                    match sk.incident(node).iter().map(|&x| x as usize).find(|&x| x != e && sk.edges[x].central && !seen[x]) {
                        Some(x) => e = x,
                        None => break,
                    }
                }
                let closed = node == start && cycles;
                path_q(&path, [!closed && deg[start] == 1, !closed && deg[node] == 1], sk, cfg, st, &mut q);
            }
        }
    }
    // Sources: central nodes, and local maxima of R off the central skeleton.
    let (_, shift, _) = cfg.nominal_wall_levels();
    let mut b = Beading::default();
    let mut halves = Vec::new();
    let mut owner = vec![NONE; n];
    let mut dist = vec![f64::INFINITY; n];
    let mut sources = Vec::new();
    for i in 0..n {
        let r = sk.nodes[i].r;
        let peak = sk.incident(i).iter().all(|&e| sk.nodes[sk.other(e as usize, i)].r <= r);
        if q[i].is_nan() && !peak {
            continue;
        }
        let t = 2.0 * r / SCALE;
        let qi = if q[i].is_nan() { st.optimal_count(t) as f64 } else { q[i] };
        owner[i] = halves.len() as u32;
        halves.push(layout(st, qi, t, shift, &mut b));
        dist[i] = 0.0;
        sources.push(Visit { d: 0.0, node: i as u32 });
    }
    let mut heap = BinaryHeap::from(sources);
    // Carry layouts down from their sources, then fill what is left (from
    // the nearest node that has one, never taking over a carried layout).
    let mut carried = Vec::new();
    for down in [true, false] {
        if !down {
            carried = owner.iter().map(|&o| o != NONE).collect();
            for i in 0..n {
                if carried[i] {
                    heap.push(Visit { d: dist[i], node: i as u32 });
                }
            }
        }
        while let Some(v) = heap.pop() {
            let i = v.node as usize;
            if v.d > dist[i] {
                continue;
            }
            for &e in sk.incident(i) {
                let k = sk.other(e as usize, i);
                if if down { sk.nodes[k].r > sk.nodes[i].r + 1.0 } else { carried[k] } {
                    continue;
                }
                let d = v.d + sk.length(e as usize);
                if d < dist[k] {
                    dist[k] = d;
                    owner[k] = owner[i];
                    heap.push(Visit { d, node: k as u32 });
                }
            }
        }
    }
    (halves, owner)
}

/// A traced piece of a line: from one junction to the next, with points in between.
struct Piece {
    from: u32,
    to: u32,
    first: u32,
    end: u32,
}

/// Every bead and marker line of a skeleton, in local units. Returns the
/// lines and the closed marker rings.
pub(crate) fn trace(sk: &Skeleton, halves: &[Half], owner: &[u32], cfg: &Config, st: &Strategy) -> (Vec<BeadLine>, Vec<Vec<Pt>>) {
    let slots = st.cap / 2 + 1;
    let marker = slots - 1;
    let r_mm = |n: u32| sk.nodes[n as usize].r / SCALE;
    let half = |n: u32| &halves[owner[n as usize] as usize];
    // (location, width) of a slot at a node; an absent bead sits on the skeleton.
    let loc = |n: u32, slot: usize| -> (f64, f64) {
        let h = half(n);
        if slot == marker {
            (if h.marker.is_nan() { r_mm(n) } else { h.marker }, 0.0)
        } else {
            h.side.get(slot).copied().unwrap_or((r_mm(n), 0.0))
        }
    };
    let mut pts: Vec<(P, f64)> = Vec::new();
    let mut junc = vec![NONE; (sk.ribs + sk.edges.len()) * slots];
    let mut pieces: Vec<Piece> = Vec::new();
    let mut inner: Vec<(P, f64)> = Vec::new();
    for f in &sk.faces {
        let [nl, nr] = f.nodes;
        for slot in 0..slots {
            let (ll, wl) = loc(nl, slot);
            let (lr, wr) = loc(nr, slot);
            let (il, ir) = (ll < r_mm(nl), lr < r_mm(nr));
            if !il && !ir {
                continue;
            }
            let mut end = |side: usize, inside: bool, l: f64, w: f64, pts: &mut Vec<(P, f64)>| -> u32 {
                let key = if inside { f.ribs[side] as usize } else { sk.ribs + f.edge as usize } * slots + slot;
                if junc[key] == NONE {
                    let p = if inside {
                        let (foot, node) = (f.feet[side], sk.nodes[f.nodes[side] as usize].pos);
                        let d = foot.dist(node);
                        (foot.lerp(node, if d > 0.0 { l * SCALE / d } else { 0.0 }), w)
                    } else {
                        // Where the slot meets the skeleton edge (both faces agree:
                        // the edge's own node order).
                        let e = &sk.edges[f.edge as usize];
                        let ((la, wa), (lb, wb)) = (loc(e.a, slot), loc(e.b, slot));
                        let (fa, fb) = (r_mm(e.a) - la, r_mm(e.b) - lb);
                        let t = if fa == fb { 0.5 } else { (fa / (fa - fb)).clamp(0.0, 1.0) };
                        (sk.nodes[e.a as usize].pos.lerp(sk.nodes[e.b as usize].pos, t), wa + (wb - wa) * t)
                    };
                    junc[key] = pts.len() as u32;
                    pts.push(p);
                }
                junc[key]
            };
            let a = end(0, il, ll, wl, &mut pts);
            let b = end(1, ir, lr, wr, &mut pts);
            let first = inner.len() as u32;
            if il && ir {
                corner_points(f.corner, f.at_v, pts[a as usize], pts[b as usize], (ll, lr), &mut inner);
            }
            if a != b {
                pieces.push(Piece { from: a, to: b, first, end: inner.len() as u32 });
            }
            let _ = slot;
        }
    }
    // Chain the pieces: open runs first (from a junction nothing enters), then cycles.
    let mut out = vec![NONE; pts.len()];
    let mut entered = vec![false; pts.len()];
    let mut loose = Vec::new();
    for (i, p) in pieces.iter().enumerate() {
        if out[p.from as usize] == NONE {
            out[p.from as usize] = i as u32;
            entered[p.to as usize] = true;
        } else {
            loose.push(i);
        }
    }
    // Which slot a junction belongs to.
    let mut slot_of = vec![0u32; pts.len()];
    for (key, &j) in junc.iter().enumerate() {
        if j != NONE {
            slot_of[j as usize] = (key % slots) as u32;
        }
    }
    let mut used = vec![false; pieces.len()];
    let mut lines = Vec::new();
    let mut rings = Vec::new();
    let emit = |start: u32, used: &mut Vec<bool>, lines: &mut Vec<BeadLine>, rings: &mut Vec<Vec<Pt>>| {
        let mut line: Vec<(P, f64)> = vec![pts[start as usize]];
        let mut j = start;
        let mut closed = false;
        while out[j as usize] != NONE && !used[out[j as usize] as usize] {
            let k = out[j as usize] as usize;
            used[k] = true;
            let p = &pieces[k];
            line.extend_from_slice(&inner[p.first as usize..p.end as usize]);
            j = p.to;
            if j == start {
                closed = true;
                break;
            }
            line.push(pts[j as usize]);
        }
        let slot = slot_of[start as usize] as usize;
        if slot == marker {
            if closed && line.len() >= 3 {
                rings.push(line.iter().map(|x| x.0.pt()).collect());
            }
        } else {
            lines.extend(split(line, closed, slot, false));
        }
    };
    for j in 0..pts.len() as u32 {
        if out[j as usize] != NONE && !entered[j as usize] {
            emit(j, &mut used, &mut lines, &mut rings);
        }
    }
    for j in 0..pts.len() as u32 {
        if out[j as usize] != NONE && !used[out[j as usize] as usize] {
            emit(j, &mut used, &mut lines, &mut rings);
        }
    }
    for i in loose {
        let p = &pieces[i];
        let mut line = vec![pts[p.from as usize]];
        line.extend_from_slice(&inner[p.first as usize..p.end as usize]);
        line.push(pts[p.to as usize]);
        let slot = slot_of[p.from as usize] as usize;
        if slot != marker {
            lines.extend(split(line, false, slot, false));
        }
    }
    middle(sk, halves, owner, &mut lines);
    let _ = cfg;
    (lines, rings)
}

/// The exact offset of the face's corner between two rib junctions: the
/// miter point at a convex vertex, an arc round a reflex one.
fn corner_points(corner: Corner, at_v: [bool; 2], a: (P, f64), b: (P, f64), l: (f64, f64), out: &mut Vec<(P, f64)>) {
    let w = |t: f64| a.1 + (b.1 - a.1) * t;
    match corner {
        Corner::None => {}
        Corner::Convex { v, n1, n2 } => {
            let det = n1.x * n2.y - n1.y * n2.x;
            if det.abs() < 1e-9 {
                return;
            }
            let (c1, c2) = (l.0 * SCALE, l.1 * SCALE);
            let m = P::new(v.x + (c1 * n2.y - c2 * n1.y) / det, v.y + (n1.x * c2 - n2.x * c1) / det);
            // Ahead of the entry along edge 1 and behind the exit along edge 2.
            let (u1, u2) = (P::new(n1.y, -n1.x), P::new(n2.y, -n2.x));
            let reach = 10.0 * c1.max(c2);
            if P::new(m.x - a.0.x, m.y - a.0.y).dot(u1) >= 0.0 && P::new(b.0.x - m.x, b.0.y - m.y).dot(u2) >= 0.0 && m.dist(v) <= reach {
                out.push((m, w(0.5)));
            }
        }
        Corner::Reflex { v, n1, n2 } => {
            let ang = |p: P| (p.y - v.y).atan2(p.x - v.x);
            let start = if at_v[0] { ang(a.0) } else { n1.y.atan2(n1.x) };
            let stop = if at_v[1] { ang(b.0) } else { n2.y.atan2(n2.x) };
            // Inward normals turn clockwise round a reflex vertex.
            let sweep = (start - stop).rem_euclid(std::f64::consts::TAU);
            if !at_v[0] {
                out.push((v.add(n1, l.0 * SCALE), w(0.0)));
            }
            if sweep < std::f64::consts::PI {
                let k = (sweep / 10f64.to_radians()).ceil() as usize;
                for i in 1..k {
                    let t = i as f64 / k as f64;
                    let ang = start - sweep * t;
                    let rad = (l.0 + (l.1 - l.0) * t) * SCALE;
                    out.push((P::new(v.x + rad * ang.cos(), v.y + rad * ang.sin()), w(t)));
                }
            }
            if !at_v[1] {
                out.push((v.add(n2, l.1 * SCALE), w(1.0)));
            }
        }
    }
}

/// Split a traced line where it carries no plastic (both ends of a segment
/// at zero width), dropping repeated points.
fn split(line: Vec<(P, f64)>, closed: bool, inset: usize, odd: bool) -> Vec<BeadLine> {
    let mut pts: Vec<(P, f64)> = Vec::with_capacity(line.len());
    for p in line {
        if let Some(last) = pts.last_mut() {
            if last.0.dist(p.0) < 1.0 {
                last.1 = last.1.max(p.1);
                continue;
            }
        }
        pts.push(p);
    }
    if closed && pts.len() > 1 && pts[0].0.dist(pts[pts.len() - 1].0) < 1.0 {
        let w = pts.pop().map_or(0.0, |p| p.1);
        pts[0].1 = pts[0].1.max(w);
    }
    const DRY: f64 = 1e-4;
    let n = pts.len();
    let dry = |i: usize, j: usize| pts[i].1 < DRY && pts[j].1 < DRY;
    let mut out = Vec::new();
    let mk = |run: &[(P, f64)], closed: bool| BeadLine { inset, closed, odd, pts: run.iter().map(|p| p.0.pt()).collect(), widths: run.iter().map(|p| p.1).collect() };
    if closed {
        let Some(cut) = (0..n).find(|&i| dry(i, (i + 1) % n)) else {
            if n >= 3 {
                out.push(mk(&pts, true));
            }
            return out;
        };
        // Start after a dry segment and walk once round.
        let order: Vec<(P, f64)> = (1..=n).map(|k| pts[(cut + k) % n]).collect();
        let mut run: Vec<(P, f64)> = Vec::new();
        for (i, p) in order.iter().enumerate() {
            if let Some(&prev) = run.last() {
                if prev.1 < DRY && p.1 < DRY {
                    if run.len() >= 2 {
                        out.push(mk(&run, false));
                    }
                    run.clear();
                }
            }
            run.push(*p);
            let _ = i;
        }
        if run.len() >= 2 {
            out.push(mk(&run, false));
        }
    } else {
        let mut run: Vec<(P, f64)> = Vec::new();
        for p in pts {
            if let Some(&prev) = run.last() {
                if prev.1 < DRY && p.1 < DRY {
                    if run.len() >= 2 {
                        out.push(mk(&run, false));
                    }
                    run.clear();
                }
            }
            run.push(p);
        }
        if run.len() >= 2 {
            out.push(mk(&run, false));
        }
    }
    out
}

/// Middle beads run along central skeleton edges where the count is odd;
/// an odd peak with no such edge gets a dot.
fn middle(sk: &Skeleton, halves: &[Half], owner: &[u32], lines: &mut Vec<BeadLine>) {
    let mid = |n: usize| halves[owner[n] as usize].mid;
    let inset = |n: usize| halves[owner[n] as usize].side.len();
    let n = sk.nodes.len();
    let on: Vec<bool> = sk.edges.iter().map(|e| e.central && (mid(e.a as usize) > 0.0 || mid(e.b as usize) > 0.0)).collect();
    let deg: Vec<usize> = (0..n).map(|i| sk.incident(i).iter().filter(|&&e| on[e as usize]).count()).collect();
    let mut used = vec![false; sk.edges.len()];
    for cycles in [false, true] {
        for start in 0..n {
            if deg[start] == 0 || (deg[start] == 2) != cycles {
                continue;
            }
            for &e0 in sk.incident(start) {
                if !on[e0 as usize] || used[e0 as usize] {
                    continue;
                }
                let mut line = vec![(sk.nodes[start].pos, mid(start))];
                let (mut node, mut e) = (start, e0 as usize);
                let mut closed = false;
                // The middle bead's index: the side beads around it where it
                // is a bead (in a ramp the side count can be one higher).
                let mut ins = if mid(start) > 0.0 { inset(start) } else { usize::MAX };
                loop {
                    used[e] = true;
                    node = sk.other(e, node);
                    if mid(node) > 0.0 {
                        ins = ins.min(inset(node));
                    }
                    if node == start && cycles {
                        closed = true;
                        break;
                    }
                    line.push((sk.nodes[node].pos, mid(node)));
                    if deg[node] != 2 {
                        break;
                    }
                    match sk.incident(node).iter().map(|&x| x as usize).find(|&x| on[x] && !used[x]) {
                        Some(x) => e = x,
                        None => break,
                    }
                }
                let ins = if ins == usize::MAX { 0 } else { ins };
                let length: f64 = line.windows(2).map(|w| w[0].0.dist(w[1].0)).sum();
                let wmax = line.iter().fold(0f64, |a, p| a.max(p.1));
                if length < 0.5 * wmax * SCALE {
                    let c = line.iter().fold(P::default(), |a, p| a.add(p.0, 1.0 / line.len() as f64));
                    lines.push(dot(c, wmax, ins));
                } else {
                    lines.extend(split(line, closed, ins, true));
                }
            }
        }
    }
    // Odd peaks off the central skeleton: a dot.
    for i in 0..n {
        let h = &halves[owner[i] as usize];
        if deg[i] == 0 && h.mid > 0.0 && sk.incident(i).iter().all(|&e| sk.nodes[sk.other(e as usize, i)].r < sk.nodes[i].r) {
            lines.push(dot(sk.nodes[i].pos, h.mid, h.side.len()));
        }
    }
}

/// A bead too short to be a line: a small closed hexagon of radius w/8
/// lays down about a round dot of width w.
fn dot(c: P, w: f64, inset: usize) -> BeadLine {
    let r = w * SCALE / 8.0;
    let pts = (0..6).map(|k| {
        let a = k as f64 * std::f64::consts::TAU / 6.0;
        P::new(c.x + r * a.cos(), c.y + r * a.sin()).pt()
    });
    BeadLine { inset, closed: true, odd: true, pts: pts.collect(), widths: vec![w; 6] }
}

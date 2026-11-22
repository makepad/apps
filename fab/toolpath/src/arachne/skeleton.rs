//! The trapezoid skeleton of a region, from a Delaunay triangulation of its
//! boundary samples.
//!
//! Every inside triangle gives a node at its circumcentre (the centre of an
//! empty circle touching three samples: a point near the medial axis) with
//! R, its exact distance to the outline. Around each sample the fan of inside
//! triangles gives a chain of nodes; a rib joins each node to its foot on the
//! sample's piece of outline (from the middle of the interval before it to
//! the middle of the one after), and two neighbouring ribs with the skeleton
//! edge between their nodes bound a face. Every rib is shared by the two
//! faces beside it and every skeleton edge by the faces on its two sides, so
//! a bead traced face by face closes up by construction.

use super::delaunay::{self, next, NONE};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::grid::SegGrid;

/// A point in local units (f64).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct P {
    pub x: f64,
    pub y: f64,
}

impl P {
    pub fn new(x: f64, y: f64) -> Self { P { x, y } }
    pub fn of(p: (i64, i64)) -> Self { P { x: p.0 as f64, y: p.1 as f64 } }
    pub fn lerp(self, q: P, t: f64) -> P { P { x: self.x + (q.x - self.x) * t, y: self.y + (q.y - self.y) * t } }
    pub fn dist(self, q: P) -> f64 { (self.x - q.x).hypot(self.y - q.y) }
    pub fn add(self, q: P, k: f64) -> P { P { x: self.x + q.x * k, y: self.y + q.y * k } }
    pub fn dot(self, q: P) -> f64 { self.x * q.x + self.y * q.y }
    pub fn pt(self) -> Pt { Pt::new(self.x.round() as i64, self.y.round() as i64) }
}

pub(crate) struct Node {
    pub pos: P,
    /// Exact distance to the outline, in units.
    pub r: f64,
}

pub(crate) struct Edge {
    pub a: u32,
    pub b: u32,
    pub central: bool,
}

/// The outline vertex inside a face's piece of outline, with the inward
/// unit normals of the edges before and after it.
#[derive(Clone, Copy)]
pub(crate) enum Corner {
    None,
    Convex { v: P, n1: P, n2: P },
    Reflex { v: P, n1: P, n2: P },
}

pub(crate) struct Face {
    /// Left rib then right rib, in outline direction (material on the left).
    pub ribs: [u32; 2],
    pub nodes: [u32; 2],
    pub feet: [P; 2],
    /// The skeleton edge between the two nodes; NONE when they are one.
    pub edge: u32,
    pub corner: Corner,
    /// Which feet sit on the corner vertex itself.
    pub at_v: [bool; 2],
}

pub(crate) struct Skeleton {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub faces: Vec<Face>,
    pub ribs: usize,
    /// Node → incident edges (CSR).
    pub at: Vec<u32>,
    pub adj: Vec<u32>,
}

impl Skeleton {
    pub fn incident(&self, n: usize) -> &[u32] { &self.adj[self.at[n] as usize..self.at[n + 1] as usize] }
    pub fn other(&self, e: usize, n: usize) -> usize {
        let e = &self.edges[e];
        if e.a as usize == n { e.b as usize } else { e.a as usize }
    }
    pub fn length(&self, e: usize) -> f64 {
        let e = &self.edges[e];
        self.nodes[e.a as usize].pos.dist(self.nodes[e.b as usize].pos)
    }
}

struct Samples {
    pts: Vec<(i64, i64)>,
    ring: Vec<u32>,
    /// Per ring: first sample and one past the last.
    rings: Vec<(u32, u32)>,
    /// The outline vertex a sample sits on (its index in `verts`), or NONE.
    vert: Vec<u32>,
}

impl Samples {
    fn next(&self, i: usize) -> usize {
        let (a, b) = self.rings[self.ring[i] as usize];
        if i + 1 == b as usize { a as usize } else { i + 1 }
    }
    fn prev(&self, i: usize) -> usize {
        let (a, b) = self.rings[self.ring[i] as usize];
        if i == a as usize { b as usize - 1 } else { i - 1 }
    }
}

/// An outline vertex: the unit directions of the edges into and out of it.
#[derive(Clone, Copy)]
struct Vert {
    u1: P,
    u2: P,
}

fn unit(a: Pt, b: Pt) -> P {
    let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let l = dx.hypot(dy).max(1e-12);
    P::new(dx / l, dy / l)
}

/// Inward (left) normal of a unit direction.
fn left(u: P) -> P { P::new(-u.y, u.x) }

const MM: f64 = SCALE;
/// Graded samples start this close to a sharp convex corner, so the bisector
/// of every corner is a run of skeleton edges between mirrored samples.
const GRADE: f64 = 0.01 * MM;
const COARSE: f64 = 0.5 * MM;
/// Triangle centres closer than this are one skeleton node.
const MERGE: f64 = 0.002 * MM;

/// Distance from `p` (with inward normal `n`) to the nearest outline point
/// across the material: the segment's nearest point lies ahead of `p` and
/// the segment faces back. Infinite when none is within `reach`.
fn across(grid: &SegGrid<u32>, p: Pt, n: P, reach: f64) -> f64 {
    let r = reach as i64 + 1;
    let mut best = f64::INFINITY;
    grid.visit_box(BBox::of(&[Pt::new(p.x - r, p.y - r), Pt::new(p.x + r, p.y + r)]), |a, b, _| {
        let ab = P::new((b.x - a.x) as f64, (b.y - a.y) as f64);
        let l2 = ab.dot(ab);
        if l2 == 0.0 {
            return;
        }
        let ap = P::new((p.x - a.x) as f64, (p.y - a.y) as f64);
        let t = (ap.dot(ab) / l2).clamp(0.0, 1.0);
        let q = P::new(a.x as f64 + ab.x * t - p.x as f64, a.y as f64 + ab.y * t - p.y as f64);
        // Past the best by more than rounding: no need for the root.
        if q.x * q.x + q.y * q.y > best * best * (1.0 + 1e-12) {
            return;
        }
        let d = q.x.hypot(q.y);
        if d < 1.0 || d >= best {
            return;
        }
        let dir = P::new(q.x / d, q.y / d);
        let l = l2.sqrt();
        let nq = left(P::new(ab.x / l, ab.y / l));
        if dir.dot(n) >= 0.5 && -dir.dot(nq) >= 0.5 {
            best = d;
        }
    });
    best
}

/// Boundary samples: every vertex, graded points at sharp convex corners,
/// then each interval split to a twelfth of the local thickness where that
/// is under `reach`. Circumcentres then sit within T/1152 of the medial
/// axis, so the slope of R along a skeleton edge reads true to about 0.04
/// (the central test compares it with sin(angle/2), 0.087 at 10°).
fn sample(region: &ExPolygon, grid: &SegGrid<u32>, reach: f64, verts: &mut Vec<Vert>) -> Samples {
    let mut s = Samples { pts: Vec::new(), ring: Vec::new(), rings: Vec::new(), vert: Vec::new() };
    let sharp = (15f64).to_radians().sin();
    let mut lo = Vec::new();
    let mut hi = Vec::new();
    let mut mid = Vec::new();
    for (r, ring) in region.rings().enumerate() {
        let pts = &ring.pts;
        let n = pts.len();
        let first = s.pts.len() as u32;
        // Coarse points of the ring with their inward normals.
        let mut coarse: Vec<(Pt, P, u32)> = Vec::new();
        let convex: Vec<bool> = (0..n)
            .map(|i| {
                let (u1, u2) = (unit(pts[(i + n - 1) % n], pts[i]), unit(pts[i], pts[(i + 1) % n]));
                u1.x * u2.y - u1.y * u2.x > sharp
            })
            .collect();
        for i in 0..n {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            let (u1, u2) = (unit(pts[(i + n - 1) % n], a), unit(a, b));
            let nv = left(u1).add(left(u2), 1.0);
            let nl = nv.x.hypot(nv.y).max(1e-12);
            verts.push(Vert { u1, u2 });
            coarse.push((a, P::new(nv.x / nl, nv.y / nl), verts.len() as u32 - 1));
            let len = a.dist(b);
            let ne = left(u2);
            let at = |d: f64| lerp(a, b, d / len);
            let half = len / 2.0;
            lo.clear();
            hi.clear();
            mid.clear();
            let mut g = GRADE;
            while g < COARSE.min(half) {
                if convex[i] {
                    lo.push(g);
                }
                if convex[(i + 1) % n] {
                    hi.push(len - g);
                }
                g *= 2.0;
            }
            let start = lo.last().copied().unwrap_or(0.0);
            let end = hi.last().copied().unwrap_or(len);
            let k = ((end - start) / COARSE).ceil().max(1.0) as usize;
            mid.extend((1..k).map(|j| start + (end - start) * j as f64 / k as f64));
            for &d in lo.iter().chain(&mid).chain(hi.iter().rev()) {
                coarse.push((at(d), ne, NONE));
            }
        }
        coarse.dedup_by(|x, y| x.0 == y.0);
        // Refine each coarse interval to a twelfth of the thickness across.
        let thick: Vec<f64> = coarse.iter().map(|&(p, nrm, _)| across(grid, p, nrm, reach)).collect();
        let m = coarse.len();
        for i in 0..m {
            let (a, _, v) = coarse[i];
            let b = coarse[(i + 1) % m].0;
            let step = (thick[i].min(thick[(i + 1) % m]) / 12.0).clamp(0.01 * MM, COARSE);
            let k = (a.dist(b) / step).ceil() as usize;
            s.pts.reserve(k.max(1));
            s.ring.reserve(k.max(1));
            s.vert.reserve(k.max(1));
            s.pts.push((a.x, a.y));
            s.ring.push(r as u32);
            s.vert.push(v);
            for j in 1..k {
                let p = lerp(a, b, j as f64 / k as f64);
                if p != a && p != b {
                    s.pts.push((p.x, p.y));
                    s.ring.push(r as u32);
                    s.vert.push(NONE);
                }
            }
        }
        s.rings.push((first, s.pts.len() as u32));
    }
    s
}

/// The half-edge of each boundary interval (sample i → next), on its inside
/// triangle; NONE when the interval is not a Delaunay edge.
fn intervals(s: &Samples, tri: &delaunay::Tri) -> Vec<u32> {
    let mut out = vec![NONE; s.pts.len()];
    for e in 0..tri.tris.len() {
        let a = tri.tris[e] as usize;
        if s.next(a) == tri.tris[next(e)] as usize {
            out[a] = e as u32;
        }
    }
    out
}

/// Split every interval that is not a Delaunay edge at its middle.
fn split_missing(s: &Samples, missing: &[u32]) -> Samples {
    let mut out = Samples { pts: Vec::new(), ring: Vec::new(), rings: Vec::new(), vert: Vec::new() };
    for (r, &(a, b)) in s.rings.iter().enumerate() {
        let first = out.pts.len() as u32;
        for i in a as usize..b as usize {
            out.pts.push(s.pts[i]);
            out.ring.push(r as u32);
            out.vert.push(s.vert[i]);
            if missing[i] != NONE {
                continue;
            }
            let (p, q) = (s.pts[i], s.pts[s.next(i)]);
            let m = (p.0 + (q.0 - p.0) / 2, p.1 + (q.1 - p.1) / 2);
            if m != p && m != q {
                out.pts.push(m);
                out.ring.push(r as u32);
                out.vert.push(NONE);
            }
        }
        out.rings.push((first, out.pts.len() as u32));
    }
    out
}

/// Foot of `p` on the polyline a → b → c: (point, arc position from a).
fn foot(p: P, a: P, b: P, c: P) -> (P, f64) {
    let mut best = (f64::INFINITY, a, 0.0);
    let mut offset = 0.0;
    for (u, v) in [(a, b), (b, c)] {
        let d = P::new(v.x - u.x, v.y - u.y);
        let dd = d.dot(d);
        let t = if dd == 0.0 { 0.0 } else { (P::new(p.x - u.x, p.y - u.y).dot(d) / dd).clamp(0.0, 1.0) };
        let q = u.lerp(v, t);
        let dist = p.dist(q);
        if dist < best.0 {
            best = (dist, q, offset + dd.sqrt() * t);
        }
        offset += dd.sqrt();
    }
    (best.1, best.2)
}

/// The skeleton of one region (local coordinates: the bounding box's lower
/// corner at the origin, at most 2^30 units across). `reach` (mm) is the
/// thickness above which the bead layout no longer changes.
pub(crate) fn build(region: &ExPolygon, transition_angle: f64, reach: f64) -> Skeleton {
    let mut segs = Vec::new();
    for ring in region.rings() {
        let n = ring.pts.len();
        for i in 0..n {
            segs.push((ring.pts[i], ring.pts[(i + 1) % n], 0u32));
        }
    }
    let grid = SegGrid::new(segs);
    let mut verts = Vec::new();
    let mut s = sample(region, &grid, reach * MM, &mut verts);
    let mut tri = delaunay::triangulate(&s.pts);
    let mut bound = tri.as_ref().map(|t| intervals(&s, t)).unwrap_or_default();
    for _ in 0..16 {
        if tri.is_none() || bound.iter().all(|&e| e != NONE) {
            break;
        }
        s = split_missing(&s, &bound);
        tri = delaunay::triangulate(&s.pts);
        bound = tri.as_ref().map(|t| intervals(&s, t)).unwrap_or_default();
    }
    let mut sk = Skeleton { nodes: Vec::new(), edges: Vec::new(), faces: Vec::new(), ribs: 0, at: vec![0], adj: Vec::new() };
    let Some(tri) = tri else { return sk };
    let ntri = tri.tris.len() / 3;
    // Inside triangles: flood from each interval's triangle, never across an
    // interval. With an interval missing, test each triangle's centroid.
    let mut inside = vec![false; ntri];
    let pt = |i: u32| s.pts[i as usize];
    if bound.iter().all(|&e| e != NONE) {
        let mut wall = vec![false; tri.tris.len()];
        let mut queue = Vec::new();
        for &e in &bound {
            wall[e as usize] = true;
            let t = tri.halfedges[e as usize];
            if t != NONE {
                wall[t as usize] = true;
            }
            let k = e as usize / 3;
            if !inside[k] {
                inside[k] = true;
                queue.push(k);
            }
        }
        while let Some(k) = queue.pop() {
            for e in 3 * k..3 * k + 3 {
                let t = tri.halfedges[e];
                if wall[e] || t == NONE {
                    continue;
                }
                let k2 = t as usize / 3;
                if !inside[k2] {
                    inside[k2] = true;
                    queue.push(k2);
                }
            }
        }
    } else {
        for k in 0..ntri {
            let (a, b, c) = (pt(tri.tris[3 * k]), pt(tri.tris[3 * k + 1]), pt(tri.tris[3 * k + 2]));
            let g = Pt::new((a.0 + b.0 + c.0) / 3, (a.1 + b.1 + c.1) / 3);
            inside[k] = delaunay::orient(a, b, c) != 0 && grid.contains_any(g);
        }
    }
    // Nodes: circumcentres (the centroid when that falls outside).
    // Neighbouring triangles whose centres nearly coincide (cocircular
    // samples, rounded to whole units) share one node: an edge that short
    // has no slope worth reading and would only seed false peaks.
    let mut centre = vec![P::default(); ntri];
    for k in 0..ntri {
        if !inside[k] {
            continue;
        }
        let (a, b, c) = (pt(tri.tris[3 * k]), pt(tri.tris[3 * k + 1]), pt(tri.tris[3 * k + 2]));
        let mut p = match delaunay::circumcentre(a, b, c) {
            Some((x, y)) if x.is_finite() && y.is_finite() => P::new(x, y),
            _ => P::new(f64::NAN, 0.0),
        };
        if !p.x.is_finite() || !grid.contains_any(p.pt()) {
            p = P::new((a.0 + b.0 + c.0) as f64 / 3.0, (a.1 + b.1 + c.1) as f64 / 3.0);
        }
        centre[k] = p;
    }
    let mut root: Vec<u32> = (0..ntri as u32).collect();
    fn find(root: &mut [u32], mut k: u32) -> u32 {
        while root[k as usize] != k {
            root[k as usize] = root[root[k as usize] as usize];
            k = root[k as usize];
        }
        k
    }
    for e in 0..tri.tris.len() {
        let t = tri.halfedges[e];
        let (k1, k2) = (e / 3, t as usize / 3);
        if t == NONE || !inside[k1] || !inside[k2] || centre[k1].dist(centre[k2]) > MERGE {
            continue;
        }
        let (a, b) = (find(&mut root, k1 as u32), find(&mut root, k2 as u32));
        if a != b {
            root[a.max(b) as usize] = a.min(b);
        }
    }
    let mut node_of = vec![NONE; ntri];
    let mut sum: Vec<(P, f64)> = Vec::new();
    for k in 0..ntri {
        if !inside[k] {
            continue;
        }
        let r = find(&mut root, k as u32) as usize;
        if node_of[r] == NONE {
            node_of[r] = sum.len() as u32;
            sum.push((P::default(), 0.0));
        }
        node_of[k] = node_of[r];
        let acc = &mut sum[node_of[k] as usize];
        acc.0 = acc.0.add(centre[k], 1.0);
        acc.1 += 1.0;
    }
    for (p, n) in sum {
        let p = P::new(p.x / n, p.y / n);
        let r = grid.nearest(p.pt(), |t| t).map_or(0.0, |x| x.0);
        sk.nodes.push(Node { pos: p, r });
    }
    // Skeleton edges: one per Delaunay edge between two inside triangles.
    let mut edge_of = vec![NONE; tri.tris.len()];
    // Delaunay edges between the same two nodes share one: each node chains the edges it is the
    // lower end of.
    let mut head = vec![NONE; sk.nodes.len()];
    let mut pair_next = Vec::new();
    for e in 0..tri.tris.len() {
        let t = tri.halfedges[e];
        if t == NONE || (t as usize) < e {
            continue;
        }
        let (a, b) = (node_of[e / 3], node_of[t as usize / 3]);
        if a == NONE || b == NONE || a == b || bound[tri.tris[e] as usize] == e as u32 || bound[tri.tris[t as usize] as usize] == t {
            continue;
        }
        let (lo, hi) = (a.min(b) as usize, a.max(b));
        let mut id = head[lo];
        while id != NONE {
            let edge = &sk.edges[id as usize];
            if edge.a.max(edge.b) == hi {
                break;
            }
            id = pair_next[id as usize];
        }
        if id == NONE {
            id = sk.edges.len() as u32;
            sk.edges.push(Edge { a, b, central: false });
            pair_next.push(head[lo]);
            head[lo] = id;
        }
        edge_of[e] = id;
        edge_of[t as usize] = id;
    }
    // Faces, sample by sample: walk the fan from the interval before the
    // sample to the interval after it.
    let n = s.pts.len();
    let mut ribs = n as u32;
    let mid = |i: usize| P::of(s.pts[i]).lerp(P::of(s.pts[s.next(i)]), 0.5);
    let mut chain: Vec<(u32, u32)> = Vec::new();
    let mut rib: Vec<(u32, P, f64, u32)> = Vec::new();
    sk.faces.reserve(3 * inside.iter().filter(|&&v| v).count());
    for i in 0..n {
        let p = s.prev(i);
        if bound[p] == NONE || bound[i] == NONE {
            continue;
        }
        chain.clear();
        let end = bound[i] as usize;
        let mut cur = next(bound[p] as usize);
        let mut ok = true;
        for _ in 0..=tri.tris.len() {
            chain.push((node_of[cur / 3], NONE));
            if cur == end {
                break;
            }
            let t = tri.halfedges[cur];
            if t == NONE || node_of[t as usize / 3] == NONE {
                ok = false;
                break;
            }
            if let Some(last) = chain.last_mut() {
                last.1 = edge_of[cur];
            }
            cur = next(t as usize);
        }
        if !ok || cur != end || chain.iter().any(|c| c.0 == NONE) {
            continue;
        }
        let (a, b, c) = (mid(p), P::of(s.pts[i]), mid(i));
        let l = a.dist(b);
        let corner = match s.vert[i] {
            NONE => Corner::None,
            v => {
                let vt = verts[v as usize];
                let turn = vt.u1.x * vt.u2.y - vt.u1.y * vt.u2.x;
                if turn.abs() < 1e-9 {
                    Corner::None
                } else if turn > 0.0 {
                    Corner::Convex { v: b, n1: left(vt.u1), n2: left(vt.u2) }
                } else {
                    Corner::Reflex { v: b, n1: left(vt.u1), n2: left(vt.u2) }
                }
            }
        };
        // Ribs: (node, foot, position along the piece, id).
        let k = chain.len();
        rib.clear();
        rib.reserve(k + 1);
        rib.push((chain[0].0, a, 0.0, p as u32));
        let mut along: f64 = 0.0;
        for j in 1..k.saturating_sub(1) {
            let (f, at) = foot(sk.nodes[chain[j].0 as usize].pos, a, b, c);
            along = along.max(at);
            let f = if at < along { if along <= l { a.lerp(b, along / l.max(1e-12)) } else { b.lerp(c, (along - l) / c.dist(b).max(1e-12)) } } else { f };
            rib.push((chain[j].0, f, along, ribs));
            ribs += 1;
        }
        rib.push((chain[k - 1].0, c, l + b.dist(c), i as u32));
        for j in 0..rib.len() - 1 {
            let (na, fa, sa, ra) = rib[j];
            let (nb, fb, sb, rb) = rib[j + 1];
            let edge = if na == nb { NONE } else { chain[j.min(k - 1)].1 };
            let on = |x: f64| (x - l).abs() < 1e-6;
            let has = match corner {
                Corner::None => false,
                Corner::Convex { .. } => sa < l && l <= sb,
                Corner::Reflex { .. } => sa <= l && l <= sb,
            };
            sk.faces.push(Face { ribs: [ra, rb], nodes: [na, nb], feet: [fa, fb], edge, corner: if has { corner } else { Corner::None }, at_v: [on(sa), on(sb)] });
        }
    }
    sk.ribs = ribs as usize;
    // Central edges: R changes slowly along them.
    let sin = (transition_angle * 0.5).sin();
    let len: Vec<f64> = (0..sk.edges.len()).map(|e| sk.length(e)).collect();
    for (e, edge) in sk.edges.iter_mut().enumerate() {
        let (ra, rb) = (sk.nodes[edge.a as usize].r, sk.nodes[edge.b as usize].r);
        edge.central = (ra - rb).abs() <= len[e] * sin;
    }
    let mut at = vec![0u32; sk.nodes.len() + 1];
    for e in &sk.edges {
        at[e.a as usize + 1] += 1;
        at[e.b as usize + 1] += 1;
    }
    for i in 0..sk.nodes.len() {
        at[i + 1] += at[i];
    }
    let mut fill = at.clone();
    let mut adj = vec![0u32; *at.last().unwrap_or(&0) as usize];
    for (k, e) in sk.edges.iter().enumerate() {
        for v in [e.a, e.b] {
            adj[fill[v as usize] as usize] = k as u32;
            fill[v as usize] += 1;
        }
    }
    sk.at = at;
    sk.adj = adj;
    sk
}

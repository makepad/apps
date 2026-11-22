//! Cutting a triangle mesh by horizontal planes into per-layer regions.

use crate::clip::{self, FillRule};
use crate::geom::*;
use crate::mesh::Mesh;
use crate::offset::{self, Join};

/// Ignore repaired loops smaller than a square of two nominal 0.4 mm line widths.
const MIN_REPAIRED_LOOP_AREA_MM2: f64 = 0.64;

#[derive(Clone, Copy, Debug)]
pub struct SliceParams {
    /// Morphological closing radius (mm) that heals sub-resolution gaps.
    pub closing_radius: f64,
    /// Contour simplification tolerance (mm).
    pub resolution: f64,
    /// Open chains are joined across gaps up to this length (mm); any
    /// remaining gap is closed by a chord, even when it is longer.
    pub max_gap: f64,
}

impl Default for SliceParams {
    fn default() -> Self {
        Self { closing_radius: 0.049, resolution: 0.012, max_gap: 2.0 }
    }
}

/// Cut `mesh` at each height in `zs` (mm); returns one region set per plane.
pub fn slice_mesh(mesh: &Mesh, zs: &[f64], params: &SliceParams) -> Vec<Vec<ExPolygon>> {
    SliceIndex::new(mesh).cut(mesh, zs, params)
}

/// A mesh's triangles sorted by their lowest z: built once, it cuts any set
/// of planes (any layer range, one plane at a time, on any thread) without sorting again.
pub struct SliceIndex {
    /// (lowest z, highest z, triangle), ascending by lowest z.
    tri_z: Vec<(f64, f64, u32)>,
    /// The highest z of each run of [`RUN`] triangles of `tri_z`: a plane skips the runs
    /// that end below it.
    run_top: Vec<f64>,
    /// The mesh's [`plane_tolerance`].
    tol: f64,
}

/// Triangles per run of [`SliceIndex::run_top`].
const RUN: usize = 64;

impl SliceIndex {
    pub fn new(mesh: &Mesh) -> Self {
        let mut tri_z: Vec<(f64, f64, u32)> = mesh
            .tris
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let z = [mesh.verts[t[0] as usize][2], mesh.verts[t[1] as usize][2], mesh.verts[t[2] as usize][2]];
                (z[0].min(z[1]).min(z[2]), z[0].max(z[1]).max(z[2]), i as u32)
            })
            .collect();
        tri_z.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let run_top = tri_z.chunks(RUN).map(|r| r.iter().fold(f64::MIN, |m, t| m.max(t.1))).collect();
        Self { tri_z, run_top, tol: plane_tolerance(mesh) }
    }

    /// The regions at each height in `zs` (mm), one set per plane.
    pub fn cut(&self, mesh: &Mesh, zs: &[f64], params: &SliceParams) -> Vec<Vec<ExPolygon>> {
        zs.iter().map(|&z| segments_to_regions(cut_triangles(mesh, self.at(z), z, self.tol), params)).collect()
    }

    /// The triangles that reach plane `z` (lowest z <= z, highest z on or above it within
    /// the tolerance), lowest z first.
    fn at(&self, z: f64) -> impl Iterator<Item = u32> + '_ {
        let end = self.tri_z.partition_point(|t| t.0 <= z);
        let low = z - self.tol;
        (0..end.div_ceil(RUN))
            .filter(move |&r| self.run_top[r] >= low)
            .flat_map(move |r| &self.tri_z[r * RUN..((r + 1) * RUN).min(end)])
            .filter(move |t| t.1 >= low)
            .map(|t| t.2)
    }
}

/// How far (mm) a vertex may sit from a cut plane and still lie on it: far below any feature a
/// mesh models, far above the rounding of f64 coordinates that went through different transforms
/// and booleans (the boolean kernel's rule: 1e-9 of the largest coordinate, never below 1e-9).
/// A face modelled at a layer's height then cuts as that face, never as a sliver of its
/// rounding.
fn plane_tolerance(mesh: &Mesh) -> f64 {
    mesh.verts.iter().fold(1.0f64, |m, v| m.max(v[0].abs()).max(v[1].abs()).max(v[2].abs())) * 1e-9
}

/// Where edge (i, j) crosses the plane, computed from the lower vertex index
/// first so both triangles sharing the edge get the identical point; an end on
/// the plane (within `tol`) is that point.
fn edge_point(mesh: &Mesh, i: u32, j: u32, z: f64, tol: f64) -> Pt {
    let (i, j) = if i < j { (i, j) } else { (j, i) };
    let a = mesh.verts[i as usize];
    let b = mesh.verts[j as usize];
    for v in [a, b] {
        if (v[2] - z).abs() <= tol {
            return Pt::from_mm(v[0], v[1]);
        }
    }
    let t = (z - a[2]) / (b[2] - a[2]);
    Pt::from_mm(a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t)
}

/// On-plane vertices (within `tol`, see [`plane_tolerance`]) count as above:
/// an on-plane edge is emitted only by a triangle extending below it.
/// Coplanar triangles and point touches emit no segments.
fn cut_triangles(mesh: &Mesh, tris: impl Iterator<Item = u32>, z: f64, tol: f64) -> Vec<(Pt, Pt)> {
    let mut segs = Vec::new();
    let low = z - tol;
    for ti in tris {
        let t = mesh.tris[ti as usize];
        // Vertices on the plane count as above.
        let above = [mesh.verts[t[0] as usize][2] >= low, mesh.verts[t[1] as usize][2] >= low, mesh.verts[t[2] as usize][2] >= low];
        let n_above = above.iter().filter(|a| **a).count();
        if n_above == 0 || n_above == 3 {
            continue;
        }
        // Walk the edges in order: the cut runs from where an edge goes down
        // through the plane to where one comes back up, which keeps the
        // material on the left.
        let mut down = None;
        let mut up = None;
        for k in 0..3 {
            let (a, b) = (k, (k + 1) % 3);
            if above[a] && !above[b] {
                down = Some(edge_point(mesh, t[a], t[b], z, tol));
            } else if !above[a] && above[b] {
                up = Some(edge_point(mesh, t[a], t[b], z, tol));
            }
        }
        if let (Some(p), Some(q)) = (down, up) {
            if p != q {
                segs.push((p, q));
            }
        }
    }
    segs
}

/// Use every directed segment once, collecting closed loops and open chains.
fn chain_segments(segs: &[(Pt, Pt)]) -> (Vec<Polygon>, Vec<Vec<Pt>>) {
    // Segment indices sorted by start point (stable: index order within one
    // start), searched by start point.
    let mut by_start: Vec<u32> = (0..segs.len() as u32).collect();
    by_start.sort_by_key(|&i| segs[i as usize].0);
    let starting_at = |p: Pt| -> &[u32] {
        let lo = by_start.partition_point(|&i| segs[i as usize].0 < p);
        let hi = lo + by_start[lo..].partition_point(|&i| segs[i as usize].0 == p);
        &by_start[lo..hi]
    };
    let mut used = vec![false; segs.len()];
    let mut loops: Vec<Polygon> = Vec::new();
    let mut open: Vec<Vec<Pt>> = Vec::new();
    for i0 in 0..segs.len() {
        if used[i0] {
            continue;
        }
        used[i0] = true;
        let mut path = vec![segs[i0].0, segs[i0].1];
        let start = segs[i0].0;
        loop {
            let end = *path.last().unwrap();
            if end == start {
                path.pop();
                loops.push(Polygon::new(path));
                break;
            }
            let next = starting_at(end).iter().map(|&j| j as usize).find(|&j| !used[j]);
            match next {
                Some(j) => {
                    used[j] = true;
                    path.push(segs[j].1);
                }
                None => {
                    open.push(path);
                    break;
                }
            }
        }
    }
    debug_assert!(used.iter().all(|&u| u));
    debug_assert_eq!(loops.iter().map(Polygon::len).sum::<usize>() + open.iter().map(|p| p.len() - 1).sum::<usize>(), segs.len());
    (loops, open)
}

/// Chain cut segments into loops, join nearby open chains, then close every
/// remaining chain of at least three points with an end-to-start chord.
/// Only repaired loops below 0.64 mm² (two line widths squared) are discarded.
///
/// Resolve loops with the non-zero fill rule: overlapping skins with the
/// same orientation have winding 2 and stay solid; a reversed loop inside
/// a positive loop cancels its winding and makes a hole. Winding adds over
/// the directed segment multiset, so junction pairing does not change the
/// union when every segment is used exactly once and every chain closes.
pub fn segments_to_regions(segs: Vec<(Pt, Pt)>, params: &SliceParams) -> Vec<ExPolygon> {
    if segs.is_empty() {
        return Vec::new();
    }
    let (mut loops, mut open) = chain_segments(&segs);
    // Join open chains end-to-start across gaps.
    let max_gap = scaled(params.max_gap) as f64;
    while !open.is_empty() {
        let mut chain = open.pop().unwrap();
        loop {
            let end = *chain.last().unwrap();
            // Closing onto itself?
            let self_gap = end.dist(chain[0]);
            let mut best: Option<(f64, usize)> = None;
            for (k, o) in open.iter().enumerate() {
                let d = end.dist(o[0]);
                if d <= max_gap && best.map_or(true, |b| d < b.0) {
                    best = Some((d, k));
                }
            }
            match best {
                Some((d, k)) if d < self_gap || chain.len() < 3 => {
                    let o = open.swap_remove(k);
                    chain.extend(o);
                }
                _ => {
                    if chain.len() >= 3 {
                        // Polygon's implicit closing edge repairs even a
                        // large missing wall; preserve either orientation.
                        let polygon = Polygon::new(chain);
                        if polygon.area_mm2().abs() >= MIN_REPAIRED_LOOP_AREA_MM2 {
                            loops.push(polygon);
                        }
                    }
                    break;
                }
            }
        }
    }
    loops_to_regions(loops, params)
}

/// Closed directed loops, including holes, through the slicer's normal region
/// construction. GPU traces already chain by edge identity before reaching here.
pub fn loops_to_regions(loops: Vec<Polygon>, params: &SliceParams) -> Vec<ExPolygon> {
    let polys: Vec<Polygon> = loops.into_iter().filter(|l| l.len() >= 3).collect();
    let mut regions = clip::union(&polys, FillRule::NonZero);
    if params.closing_radius > 0.0 {
        let r = params.closing_radius * SCALE;
        // A closing that closes no gap and changes no corner would only round the regions again.
        if !offset::closing_keeps(&regions, r, 3.0) {
            regions = offset::closing(&regions, r, r, Join::Miter(3.0));
        }
    }
    if params.resolution > 0.0 {
        let tol = params.resolution * SCALE;
        let mut simplified = Vec::with_capacity(regions.len());
        for e in regions {
            let contour = Polygon::new(simplify_ring(&e.contour.pts, tol));
            if contour.area2() <= 0 {
                continue;
            }
            let holes = e.holes.iter().map(|h| Polygon::new(simplify_ring(&h.pts, tol))).filter(|h| h.area2() < 0).collect();
            simplified.push(ExPolygon { contour, holes });
        }
        regions = simplified;
    }
    regions
}

/// Regions of a level set's loops, by nesting alone, with no boolean: the outermost rings
/// are contours, the rings directly inside them their holes, the rings inside those contours
/// again. Each ring is oriented by its depth and simplified to `resolution` (mm), as
/// `loops_to_regions` does.
///
/// A level set's loops may touch: where a surface is tangent to the cutting plane's grid (a
/// sheet grazing a face) one loop passes the same point twice. Such a loop is split at that
/// point into simple rings, as a union splits it, so the regions have the union's topology.
/// Rings may still share vertices with other rings. Any other contact (a crossing, an
/// overlap, a vertex on another ring's edge) leaves nesting undefined: those loops take the
/// union instead.
pub fn nested_loops_to_regions(loops: Vec<Polygon>, resolution: f64) -> Vec<ExPolygon> {
    let mut loops: Vec<Polygon> = loops.into_iter().filter(|l| l.len() >= 3).collect();
    let union = |loops| loops_to_regions(loops, &SliceParams { closing_radius: 0.0, resolution, max_gap: 0.0 });
    let found = contacts(&loops);
    if found.tangled { return union(loops) }
    if !found.pinches.is_empty() {
        let mut pinches = found.pinches;
        pinches.sort_unstable();
        pinches.dedup();
        let split: Vec<Polygon> = loops.iter().enumerate().flat_map(|(r, l)| {
            let at = pinches.partition_point(|p| p.0 < r as u32);
            let marks: Vec<u32> = pinches[at..].iter().take_while(|p| p.0 == r as u32).map(|p| p.1).collect();
            if marks.is_empty() { vec![l.clone()] } else { split_at_pinches(l, &marks) }
        }).collect();
        let again = contacts(&split);
        if again.tangled || !again.pinches.is_empty() { return union(loops) }
        loops = split;
    }
    let mut rings: Vec<(Polygon, i128, BBox)> = loops.into_iter().filter_map(|l| {
        let area = l.area2().abs();
        let bbox = l.bbox();
        (area > 0).then_some((l, area, bbox))
    }).collect();
    // Largest first: a ring's parent is the smallest larger ring containing it.
    rings.sort_by(|a, b| b.1.cmp(&a.1));
    let inside = |outer: &BBox, inner: &BBox| outer.min.x <= inner.min.x && outer.min.y <= inner.min.y && outer.max.x >= inner.max.x && outer.max.y >= inner.max.y;
    let mut depth = vec![0usize; rings.len()];
    let mut parent = vec![usize::MAX; rings.len()];
    for i in 0..rings.len() {
        // The first edge's midpoint: never a vertex, so never a point where two loops of the
        // zero set touch (they meet only at isolated saddle vertices).
        let (a, b) = (rings[i].0.pts[0], rings[i].0.pts[1]);
        let p = Pt { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
        if let Some(j) = (0..i).rev().find(|&j| inside(&rings[j].2, &rings[i].2) && rings[j].0.contains(p)) {
            parent[i] = j;
            depth[i] = depth[j] + 1;
        }
    }
    let tol = resolution * SCALE;
    let mut slot = vec![usize::MAX; rings.len()];
    let mut regions: Vec<ExPolygon> = Vec::new();
    for (i, (ring, _, _)) in rings.into_iter().enumerate() {
        let mut ring = Polygon::new(simplify_ring(&ring.pts, tol));
        if depth[i] % 2 == 0 {
            if !ring.is_ccw() { ring.reverse(); }
            if ring.area2() <= 0 { continue; }
            slot[i] = regions.len();
            regions.push(ExPolygon::new(ring));
        } else if slot[parent[i]] != usize::MAX {
            if ring.is_ccw() { ring.reverse(); }
            if ring.area2() < 0 { regions[slot[parent[i]]].holes.push(ring); }
        }
    }
    regions
}

/// Points closer than this (100 nm) are one point where loops touch.
const TOUCH: i64 = 100;

/// Where rings meet other than at neighbouring edges' shared vertex.
#[derive(Default)]
struct Contacts {
    /// Rings cross, overlap or touch each other away from a shared vertex: nesting by a
    /// sample point is undefined there.
    tangled: bool,
    /// (ring, vertex) where a ring comes back to one of its own vertices: a pinch to split.
    pinches: Vec<(u32, u32)>,
}

/// The rings' contacts. Edges are bucketed by the quarter-millimetre cells their box (grown by
/// [`TOUCH`]) covers, so edges that meet share a cell; most pairs in a cell fail the box test.
fn contacts(rings: &[Polygon]) -> Contacts {
    const CELL: i64 = 250_000;
    let mut cells: Vec<(u64, u32, u32)> = Vec::with_capacity(rings.iter().map(|r| r.pts.len()).sum::<usize>() * 2);
    for (r, ring) in rings.iter().enumerate() {
        let n = ring.pts.len();
        for k in 0..n {
            let (a, b) = (ring.pts[k], ring.pts[(k + 1) % n]);
            for cx in (a.x.min(b.x) - TOUCH).div_euclid(CELL)..=(a.x.max(b.x) + TOUCH).div_euclid(CELL) {
                for cy in (a.y.min(b.y) - TOUCH).div_euclid(CELL)..=(a.y.max(b.y) + TOUCH).div_euclid(CELL) {
                    cells.push((((cx as u32 as u64) << 32) | cy as u32 as u64, r as u32, k as u32));
                }
            }
        }
    }
    cells.sort_unstable_by_key(|c| c.0);
    let edge = |r: u32, k: u32| {
        let ring = &rings[r as usize].pts;
        (ring[k as usize], ring[(k as usize + 1) % ring.len()])
    };
    let mut found = Contacts::default();
    for list in cells.chunk_by(|a, b| a.0 == b.0) {
        for (i, &(_, r, k)) in list.iter().enumerate() {
            let (a, b) = edge(r, k);
            for &(_, q, m) in &list[i + 1..] {
                let n = rings[r as usize].pts.len() as u32;
                // neighbouring edges of one ring share their vertex
                if r == q && ((k + 1) % n == m || (m + 1) % n == k) { continue }
                let (c, d) = edge(q, m);
                if a.x.min(b.x) > c.x.max(d.x) + TOUCH || c.x.min(d.x) > a.x.max(b.x) + TOUCH
                    || a.y.min(b.y) > c.y.max(d.y) + TOUCH || c.y.min(d.y) > a.y.max(b.y) + TOUCH { continue }
                match edges_meet((a, b), (c, d)) {
                    Meet::Apart => {}
                    Meet::Vertex if r != q => {}
                    Meet::Vertex => found.pinches.extend([(r, k), (r, (k + 1) % n), (q, m), (q, (m + 1) % n)]),
                    Meet::Tangled => found.tangled = true,
                }
            }
        }
    }
    found
}

enum Meet { Apart, Vertex, Tangled }

/// How two edges meet: not at all, at a vertex they share (within [`TOUCH`]) and nowhere
/// else, or tangled (crossing, overlapping, or a vertex on the other edge).
fn edges_meet((a, b): (Pt, Pt), (c, d): (Pt, Pt)) -> Meet {
    let orient = |p: Pt, q: Pt, r: Pt| ((q.x - p.x) as i128 * (r.y - p.y) as i128 - (q.y - p.y) as i128 * (r.x - p.x) as i128).signum();
    if orient(a, b, c) * orient(a, b, d) < 0 && orient(c, d, a) * orient(c, d, b) < 0 { return Meet::Tangled }
    let on = |p: Pt, (s, t): (Pt, Pt)| {
        let (dx, dy) = ((t.x - s.x) as f64, (t.y - s.y) as f64);
        let (px, py) = ((p.x - s.x) as f64, (p.y - s.y) as f64);
        let u = ((px * dx + py * dy) / (dx * dx + dy * dy).max(1.0)).clamp(0.0, 1.0);
        (px - dx * u).hypot(py - dy * u) <= TOUCH as f64
    };
    let same = |p: Pt, q: Pt| (p.x - q.x).abs() <= TOUCH && (p.y - q.y).abs() <= TOUCH;
    // Away from a shared vertex, any contact is an overlap or a touch.
    if [(a, (c, d)), (b, (c, d)), (c, (a, b)), (d, (a, b))].into_iter().any(|(p, e)| !(same(p, e.0) || same(p, e.1)) && on(p, e)) { return Meet::Tangled }
    if [(a, c), (a, d), (b, c), (b, d)].into_iter().any(|(p, q)| same(p, q)) { Meet::Vertex } else { Meet::Apart }
}

/// `ring` split where it comes back (within [`TOUCH`]) to one of its own `pinch` vertices:
/// the vertices of one pinch take the position of the first of them, and [`split_ring_at`]
/// cuts the ring there into rings that each pass every point once and keep its direction.
fn split_at_pinches(ring: &Polygon, pinch: &[u32]) -> Vec<Polygon> {
    let mut pts = ring.pts.clone();
    let mut marks: Vec<Pt> = Vec::with_capacity(pinch.len());
    for &i in pinch {
        let p = pts[i as usize];
        match marks.iter().find(|q| (q.x - p.x).abs() <= TOUCH && (q.y - p.y).abs() <= TOUCH) {
            Some(&q) => pts[i as usize] = q,
            None => marks.push(p),
        }
    }
    split_ring_at(&pts, &marks).into_iter().filter(|piece| piece.len() >= 3).collect()
}

#[cfg(test)]
#[path = "slice_tests.rs"]
mod tests;

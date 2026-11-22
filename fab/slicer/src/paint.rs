//! Colour paint: the per-triangle paint trees of a mesh (the 3MF
//! `paint_color` bit stream) and the split of an object's slices into one
//! region per paint state on every layer.
//!
//! State 0 is the object's own filament; state N is filament N (1-based).
//!
//! Per layer:
//! 1. every contour edge takes the state of the nearest painted (or
//!    unpainted) triangle cut by the plane;
//! 2. an island whose edges all carry one state takes it whole; otherwise
//!    each interior point goes to the nearest painted boundary (a distance
//!    field over the island, contoured);
//! 3. painted up- and down-facing areas push their state a few layers into
//!    the part, shrinking inward each layer;
//! 4. optionally, paint only keeps a band along the outer surface.

use crate::clip::{difference_ex, intersection_ex, union_ex};
use crate::geom::*;
use crate::mesh::Mesh;
use crate::offset::{offset_ex, opening, DEFAULT_MITER};
use crate::slice::{segments_to_regions, SliceParams};

/// A leaf of a triangle's paint tree.
#[derive(Clone, Copy, Debug)]
pub struct PaintedTri {
    pub v: [[f64; 3]; 3],
    pub state: u32,
}

struct Bits {
    nibbles: Vec<u8>,
    pos: usize,
}

impl Bits {
    /// The hex string is written most significant digit first: the stream
    /// starts at the last character, each digit read from its low bit.
    fn new(hex: &str) -> Option<Self> {
        let mut nibbles = Vec::with_capacity(hex.len());
        for c in hex.bytes().rev() {
            let v = match c {
                b'0'..=b'9' => c - b'0',
                b'A'..=b'F' => c - b'A' + 10,
                _ => return None,
            };
            nibbles.push(v);
        }
        Some(Self { nibbles, pos: 0 })
    }
    fn bit(&mut self) -> Option<u32> {
        let n = *self.nibbles.get(self.pos / 4)?;
        let b = (n >> (self.pos % 4)) & 1;
        self.pos += 1;
        Some(b as u32)
    }
    fn take(&mut self, n: usize) -> Option<u32> {
        let mut v = 0;
        for i in 0..n {
            v |= self.bit()? << i;
        }
        Some(v)
    }
}

fn mid(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0, (a[2] + b[2]) / 2.0]
}

/// The children of a split triangle, in stream order's reverse (child 0
/// first). Side k runs from corner k to corner k + 1.
///
/// One split side: its midpoint joins the opposite corner. Two: the
/// special side is the one left whole; the corner between the split sides
/// is cut off and the rest halved. Three: the four midpoint triangles.
fn children(v: [[f64; 3]; 3], sides: u32, special: u32) -> Vec<[[f64; 3]; 3]> {
    let s = special as usize % 3;
    let (a, b, c) = (v[s], v[(s + 1) % 3], v[(s + 2) % 3]);
    match sides {
        1 => {
            let m = mid(a, b);
            vec![[m, b, c], [a, m, c]]
        }
        2 => {
            let m1 = mid(b, c);
            let m2 = mid(c, a);
            vec![[m2, m1, c], [a, b, m1], [a, m1, m2]]
        }
        _ => {
            let (m0, m1, m2) = (mid(v[0], v[1]), mid(v[1], v[2]), mid(v[2], v[0]));
            vec![[v[0], m0, m2], [m0, v[1], m1], [m2, m1, v[2]], [m0, m1, m2]]
        }
    }
}

fn decode_node(bits: &mut Bits, v: [[f64; 3]; 3], depth: u32, out: &mut Vec<PaintedTri>) -> Option<()> {
    let sides = bits.take(2)?;
    if sides > 0 {
        if depth > 24 {
            return None;
        }
        let special = bits.take(2)?;
        let kids = children(v, sides, special);
        // Written last child first.
        for k in kids.iter().rev() {
            decode_node(bits, *k, depth + 1, out)?;
        }
        return Some(());
    }
    let mut state = bits.take(2)?;
    if state == 3 {
        loop {
            let n = bits.take(4)?;
            state += n;
            if n != 15 {
                break;
            }
        }
    }
    out.push(PaintedTri { v, state });
    Some(())
}

/// The painted leaves of one triangle; `None` for a malformed stream.
pub fn decode(hex: &str, v: [[f64; 3]; 3]) -> Option<Vec<PaintedTri>> {
    let mut bits = Bits::new(hex)?;
    let mut out = Vec::new();
    decode_node(&mut bits, v, 0, &mut out)?;
    Some(out)
}

/// The paint stream of a whole triangle in one state (0: unpainted, "").
pub fn encode_leaf(state: u32) -> String {
    if state == 0 {
        return String::new();
    }
    // Bits in stream order: no split (2 bits), then the state.
    let mut bits: Vec<u8> = vec![0, 0];
    let put = |v: u32, n: usize, bits: &mut Vec<u8>| (0..n).for_each(|i| bits.push(((v >> i) & 1) as u8));
    if state < 3 {
        put(state, 2, &mut bits);
    } else {
        put(3, 2, &mut bits);
        let mut rest = state - 3;
        loop {
            let n = rest.min(15);
            put(n, 4, &mut bits);
            rest -= n;
            if n != 15 {
                break;
            }
        }
    }
    // Four bits per digit, low bit first; the stream starts at the last digit.
    bits.chunks(4).rev().map(|c| char::from_digit(c.iter().enumerate().map(|(i, b)| (*b as u32) << i).sum(), 16).unwrap().to_ascii_uppercase()).collect()
}

/// Every triangle of a mesh as paint leaves (unpainted triangles as one
/// state-0 leaf). Empty when nothing on the mesh is painted.
pub fn leaves(mesh: &Mesh) -> Vec<PaintedTri> {
    if mesh.paint.iter().all(|p| p.is_empty()) {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(mesh.tris.len());
    for (i, t) in mesh.tris.iter().enumerate() {
        let v = [mesh.verts[t[0] as usize], mesh.verts[t[1] as usize], mesh.verts[t[2] as usize]];
        let hex = mesh.paint.get(i).map(|s| s.as_str()).unwrap_or("");
        match if hex.is_empty() { None } else { decode(hex, v) } {
            Some(l) => out.extend(l),
            None => out.push(PaintedTri { v, state: 0 }),
        }
    }
    out
}

#[derive(Clone, Copy, Debug)]
pub struct PaintParams {
    /// Layers a painted top surface reaches down (`top_color_penetration_layers`).
    pub top_layers: usize,
    /// Layers a painted bottom surface reaches up.
    pub bottom_layers: usize,
    /// Inward shrink per extra penetration layer (mm): a wall width plus a
    /// spacing.
    pub shrink: f64,
    /// Paint only within this distance of the outer surface (0 = all).
    pub max_width: f64,
    /// Added to `max_width` on odd layers (a zig-zag interlock).
    pub interlocking_depth: f64,
}

/// One layer's split: (state, regions) for every state present, state 0
/// first; together they cover the layer's slices.
pub type LayerPaint = Vec<(u32, Vec<ExPolygon>)>;

/// Segment where a triangle crosses the plane z.
fn cut(t: &PaintedTri, z: f64) -> Option<(Pt, Pt)> {
    let mut pts = Vec::with_capacity(2);
    for k in 0..3 {
        let (a, b) = (t.v[k], t.v[(k + 1) % 3]);
        if (a[2] < z) != (b[2] < z) {
            let s = (z - a[2]) / (b[2] - a[2]);
            pts.push(Pt::from_mm(a[0] + (b[0] - a[0]) * s, a[1] + (b[1] - a[1]) * s));
        }
    }
    if pts.len() == 2 && pts[0] != pts[1] {
        Some((pts[0], pts[1]))
    } else {
        None
    }
}

fn normal_z(t: &PaintedTri) -> f64 {
    let (a, b, c) = (t.v[0], t.v[1], t.v[2]);
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len <= 0.0 {
        0.0
    } else {
        n[2] / len
    }
}

/// The XY footprint of a triangle as a region.
fn footprint(t: &PaintedTri) -> Option<ExPolygon> {
    let mut pts: Vec<Pt> = t.v.iter().map(|p| Pt::from_mm(p[0], p[1])).collect();
    let mut poly = Polygon::new(std::mem::take(&mut pts));
    if poly.area2() == 0 {
        return None;
    }
    if !poly.is_ccw() {
        poly.reverse();
    }
    Some(ExPolygon::new(poly))
}

/// Split `slices[i]` (cut at plane `zs[i]`) by the paint of `tris`.
pub fn split_layers(tris: &[PaintedTri], zs: &[f64], slices: &[Vec<ExPolygon>], p: &PaintParams) -> Vec<LayerPaint> {
    let n = zs.len();
    let base = |i: usize| -> LayerPaint { vec![(0, slices[i].clone())] };
    if tris.iter().all(|t| t.state == 0) {
        return (0..n).map(base).collect();
    }
    // Triangles by lowest z for the plane sweep.
    let mut order: Vec<usize> = (0..tris.len()).collect();
    let zmin = |t: &PaintedTri| t.v[0][2].min(t.v[1][2]).min(t.v[2][2]);
    let zmax = |t: &PaintedTri| t.v[0][2].max(t.v[1][2]).max(t.v[2][2]);
    order.sort_by(|&a, &b| zmin(&tris[a]).partial_cmp(&zmin(&tris[b])).unwrap());
    let mut planes: Vec<usize> = (0..n).collect();
    planes.sort_by(|&a, &b| zs[a].partial_cmp(&zs[b]).unwrap());
    // The cut segments per plane (one sweep), then every layer split in parallel.
    let mut cuts: Vec<Vec<(Pt, Pt, u32)>> = vec![Vec::new(); n];
    let mut next = 0;
    let mut active: Vec<usize> = Vec::new();
    for &li in &planes {
        let z = zs[li];
        while next < order.len() && zmin(&tris[order[next]]) <= z {
            active.push(order[next]);
            next += 1;
        }
        active.retain(|&t| zmax(&tris[t]) >= z);
        cuts[li] = active.iter().filter_map(|&t| cut(&tris[t], z).map(|(a, b)| (a, b, tris[t].state))).collect();
    }
    let flats = |up: bool, layers: usize| -> Vec<Flat> {
        if layers == 0 {
            return Vec::new();
        }
        tris.iter()
            .filter(|t| t.state != 0 && if up { normal_z(t) > 1e-3 } else { normal_z(t) < -1e-3 })
            .filter_map(|t| footprint(t).map(|e| ((t.v[0][2] + t.v[1][2] + t.v[2][2]) / 3.0, t.state, e)))
            .collect()
    };
    split_cuts(&cuts, [flats(true, p.top_layers), flats(false, p.bottom_layers)], zs, slices, p)
}

/// A painted flat face: its height, state and XY footprint.
pub type Flat = (f64, u32, ExPolygon);

/// Split `slices[i]` (cut at plane `zs[i]`) given each plane's painted cut
/// segments `cuts[i]` (state per segment, 0 = the object's own) and the
/// painted up / down facing flats `[up, down]` that reach `top_layers` /
/// `bottom_layers` into the part. Any contour source with colour (a painted
/// mesh, a field with material slots) splits through here.
pub fn split_cuts(cuts: &[Vec<(Pt, Pt, u32)>], flats: [Vec<Flat>; 2], zs: &[f64], slices: &[Vec<ExPolygon>], p: &PaintParams) -> Vec<LayerPaint> {
    let n = zs.len();
    let base = |i: usize| -> LayerPaint { vec![(0, slices[i].clone())] };
    let mut out: Vec<LayerPaint> = crate::par::map(n, |li| {
        let segs = &cuts[li];
        if segs.iter().all(|s| s.2 == 0) { base(li) } else { split_layer(&slices[li], segs) }
    });
    // Painted tops and bottoms reach into the part.
    let mut layer_order: Vec<usize> = (0..n).collect();
    layer_order.sort_by(|&a, &b| zs[a].partial_cmp(&zs[b]).unwrap());
    let mut push: Vec<Vec<(u32, Vec<ExPolygon>)>> = vec![Vec::new(); n];
    for (up, layers, faces) in [(true, p.top_layers, &flats[0]), (false, p.bottom_layers, &flats[1])] {
        if layers == 0 {
            continue;
        }
        if faces.is_empty() {
            continue;
        }
        for (k, &li) in layer_order.iter().enumerate() {
            // Faces between this plane and the next one out of the part.
            let (lo, hi) = if up {
                (zs[li], layer_order.get(k + 1).map(|&j| zs[j]).unwrap_or(f64::MAX))
            } else {
                (if k > 0 { zs[layer_order[k - 1]] } else { f64::MIN }, zs[li])
            };
            let mut by_state: Vec<(u32, Vec<ExPolygon>)> = Vec::new();
            for (fz, st, e) in faces.iter() {
                if *fz > lo && *fz <= hi {
                    match by_state.iter_mut().find(|b| b.0 == *st) {
                        Some(b) => b.1.push(e.clone()),
                        None => by_state.push((*st, vec![e.clone()])),
                    }
                }
            }
            for (st, polys) in by_state {
                // Where this layer is exposed to the face side.
                let next_slices: &[ExPolygon] = if up {
                    layer_order.get(k + 1).map(|&j| slices[j].as_slice()).unwrap_or(&[])
                } else if k > 0 {
                    &slices[layer_order[k - 1]]
                } else {
                    &[]
                };
                let exposed = difference_ex(&slices[li], next_slices);
                let area = intersection_ex(&exposed, &union_ex(&polys));
                if area.is_empty() {
                    continue;
                }
                for d in 0..layers {
                    let target = if up { k.checked_sub(d) } else { Some(k + d).filter(|&x| x < n) };
                    let Some(t) = target else { break };
                    let tl = layer_order[t];
                    let grown = if d == 0 { area.clone() } else { offset_ex(&area, -(d as f64) * p.shrink * SCALE, DEFAULT_MITER) };
                    let clipped = intersection_ex(&grown, &slices[tl]);
                    // An opening's round trip moves points a unit or two: grown a little further and
                    // cut back, its edges along the island are the island's, and the other states'
                    // regions lose exactly those edges with no ring left between them.
                    let clipped = opening(&clipped, 0.1 * SCALE, 0.1 * SCALE + 100.0, DEFAULT_MITER);
                    let clipped = intersection_ex(&clipped, &slices[tl]);
                    if clipped.is_empty() {
                        break;
                    }
                    push[tl].push((st, clipped));
                }
            }
        }
    }
    for li in 0..n {
        if push[li].is_empty() {
            continue;
        }
        // Lower filaments win overlaps: apply the highest first.
        let mut adds = std::mem::take(&mut push[li]);
        adds.sort_by(|a, b| b.0.cmp(&a.0));
        for (st, area) in adds {
            for (s, r) in out[li].iter_mut() {
                if *s != st {
                    *r = difference_ex(r, &area);
                }
            }
            match out[li].iter_mut().find(|x| x.0 == st) {
                Some((_, r)) => *r = union_ex(&[r.clone(), area].concat()),
                None => out[li].push((st, area)),
            }
        }
    }
    // Paint only near the outer surface.
    if p.max_width > 0.0 {
        for (k, &li) in layer_order.iter().enumerate() {
            let w = p.max_width + if k % 2 == 1 { p.interlocking_depth } else { 0.0 };
            let core = offset_ex(&slices[li], -w * SCALE, DEFAULT_MITER);
            if core.is_empty() {
                continue;
            }
            let mut back = Vec::new();
            for (s, r) in out[li].iter_mut() {
                if *s != 0 {
                    back.extend(intersection_ex(r, &core));
                    *r = difference_ex(r, &core);
                }
            }
            if !back.is_empty() {
                match out[li].iter_mut().find(|x| x.0 == 0) {
                    Some((_, r)) => *r = union_ex(&[r.clone(), back].concat()),
                    None => out[li].push((0, union_ex(&back))),
                }
            }
        }
    }
    // The first layer prints in the object's own filament whatever its
    // paint (as the reference output keeps it: layer 1 in the base filament,
    // paint from layer 2 on).
    if let Some(&first) = layer_order.first() {
        out[first] = base(first);
    }
    for l in out.iter_mut() {
        l.retain(|(_, r)| !r.is_empty());
        l.sort_by_key(|x| x.0);
    }
    out
}

/// One layer: every island split by the states along its boundary.
fn split_layer(slices: &[ExPolygon], segs: &[(Pt, Pt, u32)]) -> LayerPaint {
    let mut out: LayerPaint = Vec::new();
    let mut add = |st: u32, r: Vec<ExPolygon>| {
        if r.is_empty() {
            return;
        }
        match out.iter_mut().find(|x| x.0 == st) {
            Some(x) => x.1.extend(r),
            None => out.push((st, r)),
        }
    };
    // A contour piece takes the state of the nearest cut segment (ties: the
    // lower state).
    let piece = 0.5 * SCALE;
    let grid = crate::grid::SegGrid::new(segs.to_vec());
    let state_at = |q: Pt| -> u32 { grid.nearest(q, |s| s).map_or(0, |x| x.1) };
    for island in slices {
        // Coloured boundary pieces.
        let mut pieces: Vec<(Pt, Pt, u32)> = Vec::new();
        for ring in island.rings() {
            let m = ring.pts.len();
            for i in 0..m {
                let (a, b) = (ring.pts[i], ring.pts[(i + 1) % m]);
                let k = ((a.dist(b) / piece).ceil() as usize).max(1);
                for j in 0..k {
                    let p0 = lerp(a, b, j as f64 / k as f64);
                    let p1 = lerp(a, b, (j + 1) as f64 / k as f64);
                    let s = state_at(lerp(p0, p1, 0.5));
                    // Join a run of one state on a straight edge.
                    match pieces.last_mut() {
                        Some(last) if last.2 == s && last.1 == p0 && j > 0 => last.1 = p1,
                        _ => pieces.push((p0, p1, s)),
                    }
                }
            }
        }
        let mut states: Vec<u32> = pieces.iter().map(|p| p.2).collect();
        states.sort();
        states.dedup();
        if states.len() <= 1 {
            add(states.first().copied().unwrap_or(0), vec![island.clone()]);
            continue;
        }
        // Nearest-boundary partition, one state at a time; the last takes
        // what is left so the parts tile the island.
        let mut left = vec![island.clone()];
        for (k, &st) in states.iter().enumerate() {
            if k + 1 == states.len() {
                add(st, left.clone());
                break;
            }
            let near = nearest_region(island, &pieces, st);
            let mine = intersection_ex(&left, &near);
            left = difference_ex(&left, &mine);
            add(st, mine);
        }
    }
    for x in out.iter_mut() {
        x.1 = union_ex(&x.1);
    }
    out
}

/// The part of the island's bounding box closer to the pieces of `st` than
/// to any other piece: a distance-difference field on a grid, contoured.
fn nearest_region(island: &ExPolygon, pieces: &[(Pt, Pt, u32)], st: u32) -> Vec<ExPolygon> {
    let bb = island.bbox();
    let (w, h) = ((bb.max.x - bb.min.x) as f64, (bb.max.y - bb.min.y) as f64);
    // About 0.05 mm cells, at most ~250k nodes.
    let step = (0.05 * SCALE).max((w * h / 250_000.0).sqrt());
    let nx = (w / step).ceil() as usize + 3;
    let ny = (h / step).ceil() as usize + 3;
    let x0 = bb.min.x as f64 - step;
    let y0 = bb.min.y as f64 - step;
    let node = |i: usize, j: usize| -> (f64, f64) { (x0 + i as f64 * step, y0 + j as f64 * step) };
    // The pieces of `st` and all others, each as a grid.
    let mine = crate::grid::SegGrid::new(pieces.iter().filter(|p| p.2 == st).map(|&(a, b, _)| (a, b, ())).collect());
    let others = crate::grid::SegGrid::new(pieces.iter().filter(|p| p.2 != st).map(|&(a, b, _)| (a, b, ())).collect());
    let exact = |i: usize, j: usize| -> f64 {
        if i == 0 || j == 0 || i + 1 == nx || j + 1 == ny {
            // Closed at the frame.
            f64::MAX / 4.0
        } else {
            let (x, y) = node(i, j);
            let q = Pt::new(x.round() as i64, y.round() as i64);
            let dm = mine.nearest(q, |_| 0u8).map_or(f64::MAX, |x| x.0);
            let d_other = others.nearest(q, |_| 0u8).map_or(f64::MAX, |x| x.0);
            dm - d_other
        }
    };
    // Exact values only where the zero set can be; signs elsewhere. The
    // field is a difference of two distances, so it is 2-Lipschitz: a cell
    // whose four corners share a sign and all exceed its diagonal (plus a
    // little for the rounded sample points) has that sign throughout, and its
    // nodes just carry it. Any other cell splits in four, down to single
    // cells, whose nodes are all corners. A node is carried only when every
    // cell holding it carried, so both ends of an edge that changes sign are
    // exact. Cells holding frame nodes never carry (the frame value is not a
    // distance).
    const K: usize = 8;
    let mut f = vec![f64::NAN; nx * ny];
    let mut carried: Vec<(usize, usize, usize, usize, f64)> = Vec::new();
    let mut todo: Vec<(usize, usize, usize, usize)> = Vec::new();
    for cj in (0..ny - 1).step_by(K) {
        for ci in (0..nx - 1).step_by(K) {
            todo.push((ci, cj, (ci + K).min(nx - 1), (cj + K).min(ny - 1)));
            while let Some((i0, j0, i1, j1)) = todo.pop() {
                let mut c = [0.0; 4];
                for (n, (i, j)) in [(i0, j0), (i1, j0), (i0, j1), (i1, j1)].into_iter().enumerate() {
                    let k = j * nx + i;
                    if f[k].is_nan() {
                        f[k] = exact(i, j);
                    }
                    c[n] = f[k];
                }
                let (w, h) = (i1 - i0, j1 - j0);
                if w <= 1 && h <= 1 {
                    continue;
                }
                let frame = i0 == 0 || j0 == 0 || i1 + 1 == nx || j1 + 1 == ny;
                // Four units over the diagonal: the sample points are rounded.
                let diag = step * ((w * w + h * h) as f64).sqrt() + 4.0;
                if !frame && (c.iter().all(|&v| v > diag) || c.iter().all(|&v| v < -diag)) {
                    carried.push((i0, j0, i1, j1, c[0]));
                    continue;
                }
                let (im, jm) = (i0 + w / 2, j0 + h / 2);
                let cols: &[(usize, usize)] = if w > 1 { &[(i0, im), (im, i1)] } else { &[(i0, i1)] };
                let rows: &[(usize, usize)] = if h > 1 { &[(j0, jm), (jm, j1)] } else { &[(j0, j1)] };
                for &(r0, r1) in rows {
                    for &(c0, c1) in cols {
                        todo.push((c0, r0, c1, r1));
                    }
                }
            }
        }
    }
    for (i0, j0, i1, j1, fc) in carried {
        for j in j0..=j1 {
            for i in i0..=i1 {
                let k = j * nx + i;
                if f[k].is_nan() {
                    f[k] = fc;
                }
            }
        }
    }
    let val = |i: usize, j: usize| f[j * nx + i];
    // Where the edge between two nodes crosses zero, from the lower node
    // index first so neighbouring cells share the point exactly.
    let cross = |a: (usize, usize), b: (usize, usize)| -> Pt {
        let (a, b) = if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) };
        let (fa, fb) = (val(a.0, a.1), val(b.0, b.1));
        let t = (fa / (fa - fb)).clamp(1e-6, 1.0 - 1e-6);
        let (xa, ya) = node(a.0, a.1);
        let (xb, yb) = node(b.0, b.1);
        Pt::new((xa + (xb - xa) * t).round() as i64, (ya + (yb - ya) * t).round() as i64)
    };
    let mut segs: Vec<(Pt, Pt)> = Vec::new();
    for j in 0..ny - 1 {
        for i in 0..nx - 1 {
            // Corners counter-clockwise from bottom left.
            let c = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
            let inside = c.map(|(a, b)| val(a, b) < 0.0);
            let count = inside.iter().filter(|x| **x).count();
            if count == 0 || count == 4 {
                continue;
            }
            // Crossings in counter-clockwise order: exits (inside to
            // outside) and entries.
            let mut xs = [(false, Pt::default()); 4];
            let mut n = 0;
            for k in 0..4 {
                let k2 = (k + 1) % 4;
                if inside[k] != inside[k2] {
                    xs[n] = (inside[k], cross(c[k], c[k2]));
                    n += 1;
                }
            }
            if n == 2 {
                let (ex, en) = if xs[0].0 { (xs[0].1, xs[1].1) } else { (xs[1].1, xs[0].1) };
                segs.push((ex, en));
            } else if n == 4 {
                let centre = (val(i, j) + val(i + 1, j) + val(i + 1, j + 1) + val(i, j + 1)) / 4.0;
                for k in 0..4 {
                    if !xs[k].0 {
                        continue;
                    }
                    // A joined middle runs to the next entry; separate
                    // corners close on the one before.
                    let en = if centre < 0.0 { xs[(k + 1) % 4].1 } else { xs[(k + 3) % 4].1 };
                    segs.push((xs[k].1, en));
                }
            }
        }
    }
    segments_to_regions(segs, &SliceParams { closing_radius: 0.0, resolution: 0.005, max_gap: 0.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tri(a: [f64; 3], b: [f64; 3], c: [f64; 3], state: u32) -> PaintedTri {
        PaintedTri { v: [a, b, c], state }
    }

    #[test]
    fn decodes_leaves_and_escapes() {
        let v = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        assert_eq!(decode("4", v).unwrap()[0].state, 1);
        assert_eq!(decode("8", v).unwrap()[0].state, 2);
        // State 3 + 2: split 00, state 11, then nibble 0010.
        assert_eq!(decode("2C", v).unwrap()[0].state, 5);
        // A three-side split with four leaves, all filament 1 ("4" each
        // after the 4-bit header 11|00 = side count 3, special 0).
        let l = decode("44443", v).unwrap();
        assert_eq!(l.len(), 4);
        assert!(l.iter().all(|t| t.state == 1));
        assert!(decode("4g", v).is_none());
    }

    #[test]
    fn encoded_leaves_decode_to_their_state() {
        let v = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        assert_eq!(encode_leaf(0), "");
        assert_eq!((encode_leaf(1).as_str(), encode_leaf(2).as_str(), encode_leaf(5).as_str()), ("4", "8", "2C"));
        for st in 1..40 {
            let l = decode(&encode_leaf(st), v).unwrap();
            assert_eq!((l.len(), l[0].state), (1, st), "{st}");
        }
    }

    #[test]
    fn a_painted_top_leaves_no_thin_ring_along_the_islands_edge() {
        // A wavy island, its points where an opening's round trip moves them a unit; the top
        // paints its left half, down two layers.
        let island = ExPolygon::new(Polygon::new((0..240).map(|i| {
            let t = i as f64 / 240.0 * std::f64::consts::TAU;
            let r = 20.0 + 0.4 * (7.0 * t).sin();
            Pt::from_mm(r * t.cos(), r * t.sin())
        }).collect()));
        let left = ExPolygon::new(Polygon::new(vec![Pt::from_mm(-30.0, -30.0), Pt::from_mm(0.0, -30.0), Pt::from_mm(0.0, 30.0), Pt::from_mm(-30.0, 30.0)]));
        let p = PaintParams { top_layers: 2, bottom_layers: 0, shrink: 0.0, max_width: 0.0, interlocking_depth: 0.0 };
        let out = split_cuts(&vec![Vec::new(); 3], [vec![(3.5, 1, left)], Vec::new()], &[1.0, 2.0, 3.0], &vec![vec![island.clone()]; 3], &p);
        for (k, layer) in out.iter().enumerate() {
            for (st, regions) in layer {
                for e in regions {
                    assert!(e.area_mm2().abs() > 1.0, "layer {k} state {st}: a ring of {} mm2", e.area_mm2());
                }
            }
        }
        let painted: f64 = out[2].iter().filter(|x| x.0 == 1).map(|x| total_area_mm2(&x.1)).sum();
        assert!((painted - island.area_mm2() / 2.0).abs() < 2.0, "painted {painted} of {}", island.area_mm2());
    }

    /// A 20 mm square column whose -x side face is painted filament 2: the
    /// layer splits at the bisectors of the painted side and the others.
    #[test]
    fn splits_by_nearest_painted_side() {
        let (lo, hi) = (0.0, 20.0);
        let mut tris = Vec::new();
        let sides = [([lo, lo], [hi, lo], 0), ([hi, lo], [hi, hi], 0), ([hi, hi], [lo, hi], 0), ([lo, hi], [lo, lo], 2)];
        for (a, b, st) in sides {
            tris.push(tri([a[0], a[1], 0.0], [b[0], b[1], 0.0], [b[0], b[1], 10.0], st));
            tris.push(tri([a[0], a[1], 0.0], [b[0], b[1], 10.0], [a[0], a[1], 10.0], st));
        }
        let sq = ExPolygon::new(Polygon::new(vec![Pt::from_mm(lo, lo), Pt::from_mm(hi, lo), Pt::from_mm(hi, hi), Pt::from_mm(lo, hi)]));
        let p = PaintParams { top_layers: 0, bottom_layers: 0, shrink: 0.8, max_width: 0.0, interlocking_depth: 0.0 };
        // (Layer 1 keeps the base filament; the split shows from layer 2.)
        let out = split_layers(&tris, &[0.1, 5.0], &[vec![sq.clone()], vec![sq.clone()]], &p);
        assert!(out[0].iter().all(|x| x.0 == 0), "the first layer is in the base filament");
        let a2: f64 = out[1].iter().filter(|x| x.0 == 2).map(|x| total_area_mm2(&x.1)).sum();
        let a0: f64 = out[1].iter().filter(|x| x.0 == 0).map(|x| total_area_mm2(&x.1)).sum();
        // Points nearer the left side than the other three: a triangle of
        // the square's centre and the left corners, 100 mm².
        assert!((a2 - 100.0).abs() < 2.0, "painted area {a2}");
        assert!((a0 + a2 - 400.0).abs() < 1.0, "split covers the square: {}", a0 + a2);
        // With a 2 mm band only, paint keeps to the surface.
        let p = PaintParams { max_width: 2.0, ..p };
        let out = split_layers(&tris, &[0.1, 5.0], &[vec![sq.clone()], vec![sq]], &p);
        let a2: f64 = out[1].iter().filter(|x| x.0 == 2).map(|x| total_area_mm2(&x.1)).sum();
        assert!(a2 < 40.0 && a2 > 30.0, "banded area {a2}");
    }
}

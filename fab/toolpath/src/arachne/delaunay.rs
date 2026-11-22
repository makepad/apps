//! Radial sweep hull with exact integer Lawson flips.

pub const NONE: u32 = u32::MAX;

#[derive(Clone, Debug, Default)]
pub struct Tri { pub tris: Vec<u32>, pub halfedges: Vec<u32> }

#[inline]
pub(crate) fn next(e: usize) -> usize { e - e % 3 + (e + 1) % 3 }
#[inline]
pub(crate) fn prev(e: usize) -> usize { e - e % 3 + (e + 2) % 3 }

/// Exact: the points of a triangulation lie in a box under 2^30 across, so every difference is
/// under 2^30 and this determinant under 2^61.
#[inline]
pub(crate) fn orient(a: (i64, i64), b: (i64, i64), c: (i64, i64)) -> i64 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

/// How far, relative to the sum of its terms' sizes, rounding can move [`inside_circle`]'s
/// determinant in doubles: the forward error bound of this expansion, (10 + 96e)e at e = 2^-53,
/// taken up.
const INCIRCLE_ERROR: f64 = 1.2e-15;

/// Whether `d` lies strictly inside the circle through `a`, `b`, `c` (counter-clockwise). Doubles
/// answer when rounding cannot change the sign; a determinant nearer zero than that, as cocircular
/// samples give, is worked out exactly. Differences are under 2^30, so doubles hold them exactly.
#[inline]
pub(crate) fn inside_circle(a: (i64, i64), b: (i64, i64), c: (i64, i64), d: (i64, i64)) -> bool {
    let (ax, ay) = (a.0 - d.0, a.1 - d.1);
    let (bx, by) = (b.0 - d.0, b.1 - d.1);
    let (cx, cy) = (c.0 - d.0, c.1 - d.1);
    let (fax, fay, fbx, fby, fcx, fcy) = (ax as f64, ay as f64, bx as f64, by as f64, cx as f64, cy as f64);
    let (bc, cb, ca, ac, ab, ba) = (fbx * fcy, fby * fcx, fcx * fay, fax * fcy, fax * fby, fay * fbx);
    let (al, bl, cl) = (fax * fax + fay * fay, fbx * fbx + fby * fby, fcx * fcx + fcy * fcy);
    let det = al * (bc - cb) + bl * (ca - ac) + cl * (ab - ba);
    let size = al * (bc.abs() + cb.abs()) + bl * (ca.abs() + ac.abs()) + cl * (ab.abs() + ba.abs());
    let error = INCIRCLE_ERROR * size;
    if det > error {
        return true;
    }
    if -det > error {
        return false;
    }
    in_circle_exact(a, b, c, d)
}

/// [`inside_circle`] in whole integers: each lift is under 2^61, each cross product under 2^61.
pub(crate) fn in_circle_exact(a: (i64, i64), b: (i64, i64), c: (i64, i64), d: (i64, i64)) -> bool {
    let (ax, ay) = (a.0 - d.0, a.1 - d.1);
    let (bx, by) = (b.0 - d.0, b.1 - d.1);
    let (cx, cy) = (c.0 - d.0, c.1 - d.1);
    let lift = |x: i64, y: i64| (x * x + y * y) as i128;
    lift(ax, ay) * (bx * cy - by * cx) as i128 - lift(bx, by) * (ax * cy - ay * cx) as i128 + lift(cx, cy) * (ax * by - ay * bx) as i128 > 0
}

pub(crate) fn circumcentre(a: (i64, i64), b: (i64, i64), c: (i64, i64)) -> Option<(f64, f64)> {
    let (bx, by) = ((b.0 - a.0) as f64, (b.1 - a.1) as f64);
    let (cx, cy) = ((c.0 - a.0) as f64, (c.1 - a.1) as f64);
    let det = 2.0 * orient(a, b, c) as f64;
    if det == 0.0 { return None; }
    let (bb, cc) = (bx * bx + by * by, cx * cx + cy * cy);
    Some((a.0 as f64 + (cy * bb - by * cc) / det, a.1 as f64 + (bx * cc - cx * bb) / det))
}

fn distance(a: (i64, i64), b: (i64, i64)) -> i64 {
    let (x, y) = (a.0 - b.0, a.1 - b.1);
    x * x + y * y
}

fn hash(p: (i64, i64), c: (f64, f64), n: usize) -> usize {
    let (x, y) = (p.0 as f64 - c.0, p.1 as f64 - c.1);
    let d = x.abs() + y.abs();
    let a = if d == 0.0 { 0.0 } else { x / d };
    let a = if y > 0.0 { (3.0 - a) * 0.25 } else { (1.0 + a) * 0.25 };
    ((a * n as f64) as usize).min(n - 1)
}

impl Tri {
    fn link(&mut self, a: usize, b: u32) {
        self.halfedges[a] = b;
        if b != NONE { self.halfedges[b as usize] = a as u32; }
    }

    fn add(&mut self, a: u32, b: u32, c: u32) -> Option<usize> {
        let e = self.tris.len();
        if e > u32::MAX as usize - 3 { return None; }
        self.tris.extend_from_slice(&[a, b, c]);
        self.halfedges.extend_from_slice(&[NONE; 3]);
        Some(e)
    }

    /// Flip the edges on `stack`, each facing the point just added from the triangle it apexes,
    /// until the triangulation is Delaunay again. A point inside the circle across an edge makes a
    /// strictly convex quad with it, so a flip always leaves two proper triangles. The edges a flip
    /// gives the new point are Delaunay edges (an empty circle through them shrinks out of the old
    /// triangle's), so they never flip: only the two edges facing it are tested next.
    fn legalize(&mut self, pts: &[(i64, i64)], hull: &mut [u32], stack: &mut Vec<u32>) {
        while let Some(e) = stack.pop() {
            let e = e as usize;
            let f = self.halfedges[e];
            if f == NONE { continue; }
            let f = f as usize;
            let (en, ep, fn_, fp) = (next(e), prev(e), next(f), prev(f));
            let (a, b, c, d) = (self.tris[e], self.tris[en], self.tris[ep], self.tris[fp]);
            if !inside_circle(pts[a as usize], pts[b as usize], pts[c as usize], pts[d as usize]) { continue; }
            let (h_en, h_ep, h_fn, h_fp) = (self.halfedges[en], self.halfedges[ep], self.halfedges[fn_], self.halfedges[fp]);
            self.tris[e] = c; self.tris[en] = d; self.tris[ep] = b;
            self.tris[f] = d; self.tris[fn_] = c; self.tris[fp] = a;
            self.link(e, f as u32);
            self.link(ep, h_en); self.link(fn_, h_ep);
            self.link(fp, h_fn); self.link(en, h_fp);
            for k in [ep, fn_] {
                if self.halfedges[k] == NONE { hull[self.tris[k] as usize] = k as u32; }
            }
            for k in [en, fp] {
                if self.halfedges[k] == NONE { hull[self.tris[k] as usize] = k as u32; }
                else { stack.push(k as u32); }
            }
        }
    }
}

/// Triangle indices refer to the original input, including after deduplication.
pub fn triangulate(pts: &[(i64, i64)]) -> Option<Tri> {
    if pts.len() < 3 || pts.len() > (u32::MAX as usize - 3) / 6 { return None; }
    let mut ids: Vec<u32> = (0..pts.len() as u32).collect();
    ids.sort_unstable_by_key(|&i| (pts[i as usize], i));
    ids.dedup_by_key(|i| pts[*i as usize]);
    if ids.len() < 3 { return None; }
    let (mut lo, mut hi) = (pts[ids[0] as usize], pts[ids[0] as usize]);
    for &i in &ids {
        let p = pts[i as usize];
        lo.0 = lo.0.min(p.0); lo.1 = lo.1.min(p.1);
        hi.0 = hi.0.max(p.0); hi.1 = hi.1.max(p.1);
    }
    if hi.0 as i128 - lo.0 as i128 >= 1 << 30 || hi.1 as i128 - lo.1 as i128 >= 1 << 30 { return None; }
    let first = pts[0];
    let relative: Vec<(i64, i64)> = pts.iter().map(|p| (p.0 - first.0, p.1 - first.1)).collect();
    let pts = relative.as_slice();
    lo = (lo.0 - first.0, lo.1 - first.1); hi = (hi.0 - first.0, hi.1 - first.1);
    let mid = (lo.0 + (hi.0 - lo.0) / 2, lo.1 + (hi.1 - lo.1) / 2);
    let a = *ids.iter().min_by_key(|&&i| distance(pts[i as usize], mid))?;
    let mut b = *ids.iter().filter(|&&i| i != a).min_by_key(|&&i| distance(pts[i as usize], pts[a as usize]))?;
    let mut seed = None;
    let mut radius = f64::INFINITY;
    for &i in &ids {
        if i == a || i == b { continue; }
        if let Some(c) = circumcentre(pts[a as usize], pts[b as usize], pts[i as usize]) {
            let r = (c.0 - pts[a as usize].0 as f64).powi(2) + (c.1 - pts[a as usize].1 as f64).powi(2);
            if r < radius { radius = r; seed = Some((i, c)); }
        }
    }
    let (mut c, centre) = seed?;
    if orient(pts[a as usize], pts[b as usize], pts[c as usize]) < 0 { std::mem::swap(&mut b, &mut c); }
    let radial = |i: u32| {
        let p = pts[i as usize];
        (p.0 as f64 - centre.0).powi(2) + (p.1 as f64 - centre.1).powi(2)
    };
    let mut order: Vec<(f64, u32)> = ids.iter().map(|&i| (radial(i), i)).collect();
    order.sort_unstable_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
    let mut out = Tri { tris: Vec::with_capacity(6 * order.len()), halfedges: Vec::with_capacity(6 * order.len()) };
    out.add(a, b, c)?;
    let mut forward = vec![NONE; pts.len()];
    let mut back = vec![NONE; pts.len()];
    let mut hull = vec![NONE; pts.len()];
    for (i, j, k, e) in [(a, b, c, 0), (b, c, a, 1), (c, a, b, 2)] {
        forward[i as usize] = j; back[i as usize] = k; hull[i as usize] = e;
    }
    let buckets = (order.len() as f64).sqrt().ceil() as usize;
    let mut table = vec![NONE; buckets];
    for i in [a, b, c] { table[hash(pts[i as usize], centre, buckets)] = i; }
    let mut stack = Vec::new();
    let mut start = a;
    for (_, i) in order {
        if i == a || i == b || i == c { continue; }
        let p = pts[i as usize];
        let h = hash(p, centre, buckets);
        let mut e = start;
        let mut at = h;
        for _ in 0..buckets {
            let v = table[at];
            // The bucket's vertex can be the far end of the visible chain, and the forward walk
            // from there goes round the whole hull. Its predecessor starts on the chain then.
            // Any hull vertex finds the same first visible edge (`left`): the strictly visible
            // edges of a convex hull are one chain.
            if v != NONE && forward[v as usize] != NONE { e = back[v as usize]; break; }
            at += 1;
            if at == buckets { at = 0; }
        }
        let initial = e;
        while orient(pts[e as usize], pts[forward[e as usize] as usize], p) >= 0 {
            e = forward[e as usize];
            if e == initial { return None; }
        }
        loop {
            let v = back[e as usize];
            if orient(pts[v as usize], pts[e as usize], p) >= 0 { break; }
            e = v;
            if e == initial { return None; }
        }
        let left = e;
        let mut last = NONE;
        let mut first = NONE;
        loop {
            let v = forward[e as usize];
            if orient(pts[e as usize], pts[v as usize], p) >= 0 { break; }
            let t = out.add(e, i, v)?;
            out.link(t + 2, hull[e as usize]);
            if last != NONE { out.link(t, last); }
            else { first = t as u32; }
            last = (t + 1) as u32;
            stack.push((t + 2) as u32);
            if e != left { forward[e as usize] = NONE; back[e as usize] = NONE; }
            e = v;
        }
        forward[left as usize] = i; back[i as usize] = left;
        forward[i as usize] = e; back[e as usize] = i;
        hull[left as usize] = first; hull[i as usize] = last;
        start = i;
        out.legalize(pts, &mut hull, &mut stack);
        table[hash(pts[left as usize], centre, buckets)] = left;
        table[h] = i;
    }
    Some(out)
}

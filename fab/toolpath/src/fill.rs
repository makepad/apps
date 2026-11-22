//! Line families clipped to a region (scanlines in a rotated frame) and
//! the chainer that joins line or curve pieces along the boundary, with
//! anchors at free ends. Pattern geometry lives in `patterns`.

use makepad_fab_slicer::geom::*;

pub use crate::clipper::Pieces;

/// Where a clipped line end lies on the region boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Hit {
    x: i64,
    ring: u32,
    edge: u32,
}

/// A clipped segment in the rotated frame, with its boundary hits.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Seg {
    y: i64,
    a: Hit,
    b: Hit,
    line: u32,
}

fn rotate_rings(region: &[ExPolygon], cos: f64, sin: f64, pivot: Pt) -> Vec<Vec<Pt>> {
    let mut rings = Vec::new();
    for e in region {
        for r in e.rings() {
            rings.push(r.pts.iter().map(|p| p.sub(pivot).rotated(cos, sin)).collect());
        }
    }
    rings
}

/// Scanlines y = ys[k] (sorted) clipped to the rings by the even-odd rule.
///
/// An edge from lo to hi crosses the lines with lo.y <= ys[k] < hi.y
/// (half-open, so a vertex on a scanline counts once): k from the number of
/// lines below lo to the number below hi. That count is walked from vertex
/// to vertex along each ring, a step or two per edge, as most edges of a
/// finely traced contour cross no line at all. Hits are made in one pass in
/// ring and edge order, then bucketed per scanline in that order, and each
/// scanline's hits are sorted stably by x, as per-line lists would be.
fn scan(rings: &[Vec<Pt>], ys: &[i64]) -> Vec<Seg> {
    let nl = ys.len();
    // The number of lines below y (ys[..k] < y <= ys[k..]), walked from `k`.
    let below = |mut k: usize, y: i64| -> usize {
        while k > 0 && ys[k - 1] >= y {
            k -= 1;
        }
        while k < nl && ys[k] < y {
            k += 1;
        }
        k
    };
    let mut start = vec![0u32; nl + 1];
    let mut made: Vec<(u32, Hit)> = Vec::new();
    for (ri, r) in rings.iter().enumerate() {
        let Some(&first) = r.first() else { continue };
        let k_first = ys.partition_point(|&y| y < first.y);
        let n = r.len();
        let (mut a, mut ka) = (first, k_first);
        for i in 0..n {
            let (b, kb) = if i + 1 < n { (r[i + 1], below(ka, r[i + 1].y)) } else { (first, k_first) };
            if ka != kb {
                let (lo, hi, k0, k1) = if a.y < b.y { (a, b, ka, kb) } else { (b, a, kb, ka) };
                let (dy, dx) = ((hi.y - lo.y) as f64, (hi.x - lo.x) as f64);
                for k in k0..k1 {
                    let t = (ys[k] - lo.y) as f64 / dy;
                    let x = lo.x as f64 + dx * t;
                    start[k + 1] += 1;
                    made.push((k as u32, Hit { x: x.round() as i64, ring: ri as u32, edge: i as u32 }));
                }
            }
            a = b;
            ka = kb;
        }
    }
    for k in 0..nl {
        start[k + 1] += start[k];
    }
    let mut fill: Vec<u32> = start[..nl].to_vec();
    let mut hits = vec![Hit { x: 0, ring: 0, edge: 0 }; made.len()];
    for &(k, h) in &made {
        let f = &mut fill[k as usize];
        hits[*f as usize] = h;
        *f += 1;
    }
    let mut out = Vec::with_capacity(hits.len() / 2);
    for k in 0..nl {
        let h = &mut hits[start[k] as usize..start[k + 1] as usize];
        h.sort_by_key(|h| h.x);
        for pair in h.chunks(2) {
            if pair.len() == 2 && pair[1].x > pair[0].x {
                out.push(Seg { y: ys[k], a: pair[0], b: pair[1], line: k as u32 });
            }
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Connect {
    /// Separate lines (travel between them).
    None,
    /// Join line ends that are neighbours along the boundary when the
    /// boundary path between them is at most `max` mm; free chain ends get
    /// an anchor of up to `anchor` mm along the boundary.
    Boundary { max: f64, anchor: f64 },
}

#[derive(Clone, Copy, Debug)]
pub enum Align {
    /// Lines through `pivot + k * spacing` (stable across layers).
    Grid,
    /// Lines evenly centred in the region's extent.
    Centered,
    /// Centred, without the two outermost lines (monotonic top surfaces:
    /// ⌊W/s⌋ − 1 lines).
    CenteredInset,
    /// Solid fill: the spacing stretched so a whole number of lines spans
    /// the extent with 0.8 spacings to spare at each side (see [`fit_spacing`]).
    /// `reverse_first`: the first line runs from its far end (rotated +x)
    /// back (zig-zag solids); otherwise forward (monotonic solids).
    Fit { reverse_first: bool },
    /// Fitted axis-aligned bridges include both outer lines. `spacing` is
    /// fitted before the half-spacing inset, so endpoints span n-1 gaps.
    FitEdges,
}

/// The region's extent across lines at `angle` (units).
pub fn extent(region: &[ExPolygon], angle: f64) -> (i64, i64) {
    extent_of(region.iter().flat_map(|e| e.rings()).flat_map(|r| r.pts.iter()), angle)
}

/// The points' extent across lines at `angle` (units): each point as
/// `rotate_rings` turns it, without keeping them.
pub fn extent_of<'a>(pts: impl IntoIterator<Item = &'a Pt>, angle: f64) -> (i64, i64) {
    let (sin, cos) = angle.sin_cos();
    let mut ymin = i64::MAX;
    let mut ymax = i64::MIN;
    for p in pts {
        let y = p.rotated(cos, -sin).y;
        ymin = ymin.min(y);
        ymax = ymax.max(y);
    }
    (ymin, ymax)
}

/// Solid line spacing for `region` at `angle`: n = ⌊W/s − 0.6⌋ lines, the
/// spacing stretched to W / (n + 0.6), so the outermost lines sit 0.8
/// spacings in from the extent W (the reference slices' solid layers).
pub fn fit_spacing(region: &[ExPolygon], angle: f64, spacing_mm: f64) -> f64 {
    let (lo, hi) = extent(region, angle);
    if hi <= lo || spacing_mm <= 0.0 {
        return spacing_mm;
    }
    let w = (hi - lo) as f64 / SCALE;
    let n = (w / spacing_mm - 0.6).floor();
    if n < 1.0 {
        return spacing_mm;
    }
    w / (n + 0.6)
}

/// A clipped line in bed coordinates with the boundary edges it ends on.
#[derive(Clone, Copy, Debug)]
pub struct RawSeg {
    pub a: Pt,
    pub b: Pt,
    pub ea: (u32, u32),
    pub eb: (u32, u32),
}

/// One family of parallel lines at `angle` clipped to `region`.
pub fn family(region: &[ExPolygon], angle: f64, spacing_mm: f64, pivot: Pt, align: Align) -> Vec<RawSeg> {
    if region.is_empty() || spacing_mm <= 0.0 {
        return Vec::new();
    }
    let (sin, cos) = angle.sin_cos();
    let rings = rotate_rings(region, cos, -sin, pivot);
    let mut ymin = i64::MAX;
    let mut ymax = i64::MIN;
    for r in &rings {
        for p in r {
            ymin = ymin.min(p.y);
            ymax = ymax.max(p.y);
        }
    }
    let s = scaled(spacing_mm);
    if s <= 0 || ymax <= ymin {
        return Vec::new();
    }
    let ys: Vec<i64> = match align {
        Align::Grid => {
            let k0 = (ymin as f64 / s as f64).ceil() as i64;
            let k1 = (ymax as f64 / s as f64).floor() as i64;
            (k0..=k1).map(|k| k * s).collect()
        }
        Align::Centered => {
            let ext = ymax - ymin;
            let n = (ext / s) + 1;
            let span = (n - 1) * s;
            let y0 = ymin + (ext - span) / 2;
            (0..n).map(|k| y0 + k * s).collect()
        }
        Align::CenteredInset => {
            let ext = ymax - ymin;
            let n = (ext / s - 1).max(1);
            let span = (n - 1) * s;
            let y0 = ymin + (ext - span) / 2;
            (0..n).map(|k| y0 + k * s).collect()
        }
        Align::Fit { .. } => {
            let ext = (ymax - ymin) as f64;
            // `s` comes from `fit_spacing`: ext = (n + 0.6)·s.
            let n = ((ext / s as f64) - 0.6).round().max(1.0) as i64;
            let y0 = ymin as f64 + (ext - (n - 1) as f64 * s as f64) / 2.0;
            (0..n).map(|k| (y0 + k as f64 * s as f64).round() as i64).collect()
        }
        Align::FitEdges => {
            let n = (((ymax - ymin) as f64 / s as f64).round() as i64).max(1);
            let lo = ymin + ((ymax - ymin) / 2).min(1);
            (0..=n).map(|k| {
                let y = ymin + (((ymax - ymin) as f64 * k as f64 / n as f64).round() as i64);
                y.clamp(lo, ymax - 1)
            }).collect()
        }
    };
    let back = |p: Pt| p.rotated(cos, sin).add(pivot);
    let segs = scan(&rings, &ys);
    // Fitted lines alternate direction from the first one that hits the
    // region; grid lines by their global index.
    let first_line = if matches!(align, Align::Fit { .. } | Align::FitEdges | Align::CenteredInset) { segs.first().map(|s| s.line).unwrap_or(0) } else { 0 };
    let flip = matches!(align, Align::Fit { reverse_first: true });
    segs.into_iter()
        .map(|sg| {
            let fwd = ((sg.line - first_line) % 2 == 0) != flip;
            let (a, b) = if fwd { (sg.a, sg.b) } else { (sg.b, sg.a) };
            RawSeg { a: back(Pt::new(a.x, sg.y)), b: back(Pt::new(b.x, sg.y)), ea: (a.ring, a.edge), eb: (b.ring, b.edge) }
        })
        .collect()
}

/// Monotonic lines (in line order, alternating direction) joined by dry
/// moves: each line runs on by `into_wall` mm past its end, steps across to
/// the next line's run-on and back onto it, when the two ends are within
/// `max_mm`. Returns the polylines with the dry flag of each segment.
pub fn monotonic_dry(segs: &[RawSeg], into_wall_mm: f64, max_mm: f64) -> Vec<(Polyline, Vec<bool>)> {
    let ext = into_wall_mm * SCALE;
    let run_on = |from: Pt, to: Pt, by: f64| -> Pt {
        let d = from.dist(to);
        if d <= 0.0 {
            return to;
        }
        let k = by / d;
        Pt::new(to.x + ((to.x - from.x) as f64 * k).round() as i64, to.y + ((to.y - from.y) as f64 * k).round() as i64)
    };
    let mut out: Vec<(Polyline, Vec<bool>)> = Vec::new();
    let mut cur: Option<(Vec<Pt>, Vec<bool>)> = None;
    for sg in segs {
        if let Some((pts, dry)) = cur.as_mut() {
            let last = *pts.last().unwrap();
            let prev = pts[pts.len() - 2];
            if last.dist(sg.a) <= max_mm * SCALE {
                let e0 = run_on(prev, last, ext);
                let e1 = run_on(sg.b, sg.a, ext);
                pts.extend([e0, e1, sg.a, sg.b]);
                dry.extend([true, true, true, false]);
                continue;
            }
            let (p, d) = cur.take().unwrap();
            out.push((Polyline::new(p), d));
        }
        cur = Some((vec![sg.a, sg.b], vec![false]));
    }
    if let Some((p, d)) = cur {
        out.push((Polyline::new(p), d));
    }
    out
}

/// Parallel lines at `angle` clipped to `region`, chained per `connect`.
pub fn lines(region: &[ExPolygon], angle: f64, spacing_mm: f64, pivot: Pt, align: Align, connect: Connect) -> Vec<Polyline> {
    let segs = family(region, angle, spacing_mm, pivot, align);
    match connect {
        Connect::None => segs.into_iter().map(|s| Polyline::new(vec![s.a, s.b])).collect(),
        Connect::Boundary { max, anchor } if matches!(align, Align::Fit { .. }) && anchor <= 0.0 => chain_seq(region, &segs, max),
        Connect::Boundary { max, anchor } => chain(region, &segs, max, anchor),
    }
}

/// Zig-zag chains walked from the first line in its given direction: each
/// line end steps along the boundary to the nearest unused line end next to
/// it (at most `max_mm` away) and runs that line from there; a chain that
/// cannot continue ends, and the next starts at the first unused line.
pub fn chain_seq(region: &[ExPolygon], segs: &[RawSeg], max_mm: f64) -> Vec<Polyline> {
    let rings = ring_geo(region);
    let n = segs.len();
    let end_pt = |k: usize| if k % 2 == 0 { segs[k / 2].a } else { segs[k / 2].b };
    let end_edge = |k: usize| if k % 2 == 0 { segs[k / 2].ea } else { segs[k / 2].eb };
    let mut per_ring: Vec<Vec<(f64, usize)>> = vec![Vec::new(); rings.len()];
    let mut slot: Vec<(usize, usize)> = vec![(usize::MAX, 0); 2 * n];
    for k in 0..2 * n {
        let (r, e) = end_edge(k);
        if let Some(g) = rings.get(r as usize) {
            per_ring[r as usize].push((arc_pos(g, e, end_pt(k)), k));
        }
    }
    for (ri, list) in per_ring.iter_mut().enumerate() {
        list.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        for (i, &(_, k)) in list.iter().enumerate() {
            slot[k] = (ri, i);
        }
    }
    let max = max_mm * SCALE;
    let mut used = vec![false; n];
    let mut out = Vec::new();
    for first in 0..n {
        if used[first] {
            continue;
        }
        let mut pts: Vec<Pt> = Vec::new();
        let mut k = 2 * first;
        loop {
            used[k / 2] = true;
            let other = k ^ 1;
            pts.push(end_pt(k));
            pts.push(end_pt(other));
            let (ri, i) = slot[other];
            if ri == usize::MAX {
                break;
            }
            let list = &per_ring[ri];
            let g = &rings[ri];
            let m = list.len();
            let s0 = list[i].0;
            let mut best: Option<(f64, usize, bool)> = None;
            for (j, fwd) in [((i + 1) % m, true), ((i + m - 1) % m, false)] {
                let (s1, nk) = list[j];
                if used[nk / 2] || nk / 2 == other / 2 {
                    continue;
                }
                let d = if fwd { (s1 - s0).rem_euclid(g.len) } else { (s0 - s1).rem_euclid(g.len) };
                if d <= max && best.map_or(true, |b| d < b.0 - 1.0 || (d < b.0 + 1.0 && nk < b.1)) {
                    best = Some((d, nk, fwd));
                }
            }
            let Some((_, nk, fwd)) = best else { break };
            let s1 = list[slot[nk].1].0;
            let via = if fwd { arc_path(g, s0, s1, end_pt(other), end_pt(nk)) } else { let mut v = arc_path(g, s1, s0, end_pt(nk), end_pt(other)); v.reverse(); v };
            pts.extend(via.iter().skip(1).take(via.len().saturating_sub(2)));
            k = nk;
        }
        pts.dedup();
        if pts.len() >= 2 {
            out.push(Polyline::new(pts));
        }
    }
    out
}

struct RingGeo {
    pts: Vec<Pt>,
    /// Arc length at the start of each edge (units).
    cum: Vec<f64>,
    len: f64,
}

fn ring_geo(region: &[ExPolygon]) -> Vec<RingGeo> {
    let mut out = Vec::new();
    for e in region {
        for r in e.rings() {
            let n = r.pts.len();
            let mut cum = Vec::with_capacity(n);
            let mut acc = 0.0;
            for i in 0..n {
                cum.push(acc);
                acc += r.pts[i].dist(r.pts[(i + 1) % n]);
            }
            out.push(RingGeo { pts: r.pts.clone(), cum, len: acc });
        }
    }
    out
}

fn arc_pos(g: &RingGeo, edge: u32, p: Pt) -> f64 {
    let e = edge as usize % g.pts.len();
    g.cum[e] + g.pts[e].dist(p)
}

/// Boundary path from arc position s0 forward to s1 on ring g.
fn arc_path(g: &RingGeo, s0: f64, s1: f64, p0: Pt, p1: Pt) -> Vec<Pt> {
    let mut out = vec![p0];
    let n = g.pts.len();
    let mut s1 = s1;
    if s1 < s0 {
        s1 += g.len;
    }
    // Ring vertices strictly between the two positions.
    for k in 0..2 * n {
        let i = k % n;
        let si = g.cum[i] + if k >= n { g.len } else { 0.0 };
        if si > s0 + 1e-6 && si < s1 - 1e-6 {
            out.push(g.pts[i]);
        }
    }
    out.push(p1);
    out
}

/// A point at arc position s on ring g.
fn arc_point(g: &RingGeo, s: f64) -> Pt {
    let n = g.pts.len();
    let s = s.rem_euclid(g.len.max(1e-9));
    let mut i = g.cum.partition_point(|c| *c <= s).saturating_sub(1);
    i = i.min(n - 1);
    let a = g.pts[i];
    let b = g.pts[(i + 1) % n];
    let l = a.dist(b);
    if l <= 0.0 {
        return a;
    }
    lerp(a, b, ((s - g.cum[i]) / l).clamp(0.0, 1.0))
}

/// Chain line segments into polylines along the boundary.
///
/// Line ends on each boundary ring are sorted by arc position; neighbouring
/// ends of different segments are joined, shortest first, when the boundary
/// path is at most `max_mm` and the join does not close a cycle. Chain ends
/// left free get an anchor along the boundary into the larger free gap.
pub fn chain(region: &[ExPolygon], segs: &[RawSeg], max_mm: f64, anchor_mm: f64) -> Vec<Polyline> {
    chain_anchors(region, &Pieces::from_segs(segs), max_mm, anchor_mm, None)
}

/// [`chain`] for clipped polylines (curve patterns): pieces whose ends lie
/// on the boundary join along it; ends inside the region stay free.
pub fn chain_pieces(region: &[ExPolygon], pieces: &Pieces, max_mm: f64, anchor_mm: f64, reach_mm: Option<f64>) -> Vec<Polyline> {
    chain_anchors(region, pieces, max_mm, anchor_mm, reach_mm)
}

/// Inferred from the reference grid output: sparse strokes can reach a
/// corner beyond the two short anchors (a 5.072 mm corner join with
/// 1.628 mm free anchors). Keep the short reach away from corners so an
/// otherwise complete chain is not joined across an unrelated grid cell.
pub fn chain_sparse(region: &[ExPolygon], segs: &[RawSeg], max_mm: f64, anchor_mm: f64, reach_mm: f64) -> Vec<Polyline> {
    chain_anchors(region, &Pieces::from_segs(segs), max_mm, anchor_mm, Some(reach_mm))
}

fn chain_anchors(region: &[ExPolygon], segs: &Pieces, max_mm: f64, anchor_mm: f64, reach_mm: Option<f64>) -> Vec<Polyline> {
    let rings = ring_geo(region);
    let n = segs.len();
    if n == 0 {
        return Vec::new();
    }
    // End k: piece k/2, side k%2 (0 = first point, 1 = last).
    let end_pt = |k: usize| if k % 2 == 0 { segs.first(k / 2) } else { segs.last(k / 2) };
    let end_edge = |k: usize| if k % 2 == 0 { segs.ea[k / 2] } else { segs.eb[k / 2] };
    let mut per_ring: Vec<Vec<(f64, usize)>> = vec![Vec::new(); rings.len()];
    for k in 0..2 * n {
        let (r, e) = end_edge(k);
        if let Some(g) = rings.get(r as usize) {
            per_ring[r as usize].push((arc_pos(g, e, end_pt(k)), k));
        }
    }
    let max = max_mm * SCALE;
    // (length bucket, ring, arc position, end_from, end_to), forward from→to.
    // Equal lengths go in boundary order so parallel lines zig-zag cleanly.
    let mut cands: Vec<(i64, usize, f64, usize, usize)> = Vec::new();
    for (ri, list) in per_ring.iter_mut().enumerate() {
        list.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let m = list.len();
        if m < 2 {
            continue;
        }
        for i in 0..m {
            let (s0, k0) = list[i];
            let (s1, k1) = list[(i + 1) % m];
            let mut d = s1 - s0;
            if d < 0.0 {
                d += rings[ri].len;
            }
            let at_corner = |k: usize| {
                let (_, e) = end_edge(k);
                let g = &rings[ri];
                let p = end_pt(k);
                // Rotating to the scan frame and back rounds twice on
                // the nanometre grid; allow those two units at a vertex.
                p.dist2(g.pts[e as usize]) <= 4 || p.dist2(g.pts[(e as usize + 1) % g.pts.len()]) <= 4
            };
            let within_anchor = reach_mm.map_or(true, |reach| d <= reach * SCALE || at_corner(k0) || at_corner(k1));
            if k0 / 2 != k1 / 2 && d <= max && within_anchor {
                cands.push(((d / (0.01 * SCALE)).round() as i64, ri, s0, k0, k1));
            }
        }
    }
    cands.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.partial_cmp(&b.2).unwrap()));
    let mut link: Vec<Option<(usize, usize, bool)>> = vec![None; 2 * n]; // other end, ring, forward
    let mut uf: Vec<usize> = (0..n).collect();
    fn find(uf: &mut [usize], mut x: usize) -> usize {
        while uf[x] != x {
            uf[x] = uf[uf[x]];
            x = uf[x];
        }
        x
    }
    for (_, ri, _, k0, k1) in &cands {
        if link[*k0].is_some() || link[*k1].is_some() {
            continue;
        }
        let (a, b) = (find(&mut uf, k0 / 2), find(&mut uf, k1 / 2));
        if a == b {
            continue;
        }
        uf[a] = b;
        link[*k0] = Some((*k1, *ri, true));
        link[*k1] = Some((*k0, *ri, false));
    }
    // Walk chains from free ends.
    let mut used = vec![false; n];
    let mut out = Vec::new();
    let anchor = anchor_mm * SCALE;
    let free_anchor = |k: usize, at_start: bool| -> Vec<Pt> {
        // Anchor from end k along the boundary into the larger gap.
        if anchor <= 0.0 {
            return Vec::new();
        }
        let (r, e) = end_edge(k);
        let Some(g) = rings.get(r as usize) else { return Vec::new() };
        let list = &per_ring[r as usize];
        let s = arc_pos(g, e, end_pt(k));
        let idx = list.iter().position(|x| x.1 == k).unwrap_or(0);
        let m = list.len();
        let gap_fwd = if m > 1 { (list[(idx + 1) % m].0 - s).rem_euclid(g.len) } else { g.len };
        let gap_bwd = if m > 1 { (s - list[(idx + m - 1) % m].0).rem_euclid(g.len) } else { g.len };
        let (dir, gap) = if gap_fwd >= gap_bwd { (1.0, gap_fwd) } else { (-1.0, gap_bwd) };
        let l = anchor.min(gap / 2.0);
        if l < SCALE * 0.05 {
            return Vec::new();
        }
        let s_end = s + dir * l;
        let p = end_pt(k);
        let q = arc_point(g, s_end);
        let path = if dir > 0.0 { arc_path(g, s, s_end.rem_euclid(g.len), p, q) } else { let mut v = arc_path(g, s_end.rem_euclid(g.len), s, q, p); v.reverse(); v };
        if at_start {
            let mut v = path;
            v.reverse();
            v
        } else {
            path
        }
    };
    let mut starts: Vec<usize> = (0..2 * n).filter(|&k| link[k].is_none()).collect();
    starts.sort();
    let walk = |start_end: usize, used: &mut Vec<bool>, out: &mut Vec<Polyline>| {
        let mut pts: Vec<Pt> = Vec::new();
        let head = free_anchor(start_end, true);
        pts.extend(head.iter().take(head.len().saturating_sub(1)));
        let mut k = start_end;
        loop {
            let sgi = k / 2;
            if used[sgi] {
                break;
            }
            used[sgi] = true;
            let other = k ^ 1;
            let piece = segs.piece(sgi);
            if k % 2 == 0 {
                pts.extend_from_slice(piece);
            } else {
                pts.extend(piece.iter().rev());
            }
            match link[other] {
                Some((nk, ri, fwd)) => {
                    let g = &rings[ri];
                    let s0 = arc_pos(g, end_edge(other).1, end_pt(other));
                    let s1 = arc_pos(g, end_edge(nk).1, end_pt(nk));
                    let via = if fwd { arc_path(g, s0, s1, end_pt(other), end_pt(nk)) } else { let mut v = arc_path(g, s1, s0, end_pt(nk), end_pt(other)); v.reverse(); v };
                    pts.extend(via.iter().skip(1).take(via.len().saturating_sub(2)));
                    k = nk;
                }
                None => {
                    let tail = free_anchor(other, false);
                    pts.extend(tail.iter().skip(1));
                    break;
                }
            }
        }
        pts.dedup();
        if pts.len() >= 2 {
            out.push(Polyline::new(pts));
        }
    };
    for k in starts {
        if !used[k / 2] {
            walk(k, &mut used, &mut out);
        }
    }
    // Leftover cycles cannot occur (joins never close one), but keep any
    // unvisited segment.
    for i in 0..n {
        if !used[i] {
            walk(2 * i, &mut used, &mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `scan` by its definition: per line, every edge with lo.y <= y < hi.y in
    /// ring and edge order, sorted stably by x and paired.
    fn scan_by_definition(rings: &[Vec<Pt>], ys: &[i64]) -> Vec<Seg> {
        let mut out = Vec::new();
        for (k, &y) in ys.iter().enumerate() {
            let mut h = Vec::new();
            for (ri, r) in rings.iter().enumerate() {
                for i in 0..r.len() {
                    let (a, b) = (r[i], r[(i + 1) % r.len()]);
                    let (lo, hi) = if a.y < b.y { (a, b) } else { (b, a) };
                    if lo.y <= y && y < hi.y {
                        let t = (y - lo.y) as f64 / (hi.y - lo.y) as f64;
                        let x = lo.x as f64 + (hi.x - lo.x) as f64 * t;
                        h.push(Hit { x: x.round() as i64, ring: ri as u32, edge: i as u32 });
                    }
                }
            }
            h.sort_by_key(|h| h.x);
            for pair in h.chunks(2) {
                if pair.len() == 2 && pair[1].x > pair[0].x {
                    out.push(Seg { y, a: pair[0], b: pair[1], line: k as u32 });
                }
            }
        }
        out
    }

    /// The same segments in the same order as the definition, on random
    /// rings on a coarse grid: vertices on scanlines, horizontal edges, x ties
    /// between edges, and rings of 0, 1 and 2 vertices.
    #[test]
    fn scan_matches_its_definition() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = |n: i64| -> i64 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as i64
        };
        let mut rings: Vec<Vec<Pt>> = vec![Vec::new(), vec![Pt::new(3, 20)], vec![Pt::new(0, 0), Pt::new(5, 30)]];
        for _ in 0..300 {
            let n = 3 + rnd(30) as usize;
            let xr = if rnd(2) == 0 { 40 } else { 4000 };
            let mut r = Vec::with_capacity(n);
            let mut y = rnd(130) - 15;
            for _ in 0..n {
                if rnd(5) != 0 {
                    y = rnd(130) - 15;
                }
                r.push(Pt::new(rnd(2 * xr) - xr, y));
            }
            rings.push(r);
        }
        // Uniform lines, uneven ones with repeats (fitted edges clamp onto
        // one another), one line and none.
        let lines: Vec<Vec<i64>> = vec![(0..11).map(|k| k * 10).collect(), vec![3, 3, 10, 11, 25, 25, 25, 40, 70, 71, 99, 99], vec![50], vec![]];
        for ys in &lines {
            let want = scan_by_definition(&rings, ys);
            assert!(ys.len() < 2 || want.len() > 100, "the rings cross the lines {ys:?}");
            assert_eq!(scan(&rings, ys), want, "lines {ys:?}");
        }
    }
}

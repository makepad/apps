//! Walls of an island as paths: classic fixed-width loops, or the
//! variable-width bead lines of `wall_generator = arachne`.
//!
//! A classic outer wall's centre line sits half its width inside the
//! outline; each further wall sits one bead spacing further in. The infill
//! region is what remains inside the innermost wall's inner edge.

use crate::flow;
use crate::{ExtrusionPath, Role};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::grid::SegGrid;
use makepad_fab_slicer::source::{BeadLine, WallShape};
use makepad_fab_slicer::clip::{boolean, difference_ex, intersection_ex, BoolOp, FillRule};
use makepad_fab_slicer::offset::{offset_ex, opening, raw_ring, Join};

/// How each wall loop chooses its starting point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeamMode { Aligned, Back, Random, Nearest }

/// `wall_sequence`: the order of each outer wall and the inner walls behind it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WallOrder { InnerOuter, OuterInner, InnerOuterInner }

/// Pick the seam vertex of a ring: close to `target`, preferring corners.
pub fn seam_index(ring: &[Pt], target: Pt) -> usize {
    let n = ring.len();
    let mut best = 0;
    let mut best_score = f64::MAX;
    for i in 0..n {
        let a = ring[(i + n - 1) % n];
        let b = ring[i];
        let c = ring[(i + 1) % n];
        let u = b.sub(a);
        let v = c.sub(b);
        let lu = ((u.x as f64).powi(2) + (u.y as f64).powi(2)).sqrt().max(1.0);
        let lv = ((v.x as f64).powi(2) + (v.y as f64).powi(2)).sqrt().max(1.0);
        let cosang = dot(u, v) as f64 / (lu * lv);
        // Straight runs score worst, corners best.
        let corner = (1.0 - cosang).clamp(0.0, 2.0);
        let d = b.dist(target) / SCALE;
        let score = d - 3.0 * corner;
        if score < best_score - 1e-9 {
            best_score = score;
            best = i;
        }
    }
    best
}

fn angle_penalty(a: f64) -> f64 {
    (1.0 / (3.0 * a * a + 1.0)).exp_m1() / (std::f64::consts::E - 1.0) + 1.0 / (2.0 + (-a).exp())
}

/// Score the turn using nozzle-length arms, traversing every ring counter-clockwise.
fn corner_penalty(ring: &[Pt], i: usize, nozzle: f64, ccw: bool) -> f64 {
    let n = ring.len();
    let arm = |forward: bool| {
        let mut j = i;
        let mut length = 0.0;
        for _ in 1..n {
            let k = if forward { (j + 1) % n } else { (j + n - 1) % n };
            length += ring[j].dist(ring[k]);
            j = k;
            if length >= nozzle * SCALE { break; }
        }
        ring[j]
    };
    let u = ring[i].sub(arm(!ccw));
    let v = arm(ccw).sub(ring[i]);
    angle_penalty((cross(u, v) as f64).atan2(dot(u, v) as f64))
}

fn outer_index(ring: &Polygon, target: Pt, width: f64, nozzle: f64, mode: SeamMode) -> usize {
    if mode != SeamMode::Back { return seam_index(&ring.pts, target); }
    let pts = &ring.pts;
    let ymax = pts.iter().map(|p| p.y).max().unwrap_or(0);
    let ccw = ring.is_ccw();
    let mut best = 0;
    let mut penalty = f64::MAX;
    for (i, p) in pts.iter().enumerate() {
        if ymax - p.y > 1 { continue; }
        let score = corner_penalty(pts, i, nozzle, ccw);
        if score < penalty || (score == penalty && p.x > pts[best].x) {
            best = i;
            penalty = score;
        }
    }
    let c = nearest_index(pts, target);
    if pts[c].dist(target) <= 4.0 * width * SCALE && pts[c].y + scaled(1.5) > pts[best].y { c } else { best }
}

fn seam_hash(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

/// Choose an acceptable vertex or edge midpoint by its share of the
/// perimeter; with the seam's arc position (mm) on the ring.
fn random_path(ring: &Polygon, role: Role, width: f64, seam_gap: f64, nozzle: f64, z: f64) -> (ExtrusionPath, f64) {
    let (cands, start) = random_ring(ring, width, nozzle, z);
    let pts: Vec<Pt> = cands.iter().map(|c| c.0).collect();
    (loop_path(&pts, start, role, width, seam_gap), arc_to(&pts, start))
}

/// The ring with the midpoints of long edges added as seam candidates (each
/// with the ring vertex it follows, and whether it is a midpoint), and the
/// candidate a random seam starts at.
fn random_ring(ring: &Polygon, width: f64, nozzle: f64, z: f64) -> (Vec<(Pt, usize, bool)>, usize) {
    let mut cands = Vec::with_capacity(ring.len() * 2);
    let mut penalties = Vec::with_capacity(ring.len() * 2);
    let ccw = ring.is_ccw();
    for i in 0..ring.len() {
        let a = ring.pts[i];
        let b = ring.pts[(i + 1) % ring.len()];
        cands.push((a, i, false));
        penalties.push(0.6 * corner_penalty(&ring.pts, i, nozzle, ccw));
        if a.dist(b) > 4.0 * width * width * SCALE {
            cands.push((Pt::new(a.x + (b.x - a.x) / 2, a.y + (b.y - a.y) / 2), i, true));
            penalties.push(0.6 * angle_penalty(0.0));
        }
    }
    let pts: Vec<Pt> = cands.iter().map(|c| c.0).collect();
    let best = penalties.iter().copied().fold(f64::MAX, f64::min);
    let n = pts.len();
    let share = |i: usize| (pts[i].dist(pts[(i + n - 1) % n]) + pts[i].dist(pts[(i + 1) % n])) / 2.0;
    let total: f64 = (0..n).filter(|&i| penalties[i] <= best + 0.3).map(share).sum();
    let first = ring.pts[0];
    let hash = seam_hash(seam_hash(z.to_bits() ^ first.x as u64) ^ first.y as u64);
    let mut pick = (hash >> 11) as f64 * (1.0 / ((1u64 << 53) as f64)) * total;
    let mut start = 0;
    for i in 0..n {
        if penalties[i] > best + 0.3 { continue; }
        start = i;
        pick -= share(i);
        if pick < 0.0 { break; }
    }
    (cands, start)
}

pub(crate) fn loop_path(ring: &[Pt], start: usize, role: Role, width: f64, seam_gap: f64) -> ExtrusionPath {
    let n = ring.len();
    let mut pts = Vec::with_capacity(n + 1);
    for k in 0..=n {
        pts.push(ring[(start + k) % n]);
    }
    let loop_pts = pts.clone();
    let mut pl = Polyline::new(pts);
    if seam_gap > 0.0 {
        pl.clip_end(seam_gap * SCALE);
    }
    ExtrusionPath { role, pts: pl.pts, closed: true, loop_pts, width, height: 0.0, area: 0.0, speed: 0.0, dry: Vec::new(), z: Vec::new(), flow: Vec::new(), mode: 0, core: false, overhang: 0.0 }
}

/// The first outer seam for chaining; random seams do not chain.
pub fn seam(rings_per_wall: &[Vec<Polygon>], target: Pt, mode: SeamMode, w_ext: f64, nozzle: f64) -> Option<Pt> {
    if mode == SeamMode::Random { return None; }
    let ring = rings_per_wall.first()?.first()?;
    Some(ring.pts[outer_index(ring, target, w_ext, nozzle, mode)])
}

/// Aligned walls of one island; returns the paths in print order (inner walls
/// first, outer last), the fill region inside them, and the outer seam.
pub fn classic(island: &ExPolygon, loops: usize, w_ext: f64, w_in: f64, h: f64, seam_gap: f64, target: Pt, precise: bool) -> (Vec<ExtrusionPath>, Vec<ExPolygon>, Option<Pt>) {
    let walls = rings(island, loops, w_ext, w_in, h, precise);
    let (paths, seam) = paths(&walls.rings, w_ext, w_in, seam_gap, target, WallOrder::InnerOuter, SeamMode::Aligned, w_ext, 0.0);
    (paths.into_iter().map(|p| p.0).collect(), walls.fill, seam)
}

/// Each wall's centre inset, bead spacing and extrusion width, outer first.
pub fn levels(loops: usize, w_ext: f64, w_in: f64, h: f64, precise: bool) -> impl Iterator<Item = (f64, f64, f64)> {
    let s_ext = flow::spacing(w_ext, h);
    let s_in = flow::spacing(w_in, h);
    let first_inner = if precise { (w_ext + w_in) / 2.0 } else { (s_ext + s_in) / 2.0 };
    (0..loops).map(move |k| {
        let d = if k == 0 { w_ext / 2.0 } else { w_ext / 2.0 + first_inner + (k - 1) as f64 * s_in };
        (d, if k == 0 { s_ext } else { s_in }, if k == 0 { w_ext } else { w_in })
    })
}

/// What the geometric half of [`classic`] makes of an island.
#[derive(Debug, Default)]
pub struct RingWalls {
    /// Each wall's rings (outer first; each piece's contour, then its holes).
    pub rings: Vec<Vec<Polygon>>,
    /// The region inside the innermost wall, empty where the walls asked for stop short.
    pub fill: Vec<ExPolygon>,
    /// What the outer wall leaves of the island: too thin for a loop, free on both sides.
    pub thin: Vec<ExPolygon>,
    /// What each further wall leaves inside the one before it: walled on both sides.
    pub between: Vec<ExPolygon>,
}

/// Each wall's rings and the fill inside them, and what they leave for gap fill. A wall that
/// collapses leaves what is inside the wall before it to gap fill, not to the fill: it is
/// narrower than two beads, and sparse fill would leave it hollow.
/// Independent per island and layer.
pub fn rings(island: &ExPolygon, loops: usize, w_ext: f64, w_in: f64, h: f64, precise: bool) -> RingWalls {
    if loops == 0 {
        return RingWalls { fill: vec![island.clone()], ..Default::default() };
    }
    let src = std::slice::from_ref(island);
    let join = Join::Miter(3.0);
    let mut out = RingWalls::default();
    for (k, (d, s, w)) in levels(loops, w_ext, w_in, h, precise).enumerate() {
        // The wall's centre line, opened by half its spacing: where the
        // part is too thin for this wall, its two sides would print a bead
        // on top of each other, so the wall leaves that part out.
        // The fill is the innermost wall's inset: the shrink of its opening.
        let inset = offset_ex(src, -((d + s / 2.0) * SCALE), join);
        let r = if inset.is_empty() { Vec::new() } else { separate(offset_ex(&inset, s / 2.0 * SCALE, join), s * SCALE) };
        // The wall reaches its full width out to the island, its spacing out to the wall before.
        let bound = if k == 0 { src } else { &out.fill[..] };
        let left = if r.is_empty() { bound.to_vec() } else { difference_ex(bound, &offset_ex(&r, (if k == 0 { w } else { s }) / 2.0 * SCALE, join)) };
        if k == 0 {
            out.thin = left;
        } else {
            out.between.extend(left);
        }
        if r.is_empty() {
            out.fill.clear();
            break;
        }
        out.rings.push(r.into_iter().flat_map(|e| std::iter::once(e.contour).chain(e.holes)).collect());
        out.fill = inset;
    }
    out
}

/// Pieces of one wall's centre-line region, kept a spacing `s` apart.
///
/// Where an opening cuts a wall at a neck, the two pieces end in caps that
/// can face each other closer than a bead: both would print over the same
/// spot. Each piece is cut back from the pieces before it (in order) by `s`
/// and opened again, so no two pieces' centre lines come nearer than `s`.
///
/// The cut is `e` less the grow by `s` of the nearby pieces before it. Where the grow's raw
/// rings keep clear of `e` ([`clear_of`]), that grow neither touches `e` nor reaches its
/// boolean, so the cut is `e`'s boolean alone, without the grow's union.
pub(crate) fn separate(pieces: Vec<ExPolygon>, s: f64) -> Vec<ExPolygon> {
    if pieces.len() < 2 {
        return pieces;
    }
    let join = Join::Miter(3.0);
    // The pieces kept so far, each with its box and (once a later piece is near it) the raw
    // rings of its grow by `s`.
    let mut out: Vec<(ExPolygon, BBox, Option<Vec<Polygon>>)> = Vec::with_capacity(pieces.len());
    let keep = |out: &mut Vec<(ExPolygon, BBox, Option<Vec<Polygon>>)>, e: ExPolygon| {
        let b = e.bbox();
        out.push((e, b, None));
    };
    let reach = s.ceil() as i64 + 2;
    for e in pieces {
        let b = e.bbox();
        let near: Vec<usize> = (0..out.len())
            .filter(|&i| {
                let ob = &out[i].1;
                ob.min.x <= b.max.x + reach && b.min.x <= ob.max.x + reach && ob.min.y <= b.max.y + reach && b.min.y <= ob.max.y + reach
            })
            .collect();
        if near.is_empty() {
            keep(&mut out, e);
            continue;
        }
        for &i in &near {
            if out[i].2.is_none() {
                out[i].2 = Some(out[i].0.rings().filter_map(|r| raw_ring(r, s, join)).collect());
            }
        }
        let raw = || near.iter().flat_map(|&i| out[i].2.as_deref().unwrap_or_default());
        let cut = if clear_of(&e, raw) {
            boolean(&to_polygons(std::slice::from_ref(&e)), &[], BoolOp::Difference, FillRule::NonZero)
        } else {
            let near: Vec<ExPolygon> = near.iter().map(|&i| out[i].0.clone()).collect();
            difference_ex(std::slice::from_ref(&e), &offset_ex(&near, s, join))
        };
        if cut.len() == 1 && cut[0] == e {
            keep(&mut out, e);
            continue;
        }
        for c in opening(&cut, s / 2.0, s / 2.0, join) {
            keep(&mut out, c);
        }
    }
    out.into_iter().map(|(e, _, _)| e).collect()
}

/// More than this many units apart, the edges of two regions take no part in each other's
/// boolean: nothing crosses, touches or snaps across. A hot pixel reaches under a unit from its
/// point, and a union routes each edge through pixels under a unit from it.
const CLEAR: f64 = 4.0;

/// Whether the union of the raw rings `raw` (under the positive fill rule: a grow's region)
/// misses `e` with every one of its edges more than [`CLEAR`] from every edge of `e`. Then the
/// union's edges keep more than two units from `e`'s, so `e` less that union is `e`'s boolean
/// alone: no hot pixel, crossing or winding of the union reaches `e`'s faces, and the rings
/// come out in the same order. No raw ring at all is no grow, and no clearance.
fn clear_of<'a, I: Iterator<Item = &'a Polygon>>(e: &ExPolygon, raw: impl Fn() -> I) -> bool {
    if raw().next().is_none() || !Apart::new(e).rings(raw()) {
        return false;
    }
    // Clear of every edge, `e` lies wholly inside or outside the union: its first point says which.
    let q = e.contour.pts[0];
    raw().map(|r| r.winding(q)).sum::<i32>() <= 0
}

/// Edge clearance from one region `e`: whether rings keep every edge more than [`CLEAR`] units
/// from every edge of `e`. The grid of `e`'s edges is built on the first edge that comes near
/// its box.
pub(crate) struct Apart<'a> {
    e: &'a ExPolygon,
    bbox: BBox,
    grid: std::cell::OnceCell<SegGrid<()>>,
}

impl<'a> Apart<'a> {
    pub(crate) fn new(e: &'a ExPolygon) -> Self {
        Self { e, bbox: e.bbox(), grid: std::cell::OnceCell::new() }
    }

    pub(crate) fn bbox(&self) -> BBox {
        self.bbox
    }

    /// The region's edges, indexed on first use.
    pub(crate) fn grid(&self) -> &SegGrid<()> {
        self.grid.get_or_init(|| SegGrid::of_regions(std::slice::from_ref(self.e), |_| ()))
    }

    pub(crate) fn rings<'r>(&self, rings: impl Iterator<Item = &'r Polygon>) -> bool {
        let reach = CLEAR as i64 + 1;
        let close = |a: Pt, b: Pt| {
            let mut bb = BBox::of(&[a, b]);
            bb.min = Pt::new(bb.min.x - reach, bb.min.y - reach);
            bb.max = Pt::new(bb.max.x + reach, bb.max.y + reach);
            if !bb.overlaps(&self.bbox) {
                return false;
            }
            let mut hit = false;
            self.grid().visit_box(bb, |c, d, _| hit = hit || segments_within(a, b, c, d, CLEAR));
            hit
        };
        let mut rings = rings;
        !rings.any(|r| {
            let n = r.pts.len();
            (0..n).any(|i| close(r.pts[i], r.pts[(i + 1) % n]))
        })
    }
}

/// Wall paths with the selected seam mode and the first outer seam, if chained.
///
/// Walls print loop by loop: each outer loop (the outline, or a hole's) with
/// the inner loops that follow it inward, the innermost first and the outer
/// last (`order` turns each group round, or prints the first inner wall after the outer), so the step from an inner
/// wall to its outer wall is one bead and no travel crosses the part between
/// them. An inner loop belongs to the outer loop nearest to it.
/// Each path comes with its [`Along`] (rings numbered wall by wall).
pub fn paths(rings_per_wall: &[Vec<Polygon>], w_ext: f64, w_in: f64, seam_gap: f64, target: Pt, order: WallOrder, mode: SeamMode, nozzle: f64, z: f64) -> (Vec<(ExtrusionPath, Along)>, Option<Pt>) {
    let path = |ring: &Polygon, line: usize, role: Role, width: f64, target: Pt| {
        if mode == SeamMode::Random {
            let (p, start) = random_path(ring, role, width, seam_gap, nozzle, z);
            return (p, Along { line, start });
        }
        let i = if role == Role::OuterWall { outer_index(ring, target, width, nozzle, mode) } else { nearest_index(&ring.pts, target) };
        (loop_path(&ring.pts, i, role, width, seam_gap), Along { line, start: arc_to(&ring.pts, i) })
    };
    let first_line: Vec<usize> = rings_per_wall.iter().scan(0, |n, w| { let at = *n; *n += w.len(); Some(at) }).collect();
    // The outer loops, each with its seam (the first one's leads the chain).
    let mut seam = None;
    let mut groups: Vec<((ExtrusionPath, Along), Pt, &[Pt], Vec<(usize, (ExtrusionPath, Along))>)> = Vec::new();
    if let Some(outer) = rings_per_wall.first() {
        for (i, ring) in outer.iter().enumerate() {
            let outer = path(ring, i, Role::OuterWall, w_ext, target);
            let start = outer.0.loop_pts[0];
            if seam.is_none() && mode != SeamMode::Random { seam = Some(start); }
            groups.push((outer, start, &ring.pts[..], Vec::new()));
        }
    }
    let outer_pts = (groups.len() > 1).then(|| OuterPoints::new(groups.iter().map(|g| g.2)));
    let near_of = |ring: &[Pt]| -> usize {
        // The outer loop nearest to a few points of the ring, the first of
        // equals: the least (distance², loop) over the probes' nearest points.
        let Some(outer_pts) = &outer_pts else { return 0 };
        let step = (ring.len() / 8).max(1);
        ring.iter().step_by(step).map(|&q| outer_pts.nearest(q)).min().map_or(0, |(_, g)| g)
    };
    let mut orphans = Vec::new();
    let mut assigned: Vec<(usize, usize, (ExtrusionPath, Along))> = Vec::new();
    for k in 1..rings_per_wall.len() {
        for (i, ring) in rings_per_wall[k].iter().enumerate() {
            let line = first_line[k] + i;
            if groups.is_empty() {
                orphans.push(path(ring, line, Role::InnerWall, w_in, target));
                continue;
            }
            let g = near_of(&ring.pts);
            assigned.push((g, k, path(ring, line, Role::InnerWall, w_in, groups[g].1)));
        }
    }
    for (g, k, path) in assigned {
        groups[g].3.push((k, path));
    }
    let mut paths = orphans;
    for (outer, _, _, mut inner) in groups {
        // Innermost first, out to the outer wall (or the other way round).
        inner.sort_by(|a, b| b.0.cmp(&a.0));
        match order {
            WallOrder::OuterInner => {
                paths.push(outer);
                paths.extend(inner.into_iter().rev().map(|x| x.1));
            }
            // The deeper inner walls, the outer wall, then the first inner wall.
            WallOrder::InnerOuterInner if inner.iter().any(|x| x.0 >= 2) => {
                let (first, deeper): (Vec<_>, Vec<_>) = inner.into_iter().partition(|x| x.0 == 1);
                paths.extend(deeper.into_iter().map(|x| x.1));
                paths.push(outer);
                paths.extend(first.into_iter().map(|x| x.1));
            }
            _ => {
                paths.extend(inner.into_iter().map(|x| x.1));
                paths.push(outer);
            }
        }
    }
    (paths, seam)
}

/// The outer loops' points in a uniform grid, for exact nearest-point search.
struct OuterPoints {
    x0: i64,
    y0: i64,
    cell: i64,
    nx: i64,
    ny: i64,
    /// Cell c holds `pts[start[c]..start[c + 1]]`: (point, its loop).
    start: Vec<u32>,
    pts: Vec<(Pt, usize)>,
}

impl OuterPoints {
    fn new<'a>(loops: impl Iterator<Item = &'a [Pt]> + Clone) -> Self {
        let mut b = BBox::empty();
        let mut n = 0usize;
        for l in loops.clone() {
            for &p in l {
                b.add(p);
                n += 1;
            }
        }
        let (w, h) = ((b.max.x - b.min.x).max(1), (b.max.y - b.min.y).max(1));
        // About two points per cell.
        let cell = ((w as f64 * h as f64 * 2.0 / n.max(1) as f64).sqrt().ceil() as i64).max(1);
        let (nx, ny) = (w / cell + 1, h / cell + 1);
        let (x0, y0) = (b.min.x, b.min.y);
        let cell_of = |p: Pt| (((p.y - y0) / cell) * nx + (p.x - x0) / cell) as usize;
        let mut start = vec![0u32; (nx * ny) as usize + 1];
        for l in loops.clone() {
            for &p in l {
                start[cell_of(p) + 1] += 1;
            }
        }
        for c in 1..start.len() {
            start[c] += start[c - 1];
        }
        let mut fill = start.clone();
        let mut pts = vec![(Pt::new(0, 0), 0usize); n];
        for (g, l) in loops.enumerate() {
            for &p in l {
                let c = cell_of(p);
                pts[fill[c] as usize] = (p, g);
                fill[c] += 1;
            }
        }
        Self { x0, y0, cell, nx, ny, start, pts }
    }

    /// The least (distance², loop) over every point. Cells are searched in
    /// square rings round q's: a point outside ring r lies farther than
    /// r cells, so the search ends once the best is that near.
    fn nearest(&self, q: Pt) -> (i128, usize) {
        let cx = (q.x - self.x0).div_euclid(self.cell).clamp(0, self.nx - 1);
        let cy = (q.y - self.y0).div_euclid(self.cell).clamp(0, self.ny - 1);
        let reach = cx.max(self.nx - 1 - cx).max(cy).max(self.ny - 1 - cy);
        let mut best = (i128::MAX, usize::MAX);
        let visit = |i: i64, j: i64, best: &mut (i128, usize)| {
            if i < 0 || j < 0 || i >= self.nx || j >= self.ny {
                return;
            }
            let c = (j * self.nx + i) as usize;
            for &(p, g) in &self.pts[self.start[c] as usize..self.start[c + 1] as usize] {
                *best = (*best).min((p.dist2(q), g));
            }
        };
        for r in 0..=reach {
            if r == 0 {
                visit(cx, cy, &mut best);
            } else {
                for i in cx - r..=cx + r {
                    visit(i, cy - r, &mut best);
                    visit(i, cy + r, &mut best);
                }
                for j in cy - r + 1..cy + r {
                    visit(cx - r, j, &mut best);
                    visit(cx + r, j, &mut best);
                }
            }
            let r_len = (r * self.cell) as i128;
            if best.0 <= r_len * r_len {
                break;
            }
        }
        best
    }
}

/// The first outer seam of an island's walls for chaining; random seams do not chain.
pub fn shape_seam(shape: &WallShape, target: Pt, mode: SeamMode, w_ext: f64, nozzle: f64) -> Option<Pt> {
    match shape {
        WallShape::Rings(r) => seam(r, target, mode, w_ext, nozzle),
        WallShape::Beads(lines) => {
            if mode == SeamMode::Random {
                return None;
            }
            let ring = Polygon::new(outer_beads(lines).find(|l| Polygon::new(l.pts.clone()).is_ccw())?.pts.clone());
            Some(ring.pts[outer_index(&ring, target, w_ext, nozzle, mode)])
        }
    }
}

/// An island's wall paths in print order, each with its place on the shape,
/// and the outer seam (see [`paths`], [`bead_paths`]).
#[allow(clippy::too_many_arguments)]
pub fn shape_paths(shape: &WallShape, w_ext: f64, w_in: f64, h: f64, seam_gap: f64, target: Pt, order: WallOrder, mode: SeamMode, nozzle: f64, z: f64) -> (Vec<(ExtrusionPath, Along)>, Option<Pt>) {
    match shape {
        WallShape::Rings(r) => paths(r, w_ext, w_in, seam_gap, target, order, mode, nozzle, z),
        WallShape::Beads(lines) => bead_paths(lines, w_ext, h, seam_gap, target, order, mode, nozzle, z),
    }
}

/// The closed outer-wall bead loops.
fn outer_beads(lines: &[BeadLine]) -> impl Iterator<Item = &BeadLine> {
    lines.iter().filter(|l| l.inset == 0 && l.closed && !l.odd && l.pts.len() >= 3)
}

/// Bead widths that change by more than this along a segment step through
/// it in pieces of at most this change.
const WIDTH_STEP: f64 = 0.05;
/// A gap fill line's width steps are at least this long (mm): half a bead, about what its melt
/// can show. A steeper taper gets coarser steps at their mean width, which keeps its volume.
const GAP_WIDTH_STEP_LENGTH: f64 = 0.2;

/// One bead line as a path starting at point `start` (closed lines run
/// round and stop the seam gap short). Its width is the widest piece's
/// extrusion width; `flow` scales each segment down to its own bead width
/// (a piece's width is the wider of its two ends), empty when the widths
/// stay within 0.005 mm.
fn bead_path(pts: &[Pt], widths: &[f64], closed: bool, start: usize, role: Role, h: f64, seam_gap: f64) -> ExtrusionPath {
    stepped_path(pts, widths, closed, start, role, h, seam_gap, 0.0)
}

#[allow(clippy::too_many_arguments)]
fn stepped_path(pts: &[Pt], widths: &[f64], closed: bool, start: usize, role: Role, h: f64, seam_gap: f64, least_step: f64) -> ExtrusionPath {
    let n = pts.len();
    let at = |k: usize| (pts[(start + k) % n], widths[(start + k) % n]);
    let mut line: Vec<Pt> = vec![at(0).0];
    let mut seg: Vec<f64> = Vec::new();
    for k in 1..if closed { n + 1 } else { n } {
        let ((a, wa), (b, wb)) = (at(k - 1), at(k));
        let fine = ((wb - wa).abs() / WIDTH_STEP).ceil().max(1.0);
        let steps = fine.min((a.dist(b) / SCALE / least_step).floor().max(1.0));
        let coarse = steps < fine;
        let steps = steps as usize;
        let mut w0 = wa;
        for j in 1..=steps {
            let t = j as f64 / steps as f64;
            let w1 = wa + (wb - wa) * t;
            let p = if j == steps { b } else { lerp(a, b, t) };
            if p != *line.last().unwrap() {
                line.push(p);
                seg.push(if coarse { 0.5 * (w0 + w1) } else { w0.max(w1) });
            }
            w0 = w1;
        }
    }
    let (lo, hi) = seg.iter().fold((f64::MAX, 0f64), |m, &w| (m.0.min(w), m.1.max(w)));
    // A closed line keeps a flow for every segment of its loop (the seam
    // gap's too, for whatever walks the whole loop).
    let flow: Vec<f64> = if hi - lo <= 0.005 { Vec::new() } else { seg.iter().map(|w| w / hi).collect() };
    let loop_pts = if closed { line.clone() } else { Vec::new() };
    let mut pl = Polyline::new(line);
    if closed && seam_gap > 0.0 {
        pl.clip_end(seam_gap * SCALE);
    }
    ExtrusionPath { role, pts: pl.pts, closed, loop_pts, width: flow::width_for_spacing(hi, h), height: 0.0, area: 0.0, speed: 0.0, dry: Vec::new(), z: Vec::new(), flow, mode: 0, core: false, overhang: 0.0 }
}

/// Gap fill lines as paths, each starting where the last ended, the first nearest `at`.
pub fn gap_paths(lines: &[BeadLine], h: f64, at: Pt) -> Vec<ExtrusionPath> {
    line_paths(lines, Role::GapFill, h, at)
}

/// Lines of one bead across as paths of `role`, each starting where the last ended, the first
/// nearest `at`.
pub fn line_paths(lines: &[BeadLine], role: Role, h: f64, mut at: Pt) -> Vec<ExtrusionPath> {
    let mut left: Vec<&BeadLine> = lines.iter().collect();
    let mut out = Vec::with_capacity(left.len());
    while !left.is_empty() {
        // The nearest end of an open line, or point of a closed one.
        let near = |l: &BeadLine| -> (f64, usize) {
            let candidates: &mut dyn Iterator<Item = usize> = if l.closed { &mut (0..l.pts.len()) } else { &mut [0, l.pts.len() - 1].into_iter() };
            candidates.map(|i| (l.pts[i].dist(at), i)).min_by(|a, b| a.0.total_cmp(&b.0)).expect("a line has points")
        };
        let (next, _) = left.iter().enumerate().map(|(i, l)| (i, near(l).0)).min_by(|a, b| a.1.total_cmp(&b.1)).expect("lines are left");
        let l = left.swap_remove(next);
        let (_, start) = near(l);
        let path = if l.closed {
            stepped_path(&l.pts, &l.widths, true, start, role, h, 0.0, GAP_WIDTH_STEP_LENGTH)
        } else if start == 0 {
            stepped_path(&l.pts, &l.widths, false, 0, role, h, 0.0, GAP_WIDTH_STEP_LENGTH)
        } else {
            let pts: Vec<Pt> = l.pts.iter().rev().copied().collect();
            let widths: Vec<f64> = l.widths.iter().rev().copied().collect();
            stepped_path(&pts, &widths, false, 0, role, h, 0.0, GAP_WIDTH_STEP_LENGTH)
        };
        at = *path.pts.last().expect("a line has points");
        out.push(path);
    }
    out
}

/// Arachne bead lines as paths. Each line goes with the outer loop nearest
/// it; within a group the insets print inner first (or outer first, or the
/// second inner, outer, first inner for inner-outer-inner), a middle bead
/// right after the loop around it, and lone thin-feature beads last.
#[allow(clippy::too_many_arguments)]
/// Each path comes with its [`Along`] (line = the bead line's index).
pub fn bead_paths(lines: &[BeadLine], w_ext: f64, h: f64, seam_gap: f64, target: Pt, order: WallOrder, mode: SeamMode, nozzle: f64, z: f64) -> (Vec<(ExtrusionPath, Along)>, Option<Pt>) {
    let path = |line: usize, near: Option<Pt>| {
        let l = &lines[line];
        let role = if l.inset == 0 { Role::OuterWall } else { Role::InnerWall };
        if !l.closed {
            return (bead_path(&l.pts, &l.widths, false, 0, role, h, seam_gap), Along { line, start: 0.0 });
        }
        let ring = Polygon::new(l.pts.clone());
        if mode == SeamMode::Random {
            let (cands, start) = random_ring(&ring, w_ext, nozzle, z);
            let n = l.pts.len();
            let pts: Vec<Pt> = cands.iter().map(|c| c.0).collect();
            let widths: Vec<f64> = cands.iter().map(|&(_, i, mid)| if mid { 0.5 * (l.widths[i] + l.widths[(i + 1) % n]) } else { l.widths[i] }).collect();
            return (bead_path(&pts, &widths, true, start, role, h, seam_gap), Along { line, start: arc_to(&pts, start) });
        }
        let start = match near {
            Some(p) => nearest_index(&l.pts, p),
            None => outer_index(&ring, target, w_ext, nozzle, mode),
        };
        (bead_path(&l.pts, &l.widths, true, start, role, h, seam_gap), Along { line, start: arc_to(&l.pts, start) })
    };
    // Groups: an outer loop, its seam, and the lines that go with it.
    let mut seam = None;
    let mut groups: Vec<(usize, Pt, Vec<(usize, bool, (ExtrusionPath, Along))>)> = Vec::new();
    let mut taken = vec![false; lines.len()];
    for (i, l) in lines.iter().enumerate() {
        if l.inset == 0 && l.closed && !l.odd && l.pts.len() >= 3 {
            let p = path(i, None);
            let start = p.0.loop_pts[0];
            if seam.is_none() && mode != SeamMode::Random {
                seam = Some(start);
            }
            taken[i] = true;
            groups.push((i, start, vec![(0, false, p)]));
        }
    }
    // A line goes with the outer loop nearest to a few of its points, the first of equals.
    let outer_pts = (groups.len() > 1).then(|| OuterPoints::new(groups.iter().map(|g| &lines[g.0].pts[..])));
    let mut orphans = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if taken[i] {
            continue;
        }
        if groups.is_empty() {
            orphans.push((l.inset, l.odd, path(i, if l.closed { Some(target) } else { None })));
            continue;
        }
        let step = (l.pts.len() / 8).max(1);
        let g = outer_pts.as_ref().and_then(|o| l.pts.iter().step_by(step).map(|&q| o.nearest(q)).min()).map_or(0, |(_, g)| g);
        let p = path(i, Some(groups[g].1));
        groups[g].2.push((l.inset, l.odd, p));
    }
    let mut out = Vec::new();
    sequence(orphans, order, &mut out);
    for (_, _, members) in groups {
        sequence(members, order, &mut out);
    }
    (out, seam)
}

/// Print order of one group's bead lines (inset, middle bead, path).
fn sequence(mut members: Vec<(usize, bool, (ExtrusionPath, Along))>, order: WallOrder, out: &mut Vec<(ExtrusionPath, Along)>) {
    let top = members.iter().map(|m| m.0).max().unwrap_or(0);
    let insets: Vec<usize> = match order {
        WallOrder::OuterInner => (0..=top).collect(),
        WallOrder::InnerOuterInner if top >= 2 => (2..=top).rev().chain([0, 1]).collect(),
        _ => (0..=top).rev().collect(),
    };
    let mut take = |inset: usize, odd: bool, out: &mut Vec<(ExtrusionPath, Along)>| {
        let mut k = 0;
        while k < members.len() {
            if members[k].0 == inset && members[k].1 == odd {
                out.push(members.remove(k).2);
            } else {
                k += 1;
            }
        }
    };
    if order == WallOrder::OuterInner {
        take(0, true, out);
    }
    for k in insets {
        // A middle bead right after the loop around it.
        take(k, false, out);
        take(k + 1, true, out);
    }
    take(0, true, out);
}

fn nearest_index(ring: &[Pt], p: Pt) -> usize {
    let mut best = 0;
    let mut bd = i128::MAX;
    for (i, q) in ring.iter().enumerate() {
        let d = q.dist2(p);
        if d < bd {
            bd = d;
            best = i;
        }
    }
    best
}

/// Wall speeds by how far a bead hangs past the layer below.
#[derive(Clone, Debug, Default)]
pub struct OverhangSpeeds {
    /// Geometric detection is independent of optional speed modulation.
    pub enabled: bool,
    pub slowdown: bool,
    /// Speeds at 10%, 25%, 50% and 75% unsupported width (R2); zero
    /// inherits the wall speed. Fractional degrees interpolate these.
    pub quarter: [f64; 4],
    /// Speed for a bead entirely over air.
    pub totally: f64,
    pub bridge: f64,
    pub nozzle: f64,
    pub width: f64,
    pub smooth: f64,
    pub max_volumetric: f64,
}

/// Signed distance (units) from `p` to the outline of `region` (as a grid
/// of its ring edges): negative inside.
fn signed_distance(p: Pt, region: &SegGrid<u32>) -> f64 {
    let best = region.nearest(p, |t| t).map_or(f64::MAX, |x| x.0);
    if region.contains_any(p) {
        -best
    } else {
        best
    }
}

/// Parameters where a segment crosses the support boundary (a grid of its
/// ring edges), into `cuts` (cleared first). Predicates use the integer grid;
/// only the constructed intersection is rounded. False when the segment
/// touches the boundary nowhere, not even along it: it then lies wholly on
/// one side.
fn support_cuts(a: Pt, b: Pt, support: &SegGrid<u32>, cuts: &mut Vec<f64>) -> bool {
    cuts.clear();
    cuts.push(0.0);
    cuts.push(1.0);
    let ab = b.sub(a);
    let mut touches = false;
    support.visit_box(BBox::of(&[a, b]), |c, d, _| {
        let cd = d.sub(c);
        let den = cross(ab, cd);
        let ac = c.sub(a);
        if den == 0 {
            // Boxes that meet, on one line: the two run along each other.
            touches |= cross(ac, ab) == 0;
            return;
        }
        let (t, u) = (cross(ac, cd), cross(ac, ab));
        let inside = |v: i128| if den > 0 { v >= 0 && v <= den } else { v <= 0 && v >= den };
        if inside(t) && inside(u) {
            cuts.push(t as f64 / den as f64);
            touches = true;
        }
    });
    cuts.sort_by(f64::total_cmp);
    let length = a.dist(b);
    cuts.dedup_by(|a, b| (*a - *b).abs() * length < 1.0);
    touches
}

/// How far past the layer below a bead's centre line is still held (mm): half a nozzle. Past it
/// the overhang split calls a wall unsupported.
pub(crate) fn support_margin(nozzle: f64) -> f64 {
    nozzle * 0.5
}

/// What of the layer below holds a line of the outer wall where the island is too thin for the
/// wall to go round ([`makepad_fab_slicer::source::IslandWalls::thin`]): that layer's outline
/// where a printable bead fits (narrower, it printed nothing), grown by the support margin.
///
/// A line that leaves it would hang past what the layer below laid down further than a bead
/// bridges: that is a sliver where a surface grazes the layers, each layer's line out past the
/// last. None of such a line prints; the outer wall's loop closes the island off.
pub struct Held {
    grid: SegGrid<u32>,
}

impl Held {
    /// For `lines`, from the layer below around them. `bead`: the narrowest printable bead (mm).
    /// The layer below printed where it is that wide.
    pub fn new(lower: &[ExPolygon], lines: &[BeadLine], bead: f64, nozzle: f64) -> Self {
        let r = bead / 2.0 * SCALE;
        let margin = support_margin(nozzle) * SCALE;
        // What holds a point depends on the layer below within `2r + margin` of it (the opening,
        // then the margin), corners included within twice that: windows that much wider than
        // each line leave every answer as it is, and the rest of the layer is not worked on.
        let g = (2.0 * (2.0 * r + margin)).ceil() as i64;
        let windows: Vec<ExPolygon> = lines.iter().map(|l| {
            let b = BBox::of(&l.pts);
            ExPolygon::new(Polygon::new(vec![Pt::new(b.min.x - g, b.min.y - g), Pt::new(b.max.x + g, b.min.y - g), Pt::new(b.max.x + g, b.max.y + g), Pt::new(b.min.x - g, b.max.y + g)]))
        }).collect();
        let printed = opening(&intersection_ex(lower, &windows), r, r, Join::Square);
        Self { grid: SegGrid::of_regions(&offset_ex(&printed, margin, Join::Miter(3.0)), |k| k as u32) }
    }

    /// Whether all of `line` stays inside.
    pub fn holds(&self, line: &BeadLine) -> bool {
        let n = line.pts.len();
        let mut cuts = Vec::new();
        (0..if line.closed { n } else { n.saturating_sub(1) }).all(|i| {
            let (a, b) = (line.pts[i], line.pts[(i + 1) % n]);
            support_cuts(a, b, &self.grid, &mut cuts);
            cuts.windows(2).all(|t| self.grid.contains_any(lerp(a, b, 0.5 * (t[0] + t[1]))))
        }) && (n != 1 || self.grid.contains_any(line.pts[0]))
    }
}

fn supported_offset(width: f64, nozzle: f64) -> f64 {
    -width * 0.5 + (width + nozzle) * 0.05
}

fn degree(d: f64, width: f64, nozzle: f64) -> f64 {
    // R1 leaves the classic offset pair and quantisation unspecified.
    // Inferred from the reference's 45-degree outer / 70-degree inner
    // walls: degree 1 starts 10% between -half a bead and +half a nozzle;
    // degree 5 ends at +half a nozzle. Floor to degree tenths. This gives
    // 2.7 (36 mm/s) and 2.3 (44 mm/s) for those two supported walls.
    let lo = supported_offset(width, nozzle);
    let fraction = (d / SCALE - lo) / (nozzle * 0.5 - lo);
    if fraction <= 0.0 {
        return 0.0;
    }
    ((1.0 + 4.0 * fraction).clamp(1.0, 5.0) * 10.0).floor() / 10.0
}

fn degree_speed(d: f64, normal: f64, sp: &OverhangSpeeds) -> f64 {
    if !sp.slowdown {
        return normal;
    }
    let totally = if sp.totally > 0.0 { sp.totally } else { sp.bridge };
    let speeds = [normal, sp.quarter[0], sp.quarter[1], sp.quarter[2], sp.quarter[3], totally];
    let speed = |i: usize| if speeds[i] > 0.0 { speeds[i].min(normal) } else { normal };
    let i = (d.floor() as usize).min(4);
    let t = (d - i as f64).clamp(0.0, 1.0);
    speed(i) * (1.0 - t) + speed(i + 1) * t
}

/// S3 along one wall line (a closed ring or an open bead line) from its
/// vertex 0: where its bead stands on the layer below, with the overhang
/// degree there, and where it hangs over air. Support is geometric and
/// separate from speed: a supported wall keeps its role even when its
/// degree slows it down (R1 §3.3). The path made from the line (seam, seam
/// gap, random-seam points, width steps, texture) takes the labels by arc
/// position ([`apply_labels`]), so a GPU slicer's labels of its own rings
/// mean the same.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Labels {
    /// Consecutive pieces from 0 to `length`, in mm along the line.
    pub pieces: Vec<Label>,
    /// The full-support cuts, mm along the line: speed ramps start there.
    pub seeds: Vec<f64>,
    /// The line's length in mm (a ring's with its closing segment).
    pub length: f64,
}

/// A piece of [`Labels`]: `from..to` mm along the line, supported at an
/// overhang degree (0 ..= 5), or over air.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Label {
    pub from: f64,
    pub to: f64,
    pub supported: bool,
    pub degree: f64,
}

/// Where a wall path lies on its island's [`WallShape`]: its line (the rings
/// wall by wall, or the bead lines) and the arc position (mm) of its first
/// point on that line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Along {
    pub line: usize,
    pub start: f64,
}

/// Line `i` of a shape (numbered as [`Along`] numbers them) and whether it is closed.
pub fn shape_line(shape: &WallShape, i: usize) -> (&[Pt], bool) {
    match shape {
        WallShape::Rings(rings) => {
            let mut k = i;
            for wall in rings {
                if k < wall.len() {
                    return (&wall[k].pts, true);
                }
                k -= wall.len();
            }
            panic!("shape_line: no ring {i}")
        }
        WallShape::Beads(lines) => (&lines[i].pts, lines[i].closed),
    }
}

/// The arc length (mm) along `pts` from its first point to point `i`.
fn arc_to(pts: &[Pt], i: usize) -> f64 {
    pts[..=i].windows(2).map(|w| w[0].dist(w[1]) / SCALE).sum()
}

/// The layer below as the overhang split reads it: its outline grown by the
/// support and full-support margins, made once per layer (and per bead
/// width) instead of once per wall.
pub struct Lower<'a> {
    lower: &'a [ExPolygon],
    /// The outline itself, and grown by the support / full-support margins,
    /// each as a grid of its ring edges.
    grid: Option<SegGrid<u32>>,
    support: Option<SegGrid<u32>>,
    full: Vec<(f64, SegGrid<u32>)>,
    nozzle: f64,
    /// Variable-width beads take their full-support margin from their
    /// role's width (outer, inner), not each from its own.
    nominal: Option<(f64, f64)>,
}

impl<'a> Lower<'a> {
    pub fn new(lower: &'a [ExPolygon], sp: &OverhangSpeeds) -> Self {
        Self { lower, grid: None, support: None, full: Vec::new(), nozzle: sp.nozzle, nominal: None }
    }

    /// For variable-width beads of these outer and inner wall widths.
    pub fn for_beads(self, outer: f64, inner: f64) -> Self {
        Self { nominal: Some((outer, inner)), ..self }
    }

    fn prepare(&mut self, width: f64) -> usize {
        let (lower, nozzle) = (self.lower, self.nozzle);
        // Reference role transitions lie half a nozzle outside the lower
        // outline. Unlike a sampling boundary this conserves their lengths.
        self.support.get_or_insert_with(|| SegGrid::of_regions(&offset_ex(lower, support_margin(nozzle) * SCALE, Join::Miter(3.0)), |k| k as u32));
        self.grid.get_or_insert_with(|| SegGrid::of_regions(lower, |k| k as u32));
        if let Some(k) = self.full.iter().position(|(w, _)| *w == width) {
            return k;
        }
        self.full.push((width, SegGrid::of_regions(&offset_ex(lower, supported_offset(width, nozzle) * SCALE, Join::Miter(3.0)), |k| k as u32)));
        self.full.len() - 1
    }
}

/// The [`Labels`] of a line whose bead has role `role` and width `width`,
/// against a prepared layer below.
pub fn label_line(pts: &[Pt], closed: bool, role: Role, width: f64, below: &mut Lower, sp: &OverhangSpeeds) -> Labels {
    let mut out = Labels::default();
    let n = pts.len();
    if !sp.enabled || n < 2 || width <= 0.0 || sp.nozzle <= 0.0 {
        return out;
    }
    let margin_width = below.nominal.map_or(width, |(o, i)| if role == Role::OuterWall { o } else { i });
    let fk = {
        let _t = crate::prof::scope(crate::prof::Stage::OverhangPrepare);
        below.prepare(margin_width)
    };
    // Inside an inset of the layer below by this bead's own margin or more, the bead is fully
    // supported: its degree is zero, with no distance to measure. Next to the inset's edge
    // rounding decides that, so a segment touching the edge still measures.
    let full_means_zero = width <= margin_width && supported_offset(margin_width, sp.nozzle) < 0.0;
    let lower = below.grid.as_ref().unwrap();
    let support = below.support.as_ref().unwrap();
    let full_support = &below.full[fk].1;
    let mut walked = 0.0;
    // Each segment's cuts, in buffers the whole line reuses.
    let (mut cuts, mut full_cuts) = (Vec::new(), Vec::new());
    // Which side of each boundary the line is on, known for as long as it touches that boundary
    // nowhere: a piece asks the grid only after a touch.
    let (mut in_support, mut in_full) = (None, None);
    for si in 0..if closed { n } else { n - 1 } {
        let (a, b) = (pts[si], pts[(si + 1) % n]);
        let cuts_t = crate::prof::scope(crate::prof::Stage::OverhangCuts);
        let touches_support = support_cuts(a, b, support, &mut cuts);
        let touches_full = support_cuts(a, b, full_support, &mut full_cuts);
        if touches_support {
            in_support = None;
        }
        if touches_full {
            in_full = None;
        }
        let length = a.dist(b) / SCALE;
        out.seeds.extend(full_cuts.iter().filter(|&&t| t > 0.0 && t < 1.0).map(|&t| walked + t * length));
        cuts.extend_from_slice(&full_cuts);
        cuts.sort_by(f64::total_cmp);
        cuts.dedup_by(|u, v| (*u - *v).abs() * a.dist(b) < 1.0);
        drop(cuts_t);
        for t in cuts.windows(2) {
            let (p, q) = (lerp(a, b, t[0]), lerp(a, b, t[1]));
            if p == q {
                continue;
            }
            let mid = lerp(p, q, 0.5);
            let supported = in_support.unwrap_or_else(|| support.contains_any(mid));
            if !touches_support {
                in_support = Some(supported);
            }
            let full = supported && in_full.unwrap_or_else(|| full_support.contains_any(mid));
            if supported && !touches_full {
                in_full = Some(full);
            }
            // Degree samples only subdivide supported material. The exact
            // unsupported boundary above must never move with sample size.
            let partial = supported && !full;
            let _degree_t = crate::prof::scope(crate::prof::Stage::OverhangDegree);
            let k_n = if partial && sp.slowdown { (p.dist(q) / (2.0 * SCALE)).ceil().max(1.0) as usize } else { 1 };
            // Positions come from the cut parameters, so a path segment on
            // this one cuts at the same points.
            let at = |k: usize| walked + length * if k == k_n { t[1] } else { t[0] + (t[1] - t[0]) * k as f64 / k_n as f64 };
            for k in 0..k_n {
                let (u, v) = (lerp(p, q, k as f64 / k_n as f64), lerp(p, q, (k + 1) as f64 / k_n as f64));
                let degree = match (supported, full && full_means_zero && !touches_full) {
                    (false, _) => 5.0,
                    (true, true) => 0.0,
                    (true, false) => degree(signed_distance(lerp(u, v, 0.5), lower), width, sp.nozzle),
                };
                out.pieces.push(Label { from: at(k), to: at(k + 1), supported, degree });
            }
        }
        walked += length;
    }
    out.length = walked;
    out.pieces = sampled(std::mem::take(&mut out.pieces), sp.nozzle);
    out
}

/// A degree sample's reach along a finely tessellated line (mm).
const DEGREE_SAMPLE: f64 = 2.0;
/// The steps a sampled degree comes in.
const DEGREE_STEP: f64 = 0.25;

/// Supported pieces shorter than `short` come from how finely the line is tessellated, not from
/// the part: they join into samples about [`DEGREE_SAMPLE`] long at their mean degree, to the
/// nearest [`DEGREE_STEP`], so a smooth surface does not change speed at every point. A piece of
/// the line's own length keeps its degree; where the line leaves support stays exact.
fn sampled(pieces: Vec<Label>, short: f64) -> Vec<Label> {
    let mut out: Vec<Label> = Vec::with_capacity(pieces.len());
    // The growing sample's length-weighted degree, while the last piece out is one.
    let mut growing = None::<f64>;
    let close = |l: &mut Label, weighted: f64| l.degree = (weighted / (l.to - l.from) / DEGREE_STEP).round() * DEGREE_STEP;
    for p in pieces {
        let fine = p.supported && p.to - p.from < short;
        let joins = fine && growing.is_some() && out.last().is_some_and(|l| l.to - l.from < DEGREE_SAMPLE);
        if joins {
            let l = out.last_mut().expect("a sample is growing");
            growing = growing.map(|w| w + p.degree * (p.to - p.from));
            l.to = p.to;
            continue;
        }
        if let Some(weighted) = growing.take() {
            close(out.last_mut().expect("a sample is growing"), weighted);
        }
        if fine {
            growing = Some(p.degree * (p.to - p.from));
        }
        out.push(p);
    }
    if let Some(weighted) = growing {
        close(out.last_mut().expect("a sample is growing"), weighted);
    }
    out
}

/// Wall `path` in pieces by its line's [`Labels`]: `start` is the arc
/// position of its first point on the line, `moved` (when a texture moved
/// its points off the line) each point's arc position from the first. A
/// supported piece keeps its role at its degree's speed; a piece over air
/// is an overhang wall at the bridge bead and speed.
pub fn apply_labels(path: ExtrusionPath, labels: &Labels, start: f64, moved: Option<&[f64]>, sp: &OverhangSpeeds) -> Vec<ExtrusionPath> {
    // Pieces are rebuilt planar: 3D paths (ramps) are made after this pass,
    // never fed to it. A variable-width bead keeps each segment's flow.
    assert!(path.z.is_empty(), "apply_labels: a 3D path");
    if !sp.enabled || path.pts.len() < 2 || path.width <= 0.0 || sp.nozzle <= 0.0 || labels.pieces.is_empty() {
        return vec![path];
    }
    let at: Vec<f64> = match moved {
        Some(m) => {
            assert_eq!(m.len(), path.pts.len(), "apply_labels: one position per point");
            m.iter().map(|s| start + s).collect()
        }
        None => {
            let mut s = start;
            std::iter::once(start).chain(path.pts.windows(2).map(|w| {
                s += w[0].dist(w[1]) / SCALE;
                s
            })).collect()
        }
    };
    let (len, pieces) = (labels.length, &labels.pieces);
    // Positions past the line's end wrap round a ring.
    let label = |x: f64| {
        let x = if len > 0.0 { x.rem_euclid(len) } else { x };
        pieces[pieces.partition_point(|p| p.to <= x).min(pieces.len() - 1)]
    };
    // The line's positions c (label starts or seeds) inside (lo, hi), on every lap.
    let inside = |cs: &[f64], lo: f64, hi: f64, out: &mut Vec<f64>| {
        let laps = if len > 0.0 { (lo / len).floor() as i64..=(hi / len).floor() as i64 } else { 0..=0 };
        for lap in laps {
            let base = lap as f64 * len;
            let first = cs.partition_point(|&c| c + base <= lo);
            out.extend(cs[first..].iter().map(|&c| c + base).take_while(|&x| x < hi));
        }
    };
    let starts: Vec<f64> = pieces.iter().map(|p| p.from).collect();
    let mut ramp_seeds = Vec::new();
    let mut walked = 0.0;
    let mut out: Vec<ExtrusionPath> = Vec::new();
    let mut stops = Vec::new();
    for (si, wdw) in path.pts.windows(2).enumerate() {
        let (a, b) = (wdw[0], wdw[1]);
        let (sa, sb) = (at[si], at[si + 1]);
        let length = a.dist(b) / SCALE;
        let seg_flow = path.flow.get(si).map(|&f| vec![f]).unwrap_or_default();
        let t_of = |x: f64| if sb > sa { ((x - sa) / (sb - sa)).clamp(0.0, 1.0) } else { 0.0 };
        let mut seeds = Vec::new();
        inside(&labels.seeds, sa, sb, &mut seeds);
        ramp_seeds.extend(seeds.iter().map(|&x| walked + t_of(x) * length));
        stops.clear();
        stops.push(sa);
        inside(&starts, sa, sb, &mut stops);
        stops.push(sb);
        walked += length;
        for x in stops.windows(2) {
            let (u, v) = (lerp(a, b, t_of(x[0])), lerp(a, b, t_of(x[1])));
            if u == v {
                continue;
            }
            let l = label(0.5 * (x[0] + x[1]));
            let mut piece = ExtrusionPath { pts: vec![u, v], closed: false, loop_pts: Vec::new(), dry: Vec::new(), z: Vec::new(), flow: seg_flow.clone(), mode: path.mode, core: path.core, overhang: path.overhang, role: path.role, width: path.width, height: path.height, area: path.area, speed: path.speed };
            if l.supported {
                piece.speed = degree_speed(l.degree, path.speed, sp);
                piece.overhang = l.degree as f32;
            } else {
                piece.role = Role::OverhangWall;
                piece.overhang = 5.0;
                // The reference keeps normal layer height but uses the
                // inner-wall bead for unsupported outer walls (a bead
                // of any width bridges at that flow).
                piece.width = sp.width;
                piece.flow.clear();
                piece.area = flow::cross_section(piece.width, path.height);
                piece.speed = if sp.slowdown && sp.totally > 0.0 { sp.totally } else { sp.bridge };
                if piece.speed <= 0.0 {
                    piece.speed = path.speed;
                }
                if sp.max_volumetric > 0.0 && piece.area > 0.0 {
                    piece.speed = piece.speed.min(sp.max_volumetric / piece.area);
                }
                piece.speed = piece.speed.min(path.speed);
            }
            append_run(&mut out, piece);
        }
    }
    let _runs_t = crate::prof::scope(crate::prof::Stage::OverhangRuns);
    for piece in &mut out {
        simplify_run(piece);
    }
    // Sub-nozzle degree bands at corners belong to the adjacent longer
    // supported run. Inferred from the reference's unbroken straight
    // walls: these tiny bands are not separate speed plateaus.
    if sp.smooth > 0.0 && sp.slowdown {
        out = blend_short_runs(out, sp.nozzle, path.closed);
    }
    for piece in &mut out {
        simplify_run(piece);
    }
    if out.len() == 1 && out[0].role == path.role && (out[0].speed - path.speed).abs() < 1e-9 {
        return vec![path];
    }
    if sp.smooth > 0.0 && sp.slowdown {
        let gap = if path.closed { (Polyline::new(path.loop_pts.clone()).length() / SCALE - walked).max(0.0) } else { 0.0 };
        out = smooth_runs(out, &ramp_seeds, sp.smooth, sp.nozzle, path.closed, gap);
    }
    // The last piece ends where the loop did, so it wipes along the loop like the whole wall
    // would. It is the loop only when the loop stayed in one piece: a run of a loop is open, and
    // a closed piece would be read as the whole ring (a scarf, a paint split, a bead check).
    let whole = out.len() == 1;
    if let Some(last) = out.last_mut() {
        if path.closed {
            last.loop_pts = path.loop_pts.clone();
            last.closed = whole;
        }
    }
    out
}

fn append_run(out: &mut Vec<ExtrusionPath>, piece: ExtrusionPath) {
    if let Some(prev) = out.last_mut() {
        if prev.role == piece.role && (prev.speed - piece.speed).abs() < 1e-9 && prev.overhang == piece.overhang && prev.last() == piece.first() && prev.flow.is_empty() == piece.flow.is_empty() {
            prev.pts.extend(piece.pts.into_iter().skip(1));
            prev.flow.extend(piece.flow);
            return;
        }
    }
    out.push(piece);
}

/// Drop collinear points of a run; a variable-width run keeps every point
/// (each segment has its own flow).
fn simplify_run(piece: &mut ExtrusionPath) {
    if piece.flow.is_empty() {
        piece.pts = simplify_polyline(&piece.pts, 1.0);
    }
}

fn blend_short_runs(runs: Vec<ExtrusionPath>, nozzle: f64, closed: bool) -> Vec<ExtrusionPath> {
    let short: Vec<bool> = runs.iter().map(|p| p.length_mm() < nozzle).collect();
    let mut neighbours = vec![None::<f64>; runs.len()];
    // Find the nearest long supported run on either side in linear time.
    // An unsupported run is a boundary, never a source of a faster speed.
    for reverse in [false, true] {
        let mut speed = None::<f64>;
        for _ in 0..if closed { 2 } else { 1 } {
            for k in 0..runs.len() {
                let i = if reverse { runs.len() - 1 - k } else { k };
                if runs[i].role == Role::OverhangWall {
                    speed = None;
                } else if !short[i] {
                    speed = Some(runs[i].speed);
                } else if let Some(v) = speed {
                    neighbours[i] = Some(neighbours[i].map_or(v, |old| old.max(v)));
                }
            }
        }
    }
    let mut out = Vec::new();
    for (i, mut piece) in runs.into_iter().enumerate() {
        if let Some(speed) = neighbours[i] {
            piece.speed = speed;
        }
        append_run(&mut out, piece);
    }
    out
}

/// R1 specifies smoothing, but not its profile or sampling. Inferred from
/// reference F values: sqrt(speed) grows by coefficient / 2 per mm away
/// from a slow run, in both directions. Samples start at the full-support
/// offset and continue one nozzle apart, including the first sample that
/// reaches normal speed. Cooling may cap several samples to the same F;
/// their vertices still belong to the wall and must not be simplified.
fn smooth_runs(runs: Vec<ExtrusionPath>, seeds: &[f64], coefficient: f64, step: f64, closed: bool, gap: f64) -> Vec<ExtrusionPath> {
    assert!(runs.iter().all(|r| r.z.is_empty()), "smooth_runs: a 3D path");
    let n = runs.len();
    if n < 2 {
        return runs;
    }
    let slope = coefficient * 0.5;
    let lengths: Vec<f64> = runs.iter().map(|p| p.length_mm()).collect();
    let speeds: Vec<f64> = runs.iter().map(|p| p.speed.sqrt()).collect();
    let mut ends = vec![f64::MAX; n + 1];
    for i in 0..n {
        ends[i] = ends[i].min(speeds[i]);
        ends[i + 1] = ends[i + 1].min(speeds[i]);
    }
    // A linear distance transform on the loop: two laps in each direction
    // cover wraparound without a quadratic search from every slow piece.
    for _ in 0..if closed { 2 } else { 1 } {
        if closed {
            ends[0] = ends[0].min(ends[n] + slope * gap);
        }
        for i in 0..n {
            ends[i + 1] = ends[i + 1].min(ends[i] + slope * lengths[i]);
        }
    }
    for _ in 0..if closed { 2 } else { 1 } {
        if closed {
            ends[n] = ends[n].min(ends[0] + slope * gap);
        }
        for i in (0..n).rev() {
            ends[i] = ends[i].min(ends[i + 1] + slope * lengths[i]);
        }
    }
    let mut out = Vec::new();
    let mut start = 0.0;
    for (i, run) in runs.into_iter().enumerate() {
        let length = lengths[i];
        let base = speeds[i];
        let left = ((base - ends[i]) / slope).min(length);
        let right = ((base - ends[i + 1]) / slope).min(length);
        if left < 1e-6 && right < 1e-6 {
            append_run(&mut out, run);
            start += length;
            continue;
        }
        let mut stations = vec![0.0];
        for ab in run.pts.windows(2) {
            stations.push(stations.last().unwrap() + ab[0].dist(ab[1]) / SCALE);
        }
        // A ramp steps by `step`. Its cuts keep the corners of a line in long segments; a fine
        // tessellation's points stay inside a step, whose one speed lets them fit as arcs. A
        // variable-width run cuts at every point: each segment has its own flow.
        let mut cuts: Vec<f64> = stations.iter().enumerate().filter(|&(j, &x)| {
            j == 0 || j + 1 == stations.len() || !run.flow.is_empty() || x - stations[j - 1] >= step || stations[j + 1] - x >= step
        }).map(|(_, &x)| x).collect();
        let lo = seeds.partition_point(|&s| s <= start + 1e-6);
        let hi = seeds.partition_point(|&s| s < start + length - 1e-6);
        // Ramps from both ends meet in a short run: each steps up to the meeting, not through it.
        let meet = (left + right > length).then(|| ((ends[i + 1] - ends[i]) / slope + length) * 0.5);
        let mut ramp = Vec::new();
        if left > 1e-6 {
            let first = if lo < hi { (seeds[lo] - start).min(step) } else { step };
            ramp_cuts(&mut ramp, first, left, length, step, false);
            cuts.extend(ramp.drain(..).filter(|&c| meet.is_none_or(|m| c <= m)));
        }
        if right > 1e-6 {
            let first = if lo < hi { (start + length - seeds[hi - 1]).min(step) } else { step };
            ramp_cuts(&mut ramp, first, right, length, step, true);
            cuts.extend(ramp.drain(..).filter(|&c| meet.is_none_or(|m| c >= m)));
        }
        if let Some(m) = meet {
            cuts.push(m);
        }
        cuts.sort_by(f64::total_cmp);
        cuts.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        let point = |s: f64| {
            if s >= length - 1e-6 {
                return *run.pts.last().expect("a run has points");
            }
            let j = stations.partition_point(|&x| x <= s).saturating_sub(1).min(run.pts.len() - 2);
            let t = (s - stations[j]) / (stations[j + 1] - stations[j]);
            lerp(run.pts[j], run.pts[j + 1], t.clamp(0.0, 1.0))
        };
        let speed = |s: f64| base.min(ends[i] + slope * s).min(ends[i + 1] + slope * (length - s));
        for ss in cuts.windows(2) {
            let (a, b) = (point(ss[0]), point(ss[1]));
            if a == b {
                continue;
            }
            let speed = speed(ss[0]).max(speed(ss[1])).powi(2);
            // A variable-width piece lies on one segment.
            let seg = stations.partition_point(|&x| x <= 0.5 * (ss[0] + ss[1])).saturating_sub(1).min(run.pts.len() - 2);
            let flow = run.flow.get(seg).map(|&f| vec![f]).unwrap_or_default();
            let inside = stations.iter().enumerate().filter(|&(_, &x)| x > ss[0] + 1e-6 && x < ss[1] - 1e-6).map(|(j, _)| run.pts[j]);
            let pts: Vec<Pt> = std::iter::once(a).chain(inside).chain(std::iter::once(b)).collect();
            let piece = ExtrusionPath { pts, speed, closed: false, loop_pts: Vec::new(), dry: Vec::new(), z: Vec::new(), flow, mode: run.mode, core: run.core, overhang: run.overhang, role: run.role, width: run.width, height: run.height, area: run.area };
            append_run(&mut out, piece);
        }
        start += length;
    }
    out
}

fn ramp_cuts(cuts: &mut Vec<f64>, first: f64, extent: f64, length: f64, step: f64, reverse: bool) {
    let mut d = first.min(length);
    loop {
        cuts.push(if reverse { length - d } else { d });
        if d >= extent || d >= length {
            break;
        }
        d = (d + step).min(length);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Pt> {
        vec![Pt::from_mm(x0, y0), Pt::from_mm(x1, y0), Pt::from_mm(x1, y1), Pt::from_mm(x0, y1)]
    }

    fn speeds() -> OverhangSpeeds {
        OverhangSpeeds { enabled: true, slowdown: true, quarter: [40.0, 30.0, 20.0, 10.0], totally: 5.0, bridge: 20.0, nozzle: 0.4, width: 0.45, smooth: 0.0, max_volumetric: 0.0 }
    }

    /// Arc positions (mm from the first point) where the role changes, and the length over air.
    fn roles(pieces: &[ExtrusionPath]) -> (Vec<f64>, f64) {
        let (mut at, mut marks, mut air) = (0.0, Vec::new(), 0.0);
        for (i, p) in pieces.iter().enumerate() {
            if i > 0 && pieces[i - 1].role != p.role {
                marks.push(at);
            }
            if p.role == Role::OverhangWall {
                air += p.length_mm();
            }
            at += p.length_mm();
        }
        (marks, air)
    }

    /// A gap line's steep taper steps no shorter than half a bead and keeps its volume; a wall's
    /// keeps its fine steps.
    #[test]
    fn a_steep_gap_taper_steps_coarsely() {
        let pts = [Pt::from_mm(0.0, 0.0), Pt::from_mm(0.7, 0.0)];
        let widths = [0.05, 0.45];
        let gap = gap_paths(&[BeadLine { pts: pts.to_vec(), widths: widths.to_vec(), closed: false, inset: 0, odd: true }], 0.2, pts[0]);
        assert_eq!(gap.len(), 1);
        let path = &gap[0];
        assert_eq!(path.pts.len(), 4, "three steps of at least 0.2 mm");
        let wide = path.flow.iter().map(|f| f * crate::flow::spacing(path.width, 0.2)).collect::<Vec<_>>();
        let volume: f64 = wide.iter().map(|w| w * 0.7 / 3.0).sum();
        assert!((volume - 0.5 * (0.05 + 0.45) * 0.7).abs() < 1e-6, "{wide:?}");
        let wall = bead_path(&pts, &widths, false, 0, Role::OuterWall, 0.2, 0.0);
        assert!(wall.pts.len() >= 9, "at most 0.05 mm of width per step: {}", wall.pts.len());
    }

    /// A wall run of `n` points 0.1 mm apart along x at `speed`.
    fn fine_run(from: f64, n: usize, speed: f64) -> ExtrusionPath {
        let pts = (0..n).map(|i| Pt::from_mm(from + 0.1 * i as f64, 0.0)).collect();
        ExtrusionPath { role: Role::OuterWall, pts, closed: false, loop_pts: Vec::new(), width: 0.42, height: 0.2, area: 0.08, speed, dry: Vec::new(), z: Vec::new(), flow: Vec::new(), mode: 0, core: false, overhang: 0.0 }
    }

    /// A fine line's degree changes every sample, not every point, at the mean of what it passes,
    /// in the steps the speeds come in; a piece of the line's own length keeps its own.
    #[test]
    fn a_fine_line_samples_its_degree() {
        let label = |from: f64, to: f64, degree: f64| Label { from, to, supported: true, degree };
        let fine: Vec<Label> = (0..50).map(|i| label(0.1 * i as f64, 0.1 * (i + 1) as f64, 1.0 + 0.02 * i as f64)).collect();
        let out = sampled(fine, 0.4);
        assert_eq!(out.len(), 3, "{out:?}");
        assert!(out.windows(2).all(|w| w[0].to == w[1].from));
        assert!(out.iter().all(|l| (l.degree / DEGREE_STEP).fract() == 0.0));
        // The first 2 mm climb from 1.0 to 1.38 around 1.19, and the mean takes the nearest step.
        assert_eq!(out[0].degree, 1.25);
        let long = label(0.0, 3.0, 2.1);
        assert_eq!(sampled(vec![long.clone()], 0.4), vec![long]);
    }

    /// A ramp over a fine line steps by its step and no finer: the points inside a step stay with
    /// it at one speed, where they can fit as an arc, and each step starts where the last ended
    /// so that no zero-length travel comes between them.
    #[test]
    fn a_ramp_over_a_fine_line_steps_whole() {
        let runs = vec![fine_run(0.0, 21, 10.0), fine_run(2.0, 61, 200.0)];
        let out = smooth_runs(runs, &[], 4.0, 0.4, false, 0.0);
        assert!(out.windows(2).all(|w| w[0].pts.last() == w[1].pts.first()), "a gap between steps");
        let ramp: Vec<&ExtrusionPath> = out.iter().filter(|p| p.speed > 10.0 && p.speed < 200.0).collect();
        assert!(!ramp.is_empty(), "the fast run ramps up");
        assert!(ramp.iter().all(|p| p.length_mm() > 0.39 && p.length_mm() < 0.41 && p.pts.len() >= 4), "{:?}", ramp.iter().map(|p| (p.length_mm(), p.pts.len())).collect::<Vec<_>>());
        assert!(out.windows(2).all(|w| w[0].speed != w[1].speed), "same speeds join");
    }

    /// A 12 x 6 mm wall ring standing 4 mm out past a 10 mm square below: its labels, made once
    /// on the ring, split every path made from it (any seam vertex, a seam gap, seam-candidate
    /// midpoints) where the ring changes support, measured from the path's own start.
    #[test]
    fn labels_on_the_ring_split_every_path_made_from_it() {
        let lower = vec![ExPolygon::new(Polygon::new(rect(0.0, 0.0, 10.0, 10.0)))];
        let ring = rect(2.0, 2.0, 14.0, 8.0);
        let sp = speeds();
        let mut below = Lower::new(&lower, &sp);
        let labels = label_line(&ring, true, Role::OuterWall, 0.42, &mut below, &sp);
        assert!((labels.length - 36.0).abs() < 1e-9);
        let air: f64 = labels.pieces.iter().filter(|l| !l.supported).map(|l| l.to - l.from).sum();
        // Past x = 10.2 (half a nozzle out) on the long edges, and the whole far edge.
        assert!((air - (2.0 * 3.8 + 6.0)).abs() < 1e-6, "over air {air}");
        assert!(labels.pieces.iter().any(|l| l.supported && l.degree > 0.0 && l.degree < 5.0), "a partial band");
        assert!(labels.pieces.windows(2).all(|w| w[0].to == w[1].from));
        let flips: Vec<f64> = labels.pieces.windows(2).filter(|w| w[0].supported != w[1].supported).map(|w| w[1].from).collect();
        assert_eq!(flips.len(), 2);
        let mut with_midpoints = Vec::new();
        for k in 0..4 {
            with_midpoints.push(ring[k]);
            with_midpoints.push(Pt::new((ring[k].x + ring[(k + 1) % 4].x) / 2, (ring[k].y + ring[(k + 1) % 4].y) / 2));
        }
        let mut cases: Vec<(&[Pt], usize, f64)> = Vec::new();
        for start in 0..4 {
            for gap in [0.0, 0.3] {
                cases.push((&ring[..], start, gap));
            }
        }
        cases.extend([(&with_midpoints[..], 1, 0.3), (&with_midpoints[..], 4, 0.0)]);
        for (pts, start, gap) in cases {
            let path = ExtrusionPath { speed: 50.0, ..loop_path(pts, start, Role::OuterWall, 0.42, gap) };
            let from = arc_to(pts, start);
            let length = 36.0 - gap;
            let whole = path.loop_pts.clone();
            let pieces = apply_labels(path, &labels, from, None, &sp);
            // Runs of a loop are open; the last ends where the loop did and wipes along it.
            assert!(pieces.len() > 1 && pieces.iter().all(|p| !p.closed), "seam {start} gap {gap}: open runs");
            assert_eq!(pieces.last().unwrap().loop_pts, whole, "seam {start} gap {gap}: the last run wipes along the loop");
            let (marks, over) = roles(&pieces);
            let mut want: Vec<f64> = flips.iter().map(|f| (f - from).rem_euclid(36.0)).filter(|&x| x > 0.0 && x < length).collect();
            want.sort_by(f64::total_cmp);
            assert_eq!(marks.len(), want.len(), "seam {start} gap {gap}: {marks:?} vs {want:?}");
            for (m, w) in marks.iter().zip(&want) {
                assert!((m - w).abs() < 1e-6, "seam {start} gap {gap}: {marks:?} vs {want:?}");
            }
            let air_in = |a: f64, b: f64| labels.pieces.iter().filter(|l| !l.supported).map(|l| (l.to.min(b) - l.from.max(a)).max(0.0)).sum::<f64>();
            let want_air = air_in(from, from + length) + air_in(from - 36.0, from + length - 36.0);
            assert!((over - want_air).abs() < 1e-6, "seam {start} gap {gap}: over air {over} vs {want_air}");
            assert!(pieces.iter().all(|p| p.role != Role::OverhangWall || (p.width == sp.width && p.overhang == 5.0)));
        }
        // Moved points (as a texture moves them) keep their positions on the line.
        let path = ExtrusionPath { speed: 50.0, ..loop_path(&ring, 0, Role::OuterWall, 0.42, 0.0) };
        let mut moved = path.clone();
        moved.pts = path.pts.iter().map(|p| Pt::new(p.x + 1000, p.y - 1000)).collect();
        let at: Vec<f64> = (0..path.pts.len()).map(|i| arc_to(&path.pts, i)).collect();
        let (marks, _) = roles(&apply_labels(moved, &labels, 0.0, Some(&at), &sp));
        assert_eq!(marks.len(), flips.len());
    }
}

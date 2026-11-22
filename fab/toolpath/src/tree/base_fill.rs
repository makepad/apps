//! The support-base fill (R14 §4.4, "the support-base fill"): the lines tips are sampled on, as
//! the reference makes them, joined along the piece's rings into chains.
//!
//! Each piece of the area is filled on its own, in a line frame where the lines are vertical:
//! the piece turned by −(angle + 90°), then shrunk by h. Its grid lines run one through the
//! centre of the box of the piece's outer ring and the others L apart, within the turned piece's
//! x range. Each pair of crossings of a grid line with the shrunk piece is a line, from its lower
//! end to its upper end. The ends sit on the shrunk piece's rings, and the ten steps join them
//! along the rings, hook short stretches onto them or put out stretches of their own.
//!
//! Notation (all in units inside): L the line spacing, s the flow spacing, h = s/2, ε = 100 nm.
//! A ring runs forward counter-clockwise (outer) or clockwise (hole) in the line frame. Each end
//! has a back side and a forward side, each with a free length along the ring towards the
//! neighbouring end and a mask flag.

use std::collections::VecDeque;

use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::Join;

use super::clip::union_ex;
use super::offset::offset;

/// ε (units): a free length must be over it to count, a hook at least it to be drawn.
const EPS: f64 = 100.0;
/// How far past h (units) a free stretch may lie from its own line and still run along it.
const ALONG: f64 = 50.0;
/// How near (units) a band piece must start to the last polyline's end to be added to it.
const JOIN_GAP: f64 = 10.0;
/// A line's middle is the line less this many flow spacings off each end.
const MIDDLE_CUT: f64 = 1.7;
/// The masking strip's radius round a line's middle, in flow spacings.
const STRIP: f64 = 0.8;
/// What a hook falls short of the neighbouring end by, in h, when it cuts an unmasked side.
const HOOK_SHORT: f64 = 0.3;

const BACK: usize = 0;
const FWD: usize = 1;

/// One fill call (mm, degrees).
#[derive(Clone, Copy, Debug)]
pub struct Fill {
    /// The lines' angle from the bed's +X axis, counter-clockwise.
    pub angle_deg: f64,
    /// L: the line spacing, the flow spacing ÷ the density, unrounded.
    pub spacing: f64,
    /// s: the flow spacing.
    pub flow: f64,
    /// The L that steps 6, 9 and 10's limits (1.3 L, 2 L, 3 L) are frozen at.
    pub frozen: f64,
    /// Where the grid's one line runs through (in the part's frame): each piece's outer ring's box
    /// centre when none (§4.4's sampling), or one point that every piece and layer share (the
    /// printed fills, §9.5).
    pub anchor: Option<Pt>,
}

/// The polylines of the fill of `area`, piece by piece of its union in the reference's piece
/// order, each piece's in the reference's order: free-ring loops, band pieces, leftovers, then
/// chains and lone lines in slot order.
pub fn fill(area: &[ExPolygon], f: &Fill) -> Vec<Vec<Pt>> {
    let frame = Frame::new(f.angle_deg);
    union_ex(area).iter().flat_map(|piece| Piece::new(piece, f, &frame).map(|p| p.run()).unwrap_or_default()).collect()
}

/// A point in units, in the line frame.
type P = (f64, f64);

fn dist(a: P, b: P) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn lerp(a: P, b: P, t: f64) -> P {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// The distance from `p` to segment `a`-`b`.
fn to_segment(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    if l2 == 0.0 {
        return dist(p, a);
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0);
    dist(p, (a.0 + dx * t, a.1 + dy * t))
}

fn length(pts: &[P]) -> f64 {
    pts.windows(2).map(|w| dist(w[0], w[1])).sum()
}

/// The turn into the line frame and back: by −(angle + 90°), the angle as a float, each point
/// rounded to the nearest nm.
#[derive(Clone, Copy)]
struct Frame {
    cos: f64,
    sin: f64,
}

impl Frame {
    fn new(angle_deg: f64) -> Frame {
        // The reference turns by a single-precision angle: a few nm off far from the origin.
        let a = (-(angle_deg + 90.0).to_radians()) as f32 as f64;
        Frame { cos: a.cos(), sin: a.sin() }
    }

    fn to(&self, p: Pt) -> Pt {
        let (x, y) = (p.x as f64, p.y as f64);
        Pt::new((x * self.cos - y * self.sin).round() as i64, (x * self.sin + y * self.cos).round() as i64)
    }

    fn back(&self, p: P) -> Pt {
        Pt::new((p.0 * self.cos + p.1 * self.sin).round() as i64, (-p.0 * self.sin + p.1 * self.cos).round() as i64)
    }

    fn to_ex(&self, e: &ExPolygon) -> ExPolygon {
        let ring = |r: &Polygon| Polygon::new(r.pts.iter().map(|&p| self.to(p)).collect());
        ExPolygon { contour: ring(&e.contour), holes: e.holes.iter().map(ring).collect() }
    }
}

/// One ring of the shrunk piece, with the ends on it.
struct Ring {
    pts: Vec<P>,
    /// The arc position of each point, and the ring's length last.
    cum: Vec<f64>,
    len: f64,
    /// The ends on it, by arc position.
    ends: Vec<usize>,
}

impl Ring {
    fn new(pts: Vec<P>) -> Ring {
        let n = pts.len();
        let mut cum = Vec::with_capacity(n + 1);
        let mut at = 0.0;
        for i in 0..n {
            cum.push(at);
            at += dist(pts[i], pts[(i + 1) % n]);
        }
        cum.push(at);
        Ring { pts, cum, len: at, ends: Vec::new() }
    }

    fn wrap(&self, pos: f64) -> f64 {
        pos.rem_euclid(self.len)
    }

    /// The segment `pos` lies on.
    fn seg(&self, pos: f64) -> usize {
        let pos = self.wrap(pos);
        self.cum[1..self.pts.len()].partition_point(|&c| c <= pos)
    }

    fn at(&self, pos: f64) -> P {
        let pos = self.wrap(pos);
        let j = self.seg(pos);
        let l = self.cum[j + 1] - self.cum[j];
        let b = self.pts[(j + 1) % self.pts.len()];
        if l > 0.0 { lerp(self.pts[j], b, (pos - self.cum[j]) / l) } else { self.pts[j] }
    }

    /// The ring points after `from` going `side`, each with its arc distance from `from`, once
    /// round: forward back to the start point itself, backward not.
    fn verts(&self, from: f64, side: usize) -> impl Iterator<Item = (P, f64)> + '_ {
        let n = self.pts.len();
        let from = self.wrap(from);
        let j = self.seg(from);
        (0..n).filter_map(move |k| {
            if side == FWD {
                let i = (j + 1 + k) % n;
                let d = (self.cum[i] - from).rem_euclid(self.len);
                Some((self.pts[i], if d == 0.0 { self.len } else { d }))
            } else {
                let i = (j + n - k) % n;
                let d = (from - self.cum[i]).rem_euclid(self.len);
                (d > 0.0).then_some((self.pts[i], d))
            }
        })
    }

    /// The ring from `from` going `side` for `d`: the ring points on the way, then the point at
    /// `d`. Nothing for no length.
    fn walk(&self, from: f64, side: usize, d: f64) -> Vec<P> {
        if d <= 0.0 {
            return Vec::new();
        }
        let mut out: Vec<P> = self.verts(from, side).take_while(|v| v.1 < d).map(|v| v.0).collect();
        out.push(self.at(if side == FWD { from + d } else { from - d }));
        out
    }
}

/// One end of a line.
struct End {
    ring: usize,
    /// Its stored arc position on its ring.
    pos: f64,
    /// Where it is, and its arc position: moved by step 4, the stored one not.
    at: P,
    at_pos: f64,
    used: bool,
    free: [f64; 2],
    masked: [bool; 2],
}

/// A polyline joined from lines, with the ends at its first and last points.
struct Chain {
    pts: VecDeque<P>,
    first: usize,
    last: usize,
    slot: usize,
    lines: Vec<usize>,
}

impl Chain {
    fn turned(mut self) -> Chain {
        let pts: VecDeque<P> = self.pts.into_iter().rev().collect();
        self.pts = pts;
        std::mem::swap(&mut self.first, &mut self.last);
        self
    }
}

/// One piece being filled.
struct Piece {
    l: f64,
    h: f64,
    s: f64,
    /// The frozen L of steps 6, 9 and 10.
    lf: f64,
    rings: Vec<Ring>,
    /// Per line, its x and its lower and upper ends' points.
    lines: Vec<(f64, P, P)>,
    /// End `2 i` is line i's lower end, `2 i + 1` its upper end: end-point order.
    ends: Vec<End>,
    /// Each end's place in its ring's `ends`.
    rank: Vec<usize>,
    chains: Vec<Option<Chain>>,
    chain_of: Vec<usize>,
    out: Vec<Vec<P>>,
    frame_back: Frame,
}

/// One crossing of a grid line with a ring: its y, whether the ring runs towards +x there (an
/// in), and its ring and arc position.
struct Crossing {
    y: f64,
    into: bool,
    ring: usize,
    pos: f64,
}

/// The crossings of `ring` with the vertical line x = `x` (§4.4, set-up): one inside a segment
/// at its exact y; a point on the line once when the ring passes to the other side and not when
/// it returns; a stretch along the line once (the lower point for an in, the higher for an out)
/// when the ring passes over, and as an in and an out when it returns.
fn crossings(r: &Ring, ri: usize, x: f64, out: &mut Vec<Crossing>) {
    let n = r.pts.len();
    let side = |i: usize| {
        let px = r.pts[i % n].0;
        if px < x { -1 } else if px > x { 1 } else { 0 }
    };
    let Some(start) = (0..n).find(|&i| side(i) != 0) else { return };
    let mut k = 0;
    while k < n {
        let i = (start + k) % n;
        let si = side(i);
        // The points on the line after i, then the next one off it.
        let mut m = 1;
        while m < n && side(i + m) == 0 {
            m += 1;
        }
        let j = (i + m) % n;
        let sj = side(j);
        if m == 1 {
            if si != sj {
                let (a, b) = (r.pts[i], r.pts[j]);
                let t = (x - a.0) / (b.0 - a.0);
                let l = r.cum[i + 1] - r.cum[i];
                out.push(Crossing { y: a.1 + (b.1 - a.1) * t, into: si < sj, ring: ri, pos: r.cum[i] + l * t });
            }
        } else {
            let run: Vec<usize> = (1..m).map(|d| (i + d) % n).collect();
            let lo = run.iter().copied().min_by(|&a, &b| r.pts[a].1.total_cmp(&r.pts[b].1)).unwrap_or(j);
            let hi = run.iter().copied().max_by(|&a, &b| r.pts[a].1.total_cmp(&r.pts[b].1)).unwrap_or(j);
            let at = |v: usize, into: bool| Crossing { y: r.pts[v].1, into, ring: ri, pos: r.cum[v] };
            if si != sj {
                let into = si < sj;
                out.push(at(if into { lo } else { hi }, into));
            } else if run.len() > 1 {
                out.push(at(lo, true));
                out.push(at(hi, false));
            }
        }
        k += m;
    }
}

impl Piece {
    fn new(piece: &ExPolygon, f: &Fill, frame: &Frame) -> Option<Piece> {
        let (l, s) = (f.spacing * SCALE, f.flow * SCALE);
        // The inset reaches the offset in whole nm, truncated (§1.5): 188 539 nm at R14's flow.
        let h = ((s / 2.0) as f32 as f64).trunc();
        let turned = frame.to_ex(piece);
        let shrunk = offset(std::slice::from_ref(&turned), -h, Join::Miter(3.0));
        let mut rings = Vec::new();
        for e in &shrunk {
            for (k, r) in e.rings().enumerate() {
                let mut pts: Vec<P> = r.pts.iter().map(|p| (p.x as f64, p.y as f64)).collect();
                if (k == 0) != r.is_ccw() {
                    pts.reverse();
                }
                if pts.len() >= 2 {
                    rings.push(Ring::new(pts));
                }
            }
        }
        // The grid: one line through the anchor, or the outer ring's box centre (in the part's
        // frame, then turned), the others L apart, truncated to nm, within the turned piece's x
        // range.
        let x0 = frame.to(f.anchor.unwrap_or_else(|| piece.contour.bbox().center())).x as f64;
        let step = l.trunc();
        let bb = turned.contour.bbox();
        let (k0, k1) = (((bb.min.x as f64 - x0) / step).ceil() as i64, ((bb.max.x as f64 - x0) / step).floor() as i64);
        let mut lines = Vec::new();
        let mut ends = Vec::new();
        for k in k0..=k1 {
            let x = x0 + k as f64 * step;
            let mut cs = Vec::new();
            for (ri, r) in rings.iter().enumerate() {
                crossings(r, ri, x, &mut cs);
            }
            cs.sort_by(|a, b| a.y.total_cmp(&b.y));
            if cs.len() % 2 == 1 || cs.chunks(2).any(|c| !c[0].into || c[1].into) {
                // The reference's fill gives up on the whole piece.
                return None;
            }
            for c in cs.chunks(2) {
                let lo = (x.round(), c[0].y.round());
                let hi = (x.round(), c[1].y.round());
                lines.push((x, lo, hi));
                for (p, cr) in [(lo, &c[0]), (hi, &c[1])] {
                    ends.push(End { ring: cr.ring, pos: cr.pos, at: p, at_pos: cr.pos, used: false, free: [0.0; 2], masked: [false; 2] });
                }
            }
        }
        for (i, e) in ends.iter().enumerate() {
            rings[e.ring].ends.push(i);
        }
        let mut rank = vec![0; ends.len()];
        for r in &mut rings {
            r.ends.sort_by(|&a, &b| ends[a].pos.total_cmp(&ends[b].pos).then(a.cmp(&b)));
            for (k, &e) in r.ends.iter().enumerate() {
                rank[e] = k;
            }
        }
        let chains = lines.iter().enumerate().map(|(i, &(_, lo, hi))| Some(Chain { pts: VecDeque::from([lo, hi]), first: 2 * i, last: 2 * i + 1, slot: i, lines: vec![i] })).collect();
        let mut p = Piece { l, h, s, lf: f.frozen * SCALE, rings, chain_of: (0..lines.len()).collect(), lines, ends, rank, chains, out: Vec::new(), frame_back: *frame };
        for e in 0..p.ends.len() {
            for side in [BACK, FWD] {
                p.ends[e].free[side] = p.arc(e, side);
            }
        }
        Some(p)
    }

    fn ring_of(&self, e: usize) -> &Ring {
        &self.rings[self.ends[e].ring]
    }

    /// The neighbouring end of `e` on `side`: itself when it is alone on its ring.
    fn nb(&self, e: usize, side: usize) -> usize {
        let r = self.ring_of(e);
        let n = r.ends.len();
        r.ends[if side == FWD { (self.rank[e] + 1) % n } else { (self.rank[e] + n - 1) % n }]
    }

    fn alone(&self, e: usize) -> bool {
        self.ring_of(e).ends.len() == 1
    }

    fn other(e: usize) -> usize {
        e ^ 1
    }

    fn x(&self, e: usize) -> f64 {
        self.lines[e / 2].0
    }

    /// The arc from `e` to its neighbour on `side`, from their stored positions.
    fn arc(&self, e: usize, side: usize) -> f64 {
        let q = self.nb(e, side);
        let r = self.ring_of(e);
        if q == e {
            return r.len;
        }
        let (a, b) = (self.ends[e].pos, self.ends[q].pos);
        if side == FWD { (b - a).rem_euclid(r.len) } else { (a - b).rem_euclid(r.len) }
    }

    /// The same arc from where the two ends are now.
    fn arc_now(&self, e: usize, side: usize) -> f64 {
        let q = self.nb(e, side);
        let r = self.ring_of(e);
        if q == e {
            return r.len;
        }
        let (a, b) = (self.ends[e].at_pos, self.ends[q].at_pos);
        if side == FWD { (b - a).rem_euclid(r.len) } else { (a - b).rem_euclid(r.len) }
    }

    fn can_take(&self, e: usize, side: usize) -> bool {
        !self.ends[e].used && self.ends[e].free[side] > EPS
    }

    /// Cuts the free length of `e`'s `side` to at most `to`, and masks it.
    fn cut(&mut self, e: usize, side: usize, to: f64) {
        let end = &mut self.ends[e];
        end.free[side] = end.free[side].min(to.max(0.0));
        end.masked[side] = true;
    }

    fn chain(&self, e: usize) -> usize {
        self.chain_of[e / 2]
    }

    fn run(mut self) -> Vec<Vec<Pt>> {
        self.mask();
        self.free_rings();
        self.band_pieces();
        self.extend();
        self.same_line();
        let costs: Vec<[f64; 2]> = (0..self.ends.len()).map(|e| [self.cost(e, BACK), self.cost(e, FWD)]).collect();
        self.long_arcs(&costs);
        self.zig_zag();
        self.caps();
        self.t_joints(&costs);
        self.leftovers(&costs);
        let mut chains: Vec<Chain> = std::mem::take(&mut self.chains).into_iter().flatten().collect();
        chains.sort_by_key(|c| c.slot);
        let mut out = std::mem::take(&mut self.out);
        out.extend(chains.into_iter().map(|c| c.pts.into_iter().collect()));
        out.into_iter().map(|pl| pl.into_iter().map(|p| self.frame_back.back(p)).collect()).collect()
    }

    /// Step 1: masking. (a) A strip round each line's middle masks the ring stretches inside
    /// it; (b) a free stretch that runs along its own line is used up.
    fn mask(&mut self) {
        let r = STRIP * self.s;
        let cut = MIDDLE_CUT * self.s;
        let mut stretches: Vec<Vec<(f64, f64)>> = vec![Vec::new(); self.rings.len()];
        for &(x, lo, hi) in &self.lines {
            if hi.1 - lo.1 <= 2.0 * cut {
                continue;
            }
            let (c0, c1) = ((x, lo.1 + cut), (x, hi.1 - cut));
            for (ri, ring) in self.rings.iter().enumerate() {
                if ring.ends.is_empty() {
                    continue;
                }
                let n = ring.pts.len();
                for j in 0..n {
                    let (a, b) = (ring.pts[j], ring.pts[(j + 1) % n]);
                    if let Some((t0, t1)) = in_capsule(a, b, c0, c1, r) {
                        let l = ring.cum[j + 1] - ring.cum[j];
                        stretches[ri].push((ring.cum[j] + l * t0, ring.cum[j] + l * t1));
                    }
                }
            }
        }
        for (ri, mut st) in stretches.into_iter().enumerate() {
            if st.is_empty() {
                continue;
            }
            let len = self.rings[ri].len;
            st.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut merged: Vec<(f64, f64)> = Vec::new();
            for (a, b) in st {
                match merged.last_mut() {
                    Some(m) if a <= m.1 => m.1 = m.1.max(b),
                    _ => merged.push((a, b)),
                }
            }
            if merged.len() > 1 && merged[0].0 <= 0.0 && merged[merged.len() - 1].1 >= len {
                let first = merged.remove(0);
                let last = merged.len() - 1;
                merged[last].1 = first.1 + len;
            }
            for (a, b) in merged {
                self.mask_stretch(ri, a, b);
            }
        }
        let ends = self.ends.len();
        for e in 0..ends {
            for side in [FWD, BACK] {
                if !self.can_take(e, side) {
                    self.ends[e].free[side] = 0.0;
                    continue;
                }
                if self.runs_along(e, side) {
                    let was = self.ends[e].masked[side];
                    self.cut(e, side, 0.0);
                    if !was {
                        let q = self.nb(e, side);
                        self.cut(q, 1 - side, 0.0);
                    }
                }
            }
        }
    }

    /// The ring stretch from arc position `a` to `b` (`b` may pass the ring's end) is masked.
    fn mask_stretch(&mut self, ri: usize, a: f64, b: f64) {
        let ring = &self.rings[ri];
        let len = ring.len;
        let ids = ring.ends.clone();
        let inside = |pos: f64| {
            let p = if pos < a { pos + len } else { pos };
            p > a && p < b
        };
        let mut before: Option<(f64, usize)> = None;
        let mut after: Option<(f64, usize)> = None;
        for &e in &ids {
            let pos = self.ends[e].pos;
            if inside(pos) || b - a >= len {
                let end = &mut self.ends[e];
                end.used = true;
                end.free = [0.0; 2];
                end.masked = [true; 2];
                continue;
            }
            let db = (a - pos).rem_euclid(len);
            if before.is_none_or(|x| db < x.0) {
                before = Some((db, e));
            }
            let da = (pos - b).rem_euclid(len);
            if after.is_none_or(|x| da < x.0) {
                after = Some((da, e));
            }
        }
        if let Some((d, e)) = before {
            self.cut(e, FWD, d);
        }
        if let Some((d, e)) = after {
            self.cut(e, BACK, d);
        }
    }

    /// Whether the free stretch of `e`'s `side` stays within h + 50 nm of its own line: at the
    /// ring points along it and where it ends.
    fn runs_along(&self, e: usize, side: usize) -> bool {
        let (_, lo, hi) = self.lines[e / 2];
        let end = &self.ends[e];
        let tol = self.h + ALONG;
        self.ring_of(e).walk(end.at_pos, side, end.free[side]).iter().all(|&p| to_segment(p, lo, hi) <= tol)
    }

    /// Step 2: a ring with no end on it is put out as a loop when it is longer than 0.3 h + L/2,
    /// from its first point forward, 0.3 h short of closing.
    fn free_rings(&mut self) {
        let short = HOOK_SHORT * self.h;
        for r in &self.rings {
            if r.ends.is_empty() && r.len > short + self.l / 2.0 {
                let mut pl = vec![r.pts[0]];
                pl.extend(r.walk(0.0, FWD, r.len - short));
                self.out.push(pl);
            }
        }
    }

    /// Step 3: between an end whose forward side is masked and the next end, whose back side is
    /// masked, the masked middle of the arc gives pieces inside the band of line-frame x next to
    /// the end's line.
    fn band_pieces(&mut self) {
        for e in 0..self.ends.len() {
            let q = self.nb(e, FWD);
            if !self.ends[e].masked[FWD] || !self.ends[q].masked[BACK] {
                continue;
            }
            let (fe, fq) = (self.ends[e].free[FWD], self.ends[q].free[BACK]);
            let middle = self.arc(e, FWD) - fe - fq;
            if middle <= 0.0 {
                continue;
            }
            let x = self.x(e);
            let (h, l) = (self.h, self.l);
            let band = if e % 2 == 0 { (x + h, x + l - h) } else { (x - l + h, x - h) };
            let r = self.ring_of(e);
            let from = self.ends[e].pos + fe;
            let mut pts = vec![r.at(from)];
            pts.extend(r.verts(from, FWD).take_while(|v| v.1 < middle).map(|v| v.0));
            pts.push(closing_quirk(r, from + middle));
            let pieces = band_walk(&pts, band);
            for piece in pieces {
                match self.out.last_mut() {
                    Some(last) if last.last().is_some_and(|&p| dist(p, piece[0]) <= JOIN_GAP) => last.extend(piece),
                    _ if length(&piece) > l / 2.0 => self.out.push(piece),
                    _ => {}
                }
            }
        }
    }

    /// Step 4: each end, used or not, may run on along the ring while the ring stays within
    /// 0.33 L of its line's x, if that reaches more than L/2 beyond it along the line.
    fn extend(&mut self) {
        let near = (0.33 * self.l).trunc();
        let reach_min = (self.l / 2.0).trunc();
        for e in 0..self.ends.len() {
            let x = self.x(e);
            let upper = e % 2 == 1;
            let mut best: Option<(usize, Vec<P>, f64)> = None;
            // §4.4 step 4 quirk: when both sides qualify the back side wins.
            for side in [BACK, FWD] {
                let q = self.nb(e, side);
                if self.x(q) == x {
                    continue;
                }
                let arc = self.arc(e, side);
                let end = &self.ends[e];
                let mut walked: Vec<P> = Vec::new();
                let mut d = 0.0;
                for (p, dv) in self.ring_of(e).verts(end.at_pos, side) {
                    if dv >= arc || (p.0 - x).abs() > near {
                        break;
                    }
                    walked.push(p);
                    d = dv;
                }
                let Some(&cand) = walked.last() else { continue };
                let reach = if upper { cand.1 - end.at.1 } else { end.at.1 - cand.1 };
                if reach > reach_min && d < end.free[side] && best.is_none() {
                    best = Some((side, walked, d));
                }
            }
            let Some((side, walked, d)) = best else { continue };
            let ci = self.chain(e);
            let chain = self.chains[ci].as_mut().expect("a line's chain");
            for &p in &walked {
                if upper { chain.pts.push_back(p) } else { chain.pts.push_front(p) }
            }
            let len = self.ring_of(e).len;
            let end = &mut self.ends[e];
            // §4.4 step 4 quirk: the end moves but its stored position stays; arcs still measure from it.
            end.at = walked[walked.len() - 1];
            end.at_pos = if side == FWD { end.at_pos + d } else { end.at_pos - d }.rem_euclid(len);
            let q = self.nb(e, side);
            if self.ends[e].masked[side] {
                self.ends[e].free[side] -= d;
            } else {
                let rest = self.arc_now(e, side);
                self.ends[e].free[side] = rest;
                self.ends[q].free[1 - side] = rest;
            }
            let other = 1 - side;
            self.ends[e].free[other] = 0.0;
            self.ends[e].masked[other] = true;
            let qo = self.nb(e, other);
            self.ends[qo].masked[side] = true;
        }
    }

    /// Step 5: an end whose neighbour lies on the same grid line (another line split off by a
    /// hole or a notch) is joined to it.
    fn same_line(&mut self) {
        for e in 0..self.ends.len() {
            if self.ends[e].used {
                continue;
            }
            let cand: Vec<usize> = [BACK, FWD]
                .into_iter()
                .filter(|&side| {
                    let q = self.nb(e, side);
                    q != e && self.can_take(e, side) && self.x(q) == self.x(e) && !self.ends[q].used && q != Self::other(e)
                })
                .collect();
            let side = match cand[..] {
                [] => continue,
                [one] => one,
                _ => {
                    let end = &self.ends[e];
                    let (mb, mf) = (end.masked[BACK], end.masked[FWD]);
                    let back = if mb == mf { end.free[BACK] > end.free[FWD] } else { !mb };
                    if back { BACK } else { FWD }
                }
            };
            if !self.ends[e].masked[side] || self.ends[e].free[side] > 1.3 * self.l {
                self.take(e, side);
            }
        }
    }

    /// A side's cost (step 6): how far its arc strays from the straight segment between the
    /// arc's ends, or past their y range. Its whole arc unmasked, its free length masked.
    fn cost(&self, e: usize, side: usize) -> f64 {
        let end = &self.ends[e];
        if end.free[side] <= EPS {
            return 0.0;
        }
        let d = if end.masked[side] { end.free[side] } else { self.arc_now(e, side) };
        let mut pts = vec![end.at];
        pts.extend(self.ring_of(e).walk(end.at_pos, side, d));
        let (a, b) = (pts[0], pts[pts.len() - 1]);
        let (ylo, yhi) = (a.1.min(b.1), a.1.max(b.1));
        pts.iter().map(|&p| to_segment(p, a, b).max(ylo - p.1).max(p.1 - yhi)).fold(0.0, f64::max)
    }

    /// Step 6: an end with one long arc and one much shorter takes its long arcs, the costliest
    /// first.
    fn long_arcs(&mut self, costs: &[[f64; 2]]) {
        // §4.4 steps 6, 9 and 10 quirk: the limits are frozen at the first fill call's L.
        let lf = self.lf;
        let mut list = Vec::new();
        for (e, c) in costs.iter().enumerate() {
            if self.ends[e].used {
                continue;
            }
            let (a, b) = (c[0].min(c[1]), c[0].max(c[1]));
            if b < 1.3 * lf || a > 2.0 * lf || b - a < 0.25 * b {
                continue;
            }
            for side in [BACK, FWD] {
                if c[side] > 1.3 * lf {
                    list.push((c[side], e, side));
                }
            }
        }
        // Ties in end-point order, back side first: the list is built so and the sort is stable.
        list.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, e, side) in list {
            if !self.ends[e].used {
                self.take(e, side);
            }
        }
    }

    /// Step 7: each unused lower end takes its forward side, each unused upper end its back
    /// side, between different chains.
    fn zig_zag(&mut self) {
        for e in 0..self.ends.len() {
            if self.ends[e].used {
                continue;
            }
            let side = if e % 2 == 0 { FWD } else { BACK };
            if self.chain(e) != self.chain(self.nb(e, side)) {
                self.take(e, side);
            }
        }
    }

    /// Step 8: a side facing its own line's other end is hooked towards it.
    fn caps(&mut self) {
        for e in 0..self.ends.len() {
            for side in [BACK, FWD] {
                if self.nb(e, side) == Self::other(e) && self.can_take(e, side) {
                    self.take(e, side);
                }
            }
        }
    }

    /// Step 9: every side still free is taken, the costliest first, between different chains
    /// or when it costs over 2 L.
    fn t_joints(&mut self, costs: &[[f64; 2]]) {
        let mut list = Vec::new();
        for (e, c) in costs.iter().enumerate() {
            for side in [BACK, FWD] {
                if self.can_take(e, side) {
                    list.push((c[side], e, side));
                }
            }
        }
        list.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (cost, e, side) in list {
            if self.can_take(e, side) && (self.chain(e) != self.chain(self.nb(e, side)) || cost > 2.0 * self.lf) {
                self.take(e, side);
            }
        }
    }

    /// Step 10: a costly free stretch left over is put out on its own, h from its end.
    fn leftovers(&mut self, costs: &[[f64; 2]]) {
        let h = self.h;
        for e in 0..self.ends.len() {
            for side in [BACK, FWD] {
                let q = self.nb(e, side);
                let limit = if q == Self::other(e) { self.l / 2.0 } else { 3.0 * self.lf };
                if self.ends[e].free[side] <= EPS || costs[e][side] <= limit {
                    continue;
                }
                if !self.ends[e].masked[side] {
                    self.ends[e].free[side] -= h;
                    self.ends[q].free[1 - side] = 0.0;
                }
                let free = self.ends[e].free[side];
                if free > EPS && free > h {
                    let r = self.ring_of(e);
                    let from = if side == FWD { self.ends[e].at_pos + h } else { self.ends[e].at_pos - h };
                    let mut pl = vec![r.at(from)];
                    pl.extend(r.walk(from, side, free - h));
                    self.out.push(pl);
                }
                self.ends[e].free[side] = 0.0;
            }
        }
    }

    /// Takes `e`'s `side` towards its neighbour: a join when nothing stands in the way, else a
    /// hook that grows only `e`'s chain.
    fn take(&mut self, e: usize, side: usize) {
        if self.ends[e].used {
            return;
        }
        let q = self.nb(e, side);
        let facing = 1 - side;
        let arc = self.arc(e, side);
        let join = !self.ends[e].masked[side] && !self.ends[q].used && self.chain(e) != self.chain(q) && q != Self::other(e) && !self.alone(e);
        if join {
            let (p1, p2) = if side == FWD { (e, q) } else { (q, e) };
            let r = self.ring_of(p1);
            let d = self.arc_now(p1, FWD);
            let mut arc_pts = r.walk(self.ends[p1].at_pos, FWD, d);
            arc_pts.pop();
            let (ca, cb) = (self.chain(p1), self.chain(p2));
            let a = self.chains[ca].take().expect("a live chain");
            let b = self.chains[cb].take().expect("a live chain");
            let a = if a.last == p1 { a } else { a.turned() };
            let b = if b.first == p2 { b } else { b.turned() };
            let slot = a.slot.min(b.slot);
            let mut pts = a.pts;
            pts.extend(arc_pts);
            pts.extend(b.pts);
            let mut lines = a.lines;
            lines.extend(b.lines);
            let into = if a.slot <= b.slot { ca } else { cb };
            for &line in &lines {
                self.chain_of[line] = into;
            }
            self.chains[into] = Some(Chain { pts, first: a.first, last: b.last, slot, lines });
            for (x, s) in [(e, side), (q, facing)] {
                let end = &mut self.ends[x];
                end.used = true;
                end.free[s] = 0.0;
            }
            return;
        }
        if !self.ends[e].masked[side] {
            self.cut(e, side, arc - HOOK_SHORT * self.h - EPS);
            self.cut(q, facing, 0.0);
        }
        let len = self.ends[e].free[side].min(arc - self.h);
        if len >= EPS {
            let pts = self.ring_of(e).walk(self.ends[e].at_pos, side, len);
            let ci = self.chain(e);
            let chain = self.chains[ci].as_mut().expect("a live chain");
            if chain.last == e {
                chain.pts.extend(pts);
            } else {
                for p in pts {
                    chain.pts.push_front(p);
                }
            }
        }
        self.ends[e].used = true;
        self.ends[e].free[side] = 0.0;
        self.cut(q, facing, arc - len.max(0.0));
    }
}

/// The parameter interval of segment `a`-`b` within `r` of segment `c0`-`c1` (vertical): the
/// round-ended strip, as one interval since the strip is convex.
fn in_capsule(a: P, b: P, c0: P, c1: P, r: f64) -> Option<(f64, f64)> {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    let mut add = |iv: Option<(f64, f64)>| {
        if let Some((t0, t1)) = iv {
            lo = lo.min(t0);
            hi = hi.max(t1);
        }
    };
    add(clip_box(a, b, (c0.0 - r, c0.1.min(c1.1)), (c0.0 + r, c0.1.max(c1.1))));
    add(in_disc(a, b, c0, r));
    add(in_disc(a, b, c1, r));
    (lo <= hi).then_some((lo, hi))
}

/// The parameter interval of segment `a`-`b` inside the box `lo`-`hi`.
fn clip_box(a: P, b: P, lo: P, hi: P) -> Option<(f64, f64)> {
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, d, mn, mx) in [(a.0, b.0 - a.0, lo.0, hi.0), (a.1, b.1 - a.1, lo.1, hi.1)] {
        if d == 0.0 {
            if p < mn || p > mx {
                return None;
            }
            continue;
        }
        let (u, v) = ((mn - p) / d, (mx - p) / d);
        t0 = t0.max(u.min(v));
        t1 = t1.min(u.max(v));
    }
    (t0 <= t1).then_some((t0, t1))
}

/// The parameter interval of segment `a`-`b` within `r` of `c`.
fn in_disc(a: P, b: P, c: P, r: f64) -> Option<(f64, f64)> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (fx, fy) = (a.0 - c.0, a.1 - c.1);
    let qa = dx * dx + dy * dy;
    let qb = 2.0 * (fx * dx + fy * dy);
    let qc = fx * fx + fy * fy - r * r;
    if qa == 0.0 {
        return (qc <= 0.0).then_some((0.0, 1.0));
    }
    let disc = qb * qb - 4.0 * qa * qc;
    if disc < 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    let (t0, t1) = (((-qb - sq) / (2.0 * qa)).max(0.0), ((-qb + sq) / (2.0 * qa)).min(1.0));
    (t0 <= t1).then_some((t0, t1))
}

/// The point at arc position `pos` on `r`, as the reference places a band walk's end: on the
/// closing segment (last point back to the first) at the fraction pos ÷ length from the first
/// point (§4.4 step 3 quirk).
fn closing_quirk(r: &Ring, pos: f64) -> P {
    let pos = r.wrap(pos);
    let n = r.pts.len();
    if r.seg(pos) == n - 1 {
        lerp(r.pts[0], r.pts[n - 1], pos / r.len)
    } else {
        r.at(pos)
    }
}

/// Step 3's pieces of the walk `pts` inside the band `band` of line-frame x. An excursion out
/// and back in through the left edge below the exit, or the right edge at or above it, stays in
/// the piece without its exit and re-entry points; any other re-entry ends the piece at its last
/// point before the exit and starts a new one. At the walk's end a piece out of the band ends at
/// its last inside point, and one still inside is dropped.
fn band_walk(pts: &[P], band: (f64, f64)) -> Vec<Vec<P>> {
    enum At {
        Out,
        In,
        Away { exit_y: f64, outside: Vec<P> },
    }
    let inside = |p: P| p.0 >= band.0 && p.0 <= band.1;
    let mut out = Vec::new();
    let mut piece: Vec<P> = Vec::new();
    let mut at = if inside(pts[0]) {
        piece.push(pts[0]);
        At::In
    } else {
        At::Out
    };
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        // The band's edges this segment passes, in order along it: (point, through the left).
        let edge = |e: f64| lerp(a, b, (e - a.0) / (b.0 - a.0));
        let mut cross: Vec<(P, bool)> = Vec::new();
        match (inside(a), inside(b)) {
            (true, false) => cross.push((edge(if b.0 < band.0 { band.0 } else { band.1 }), b.0 < band.0)),
            (false, true) => cross.push((edge(if a.0 < band.0 { band.0 } else { band.1 }), a.0 < band.0)),
            (false, false) if (a.0 < band.0 && b.0 > band.1) || (a.0 > band.1 && b.0 < band.0) => {
                let left_first = a.0 < band.0;
                let (e0, e1) = if left_first { (band.0, band.1) } else { (band.1, band.0) };
                cross.push((edge(e0), left_first));
                cross.push((edge(e1), !left_first));
            }
            _ => {}
        }
        for (p, left) in cross {
            at = match std::mem::replace(&mut at, At::Out) {
                At::Out => {
                    piece = vec![p];
                    At::In
                }
                At::In => At::Away { exit_y: p.1, outside: Vec::new() },
                At::Away { exit_y, outside } => {
                    if (left && p.1 < exit_y) || (!left && p.1 >= exit_y) {
                        piece.extend(outside);
                    } else {
                        out.push(std::mem::replace(&mut piece, vec![p]));
                    }
                    At::In
                }
            };
        }
        match &mut at {
            At::In => piece.push(b),
            At::Away { outside, .. } => outside.push(b),
            At::Out => {}
        }
    }
    if matches!(at, At::Away { .. }) {
        out.push(piece);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poly(pts: &[(f64, f64)]) -> Polygon {
        Polygon::new(pts.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect())
    }

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon {
        poly(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)])
    }

    fn mm(pl: &[Pt]) -> Vec<(f64, f64)> {
        pl.iter().map(|p| ((p.x_mm() * 1e4).round() / 1e4, (p.y_mm() * 1e4).round() / 1e4)).collect()
    }

    /// No-roof lines in R14's profile: 1.377 mm apart, flow spacing 0.377, limits frozen at the
    /// roof's 0.877.
    fn bare(angle_deg: f64) -> Fill {
        Fill { angle_deg, spacing: 1.377, flow: 0.377, frozen: 0.877, anchor: None }
    }

    /// §4.4 set-up and steps 7-8: lines at 0° are cut from the piece shrunk by h, one through the
    /// box's centre; the line frame turns by −90°, so slots run up the bed and a line's lower end
    /// is its right end. A plain convex piece with an odd count of lines is one zig-zag from the
    /// last line's lower end, and the caps hook each free end towards its own line's other end,
    /// h short of it: the first in front of the chain, the last after it.
    #[test]
    fn a_convex_piece_is_one_zig_zag_capped_at_both_ends() {
        // Lines at y = 0.6885 … 6.1965, each 0.5 mm from the shrunk outline: nothing is masked
        // and no end runs on (0.5 is over 0.33 L).
        let piece = ExPolygon::new(rect(0.0, 0.0, 10.0, 6.885));
        let out = fill(&[piece], &bare(0.0));
        assert_eq!(out.len(), 1, "one chain: {:?}", out.iter().map(|p| mm(p)).collect::<Vec<_>>());
        let (l, r) = (0.1885, 9.8115);
        let want = vec![
            (l, 6.385), (l, 6.6965), (r, 6.6965),
            (r, 6.1965), (l, 6.1965), (l, 4.8195), (r, 4.8195), (r, 3.4425), (l, 3.4425), (l, 2.0655), (r, 2.0655), (r, 0.6885), (l, 0.6885),
            (l, 0.1885), (r, 0.1885), (r, 0.5),
        ];
        assert_eq!(mm(&out[0]), want, "from the top line's cap round to h short of the bottom line's right end");
    }

    /// §4.4 step 2: a ring no line reaches is put out first, as a loop from its first point in
    /// its stored direction (clockwise for a hole), 0.3 h short of closing.
    #[test]
    fn a_ring_with_no_ends_is_a_loop() {
        // Lines (vertical at −90°) at x = 0.6885, 2.0655, 3.4425; the hole, grown by h, lies
        // between the first two.
        let piece = ExPolygon { contour: rect(0.0, 0.0, 4.131, 10.0), holes: vec![poly(&[(1.1, 4.0), (1.1, 6.0), (1.6, 6.0), (1.6, 4.0)])] };
        let out = fill(&[piece], &Fill { angle_deg: -90.0, ..bare(0.0) });
        assert_eq!(out.len(), 2, "the loop and one chain");
        let ring = Polygon::new(out[0].clone());
        assert!(!ring.is_ccw(), "a hole's loop runs clockwise");
        let len: f64 = out[0].windows(2).map(|w| w[0].dist(w[1])).sum::<f64>() / SCALE;
        assert!((len - (2.0 * (0.877 + 2.377) - 0.3 * 0.1885)).abs() < 1e-4, "the grown hole's ring less 0.3 h: {len}");
        let b = ring.bbox();
        assert!((b.min.x_mm() - 0.9115).abs() < 1e-4 && (b.max.y_mm() - 6.1885).abs() < 1e-4, "the hole grown by h");
    }

    /// §4.4 step 5: two lines on one grid line, split by a hole, are joined round the hole on
    /// the side whose free length is strictly longer when neither is masked: here the right.
    #[test]
    fn lines_split_by_a_hole_join_round_it() {
        let piece = ExPolygon { contour: rect(0.0, 0.0, 4.131, 10.0), holes: vec![poly(&[(1.6, 4.0), (1.6, 6.0), (2.6, 6.0), (2.6, 4.0)])] };
        let out = fill(&[piece], &Fill { angle_deg: -90.0, ..bare(0.0) });
        let pts: Vec<Vec<(f64, f64)>> = out.iter().map(|p| mm(p)).collect();
        let joined = pts.iter().any(|pl| pl.windows(4).any(|w| w == [(2.0655, 3.8115), (2.7885, 3.8115), (2.7885, 6.1885), (2.0655, 6.1885)] || w == [(2.0655, 6.1885), (2.7885, 6.1885), (2.7885, 3.8115), (2.0655, 3.8115)]));
        assert!(joined, "the middle grid line's two lines joined round the hole's right side: {pts:?}");
    }

    /// §4.4 step 1: a ring stretch inside the 0.8 s strip round a line's middle is masked, and
    /// a free stretch within h + 50 nm of its own line is used up.
    #[test]
    fn a_ring_alongside_a_line_is_masked() {
        // Lines at x = 0.2885, 1.6655, 3.0425: the shrunk outline's left side, at 0.1885, is
        // 0.1 mm left of the first.
        let piece = ExPolygon::new(rect(0.0, 0.0, 3.331, 10.0));
        let f = Fill { angle_deg: -90.0, ..bare(0.0) };
        let mut p = Piece::new(&piece, &f, &Frame::new(f.angle_deg)).expect("lines pair up");
        assert!((p.lines[0].0 / SCALE - 0.2885).abs() < 1e-6, "the first line at 0.2885: {}", p.lines[0].0 / SCALE);
        p.mask();
        let (lo, hi) = (&p.ends[0], &p.ends[1]);
        assert!(lo.masked[BACK] && hi.masked[FWD], "the left side is masked from both of the line's ends");
        assert_eq!((lo.free[BACK], hi.free[FWD]), (0.0, 0.0), "and what is left of it runs along the line: used up");
        assert!(!lo.masked[FWD] && !hi.masked[BACK], "the sides towards the next line stay free");
    }

    /// §4.4 step 4 and its two quirks. An end on a ridge whose ring falls away on both sides,
    /// within 0.33 L of its x and more than L/2 down, runs on along its back side, though both
    /// qualify. Its stored position stays, so a later hook from it on that side runs all the way
    /// to the neighbouring end, with no h gap.
    #[test]
    fn an_end_runs_on_along_its_back_side() {
        // Lines at x = −1.377, 0, 1.377; a W in the bottom with its ridge on the middle line,
        // valleys 0.3 mm either side and 1 mm down. h is 1 µm.
        let piece = ExPolygon::new(poly(&[(-2.0, 2.0), (-2.0, 0.0), (-0.6, 0.0), (-0.3, -1.0), (0.0, 0.0), (0.3, -1.0), (0.6, 0.0), (2.0, 0.0), (2.0, 2.0)]));
        let f = Fill { angle_deg: -90.0, spacing: 1.377, flow: 0.002, frozen: 0.877, anchor: None };
        let mut p = Piece::new(&piece, &f, &Frame::new(f.angle_deg)).expect("lines pair up");
        assert_eq!(p.lines.len(), 3);
        p.mask();
        p.free_rings();
        p.band_pieces();
        p.extend();
        let mid = p.chains[1].as_ref().expect("the middle line");
        let front = mid.pts[0];
        assert!((front.0 / SCALE + 0.3).abs() < 0.01 && front.1 / SCALE < -0.99, "the middle line's lower end ran on to the left (back) valley: {:?}", mid.pts);
        assert!(p.ends[2].free[FWD] == 0.0 && p.ends[2].masked[FWD], "its other side is used up");
        // A hook from it on its back side, towards the left line's lower end (made unusable).
        p.ends[0].used = true;
        p.take(2, BACK);
        let mid = p.chains[1].as_ref().expect("the middle line");
        let reached = mid.pts[0];
        let target = p.ends[0].at;
        assert!(dist(reached, target) < 1.0, "the hook reaches the left line's lower end itself, no h short: {} nm off", dist(reached, target));
    }
}

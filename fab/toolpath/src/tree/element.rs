//! An element (R14 §1.3, §4.6, §5): one branch's state and influence area on one layer.

use makepad_fab_slicer::geom::*;

use super::clip::{as_simplified, union, FillRule};
use super::settings::Settings;
use super::volumes::Kind;

/// A move one layer down (§5.1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Move {
    /// The avoidance it keeps out of.
    pub kind: Kind,
    /// How far (mm) its area grows.
    pub speed: f64,
    /// Whether the radius may grow on it.
    pub grow: bool,
    /// Whether it may end inside the avoidance.
    pub may_stray: bool,
    /// Whether it keeps xy_min rather than the full side gap.
    pub min_gap: bool,
    /// Whether the area moves at all.
    pub moves: bool,
}

/// One branch on one layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    /// The layer it is on.
    pub layer: usize,
    /// Its next position: where the tip stands, carried down and set by merges.
    pub point: Pt,
    /// Layers below its tip.
    pub depth: usize,
    /// The counter its collision radius follows; it may lag its depth.
    pub ramp: usize,
    /// Foot counts (fractional).
    pub foot: f64,
    /// It gets down to the bed.
    pub to_bed: bool,
    /// It may land on a model top, not only against a wall.
    pub on_top: bool,
    /// It may use the hole-free avoidances.
    pub hole_free: bool,
    /// It keeps xy_min, not the full side gap: every tip starts so when the side gap is more than
    /// xy_min, and switches for good on its first move at the full side gap.
    pub min_gap: bool,
    /// It holds a roof up.
    pub under_roof: bool,
    /// Layers it does not move sideways for ("don't move until").
    pub hold: usize,
    /// Roof layers it makes up itself, as discs.
    pub roof_discs: usize,
    /// Its section stays round.
    pub round: bool,
    /// The highest tip it serves: that tip's layer.
    pub highest_tip: usize,
    /// The layer it is out of Fab's keep-out by ([`super::Input::keep_out`]): on a layer over it,
    /// it is leaving the keep-out and stays clear of the part alone; on it and under it, it keeps
    /// out. A tip's own layer for every tip but one under a face in the keep-out.
    pub exit: usize,
    /// Its influence area.
    pub area: Vec<ExPolygon>,
    /// The move that made it, none for a tip.
    pub last: Option<Move>,
    /// It was made by the move that may stray into the avoidance.
    pub strayed: bool,
    /// No move took it a layer further down.
    pub stuck: bool,
    /// How much radius (mm) it gained by merges with branches to the bed while bound for the
    /// model.
    pub merge_gain: f64,
    /// The widest any element it carries is drawn (mm) ([`Element::width`]).
    pub carried: f64,
    /// Its indices on the layer above: the elements it carries.
    pub carries: Vec<usize>,
    /// Where it stands on its layer: a tip's own point from the start, kept by a move that stays
    /// (§5.2); set in placement (§6).
    pub placed: Option<Pt>,
}

/// The radius (nm) of the base circle R14's discs are made from (§4.7, the disc's shape).
const BASE_RADIUS: i64 = 10_000;
/// The circle maker's largest error from a true circle (nm): the "25" R14 asks it for.
const CIRCLE_ERROR: f64 = 25.0;

/// The circle R14's circle maker draws with radius `r` (nm) about (0, 0) (§4.7, the disc's
/// shape): ⌈2π ÷ 2 acos(1 − 25 ÷ `r`)⌉ corners, 45 at 0.01 mm; corner k at k · 2π/n from +X in
/// doubles, each coordinate truncated to whole nm towards zero.
fn circle(r: i64) -> Vec<Pt> {
    let rf = r as f64;
    let n = (std::f64::consts::TAU / (2.0 * (1.0 - CIRCLE_ERROR / rf).acos())).ceil() as usize;
    (0..n).map(|k| {
        let a = k as f64 * std::f64::consts::TAU / n as f64;
        Pt::new((a.cos() * rf) as i64, (a.sin() * rf) as i64)
    })
    .collect()
}

/// A roof disc (§4.7), contact or interface, of radius `r_tip` (mm) at `p`: the base circle with
/// each corner multiplied by `r_tip` ÷ 0.01 mm, an integer division of the radii in nm (40 in
/// R14's profile), then moved to `p`.
pub fn roof_disc(p: Pt, r_tip: f64) -> ExPolygon {
    base_circle(p, (r_tip * SCALE) as i64 / BASE_RADIUS)
}

/// The base circle with each corner multiplied by `f`, moved to `p`.
fn base_circle(p: Pt, f: i64) -> ExPolygon {
    ExPolygon::new(Polygon::new(circle(BASE_RADIUS).into_iter().map(|q| Pt::new(p.x + q.x * f, p.y + q.y * f)).collect()))
}

impl Element {
    /// A new tip on layer `layer` at `point` (§4.6): both counters and the foot at 0, its area the
    /// base circle (45 corners at 0.01 mm, §4.7) moved to the point, and placed at its point.
    pub fn tip(s: &Settings, layer: usize, point: Pt) -> Element {
        Element {
            layer,
            point,
            depth: 0,
            ramp: 0,
            foot: 0.0,
            to_bed: false,
            on_top: false,
            hole_free: false,
            min_gap: s.keys.xy_distance > s.xy_min,
            under_roof: false,
            hold: 0,
            roof_discs: 0,
            round: false,
            highest_tip: layer,
            exit: layer,
            area: vec![base_circle(point, 1)],
            last: None,
            strayed: false,
            stuck: false,
            merge_gain: 0.0,
            carried: 0.0,
            carries: Vec::new(),
            placed: Some(point),
        }
    }

    /// The radius (mm) routing keeps clear of the part.
    pub fn collision_radius(&self, s: &Settings) -> f64 {
        s.collision_radius(self.ramp, self.foot)
    }

    /// The radius (mm) its counters draw it at (§4.4): by its depth inside the tip's ramp, by its
    /// ramp counter below it.
    pub fn drawn_radius(&self, s: &Settings) -> f64 {
        s.drawn_radius(self.depth, self.ramp, self.foot)
    }

    /// The radius (mm) it is drawn at, Fab's departure from §8.2: its counters' radius, or the
    /// widest of what it carries where that is wider, so a branch never narrows going down. The
    /// reference narrows one where its ramp counter lags its depth (moves that could not grow
    /// it). The merges, the smoothing's anchors and the drawing all go by it, so the joints they
    /// plan are the ones drawn. The smoothing's push goes by the counters' radius, the one routing
    /// kept clear (D24), and the draw cuts off what the width overhangs the part.
    pub fn width(&self, s: &Settings) -> f64 {
        self.drawn_radius(s).max(self.carried)
    }

    /// The rung its collision radius asks volumes for, at its xy kind.
    pub fn rung(&self, s: &Settings) -> usize {
        s.rung_for(self.collision_radius(s), !self.min_gap)
    }

    /// Whether it is still leaving Fab's keep-out: on a layer over its exit.
    pub fn leaving(&self) -> bool {
        self.layer > self.exit
    }
}

/// A disc of radius `r` (mm) round `c`, its edges within 0.015 mm of the circle.
pub fn disc(c: Pt, r: f64) -> ExPolygon {
    let tol = 0.015_f64.min(r / 2.0);
    ngon(c, r, ((std::f64::consts::PI / (1.0 - tol / r).acos()).ceil() as usize).max(8))
}

/// A regular polygon of `n` sides with its corners `r` (mm) from `c`, the first on +X.
pub fn ngon(c: Pt, r: f64, n: usize) -> ExPolygon {
    let pts = (0..n)
        .map(|i| {
            let a = i as f64 * std::f64::consts::TAU / n as f64;
            Pt::new(c.x + (r * a.cos() * SCALE).round() as i64, c.y + (r * a.sin() * SCALE).round() as i64)
        })
        .collect();
    ExPolygon::new(Polygon::new(pts))
}

/// `set` simplified at `tol` (mm) as the reference's per-ring simplify does (§1.6): each ring by
/// [`dp_ring`] from its own first point, the one the operation before left, then cleaned up, which
/// starts it again: an outer ring by Rule A, a hole, cleaned as an outer ring and turned back, by
/// Rule B.
pub fn simplified(set: &[ExPolygon], tol: f64) -> Vec<ExPolygon> {
    if set.is_empty() {
        return Vec::new();
    }
    let rings: Vec<Polygon> = set.iter().flat_map(|e| e.rings()).map(|r| Polygon::new(dp_ring(&r.pts, tol * SCALE))).filter(|r| r.len() >= 3).collect();
    as_simplified(union(&rings, FillRule::NonZero))
}

/// `set` simplified at `tol` (mm) as the reference's island simplify does (§1.6; the slices', §2.0
/// and §2.7): island by island, each ring by [`dp_ring`] from its own first point, then the
/// island's rings in one union into islands, every ring by Rule A. The islands are not united
/// with each other: each island's result follows the one before, as the reference stores them.
/// (The reference's first union is strictly simple; the engine has none, which can only tell where
/// rings touch at a vertex, N16.2.)
pub fn island_simplified(set: &[ExPolygon], tol: f64) -> Vec<ExPolygon> {
    let mut out = Vec::new();
    for e in set {
        let rings: Vec<Polygon> = e.rings().map(|r| Polygon::new(dp_ring(&r.pts, tol * SCALE))).filter(|r| r.len() >= 3).collect();
        out.extend(union(&rings, FillRule::NonZero));
    }
    out
}

/// Douglas–Peucker on the closed ring `pts` at `tol` (units), as the reference's simplify runs it
/// (§1.6). The first point stays. The point farthest from it (the earlier on a tie) stays if it is
/// more than `tol` away; then on each side of it, the point farthest from the segment between the
/// two points kept round it (the distance clamped at the segment's ends) stays if it is more than
/// `tol` away, and so on. There is no size guard: a ring can come back with under 3 points, which
/// the clean-up drops.
pub fn dp_ring(pts: &[Pt], tol: f64) -> Vec<Pt> {
    let n = pts.len();
    let mut keep = vec![false; n];
    let Some(&first) = pts.first() else { return Vec::new() };
    keep[0] = true;
    let (mut far, mut best) = (0, 0i128);
    for (i, p) in pts.iter().enumerate().skip(1) {
        let d = first.dist2(*p);
        if d > best {
            (far, best) = (i, d);
        }
    }
    if far > 0 && (best as f64).sqrt() > tol {
        keep[far] = true;
        dp_mark(&pts[..=far], tol, &mut keep[..=far]);
        let mut back: Vec<Pt> = pts[far..].to_vec();
        back.push(first);
        let mut kept = vec![false; back.len()];
        dp_mark(&back, tol, &mut kept);
        for (k, &on) in kept.iter().enumerate().take(back.len() - 1).skip(1) {
            keep[far + k] |= on;
        }
    }
    pts.iter().zip(&keep).filter(|&(_, &k)| k).map(|(p, _)| *p).collect()
}

/// Douglas–Peucker on the open path `path` at `tol` (units) with both its ends kept, as the
/// reference simplifies every support path once more before the writer (§8.7 Order, §9.5). Run on
/// a loop's ring from its first point round to that point again, it is [`dp_ring`].
pub fn dp_path(path: &[Pt], tol: f64) -> Vec<Pt> {
    if path.len() < 3 {
        return path.to_vec();
    }
    let mut keep = vec![false; path.len()];
    keep[0] = true;
    keep[path.len() - 1] = true;
    dp_mark(path, tol, &mut keep);
    path.iter().zip(&keep).filter(|&(_, &k)| k).map(|(p, _)| *p).collect()
}

/// Douglas–Peucker over the open path `path`, both ends kept: a point between two kept ones stays
/// when it is the farthest from the segment between them (the earlier on a tie) and more than
/// `tol` from it.
fn dp_mark(path: &[Pt], tol: f64, keep: &mut [bool]) {
    let mut spans = vec![(0, path.len() - 1)];
    while let Some((a, b)) = spans.pop() {
        if b <= a + 1 {
            continue;
        }
        let (mut at, mut best) = (a, -1.0);
        for (i, &p) in path.iter().enumerate().take(b).skip(a + 1) {
            let d = dist_point_segment(p, path[a], path[b]);
            if d > best {
                (at, best) = (i, d);
            }
        }
        if best > tol {
            keep[at] = true;
            spans.push((a, at));
            spans.push((at, b));
        }
    }
}

/// Whether `set` holds more than R14's tiny area.
pub fn some(set: &[ExPolygon]) -> bool {
    total_area_mm2(set) > super::settings::TINY_AREA
}

/// The point of `set` nearest `p`: `p` itself when it is inside (or on the boundary), else the
/// nearest point of any ring, holes included (§6 "moved inside").
pub fn moved_inside(set: &[ExPolygon], p: Pt) -> Pt {
    if set.iter().any(|e| e.contains(p)) {
        return p;
    }
    let mut best: Option<(f64, Pt)> = None;
    for e in set {
        for r in e.rings() {
            let m = r.pts.len();
            for i in 0..m {
                let q = nearest_on(r.pts[i], r.pts[(i + 1) % m], p);
                let d = p.dist(q);
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, q));
                }
            }
        }
    }
    best.map_or(p, |b| b.1)
}

/// The point of segment `a`-`b` nearest `p`.
fn nearest_on(a: Pt, b: Pt, p: Pt) -> Pt {
    let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let l2 = dx * dx + dy * dy;
    if l2 == 0.0 {
        return a;
    }
    let t = ((((p.x - a.x) as f64) * dx + ((p.y - a.y) as f64) * dy) / l2).clamp(0.0, 1.0);
    Pt::new(a.x + (dx * t).round() as i64, a.y + (dy * t).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::super::settings::tests::r14;
    use super::*;

    /// §4.7, the disc's shape: the circle maker's corner count from its 25 nm error, corner 0 on
    /// +X, every coordinate truncated towards zero; a roof disc is the base circle times the
    /// integer ratio of the radii, moved to its point.
    #[test]
    fn discs_are_the_base_circle_scaled() {
        let base = circle(BASE_RADIUS);
        assert_eq!(base.len(), 45, "45 corners at 0.01 mm");
        assert_eq!(circle(500_000).len(), 315, "315 at 0.5 mm, an enforcer point's disc");
        assert_eq!(base[0], Pt::new(10_000, 0), "corner 0 on +X");
        // Corner 1 at 8°: cos 9 902.680 7…, sin 1 391.731 0…, both truncated.
        assert_eq!(base[1], Pt::new(9_902, 1_391), "truncated, not rounded");
        // Corner 12 at 96°: cos −1 045.284 6…, truncated towards zero.
        assert_eq!(base[12], Pt::new(-1_045, 9_945), "negative coordinates truncate towards zero");
        let at = Pt::new(3_000_001, -2_000_003);
        let roof = roof_disc(at, 0.4);
        assert!(roof.holes.is_empty());
        let corners = &roof.contour.pts;
        assert_eq!(corners.len(), 45);
        assert!(corners.iter().zip(&base).all(|(c, q)| *c == Pt::new(at.x + 40 * q.x, at.y + 40 * q.y)), "r_tip 0.4 mm: the base corners times 40, moved to the point");
        // The ratio is an integer division of the radii in nm: 0.405 mm still scales by 40.
        assert_eq!(roof_disc(at, 0.405).contour.pts, roof.contour.pts, "405 000 ÷ 10 000 is 40");
    }

    /// §1.6's Douglas–Peucker with no size guard, on its own example: a triangle 0.03 mm across
    /// and 0.008 mm high, simplified at 0.012 mm, keeps all three points from its apex and drops to
    /// two from a base corner; the far point goes when it is within the tolerance; distances are to
    /// the segment, clamped at its ends.
    #[test]
    fn the_simplify_has_no_size_guard() {
        let (b1, b2, apex) = (Pt::new(0, 0), Pt::new(30_000, 0), Pt::new(15_000, 8_000));
        assert_eq!(dp_ring(&[apex, b1, b2], 12_000.0), [apex, b1, b2], "from the apex: both corners 0.017 mm from it, the second 0.017 mm from the segment's end");
        assert_eq!(dp_ring(&[b1, b2, apex], 12_000.0), [b1, b2], "from a base corner: the apex is 0.008 mm from the base");
        assert_eq!(dp_ring(&[b1, Pt::new(5_000, 0), Pt::new(5_000, 5_000)], 12_000.0), [b1], "nothing beyond the tolerance from the first point: the ring falls to one point");
        let behind = [Pt::new(0, 0), Pt::new(-20_000, 5_000), Pt::new(100_000, 0)];
        assert_eq!(dp_ring(&behind, 12_000.0), behind, "a point behind the segment's start counts its distance to that end: 0.021 mm, not 0.005 mm to the line");
        let set = vec![ExPolygon::new(Polygon::new(vec![b1, b2, apex]))];
        assert!(simplified(&set, 0.012).is_empty(), "the clean-up drops the ring left with two points");
        assert_eq!(simplified(&[ExPolygon::new(Polygon::new(vec![apex, b1, b2]))], 0.012).len(), 1, "the same triangle from its apex survives");
    }

    /// §1.6's island simplify: each ring from its own first point, the island's rings united
    /// with every ring by Rule A, and no union across islands. The triangle above, as a hole run
    /// clockwise, stays in its island from its apex and goes from a base corner; two islands that
    /// share an edge stay two, where the per-ring kind unites them.
    #[test]
    fn the_island_simplify_keeps_one_entry_per_island() {
        let (b1, b2, apex) = (Pt::new(1_000_000, 1_000_000), Pt::new(1_030_000, 1_000_000), Pt::new(1_015_000, 1_008_000));
        let square = |x0: f64| Polygon::rect(Pt::from_mm(x0, 0.0), Pt::from_mm(x0 + 2.0, 2.0));
        let holed = |hole: Vec<Pt>| ExPolygon { contour: square(0.0), holes: vec![Polygon::new(hole)] };
        let from_apex = island_simplified(&[holed(vec![apex, b2, b1])], 0.012);
        assert_eq!((from_apex.len(), from_apex[0].holes.len()), (1, 1), "from its apex the hole keeps its three corners");
        assert_eq!(from_apex[0].contour.pts[0], Pt::from_mm(2.0, 2.0), "the outer ring by Rule A: after its end vertex, the flat bottom's right end");
        let from_corner = island_simplified(&[holed(vec![b1, apex, b2])], 0.012);
        assert_eq!((from_corner.len(), from_corner[0].holes.len()), (1, 0), "from a base corner it falls to two points and goes");
        let pair = [ExPolygon::new(square(0.0)), ExPolygon::new(square(2.0))];
        assert_eq!(island_simplified(&pair, 0.012).len(), 2, "islands that share an edge stay two");
        assert_eq!(simplified(&pair, 0.012).len(), 1, "the per-ring kind unites them");
    }

    /// §8.7 Order, §9.5: the last simplify keeps both ends of a path, however close to the rest;
    /// of the points between, one more than the tolerance off the line between its kept neighbours
    /// stays and the others go; and on a loop's ring run from its first point round to that point
    /// again it keeps what [`dp_ring`] keeps.
    #[test]
    fn the_last_simplify_keeps_both_ends() {
        let tol = 12_000.0;
        let (a, e) = (Pt::new(0, 0), Pt::new(200_000, 0));
        let path = [a, Pt::new(50_000, 11_000), Pt::new(100_000, 0), Pt::new(150_000, 13_000), e];
        assert_eq!(dp_path(&path, tol), [a, Pt::new(150_000, 13_000), e], "0.013 mm off the line stays; then 0.007 and 0.009 mm off the new chord go");
        let hook = [a, Pt::new(100_000, 0), Pt::new(100_000, 5_000)];
        assert_eq!(dp_path(&hook, tol), [a, Pt::new(100_000, 5_000)], "the end stays 0.005 mm from the corner it loses");
        assert_eq!(dp_path(&hook[..2], tol), hook[..2], "two points stay as they are");
        let disc = roof_disc(Pt::new(3_000_000, 2_000_000), 0.4).contour.pts;
        let mut round = disc.clone();
        round.push(disc[0]);
        let mut kept = dp_path(&round, tol);
        assert_eq!(kept.pop(), Some(disc[0]), "the loop ends where it starts");
        assert_eq!(kept, dp_ring(&disc, tol), "a loop run round from its first point is the ring's simplify");
    }

    /// §4.6: a new tip has both counters and the foot at 0, its influence area the base circle
    /// (45 corners, 0.01 mm) moved to its point, and keeps xy_min while the side gap is wider.
    #[test]
    fn a_new_tip_is_a_tiny_disc() {
        let s = r14(0.8);
        let at = Pt::from_mm(3.0, 4.0);
        let e = Element::tip(&s, 20, at);
        assert_eq!((e.depth, e.ramp, e.foot, e.highest_tip), (0, 0, 0.0, 20), "counters at 0, the target its own layer");
        assert!(e.min_gap, "a 0.8 mm side gap is wider than xy_min: the tip starts at xy_min");
        assert!(!Element::tip(&r14(0.21), 20, at).min_gap, "at xy_min itself there is nothing to keep");
        assert_eq!(e.area.len(), 1);
        let ring = &e.area[0].contour.pts;
        assert!(ring.iter().zip(circle(BASE_RADIUS)).all(|(c, q)| *c == Pt::new(at.x + q.x, at.y + q.y)), "the base circle moved to the point");
        assert!(ring.iter().all(|q| (q.dist(at) / SCALE - 0.01).abs() < 2e-6), "every corner 0.01 mm from the point");
        assert_eq!(e.placed, Some(at), "placed at its point from the start");
        assert_eq!(moved_inside(&e.area, at), at, "the point is inside its own area");
        let out = moved_inside(&e.area, Pt::from_mm(4.0, 4.0));
        assert!((out.dist(at) / SCALE - 0.01).abs() < 2e-6, "a point outside moves to the disc's edge: {}", out.dist(at) / SCALE);
    }
}

//! The tree's booleans (R14 §1.6): the engine's union, difference and intersection, each result
//! handed on as the reference's engine hands it on. Every ring ends at its end vertex V and starts
//! at the vertex after it (Rule A); the islands come highest first, each followed at once by the
//! islands inside its holes. R14 measures from a ring's first point (§1.5's close points, every
//! simplification's fixed point, §2.10's overlap test, §4.4's one point and fills, §8.5, §8.6) and
//! takes pieces in their order (§2.3–§2.7, §4.4, §4.7, §8.6), so every boolean the tree makes goes
//! through here, and every offset through offset.rs, which hands its results on the same way.

use makepad_fab_slicer::clip;
pub use makepad_fab_slicer::clip::{BoolOp, FillRule};
use makepad_fab_slicer::geom::*;

/// The end vertex V of `ring` (§1.6), among its lowest vertices (least y, exactly): where two or
/// more in a row share that y, the right end of the leftmost such flat bottom; else the rightmost.
/// None for a ring with no vertex above its lowest.
fn end_vertex(ring: &[Pt]) -> Option<usize> {
    let n = ring.len();
    let low = ring.iter().map(|p| p.y).min()?;
    // From a vertex above the bottom, so that no run of lowest vertices wraps round the end.
    let s = (0..n).find(|&i| ring[i].y != low)?;
    let (mut flat, mut lone): (Option<(i64, usize)>, Option<usize>) = (None, None);
    let mut k = 1;
    while k < n {
        let i = (s + k) % n;
        if ring[i].y != low {
            k += 1;
            continue;
        }
        // A run of lowest vertices: its leftmost x and its right end.
        let (mut len, mut left, mut right) = (0, i64::MAX, i);
        while k < n && ring[(s + k) % n].y == low {
            let j = (s + k) % n;
            left = left.min(ring[j].x);
            if ring[j].x > ring[right].x {
                right = j;
            }
            len += 1;
            k += 1;
        }
        if len == 1 {
            if lone.is_none_or(|v| ring[i].x > ring[v].x) {
                lone = Some(i);
            }
        } else if flat.is_none_or(|(l, _)| left < l) {
            flat = Some((left, right));
        }
    }
    flat.map(|(_, v)| v).or(lone)
}

/// Rule A (§1.6): `ring` as it comes straight from the engine, ending at its end vertex V, so that
/// its first point is the vertex after V in stored order (counter-clockwise for an outer ring,
/// clockwise for a hole).
pub fn rule_a(ring: &mut [Pt]) {
    if let Some(v) = end_vertex(ring) {
        ring.rotate_left((v + 1) % ring.len());
    }
}

/// Rule B (§1.6): `ring` as a wrapper leaves it that turns an engine ring round, starting at its
/// end vertex V: the per-ring simplify's holes, cleaned as outer rings and turned back to
/// clockwise.
pub fn rule_b(ring: &mut [Pt]) {
    if let Some(v) = end_vertex(ring) {
        ring.rotate_left(v);
    }
}

/// Where a ring sorts in a result (§1.6): by its highest point, highest first; between pieces whose
/// tops share a y, left to right, as pieces cut from one top edge come out.
fn top(ring: &[Pt]) -> (i64, i64) {
    ring.iter().map(|p| (-p.y, p.x)).min().unwrap_or((i64::MAX, i64::MAX))
}

/// `set` as the reference's engine hands an island result on (§1.6): every ring by Rule A, each
/// island's holes highest first, and the islands highest first, each followed at once by the
/// islands inside its holes, depth first and hole by hole.
pub fn in_reference_order(mut set: Vec<ExPolygon>) -> Vec<ExPolygon> {
    for e in &mut set {
        rule_a(&mut e.contour.pts);
        for h in &mut e.holes {
            rule_a(&mut h.pts);
        }
        // Each ring's top once, not once a comparison; the sorts are stable, ties as they came.
        e.holes.sort_by_cached_key(|h| top(&h.pts));
    }
    in_island_order(set)
}

/// `set`'s islands, their rings as they are, in the reference's island order (§1.6): highest
/// first, each followed at once by the islands inside its holes. Results made apart, a group of
/// islands at a time, come together so as one result.
pub fn in_island_order(mut set: Vec<ExPolygon>) -> Vec<ExPolygon> {
    set.sort_by_cached_key(|e| top(&e.contour.pts));
    nested(set)
}

/// `set` (sorted) with each island moved to follow at once the island whose hole holds it, in that
/// island's hole order, depth first.
fn nested(set: Vec<ExPolygon>) -> Vec<ExPolygon> {
    if set.iter().all(|e| e.holes.is_empty()) {
        return set;
    }
    let n = set.len();
    let within = |outer: &BBox, inner: &BBox| outer.min.x <= inner.min.x && outer.min.y <= inner.min.y && outer.max.x >= inner.max.x && outer.max.y >= inner.max.y;
    // Each island's holder: the smallest hole of another island round it. Every hole's box once.
    let boxes: Vec<Vec<BBox>> = set.iter().map(|e| e.holes.iter().map(|h| h.bbox()).collect()).collect();
    let mut kids: Vec<Vec<Vec<usize>>> = set.iter().map(|e| vec![Vec::new(); e.holes.len()]).collect();
    let mut held = vec![false; n];
    for i in 0..n {
        let (bb, p) = (set[i].contour.bbox(), set[i].contour.pts[0]);
        let mut best: Option<(i128, usize, usize)> = None;
        for (j, e) in set.iter().enumerate().filter(|&(j, _)| j != i) {
            for (h, hole) in e.holes.iter().enumerate() {
                if within(&boxes[j][h], &bb) && hole.contains(p) {
                    let a = hole.area2().abs();
                    if best.is_none_or(|b| a < b.0) {
                        best = Some((a, j, h));
                    }
                }
            }
        }
        if let Some((_, j, h)) = best {
            kids[j][h].push(i);
            held[i] = true;
        }
    }
    let mut order = Vec::with_capacity(n);
    let mut stack: Vec<usize> = (0..n).filter(|&i| !held[i]).rev().collect();
    while let Some(i) = stack.pop() {
        order.push(i);
        stack.extend(kids[i].iter().flatten().rev());
    }
    let mut rank = vec![0; n];
    for (r, &i) in order.iter().enumerate() {
        rank[i] = r;
    }
    let mut ranked: Vec<(usize, ExPolygon)> = set.into_iter().enumerate().map(|(i, e)| (rank[i], e)).collect();
    ranked.sort_by_key(|r| r.0);
    ranked.into_iter().map(|r| r.1).collect()
}

/// `set` as the reference's per-ring simplify leaves it (§1.6): its clean-up's order and outer
/// rings by Rule A, and its holes, cleaned as outer rings and turned back, by Rule B. (The island
/// simplify of the slices, §2.0, leaves every ring by Rule A: [`in_reference_order`].)
pub fn as_simplified(set: Vec<ExPolygon>) -> Vec<ExPolygon> {
    let mut set = in_reference_order(set);
    for e in &mut set {
        for h in &mut e.holes {
            rule_b(&mut h.pts);
        }
    }
    set
}

/// The union of `polys` under `fill`, in the reference's order.
pub fn union(polys: &[Polygon], fill: FillRule) -> Vec<ExPolygon> {
    in_reference_order(clip::union(polys, fill))
}

/// The union of `ex`, in the reference's order.
pub fn union_ex(ex: &[ExPolygon]) -> Vec<ExPolygon> {
    in_reference_order(clip::union_ex(ex))
}

/// The union of `a` and `b`, in the reference's order.
pub fn union2_ex(a: &[ExPolygon], b: &[ExPolygon]) -> Vec<ExPolygon> {
    in_reference_order(clip::union2_ex(a, b))
}

/// `a` less `b`, in the reference's order. `b` is cut to `a`'s window first ([`window`]): the
/// tree takes small areas from a whole layer's volumes, and the engine's work goes with what it
/// is handed.
pub fn difference_ex(a: &[ExPolygon], b: &[ExPolygon]) -> Vec<ExPolygon> {
    in_reference_order(less(a, b))
}

/// `a` within `b`, in the reference's order, each cut to the window of what they share first
/// ([`window`]).
pub fn intersection_ex(a: &[ExPolygon], b: &[ExPolygon]) -> Vec<ExPolygon> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let (ab, bb) = (bbox_of(a), bbox_of(b));
    let both = BBox { min: Pt::new(ab.min.x.max(bb.min.x), ab.min.y.max(bb.min.y)), max: Pt::new(ab.max.x.min(bb.max.x), ab.max.y.min(bb.max.y)) };
    let w = widened(&both);
    if w.min.x > w.max.x || w.min.y > w.max.y {
        return Vec::new();
    }
    let (pa, pb) = (window(a, &w), window(b, &w));
    if pa.is_empty() || pb.is_empty() {
        return Vec::new();
    }
    in_reference_order(clip::boolean(&pa, &pb, BoolOp::Intersection, FillRule::NonZero))
}

/// `a` less `b` as the engine leaves it, `b` cut to `a`'s window first ([`window`]).
pub fn less(a: &[ExPolygon], b: &[ExPolygon]) -> Vec<ExPolygon> {
    if a.is_empty() {
        return Vec::new();
    }
    if b.is_empty() {
        return a.to_vec();
    }
    clip::boolean(&to_polygons(a), &window(b, &widened(&bbox_of(a))), BoolOp::Difference, FillRule::NonZero)
}

/// How far (units) a window reaches past the box it is for: as far as the engine's own nearness
/// test, so no point a cut makes comes within a hot pixel of what is inside the box.
const WINDOW_MARGIN: i64 = 2;

pub(crate) fn widened(b: &BBox) -> BBox {
    BBox { min: Pt::new(b.min.x - WINDOW_MARGIN, b.min.y - WINDOW_MARGIN), max: Pt::new(b.max.x + WINDOW_MARGIN, b.max.y + WINDOW_MARGIN) }
}

/// The rings of `set` as they count inside `w`: a ring whose box lies in `w` as it is, one whose
/// box misses `w` left out, and any other cut to `w` side by side (Sutherland–Hodgman), what it
/// did beyond a side coming back along that side. Every point inside `w` keeps its winding; a
/// cut edge runs to its crossing rounded onto the side, under a unit from where the edge crossed.
fn window(set: &[ExPolygon], w: &BBox) -> Vec<Polygon> {
    let mut out = Vec::new();
    for ring in set.iter().flat_map(|e| e.rings()) {
        let b = ring.bbox();
        if !b.overlaps(w) {
            continue;
        }
        if b.min.x >= w.min.x && b.min.y >= w.min.y && b.max.x <= w.max.x && b.max.y <= w.max.y {
            out.push(ring.clone());
            continue;
        }
        let mut pts = ring.pts.clone();
        for side in [Side::Left(w.min.x), Side::Right(w.max.x), Side::Bottom(w.min.y), Side::Top(w.max.y)] {
            if pts.is_empty() {
                break;
            }
            pts = cut(&pts, side);
        }
        if pts.len() >= 3 {
            out.push(Polygon::new(pts));
        }
    }
    out
}

/// A side of a window: the half-plane x ≥ (left), x ≤ (right), y ≥ (bottom) or y ≤ (top) it.
#[derive(Clone, Copy)]
enum Side {
    Left(i64),
    Right(i64),
    Bottom(i64),
    Top(i64),
}

impl Side {
    fn keeps(self, p: Pt) -> bool {
        match self {
            Side::Left(x) => p.x >= x,
            Side::Right(x) => p.x <= x,
            Side::Bottom(y) => p.y >= y,
            Side::Top(y) => p.y <= y,
        }
    }

    /// Where the edge from `a` to `b`, one kept and one not, crosses the side, rounded onto it.
    /// The same for the edge either way round.
    fn crossing(self, a: Pt, b: Pt) -> Pt {
        // The line through a and b at u = k, for (u, v) = (x, y) or (y, x): v exactly, rounded.
        let at = |k: i64, (au, av): (i64, i64), (bu, bv): (i64, i64)| -> i64 {
            let (num, den) = (av as i128 * (bu - au) as i128 + (bv - av) as i128 * (k - au) as i128, (bu - au) as i128);
            let (num, den) = if den < 0 { (-num, -den) } else { (num, den) };
            (if num >= 0 { (2 * num + den) / (2 * den) } else { -((-2 * num + den) / (2 * den)) }) as i64
        };
        match self {
            Side::Left(x) | Side::Right(x) => Pt::new(x, at(x, (a.x, a.y), (b.x, b.y))),
            Side::Bottom(y) | Side::Top(y) => Pt::new(at(y, (a.y, a.x), (b.y, b.x)), y),
        }
    }
}

/// The closed ring `pts` cut to `side`.
fn cut(pts: &[Pt], side: Side) -> Vec<Pt> {
    if pts.iter().all(|&p| side.keeps(p)) {
        return pts.to_vec();
    }
    let mut out = Vec::with_capacity(pts.len() + 4);
    let mut prev = pts[pts.len() - 1];
    for &p in pts {
        match (side.keeps(prev), side.keeps(p)) {
            (true, true) => out.push(p),
            (true, false) => out.push(side.crossing(prev, p)),
            (false, true) => {
                out.push(side.crossing(prev, p));
                out.push(p);
            }
            (false, false) => {}
        }
        prev = p;
    }
    out
}

/// `subject` `op` `other` under `fill`, in the reference's order.
pub fn boolean(subject: &[Polygon], other: &[Polygon], op: BoolOp, fill: FillRule) -> Vec<ExPolygon> {
    in_reference_order(clip::boolean(subject, other, op, fill))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square of side `s` from (`x`, `y`), counter-clockwise from its top right corner.
    fn square(x: i64, y: i64, s: i64) -> Polygon {
        Polygon::new(vec![Pt::new(x + s, y + s), Pt::new(x, y + s), Pt::new(x, y), Pt::new(x + s, y)])
    }

    /// The same square clockwise, as a hole, from its top left corner.
    fn square_hole(x: i64, y: i64, s: i64) -> Polygon {
        Polygon::new(vec![Pt::new(x, y + s), Pt::new(x + s, y + s), Pt::new(x + s, y), Pt::new(x, y)])
    }

    /// §1.6's end vertex and the two rules: a pointed bottom, a flat one (its right end), the
    /// leftmost of two flats, the rightmost of two points; Rule A ends there, Rule B starts there.
    #[test]
    fn rings_end_at_their_end_vertex() {
        let pointed = vec![Pt::new(5, 5), Pt::new(0, 9), Pt::new(-4, 0), Pt::new(6, 1)];
        assert_eq!(end_vertex(&pointed), Some(2), "one lowest vertex");
        let mut a = pointed.clone();
        rule_a(&mut a);
        assert_eq!(a, [Pt::new(6, 1), Pt::new(5, 5), Pt::new(0, 9), Pt::new(-4, 0)], "Rule A: V last, the vertex after it first");
        let mut b = pointed;
        rule_b(&mut b);
        assert_eq!(b[0], Pt::new(-4, 0), "Rule B: V first");
        let mut flat = square(0, 0, 10).pts;
        rule_a(&mut flat);
        assert_eq!(flat, [Pt::new(10, 10), Pt::new(0, 10), Pt::new(0, 0), Pt::new(10, 0)], "a flat bottom ends at its right end, and the ring goes on up the right side");
        let mut hole = square_hole(2, 2, 6).pts;
        rule_a(&mut hole);
        assert_eq!(hole, [Pt::new(2, 2), Pt::new(2, 8), Pt::new(8, 8), Pt::new(8, 2)], "a hole too ends at its flat bottom's right end, and starts at the next vertex clockwise");
        // Two flat bottoms (an M upside down): the leftmost one's right end, at index 2.
        let two = vec![Pt::new(9, 5), Pt::new(0, 5), Pt::new(0, 0), Pt::new(3, 0), Pt::new(5, 2), Pt::new(7, 0), Pt::new(9, 0)];
        assert_eq!(end_vertex(&two), Some(3), "the leftmost flat bottom's right end");
        // Two lowest points apart (a W): the rightmost.
        let w = vec![Pt::new(9, 9), Pt::new(0, 9), Pt::new(2, 0), Pt::new(4, 4), Pt::new(6, 0)];
        assert_eq!(end_vertex(&w), Some(4), "the rightmost of two lowest points");
        // A run of lowest vertices that wraps round the stored end.
        let wrap = vec![Pt::new(10, 0), Pt::new(10, 10), Pt::new(0, 10), Pt::new(0, 0), Pt::new(5, 0)];
        assert_eq!(end_vertex(&wrap), Some(0), "a flat bottom stored across the end");
        assert_eq!(end_vertex(&[Pt::new(0, 0), Pt::new(5, 0)]), None, "nothing above the bottom");
    }

    /// §1.6's island order: highest first, left to right on a tie, each island followed at once by
    /// the islands inside its holes; holes highest first; every ring by Rule A.
    #[test]
    fn islands_come_highest_first_and_nested() {
        let mut frame = ExPolygon::new(square(0, 0, 100));
        frame.holes = vec![square_hole(10, 10, 30), square_hole(10, 60, 30)];
        let low_in = ExPolygon::new(square(15, 15, 5));
        let high_in = ExPolygon::new(square(15, 65, 5));
        let left = ExPolygon::new(square(-80, 60, 40));
        let right = ExPolygon::new(square(200, 60, 40));
        let out = in_reference_order(vec![low_in, right, high_in, frame, left]);
        let tops: Vec<Pt> = out.iter().map(|e| e.contour.pts.iter().copied().max_by_key(|p| (p.y, -p.x)).unwrap()).collect();
        assert_eq!(tops, [Pt::new(-80, 100), Pt::new(0, 100), Pt::new(15, 70), Pt::new(15, 20), Pt::new(200, 100)], "left to right at one height, the frame followed at once by its holes' islands, the upper hole's first");
        let holes: Vec<Pt> = out[1].holes.iter().map(|h| h.pts[0]).collect();
        assert_eq!(holes, [Pt::new(10, 60), Pt::new(10, 10)], "the frame's holes highest first, each from the vertex after its end vertex");
        assert!(out.iter().all(|e| e.contour.pts.last() == Some(&Pt::new(e.contour.bbox().max.x, e.contour.bbox().min.y))), "every contour ends at its bottom right corner");
    }

    /// The wrappers hand every result on by Rule A and highest first, the engine's own
    /// short cuts (an empty operand) included; the per-ring simplify's holes go by Rule B.
    #[test]
    fn every_boolean_hands_its_result_on_in_order() {
        let a = vec![ExPolygon::new(square(0, 0, 10)), ExPolygon::new(square(0, 20, 10))];
        for out in [union_ex(&a), difference_ex(&a, &[]), union2_ex(&a, &[]), difference_ex(&a, &[ExPolygon::new(square(50, 50, 1))])] {
            let firsts: Vec<Pt> = out.iter().map(|e| e.contour.pts[0]).collect();
            assert_eq!(firsts, [Pt::new(10, 30), Pt::new(10, 10)], "the upper square first, each from its top right corner");
        }
        let mut ring = ExPolygon::new(square(0, 0, 10));
        ring.holes = vec![square_hole(2, 2, 6)];
        let s = as_simplified(vec![ring]);
        assert_eq!(s[0].contour.pts[0], Pt::new(10, 10), "the outer ring by Rule A");
        assert_eq!(s[0].holes[0].pts[0], Pt::new(8, 2), "the hole by Rule B: from its end vertex");
    }

    /// A whole layer's set (a frame with two holes, and an island far off) and a small area
    /// across its edges: cut to the small area's window first, the booleans come out exactly as
    /// the engine makes them uncut, edges on the grid crossing the window's sides on the grid.
    #[test]
    fn a_window_changes_nothing_inside_it() {
        let mut frame = ExPolygon::new(square(0, 0, 1000));
        frame.holes = vec![square_hole(100, 100, 300), square_hole(600, 600, 300)];
        let big = vec![frame, ExPolygon::new(square(5000, 0, 100))];
        for small in [square(350, 350, 100), square(-50, 450, 200), square(380, 50, 40), square(2000, 0, 10)] {
            let small = vec![ExPolygon::new(small)];
            assert_eq!(difference_ex(&small, &big), in_reference_order(clip::difference_ex(&small, &big)), "less {:?}", small[0].contour.pts[0]);
            assert_eq!(intersection_ex(&small, &big), in_reference_order(clip::intersection_ex(&small, &big)), "within {:?}", small[0].contour.pts[0]);
            assert_eq!(intersection_ex(&big, &small), in_reference_order(clip::intersection_ex(&big, &small)), "within, the other way {:?}", small[0].contour.pts[0]);
        }
    }

    /// A window keeps every winding inside it: a ring round it all counts as the window itself,
    /// one that bends round it without coming in counts for nothing, and a slanted edge across
    /// it moves under a unit where it is cut.
    #[test]
    fn a_window_keeps_the_winding_inside_it() {
        let small = vec![ExPolygon::new(square(0, 0, 100))];
        let round = vec![ExPolygon::new(square(-1000, -1000, 3000))];
        assert!(difference_ex(&small, &round).is_empty(), "inside a ring round it: nothing left");
        assert_eq!(intersection_ex(&small, &round), in_reference_order(small.clone()), "all of it within");
        // A C round the square, open to the right, its arms and back clear of it.
        let c = Polygon::new(vec![Pt::new(1000, -1000), Pt::new(1000, -500), Pt::new(-500, -500), Pt::new(-500, 600), Pt::new(1000, 600), Pt::new(1000, 1100), Pt::new(-1000, 1100), Pt::new(-1000, -1000)]);
        let c = vec![ExPolygon::new(c)];
        assert_eq!(difference_ex(&small, &c), in_reference_order(small.clone()), "a ring that bends round it takes nothing");
        assert!(intersection_ex(&small, &c).is_empty());
        // A slanted half-plane's edge through the square, its ends far outside.
        let slant = vec![ExPolygon::new(Polygon::new(vec![Pt::new(-100_000, -99_917), Pt::new(100_000, 100_041), Pt::new(-100_000, 100_041)]))];
        let (got, want) = (intersection_ex(&small, &slant), clip::intersection_ex(&small, &slant));
        let area = |s: &[ExPolygon]| s.iter().map(|e| e.contour.area2()).sum::<i128>();
        assert!((area(&got) - area(&want)).abs() <= 2 * 100, "the area within a unit along the cut: {} vs {}", area(&got), area(&want));
        assert!(got.iter().flat_map(|e| &e.contour.pts).all(|p| want.iter().flat_map(|e| &e.contour.pts).any(|q| (p.x - q.x).abs() <= 1 && (p.y - q.y).abs() <= 1)), "{got:?} vs {want:?}");
    }
}

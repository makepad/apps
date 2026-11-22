//! Where a support gets down from to its footing, exactly.
//!
//! Per layer `l`, R_l is where a support's centre may stand and still get down, a layer at a
//! time, to the bed (or, not from the bed only, to the part it may stand on). R_0 is what
//! nothing keeps it off on the bed; R_l is what nothing keeps it off within the layer's lean m_l
//! of R_{l-1}, or on a part top it may stand on. So reach goes round walls and through real
//! windows of any width, and does not hang on where the part lies against any grid.
//!
//! It is kept as its complement over the part, B_l: what keeps a support off, K_l, and what it
//! does not reach, flooded up layer by layer as B_l = K_l ∪ ((B_{l-1} ⊖ m_l) \ the part tops).
//! An erosion's arcs are cut by chords inside them, so what is not reached only ever comes out
//! larger: every point of R_l has a point of R_{l-1} within m_l, the next step of its route
//! down.
//!
//! The grid of [`crate::footing`] is the broad phase: a cell that touches nothing of B is reached
//! without asking the polygons.

use crate::footing::Footing;
use crate::par::{Cancelled, Ctl};
use makepad_fab_slicer::clip::{difference_ex, union_ex};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::{offset_ex, Join};

/// How far an erosion's chord may lie inside its arc (units): what is not reached grows by no
/// more than this at a corner.
const ARC: f64 = 5000.0;
/// How far (units) past its lean a route's next step may be: the rounding of an eroded corner.
const WITNESS: f64 = 10.0;
/// The furthest (units) a point found on the edge of what is not reached is moved off it.
const NUDGE: i64 = 64;

/// Per layer, where a support reaches its footing from (see the module).
pub struct Reach {
    /// K_l.
    keep: Vec<Vec<ExPolygon>>,
    /// B_l: K_l and what is not reached.
    blocked: Vec<Vec<ExPolygon>>,
    /// Per layer, the part tops a support may stand on there.
    rests: Vec<Vec<ExPolygon>>,
    /// m_l (mm): how far layer `l` may lie past layer `l - 1`.
    lean: Vec<f64>,
    broad: Footing,
}

/// `set` eroded by `by` (units) with round joins at [`ARC`]: how [`Reach::new`] and
/// [`Reach::placed`] take a layer's lean off what is not reached under it.
fn round(set: &[ExPolygon], by: f64) -> Vec<ExPolygon> {
    offset_ex(set, -by, Join::Round(ARC))
}

/// A layer's B_l as it comes: how [`Reach::new`] and [`Reach::placed`] finish a layer.
fn as_is(set: Vec<ExPolygon>) -> Vec<ExPolygon> {
    set
}

fn less(a: &[ExPolygon], b: &[ExPolygon]) -> Vec<ExPolygon> {
    if a.is_empty() || b.is_empty() { a.to_vec() } else { difference_ex(a, b) }
}

/// K_l as a reach keeps it: what keeps a support's centre off on a layer, united.
pub fn keep_of(keep: &[ExPolygon]) -> Vec<ExPolygon> {
    if keep.is_empty() { Vec::new() } else { union_ex(keep) }
}

/// The part tops a support may stand on on a layer, as a reach keeps them: `top` united, less
/// K_l (as [`keep_of`] leaves it) unless they are taken out last ([`Reach::placed`]).
pub fn rests_of(top: &[ExPolygon], keep: &[ExPolygon], placed: bool) -> Vec<ExPolygon> {
    if top.is_empty() {
        Vec::new()
    } else if placed {
        union_ex(top)
    } else {
        less(&union_ex(top), keep)
    }
}

/// B_l as [`Reach::eroded_by`] makes it, before its `finish`: from K_l and the part tops (as
/// [`keep_of`] and [`rests_of`] leave them) and B_{l-1} (none on the bottom layer), the lean
/// `lean` (mm) taken off B_{l-1} by `erode`.
pub fn layer_of(keep: &[ExPolygon], rests: &[ExPolygon], below: &[ExPolygon], lean: f64, placed: bool, erode: &dyn Fn(&[ExPolygon], f64) -> Vec<ExPolygon>) -> Vec<ExPolygon> {
    let eroded = if below.is_empty() { Vec::new() } else { erode(below, lean * SCALE) };
    if placed {
        less(&if eroded.is_empty() { keep.to_vec() } else { union_ex(&[keep.to_vec(), eroded].concat()) }, rests)
    } else {
        let out = less(&eroded, rests);
        if out.is_empty() { keep.to_vec() } else { union_ex(&[keep.to_vec(), out].concat()) }
    }
}

/// What has nothing reached within `by` (mm) of it, for a layer's B_l: B_l less `by` round it.
/// Rounded corners only ever come out larger.
pub fn beyond_of(blocked: &[ExPolygon], by: f64) -> Vec<ExPolygon> {
    if blocked.is_empty() { Vec::new() } else { offset_ex(blocked, -by * SCALE, Join::Round(ARC)) }
}

/// Whether `p` is inside `set`.
pub fn inside(set: &[ExPolygon], p: Pt) -> bool {
    set.iter().any(|e| {
        let b = e.bbox();
        p.x >= b.min.x && p.x <= b.max.x && p.y >= b.min.y && p.y <= b.max.y && e.contains(p)
    })
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

/// The unit normal of `a`-`b` on its right: out of a region whose rings run as the slicer's do.
fn right_of(a: Pt, b: Pt) -> (f64, f64) {
    let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let l = dx.hypot(dy);
    if l == 0.0 { (0.0, 0.0) } else { (dy / l, -dx / l) }
}

/// The point nearest `p` just off the edge of `set`, within `max` mm of `p`, that `ok` takes:
/// per edge its nearest point, nudged away from `p`, out of the edge, and at a corner out of
/// both.
pub fn nearest_off(set: &[ExPolygon], p: Pt, max: f64, ok: impl Fn(Pt) -> bool) -> Option<Pt> {
    let r = (max * SCALE).ceil() as i64 + NUDGE;
    let near = |e: &&ExPolygon| {
        let b = e.bbox();
        p.x >= b.min.x - r && p.x <= b.max.x + r && p.y >= b.min.y - r && p.y <= b.max.y + r
    };
    // Per edge, its nearest point and the ways off it: away from `p`, the edge's outside, and
    // at a corner between the two edges' outsides.
    let mut found: Vec<(f64, Pt, [(f64, f64); 3])> = Vec::new();
    for e in set.iter().filter(near) {
        for ring in e.rings() {
            let m = ring.pts.len();
            for i in 0..m {
                let (a, b, c) = (ring.pts[(i + m - 1) % m], ring.pts[i], ring.pts[(i + 1) % m]);
                let q = nearest_on(b, c, p);
                let d = p.dist(q);
                if d > r as f64 {
                    continue;
                }
                let away = if d > 0.0 { (((q.x - p.x) as f64) / d, ((q.y - p.y) as f64) / d) } else { (0.0, 0.0) };
                let out = right_of(b, c);
                let corner = if q == b { let (u, v) = right_of(a, b); (u + out.0, v + out.1) } else { out };
                found.push((d, q, [away, out, corner]));
            }
        }
    }
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    let limit = max * SCALE + NUDGE as f64;
    for (_, q, ways) in found {
        for (u, v) in ways {
            let l2 = u.hypot(v);
            if l2 == 0.0 {
                continue;
            }
            let mut k = 1;
            while k <= NUDGE {
                let s = Pt::new(q.x + (u / l2 * k as f64).round() as i64, q.y + (v / l2 * k as f64).round() as i64);
                if p.dist(s) <= limit && ok(s) {
                    return Some(s);
                }
                k *= 2;
            }
        }
    }
    None
}

impl Reach {
    /// `keep`, per layer: what keeps a support's centre off. `rests`, per layer (none past its
    /// end, and none from the bed only): the part's top a support on it may stand on.
    /// `lean`, per layer: m_l (mm), how far layer `l` may lie past the one under it.
    pub fn new(ctl: Ctl, keep: &[Vec<ExPolygon>], rests: &[Vec<ExPolygon>], lean: &[f64]) -> Result<Self, Cancelled> {
        Self::build(ctl, keep, rests, lean, false, &round, &as_is)
    }

    /// [`Reach::new`] where a support arrives anywhere on a part top it may stand on, inside
    /// what keeps it off there too: the tops are taken out last, B_l = (K_l ∪ (B_{l-1} ⊖ m_l)) \
    /// the part tops, so what the next layer up floods from has them taken out as well.
    pub fn placed(ctl: Ctl, keep: &[Vec<ExPolygon>], rests: &[Vec<ExPolygon>], lean: &[f64]) -> Result<Self, Cancelled> {
        Self::build(ctl, keep, rests, lean, true, &round, &as_is)
    }

    /// [`Reach::new`], or with `placed` [`Reach::placed`], with each layer's lean taken off what
    /// is not reached under it by `erode` (the set, and the lean in units) in place of the round
    /// erosion at [`ARC`], and each layer's B_l but the bottom one's passed through `finish`
    /// before the layer over it floods from it. What is not reached comes out larger, as the
    /// module has it, only as far as `erode` keeps its chords inside its arcs and `finish` moves
    /// no edge in; one that simplifies moves edges both ways, so B_l need not hold K_l, and
    /// [`Reach::route`] and [`Reach::nearest`] are not for such a reach.
    pub fn eroded_by(ctl: Ctl, keep: &[Vec<ExPolygon>], rests: &[Vec<ExPolygon>], lean: &[f64], placed: bool, erode: &dyn Fn(&[ExPolygon], f64) -> Vec<ExPolygon>, finish: &dyn Fn(Vec<ExPolygon>) -> Vec<ExPolygon>) -> Result<Self, Cancelled> {
        Self::build(ctl, keep, rests, lean, placed, erode, finish)
    }

    fn build(ctl: Ctl, keep: &[Vec<ExPolygon>], rests: &[Vec<ExPolygon>], lean: &[f64], last: bool, erode: &dyn Fn(&[ExPolygon], f64) -> Vec<ExPolygon>, finish: &dyn Fn(Vec<ExPolygon>) -> Vec<ExPolygon>) -> Result<Self, Cancelled> {
        let n = keep.len();
        let keep: Vec<Vec<ExPolygon>> = crate::par::try_map(ctl, n, |l| keep_of(&keep[l]))?;
        let rests: Vec<Vec<ExPolygon>> = crate::par::try_map(ctl, n, |l| rests_of(rests.get(l).map_or(&[][..], Vec::as_slice), &keep[l], last))?;
        let mut blocked: Vec<Vec<ExPolygon>> = Vec::with_capacity(n);
        for l in 0..n {
            if ctl.cancelled() {
                return Err(Cancelled);
            }
            let here = layer_of(&keep[l], &rests[l], blocked.last().map_or(&[][..], Vec::as_slice), lean[l], last, erode);
            blocked.push(if l == 0 { here } else { finish(here) });
        }
        let broad = Footing::cover(ctl, &blocked)?;
        Ok(Reach { keep, blocked, rests, lean: lean.to_vec(), broad })
    }

    /// Whether a support's centre at `p` on layer `l` gets down to its footing.
    pub fn reached(&self, p: Pt, l: usize) -> bool {
        self.broad.clear(p, l) || !inside(&self.blocked[l], p)
    }

    /// What nothing keeps a support off on layer `l` but it does not reach.
    pub fn unreached(&self, l: usize) -> Vec<ExPolygon> {
        less(&self.blocked[l], &self.keep[l])
    }

    /// What has nothing reached within `by` (mm) of it on layer `l`: what keeps a support off and
    /// what it does not reach, less `by` round it. Rounded corners only ever come out larger.
    pub fn beyond(&self, l: usize, by: f64) -> Vec<ExPolygon> {
        beyond_of(&self.blocked[l], by)
    }

    /// What is reached on layer `l` within `bb`.
    pub fn area(&self, l: usize, bb: &BBox) -> Vec<ExPolygon> {
        let window = vec![ExPolygon::new(Polygon::rect(bb.min, bb.max))];
        let blocked: Vec<ExPolygon> = self.blocked[l].iter().filter(|e| e.bbox().overlaps(bb)).cloned().collect();
        less(&window, &blocked)
    }

    /// The reached point on layer `l` nearest `p`, within `max` mm of it: `p` itself, or a point
    /// just off the edge of what is not reached.
    pub fn nearest(&self, p: Pt, l: usize, max: f64) -> Option<Pt> {
        if self.reached(p, l) {
            return Some(p);
        }
        nearest_off(&self.blocked[l], p, max, |s| self.reached(s, l))
    }

    /// Whether nothing of what keeps a support off or what it does not reach comes near `p` on
    /// layer `l`: the broad phase alone, so a `false` asks nothing more.
    pub fn clear(&self, p: Pt, l: usize) -> bool {
        self.broad.clear(p, l)
    }

    /// Whether a support at `p` on layer `l` stands on the part there.
    pub fn rests_on(&self, p: Pt, l: usize) -> bool {
        inside(&self.rests[l], p)
    }

    pub fn layers(&self) -> usize {
        self.blocked.len()
    }

    /// m_l (mm): how far layer `l` may lie past the one under it.
    pub fn lean(&self, l: usize) -> f64 {
        self.lean.get(l).copied().unwrap_or(0.0)
    }

    /// What of `set` is reached on layer `l`.
    pub fn reached_of(&self, l: usize, set: Vec<ExPolygon>) -> Vec<ExPolygon> {
        if set.is_empty() {
            return set;
        }
        let bb = bbox_of(&set);
        let blocked: Vec<ExPolygon> = self.blocked[l].iter().filter(|e| e.bbox().overlaps(&bb)).cloned().collect();
        less(&set, &blocked)
    }

    /// K_l: what keeps a support's centre off on layer `l`.
    pub fn keep(&self, l: usize) -> &[ExPolygon] {
        &self.keep[l]
    }

    /// B_l: where a support's centre on layer `l` does not get down from, what keeps it off
    /// there included.
    pub fn blocked(&self, l: usize) -> &[ExPolygon] {
        &self.blocked[l]
    }

    /// How a support at `p` on layer `l` gets down: its centre on each layer from `l` down to
    /// its footing (the bed, or a part top it stands on), each within its layer's lean of the
    /// one over it. `None` when `p` is not reached.
    pub fn route(&self, p: Pt, l: usize) -> Option<Vec<Pt>> {
        if !self.reached(p, l) {
            return None;
        }
        let mut out = vec![p];
        for j in (1..=l).rev() {
            let at = out[out.len() - 1];
            if inside(&self.rests[j], at) {
                break;
            }
            out.push(self.nearest(at, j - 1, self.lean[j] + WITNESS / SCALE)?);
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_slicer::clip::intersection_ex;

    /// A layer's lean at 0.2 mm layers and 40 degrees (mm).
    const LEAN: f64 = 0.168;
    /// How far past its layer's lean a route step may be (mm): the rounding, not a cell.
    const STEP_SLACK: f64 = 1e-4;

    fn reach(keep: &[Vec<ExPolygon>], lean: &[f64]) -> Reach {
        Reach::new(Ctl::default(), keep, &[], lean).unwrap()
    }

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon::new(vec![Pt::from_mm(x0, y0), Pt::from_mm(x1, y0), Pt::from_mm(x1, y1), Pt::from_mm(x0, y1)]))
    }

    fn quad(p: [(f64, f64); 4]) -> ExPolygon {
        ExPolygon::new(Polygon::new(p.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect()))
    }

    fn shifted(set: &[ExPolygon], t: f64) -> Vec<ExPolygon> {
        set.iter().map(|e| { let mut e = e.clone(); e.translate(Pt::from_mm(t, t)); e }).collect()
    }

    /// A post at the origin on every layer: the corner the broad phase's grid is laid from, so
    /// what is moved moves against the grid.
    fn pin() -> ExPolygon {
        rect(0.0, 0.0, 1.0, 1.0)
    }

    /// A room 30 mm square at (20, 20), moved by (t, t): 2 mm walls on a floor under layer 5, up
    /// to layer 80, with `cuts` taken out of its walls on layers 10 to 60. What keeps a support's
    /// centre off, per layer, with the pin.
    fn room(t: f64, cuts: &[ExPolygon], n: usize) -> Vec<Vec<ExPolygon>> {
        let (lo, hi) = (20.0, 50.0);
        let walls = difference_ex(&[rect(lo, lo, hi, hi)], &[rect(lo + 2.0, lo + 2.0, hi - 2.0, hi - 2.0)]);
        let open = if cuts.is_empty() { walls.clone() } else { difference_ex(&walls, cuts) };
        (0..n).map(|l| {
            let at = if l < 5 { vec![rect(lo, lo, hi, hi)] } else if l >= 80 { Vec::new() } else if (10..60).contains(&l) { open.clone() } else { walls.clone() };
            let mut set = shifted(&at, t);
            set.push(pin());
            set
        }).collect()
    }

    /// A slot at 45 degrees through the room's x+ wall, 0.3 mm across, its middle 0.18 mm from
    /// every grid cell's centre.
    fn diagonal_slot() -> ExPolygon {
        let (c, h) = (-13.75, 0.15 * std::f64::consts::SQRT_2);
        quad([(47.5, 47.5 + c - h), (50.5, 50.5 + c - h), (50.5, 50.5 + c + h), (47.5, 47.5 + c + h)])
    }

    /// A slot along x through the room's x+ wall, one bead (0.42 mm) across.
    fn bead_slot() -> ExPolygon {
        rect(47.5, 35.0, 50.5, 35.42)
    }

    fn leans(n: usize) -> Vec<f64> {
        vec![LEAN; n]
    }

    #[test]
    fn a_sub_cell_diagonal_window_is_reached() {
        let n = 90;
        let open = reach(&room(0.0, &[diagonal_slot()], n), &leans(n));
        let shut = reach(&room(0.0, &[], n), &leans(n));
        // 2 mm in from where the slot comes into the room (48, 34.25), 15 layers after the window
        // shuts.
        let inside = Pt::from_mm(46.0, 34.25);
        assert!(open.reached(inside, 75), "through the window");
        assert!(!shut.reached(inside, 75), "with no window");
        assert!(!open.reached(Pt::from_mm(23.0, 23.0), 75), "far from the window");
    }

    #[test]
    fn grid_translation_does_not_change_reach() {
        let n = 90;
        let probes: Vec<Pt> = (0..70).flat_map(|i| (0..70).map(move |j| Pt::from_mm(20.123 + 0.37 * i as f64, 20.123 + 0.37 * j as f64))).collect();
        let at = |t: f64| {
            let r = reach(&room(t, &[diagonal_slot(), bead_slot()], n), &leans(n));
            [30, 50, 70].map(|l| probes.iter().map(|p| r.reached(p.add(Pt::from_mm(t, t)), l)).collect::<Vec<bool>>())
        };
        let still = at(0.0);
        for t in [0.13, 0.26, 0.39] {
            let moved = at(t);
            for (k, l) in [30, 50, 70].iter().enumerate() {
                let differ = still[k].iter().zip(&moved[k]).filter(|(a, b)| a != b).count();
                assert_eq!(differ, 0, "moved {t} mm, layer {l}: {differ} of {} probes changed", probes.len());
            }
        }
    }

    #[test]
    fn a_thin_wall_is_not_reached_through() {
        let n = 70;
        for t in [0.0, 0.1, 0.2, 0.3, 0.4] {
            let walls = difference_ex(&[rect(30.0, 30.0, 40.0, 40.0)], &[rect(30.3, 30.3, 39.7, 39.7)]);
            let keep: Vec<Vec<ExPolygon>> = (0..n).map(|l| {
                let mut set = shifted(&if l < 5 { vec![rect(30.0, 30.0, 40.0, 40.0)] } else if l < 60 { walls.clone() } else { Vec::new() }, t);
                set.push(pin());
                set
            }).collect();
            let r = reach(&keep, &leans(n));
            assert!(r.reached(Pt::from_mm(29.5 + t, 35.0 + t), 50), "outside, moved {t}");
            for (x, y) in [(35.0, 35.0), (30.5, 35.0), (35.0, 39.5)] {
                assert!(!r.reached(Pt::from_mm(x + t, y + t), 50), "inside at ({x}, {y}), moved {t}");
            }
        }
    }

    #[test]
    fn a_one_bead_slot_is_reached_at_any_phase() {
        let n = 80;
        for k in 0..10 {
            let t = 0.05 * k as f64;
            let r = reach(&room(t, &[bead_slot()], n), &leans(n));
            assert!(r.reached(Pt::from_mm(46.0 + t, 35.21 + t), 70), "2 mm in from the slot, moved {t}");
        }
    }

    #[test]
    fn every_route_step_is_within_its_layer_reach() {
        let n = 90;
        let mut keep = room(0.0, &[bead_slot()], n);
        for (l, set) in keep.iter_mut().enumerate() {
            if l < 10 {
                set.push(rect(55.0, 20.0, 75.0, 60.0));
            }
        }
        // 0.2 and 0.1 mm layers by turns.
        let lean: Vec<f64> = (0..n).map(|l| if l % 2 == 0 { LEAN } else { LEAN / 2.0 }).collect();
        let r = reach(&keep, &lean);
        let mut routed = 0;
        for l in [15, 40, 70] {
            for i in 0..56 {
                for j in 0..40 {
                    let p = Pt::from_mm(18.07 + 1.1 * i as f64, 18.07 + 1.1 * j as f64);
                    if !r.reached(p, l) {
                        continue;
                    }
                    let route = r.route(p, l).unwrap_or_else(|| panic!("no route from {p:?} at layer {l}"));
                    assert_eq!(route.len(), l + 1, "down to the bed from {p:?} at layer {l}");
                    for (k, w) in route.windows(2).enumerate() {
                        let j = l - k;
                        let d = w[0].dist(w[1]) / SCALE;
                        assert!(d <= lean[j] + STEP_SLACK, "from {p:?} at layer {l}: the step to layer {} is {d:.4} mm, its lean {:.4}", j - 1, lean[j]);
                        assert!(r.reached(w[1], j - 1), "from {p:?} at layer {l}: the step to layer {} is not reached", j - 1);
                    }
                    routed += 1;
                }
            }
        }
        assert!(routed > 1000, "{routed} routes");
    }

    /// A slab on the bed, 20 by 40 mm and 10 layers thick: over its middle a support gets out
    /// from over it only with the layers to lean that far, and off the bed only, on it at once.
    #[test]
    fn a_support_leans_out_from_over_the_part_as_far_as_its_layers_let_it() {
        let slab = rect(0.0, 0.0, 20.0, 40.0);
        let off: Vec<Vec<ExPolygon>> = (0..80).map(|l| if l < 10 { vec![slab.clone()] } else { Vec::new() }).collect();
        let lean = vec![0.2; 80];
        let bed = reach(&off, &lean);
        let at = |x: f64, y: f64| Pt::from_mm(x, y);
        assert!(bed.reached(at(-1.0, 20.0), 0) && bed.reached(at(-1.0, 20.0), 15), "beside the slab it stands on the bed");
        assert!(!bed.reached(at(10.0, 20.0), 15), "6 layers over the slab's middle are too few to lean 10 mm out");
        assert!(bed.reached(at(10.0, 20.0), 70), "60 layers over it are enough");
        // 6 layers over it, 1.2 mm in from its edge is as far in as it leans from.
        assert!(bed.reached(at(1.1, 20.0), 15) && !bed.reached(at(1.3, 20.0), 15), "it leans out a layer's lean a layer");
        let q = bed.nearest(at(10.0, 20.0), 15, 20.0).expect("the slab's edge is near");
        let d = q.dist(at(10.0, 20.0)) / SCALE;
        assert!((d - 8.8).abs() < 1e-3, "the nearest it gets down from is in from the edge by the lean: {d} mm");
        let over = intersection_ex(&bed.area(15, &slab.bbox()), std::slice::from_ref(&slab));
        let band = 20.0 * 40.0 - 17.6 * 37.6;
        assert!((total_area_mm2(&over) - band).abs() < 0.1, "only a band round the slab's edge: {} mm2", total_area_mm2(&over));
        // Not from the bed only, on the slab's top: its last layer under the first over it.
        let top: Vec<Vec<ExPolygon>> = (0..80).map(|l| if l == 10 { vec![slab.clone()] } else { Vec::new() }).collect();
        let anywhere = Reach::new(Ctl::default(), &off, &top, &lean).unwrap();
        assert!(anywhere.reached(at(10.0, 20.0), 10) && anywhere.reached(at(10.0, 20.0), 15), "it stands on the slab");
        assert_eq!(anywhere.route(at(10.0, 20.0), 15).map(|r| r.len()), Some(6), "down to the slab's top");
    }

    /// A shelf in a cage: walls on a floor round a shelf high up, with a window in one wall under
    /// it. Under the shelf, a support gets down through the window from as far in as it leans from
    /// over the layers between; from further in, or with no window, from nowhere.
    #[test]
    fn a_shelf_in_a_cage_is_reached_through_its_window() {
        let (lo, hi) = (0.0, 30.0);
        let room = rect(lo + 2.0, lo + 2.0, hi - 2.0, hi - 2.0);
        let cage = |l: usize, window: bool| -> Vec<ExPolygon> {
            if l < 5 || (55..58).contains(&l) {
                return vec![rect(lo, lo, hi, hi)];
            }
            let walls = difference_ex(&[rect(lo, lo, hi, hi)], std::slice::from_ref(&room));
            if window && (20..50).contains(&l) { difference_ex(&walls, &[rect(hi - 3.0, 10.0, hi + 1.0, 20.0)]) } else { walls }
        };
        for window in [false, true] {
            let off: Vec<Vec<ExPolygon>> = (0..70).map(|l| cage(l, window)).collect();
            let f = reach(&off, &[0.5; 70]);
            assert!(f.reached(Pt::from_mm(hi + 2.0, 15.0), 54), "outside the cage it stands on the bed");
            // Under the shelf, 34 layers over the window's sill at 0.5 mm a layer: 2 mm in from the
            // wall it leans out through the window, 20 mm in is further than it leans.
            assert_eq!(f.reached(Pt::from_mm(hi - 4.0, 15.0), 54), window, "under the shelf, just in from the window: window {window}");
            assert!(!f.reached(Pt::from_mm(lo + 8.0, 15.0), 54), "under the shelf, far from the window: window {window}");
            let under = intersection_ex(&f.area(54, &room.bbox()), std::slice::from_ref(&room));
            assert_eq!(total_area_mm2(&under) > 5.0, window, "what of the shelf's underside is reached: {} mm2, window {window}", total_area_mm2(&under));
        }
    }
}

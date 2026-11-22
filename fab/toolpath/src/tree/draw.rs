//! Drawing (R14 §8): each tree's elements become tubes (§8.1, §8.2), the tubes are cut at the
//! layers' middles less the part (§8.3), roots off the bed get floors or drop (§8.4), and each
//! layer's pieces are joined, smoothed outward and split into its base and floors (§8.5). What a
//! base layer prints as is §8.6, and the first layer's pad §8.7.
//!
//! Quirks of the reference are copied for parity and marked where they are: D7, a drop's floors
//! filed a layer low (§8.4); D8, a fork's cap cut up to the print z under its pole (§8.3); and D9,
//! an anchor tail's first piece left uncharged (§8.6).
//!
//! Departures: a branch is never drawn narrower below than above ([`Element::width`], §8.2);
//! D14, D16 and D17, where the part cuts a branch's slice or its tube folds ([`cut`], §8.3); D18,
//! the smoothed base keeps out of the part; D19, a crack's last chord with one side blocked moves
//! its far end, where the reference (D10) moves the blocked one (§8.5); and D27, a branch that
//! leaves its fork flatter than one in [`LEVEL_RUN`] leaves it level ([`tube`], §8.2).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::{offset_polygons, Join};

use super::clip::{difference_ex, in_island_order, intersection_ex, union, union2_ex, union_ex, FillRule};
use super::element::{simplified, Element};
use super::offset::{as_engine_takes, offset, sparse, NEAR_SHARE};
use super::volumes::{reading, Reads, Volumes};
use crate::par::{Cancelled, Ctl};
use crate::{flow, ExtrusionPath, Role};

/// How near (mm) a ring's edges stay to its circle, and a cap's steps to its sphere (§8.2).
const RING_TOL: f64 = 0.015;
/// D27, Fab's: a branch whose first step from its fork runs more than this many times what it
/// rises leaves the fork level ([`tube`]), as a merge may leave a narrow branch near a wide one's
/// rim (§5.3). A steeper first step keeps the reference's facing.
const LEVEL_RUN: f64 = 5.0;
/// A root that does not stand on the model drops at most this many of its radii (§8.4).
const DROP_RADII: i64 = 5;
/// A drop stops before a remainder under the larger of these shares of π r² at the root's radius
/// and at r_br (§8.4).
const DROP_ROOT_SHARE: f64 = 0.2;
const DROP_BRANCH_SHARE: f64 = 0.5;
/// The simplification after smoothing (mm) is the resolution, at most this (§8.5).
const SIMPLIFY_MAX: f64 = 0.03;
/// Before smoothing, a point nearer than this (mm) to the last point kept goes (§8.5).
const SMOOTH_NEAR: f64 = 0.01;
/// A concave corner whose edges meet at less than this (radians) is cut (§8.5).
const SHARP: f64 = 0.75 * std::f64::consts::PI;
/// Within this (grid units: 100 nm) of the clip distance, or of the corner's bisector, smoothing
/// only takes the corner out (§8.5).
const SMOOTH_SLACK: f64 = 100.0;
/// An island of a base layer over this area (mm²) prints two walls (§8.6): π · 2.5².
pub const TWO_WALLS_AREA: f64 = std::f64::consts::PI * 2.5 * 2.5;
/// Every island of a base layer whose top is above this height (mm) prints two walls (§8.6).
pub const TWO_WALLS_ABOVE: f64 = 100.0;
/// A base layer's islands are its polygons grown by this (mm), then shrunk by it and half a line
/// (§8.6 step 1); a pad's sheath likewise (§8.7).
const ISLAND_EPS: f64 = 0.0001;
/// The second wall's region: the island less this many lines, grown back by the next (§8.6 step 3).
const SECOND_IN: f64 = 1.5;
const SECOND_OUT: f64 = 0.5;
/// A loop ends this many spacings short of its start (§8.6 step 4).
const LOOP_END_CUT: f64 = 0.15;
/// An anchor tail runs on the loop's region less a line, mitred at this limit (§8.6 step 5), which
/// the engine takes as 2 (§1.5).
const TAIL_MITRE: f64 = 1.2;
/// A tail is used only if it comes within this many lines of the loop's start (§8.6 step 5).
const TAIL_REACH: f64 = 3.0;
/// A tail and its step out to the loop are this many spacings long (§8.6 step 5), less the
/// uncharged piece of quirk D9.
const TAIL_LENGTH: f64 = 6.0;
/// A tail's nearest point within this share of its segment's length of an end is on that end
/// (§8.6 step 5).
const FOOT_END: f64 = 1e-4;
/// The first layer's pad grows this much (mm) past the feet (§8.7), in at least this many steps.
const PAD_GROWTH: f64 = 2.0;
const PAD_STEPS: usize = 5;
/// The pad's fill keeps this many spacings inside its sheath, and the filler's outer set this many
/// more (§8.7).
const PAD_FILL_IN: f64 = 0.4;
const PAD_END_IN: f64 = 0.05;

/// What the drawing gives per layer (§8.5): the base, and the floors (the bottom contacts of
/// branches standing on the model).
#[derive(Clone, Debug, Default)]
pub struct Drawn {
    pub base: Vec<Vec<ExPolygon>>,
    pub floor: Vec<Vec<ExPolygon>>,
}

/// A run of elements between forks (§8.1), from its bottom up, as (layer, index on the layer).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub path: Vec<(usize, usize)>,
    /// It starts at its tree's root ("has root"), not at a fork.
    pub root: bool,
    /// It ends at a tip, an element that carries nothing ("has tip"), not at a fork.
    pub tip: bool,
}

/// The branches of `trees` (§8.1): per layer the elements, each carrying indices into the layer
/// over it; only placed ones are drawn (placement deletes the others, §6). A tree starts at an
/// element nothing carries that carries something, layer 0 first and each layer in its order;
/// an element that neither carries nor is carried is not drawn. From a start, one branch goes up
/// through each element it carries, in carry order, while each carries exactly one, to a tip or
/// the next fork. That fork is the top of this branch and the bottom of each of its own, which
/// come next, depth first. Every branch from the root has root, all of them when the root forks.
pub fn branches(trees: &[Vec<Element>]) -> Vec<Branch> {
    let placed = |l: usize, i: usize| trees[l][i].placed.is_some();
    // The placed elements a placed element carries.
    let up = |l: usize, i: usize| -> Vec<usize> {
        if l + 1 >= trees.len() || !placed(l, i) {
            return Vec::new();
        }
        trees[l][i].carries.iter().copied().filter(|&c| placed(l + 1, c)).collect()
    };
    let mut carried: Vec<Vec<bool>> = trees.iter().map(|t| vec![false; t.len()]).collect();
    for l in 0..trees.len() {
        for i in 0..trees[l].len() {
            for c in up(l, i) {
                carried[l + 1][c] = true;
            }
        }
    }
    let mut out = Vec::new();
    for l in 0..trees.len() {
        for i in 0..trees[l].len() {
            if !placed(l, i) || carried[l][i] {
                continue;
            }
            // The starts with branches still to draw, each with the elements it has yet to go up
            // through.
            let mut starts = vec![((l, i), true, up(l, i).into_iter())];
            while let Some((start, root, rest)) = starts.last_mut() {
                let Some(c) = rest.next() else {
                    starts.pop();
                    continue;
                };
                let (start, root) = (*start, *root);
                let mut path = vec![start, (start.0 + 1, c)];
                loop {
                    let (pl, pi) = *path.last().unwrap();
                    let next = up(pl, pi);
                    if next.len() == 1 {
                        path.push((pl + 1, next[0]));
                        continue;
                    }
                    let tip = next.is_empty();
                    out.push(Branch { path, root, tip });
                    if !tip {
                        starts.push(((pl, pi), false, next.into_iter()));
                    }
                    break;
                }
            }
        }
    }
    out
}

/// A point or direction in 32-bit floats, as the reference builds its tubes (§8.2).
type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn mul(a: V3, k: f32) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn unit(a: V3) -> V3 {
    mul(a, 1.0 / dot(a, a).sqrt())
}

/// The corners of a ring of radius `r` (mm) whose edges stay within [`RING_TOL`] of its circle
/// (§8.2): ⌈2π ÷ (2 acos(1 − 0.015 ÷ r))⌉.
fn sides(r: f64) -> usize {
    debug_assert!(r > RING_TOL / 2.0, "a ring of {r} mm");
    (std::f64::consts::PI / (1.0 - RING_TOL / r).acos()).ceil() as usize
}

/// The steps from a cap's rim to its pole at radius `r` (mm) (§8.2): ⌈π ÷ (2 · 2 acos(1 − 0.015
/// ÷ r))⌉, each a quarter turn over that.
fn cap_steps(r: f64) -> usize {
    (std::f64::consts::PI / (4.0 * (1.0 - RING_TOL / r).acos())).ceil() as usize
}

/// A branch's tube (§8.2): its vertices (mm, 32-bit floats) in the reference's numbering, which
/// the cut works from (§8.3), and its triangles, anticlockwise seen from outside; and its axis,
/// the line every ring and cap ring is centred on, from the bottom pole (the bed under a foot)
/// through the elements' points to the top pole, rising all the way.
#[derive(Default)]
struct Tube {
    verts: Vec<V3>,
    tris: Vec<[u32; 3]>,
    axis: Vec<V3>,
}

impl Tube {
    /// Where the axis crosses the height `z` (mm); past an end, at that end.
    fn axis_at(&self, z: f32) -> Pt {
        let a = &self.axis;
        let k = a.partition_point(|p| p[2] < z).clamp(1, a.len() - 1);
        let (p, q) = (a[k - 1], a[k]);
        let t = if q[2] > p[2] { ((z - p[2]) / (q[2] - p[2])).clamp(0.0, 1.0) } else { 0.0 };
        Pt::from_mm((p[0] + (q[0] - p[0]) * t) as f64, (p[1] + (q[1] - p[1]) * t) as f64)
    }
}

fn vertex(mesh: &mut Tube, p: V3) -> u32 {
    mesh.verts.push(p);
    (mesh.verts.len() - 1) as u32
}

/// A ring of radius `r` (mm) round `centre`, square to `n` (§8.2), with its own corner count:
/// corner 0 along n × (0, −1, 0), the corners turning towards n × that (anticlockwise seen from
/// above when n is up), the angle summed in 32-bit floats.
fn ring(mesh: &mut Tube, centre: V3, r: f32, n: V3) -> Vec<u32> {
    let u = unit([n[2], 0.0, -n[0]]);
    let w = cross(n, u);
    let k = sides(r as f64);
    let step = std::f32::consts::TAU / k as f32;
    let mut a = 0.0_f32;
    let mut out = Vec::with_capacity(k);
    for _ in 0..k {
        out.push(vertex(mesh, add(centre, add(mul(u, r * a.cos()), mul(w, r * a.sin())))));
        a += step;
    }
    out
}

/// The triangles between ring `lo` and the ring `hi` over it (§8.2): lo's first corner pairs with
/// hi's nearest (the first of equals); then one triangle a step, lo advancing when its next
/// corner is strictly nearer hi's current one than hi's next is to lo's current, else hi, and
/// only the other once one has gone round. Squared distances in 32-bit floats.
fn strip(mesh: &mut Tube, lo: &[u32], hi: &[u32]) {
    let at = |i: u32| -> V3 { mesh.verts[i as usize] };
    let d2 = |a: V3, b: V3| {
        let d = sub(a, b);
        dot(d, d)
    };
    let (n, m) = (lo.len(), hi.len());
    let p0 = at(lo[0]);
    let mut j0 = 0;
    for j in 1..m {
        if d2(p0, at(hi[j])) < d2(p0, at(hi[j0])) {
            j0 = j;
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut tris = Vec::with_capacity(n + m);
    while i < n || j < m {
        let (a, b) = (lo[i % n], hi[(j0 + j) % m]);
        let (a1, b1) = (lo[(i + 1) % n], hi[(j0 + j + 1) % m]);
        if j == m || (i < n && d2(at(a1), at(b)) < d2(at(b1), at(a))) {
            tris.push([a, a1, b]);
            i += 1;
        } else {
            tris.push([a, b1, b]);
            j += 1;
        }
    }
    mesh.tris.extend(tris);
}

/// A branch's tube (§8.2) through `rings` (each element's placed point at its layer's print z,
/// and its drawn radius: x, y, z and r in mm, from the bottom up), with the z (mm) of its bottom
/// and top caps' poles. Each element's ring is square to the normalized sum of the unit
/// directions into and out of it, an end ring to its one segment. Over the last ring is a
/// hemisphere in [`cap_steps`]; under the first, the same without its rim, so the mesh runs from
/// the cap's last ring straight to the second element's. With `foot`, the first element stands
/// on the bed instead: its ring level, over a level ring on the bed. Every cap ring faces up the
/// branch, as its end ring does. With `level` (D27), the first segment faces straight up
/// wherever a facing is taken from it: the bottom cap faces straight down, its pole the tube's
/// lowest point, and the second element's ring is square to up and the segment over it. The
/// vertices are numbered as the reference numbers them: the bed's centre and the bed ring, or the
/// bottom pole and the cap's rings from it; the elements' rings; the top cap's from its rim; the
/// top pole.
fn tube(rings: &[[f64; 4]], foot: bool, level: bool) -> (Tube, f64, f64) {
    let k = rings.len();
    debug_assert!(k >= 2, "a branch has a start and one element over it");
    let c: Vec<V3> = rings.iter().map(|r| [r[0] as f32, r[1] as f32, r[2] as f32]).collect();
    let rad: Vec<f32> = rings.iter().map(|r| r[3] as f32).collect();
    let up: V3 = [0.0, 0.0, 1.0];
    let seg: Vec<V3> = (1..k).map(|i| if i == 1 && level { up } else { unit(sub(c[i], c[i - 1])) }).collect();
    let normal = |i: usize| -> V3 {
        match i {
            0 if foot => up,
            0 => seg[0],
            _ if i == k - 1 => seg[k - 2],
            _ => unit(add(seg[i - 1], seg[i])),
        }
    };
    let quarter = std::f32::consts::FRAC_PI_2;
    let mut mesh = Tube::default();
    let mut stack: Vec<Vec<u32>> = Vec::new();
    let (bottom, low) = if foot {
        let bed = [c[0][0], c[0][1], 0.0];
        let centre = vertex(&mut mesh, bed);
        mesh.axis.push(bed);
        stack.push(ring(&mut mesh, bed, rad[0], up));
        (centre, 0.0)
    } else {
        let (d, r) = (seg[0], rad[0]);
        let p = sub(c[0], mul(d, r));
        let pole = vertex(&mut mesh, p);
        mesh.axis.push(p);
        let m = cap_steps(r as f64);
        let step = quarter / m as f32;
        for j in 1..m {
            let t = j as f32 * step;
            stack.push(ring(&mut mesh, sub(c[0], mul(d, r * t.cos())), r * t.sin(), d));
        }
        (pole, p[2] as f64)
    };
    // Without a foot the first element has no ring of its own: its cap stops a step short.
    let start = if foot { 0 } else { 1 };
    for i in start..k {
        stack.push(ring(&mut mesh, c[i], rad[i], normal(i)));
    }
    let (d, r) = (seg[k - 2], rad[k - 1]);
    let m = cap_steps(r as f64);
    let step = quarter / m as f32;
    for j in (1..m).rev() {
        let t = j as f32 * step;
        stack.push(ring(&mut mesh, add(c[k - 1], mul(d, r * t.cos())), r * t.sin(), d));
    }
    let p = add(c[k - 1], mul(d, r));
    let top = vertex(&mut mesh, p);
    mesh.axis.extend_from_slice(&c);
    mesh.axis.push(p);
    let first = &stack[0];
    for j in 0..first.len() {
        mesh.tris.push([bottom, first[(j + 1) % first.len()], first[j]]);
    }
    for w in 1..stack.len() {
        strip(&mut mesh, &stack[w - 1], &stack[w]);
    }
    let last = &stack[stack.len() - 1];
    for j in 0..last.len() {
        mesh.tris.push([last[j], last[(j + 1) % last.len()], top]);
    }
    (mesh, low, p[2] as f64)
}

/// The height (mm) each layer is cut at (§8.3): half way between its print z and the one under
/// it, the bed's 0 under layer 0.
fn planes(heights: &[(f64, f64)]) -> Vec<f64> {
    (0..heights.len()).map(|l| (heights[l].0 + if l > 0 { heights[l - 1].0 } else { 0.0 }) / 2.0).collect()
}

/// The layers a branch is cut on (§8.3), from its first element's layer `first` to its last's
/// `last`, with its caps' poles at `low` and `high` (mm), over `print` (each layer's print z, mm),
/// the layers `lh` (mm) apart from the first. A branch from the root starts on the root's layer,
/// any other on the lowest layer whose print z is at or over its bottom pole, ⌈(low − z0) ÷ lh⌉
/// with z0 the first layer's print z. A branch to a tip ends on the tip's layer, any other on the
/// highest whose print z is at or under its top pole, ⌊(high − z0) ÷ lh⌋. Both in doubles and at
/// least layer 0, as the reference has them.
fn span(b: &Branch, first: usize, last: usize, low: f64, high: f64, print: &[f64], lh: f64) -> (usize, usize) {
    let z0 = print[0];
    let from = if b.root { first } else { first.min(((low - z0) / lh).ceil().max(0.0) as usize) };
    // D8 (§8.3, copied for parity): the pole is held against the print z, not the plane, so a
    // layer whose plane is under the pole but whose print z is over it is not cut.
    let to = if b.tip { last } else { last.max((((high - z0) / lh).floor().max(0.0) as usize).min(print.len() - 1)) };
    (from, to)
}

/// The bed's area (§8.3, §8.5, §9.4), and its box when the area is that box. The trees' bed is
/// always a box today (the job's printable area slid round the part, `bed_area` in the crate
/// root); any other area has no box here, and nothing is held on it. A branch's slice inside the
/// box is on the bed as it is ([`Bed::clip`]); the layers go on it through the engine's
/// intersection ([`Bed::intersect`]), which hands a set inside the box on the same, ring for ring,
/// whatever bed's box holds it.
pub(super) struct Bed<'a> {
    area: &'a [ExPolygon],
    boxed: Option<BBox>,
}

impl<'a> Bed<'a> {
    pub(super) fn new(area: &'a [ExPolygon]) -> Self {
        let boxed = match area {
            [e] if e.holes.is_empty() => {
                let b = e.bbox();
                let box2 = 2 * (b.max.x - b.min.x) as i128 * (b.max.y - b.min.y) as i128;
                (e.contour.area2() == box2).then_some(b)
            }
            _ => None,
        };
        Bed { area, boxed }
    }

    /// Whether the bed is its box and that box holds `b`: never on a bed of another shape.
    pub(super) fn holds(&self, b: &BBox) -> bool {
        self.boxed.is_some_and(|x| b.min.x >= x.min.x && b.min.y >= x.min.y && b.max.x <= x.max.x && b.max.y <= x.max.y)
    }

    /// `set` on the bed, as it is when the bed's box holds it: `held` takes in its box when the
    /// bed's box holds it, and is none once the bed cuts a set.
    pub(super) fn clip(&self, set: Vec<ExPolygon>, held: &mut Option<BBox>) -> Vec<ExPolygon> {
        if set.is_empty() || self.hold(&set, held) { set } else { intersection_ex(&set, self.area) }
    }

    /// `set` on the bed as the engine's intersection with it hands it on (§1.6, §9.4), every ring
    /// by Rule A and the islands in their order, a set the bed's box holds too: `held` as
    /// [`Bed::clip`] keeps it.
    pub(super) fn intersect(&self, set: Vec<ExPolygon>, held: &mut Option<BBox>) -> Vec<ExPolygon> {
        if set.is_empty() {
            return set;
        }
        self.hold(&set, held);
        intersection_ex(&set, self.area)
    }

    /// Whether the bed's box holds `set`, `held` taking in its box if it does and none if not.
    fn hold(&self, set: &[ExPolygon], held: &mut Option<BBox>) -> bool {
        let b = bbox_of(set);
        let holds = self.holds(&b);
        if !holds {
            *held = None;
        } else if let Some(h) = held {
            h.merge(&b);
        }
        holds
    }
}

/// `a` less `b`.
fn less(a: Vec<ExPolygon>, b: &[ExPolygon]) -> Vec<ExPolygon> {
    if a.is_empty() || b.is_empty() { a } else { difference_ex(&a, b) }
}

/// A branch's slices (§8.3): from layer `first` up, with the empty layers at both ends taken off,
/// and per slice whether the part or the lean cut it (D14); the layers it was cut on; and their
/// box when the bed's box held each as it was ([`Bed::clip`]).
struct Cut {
    first: usize,
    slices: Vec<Vec<ExPolygon>>,
    cut: Vec<bool>,
    span: (usize, usize),
    held: Option<BBox>,
}

impl Cut {
    /// Its slice on layer `l`, nothing off its ends, and whether the part or the lean cut it.
    fn at(&self, l: usize) -> (&[ExPolygon], bool) {
        match l.checked_sub(self.first).filter(|&k| k < self.slices.len()) {
            Some(k) => (self.slices[k].as_slice(), self.cut[k]),
            None => (&[], false),
        }
    }

    /// Its slice on layer `l` when the part or the lean cut it there (D14), and that is not
    /// nothing.
    fn cut_on(&self, l: usize) -> Option<&[ExPolygon]> {
        Some(self.at(l)).filter(|(s, cut)| *cut && !s.is_empty()).map(|(s, _)| s)
    }

    /// D28: the id of its slices the part or the lean cut under the layers `arm` was cut on, by
    /// layer, when it has any: what `arm`, a branch from the fork this one ends at, is cut on.
    fn under(&self, arm: &Cut) -> Option<Id> {
        let layers = arm.span.0.saturating_sub(1).max(self.first)..arm.span.1.min(self.first + self.slices.len());
        let cut: Vec<(usize, Rings)> = layers.filter_map(|l| self.cut_on(l).map(|s| (l, Rings(s)))).collect();
        (!cut.is_empty()).then(|| id_of(&cut))
    }
}

/// D28: what a branch from a fork is cut again on: the cut of the branch under the fork, and its
/// own cut on its own, by which it ends.
#[derive(Clone, Copy)]
struct Again<'a> {
    lower: &'a Cut,
    alone: &'a Cut,
}

/// The tube's x and y are cut in nm, scaled by this 32-bit float (§8.3).
const CUT_SCALE: f32 = 1_000_000.0;
/// Chains still open after the exact joins are closed over gaps up to this (mm) (§8.3).
const CUT_GAP: f64 = 2.0;
/// D14: a slice that loses more than this (mm2) to the part, or to its lean, is cut.
const CUT_AREA: f64 = 1e-4;
/// D14: the lean's grow is round, its chords within this (mm) of the arc.
const LEAN_ARC: f64 = 0.005;

/// Where a point of a section lies on its tube (§8.3): on an edge, by its two vertices, lower
/// number first, or at a vertex in the plane. A section's pieces chain through these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum On {
    Edge(u32, u32),
    Vertex(u32),
}

/// One triangle's piece of a section (§8.3).
#[derive(Clone, Copy, Debug)]
struct Piece {
    from: (On, Pt),
    to: (On, Pt),
}

/// `mesh` cut on each of `planes` (z in mm, 32-bit floats), as the reference's own mesh slicer
/// cuts it (§8.3): per plane, the loops, in no set order and either way round.
fn sections(mesh: &Tube, planes: &[f32]) -> Vec<Vec<Polygon>> {
    // x and y in nm, z in mm, all 32-bit floats.
    let v: Vec<V3> = mesh.verts.iter().map(|p| [p[0] * CUT_SCALE, p[1] * CUT_SCALE, p[2]]).collect();
    let mut pieces: Vec<Vec<Piece>> = vec![Vec::new(); planes.len()];
    for t in &mesh.tris {
        let z = [v[t[0] as usize][2], v[t[1] as usize][2], v[t[2] as usize][2]];
        if z[0] == z[1] && z[1] == z[2] {
            continue;
        }
        let (lo, hi) = (z[0].min(z[1]).min(z[2]), z[0].max(z[1]).max(z[2]));
        for k in planes.partition_point(|&p| p < lo)..planes.partition_point(|&p| p <= hi) {
            if let Some(piece) = crossing(&v, t, z, planes[k]) {
                pieces[k].push(piece);
            }
        }
    }
    pieces.into_iter().map(chain).collect()
}

/// Triangle `t`'s piece of plane `z` (§8.3), its vertices' z `tz`. Its edges are taken from its
/// lowest vertex (the first of slots 1, 2, 0 with the least z) round. An edge in the plane is the
/// piece, if the third vertex is under it; if over, the triangle gives nothing. A vertex in the
/// plane is a point, once, truncated to whole nm. An edge crossing it gives the point from its
/// higher-numbered vertex b towards a, at t = (z − z_b) ÷ (z_a − z_b), half up to whole nm. Two
/// points make a piece, from the second to the first.
fn crossing(v: &[V3], t: &[u32; 3], tz: [f32; 3], z: f32) -> Option<Piece> {
    let mut lo = 1;
    if tz[2] < tz[lo] {
        lo = 2;
    }
    if tz[0] < tz[lo] {
        lo = 0;
    }
    let at = |w: u32| -> Pt { Pt::new(v[w as usize][0] as i64, v[w as usize][1] as i64) };
    let mut found: Vec<(On, Pt)> = Vec::with_capacity(2);
    for e in 0..3 {
        let (i, j) = ((lo + e) % 3, (lo + e + 1) % 3);
        if tz[i] == z && tz[j] == z {
            return (tz[3 - i - j] < z).then(|| Piece { from: (On::Vertex(t[j]), at(t[j])), to: (On::Vertex(t[i]), at(t[i])) });
        }
        let on = if tz[i] == z { Some(t[i]) } else if tz[j] == z { Some(t[j]) } else { None };
        if let Some(w) = on {
            if !found.iter().any(|f| f.0 == On::Vertex(w)) {
                found.push((On::Vertex(w), at(w)));
            }
        } else if (tz[i] < z) != (tz[j] < z) {
            let (a, b) = if t[i] < t[j] { (t[i], t[j]) } else { (t[j], t[i]) };
            let (pa, pb) = (v[a as usize], v[b as usize]);
            let k = (z as f64 - pb[2] as f64) / (pa[2] as f64 - pb[2] as f64);
            let half_up = |c: usize| (pb[c] as f64 + (pa[c] as f64 - pb[c] as f64) * k + 0.5).floor() as i64;
            found.push((On::Edge(a, b), Pt::new(half_up(0), half_up(1))));
        }
    }
    (found.len() == 2).then(|| Piece { from: found[1], to: found[0] })
}

/// A section's pieces chained into loops (§8.3): through the tube first, each piece's end where
/// the next one starts; then the chains still open, joined where an end meets a start exactly,
/// then also reversed; then over gaps up to [`CUT_GAP`] the same way. What stays open is dropped.
fn chain(pieces: Vec<Piece>) -> Vec<Polygon> {
    let mut starts: std::collections::HashMap<On, Vec<usize>> = std::collections::HashMap::new();
    for (k, p) in pieces.iter().enumerate() {
        starts.entry(p.from.0).or_default().push(k);
    }
    let mut used = vec![false; pieces.len()];
    let (mut loops, mut open) = (Vec::new(), Vec::new());
    for k0 in 0..pieces.len() {
        if used[k0] {
            continue;
        }
        used[k0] = true;
        let mut pts = vec![pieces[k0].from.1];
        let mut at = k0;
        let closed = loop {
            let end = pieces[at].to;
            if end.0 == pieces[k0].from.0 {
                break true;
            }
            pts.push(end.1);
            match starts.get(&end.0).and_then(|ks| ks.iter().copied().find(|&k| !used[k])) {
                Some(k) => {
                    used[k] = true;
                    at = k;
                }
                None => break false,
            }
        };
        if closed { loops.push(pts) } else { open.push(pts) }
    }
    for gap in [0.0, CUT_GAP * SCALE] {
        join_open(&mut open, &mut loops, gap);
    }
    loops.into_iter().map(Polygon::new).collect()
}

/// Joins `open` chains whose ends lie within `gap` (grid units) (§8.3): each chain in turn is
/// closed on its own start, or takes on the nearest chain that starts at its end, or else the
/// nearest that ends there, reversed; until none joins. Closed ones go to `loops`.
fn join_open(open: &mut Vec<Vec<Pt>>, loops: &mut Vec<Vec<Pt>>, gap: f64) {
    'again: loop {
        for i in 0..open.len() {
            let end = *open[i].last().unwrap();
            if open[i].len() > 2 && end.dist(open[i][0]) <= gap {
                let mut done = open.swap_remove(i);
                if done.last() == done.first() {
                    done.pop();
                }
                loops.push(done);
                continue 'again;
            }
            for reversed in [false, true] {
                let mut best: Option<(f64, usize)> = None;
                for (j, c) in open.iter().enumerate() {
                    let d = end.dist(if reversed { *c.last().unwrap() } else { c[0] });
                    if j != i && d <= gap && best.is_none_or(|b| d < b.0) {
                        best = Some((d, j));
                    }
                }
                if let Some((_, j)) = best {
                    let mut more = open.swap_remove(j);
                    let i = if i == open.len() { j } else { i };
                    if reversed {
                        more.reverse();
                    }
                    if open[i].last() == more.first() {
                        more.remove(0);
                    }
                    open[i].extend(more);
                    continue 'again;
                }
            }
        }
        return;
    }
}

/// `mesh` cut on `layers` at `planes` (§8.3, 32-bit floats), with no closing: every
/// loop turned anticlockwise, so a section has no hole and a folded tube's loops all count; less
/// the part (the collision at radius 0, xy_min), on the bed. Fab's keep-out: from layer `free`
/// up, where the branch is leaving it, less the part alone.
///
/// D17, Fab's: a section the tube folds into takes in the hull of its loops as far as the fast
/// move from the branch's slice under it. Where a wide branch bends or narrows, its rings, square
/// to it, cross the ones beside them and the tube folds: its loops unite into more than one island
/// (a horn beside the section, apart from it across a sliver of nothing), or into one with a hole
/// (a crack, with a sliver between it and the edge), or into one whose area is not the loops' own
/// (where they overlap). Horn, crack and sliver end a layer or a few up holding nothing, or print
/// narrower than two lines. The hull takes them in; the fast move keeps it from growing the branch
/// past the steep angle (R2). A section that is one simple loop stays as it is: its hull would
/// only fill the dents along its edge, and move the corners §8.5's smoothing cuts.
///
/// D14, Fab's: a slice over one the part cut keeps only what lies within the fast move of it,
/// and so on up while that cuts, so where the part lets a branch go it grows back a lean a layer
/// (R2), not all at once. It also keeps what lies in that slice moved as the branch moves (by
/// its axis, from plane to plane), so a branch the part cuts as it moves keeps what the part
/// leaves it, however fast it moves. Not by its section's middle: that jumps where the section's
/// shape changes, as where the rim of a cap, rising behind a leaning branch's end, ends.
///
/// D16, Fab's: what a cut leaves narrower than two lines does not print (R6), unless that is all
/// of it: then it stays, as the branch over it stands on it. The fast move is from the slice that
/// prints; what moves with the branch is the slice as the part left it, or the opening's arcs,
/// taken again each layer, would wear a moving branch's cut edge back a little each layer.
///
/// D28, Fab's: a branch from a fork cut `again` stands on the cut of the branch under the fork.
/// Over a slice of that one the part or its lean cut, the branch keeps only what lies within the
/// fast move of both slices under, its own and that one, so a fork's branches grow back with the
/// branch under them: cut on their own, their bottom caps would hang ahead of it (R2). Under the
/// first of that one's cut slices it stands on, it is its cut on its own: it is cut from the
/// highest layer there whose slice under that one left uncut, or its first, and has those under
/// it, and their box, as that one. Past the last, the first layer it comes out of uncut and as
/// that one has it, it goes on as that one: from there both cut the same.
fn cut(ctl: Ctl, v: &Volumes, planes: &[f32], bed: &Bed, mesh: &Tube, layers: std::ops::RangeInclusive<usize>, free: Option<usize>, again: Option<Again<'_>>) -> Result<Cut, Cancelled> {
    let (from, to) = (*layers.start(), *layers.end());
    let lean = v.settings().fast * SCALE;
    let mut slices: Vec<Vec<ExPolygon>> = Vec::with_capacity(to + 1 - from);
    let mut cuts: Vec<bool> = Vec::with_capacity(to + 1 - from);
    let mut held = Some(BBox::empty());
    // D28: the layers under this branch's whose slices of the lower branch were cut, and the
    // layer it is cut from.
    let lows: Vec<usize> = again.map_or_else(Vec::new, |a| (from.saturating_sub(1)..to).filter(|&k| a.lower.cut_on(k).is_some()).collect());
    let mut start = from;
    if let (Some(a), Some(&first)) = (again, lows.first()) {
        start = first + 1;
        while start > from && a.alone.at(start - 1).1 {
            start -= 1;
        }
        for k in from..start {
            let (slice, cut) = a.alone.at(k);
            slices.push(slice.to_vec());
            cuts.push(cut);
        }
        held = a.alone.held;
    }
    // Whether the slice under was cut, by the part or by its lean, where the axis crossed its
    // plane, and what the part and the lean left of it, before D16.
    let (mut was_cut, mut was_at, mut left) = (false, Pt::new(0, 0), Vec::<ExPolygon>::new());
    for (l, mut loops) in (start..=to).zip(sections(mesh, &planes[start..=to])) {
        loops.retain(|p| p.len() >= 3);
        for p in &mut loops {
            if p.area2() < 0 {
                p.reverse();
            }
        }
        let part = if free.is_some_and(|f| l >= f) { v.free() } else { v };
        if loops.is_empty() {
            slices.push(Vec::new());
            cuts.push(false);
            was_cut = false;
            continue;
        }
        let mut whole = union(&loops, FillRule::NonZero);
        // D17: the tube folds here when its loops are not one simple loop's island.
        let folded = whole.len() > 1 || whole.iter().any(|e| !e.holes.is_empty()) || whole.iter().map(ExPolygon::area2).sum::<i128>() != loops.iter().map(Polygon::area2).sum::<i128>();
        if folded {
            if let Some(under) = slices.last().filter(|u| !u.is_empty()) {
                let hull = [ExPolygon::new(Polygon::new(convex_hull(&mut loops.iter().flat_map(|p| p.pts.iter().copied()).collect())))];
                whole = union2_ex(&whole, &intersection_ex(&hull, &offset(under, lean, Join::Round(LEAN_ARC * SCALE))));
            }
        }
        let here = mesh.axis_at(planes[l]);
        let mut slice = less(whole.clone(), part.collision(ctl, l, 0)?);
        // D28: the lower branch's slice under, where the part or its lean cut it.
        let low = l.checked_sub(1).zip(again).and_then(|(k, a)| a.lower.cut_on(k));
        if was_cut || low.is_some() {
            let joined;
            let under = match (slices.last().filter(|u| !u.is_empty()), low) {
                (Some(own), Some(low)) => {
                    joined = union2_ex(own, low);
                    Some(joined.as_slice())
                }
                (own, low) => own.map(Vec::as_slice).or(low),
            };
            if let Some(under) = under {
                let reach = offset(under, lean, Join::Round(LEAN_ARC * SCALE));
                slice = intersection_ex(&slice, &if was_cut {
                    let mut moved = std::mem::take(&mut left);
                    moved.iter_mut().for_each(|e| e.translate(here.sub(was_at)));
                    union2_ex(&reach, &moved)
                } else {
                    reach
                });
            }
        }
        was_at = here;
        was_cut = total_area_mm2(&slice) < total_area_mm2(&whole) - CUT_AREA;
        cuts.push(was_cut);
        if was_cut {
            let printable = crate::support_rules::wide(&slice, &[], v.settings().keys.line_width);
            left = slice.clone();
            if !printable.is_empty() {
                slice = printable;
            }
        }
        slices.push(bed.clip(slice, &mut held));
        // D28: from here on it cuts as its cut on its own does.
        if let Some(a) = again.filter(|a| !was_cut && lows.last().is_some_and(|&k| l > k) && a.alone.at(l) == (slices[l - from].as_slice(), false)) {
            for k in l + 1..=to {
                let (slice, cut) = a.alone.at(k);
                slices.push(slice.to_vec());
                cuts.push(cut);
            }
            break;
        }
    }
    let lead = slices.iter().take_while(|s| s.is_empty()).count();
    let keep = slices.len() - slices.iter().rev().take_while(|s| s.is_empty()).count();
    let (slices, cut) = if lead < keep { (slices.drain(lead..keep).collect(), cuts.drain(lead..keep).collect()) } else { (Vec::new(), Vec::new()) };
    Ok(Cut { first: from + lead, slices, cut, span: (from, to), held })
}

/// Polygons per layer, as (layer, polygons).
type Layered = Vec<(usize, Vec<ExPolygon>)>;

/// The layers a root that does not stand on the model drops through (§8.4): from `slice`, its
/// slice on its own layer `at`, down layer by layer less the collision at radius 0 with the full
/// side gap, at most ⌊5 r ÷ layer height⌋ layers (whole nm, `r` its radius in mm) or to the bed
/// when it is `stuck`; stopping before a remainder under max(0.2 π r², 0.5 π r_br²). Each is
/// (layer, remainder), from the top down.
fn drop(ctl: Ctl, v: &Volumes, at: usize, slice: &[ExPolygon], r: f64, stuck: bool) -> Result<Layered, Cancelled> {
    let s = v.settings();
    let reach = if stuck { at } else { ((DROP_RADII * scaled(r)) / scaled(s.keys.layer_height)) as usize }.min(at);
    let least = (DROP_ROOT_SHARE * r * r).max(DROP_BRANCH_SHARE * s.r_br * s.r_br) * std::f64::consts::PI;
    let mut out = Vec::new();
    let mut rest = slice.to_vec();
    for l in (at - reach..at).rev() {
        rest = less(rest, v.collision(ctl, l, s.zero_full())?);
        if total_area_mm2(&rest) < least {
            break;
        }
        out.push((l, rest.clone()));
    }
    Ok(out)
}

/// What one branch draws (§8.3, §8.4): its slices, and a root's drop and floors; and the volume
/// layers its drawing read ([`reading`]).
struct Branched {
    cut: Cut,
    down: Layered,
    floor: Layered,
    read: Reads,
}

/// A branch as its drawing reads it: per element its ring (placed x and y, print z, and width,
/// [`Element::width`], as bits), its first and last layers, whether it has root and tip, the
/// lowest layer it is leaving the keep-out on, whether its root is on top and stuck, and, cut
/// again (D28), the slices of the branch under its fork it was cut on, by their id
/// ([`Cut::under`]).
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    rings: Vec<[u64; 4]>,
    first: usize,
    last: usize,
    root: bool,
    tip: bool,
    free: Option<usize>,
    on_top: bool,
    stuck: bool,
    under: Option<Id>,
}

/// What a drawing made per branch, by [`Key`], with the volume layers it read, and per group of a
/// layer's pieces, by its id ([`apart`]); the heights and the bed it was made on, the box of all
/// its branches' slices when the bed's box held each as it was, and each layer's part the groups
/// were cut by. A drawing on the same settings ([`super::Cache`]) and heights, on that bed or
/// another whose box holds that box, takes a branch drawn before as it is where the volumes have
/// every layer it read as it read it ([`Volumes::holds`]), and a group where they have its layer's
/// part as it was. Each drawing keeps what it made or used.
#[derive(Default)]
pub struct Memo {
    bed: Vec<ExPolygon>,
    held: Option<BBox>,
    /// The print z and height of each layer it drew.
    heights: Vec<(f64, f64)>,
    branches: HashMap<Key, Arc<Branched>>,
    /// Each group's base, by the group's id ([`apart`]).
    bases: HashMap<Id, Arc<Vec<ExPolygon>>>,
    /// Per layer, the part its groups' bases were cut by (D18), as the volumes had it.
    parts: Vec<Reads>,
    /// The branches the last drawing drew afresh and the groups it joined ([`super::Work`]).
    pub(super) made: usize,
}

/// Draws `trees` (per layer, the elements placement and smoothing left, each carrying indices
/// into the layer over it, at its placed point) over `v`'s part at `heights` (print z and height
/// per layer), each branch's slices on `bed` (its area, §8.3), with `contacts` per layer (the top
/// contacts: roof layer 0 and the contact discs, §4.3): the base and the floors per layer (§8.5).
/// A branch or a group `memo` holds from a drawing before at the same `heights`, on a bed that
/// cuts it alike, and on volumes with what it read as it was ([`Memo`]), is taken from it, and
/// `memo` keeps this drawing's.
pub fn draw(ctl: Ctl, v: &Volumes, heights: &[(f64, f64)], bed: &[ExPolygon], trees: &[Vec<Element>], contacts: &[Vec<ExPolygon>], memo: &mut Memo) -> Result<Drawn, Cancelled> {
    crate::prof::scope_fanned(crate::prof::Stage::TreeDraw, || drawing(ctl, v, heights, bed, trees, contacts, memo))
}

/// [`draw`], untimed.
fn drawing(ctl: Ctl, v: &Volumes, heights: &[(f64, f64)], bed_area: &[ExPolygon], trees: &[Vec<Element>], contacts: &[Vec<ExPolygon>], memo: &mut Memo) -> Result<Drawn, Cancelled> {
    let s = v.settings();
    let n = heights.len().min(v.layers());
    // Each layer's plane as the cut takes it, a 32-bit float (§8.3).
    let planes: Vec<f32> = planes(&heights[..n]).into_iter().map(|z| z as f32).collect();
    let print: Vec<f64> = heights[..n].iter().map(|h| h.0).collect();
    let bed = Bed::new(bed_area);
    let floors_on = s.keys.floor_layers > 0;
    let all = branches(&trees[..n.min(trees.len())]);
    if memo.heights != heights[..n] || (memo.bed != bed_area && !memo.held.is_some_and(|h| bed.holds(&h))) {
        *memo = Memo { heights: heights[..n].to_vec(), ..Memo::default() };
    }
    if memo.bed != bed_area {
        memo.bed = bed_area.to_vec();
    }
    let seen = &memo.branches;
    // A branch's slices, and a root's drop and floors, and whether it drew them afresh; or, from a
    // fork, cut `again` on the cut of the branch under it, with its own drawing on its own and
    // the id of the cut slices under it it reads (D28).
    let one = |b: usize, again: Option<(&Cut, &Branched, Id)>| -> Result<(Key, Arc<Branched>, bool), Cancelled> {
        let br = &all[b];
        let rings: Vec<[f64; 4]> = br
            .path
            .iter()
            .map(|&(l, i)| {
                let e = &trees[l][i];
                let p = e.placed.expect("branches walk placed elements only");
                [p.x_mm(), p.y_mm(), print[l], e.width(s)]
            })
            .collect();
        let (l0, i0) = br.path[0];
        let last = br.path[br.path.len() - 1].0;
        // Fab's keep-out: the lowest layer the branch is still leaving it on.
        let free = br.path.iter().filter(|&&(l, i)| trees[l][i].leaving()).map(|&(l, _)| l).min();
        let root = &trees[l0][i0];
        let key = Key { rings: rings.iter().map(|r| r.map(f64::to_bits)).collect(), first: l0, last, root: br.root, tip: br.tip, free, on_top: root.on_top, stuck: root.stuck, under: again.map(|a| a.2) };
        if let Some(d) = seen.get(&key) {
            if v.holds(ctl, &d.read)? {
                super::tell(ctl, super::Stage::Draw, l0, all.len(), super::Grown::Nothing);
                return Ok((key, d.clone(), false));
            }
        }
        // Every volume layer it reads, as the drawing reads it.
        let (made, read) = reading(|| -> Result<(Cut, Layered, Layered), Cancelled> {
        // D27, Fab's: a branch whose first step is flatter than one in [`LEVEL_RUN`] leaves its
        // fork level. The reference's tube stands the fork's ring and the cap under it on their
        // side, square to that step: they reach the fork's width over the fork beside the branch,
        // tops nothing is over (R4), and its cut starts at that cap's pole (D8), the lowest slice
        // most of the fork's disc with nothing of the cap under it (R2).
            let level = !br.root && (rings[1][0] - rings[0][0]).hypot(rings[1][1] - rings[0][1]) > LEVEL_RUN * (rings[1][2] - rings[0][2]);
            let (mesh, low, high) = tube(&rings, br.root && l0 == 0, level);
            let (from, to) = span(br, l0, last, low, high, &print, s.keys.layer_height);
            let c = cut(ctl, v, &planes, &bed, &mesh, from..=to, free, again.map(|a| Again { lower: a.0, alone: &a.1.cut }))?;
            let (mut down, mut floor) = (Layered::new(), Layered::new());
            // §8.4: a root above layer 0 goes by its slice on its own layer, and does nothing when
            // that is empty.
            if br.root && l0 > 0 && c.first == l0 && !c.slices.is_empty() {
                let own = &c.slices[0];
                if root.on_top {
                    if floors_on {
                        floor.push((l0, intersection_ex(own, v.placeable(ctl, l0, 0)?)));
                    }
                } else {
                    down = drop(ctl, v, l0, own, root.width(s), root.stuck)?;
                    if floors_on {
                        // D7 (§8.4, copied for parity): each drop layer but the lowest makes a
                        // floor, its remainder on the placeable area at radius 0 there, and files
                        // it a layer lower, not clipped to that layer's slice: it can reach into
                        // that layer's collision.
                        for (l, rest) in &down[..down.len().saturating_sub(1)] {
                            floor.push((l - 1, intersection_ex(rest, v.placeable(ctl, *l, 0)?)));
                        }
                    }
                }
            }
            Ok((c, down, floor))
        });
        let (c, down, floor) = made?;
        // Cut again, it goes on as its drawing on its own, by what that read.
        let read = match again {
            Some(a) => Reads::all([&v.now(ctl, read)?, &a.1.read]),
            None => v.now(ctl, read)?,
        };
        super::tell(ctl, super::Stage::Draw, l0, all.len(), super::Grown::Nothing);
        Ok((key, Arc::new(Branched { cut: c, down, floor, read }), true))
    };
    // Per branch, on its own.
    let drawn = crate::par::try_map(ctl, all.len(), |b| one(b, None))?;
    let mut drawn: Vec<(Key, Arc<Branched>, bool)> = drawn.into_iter().collect::<Result<_, _>>()?;
    let mut fresh = drawn.iter().filter(|d| d.2).count();
    // D28: per branch from a fork the branch under the fork, and per branch those from the fork
    // it ends at.
    let ends: HashMap<(usize, usize), usize> = all.iter().enumerate().filter(|(_, br)| !br.tip).map(|(b, br)| (br.path[br.path.len() - 1], b)).collect();
    let lower: Vec<Option<usize>> = all.iter().map(|br| ends.get(&br.path[0]).copied().filter(|_| !br.root)).collect();
    let mut over: Vec<Vec<usize>> = vec![Vec::new(); all.len()];
    for (b, t) in lower.iter().enumerate() {
        if let Some(t) = *t {
            over[t].push(b);
        }
    }
    // Each branch from a fork is cut again once, on the cut of the branch under it as that came
    // out, when that was cut under it: first those whose lower branch stands on no other, side by
    // side, then those from the forks over them, and so on up. Per branch its cut again, if it
    // has one.
    let mut again: Vec<Option<(Key, Arc<Branched>, bool)>> = vec![None; all.len()];
    let mut kept = Vec::new();
    let mut next: Vec<usize> = (0..all.len()).filter(|&b| lower[b].is_some_and(|t| lower[t].is_none())).collect();
    while !next.is_empty() {
        let now = |b: usize| again[b].as_ref().map_or(&drawn[b].1, |d| &d.1);
        let made = crate::par::try_map(ctl, next.len(), |k| -> Result<Option<(Key, Arc<Branched>, bool)>, Cancelled> {
            let b = next[k];
            let under = lower[b].map(|t| &now(t).cut);
            under.and_then(|c| c.under(&drawn[b].1.cut).map(|id| (c, id))).map(|(c, id)| one(b, Some((c, &*drawn[b].1, id)))).transpose()
        })?;
        for (&b, d) in next.iter().zip(made) {
            let d = d?;
            fresh += d.as_ref().map_or(0, |d| usize::from(d.2));
            again[b] = d;
        }
        next = next.iter().flat_map(|&b| over[b].iter().copied()).collect();
    }
    // Each branch as it stands; the memo keeps its cut on its own too.
    for (b, d) in again.into_iter().enumerate() {
        if let Some(d) = d {
            kept.push(std::mem::replace(&mut drawn[b], d));
        }
    }
    // Each layer's pieces, each with its id: a 128-bit hash of the full content, its branch's key,
    // which of the branch's it is (its slice or its drop), its layer and its rings, so two pieces
    // alike of two branches are two.
    let ids = crate::par::try_map(ctl, drawn.len(), |b| {
        let (key, d, _) = &drawn[b];
        let id = id_of(key);
        let sliced: Vec<Id> = d.cut.slices.iter().enumerate().map(|(k, set)| id_of(&(id, 0, d.cut.first + k, Rings(set)))).collect();
        (sliced, d.down.iter().map(|(l, rest)| id_of(&(id, 1, *l, Rings(rest)))).collect::<Vec<Id>>())
    })?;
    let mut pieces: Vec<Vec<(Id, &[ExPolygon])>> = vec![Vec::new(); n];
    let mut floors: Vec<Vec<ExPolygon>> = vec![Vec::new(); n];
    for ((_, d, _), (sliced, dropped)) in drawn.iter().zip(&ids) {
        for (k, (s, id)) in d.cut.slices.iter().zip(sliced).enumerate() {
            pieces[d.cut.first + k].push((*id, s));
        }
        for ((l, rest), id) in d.down.iter().zip(dropped) {
            pieces[*l].push((*id, rest));
        }
        for (l, f) in &d.floor {
            floors[*l].extend_from_slice(f);
        }
    }
    let tol = s.keys.resolution.min(SIMPLIFY_MAX);
    let clip = grid_width(s.keys.line_width);
    let contact = |l: usize| -> &[ExPolygon] { contacts.get(l).map_or(&[], |c| c.as_slice()) };
    // The floors lose the top contacts.
    let floor = crate::par::try_map(ctl, n, |l| if floors[l].is_empty() { Vec::new() } else { less(union_ex(&floors[l]), contact(l)) })?;
    // The base, a group of a layer's pieces at a time ([`apart`]): joined, smoothed outward and
    // simplified, then less the top contacts and the floors near it. A group drawn before from the
    // same pieces, contacts and floors is taken as the memo holds it.
    //
    // D18, Fab's: and less the collision at radius 0 (the part's own, keep-out or not, which every
    // piece is cut by): §8.5 cuts a notch the part leaves in a branch by a chord, which reaches
    // back into the part's gap (R5). A group is taken only where the volumes have that layer of
    // the part as the drawing before cut its groups by.
    let groups: Vec<(usize, Vec<usize>)> = crate::par::try_map(ctl, n, |l| apart(&pieces[l]))?.into_iter().enumerate().flat_map(|(l, gs)| gs.into_iter().map(move |g| (l, g))).collect();
    let (part, parts): (Vec<&[ExPolygon]>, Vec<Vec<super::volumes::Read>>) = (0..n).map(|l| {
        let (set, read) = reading(|| v.free().collision(ctl, l, 0));
        set.map(|set| (set, read))
    }).collect::<Result<Vec<_>, _>>()?.into_iter().unzip();
    let parts: Vec<Reads> = parts.into_iter().map(|read| v.now(ctl, read)).collect::<Result<_, _>>()?;
    let same_part: Vec<bool> = (0..n).map(|l| memo.parts.get(l).map_or(Ok(false), |was| v.holds(ctl, was))).collect::<Result<_, _>>()?;
    let seen = &memo.bases;
    let made = crate::par::try_map(ctl, groups.len(), |k| {
        let (l, g) = (groups[k].0, &groups[k].1);
        let mut b = BBox::empty();
        for &i in g {
            b.merge(&bbox_of(pieces[l][i].1));
        }
        let near = super::clip::widened(&b);
        let by = |set: &[ExPolygon]| -> Vec<ExPolygon> { set.iter().filter(|e| e.contour.bbox().overlaps(&near)).cloned().collect() };
        let (c, f) = (by(contact(l)), by(&floor[l]));
        let id = id_of(&(g.iter().map(|&i| pieces[l][i].0).collect::<Vec<_>>(), Rings(&c), Rings(&f)));
        match seen.get(&id).filter(|_| same_part[l]) {
            Some(base) => (id, base.clone(), false),
            None => {
                let set: Vec<ExPolygon> = g.iter().flat_map(|&i| pieces[l][i].1.iter().cloned()).collect();
                (id, Arc::new(less(less(less(simplified(&smooth_outward(&union_ex(&set), clip), tol), &c), &f), part[l])), true)
            }
        }
    })?;
    let joined = made.iter().filter(|m| m.2).count();
    let mut base: Vec<Vec<ExPolygon>> = vec![Vec::new(); n];
    let mut bases = HashMap::with_capacity(made.len());
    for ((l, _), (id, b, _)) in groups.iter().zip(made) {
        base[*l].extend(b.iter().cloned());
        bases.insert(id, b);
    }
    // No bed clip here: §9.4 makes the one clip, after the pad (Q11).
    let base = crate::par::try_map_vec(ctl, base, |_, b| in_island_order(b))?;
    memo.held = drawn.iter().chain(&kept).try_fold(BBox::empty(), |mut all, (_, d, _)| {
        all.merge(&d.cut.held?);
        Some(all)
    });
    memo.made = fresh + joined;
    memo.branches = drawn.into_iter().chain(kept).map(|(key, d, _)| (key, d)).collect();
    memo.bases = bases;
    memo.parts = parts;
    Ok(Drawn { base, floor })
}

/// A piece's or a group's id, or a printed layer's ([`super::interface::Memo`]): a 128-bit hash
/// of the full content, every coordinate at its full width.
pub(super) type Id = u128;

pub(super) fn id_of(what: &impl std::hash::Hash) -> Id {
    use std::hash::{Hash, Hasher};
    let half = |salt: u8| {
        let mut h = std::hash::DefaultHasher::new();
        (what, salt).hash(&mut h);
        h.finish() as Id
    };
    half(0) << 64 | half(1)
}

/// A set's rings as an id takes them: per island, its ring count, then each ring's points.
pub(super) struct Rings<'a>(pub &'a [ExPolygon]);

impl std::hash::Hash for Rings<'_> {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        h.write_usize(self.0.len());
        for e in self.0 {
            h.write_usize(e.holes.len());
            for r in e.rings() {
                r.pts.hash(h);
            }
        }
    }
}

/// A layer's `pieces` in groups that never meet, by index: two share a group when their boxes
/// come within the engine's nearness ([`super::clip::widened`]) of each other, or through
/// others that do. Each group keeps its pieces' order, and the groups go by their first piece. A
/// group's join, smoothing and simplifying do not reach another's.
fn apart(pieces: &[(Id, &[ExPolygon])]) -> Vec<Vec<usize>> {
    let n = pieces.len();
    let boxes: Vec<BBox> = pieces.iter().map(|p| super::clip::widened(&bbox_of(p.1))).collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (boxes[i].min.x, i));
    let mut root: Vec<usize> = (0..n).collect();
    fn find(root: &mut [usize], mut i: usize) -> usize {
        while root[i] != i {
            root[i] = root[root[i]];
            i = root[i];
        }
        i
    }
    for (k, &i) in order.iter().enumerate() {
        for &j in &order[k + 1..] {
            if boxes[j].min.x > boxes[i].max.x {
                break;
            }
            if boxes[i].overlaps(&boxes[j]) {
                let (ri, rj) = (find(&mut root, i), find(&mut root, j));
                root[ri.max(rj)] = ri.min(rj);
            }
        }
    }
    let mut at: Vec<Option<usize>> = vec![None; n];
    let mut out: Vec<Vec<usize>> = Vec::new();
    for i in 0..n {
        let r = find(&mut root, i);
        let g = *at[r].get_or_insert_with(|| {
            out.push(Vec::new());
            out.len() - 1
        });
        out[g].push(i);
    }
    out
}

/// The walls an island of a base layer prints (§8.6): with `wall_count` 0, two over
/// [`TWO_WALLS_AREA`] and one under; 1 never two, 2 or more always two; and two whatever the count
/// on a layer whose top `z` (mm) is above [`TWO_WALLS_ABOVE`].
pub fn walls(area_mm2: f64, z: f64, wall_count: usize) -> usize {
    match wall_count {
        _ if z > TWO_WALLS_ABOVE => 2,
        0 => if area_mm2 > TWO_WALLS_AREA { 2 } else { 1 },
        1 => 1,
        _ => 2,
    }
}

/// A width (mm) in whole grid units, as the reference takes its line widths and spacings (§1.2,
/// §8.6): the 32-bit float, divided by the grid's unit in doubles, then truncated. A 0.42 mm line
/// is 419 999, its 0.377 mm spacing 377 079.
pub fn grid_width(mm: f64) -> f64 {
    (mm as f32 as f64 / (1.0 / SCALE)).trunc()
}

/// The loops a drawn base layer prints (§8.6), its top at `z` (mm), with the support line `w` and
/// its `spacing` (mm), both taken as [`grid_width`]s. Its islands are the base closed by half a
/// line. Each island prints one loop on its outline, led by an anchor tail; with two walls
/// ([`walls`]), a loop led by its tail on each part of its second wall's region, a line further
/// in, then a plain loop on its outline. Every loop runs anticlockwise, holes too, and ends 0.15
/// spacings short of its start.
pub fn base_walls(base: &[ExPolygon], z: f64, wall_count: usize, w: f64, spacing: f64) -> Vec<Vec<Pt>> {
    base_islands(base, w).iter().flat_map(|island| island_walls(island, walls(island.area_mm2(), z, wall_count) == 2, w, spacing)).collect()
}

/// A drawn base layer's islands as [`base_walls`] prints them, with the support line `w` (mm):
/// the base closed by half a line.
pub fn base_islands(base: &[ExPolygon], w: f64) -> Vec<ExPolygon> {
    let mitre = Join::Miter(3.0);
    let eps = ISLAND_EPS * SCALE;
    offset(&offset(base, eps, mitre), -(eps + 0.5 * grid_width(w)), mitre)
}

/// The loops one of [`base_islands`] prints ([`base_walls`]), with two walls or one, the support
/// line `w` and its `spacing` (mm). It is all the island's own: islands apart print apart.
pub fn island_walls(island: &ExPolygon, two: bool, w: f64, spacing: f64) -> Vec<Vec<Pt>> {
    let (line, spacing) = (grid_width(w), grid_width(spacing));
    let mitre = Join::Miter(3.0);
    let second = if two { offset(&offset(std::slice::from_ref(island), -SECOND_IN * line, mitre), SECOND_OUT * line, mitre) } else { Vec::new() };
    let mut out = Vec::new();
    if second.is_empty() {
        out.extend(anchored_loops(island, line, spacing));
    } else {
        for part in &second {
            out.extend(anchored_loops(part, line, spacing));
        }
        out.extend(island.rings().filter_map(|r| plain_loop(r, LOOP_END_CUT * spacing)));
    }
    out
}

/// The first support layer's pad (§8.7): `feet` (the drawn layer-0 base) grown 2 mm, less 0.0001,
/// in max(5, ⌈2 ÷ `w`⌉) mitred steps (`w` the first layer's line, mm), each step less `part` (the
/// part's first layer, and with D20 the part over it within the top gap) grown by `gap` (mm) with
/// square corners, so the pad never leaks through a thin wall of the part.
pub fn pad(feet: &[ExPolygon], part: &[ExPolygon], w: f64, gap: f64) -> Vec<ExPolygon> {
    if feet.is_empty() {
        return Vec::new();
    }
    let steps = PAD_STEPS.max((PAD_GROWTH / w - 1e-9).ceil() as usize);
    let step = (PAD_GROWTH - ISLAND_EPS) / steps as f64;
    let keep = if part.is_empty() { Vec::new() } else { offset(part, gap * SCALE, Join::Square) };
    let mut out = feet.to_vec();
    for _ in 0..steps {
        out = less(offset(&out, step * SCALE, Join::Miter(3.0)), &keep);
    }
    out
}

/// What a pad prints (§8.7) with the first layer's line `w` and `spacing` (mm), filled at
/// `density` (a share): one sheath loop on it closed by half a line, without a tail; and lines at
/// 90° a spacing over the density apart through `pivot`, linked into zigzags, their ends 0.45
/// spacings inside the sheath. The sheath's loops, then the fill.
pub fn pad_lines(pad: &[ExPolygon], w: f64, spacing: f64, density: f64, pivot: Pt) -> (Vec<Vec<Pt>>, Vec<Vec<Pt>>) {
    let mitre = Join::Miter(3.0);
    let eps = ISLAND_EPS * SCALE;
    let sheath = offset(&offset(pad, eps, mitre), -(eps + 0.5 * grid_width(w)), mitre);
    let loops = sheath.iter().flat_map(|e| e.rings()).filter_map(|r| plain_loop(r, LOOP_END_CUT * grid_width(spacing))).collect();
    let inside = pad_fill_area(&sheath, spacing);
    let pitch = spacing / density;
    let lines = crate::fill::lines(&inside, std::f64::consts::FRAC_PI_2, pitch, pivot, crate::fill::Align::Grid, crate::fill::Connect::Boundary { max: 3.0 * pitch, anchor: 0.0 });
    (loops, lines.into_iter().map(|l| l.pts).filter(|p| p.len() >= 2).collect())
}

/// The area a pad's lines fill (§8.7): `sheath`, the sheath's centre line, shrunk by 0.4 of the
/// first layer's `spacing` (mm, taken as a [`grid_width`]; the distance a float), then by the
/// filler's outer set, a further 0.05 of it (a float, truncated to whole nm). Each shrink drops its
/// close points first, at its own threshold (§1.5).
fn pad_fill_area(sheath: &[ExPolygon], spacing: f64) -> Vec<ExPolygon> {
    let (mitre, sn) = (Join::Miter(3.0), grid_width(spacing));
    let area = offset(sheath, -((PAD_FILL_IN * sn) as f32 as f64), mitre);
    offset(&area, -((PAD_END_IN * sn) as f32 as f64).trunc(), mitre)
}

/// A support bead (§8.6, §8.7, §9.5): its line and layer height (mm), and its speed (mm/s).
#[derive(Clone, Copy, Debug)]
pub struct Bead {
    pub w: f64,
    pub h: f64,
    pub speed: f64,
}

impl Bead {
    /// `pts` as an open path of this bead in `role`.
    pub fn open(self, role: Role, pts: Vec<Pt>) -> ExtrusionPath {
        ExtrusionPath { role, pts, width: self.w, height: self.h, area: flow::cross_section(self.w, self.h), speed: self.speed, ..ExtrusionPath::default() }
    }

    /// Ring `pts` as a closed loop of this bead in `role`.
    pub fn closed(self, role: Role, pts: Vec<Pt>) -> ExtrusionPath {
        ExtrusionPath { loop_pts: pts.clone(), closed: true, ..self.open(role, pts) }
    }
}

/// A drawn base layer as it prints (§8.6), its top at `z` (mm): the loops of [`base_walls`] in
/// `bead`, the support line at the layer's height. Each is open, as it ends short of its start.
pub fn base_paths(base: &[ExPolygon], z: f64, wall_count: usize, bead: Bead) -> Vec<ExtrusionPath> {
    base_islands(base, bead.w).iter().flat_map(|island| island_paths(island, walls(island.area_mm2(), z, wall_count) == 2, bead)).collect()
}

/// One of [`base_islands`] as it prints ([`base_paths`]), with two walls or one.
pub fn island_paths(island: &ExPolygon, two: bool, bead: Bead) -> Vec<ExtrusionPath> {
    island_walls(island, two, bead.w, flow::spacing(bead.w, bead.h)).into_iter().map(|pts| bead.open(Role::Support, pts)).collect()
}

/// The first support layer's `pad` as it prints (§8.7), in `bead`, the first layer's line and
/// height, filled at `density` (a share) through `pivot`: its sheath, then its fill.
pub fn pad_paths(pad: &[ExPolygon], density: f64, pivot: Pt, bead: Bead) -> Vec<ExtrusionPath> {
    let (loops, lines) = pad_lines(pad, bead.w, flow::spacing(bead.w, bead.h), density, pivot);
    loops.into_iter().chain(lines).map(|pts| bead.open(Role::Support, pts)).collect()
}

/// A loop without a tail on ring `r` (§8.6 step 5, §8.7): from its first stored point,
/// anticlockwise, ending `cut` (grid units) short of its start. None when that leaves under two
/// points.
fn plain_loop(r: &Polygon, cut: f64) -> Option<Vec<Pt>> {
    let mut pts = r.pts.clone();
    if pts.is_empty() {
        return None;
    }
    if r.area2() < 0 {
        pts[1..].reverse();
    }
    pts.push(pts[0]);
    cut_end(&mut pts, cut);
    (pts.len() >= 2).then_some(pts)
}

/// `pts` with the last `by` (grid units) of its length taken off, walked back in doubles; only the
/// new end is truncated to the grid.
fn cut_end(pts: &mut Vec<Pt>, by: f64) {
    let mut left = by;
    while pts.len() >= 2 {
        let n = pts.len();
        let (a, b) = (pts[n - 2], pts[n - 1]);
        let d = a.dist(b);
        if d > left {
            pts[n - 1] = towards(b, a, left);
            return;
        }
        left -= d;
        pts.pop();
    }
}

/// The point `by` (grid units, under their distance) from `a` towards `b`, truncated to the grid.
fn towards(a: Pt, b: Pt, by: f64) -> Pt {
    let t = by / a.dist(b);
    Pt::new((a.x as f64 + (b.x - a.x) as f64 * t) as i64, (a.y as f64 + (b.y - a.y) as f64 * t) as i64)
}

/// The loops on the rings of `region` (§8.6 steps 4 and 5), the contour first, then its holes,
/// each led by its anchor tail where it has one; `line` and `spacing` in whole grid units. A ring
/// is taken as a clockwise path from its start S0 (the contour's last stored point, a hole's
/// first), closed, and cut 0.15 spacings short at Q. The tail ([`tail`]) follows Q, and the whole
/// path is printed backwards: from the tail's far end along the inner line, out to Q, and round
/// anticlockwise to S0.
fn anchored_loops(region: &ExPolygon, line: f64, spacing: f64) -> Vec<Vec<Pt>> {
    let inner = inner_line(region, line);
    region
        .rings()
        .enumerate()
        .filter_map(|(k, r)| {
            let mut path = r.pts.clone();
            if path.is_empty() {
                return None;
            }
            if r.area2() > 0 {
                path.reverse();
            }
            path.push(path[0]);
            cut_end(&mut path, LOOP_END_CUT * spacing);
            if path.len() < 2 {
                return None;
            }
            if let Some(t) = tail(&inner, k, path[path.len() - 1], TAIL_REACH * line, TAIL_LENGTH * spacing) {
                path.extend(t);
            }
            path.reverse();
            Some(path)
        })
        .collect()
}

/// A point of an anchor's inner line (§8.6 step 5) and the ring of its region it comes from: 0 the
/// contour, 1, 2 … the holes in order; none where two rings' offsets cross.
type Sourced = (Pt, Option<usize>);

/// The inner line of an anchored region (§8.6 step 5): its contour shrunk, and each of its holes
/// grown, by `line` (grid units), each ring on its own and mitred at [`TAIL_MITRE`] as the engine
/// takes it (§1.5), once its close points are gone as every offset drops them (§1.5, [`sparse`]
/// at [`NEAR_SHARE`] of `line`, in the ring's stored order); then the grown holes taken out of the
/// shrunk contour. Every ring of it runs clockwise.
fn inner_line(region: &ExPolygon, line: f64) -> Vec<Vec<Sourced>> {
    let mitre = as_engine_takes(Join::Miter(TAIL_MITRE));
    let mut from: HashMap<Pt, usize> = HashMap::new();
    let mut alone = |k: usize, r: &Polygon| {
        let mut ring = Polygon::new(sparse(&r.pts, NEAR_SHARE * line));
        if ring.len() < 3 {
            return Vec::new();
        }
        if ring.area2() < 0 {
            ring.reverse();
        }
        let off = offset_polygons(&[ring], if k == 0 { -line } else { line }, mitre);
        for p in off.iter().flat_map(|e| e.rings()).flat_map(|r| r.pts.iter()) {
            from.entry(*p).or_insert(k);
        }
        off
    };
    let shrunk = alone(0, &region.contour);
    if shrunk.is_empty() {
        return Vec::new();
    }
    let grown: Vec<ExPolygon> = region.holes.iter().enumerate().flat_map(|(h, r)| alone(h + 1, r)).collect();
    difference_ex(&shrunk, &grown)
        .iter()
        .flat_map(|e| e.rings())
        .map(|r| {
            let mut pts = r.pts.clone();
            if r.area2() > 0 {
                pts.reverse();
            }
            pts.into_iter().map(|p| (p, from.get(&p).copied())).collect()
        })
        .collect()
}

/// The anchor tail after a loop's cut end `q` (§8.6 step 5), the loop on its region's ring `k`.
/// It is searched for on the inner line's segments from ring `k`: at least one end from it, and
/// the nearest point not on an end that is not. None unless the nearest point comes within `reach`
/// (grid units). Else that point C (the first of equals), then the inner ring clockwise from C,
/// the way the loop ran, for `length` less the step from `q` to C: inside C's own segment if the
/// rest of it is longer than that, else on point by point to where the length runs out, to the
/// first point not from ring `k` (taking it), or round to the start of C's segment. Quirk D9
/// (copied for parity): past C's own segment, the piece of it from C on is not charged, so most
/// tails are longer than `length` by that piece.
fn tail(inner: &[Vec<Sourced>], k: usize, q: Pt, reach: f64, length: f64) -> Option<Vec<Pt>> {
    let mine = |s: Option<usize>| s == Some(k);
    let qf = (q.x as f64, q.y as f64);
    let mut best: Option<(f64, usize, usize, P2)> = None;
    for (ri, r) in inner.iter().enumerate() {
        let n = r.len();
        for i in 0..n {
            let ((a, sa), (b, sb)) = (r[i], r[(i + 1) % n]);
            if !mine(sa) && !mine(sb) {
                continue;
            }
            let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
            let l2 = dx * dx + dy * dy;
            let t = if l2 > 0.0 { (((qf.0 - a.x as f64) * dx + (qf.1 - a.y as f64) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
            if (t < FOOT_END && !mine(sa)) || (t > 1.0 - FOOT_END && !mine(sb)) {
                continue;
            }
            let foot = (a.x as f64 + dx * t, a.y as f64 + dy * t);
            let d = sq(foot, qf);
            if best.is_none_or(|b| d < b.0) {
                best = Some((d, ri, i, foot));
            }
        }
    }
    let (d, ri, i, foot) = best?;
    if d >= reach * reach {
        return None;
    }
    let r = &inner[ri];
    let n = r.len();
    let c = Pt::new(foot.0 as i64, foot.1 as i64);
    let mut left = length - q.dist(c);
    let (far, source) = r[(i + 1) % n];
    if c.dist(far) > left {
        return Some(vec![c, towards(c, far, left)]);
    }
    // D9: the rest of C's own segment is not charged.
    let mut out = vec![c, far];
    let (mut j, mut on) = ((i + 1) % n, mine(source));
    while on && j != i {
        let (a, (b, sb)) = (r[j].0, r[(j + 1) % n]);
        let seg = a.dist(b);
        if seg > left {
            out.push(towards(a, b, left));
            break;
        }
        out.push(b);
        left -= seg;
        (j, on) = ((j + 1) % n, mine(sb));
    }
    Some(out)
}

/// `set` smoothed outward (§8.5) with the clip distance `clip` (grid units): one pass over every
/// ring's corners. Convex corners stay. A concave corner whose edges meet at under 135° goes: a
/// crack (its neighbours already nearer than `clip`) is zipped shut from the corner out to about
/// where it is `clip` wide, and any other such corner is cut by a chord `clip` long. A ring the
/// zip closes goes, and so does a clockwise triangle lower than `clip`. The pass makes no union
/// (§1.6): each ring keeps the first point it came with, for the simplify after it. The union here
/// only puts the rings back into islands, so each ring is turned back to start there.
pub fn smooth_outward(set: &[ExPolygon], clip: f64) -> Vec<ExPolygon> {
    let rings: Vec<Polygon> = set.iter().flat_map(|e| e.rings()).filter_map(|r| smooth_ring(&r.pts, clip)).map(Polygon::new).collect();
    if rings.is_empty() {
        return Vec::new();
    }
    let firsts: HashSet<Pt> = rings.iter().map(|r| r.pts[0]).collect();
    let mut out = union(&rings, FillRule::NonZero);
    for e in &mut out {
        for r in std::iter::once(&mut e.contour).chain(e.holes.iter_mut()) {
            if let Some(i) = r.pts.iter().position(|p| firsts.contains(p)) {
                r.pts.rotate_left(i);
            }
        }
    }
    out
}

type P2 = (f64, f64);

/// The distance from `a` to `b`.
fn d2(a: P2, b: P2) -> f64 {
    sq(a, b).sqrt()
}

/// The squared distance from `a` to `b`.
fn sq(a: P2, b: P2) -> f64 {
    (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)
}

/// How a side of a crack's zip stopped (§8.5): free to go on, blocked by the ring, or held off by
/// the clip distance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Zip {
    Free,
    Blocked,
    Far,
}

/// `p` truncated to the grid, as the reference makes its new points (§8.5).
fn trunc(p: P2) -> P2 {
    (p.0.trunc(), p.1.trunc())
}

/// The point of segment `from`-`to` at distance `len` from `other`: of two, the one nearer `to`;
/// the nearest end when none is that far.
fn at_distance(from: P2, to: P2, other: P2, len: f64) -> P2 {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let (fx, fy) = (from.0 - other.0, from.1 - other.1);
    let a = dx * dx + dy * dy;
    let b = 2.0 * (fx * dx + fy * dy);
    let c = fx * fx + fy * fy - len * len;
    let t = if a > 0.0 { ((-b + (b * b - 4.0 * a * c).max(0.0).sqrt()) / (2.0 * a)).clamp(0.0, 1.0) } else { 0.0 };
    (from.0 + dx * t, from.1 + dy * t)
}

/// One ring smoothed outward (§8.5), in grid units, `clip` included; the material on its left.
/// None when it goes. The pass visits the ring's own points once each, in order, seeing the ring
/// as earlier corners left it; the points it makes are never visited, and new points are cut to
/// whole nm.
fn smooth_ring(ring: &[Pt], clip: f64) -> Option<Vec<Pt>> {
    let near = (SMOOTH_NEAR * SCALE).powi(2);
    // From the second point on, a point nearer than 0.01 mm to the last point kept goes; the
    // last and the first are not compared.
    let mut p: Vec<P2> = Vec::with_capacity(ring.len() + 8);
    for q in ring {
        let q = (q.x as f64, q.y as f64);
        if p.last().is_none_or(|&l| sq(l, q) >= near) {
            p.push(q);
        }
    }
    let n = p.len();
    if n < 3 {
        return None;
    }
    let mut prev: Vec<usize> = (0..n).map(|i| (i + n - 1) % n).collect();
    let mut next: Vec<usize> = (0..n).map(|i| (i + 1) % n).collect();
    let mut alive = vec![true; n];
    let mut count = n;
    // Takes `i` out of the ring.
    fn unlink(i: usize, prev: &mut [usize], next: &mut [usize], alive: &mut [bool], count: &mut usize) {
        let (a, b) = (prev[i], next[i]);
        next[a] = b;
        prev[b] = a;
        alive[i] = false;
        *count -= 1;
    }
    // Puts a new point after `a`.
    fn insert(at: P2, a: usize, p: &mut Vec<P2>, prev: &mut Vec<usize>, next: &mut Vec<usize>, alive: &mut Vec<bool>, count: &mut usize) {
        let b = next[a];
        p.push(at);
        prev.push(a);
        next.push(b);
        alive.push(true);
        let k = p.len() - 1;
        next[a] = k;
        prev[b] = k;
        *count += 1;
    }
    let out = |from: usize, p: &[P2], next: &[usize]| -> Vec<Pt> {
        let mut pts = Vec::new();
        let mut k = from;
        loop {
            pts.push(Pt::new(p[k].0 as i64, p[k].1 as i64));
            k = next[k];
            if k == from {
                return pts;
            }
        }
    };
    for i in 0..n {
        if !alive[i] {
            continue;
        }
        if count < 3 {
            return None;
        }
        let (a, b) = (prev[i], next[i]);
        let (v, pa, pb) = (p[i], p[a], p[b]);
        // A left turn or none: a convex corner, never touched.
        if (v.0 - pa.0) * (pb.1 - v.1) - (v.1 - pa.1) * (pb.0 - v.0) >= 0.0 {
            continue;
        }
        let (e1, e2) = ((pa.0 - v.0, pa.1 - v.1), (pb.0 - v.0, pb.1 - v.1));
        let (l1, l2) = ((e1.0 * e1.0 + e1.1 * e1.1).sqrt(), (e2.0 * e2.0 + e2.1 * e2.1).sqrt());
        let angle = ((e1.0 * e2.0 + e1.1 * e2.1) / (l1 * l2)).clamp(-1.0, 1.0).acos();
        if angle >= SHARP {
            continue;
        }
        let gap = d2(pa, pb);
        if gap < clip {
            // A crack, zipped shut from the corner out (§8.5): the chord runs from the previous
            // side's end `l` to the next side's `r`. Each step tries the next side if its end is
            // strictly nearer the corner, or the previous side has stopped; else the previous
            // side (so a tie goes to it). A try stops the side as blocked if its next point lies
            // strictly left of the chord, inside the ring; as far if the chord to that point is
            // over `clip`; else that point becomes the side's end. With both far, both go on one
            // more if their next points are within `clip`. The zip goes on at three points, where
            // a side is blocked exactly when the triangle runs counter-clockwise, so a clockwise
            // one with short sides loses a point; a ring under three points goes. Stopped at three
            // points, not both far, the zip keeps the triangle from the ring's head (its first
            // point, or the first one after it still there), and the pass ends.
            unlink(i, &mut prev, &mut next, &mut alive, &mut count);
            let (mut l, mut r) = (a, b);
            let (mut sl, mut sr) = (Zip::Free, Zip::Free);
            loop {
                if count < 3 {
                    return None;
                }
                if sl != Zip::Free && sr != Zip::Free {
                    if sl == Zip::Far && sr == Zip::Far && sq(p[prev[l]], p[next[r]]) <= clip * clip {
                        let (nl, nr) = (prev[l], next[r]);
                        unlink(l, &mut prev, &mut next, &mut alive, &mut count);
                        unlink(r, &mut prev, &mut next, &mut alive, &mut count);
                        (l, r, sl, sr) = (nl, nr, Zip::Free, Zip::Free);
                        continue;
                    }
                    if count == 3 {
                        return Some(out((0..p.len()).find(|&k| alive[k])?, &p, &next));
                    }
                    break;
                }
                let right = sl != Zip::Free || (sr == Zip::Free && sq(p[r], v) < sq(p[l], v));
                let cand = if right { next[r] } else { prev[l] };
                let (pl, pr, pc) = (p[l], p[r], p[cand]);
                let side = if (pr.0 - pl.0) * (pc.1 - pl.1) - (pr.1 - pl.1) * (pc.0 - pl.0) > 0.0 {
                    Zip::Blocked
                } else if sq(pc, if right { pl } else { pr }) > clip * clip {
                    Zip::Far
                } else {
                    unlink(if right { r } else { l }, &mut prev, &mut next, &mut alive, &mut count);
                    if right { r = cand } else { l = cand }
                    continue;
                };
                if right { sr = side } else { sl = side }
            }
            // The last chord: its ends, existing points of the ring, moved in place (a moved end
            // still to be visited is visited where it now is). Nothing moves with both sides
            // blocked or the chord within 100 nm of `clip`. With both far, both ends slide
            // towards their next points by the one share that would make the chord `clip` were
            // the crack straight there.
            let cur = d2(p[l], p[r]);
            if clip - cur > SMOOTH_SLACK {
                let moved = |from: P2, by: P2| (from.0 + by.0.trunc(), from.1 + by.1.trunc());
                match (sl, sr) {
                    (Zip::Far, Zip::Far) => {
                        let (nl, nr) = (p[prev[l]], p[next[r]]);
                        let t = (clip - cur) / (d2(nl, nr) - cur);
                        let (ml, mr) = (((nl.0 - p[l].0) * t, (nl.1 - p[l].1) * t), ((nr.0 - p[r].0) * t, (nr.1 - p[r].1) * t));
                        p[l] = moved(p[l], ml);
                        p[r] = moved(p[r], mr);
                    }
                    // D19, Fab's (for the reference's D10): with one side blocked and the other
                    // far, the far end moves to the point F of its next edge `clip` from the
                    // blocked end, which stays, so the chord is `clip` long. The reference moves
                    // the blocked end by the far end's way to F instead, off its own side into the
                    // crack's mouth, which leaves a tooth there.
                    (Zip::Blocked, Zip::Far) => {
                        p[r] = trunc(at_distance(p[r], p[next[r]], p[l], clip));
                    }
                    (Zip::Far, Zip::Blocked) => {
                        p[l] = trunc(at_distance(p[l], p[prev[l]], p[r], clip));
                    }
                    _ => {}
                }
            }
            continue;
        }
        if gap < clip + SMOOTH_SLACK {
            unlink(i, &mut prev, &mut next, &mut alive, &mut count);
            continue;
        }
        let half = clip / 2.0;
        let (short, long, ls) = if l1 <= l2 { (a, b, l1) } else { (b, a, l2) };
        // How far the shorter edge's far end lies from the corner's bisector.
        let off = ls * (angle / 2.0).sin();
        if off <= SMOOTH_SLACK {
            unlink(i, &mut prev, &mut next, &mut alive, &mut count);
            continue;
        }
        if off <= half {
            // The shorter edge goes, and the cut runs from its far end to the point of the
            // longer edge `clip` from it.
            p[i] = trunc(at_distance(v, p[long], p[short], clip));
            continue;
        }
        // A chord `clip` long, square to the bisector, at equal distances along both edges.
        let t = half / (angle / 2.0).sin();
        let (u1, u2) = ((e1.0 / l1, e1.1 / l1), (e2.0 / l2, e2.1 / l2));
        p[i] = trunc((v.0 + u1.0 * t, v.1 + u1.1 * t));
        insert(trunc((v.0 + u2.0 * t, v.1 + u2.1 * t)), i, &mut p, &mut prev, &mut next, &mut alive, &mut count);
    }
    if count < 3 {
        return None;
    }
    let pts = out((0..p.len()).find(|&k| alive[k])?, &p, &next);
    if pts.len() == 3 {
        let area2 = (pts[1].x - pts[0].x) as f64 * (pts[2].y - pts[0].y) as f64 - (pts[1].y - pts[0].y) as f64 * (pts[2].x - pts[0].x) as f64;
        let longest = pts[0].dist(pts[1]).max(pts[1].dist(pts[2])).max(pts[2].dist(pts[0]));
        if area2 < 0.0 && area2.abs() / longest < clip {
            return None;
        }
    }
    Some(pts)
}

#[cfg(test)]
mod tests {
    use super::super::element::ngon;
    use super::super::settings::tests::r14;
    use super::super::settings::Settings;
    use super::*;

    fn ring(pts: &[(f64, f64)]) -> ExPolygon {
        ExPolygon::new(Polygon::new(pts.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect()))
    }

    /// `pts` with every edge cut into pieces at most `step` (mm) long.
    fn fine(pts: &[(f64, f64)], step: f64) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            let k = ((d2(a, b) / step).ceil() as usize).max(1);
            for j in 0..k {
                let t = j as f64 / k as f64;
                out.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
            }
        }
        out
    }

    fn area(set: &[ExPolygon]) -> f64 {
        total_area_mm2(set)
    }

    /// The bed the drawing tests stand on: 200 mm square round the origin.
    fn bed() -> Vec<ExPolygon> {
        vec![ring(&[(-100.0, -100.0), (100.0, -100.0), (100.0, 100.0), (-100.0, 100.0)])]
    }

    /// §8.5: convex corners and notches of 135° or more stay; a 60° notch is cut by a 0.42 mm chord
    /// at equal distances along both edges; a notch whose sides are within 100 nm over 0.42 mm
    /// apart loses only its corner; a crack narrower than 0.42 mm is zipped shut; a narrow hole
    /// closes; a clockwise triangle lower than 0.42 mm goes.
    #[test]
    fn smoothing_outward_cuts_sharp_notches_and_zips_cracks() {
        let clip = grid_width(0.42);
        let square = ring(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
        assert!((area(&smooth_outward(std::slice::from_ref(&square), clip)) - 100.0).abs() < 1e-9, "a square has no concave corner");
        // A 150° notch, wider than 135°: it stays.
        let deep = (15.0_f64).to_radians().tan() * 1.0;
        let wide = ring(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (6.0, 10.0), (5.0, 10.0 - deep), (4.0, 10.0), (0.0, 10.0)]);
        assert!((area(&smooth_outward(std::slice::from_ref(&wide), clip)) - area(std::slice::from_ref(&wide))).abs() < 1e-9, "a 150° notch stays");
        // A 60° notch with 2 mm edges: cut at 0.42 mm along each edge, so a 0.42 mm chord, and the
        // area grows by that triangle.
        let h = 1.0 / (30.0_f64).to_radians().tan();
        let notch = ring(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (6.0, 10.0), (5.0, 10.0 - h), (4.0, 10.0), (0.0, 10.0)]);
        let cut = smooth_outward(std::slice::from_ref(&notch), clip);
        let gained = area(&cut) - area(std::slice::from_ref(&notch));
        let want = 0.5 * 0.42 * 0.42 * (60.0_f64).to_radians().sin();
        assert!((gained - want).abs() < 1e-4, "the 60° notch gains its 0.42 mm triangle: {gained} mm2, want {want}");
        let pts = &cut[0].contour.pts;
        let tip = Pt::from_mm(5.0, 10.0 - h);
        assert!(!pts.contains(&tip), "the corner itself is gone");
        let ends: Vec<&Pt> = pts.iter().filter(|q| (q.dist(tip) / SCALE - 0.42).abs() < 1e-5).collect();
        assert_eq!(ends.len(), 2, "the chord's ends are 0.42 mm from the corner along both edges");
        assert!((ends[0].dist(*ends[1]) / SCALE - 0.42).abs() < 1e-5, "and 0.42 mm apart");
        // A notch 3 mm deep whose mouth is 25 nm over 0.42 mm: only its corner goes, so the
        // square is whole again.
        let mouth = Polygon::new(vec![Pt::from_mm(0.0, 0.0), Pt::from_mm(10.0, 0.0), Pt::from_mm(10.0, 10.0), Pt::new(5_210_000, 10_000_000), Pt::from_mm(5.0, 7.0), Pt::new(4_789_975, 10_000_000), Pt::from_mm(0.0, 10.0)]);
        assert!((area(&smooth_outward(&[ExPolygon::new(mouth)], clip)) - 100.0).abs() < 1e-6, "a mouth within 100 nm over the clip loses only the corner");
        // A crack 0.2 mm across at its mouth and 3 mm deep: zipped shut, the square whole again.
        let crack = ring(&fine(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (5.1, 10.0), (5.0, 7.0), (4.9, 10.0), (0.0, 10.0)], 0.05));
        let zipped = smooth_outward(std::slice::from_ref(&crack), clip);
        assert!((area(&zipped) - 100.0).abs() < 1e-3, "the crack closes: {} mm2", area(&zipped));
        // A hole 0.2 mm across and 2 mm long closes; one 1 mm across stays.
        let with = |w: f64| {
            let mut e = ring(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
            let mut hole = Polygon::new(fine(&[(5.0, 6.0), (5.0 + w / 2.0, 5.0), (5.0, 4.0), (5.0 - w / 2.0, 5.0)], 0.05).iter().map(|&(x, y)| Pt::from_mm(x, y)).collect());
            if hole.area2() > 0 {
                hole.reverse();
            }
            e.holes.push(hole);
            e
        };
        assert!((area(&smooth_outward(&[with(0.2)], clip)) - 100.0).abs() < 1e-3, "a hole narrower than the clip closes");
        assert!(!smooth_outward(&[with(1.0)], clip)[0].holes.is_empty(), "a wider one stays");
        // A clockwise triangle lower than 0.42 mm goes.
        let mut sliver = ring(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
        sliver.holes.push(Polygon::new(vec![Pt::from_mm(3.0, 5.0), Pt::from_mm(5.0, 5.3), Pt::from_mm(7.0, 5.0)]));
        assert!(sliver.holes[0].area2() < 0, "the hole runs clockwise");
        assert!((area(&smooth_outward(&[sliver], clip)) - 100.0).abs() < 1e-9, "a clockwise triangle 0.3 mm high goes");
    }

    /// §8.6: two walls over π 2.5² mm² with the count at 0, one under; the count 1 never two, 2
    /// always; above 100 mm always two.
    #[test]
    fn a_base_island_prints_two_walls_over_19_635_mm2() {
        assert!((TWO_WALLS_AREA - 19.635).abs() < 1e-3, "π · 2.5²");
        assert_eq!(walls(19.63, 50.0, 0), 1);
        assert_eq!(walls(19.64, 50.0, 0), 2);
        assert_eq!(walls(30.0, 50.0, 1), 1, "1 is never two");
        assert_eq!(walls(1.0, 50.0, 2), 2, "2 is always two");
        assert_eq!(walls(1.0, 100.2, 0), 2, "above 100 mm always two");
        assert_eq!(walls(1.0, 100.2, 1), 2, "whatever the count");
    }

    /// Distance (mm) of `p` from the origin.
    fn radius(p: Pt) -> f64 {
        p.dist(Pt::new(0, 0)) / SCALE
    }

    /// The length (mm) of `pts` as a path.
    fn length(pts: &[Pt]) -> f64 {
        pts.windows(2).map(|w| w[0].dist(w[1])).sum::<f64>() / SCALE
    }

    /// Twice the signed area of `pts` as a closed ring.
    fn area2(pts: &[Pt]) -> f64 {
        (0..pts.len()).map(|i| (pts[i].x as f64) * (pts[(i + 1) % pts.len()].y as f64) - (pts[(i + 1) % pts.len()].x as f64) * (pts[i].y as f64)).sum()
    }

    /// §8.6 on round bases: a branch drawn at 2.65 mm is an island of 2.44 mm (in by half a line),
    /// under π 2.5², and prints one anchored loop; at 2.75 mm (2.54) it prints an anchored loop a
    /// line further in, at 2.12 mm, then its outer loop without a tail. Loops run anticlockwise and
    /// end 0.15 spacings (0.057 mm) short; a tail runs a line inside its loop, 6 spacings long with
    /// its step out, and longer by the uncharged rest of its first segment (D9), under one of the
    /// inner line's 9 µm segments here. Above 100 mm a small island prints two walls; a ring's hole
    /// prints its own anticlockwise loop.
    #[test]
    fn a_round_base_prints_one_wall_under_2_71_mm_and_two_over() {
        let (w, sp) = (0.42, flow::spacing(0.42, 0.2));
        let six = 6.0 * grid_width(sp) / SCALE;
        let disc = |r: f64| vec![ngon(Pt::new(0, 0), r, 1440)];
        // The loop part of an anchored path starts at its first point out at `r`.
        let split = |path: &[Pt], r: f64| path.iter().position(|&q| (radius(q) - r).abs() < 0.01).unwrap();
        let one = base_walls(&disc(2.65), 10.0, 0, w, sp);
        assert_eq!(one.len(), 1, "one wall at 2.65 mm");
        let k = split(&one[0], 2.44);
        assert!(k > 1, "led by a tail");
        let (tail, lp) = one[0].split_at(k);
        assert!(lp.iter().all(|&q| (radius(q) - 2.44).abs() < 2e-3), "the loop half a line in");
        assert!(tail.iter().all(|&q| (radius(q) - 2.02).abs() < 2e-3), "its tail a line further in");
        let got = length(&one[0][..=k]);
        assert!(got > six - 1e-5 && got < six + 0.009, "tail and step: {got} mm, 6 spacings {six} and a piece");
        assert!(area2(lp) > 0.0, "anticlockwise");
        assert!((lp[lp.len() - 1].dist(lp[0]) / SCALE - 0.15 * sp).abs() < 1e-3, "it stops 0.057 mm short");
        let two = base_walls(&disc(2.75), 10.0, 0, w, sp);
        assert_eq!(two.len(), 2, "two walls at 2.75 mm");
        assert!(two[1].iter().all(|&q| (radius(q) - 2.54).abs() < 2e-3), "the outer loop, without a tail");
        let k = split(&two[0], 2.12);
        assert!(k > 1 && two[0][k..].iter().all(|&q| (radius(q) - 2.12).abs() < 2e-3), "the inner one a line in, led by its tail");
        assert!(two[0][..k].iter().all(|&q| (radius(q) - 1.70).abs() < 2e-3), "its tail a line further in");
        assert_eq!(base_walls(&disc(1.5), 100.2, 0, w, sp).len(), 2, "above 100 mm two walls");
        // A ring 1 mm wide: one island, under the area, with its hole's own loop; a line in
        // nothing is left, so neither loop has a tail.
        let mut band = disc(2.5);
        let mut hole = ngon(Pt::new(0, 0), 1.5, 1440).contour;
        hole.reverse();
        band[0].holes.push(hole);
        let loops = base_walls(&band, 10.0, 0, w, sp);
        assert_eq!(loops.len(), 2, "the outline and the hole");
        assert!(loops.iter().all(|l| area2(l) > 0.0), "both anticlockwise");
        assert!(loops.iter().any(|l| l.iter().all(|&q| (radius(q) - 1.71).abs() < 2e-3)), "the hole's loop half a line out from it");
    }

    /// §1.2, §8.6: widths as whole grid units, from the 32-bit float, truncated.
    #[test]
    fn widths_are_whole_nm_from_their_floats() {
        assert_eq!(grid_width(0.42), 419_999.0);
        assert_eq!(grid_width(flow::spacing(0.42, 0.2)), 377_079.0);
        assert_eq!(grid_width(0.5), 500_000.0);
    }

    /// A point in whole nm.
    fn nm(x: i64, y: i64) -> Pt {
        Pt::new(x, y)
    }

    /// §8.6 step 5 on a square whose last stored point S0 is at (0, 9 mm): its loop is a clockwise
    /// path from S0, cut 0.15 spacings short at Q; the tail starts at C, the inner line's nearest
    /// point to Q a line in, and runs clockwise for 6 spacings less the step out, after the rest of
    /// C's segment, which is not charged (D9). Printed backwards: the tail's end, the inner
    /// corner, C, Q, then the loop round anticlockwise to S0.
    #[test]
    fn an_anchor_tail_leads_its_loop_and_runs_past_its_first_segment() {
        let (line, spacing) = (grid_width(0.42), grid_width(flow::spacing(0.42, 0.2)));
        let square = ExPolygon::new(Polygon::new(vec![nm(0, 0), nm(10_000_000, 0), nm(10_000_000, 10_000_000), nm(0, 10_000_000), nm(0, 9_000_000)]));
        let loops = anchored_loops(&square, line, spacing);
        assert_eq!(loops.len(), 1);
        let path = &loops[0];
        // Q is 56 561.85 nm from S0 towards (0, 0), truncated.
        let q = nm(0, 8_943_438);
        assert_eq!(&path[3..], &[q, nm(0, 0), nm(10_000_000, 0), nm(10_000_000, 10_000_000), nm(0, 10_000_000), nm(0, 9_000_000)], "Q, then round anticlockwise to S0");
        let c = path[2];
        assert!(c.x == 419_999 && (c.y - 8_943_438).abs() <= 1, "C a line in from Q: {c:?}");
        let corner = nm(419_999, 9_580_001);
        assert_eq!(path[1], corner, "C's segment run out to the inner line's corner");
        let end = path[0];
        let budget = TAIL_LENGTH * spacing - line;
        assert!(end.y == corner.y && (end.x as f64 - (corner.x as f64 + budget)).abs() <= 1.0, "then 6 spacings less the step along the top: {end:?}");
        assert!(c.dist(corner) > 600_000.0, "so the tail is longer by the 0.64 mm left of C's segment");
    }

    /// §8.6 step 5 with a hole whose grown offset crosses the shrunk contour: the points made where
    /// they cross come from neither ring, and a tail stops at the first one it meets, taking it. A
    /// hole's loop starts at its first stored point, searches only the inner line from its own
    /// ring, and runs anticlockwise.
    #[test]
    fn a_tail_stops_where_two_offsets_cross() {
        let (line, spacing) = (grid_width(0.42), grid_width(flow::spacing(0.42, 0.2)));
        let mut region = ExPolygon::new(Polygon::new(vec![nm(10_000_000, 10_000_000), nm(0, 10_000_000), nm(0, 0), nm(10_000_000, 0), nm(10_000_000, 7_000_000)]));
        region.holes.push(Polygon::new(vec![nm(8_900_000, 4_000_000), nm(8_900_000, 6_000_000), nm(9_500_000, 6_000_000), nm(9_500_000, 4_000_000)]));
        assert!(region.holes[0].area2() < 0, "the hole runs clockwise");
        let loops = anchored_loops(&region, line, spacing);
        assert_eq!(loops.len(), 2, "the contour's loop and the hole's");
        let near = |a: Pt, b: Pt| (a.x - b.x).abs() <= 1 && (a.y - b.y).abs() <= 1;
        // The contour: S0 at (10, 7 mm), Q 0.057 mm up the right side, C a line in, and the
        // tail down to where the hole's offset crosses the contour's.
        let outer = &loops[0];
        assert_eq!(outer[0], nm(9_580_001, 6_419_999), "the tail ends at the crossing");
        assert!(near(outer[1], nm(9_580_001, 7_056_561)), "from C: {:?}", outer[1]);
        assert_eq!(&outer[2..], &[nm(10_000_000, 7_056_561), nm(10_000_000, 10_000_000), nm(0, 10_000_000), nm(0, 0), nm(10_000_000, 0), nm(10_000_000, 7_000_000)]);
        // The hole: S0 its first point, Q 0.057 mm along its bottom, C on the hole's grown
        // bottom, and the tail out to the crossing.
        let hole = &loops[1];
        assert_eq!(hole[0], nm(9_580_001, 3_580_001), "the tail ends at the crossing");
        assert!(near(hole[1], nm(8_956_561, 3_580_001)), "from C: {:?}", hole[1]);
        assert_eq!(&hole[2..], &[nm(8_956_561, 4_000_000), nm(9_500_000, 4_000_000), nm(9_500_000, 6_000_000), nm(8_900_000, 6_000_000), nm(8_900_000, 4_000_000)]);
        assert!(area2(&hole[2..]) > 0.0, "anticlockwise");
    }

    /// §8.3: a tube is cut as the reference cuts it. On an octahedron: at its equator, the four
    /// corners in the plane, each once, from the faces under it only; at a third of its height,
    /// the edge crossings rounded half up (to −666 667 nm, not −666 666); at its poles, nothing.
    /// Each section runs anticlockwise.
    #[test]
    fn a_tube_is_cut_as_the_reference_cuts_it() {
        let verts: Vec<V3> = vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 0.0, -1.0]];
        let tris = vec![[0, 1, 4], [1, 2, 4], [2, 3, 4], [3, 0, 4], [1, 0, 5], [2, 1, 5], [3, 2, 5], [0, 3, 5]];
        let cut = sections(&Tube { verts, tris, ..Tube::default() }, &[-1.0, 0.0, 1.0 / 3.0, 1.0]);
        assert!(cut[0].is_empty() && cut[3].is_empty(), "nothing at the poles");
        let mut equator = cut[1].clone();
        assert_eq!(equator.len(), 1);
        assert!(equator[0].area2() > 0, "anticlockwise");
        equator[0].pts.sort();
        assert_eq!(equator[0].pts, vec![nm(-1_000_000, 0), nm(0, -1_000_000), nm(0, 1_000_000), nm(1_000_000, 0)], "the corners, once each");
        let mut third = cut[2].clone();
        assert_eq!(third.len(), 1);
        third[0].pts.sort();
        assert_eq!(third[0].pts, vec![nm(-666_667, 0), nm(0, -666_667), nm(0, 666_667), nm(666_667, 0)], "half up");
    }

    /// §8.5, C7 and the last chord: in a crack whose two first points tie, the previous side goes
    /// first. Its next point is too far from the next side's end, so it stops there, and the next
    /// side zips one point on before it too is held. Both held off with the crack widening, both
    /// ends slide by t = (clip − chord) ÷ (next chord − chord) towards their next points, each
    /// move truncated. Had the next side gone first, the previous side's first point would have
    /// been zipped away.
    #[test]
    fn a_crack_zips_the_previous_side_first_and_slides_both_ends() {
        let clip = grid_width(0.42);
        let (v, p1, p2, p3) = (nm(0, 0), nm(50_000, 200_000), nm(150_000, 600_000), nm(400_000, 2_000_000));
        let (n1, n2, n3, n4) = (nm(-50_000, 200_000), nm(-100_000, 450_000), nm(-300_000, 900_000), nm(-500_000, 2_000_000));
        let ring = vec![nm(-5_000_000, -5_000_000), nm(5_000_000, -5_000_000), nm(5_000_000, 2_000_000), p3, p2, p1, v, n1, n2, n3, n4, nm(-5_000_000, 2_000_000)];
        let out = smooth_ring(&ring, clip).unwrap();
        let f = |p: Pt| (p.x as f64, p.y as f64);
        let cur = d2(f(p1), f(n2));
        let t = (clip - cur) / (d2(f(p2), f(n3)) - cur);
        let slid = |a: Pt, b: Pt| nm(a.x + ((b.x - a.x) as f64 * t).trunc() as i64, a.y + ((b.y - a.y) as f64 * t).trunc() as i64);
        assert!(!out.contains(&v) && !out.contains(&n1), "the corner and the next side's first point go");
        assert!(out.contains(&slid(p1, p2)), "the previous side's first point stays, slid towards p2: {out:?}");
        assert!(out.contains(&slid(n2, n3)), "the next side's end slid towards n3");
        assert!(out.contains(&p2) && out.contains(&n3), "their next points stay");
    }

    /// §8.5, D19 (Fab's, for the reference's D10): with the next side blocked (its next point
    /// inside the chord) and the previous side held off, the far end moves to the point F of its
    /// next edge 0.42 mm from the blocked end, which stays where it is, so the last chord is the
    /// clip long. The reference moves the blocked end by the far end's way to F instead, off its
    /// own side into the crack's mouth, which leaves the chord far under the clip and a tooth.
    #[test]
    fn a_crack_with_one_side_blocked_moves_the_far_end() {
        let clip = grid_width(0.42);
        let side = |k: i64| nm(5_000_000 + 50_000 * k, 7_000_000 + 200_000 * k);
        let (v, n1, n2) = (nm(5_000_000, 7_000_000), nm(4_950_000, 7_200_000), nm(4_970_000, 7_100_000));
        let mut ring = vec![nm(0, 0), nm(10_000_000, 0), nm(10_000_000, 10_000_000)];
        ring.extend((1..=15).rev().map(side));
        ring.extend([v, n1, n2, nm(4_000_000, 7_000_000), nm(4_000_000, 10_000_000), nm(0, 10_000_000)]);
        let out = smooth_ring(&ring, clip).unwrap();
        let f = |p: Pt| (p.x as f64, p.y as f64);
        let (p2, p3, b) = (f(side(2)), f(side(3)), f(n1));
        // F on p2-p3 at the clip from n1.
        let (d, o) = ((p3.0 - p2.0, p3.1 - p2.1), (p2.0 - b.0, p2.1 - b.1));
        let (qa, qb, qc) = (d.0 * d.0 + d.1 * d.1, 2.0 * (o.0 * d.0 + o.1 * d.1), o.0 * o.0 + o.1 * o.1 - clip * clip);
        let t = (-qb + (qb * qb - 4.0 * qa * qc).sqrt()) / (2.0 * qa);
        let at = nm((p2.0 + d.0 * t).trunc() as i64, (p2.1 + d.1 * t).trunc() as i64);
        assert!(out.contains(&n1), "the blocked end stays");
        assert!(!out.contains(&side(1)) && !out.contains(&v) && !out.contains(&side(2)), "the corner and the zipped point go, and the far end moves");
        assert!(out.iter().any(|q| (q.x - at.x).abs() <= 1 && (q.y - at.y).abs() <= 1), "the far end moved to F, {at:?}: {out:?}");
        assert!((at.dist(n1) - clip).abs() < 2.0, "so the last chord is the clip long");
    }

    /// §8.5 (R14 rev 18): the zip goes on at 3 points. A side there is blocked exactly when the
    /// triangle runs counter-clockwise. A clockwise triangle with short sides loses another point
    /// and goes, so a tiny hole closes. A counter-clockwise one is kept, from the ring's head.
    #[test]
    fn a_zip_goes_on_at_three_points() {
        let clip = grid_width(0.42);
        // A hole of the cover's drawn base (layer 20, member +3), 0.00065 mm²: its crack zips down
        // to a clockwise triangle of 0.00013 mm² with sides of 12-24 µm, which goes as well.
        let hole = [nm(-14_865_384, 21_708_920), nm(-14_859_134, 21_758_227), nm(-14_849_748, 21_726_914), nm(-14_843_658, 21_704_510), nm(-14_852_826, 21_705_518), nm(-14_866_295, 21_696_744), nm(-14_867_153, 21_696_047)];
        assert!(Polygon::new(hole.to_vec()).area2() < 0, "the hole runs clockwise");
        assert_eq!(smooth_ring(&hole, clip), None, "the clockwise triangle the zip reaches loses another point");
        // A dart whose notch is a crack 0.4 mm across: the zip leaves a counter-clockwise
        // triangle. Both sides are blocked there, so it is kept, starting at the ring's first
        // point, not at the previous side's end.
        let (a, b, c, d) = (Pt::from_mm(0.0, 0.0), Pt::from_mm(0.2, 0.5), Pt::from_mm(0.0, 0.2), Pt::from_mm(-0.2, 0.5));
        assert_eq!(smooth_ring(&[a, b, c, d], clip), Some(vec![a, b, d]), "the notch zips to the triangle, from its head");
    }

    /// §8.7: a foot grows 2 mm (less 0.1 µm) into its pad, in 5 steps at a 0.5 mm line, each less
    /// the part grown by 0.2 mm, so the pad stops 0.2 mm from the part and never leaks past a thin
    /// wall of it.
    #[test]
    fn a_pad_grows_2_mm_and_keeps_0_2_mm_off_the_part() {
        let foot = vec![ngon(Pt::new(0, 0), 1.0, 1440)];
        let free = pad(&foot, &[], 0.5, 0.2);
        let r = 3.0 - ISLAND_EPS;
        assert!((area(&free) - std::f64::consts::PI * r * r).abs() < 1e-3 * area(&free), "2 mm out all round: {} mm2", area(&free));
        // A wall 0.1 mm thick 0.6 mm from the foot: in one go the pad would reach 1.1 mm past it.
        let wall = ring(&[(1.6, -10.0), (1.7, -10.0), (1.7, 10.0), (1.6, 10.0)]);
        let held = pad(&foot, std::slice::from_ref(&wall), 0.5, 0.2);
        let far = held.iter().flat_map(|e| e.rings()).flat_map(|r| r.pts.iter()).map(|q| q.x_mm()).fold(f64::MIN, f64::max);
        assert!((far - 1.4).abs() < 1e-3, "it stops 0.2 mm short of the wall, and nothing gets past: {far} mm");
    }

    /// §8.7: a pad prints one sheath loop half a line in, and fill along Y, 0.446 ÷ 0.9 = 0.4956 mm
    /// apart, its lines' ends 0.45 spacings (0.2 mm) inside the sheath.
    #[test]
    fn a_pad_prints_one_sheath_and_lines_along_y() {
        let (w, sp) = (0.5, 0.446);
        let square = vec![ring(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)])];
        let (loops, lines) = pad_lines(&square, w, sp, 0.9, Pt::new(0, 0));
        assert_eq!(loops.len(), 1, "one sheath");
        let on_sheath = |q: &Pt| {
            let (x, y) = (q.x_mm(), q.y_mm());
            let near = |a: f64, b: f64| (a - b).abs() < 1e-3;
            (near(x, 0.25) || near(x, 9.75) || near(y, 0.25) || near(y, 9.75)) && (0.249..=9.751).contains(&x) && (0.249..=9.751).contains(&y)
        };
        assert!(loops[0].iter().all(on_sheath), "the sheath half a line in");
        // The fill's runs along Y, and where they end.
        let mut xs: Vec<f64> = Vec::new();
        for l in &lines {
            for s in l.windows(2) {
                if (s[0].x - s[1].x).abs() <= 2 && s[0].dist(s[1]) > SCALE {
                    xs.push(s[0].x_mm());
                    for q in s {
                        let y = q.y_mm();
                        assert!((y - (0.25 + 0.45 * sp)).abs() < 1e-3 || (y - (9.75 - 0.45 * sp)).abs() < 1e-3, "a run ends 0.45 spacings inside the sheath: {y}");
                    }
                }
            }
        }
        xs.sort_by(f64::total_cmp);
        xs.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        assert!(xs.len() > 15, "{} runs", xs.len());
        assert!(xs.windows(2).all(|p| (p[1] - p[0] - sp / 0.9).abs() < 1e-5), "0.4956 mm apart: {xs:?}");
        let pitch = sp / 0.9;
        assert!(xs.iter().all(|x| (x - (x / pitch).round() * pitch).abs() < 1e-4), "on the grid through the pivot, (0, 0): {xs:?}");
    }

    /// §8.7 with §1.5: the fill area is the sheath shrunk by 0.4 spacings, 178 539.594 nm, then by
    /// the filler's outer set, 22 317 nm, each shrink dropping close points at its own threshold. A
    /// point 949 nm from a corner, 300 nm out of the bottom edge, is past the first shrink's
    /// 892.698 nm, so it stays and tilts the edge: the fill area's corner by it comes out 287 nm
    /// lower than the far one, which sits 6 nm under the 200 856.6 nm of the two shrinks for the
    /// edge's own slope. One 0.45-spacing shrink, at 1 004 nm, would drop the point and leave the
    /// edge level.
    #[test]
    fn a_pads_fill_area_shrinks_twice_at_its_own_thresholds() {
        let sp = flow::spacing(0.5, 0.25);
        assert_eq!(grid_width(sp), 446_349.0);
        assert_eq!((0.4 * grid_width(sp)) as f32, 178_539.594, "the first shrink, as the table has it");
        let p = |x: i64, y: i64| Pt::new(x, y);
        let sheath = vec![ExPolygon::new(Polygon::new(vec![p(0, 0), p(900, -300), p(10_000_000, 0), p(10_000_000, 10_000_000), p(0, 10_000_000)]))];
        let area = pad_fill_area(&sheath, sp);
        assert_eq!(area.len(), 1);
        let low = |near_x: i64| area[0].contour.pts.iter().filter(|q| (q.x - near_x).abs() < 1_000_000 && q.y < 5_000_000).map(|q| q.y).min().unwrap();
        let (left, right) = (low(0), low(10_000_000));
        assert!((200_849..=200_851).contains(&right), "0.45 spacings in by the far corner, less the edge's slope: {right} nm");
        assert!((right - left - 287).abs() <= 2, "the near corner {left} nm, the far one {right} nm");
    }

    /// §8.6, §8.7 as paths: open support paths in the bead given, a base's loops and a pad's
    /// sheath before its fill.
    #[test]
    fn base_and_pad_paths_print_in_their_beads() {
        let bead = Bead { w: 0.42, h: 0.2, speed: 150.0 };
        let base = base_paths(&[ngon(Pt::new(0, 0), 2.75, 1440)], 10.0, 0, bead);
        assert_eq!(base.len(), 2, "two walls");
        assert!(base.iter().all(|p| p.role == Role::Support && !p.closed && p.width == 0.42 && p.height == 0.2 && p.speed == 150.0 && (p.area - flow::cross_section(0.42, 0.2)).abs() < 1e-12));
        let first = Bead { w: 0.5, h: 0.25, speed: 50.0 };
        let printed = pad_paths(&pad(&[ngon(Pt::new(0, 0), 1.0, 1440)], &[], 0.5, 0.2), 0.9, Pt::new(0, 0), first);
        assert!(printed.len() > 1 && printed.iter().all(|p| p.role == Role::Support && p.width == 0.5 && p.speed == 50.0));
        assert!(printed[0].pts.iter().all(|&q| (radius(q) - (3.0 - ISLAND_EPS - 0.25)).abs() < 2e-3), "the sheath first, half a line in");
    }

    /// Heights as R14's profile cuts them: a 0.25 mm first layer, then 0.2 mm.
    fn heights(n: usize) -> Vec<(f64, f64)> {
        (0..n).map(|l| if l == 0 { (0.25, 0.25) } else { (0.25 + 0.2 * l as f64, 0.2) }).collect()
    }

    /// A straight column of elements from `from` to `to` (layers) at `at`, its tip on `to`.
    fn column(s: &Settings, n: usize, from: usize, to: usize, at: Pt) -> Vec<Vec<Element>> {
        let mut trees: Vec<Vec<Element>> = vec![Vec::new(); n];
        for l in from..=to {
            let mut e = Element::tip(s, l, at);
            e.depth = to - l;
            e.ramp = to - l;
            e.placed = Some(at);
            if l < to {
                e.carries = vec![0];
            }
            trees[l].push(e);
        }
        trees
    }

    /// §8.1: a root, a fork and two tips make three branches, the fork the top of one and the
    /// bottom of the two others, depth first. A root that forks at once has root on both its
    /// branches; an element that neither carries nor is carried is not drawn.
    #[test]
    fn branches_run_between_forks() {
        let s = r14(0.35);
        let at = Pt::from_mm(0.0, 0.0);
        let mut trees = column(&s, 6, 0, 2, at);
        trees[2][0].carries = vec![0, 1];
        for (k, x) in [(0usize, -1.0), (1, 1.0)] {
            for l in 3..6 {
                let mut e = Element::tip(&s, l, Pt::from_mm(x, 0.0));
                e.placed = Some(e.point);
                if l < 5 {
                    e.carries = vec![k];
                }
                trees[l].push(e);
            }
        }
        let b = branches(&trees);
        assert_eq!(
            b,
            vec![
                Branch { path: vec![(0, 0), (1, 0), (2, 0)], root: true, tip: false },
                Branch { path: vec![(2, 0), (3, 0), (4, 0), (5, 0)], root: false, tip: true },
                Branch { path: vec![(2, 0), (3, 1), (4, 1), (5, 1)], root: false, tip: true },
            ],
            "the trunk to the fork, then the fork's arms"
        );
        let mut fork = column(&s, 3, 0, 2, at);
        fork[0][0].carries = vec![0, 1];
        let mut arm = fork[1][0].clone();
        arm.carries = vec![1];
        fork[1].push(arm);
        let tip = fork[2][0].clone();
        fork[2].push(tip);
        let mut lone = fork[0][0].clone();
        lone.carries.clear();
        fork[0].push(lone);
        let b = branches(&fork);
        assert_eq!(b.len(), 2, "the lone element is not drawn");
        assert!(b.iter().all(|b| b.root && b.tip && b.path.len() == 3), "both arms of a forking root have root");
    }

    /// §8.2, §8.3: a column on the bed stands on a flat foot, so layer 0 (cut at 0.125 mm) is the
    /// root's ring; each layer above is cut 0.1 mm under its ring, between two rings of their own
    /// corner counts; and a branch to a tip is not cut over the tip's layer.
    #[test]
    fn a_column_on_the_bed_has_a_flat_foot_and_ends_at_its_tip() {
        let s = r14(0.35);
        let n = 20;
        let part: Vec<Vec<ExPolygon>> = vec![Vec::new(); n];
        let v = Volumes::new(Ctl::default(), &s, &part, &[]).unwrap();
        let at = Pt::from_mm(0.0, 0.0);
        let trees = column(&s, n, 0, 9, at);
        let d = draw(Ctl::default(), &v, &heights(n), &bed(), &trees, &[], &mut Memo::default()).unwrap();
        let polygon = |r: f64| {
            let k = sides(r) as f64;
            k / 2.0 * r * r * (std::f64::consts::TAU / k).sin()
        };
        let r0 = trees[0][0].drawn_radius(&s);
        let foot = area(&d.base[0]);
        assert!((foot - polygon(r0)).abs() < 1e-3 * foot, "the foot is the root's ring: {foot} mm2 at r {r0}");
        // Layer 5 is cut half way between the rings of layers 4 and 5: between their areas, and
        // near a disc of their mean radius (short of it by the corners' chords and twist).
        let (r4, r5) = (trees[4][0].drawn_radius(&s), trees[5][0].drawn_radius(&s));
        let (a5, disc) = (area(&d.base[5]), std::f64::consts::PI * ((r4 + r5) / 2.0).powi(2));
        assert!(polygon(r5) < a5 && a5 < polygon(r4) && (a5 - disc).abs() < 0.03 * disc, "between two rings: {a5} mm2, a disc at their mean radius {disc}");
        assert!((1..9).all(|l| area(&d.base[l + 1]) < area(&d.base[l])), "the sections shrink towards the tip");
        assert!(!d.base[9].is_empty() && d.base[10].is_empty(), "nothing over the tip's layer");
    }

    /// Fab's departure from §8.2: a tip whose ramp counter lags (held at 0 by moves that could
    /// not grow) is drawn by its depth inside the ramp and by the counter below it, a ring under
    /// the ramp's widest; drawn at its width, as wide as what it carries, the sections never
    /// shrink going down.
    #[test]
    fn a_branch_never_narrows_going_down() {
        let s = r14(0.35);
        let n = 20;
        let at = Pt::from_mm(0.0, 0.0);
        let mut trees = column(&s, n, 0, 9, at);
        // The first element under the ramp, at depth tip_layers.
        let k = 9 - s.tip_layers;
        trees[k][0].ramp = 0;
        let (over, under) = (trees[k + 1][0].drawn_radius(&s), trees[k][0].drawn_radius(&s));
        assert!(under < over, "the reference's radii narrow it: {under} mm under {over} mm");
        // What each element carries, as the descent's steps carry it down.
        for l in (0..9).rev() {
            trees[l][0].carried = trees[l + 1][0].width(&s);
        }
        assert_eq!(trees[k][0].width(&s), over, "drawn as wide as what it carries");
        let part: Vec<Vec<ExPolygon>> = vec![Vec::new(); n];
        let v = Volumes::new(Ctl::default(), &s, &part, &[]).unwrap();
        let d = draw(Ctl::default(), &v, &heights(n), &bed(), &trees, &[], &mut Memo::default()).unwrap();
        assert!((1..9).all(|l| area(&d.base[l + 1]) <= area(&d.base[l]) * (1.0 + 1e-6)), "the sections never grow going up");
    }

    /// §8.2, §8.4: a root above layer 0 has no ring at its own point (its bottom cap stops a step
    /// short of its rim), so the root layer's plane, 0.1 mm under the point, cuts the strip from
    /// the cap's last ring to the next element's ring, one point a strip triangle. R14's check
    /// values for a vertical branch: at r 0.4, 23 points at 0.350-0.360 mm, 78 % of π r²; at r 1,
    /// 37 points at 0.958-0.971 mm, 93 %.
    #[test]
    fn a_root_off_the_bed_is_cut_smaller_than_its_ring() {
        let print: Vec<f64> = heights(20).iter().map(|h| h.0).collect();
        for (r, points, lo, hi, share) in [(0.4, 23, 0.350, 0.360, 0.78), (1.0, 37, 0.958, 0.971, 0.93)] {
            let rings: Vec<[f64; 4]> = (10..14).map(|l| [0.0, 0.0, print[l], r]).collect();
            let (mesh, low, _) = tube(&rings, false, false);
            assert!((low - (print[10] - r)).abs() < 1e-5, "the bottom pole r under the root");
            let loops = sections(&mesh, &[planes(&heights(20))[10] as f32]).remove(0);
            assert_eq!(loops.len(), 1);
            assert_eq!(loops[0].len(), points, "r {r}: one point a strip triangle");
            assert!(loops[0].pts.iter().all(|&q| (lo - 1e-3..=hi + 1e-3).contains(&radius(q))), "r {r}: at {lo}-{hi} mm");
            let got = loops[0].area_mm2() / (std::f64::consts::PI * r * r);
            assert!((got - share).abs() < 0.01, "r {r}: {got} of π r²");
        }
    }

    /// §8.3, D8 (copied for parity): a branch that ends at a fork is cut up to the highest layer
    /// whose print z is at or under its top pole. With the pole 0.95 mm over the fork's ring,
    /// layer fork + 5 is left uncut (print z 1 mm over), though its plane (0.9 mm over) is under
    /// the pole. A branch from a fork is cut from the lowest layer whose print z is at or over its
    /// bottom pole.
    #[test]
    fn a_fork_cap_is_cut_up_to_the_print_z_under_its_pole() {
        let print: Vec<f64> = heights(40).iter().map(|h| h.0).collect();
        let rings: Vec<[f64; 4]> = (10..=20).map(|l| [0.0, 0.0, print[l], 0.95]).collect();
        let (_, low, high) = tube(&rings, false, false);
        assert!((high - (print[20] + 0.95)).abs() < 1e-5, "the top pole 0.95 mm over the fork");
        let fork = Branch { path: (10..=20).map(|l| (l, 0)).collect(), root: false, tip: false };
        let (from, to) = span(&fork, 10, 20, low, high, &print, 0.2);
        assert_eq!(to, 24, "layer 25's print z is over the pole");
        assert!(planes(&heights(40))[25] < high, "though its plane is under it");
        assert_eq!(from, 6, "layer 6 is the lowest whose print z is at or over the bottom pole");
        let tip = Branch { tip: true, root: true, ..fork };
        assert_eq!(span(&tip, 10, 20, low, high, &print, 0.2), (10, 20), "from a root and to a tip, the elements' own layers");
    }

    /// D27: a 0.4 mm arm a merge left 2.4 mm out on a trunk 2.94 mm wide, its first step flatter
    /// than one in five, leaves the fork level: the cap under the fork faces straight down, its
    /// cut from the fork's width under the fork, and nothing of it over its first element reaches
    /// past its own ring. One 0.6 mm out, steeper, leaves along its first step as the reference's
    /// does, the fork's ring on its side standing over its first element beside it.
    #[test]
    fn a_branch_a_merge_left_off_its_fork_leaves_it_level() {
        let (s, n, ctl) = (r14(0.35), 40, Ctl::default());
        let v = Volumes::new(ctl, &s, &vec![Vec::new(); n], &[]).unwrap();
        let print: Vec<f64> = heights(n).iter().map(|h| h.0).collect();
        for (off, far) in [(2.4, true), (0.6, false)] {
            assert_eq!(far, off > LEVEL_RUN * (print[21] - print[20]), "{off} mm against a rise of {} mm", print[21] - print[20]);
            let mut trees = column(&s, n, 0, 35, Pt::from_mm(0.0, 0.0));
            for layer in &mut trees[..=35] {
                layer[0].carried = 2.94;
            }
            trees[20][0].carries = vec![0, 1];
            for (l, mut arm) in column(&s, n, 21, 35, Pt::from_mm(off, 0.0)).into_iter().enumerate().skip(21) {
                for e in &mut arm {
                    e.carries.iter_mut().for_each(|c| *c = 1);
                    (e.depth, e.ramp) = (0, 0);
                }
                trees[l].extend(arm);
            }
            let (r0, r) = (trees[20][0].width(&s), trees[21][1].width(&s));
            assert_eq!((r0, r), (2.94, s.r_tip), "the trunk carries 2.94 mm, the arm is a tip's width");
            let mut memo = Memo::default();
            draw(ctl, &v, &heights(n), &bed(), &trees, &[], &mut memo).unwrap();
            let arm = memo.branches.iter().find(|(k, _)| !k.root && (f64::from_bits(k.rings[1][0]) - off).abs() < 1e-6).map(|(_, a)| a).expect("the arm");
            let own = |p: &Pt| (p.x_mm() - off).hypot(p.y_mm()) <= r + 1e-3;
            let over = &arm.cut.slices[22 - arm.cut.first..];
            let within = !over.is_empty() && over.iter().flatten().all(|e| e.contour.pts.iter().all(own));
            if far {
                assert_eq!(arm.cut.first, ((print[20] - r0 - print[0]) / s.keys.layer_height).ceil() as usize, "from the fork's width under the fork");
                assert!(within, "over its first element, within its own ring");
            } else {
                assert!(!within, "the fork's ring stands over its first element");
            }
        }
    }

    /// §8.4: a root off the model drops its slice, at most ⌊5 r ÷ 0.2⌋ layers, and stops before a
    /// remainder under max(0.2 π r², 0.5 π r_br²); stuck, it drops to the bed.
    #[test]
    fn a_root_off_the_model_drops_and_stops() {
        let s = r14(0.35);
        let n = 80;
        let at = Pt::from_mm(0.0, 0.0);
        // No part: a root on layer 50 drops its own section all the way.
        let part: Vec<Vec<ExPolygon>> = vec![Vec::new(); n];
        let v = Volumes::new(Ctl::default(), &s, &part, &[]).unwrap();
        let trees = column(&s, n, 50, 70, at);
        let d = draw(Ctl::default(), &v, &heights(n), &bed(), &trees, &[], &mut Memo::default()).unwrap();
        let r = trees[50][0].drawn_radius(&s);
        let reach = (5.0 * r / 0.2 + 1e-9).floor() as usize;
        assert_eq!(reach, 31, "5 × {r} mm over 0.2 mm layers, rounded down");
        let lowest = (0..n).find(|&l| !d.base[l].is_empty()).unwrap();
        assert_eq!(lowest, 50 - reach, "the drop's last layer");
        assert!((area(&d.base[lowest]) - area(&d.base[50])).abs() < 1e-6, "the root's own section");
        // Stuck, it drops to the bed.
        let mut stuck = trees.clone();
        stuck[50][0].stuck = true;
        let d = draw(Ctl::default(), &v, &heights(n), &bed(), &stuck, &[], &mut Memo::default()).unwrap();
        assert!(!d.base[0].is_empty(), "a stuck root drops to the bed");
        // A wall under it from layer 30 down (its collision, with the bottom gap, from 31) leaves
        // a remainder under 0.5 π r_br² there: the drop stops on layer 32.
        let wall: Vec<Vec<ExPolygon>> = (0..n).map(|l| if l <= 30 { vec![ring(&[(-5.0, -5.0), (5.0, -5.0), (5.0, 0.3), (-5.0, 0.3)])] } else { Vec::new() }).collect();
        let v = Volumes::new(Ctl::default(), &s, &wall, &[]).unwrap();
        let trees = column(&s, n, 40, 70, at);
        let d = draw(Ctl::default(), &v, &heights(n), &bed(), &trees, &[], &mut Memo::default()).unwrap();
        let r = trees[40][0].drawn_radius(&s);
        let h = r - 0.3 - s.keys.xy_distance;
        let left = r * r * ((r - h) / r).acos() - (r - h) * (2.0 * r * h - h * h).sqrt();
        assert!(left < 0.5 * std::f64::consts::PI, "the wall leaves under {left} mm2, under 0.5 π r_br²");
        let lowest = (0..n).find(|&l| !d.base[l].is_empty()).unwrap();
        assert_eq!(lowest, 32, "the drop stops over the wall's collision");
    }

    /// D14, Fab's: where the part cuts a branch's slice, the slices over it come back a fast move
    /// a layer (R2), not all at once. A wall on layers 20 to 22 takes the +x side of a 1 mm
    /// column; over the last layer its collision cuts, each section lies within the fast move of
    /// the one under, the first still short of the column, and the column is whole again within
    /// the fast moves its cut side takes to grow back.
    #[test]
    fn a_branch_the_part_cut_grows_back_a_fast_move_a_layer() {
        let s = r14(0.35);
        let n = 60;
        let hs = heights(n);
        let print: Vec<f64> = hs.iter().map(|h| h.0).collect();
        let planes: Vec<f32> = planes(&hs).into_iter().map(|z| z as f32).collect();
        let (mesh, _, _) = tube(&(0..=50).map(|l| [0.0, 0.0, print[l], 1.0]).collect::<Vec<_>>(), true, false);
        let wall: Vec<Vec<ExPolygon>> = (0..n).map(|l| if (20..=22).contains(&l) { vec![ring(&[(0.0, -5.0), (5.0, -5.0), (5.0, 5.0), (0.0, 5.0)])] } else { Vec::new() }).collect();
        let v = Volumes::new(Ctl::default(), &s, &wall, &[]).unwrap();
        let bed_area = bed();
        let c = cut(Ctl::default(), &v, &planes, &Bed::new(&bed_area), &mesh, 1..=49, None, None).unwrap();
        assert_eq!((c.first, c.slices.len()), (1, 49), "a slice on every layer");
        let slice = |l: usize| c.slices[l - 1].as_slice();
        let whole = slice(5).to_vec();
        let is_whole = |l: usize| (area(slice(l)) - area(&whole)).abs() < CUT_AREA;
        let takes = |l: usize| !intersection_ex(&whole, v.collision(Ctl::default(), l, 0).unwrap()).is_empty();
        let last = (1..=49).filter(|&l| takes(l)).max().unwrap();
        assert!((20..=22).all(takes) && last < 30, "the wall's collision cuts layers 20 to 22, and up to {last}");
        for l in last + 1..=49 {
            let past = difference_ex(slice(l), &offset(slice(l - 1), (s.fast + 1e-3) * SCALE, Join::Round(LEAN_ARC * SCALE)));
            assert!(area(&past) < 1e-6, "layer {l} lies within the fast move of the one under: {} mm2 past it", area(&past));
        }
        assert!(area(slice(last + 1)) < area(&whole) - 0.1, "the first layer over the cut is not whole at once");
        let back = (last + 1..=49).find(|&l| is_whole(l)).expect("whole again");
        assert!((back..=49).all(is_whole), "and whole from there on");
        // The wall and its side gap take the column from x = −xy: 1 + xy to grow back.
        let moves = ((1.0 + s.keys.xy_distance) / s.fast).ceil() as usize;
        assert!(back - last <= moves + 2, "whole again {} layers over the cut, {moves} fast moves", back - last);
    }

    /// D14 follows the branch: a column leaning 0.25 mm a layer, past the fast move, along a wall
    /// that takes its +y side on every layer keeps what the wall leaves it on every layer, less
    /// what prints narrower than two lines (D16), to 0.5 % (its sections are cut anew each
    /// layer): the slice under moved with it, not a slice that the lean, or the opening, shrinks a
    /// layer at a time. It is cut clear of its end caps, where its sections are the same ellipse
    /// moved.
    #[test]
    fn a_branch_the_part_cuts_as_it_moves_keeps_what_the_part_leaves() {
        let s = r14(0.35);
        let n = 60;
        let hs = heights(n);
        let print: Vec<f64> = hs.iter().map(|h| h.0).collect();
        let planes: Vec<f32> = planes(&hs).into_iter().map(|z| z as f32).collect();
        assert!(0.25 > s.fast, "it moves past the fast move");
        let (mesh, _, _) = tube(&(10..=50).map(|l| [0.25 * (l - 10) as f64, 0.0, print[l], 1.0]).collect::<Vec<_>>(), false, false);
        let bed_area = bed();
        let wall: Vec<Vec<ExPolygon>> = vec![vec![ring(&[(-20.0, 0.5), (40.0, 0.5), (40.0, 5.0), (-20.0, 5.0)])]; n];
        let (open, walled) = (Volumes::new(Ctl::default(), &s, &vec![Vec::new(); n], &[]).unwrap(), Volumes::new(Ctl::default(), &s, &wall, &[]).unwrap());
        let cut_in = |v: &Volumes| cut(Ctl::default(), v, &planes, &Bed::new(&bed_area), &mesh, 16..=44, None, None).unwrap();
        let (whole, left) = (cut_in(&open), cut_in(&walled));
        assert_eq!((whole.first, left.first, left.slices.len()), (16, 16, 29), "a slice on every layer");
        for l in 17..=44 {
            let all = whole.slices[l - 16].as_slice();
            let leaves = difference_ex(all, walled.collision(Ctl::default(), l, 0).unwrap());
            assert!(0.3 * area(all) < area(&leaves) && area(&leaves) < 0.9 * area(all), "the wall takes the +y side on layer {l}: {} of {} mm2", area(&leaves), area(all));
            let prints = area(&crate::support_rules::wide(&leaves, &[], s.keys.line_width));
            let kept = area(&left.slices[l - 16]);
            assert!((kept - prints).abs() < 0.005 * prints, "layer {l} keeps what the wall leaves it that prints: {kept} mm2 of {prints}");
        }
    }

    /// D17, Fab's: a branch 4 mm wide where it bends, narrowing 0.0175 mm a layer, that leans
    /// 0.18 mm a layer and then 0.11 folds its tube on its high side, where a layer's loops leave
    /// a horn apart from the section. That layer takes in the hull of its loops as far as the fast
    /// move from the slice under it: every layer is one island, holding every loop, adding nothing
    /// past the fast move from the layer under it, and where the loops make one island it is that
    /// island.
    #[test]
    fn a_bent_branch_is_cut_one_island_a_layer() {
        let s = r14(0.35);
        let n = 80;
        let hs = heights(n);
        let print: Vec<f64> = hs.iter().map(|h| h.0).collect();
        let planes: Vec<f32> = planes(&hs).into_iter().map(|z| z as f32).collect();
        let rings: Vec<[f64; 4]> = (10..=70)
            .map(|l| {
                let x = if l <= 30 { 0.18 * (l - 10) as f64 } else { 3.6 + 0.11 * (l - 30) as f64 };
                [x, 0.0, print[l], 4.0 + 0.0175 * (30.0 - l as f64)]
            })
            .collect();
        let (mesh, _, _) = tube(&rings, false, false);
        // The loops as R14 unites them.
        let united = |loops: &[Polygon]| -> Vec<ExPolygon> {
            let mut all = loops.to_vec();
            all.retain(|p| p.len() >= 3);
            for p in &mut all {
                if p.area2() < 0 {
                    p.reverse();
                }
            }
            union(&all, FillRule::NonZero)
        };
        let raw: Vec<Vec<ExPolygon>> = sections(&mesh, &planes[12..=68]).iter().map(|loops| united(loops.as_slice())).collect();
        assert!(raw.iter().any(|set| set.len() > 1), "the tube folds where it bends");
        let bed_area = bed();
        let v = Volumes::new(Ctl::default(), &s, &vec![Vec::new(); n], &[]).unwrap();
        let c = cut(Ctl::default(), &v, &planes, &Bed::new(&bed_area), &mesh, 12..=68, None, None).unwrap();
        assert_eq!((c.first, c.slices.len()), (12, 57), "a slice on every layer");
        for (k, set) in raw.iter().enumerate() {
            let got = &c.slices[k];
            assert_eq!(got.len(), 1, "layer {}: one island, of {}", 12 + k, set.len());
            assert!(area(&difference_ex(set, got)) < 1e-6, "layer {}: holding every loop", 12 + k);
            if set.len() == 1 {
                assert!((area(got) - area(set)).abs() < 1e-6, "layer {}: the loops' own island", 12 + k);
            } else {
                let reach = offset(&c.slices[k - 1], s.fast * SCALE + 1000.0, Join::Round(LEAN_ARC * SCALE));
                let past = area(&difference_ex(&difference_ex(got, set), &reach));
                assert!(past < 1e-6, "layer {}: nothing added past the fast move: {past} mm2", 12 + k);
            }
        }
    }

    /// D18, Fab's: a column round a post's corner, which the collision's mitre leaves a notch of
    /// 90° in on every layer it is wider than the corner's reach. §8.5 cuts that notch by a chord,
    /// into the collision; the base keeps out of it all the same, the notch and all, and keeps
    /// every other part of the section that prints (D16).
    #[test]
    fn a_smoothed_base_keeps_out_of_the_collision() {
        let s = r14(0.35);
        let n = 20;
        let post = vec![ring(&[(0.3, 0.3), (5.0, 0.3), (5.0, 5.0), (0.3, 5.0)])];
        let v = Volumes::new(Ctl::default(), &s, &vec![post; n], &[]).unwrap();
        let open = Volumes::new(Ctl::default(), &s, &vec![Vec::new(); n], &[]).unwrap();
        let trees = column(&s, n, 0, 9, Pt::from_mm(0.0, 0.0));
        let d = draw(Ctl::default(), &v, &heights(n), &bed(), &trees, &[], &mut Memo::default()).unwrap();
        let whole = draw(Ctl::default(), &open, &heights(n), &bed(), &trees, &[], &mut Memo::default()).unwrap();
        let clip = grid_width(s.keys.line_width);
        let mut notched = 0;
        for l in 1..=7 {
            let part = v.collision(Ctl::default(), l, 0).unwrap();
            let base = &d.base[l];
            assert!(area(&intersection_ex(base, part)) < 1e-9, "layer {l}: nothing in the collision");
            if area(&intersection_ex(&smooth_outward(base, clip), part)) > 0.03 {
                notched += 1;
            }
            // As near as two simplifications of the section's edge agree.
            let prints = crate::support_rules::wide(&difference_ex(&whole.base[l], part), &[], s.keys.line_width);
            let left = area(&difference_ex(&prints, base));
            assert!(left < 5e-3, "layer {l}: all of the section the collision leaves: {left} mm2 short");
        }
        assert!(notched >= 3, "the notch, which a chord into the collision cuts, stays on {notched} layers");
    }

    /// D16, Fab's: what the part's cut leaves narrower than two lines does not print (R6). A
    /// wall through a 1 mm column on every layer, 0.3 to 0.5 mm off its axis, leaves a body on
    /// one side and a strip under two lines past it: the strip goes, the body stays (its corners
    /// opened). A wall that leaves only the strip leaves it: the branch over it stands on it.
    #[test]
    fn a_cut_leaves_nothing_under_two_lines_beside_what_it_keeps() {
        let s = r14(0.35);
        let n = 30;
        let hs = heights(n);
        let print: Vec<f64> = hs.iter().map(|h| h.0).collect();
        let planes: Vec<f32> = planes(&hs).into_iter().map(|z| z as f32).collect();
        let (mesh, _, _) = tube(&(0..=20).map(|l| [0.0, 0.0, print[l], 1.0]).collect::<Vec<_>>(), true, false);
        let bed_area = bed();
        let band = |x0: f64, x1: f64| vec![ring(&[(x0, -5.0), (x1, -5.0), (x1, 5.0), (x0, 5.0)])];
        let (body, past) = (band(-5.0, 0.3), band(0.5, 5.0));
        let open = Volumes::new(Ctl::default(), &s, &vec![Vec::new(); n], &[]).unwrap();
        let whole = cut(Ctl::default(), &open, &planes, &Bed::new(&bed_area), &mesh, 1..=19, None, None).unwrap();
        for (from, strip_goes) in [(0.3, true), (-5.0, false)] {
            let v = Volumes::new(Ctl::default(), &s, &vec![band(from, 0.5); n], &[]).unwrap();
            let c = cut(Ctl::default(), &v, &planes, &Bed::new(&bed_area), &mesh, 1..=19, None, None).unwrap();
            assert_eq!((c.first, c.slices.len()), (1, 19), "a slice on every layer");
            for l in 1..=19 {
                let leaves = difference_ex(&whole.slices[l - 1], v.collision(Ctl::default(), l, 0).unwrap());
                let strip = intersection_ex(&leaves, &past);
                assert!(area(&strip) > 0.05 && crate::support_rules::wide(&strip, &[], s.keys.line_width).is_empty(), "the wall leaves a strip under two lines on layer {l}");
                let slice = &c.slices[l - 1];
                if strip_goes {
                    assert!(intersection_ex(slice, &past).is_empty(), "the strip goes on layer {l}");
                    let kept = area(&intersection_ex(slice, &body));
                    assert!(kept > 0.9 * area(&intersection_ex(&leaves, &body)), "the body stays on layer {l}: {kept} mm2");
                } else {
                    assert!((area(slice) - area(&leaves)).abs() < 1e-6, "a strip alone stays on layer {l}");
                }
            }
        }
    }

    /// Pieces join a group through a chain of boxes within the engine's nearness of each other; a
    /// piece farther off is a group of its own. The groups go by their first piece.
    #[test]
    fn pieces_group_through_chains_of_near_boxes() {
        let sq = |x: f64| vec![ring(&[(x, 0.0), (x + 1.0, 0.0), (x + 1.0, 1.0), (x, 1.0)])];
        // 1 mm squares: at 0, 3 mm off, and 1 mm plus 3 nm on (within 4 nm of the first: their
        // boxes widened by 2 nm meet), then one at 6 mm.
        let (a, b, c, d) = (sq(0.0), sq(3.0), sq(1.000_003), sq(6.0));
        let pieces: Vec<(Id, &[ExPolygon])> = vec![(0, &b), (1, &a), (2, &c), (3, &d)];
        assert_eq!(apart(&pieces), vec![vec![0], vec![1, 2], vec![3]]);
        let (e, f) = (sq(1.000_005), sq(2.000_008));
        let pieces: Vec<(Id, &[ExPolygon])> = vec![(0, &a), (1, &e), (2, &f)];
        assert_eq!(apart(&pieces), vec![vec![0], vec![1, 2]], "5 nm apart is apart; 3 nm joins");
    }

    /// A drawing takes a branch it drew before from its memo and keeps only its own: two columns
    /// drawn, then one alone, draws as the one alone does afresh, and leaves one branch in the
    /// memo. That branch is taken as the memo holds it, on a new bed too whose box holds every
    /// slice; a bed whose edge cuts one starts the memo afresh.
    #[test]
    fn a_branch_drawn_before_is_taken_from_the_memo() {
        let (s, n, ctl) = (r14(0.35), 20, Ctl::default());
        let v = Volumes::new(ctl, &s, &vec![Vec::new(); n], &[]).unwrap();
        let one = column(&s, n, 0, 9, Pt::from_mm(0.0, 0.0));
        let mut two = one.clone();
        for (l, mut more) in column(&s, n, 0, 6, Pt::from_mm(5.0, 0.0)).into_iter().enumerate() {
            for e in &mut more {
                e.carries.iter_mut().for_each(|c| *c = 1);
            }
            two[l].extend(more);
        }
        let mut memo = Memo::default();
        let both = draw(ctl, &v, &heights(n), &bed(), &two, &[], &mut memo).unwrap();
        assert_eq!(memo.branches.len(), 2, "a branch per column");
        let fresh = draw(ctl, &v, &heights(n), &bed(), &one, &[], &mut Memo::default()).unwrap();
        let kept = draw(ctl, &v, &heights(n), &bed(), &one, &[], &mut memo).unwrap();
        assert!(area(&both.base[3]) > area(&fresh.base[3]) + 0.1, "the second column drew");
        assert_eq!((&kept.base, &kept.floor), (&fresh.base, &fresh.floor), "as drawn afresh");
        assert_eq!(memo.branches.len(), 1, "only this drawing's branch is kept");
        for d in memo.branches.values_mut() {
            *d = Arc::new(Branched { cut: Cut { first: 0, slices: Vec::new(), cut: Vec::new(), span: (0, 0), held: Some(BBox::empty()) }, down: Vec::new(), floor: Vec::new(), read: Reads::default() });
        }
        let taken = draw(ctl, &v, &heights(n), &bed(), &one, &[], &mut memo).unwrap();
        assert!(taken.base.iter().all(|b| b.is_empty()), "the branch is the memo's");
        // A group of pieces drawn before is the memo's too.
        let mut memo = Memo::default();
        draw(ctl, &v, &heights(n), &bed(), &two, &[], &mut memo).unwrap();
        assert_eq!(memo.bases.len(), 10 + 7, "a group per column per layer");
        for b in memo.bases.values_mut() {
            *b = Arc::new(Vec::new());
        }
        let taken = draw(ctl, &v, &heights(n), &bed(), &one, &[], &mut memo).unwrap();
        assert!(taken.base.iter().all(|b| b.is_empty()), "the groups are the memo's");
        assert_eq!(memo.bases.len(), 10, "only this drawing's groups are kept");
        let wider = vec![ring(&[(-101.0, -100.0), (100.0, -100.0), (100.0, 100.0), (-101.0, 100.0)])];
        let other = draw(ctl, &v, &heights(n), &wider, &one, &[], &mut memo).unwrap();
        assert!(other.base.iter().all(|b| b.is_empty()), "a bed whose box holds every slice takes the memo's groups");
        let edge = vec![ring(&[(0.0, -100.0), (100.0, -100.0), (100.0, 100.0), (0.0, 100.0)])];
        let cut = draw(ctl, &v, &heights(n), &edge, &one, &[], &mut memo).unwrap();
        let alone = draw(ctl, &v, &heights(n), &edge, &one, &[], &mut Memo::default()).unwrap();
        assert!(cut.base == alone.base && cut.base.iter().any(|b| !b.is_empty()), "a bed whose edge cuts a slice draws afresh");
    }

    /// A branch is taken from the memo only where the volumes have every layer its drawing read as
    /// it read it, and a group of a layer's pieces only where the part on that layer is as it was
    /// and its pieces are the ones it was joined from, each known by its branch, its layer and all
    /// its rings. A wall on layers 20 to 22 by the first of two tall columns has both drawn afresh
    /// and the short one under it taken; over the wall's collision the first grows back (D14) on
    /// a part as it was, and its groups there are joined afresh while the other's are taken.
    #[test]
    fn a_branch_or_a_group_is_taken_only_as_it_was_read() {
        let (s, n, ctl) = (r14(0.35), 60, Ctl::default());
        let hs = heights(n);
        let open = Volumes::new(ctl, &s, &vec![Vec::new(); n], &[]).unwrap();
        let mut trees = column(&s, n, 0, 50, Pt::from_mm(0.0, 0.0));
        for (k, (x, to)) in [(30.0, 50), (60.0, 10)].into_iter().enumerate() {
            for (l, mut more) in column(&s, n, 0, to, Pt::from_mm(x, 0.0)).into_iter().enumerate() {
                for e in &mut more {
                    e.carries.iter_mut().for_each(|c| *c = k + 1);
                }
                trees[l].extend(more);
            }
        }
        // A layer's base round x = `x0` to `x1` mm.
        let within = |d: &Drawn, l: usize, x0: f64, x1: f64| -> Vec<ExPolygon> {
            d.base[l].iter().filter(|e| (x0..x1).contains(&((e.bbox().min.x + e.bbox().max.x) as f64 / 2.0 / SCALE))).cloned().collect()
        };
        let before = draw(ctl, &open, &hs, &bed(), &trees, &[], &mut Memo::default()).unwrap();
        // Taking the layers the drawing built.
        let wall: Vec<Vec<ExPolygon>> = (0..n).map(|l| if (20..=22).contains(&l) { vec![ring(&[(0.0, -5.0), (5.0, -5.0), (5.0, 5.0), (0.0, 5.0)])] } else { Vec::new() }).collect();
        let walled = Volumes::new(ctl, &s, &wall, &[]).unwrap().taking(&open);
        let fresh = draw(ctl, &walled, &hs, &bed(), &trees, &[], &mut Memo::default()).unwrap();
        let part_kept = |l: usize| open.collision(ctl, l, 0).unwrap() == walled.collision(ctl, l, 0).unwrap();
        let grown: Vec<usize> = (0..=50).filter(|&l| part_kept(l) && within(&fresh, l, -15.0, 15.0) != within(&before, l, -15.0, 15.0)).collect();
        assert!(!grown.is_empty(), "the first column grows back over the wall's collision");
        // Every branch kept empty, with what it read: a branch taken draws nothing.
        let mut memo = Memo::default();
        draw(ctl, &open, &hs, &bed(), &trees, &[], &mut memo).unwrap();
        for d in memo.branches.values_mut() {
            *d = Arc::new(Branched { cut: Cut { first: 0, slices: Vec::new(), cut: Vec::new(), span: (0, 0), held: Some(BBox::empty()) }, down: Vec::new(), floor: Vec::new(), read: d.read.clone() });
        }
        let warm = draw(ctl, &walled, &hs, &bed(), &trees, &[], &mut memo).unwrap();
        assert!((0..=10).all(|l| within(&warm, l, 45.0, 75.0).is_empty()) && !within(&fresh, 5, 45.0, 75.0).is_empty(), "the short column is the memo's");
        assert!((0..=50).all(|l| within(&warm, l, -15.0, 45.0) == within(&fresh, l, -15.0, 45.0)), "the tall columns drawn afresh");
        // Every group kept empty: a group taken is nothing.
        let mut memo = Memo::default();
        draw(ctl, &open, &hs, &bed(), &trees, &[], &mut memo).unwrap();
        for b in memo.bases.values_mut() {
            *b = Arc::new(Vec::new());
        }
        let warm = draw(ctl, &walled, &hs, &bed(), &trees, &[], &mut memo).unwrap();
        for l in (0..=50).filter(|&l| !part_kept(l)) {
            assert_eq!(warm.base[l], fresh.base[l], "layer {l}, where the wall changed the part: every group joined afresh");
        }
        for &l in &grown {
            assert_eq!(within(&warm, l, -15.0, 15.0), within(&fresh, l, -15.0, 15.0), "layer {l}: the first column's group joined afresh");
            assert!(within(&warm, l, 15.0, 45.0).is_empty() && !within(&fresh, l, 15.0, 45.0).is_empty(), "layer {l}: the other column's group taken");
        }
    }

    /// §8.4, D7 (copied for parity): a root hanging off a block's top edge drops past it. The
    /// drop layer two over the block's top has placeable area under part of the root's slice,
    /// and the floor made there is filed a layer lower, unclipped, where it lies in that layer's
    /// collision. The top drop layer keeps no floor. Fixing D7 flips this test.
    #[test]
    fn a_drop_files_its_floors_a_layer_low() {
        let s = r14(0.35);
        let (n, top) = (40, 10);
        let block: Vec<Vec<ExPolygon>> = (0..n).map(|l| if l <= top { vec![ring(&[(-10.0, -5.0), (0.0, -5.0), (0.0, 5.0), (-10.0, 5.0)])] } else { Vec::new() }).collect();
        let v = Volumes::new(Ctl::default(), &s, &block, &[]).unwrap();
        let ctl = Ctl::default();
        let trees = column(&s, n, top + 3, top + 23, Pt::from_mm(0.6, 0.0));
        assert!(!trees[top + 3][0].on_top && !trees[top + 3][0].to_bed, "a root bound for the model, not on its top");
        let d = draw(ctl, &v, &heights(n), &bed(), &trees, &[], &mut Memo::default()).unwrap();
        let made = top + 2;
        let place = v.placeable(ctl, made, 0).unwrap();
        assert!(!place.is_empty(), "placeable area on layer {made}, over the block");
        assert!(d.floor[made].is_empty() && d.floor[top + 3].is_empty(), "no floor on the top drop layer, nor the root's");
        let filed = &d.floor[made - 1];
        let want = area(&intersection_ex(&d.base[made], place));
        assert!(want > 0.1 && (area(filed) - want).abs() < 0.01 * want, "the floor made on layer {made} lies on layer {}: {} mm2, want {want}", made - 1, area(filed));
        let into = area(&intersection_ex(filed, v.collision(ctl, made - 1, s.zero_full()).unwrap()));
        assert!((into - area(filed)).abs() < 1e-6, "all of it in that layer's collision: {into} of {} mm2", area(filed));
        assert!((0..made - 1).all(|l| d.floor[l].is_empty()), "the layers under it have no placeable area left");
    }
}

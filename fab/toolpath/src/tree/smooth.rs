//! Smoothing (R14 §7): the placed points pushed off the part and averaged with their
//! neighbours, at most 100 rounds, every free point in parallel from the round before's.
//!
//! - Each element is a sphere at its placed point and its counters' drawn radius
//!   ([`Element::drawn_radius`]): the push goes by it. It tests its own layer and the layers
//!   above it within that radius, no higher than the lowest tip above it. Its anchors' reach goes
//!   by its width (Fab's, [`Element::width`]), so the joints held are the ones drawn (D22).
//! - D24, Fab's: the push is not by the width. The descent routes an element clear of the part
//!   at its counters' radius, and where those lag (a fork or a trunk under wider branches, 0.40
//!   mm under 0.88) a push by the width moved points placement had cleared 0.3-0.7 mm, and the
//!   links took the move: a fork's jumped up to 0.48 mm a layer and its tube hung off the layer
//!   under it; held, the chain over it stretched instead, and its tube rose past the part's edge
//!   at the top. What the width overhangs the part, the draw cuts off.
//! - Tips, and roots on the part (no child, above layer 0), are locked; roots on the bed are free.
//! - Each round a free sphere is first pushed off the part's outline (the collision at radius 0)
//!   where it cuts into it, then moved half way to the average of the elements it carries and the
//!   one under it. Each move is held within reach of those neighbours.
//! - D23, Fab's: then each link with a fork at either end (the element, or the one under it,
//!   carries more than one) is held within its bound, from the bottom up: within the anchor's
//!   reach, or the link's length as placed where that is longer, of the point under it as this
//!   round left it, then of each locked one the fork carries. The reference holds a move only
//!   against its neighbours' points of the round before, so where the push moves a fork's
//!   branches apart (each off its own side of a strut) the links at the fork grow a little every
//!   round, up to 0.48 mm a layer at the lattice cube's forks, whose tubes then hang off the layer
//!   under them. A chain keeps the reference's round: pushed sideways while it leans at the fast
//!   move, its links stretch and it keeps its lean; held to their bound, it would trail its placed
//!   points by up to 1 mm, into the part beside it.
//! - It stops after a round in which nothing cut into the outline, so a tree that touches nothing
//!   is smoothed once. A tree that neither moved nor cut in a round is at rest: the rounds after
//!   leave it out, as they would only bring it back to the same points.
//! - A tree the same as one of the smoothing before, on volumes that have every outline layer its
//!   rounds read as they read it, comes out as that one did through as many rounds ([`Memo`]), so
//!   an edit steps only the trees it changed and those whose outline it changed.
//! - Positions are 32-bit float mm between rounds, and every placed point is written back from
//!   its float.
//!
//! Fab's keep-out, which is not the reference's: a sphere leaving it ([`Element::leaving`]) is
//! pushed off the part's own outline; every other is pushed off the keep-out too.

use std::cell::Cell;
use std::collections::HashMap;

use makepad_fab_slicer::geom::*;

use super::draw::{id_of, Id};
use super::element::Element;
use super::settings::Settings;
use super::volumes::{reading, Reads, Volumes};
use crate::par::{Cancelled, Ctl};

/// The most rounds.
const ROUNDS: usize = 100;
/// What a push adds to the overlap (mm).
const PUSH_SLACK: f64 = 0.1;
/// The longest push (mm), before it is squared.
const PUSH_MAX: f64 = 0.5;
/// The longest smoothing move (mm).
const SMOOTH_MAX: f64 = 0.2;
/// A cut deeper than this (mm) keeps the rounds going; an anchor nearer than this does not pull.
const EPSILON: f64 = 0.0001;

/// A point in mm, holding 32-bit float values.
type P = (f64, f64);

/// `p` rounded to 32-bit floats.
fn float(p: P) -> P {
    (p.0 as f32 as f64, p.1 as f32 as f64)
}

/// A placed point as float mm: the whole nm as a float, times the float 0.000 001.
fn to_float_mm(p: Pt) -> P {
    ((p.x as f32 * 1e-6_f32) as f64, (p.y as f32 * 1e-6_f32) as f64)
}

/// Float mm back to whole nm: divided by the float 0.000 001 in floats, truncated.
fn to_nm(p: P) -> Pt {
    Pt::new((p.0 as f32 / 1e-6_f32).trunc() as i64, (p.1 as f32 / 1e-6_f32).trunc() as i64)
}

fn dist(a: P, b: P) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// One element's sphere.
struct Sphere {
    l: usize,
    i: usize,
    /// Its element's counters' drawn radius ([`Element::drawn_radius`]), a 32-bit float: what
    /// the push goes by.
    r: f64,
    /// Its element's width ([`Element::width`]), a 32-bit float: what its anchors' reach goes by.
    w: f64,
    locked: bool,
    /// The highest layer it tests.
    top: usize,
    /// The element under it that carries it.
    child: Option<usize>,
}

/// One sphere per element of `layers`, in layer order, at the layers' print z `z` (mm).
fn spheres(s: &Settings, layers: &[Vec<Element>], z: &[f64]) -> Vec<Sphere> {
    let n = layers.len();
    let mut child: Vec<Vec<Option<usize>>> = layers.iter().map(|l| vec![None; l.len()]).collect();
    for l in 0..n.saturating_sub(1) {
        for (i, e) in layers[l].iter().enumerate() {
            for &c in &e.carries {
                child[l + 1][c] = Some(i);
            }
        }
    }
    // The float z of the lowest tip above, top-down: a tip's own, else the least of what it
    // carries.
    let mut tip_z: Vec<Vec<f32>> = layers.iter().map(|l| vec![0.0; l.len()]).collect();
    for l in (0..n).rev() {
        for (i, e) in layers[l].iter().enumerate() {
            tip_z[l][i] = if e.carries.is_empty() { z[l] as f32 } else { e.carries.iter().map(|&c| tip_z[l + 1][c]).fold(f32::INFINITY, f32::min) };
        }
    }
    let highest = (0..n).rev().find(|&l| !layers[l].is_empty()).unwrap_or(0);
    let h = s.keys.layer_height;
    let mut spheres = Vec::new();
    for l in 0..n {
        for (i, e) in layers[l].iter().enumerate() {
            let (r, w) = (e.drawn_radius(s) as f32, e.width(s) as f32);
            // The span's bottom, from the chain's root, never counts: the layer difference is
            // taken unsigned, so every layer below wraps far out of reach (Q83, R14 §7 set-up).
            let top = tip_z[l][i].min(z[l] as f32 + r);
            let k = ((top as f64 - z[0]) / h).floor().max(0.0) as usize;
            let locked = e.carries.is_empty() || (child[l][i].is_none() && l > 0);
            spheres.push(Sphere { l, i, r: r as f64, w: w as f64, locked, top: k.max(l).min(highest), child: child[l][i] });
        }
    }
    spheres
}

/// Each layer's first sphere in [`spheres`]' order.
fn firsts(layers: &[Vec<Element>]) -> Vec<usize> {
    layers.iter().scan(0, |k, l| Some(std::mem::replace(k, *k + l.len()))).collect()
}

/// D23: each sphere's link to its child, as far as [`hold`] lets it stretch: where it or its child
/// carries more than one, the anchor's reach, or its length as placed where that is longer;
/// along a chain, without end. 0 for a sphere without a child.
fn links(s: &Settings, layers: &[Vec<Element>], spheres: &[Sphere], first: &[usize]) -> Vec<f64> {
    let at = |sp: &Sphere| to_float_mm(layers[sp.l][sp.i].placed.expect("§6 places every element it keeps"));
    let fork = |sp: &Sphere| layers[sp.l][sp.i].carries.len() > 1;
    spheres
        .iter()
        .map(|sp| {
            sp.child.map_or(0.0, |c| {
                let under = &spheres[first[sp.l - 1] + c];
                if fork(sp) || fork(under) {
                    reach(s, under.w, sp.w).max(dist(at(sp), at(under)))
                } else {
                    f64::INFINITY
                }
            })
        })
        .collect()
}

/// Smooths the placed points of `layers` (after placement: every element placed, carries
/// indexing the layer above), at the layers' print z `z` (mm); with what `memo` kept of the
/// smoothing before on the same settings, and keeping this one's.
pub fn smooth(ctl: Ctl, s: &Settings, v: &Volumes, layers: &mut [Vec<Element>], z: &[f64], memo: &mut Memo) -> Result<(), Cancelled> {
    crate::prof::scope_fanned(crate::prof::Stage::TreeSmooth, || {
        let shared: &[Vec<Element>] = layers;
        let spheres = spheres(s, shared, z);
        let zero = s.rung_for(0.0, false);
        let none: &[ExPolygon] = &[];
        let outlines: Vec<&[ExPolygon]> = (0..shared.len()).map(|k| if shared[k].is_empty() { Ok(none) } else { v.collision(ctl, k, zero) }).collect::<Result<_, _>>()?;
        // How far it is: a share per layer's edges and one per round (the rounds a smoothing
        // stops short of are told by the next stage's start).
        let of = outlines.len() + ROUNDS;
        let lines = crate::par::try_map(ctl, outlines.len(), |k| {
            let edges = Edges::new(outlines[k]);
            super::tell(ctl, super::Stage::Smooth, k, of, super::Grown::Nothing);
            edges
        })?;
        // Fab's keep-out: the outline of the part alone, for the spheres leaving it.
        let free_lines = if v.keeps_out() && shared.iter().flatten().any(Element::leaving) {
            let free = v.free();
            let outlines: Vec<&[ExPolygon]> = (0..shared.len()).map(|k| if shared[k].is_empty() { Ok(none) } else { free.collision(ctl, k, zero) }).collect::<Result<_, _>>()?;
            crate::par::try_map(ctl, outlines.len(), |k| Edges::new(outlines[k]))?
        } else {
            Vec::new()
        };
        // The trees, each its spheres in order, and each sphere's tree and place in it: the
        // spheres go layer by layer, each element's at its layer's first plus its index, and a
        // root comes before what it carries.
        let first = firsts(shared);
        let (mut tree, mut slot, mut members) = (Vec::with_capacity(spheres.len()), Vec::with_capacity(spheres.len()), Vec::<Vec<usize>>::new());
        for (k, sp) in spheres.iter().enumerate() {
            let t = sp.child.map_or(members.len(), |c| tree[first[sp.l - 1] + c]);
            if t == members.len() {
                members.push(Vec::new());
            }
            tree.push(t);
            slot.push(members[t].len());
            members[t].push(k);
        }
        // Each tree's free spheres, and its points in its order: a sphere moves by its own tree's
        // points and the outline alone, so the trees go through each round apart.
        let frees: Vec<Vec<usize>> = members.iter().map(|m| m.iter().copied().filter(|&k| !spheres[k].locked).collect()).collect();
        let placed = |k: usize| to_float_mm(shared[spheres[k].l][spheres[k].i].placed.expect("§6 places every element it keeps"));
        let mut pos: Vec<Vec<P>> = members.iter().map(|m| m.iter().map(|&k| placed(k)).collect()).collect();
        // Each tree's id: everything its rounds read but the outline, whose layers they log.
        let ids: Vec<Id> = crate::par::try_map(ctl, members.len(), |t| {
            id_of(&members[t].iter().map(|&k| {
                let (sp, e) = (&spheres[k], &shared[spheres[k].l][spheres[k].i]);
                let carries: Vec<usize> = e.carries.iter().map(|&c| slot[first[sp.l + 1] + c]).collect();
                (sp.l, sp.r.to_bits(), sp.w.to_bits(), sp.locked, sp.top, sp.child.map(|c| slot[first[sp.l - 1] + c]), e.placed, e.leaving(), carries)
            }).collect::<Vec<_>>())
        })?;
        let mut out: Vec<Smoothed> = members.iter().map(|_| Smoothed::NONE).collect();
        // The outline layers each tree's rounds before these read; these note theirs in `out`.
        let mut read: Vec<Reads> = vec![Reads::default(); members.len()];
        // The trees smoothed before, on volumes with every outline layer their rounds read as they
        // read it, start from where they ended: the rounds go on for those that were not at rest
        // when theirs stopped, from the round they stopped at.
        let (mut join, mut known) = (Vec::new(), 0u128);
        for (t, id) in ids.iter().enumerate() {
            let was = match memo.trees.get(id) {
                Some((was, reads)) if v.holds(ctl, reads)? => {
                    read[t] = reads.clone();
                    was
                }
                _ => {
                    join.push((0, t));
                    continue;
                }
            };
            for (at, &p) in pos[t].iter_mut().zip(&was.pos) {
                *at = p;
            }
            (out[t].cuts, out[t].ran, out[t].rested) = (was.cuts, was.ran, was.rested);
            known |= was.cuts;
            if !was.rested {
                join.push((was.ran, t));
            }
        }
        // The rounds each tree went through before these.
        let from: Vec<usize> = out.iter().map(|o| o.ran).collect();
        let links = links(s, shared, &spheres, &first);
        let rounds = Rounds { s, shared, lines: &lines, free_lines: &free_lines, spheres: &spheres, first: &first, links: &links, slot: &slot, frees: &frees, of };
        let ran = rounds.run(ctl, &mut pos, &mut out, &join, known, ROUNDS)?;
        // A tree smoothed before through more rounds than these is smoothed afresh through these.
        let over: Vec<(usize, usize)> = (0..members.len()).filter(|&t| out[t].ran > ran).map(|t| (0, t)).collect();
        for &(_, t) in &over {
            pos[t] = members[t].iter().map(|&k| placed(k)).collect();
            out[t] = Smoothed::NONE;
            read[t] = Reads::default();
        }
        Rounds { of: 0, ..rounds }.run(ctl, &mut pos, &mut out, &over, u128::MAX, ran)?;
        for (m, p) in members.iter().zip(&pos) {
            for (&k, &p) in m.iter().zip(p) {
                layers[spheres[k].l][spheres[k].i].placed = Some(to_nm(p));
            }
        }
        memo.made = (0..out.len()).filter(|&t| out[t].ran != from[t]).count();
        let mut trees = HashMap::with_capacity(out.len());
        for (t, (id, mut o)) in ids.into_iter().zip(out).enumerate() {
            o.pos = std::mem::take(&mut pos[t]);
            // The outline layers its steps asked for, asked of the volumes as the steps read them.
            let asked = std::mem::take(&mut o.asked);
            let (all, log) = reading(|| -> Result<(), Cancelled> {
                for (part, range) in [v, v.free()].into_iter().zip(asked) {
                    if let Some((lo, hi)) = range {
                        for j in lo..=hi {
                            part.collision(ctl, j, zero)?;
                        }
                    }
                }
                Ok(())
            });
            all?;
            let logged = v.now(ctl, log)?;
            trees.insert(id, (o, Reads::all([&read[t], &logged])));
        }
        memo.trees = trees;
        Ok(())
    })
}

/// What a smoothing keeps for the next on the same settings: by each tree's id, how it came out
/// and the outline layers its rounds read ([`reading`]), each held as the volumes had it. A sphere
/// moves by its own tree's points and the outline alone, so a tree the same as one before, on
/// volumes that have every layer it read as it read it ([`Volumes::holds`]), comes to the same
/// through as many rounds.
#[derive(Default)]
pub struct Memo {
    trees: HashMap<Id, (Smoothed, Reads)>,
    /// The trees the last smoothing took through a round or more ([`super::Work`]).
    pub(super) made: usize,
}

/// How a tree came out of a smoothing.
struct Smoothed {
    /// Where its spheres ended, in its order.
    pos: Vec<P>,
    /// Bit `j`: it cut in round `j`.
    cuts: u128,
    /// The rounds it was stepped through.
    ran: usize,
    /// Whether it was at rest in the last of them: no round after moves it.
    rested: bool,
    /// The lowest and the highest outline layer its steps in this smoothing asked for, of the
    /// part with the keep-out and of the part alone ([`Volumes::free`]), until the smoothing keeps
    /// what they read with it.
    asked: [Option<(usize, usize)>; 2],
}

impl Smoothed {
    /// A tree not stepped yet.
    const NONE: Smoothed = Smoothed { pos: Vec::new(), cuts: 0, ran: 0, rested: true, asked: [None; 2] };
}

const _: () = assert!(ROUNDS <= u128::BITS as usize, "a tree's cuts are a bit per round");

/// What the rounds read.
struct Rounds<'a> {
    s: &'a Settings,
    shared: &'a [Vec<Element>],
    lines: &'a [Edges],
    free_lines: &'a [Edges],
    spheres: &'a [Sphere],
    /// Each layer's first sphere.
    first: &'a [usize],
    /// Each sphere's link to its child ([`hold`]).
    links: &'a [f64],
    /// Each sphere's place in its tree.
    slot: &'a [usize],
    /// Each tree's free spheres, in its order.
    frees: &'a [Vec<usize>],
    /// Telling round `j` as `j` out of this, if it is not 0.
    of: usize,
}

/// One tree's points, in its order, by each sphere's layer and index there.
struct TreePoints<'a> {
    pos: Vec<P>,
    first: &'a [usize],
    slot: &'a [usize],
}

/// Points by layer and index: every layer's, or one tree's ([`TreePoints`]).
trait Points {
    fn at(&self, l: usize, i: usize) -> P;
    fn set(&mut self, l: usize, i: usize, p: P);
}

impl Points for Vec<Vec<P>> {
    fn at(&self, l: usize, i: usize) -> P {
        self[l][i]
    }

    fn set(&mut self, l: usize, i: usize, p: P) {
        self[l][i] = p;
    }
}

impl Points for TreePoints<'_> {
    fn at(&self, l: usize, i: usize) -> P {
        self.pos[self.slot[self.first[l] + i]]
    }

    fn set(&mut self, l: usize, i: usize, p: P) {
        self.pos[self.slot[self.first[l] + i]] = p;
    }
}

/// One tree's round: its points after, whether a sphere of it cut, whether it moved or cut,
/// and the lowest and the highest outline layer its steps asked for, of the part with the
/// keep-out and of the part alone.
struct Round {
    pos: Vec<P>,
    deep: bool,
    stirred: bool,
    asked: [Option<(usize, usize)>; 2],
}

impl Rounds<'_> {
    /// Steps the free spheres of each `(j, t)` of `join`, tree `t`, from round `j` on, from their
    /// points in `pos` (each tree's in its order): until a round in which none of them and none
    /// of `known`'s bits cut, or `most` rounds. Each tree's rounds, cuts, rest and the outline
    /// layers its steps ask for go in `out`. Returns the rounds run.
    fn run(&self, ctl: Ctl, pos: &mut [Vec<P>], out: &mut [Smoothed], join: &[(usize, usize)], known: u128, most: usize) -> Result<usize, Cancelled> {
        // The trees not at rest that have free spheres.
        let mut going: Vec<usize> = Vec::new();
        for round in 0..most {
            going.extend(join.iter().filter(|j| j.0 == round && !self.frees[j.1].is_empty()).map(|j| j.1));
            let trees: Vec<(usize, Vec<P>)> = going.iter().map(|&t| (t, std::mem::take(&mut pos[t]))).collect();
            let rounds = crate::par::try_map_vec(ctl, trees, |_, (t, prev)| self.round(t, prev))?;
            let mut cut = (known >> round) & 1 == 1;
            for (&t, r) in going.iter().zip(rounds) {
                pos[t] = r.pos;
                let o = &mut out[t];
                for (at, asked) in o.asked.iter_mut().zip(r.asked) {
                    if let Some((lo, hi)) = asked {
                        *at = Some(at.map_or((lo, hi), |(l, h)| (l.min(lo), h.max(hi))));
                    }
                }
                o.cuts |= u128::from(r.deep) << round;
                o.ran = round + 1;
                o.rested = !r.stirred;
                cut |= r.deep;
            }
            if self.of > 0 {
                super::tell(ctl, super::Stage::Smooth, round, self.of, super::Grown::Nothing);
            }
            // Q90 (R14 §7 stopping): only cuts keep it going, so a tree that touches nothing is
            // smoothed once.
            if !cut {
                return Ok(round + 1);
            }
            // A tree that neither moved nor cut this round comes to the same every round after:
            // it is left at rest, and the rounds go on for the rest.
            going.retain(|&t| !out[t].rested);
        }
        Ok(most)
    }

    /// Tree `t`'s round from its points `prev`: each free sphere stepped from those, then held
    /// ([`hold`]).
    fn round(&self, t: usize, prev: Vec<P>) -> Round {
        let (spheres, frees) = (self.spheres, &self.frees[t]);
        let prev = TreePoints { pos: prev, first: self.first, slot: self.slot };
        let mut pos = TreePoints { pos: prev.pos.clone(), first: self.first, slot: self.slot };
        let (mut deep, mut asked) = (false, [None; 2]);
        for &k in frees {
            let sp = &spheres[k];
            // The part's own outline for a sphere leaving the keep-out; and the lowest and the
            // highest layer of it the step asks for.
            let free = self.shared[sp.l][sp.i].leaving();
            let lines = if free { self.free_lines } else { self.lines };
            let range = Cell::new((usize::MAX, 0));
            let outline = |j: usize| {
                let (lo, hi) = range.get();
                range.set((lo.min(j), hi.max(j)));
                &lines[j]
            };
            let (p, cut) = step(self.s, self.shared, outline, &prev, sp);
            let (lo, hi) = range.get();
            if lo <= hi {
                let at: &mut Option<(usize, usize)> = &mut asked[usize::from(free)];
                *at = Some(at.map_or((lo, hi), |(l, h)| (l.min(lo), h.max(hi))));
            }
            pos.set(sp.l, sp.i, p);
            deep |= cut;
        }
        hold(self.shared, spheres, self.first, self.links, &mut pos, frees);
        let stirred = deep || frees.iter().any(|&k| pos.at(spheres[k].l, spheres[k].i) != prev.at(spheres[k].l, spheres[k].i));
        Round { pos: pos.pos, deep, stirred, asked }
    }
}

/// How deep sphere `sp` at `here` cuts into the outline at its deepest over its layers, each
/// layer's edges as `outline` hands them, and the outline point there; on a tie the lowest
/// layer's.
fn deepest<'e>(s: &Settings, outline: &impl Fn(usize) -> &'e Edges, here: P, sp: &Sphere) -> Option<(f64, P)> {
    let mut best: Option<(f64, P)> = None;
    for k in sp.l..=sp.top {
        let dz = (k - sp.l) as f64 * s.keys.layer_height;
        if dz >= sp.r {
            continue;
        }
        let rc = (sp.r * sp.r - dz * dz).sqrt();
        let Some((hit, d)) = outline(k).nearest(here, rc) else { continue };
        // Unsigned: a point inside the outline is pushed off its nearest edge, deeper in.
        if d < rc && best.is_none_or(|b| rc - d > b.0) {
            best = Some((rc - d, float(hit)));
        }
    }
    best
}

/// The grid cell (mm) a layer's outline edges are filed in.
const CELL: f64 = 1.0;
/// How much (mm) a search looks past its reach, so no edge a rounding would bring within it is
/// missed.
const REACH_SLACK: f64 = 1e-6;

/// A layer's outline edges (mm), in ring order, filed by the grid cells their boxes touch, so
/// a search near a point looks only at the edges that come near it.
struct Edges {
    edges: Vec<(P, P)>,
    origin: P,
    cols: usize,
    rows: usize,
    /// Per cell, row by row, the edges whose boxes touch it.
    cells: Vec<Vec<u32>>,
}

impl Edges {
    fn new(set: &[ExPolygon]) -> Edges {
        let mut edges = Vec::new();
        for r in set.iter().flat_map(|e| e.rings()) {
            let n = r.pts.len();
            for k in 0..n {
                let (a, b) = (r.pts[k], r.pts[(k + 1) % n]);
                edges.push(((a.x as f64 / SCALE, a.y as f64 / SCALE), (b.x as f64 / SCALE, b.y as f64 / SCALE)));
            }
        }
        let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
        for &(a, b) in &edges {
            lo = (lo.0.min(a.0).min(b.0), lo.1.min(a.1).min(b.1));
            hi = (hi.0.max(a.0).max(b.0), hi.1.max(a.1).max(b.1));
        }
        if edges.is_empty() {
            return Edges { edges, origin: (0.0, 0.0), cols: 0, rows: 0, cells: Vec::new() };
        }
        let cols = ((hi.0 - lo.0) / CELL).floor() as usize + 1;
        let rows = ((hi.1 - lo.1) / CELL).floor() as usize + 1;
        let mut cells = vec![Vec::new(); cols * rows];
        let at = |v: f64, from: f64, most: usize| (((v - from) / CELL).floor().max(0.0) as usize).min(most - 1);
        for (i, &(a, b)) in edges.iter().enumerate() {
            let (c0, c1) = (at(a.0.min(b.0), lo.0, cols), at(a.0.max(b.0), lo.0, cols));
            let (r0, r1) = (at(a.1.min(b.1), lo.1, rows), at(a.1.max(b.1), lo.1, rows));
            for r in r0..=r1 {
                for c in c0..=c1 {
                    cells[r * cols + c].push(i as u32);
                }
            }
        }
        Edges { edges, origin: lo, cols, rows, cells }
    }

    /// The nearest point to `p` (mm) on the edges that come within `reach` (mm) of it, and its
    /// distance; on a tie the first edge in ring order. Wherever an edge is nearer than `reach`,
    /// this is the nearest point on any edge, as a search of every edge finds it.
    fn nearest(&self, p: P, reach: f64) -> Option<(P, f64)> {
        if self.cells.is_empty() {
            return None;
        }
        let r = reach + REACH_SLACK;
        let span = |v: f64, from: f64, most: usize| -> Option<(usize, usize)> {
            let (a, b) = (((v - r - from) / CELL).floor(), ((v + r - from) / CELL).floor());
            (b >= 0.0 && a < most as f64).then(|| (a.max(0.0) as usize, (b as usize).min(most - 1)))
        };
        let ((c0, c1), (r0, r1)) = (span(p.0, self.origin.0, self.cols)?, span(p.1, self.origin.1, self.rows)?);
        let mut best: Option<(P, f64, u32)> = None;
        for row in r0..=r1 {
            for &i in self.cells[row * self.cols + c0..=row * self.cols + c1].iter().flatten() {
                let (a, b) = self.edges[i as usize];
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let l2 = dx * dx + dy * dy;
                let t = if l2 == 0.0 { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0) };
                let q = (a.0 + dx * t, a.1 + dy * t);
                let d = dist(p, q);
                if best.is_none_or(|b| d < b.1 || (d == b.1 && i < b.2)) {
                    best = Some((q, d, i));
                }
            }
        }
        best.map(|(q, d, _)| (q, d))
    }
}

/// One round's new position for sphere `sp`, from the round before's positions `prev`, and
/// whether it cut deeper than [`EPSILON`] into the outline, each layer's edges as `outline` hands
/// them.
fn step<'e>(s: &Settings, layers: &[Vec<Element>], outline: impl Fn(usize) -> &'e Edges, prev: &impl Points, sp: &Sphere) -> (P, bool) {
    let e = &layers[sp.l][sp.i];
    let mut here = prev.at(sp.l, sp.i);
    // 1. The push, straight off the outline point it cuts deepest at, by n squared.
    let cut = deepest(s, &outline, here, sp);
    if let Some((d, hit)) = cut {
        let n = (d + PUSH_SLACK).min(PUSH_MAX);
        let (dx, dy) = ((here.0 as f32 - hit.0 as f32) as f64, (here.1 as f32 - hit.1 as f32) as f64);
        let l = dx.hypot(dy);
        if l > 0.0 {
            let to = (here.0 + dx / l * n * n, here.1 + dy / l * n * n);
            here = float(anchored(s, layers, prev, sp, to, here));
        }
    }
    // 2. Half way to the average of the carried ones and the child, the child weighing as much
    // as all of those, by at most 0.2 mm.
    let count = e.carries.len() as f64;
    let mut sum = e.carries.iter().fold((0.0, 0.0), |a, &c| {
        let q = prev.at(sp.l + 1, c);
        (a.0 + q.0, a.1 + q.1)
    });
    let mut w = count;
    if let Some(c) = sp.child {
        let q = prev.at(sp.l - 1, c);
        sum = (sum.0 + q.0 * count, sum.1 + q.1 * count);
        w *= 2.0;
    }
    let avg = (sum.0 / w, sum.1 / w);
    let (dx, dy) = ((avg.0 - here.0) / 2.0, (avg.1 - here.1) / 2.0);
    let l = dx.hypot(dy);
    if l > 0.0 {
        let k = if l > SMOOTH_MAX { SMOOTH_MAX / l } else { 1.0 };
        here = float(anchored(s, layers, prev, sp, (here.0 + dx * k, here.1 + dy * k), here));
    }
    (here, cut.is_some_and(|c| c.0 > EPSILON))
}

/// Candidate `p` held within reach of the child, then of each carried one in turn, around their
/// previous positions; a sphere already farther than that from one (at `here`) keeps its
/// distance as the limit.
fn anchored(s: &Settings, layers: &[Vec<Element>], prev: &impl Points, sp: &Sphere, mut p: P, here: P) -> P {
    let mut limit = |anchor: P, most: f64| p = within(p, anchor, most.max(dist(here, anchor)));
    let r = |e: &Element| e.width(s) as f32 as f64;
    if let Some(c) = sp.child {
        limit(prev.at(sp.l - 1, c), reach(s, r(&layers[sp.l - 1][c]), sp.w));
    }
    for &c in &layers[sp.l][sp.i].carries {
        limit(prev.at(sp.l + 1, c), reach(s, sp.w, r(&layers[sp.l + 1][c])));
    }
    p
}

/// An anchor's reach (mm) between a sphere of radius `lower` and one of radius `upper` it
/// carries: the lower's radius less the upper's, if more, plus the slow move.
fn reach(s: &Settings, lower: f64, upper: f64) -> f64 {
    (lower - upper).max(0.0) + s.slow
}

/// `p` held within `reach` (mm) of `anchor`: taken straight towards it where it is farther.
fn within(p: P, anchor: P, reach: f64) -> P {
    let d = dist(p, anchor);
    if d > reach && d > EPSILON {
        (anchor.0 + (p.0 - anchor.0) * reach / d, anchor.1 + (p.1 - anchor.1) * reach / d)
    } else {
        p
    }
}

/// D23: each sphere `ks` names (each tree's in order, a child before what it carries) held
/// within its link ([`links`]) of its child's point in `pos`, which the child has already been
/// held to, then within each locked one's link of that one, which never moves. A link along a
/// chain holds nothing.
fn hold(layers: &[Vec<Element>], spheres: &[Sphere], first: &[usize], links: &[f64], pos: &mut impl Points, ks: &[usize]) {
    for &k in ks {
        let sp = &spheres[k];
        let mut p = pos.at(sp.l, sp.i);
        if let Some(c) = sp.child {
            p = within(p, pos.at(sp.l - 1, c), links[k]);
        }
        for &c in &layers[sp.l][sp.i].carries {
            let up = first[sp.l + 1] + c;
            if spheres[up].locked {
                p = within(p, pos.at(sp.l + 1, c), links[up]);
            }
        }
        pos.set(sp.l, sp.i, float(p));
    }
}

#[cfg(test)]
mod tests {
    use super::super::settings::tests::r14;
    use super::super::settings::Keys;
    use super::*;

    fn ctl() -> Ctl<'static> {
        Ctl::default()
    }

    /// R14's print z: 0.25 on layer 0, then 0.2 mm a layer.
    fn print_z(n: usize) -> Vec<f64> {
        (0..n).map(|k| 0.25 + 0.2 * k as f64).collect()
    }

    /// An element on layer `l` placed at (`x`, 0) mm, at `depth` (so its drawn radius is R14's
    /// 0.4 + 0.12 per layer down the ramp), carrying `carries`.
    fn at(s: &Settings, l: usize, x: f64, depth: usize, carries: Vec<usize>) -> Element {
        let mut e = Element::tip(s, l, Pt::from_mm(x, 0.0));
        (e.depth, e.ramp, e.carries) = (depth, depth, carries);
        e
    }

    fn near(what: &str, got: P, want: (f64, f64)) {
        assert!((got.0 - want.0).abs() < 1e-6 && (got.1 - want.1).abs() < 1e-6, "{what}: {got:?} mm, R14 says {want:?}");
    }

    /// `n` layers with no outline.
    fn no_lines(n: usize) -> Vec<Edges> {
        (0..n).map(|_| Edges::new(&[])).collect()
    }

    /// The nearest point on any edge of `set`, every edge searched in ring order, the first on a
    /// tie: what [`Edges::nearest`] must give wherever an edge is within its reach.
    fn every_edge(set: &[ExPolygon], p: P) -> Option<(P, f64)> {
        let mut best: Option<(P, f64)> = None;
        for r in set.iter().flat_map(|e| e.rings()) {
            let n = r.pts.len();
            for k in 0..n {
                let (a, b) = (r.pts[k], r.pts[(k + 1) % n]);
                let (a, b) = ((a.x as f64 / SCALE, a.y as f64 / SCALE), (b.x as f64 / SCALE, b.y as f64 / SCALE));
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let l2 = dx * dx + dy * dy;
                let t = if l2 == 0.0 { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0) };
                let q = (a.0 + dx * t, a.1 + dy * t);
                let d = dist(p, q);
                if best.is_none_or(|b| d < b.1) {
                    best = Some((q, d));
                }
            }
        }
        best
    }

    /// The edge grid finds what a search of every edge finds, point for point and on ties,
    /// wherever an edge is within the reach; past it, nothing nearer than the reach.
    #[test]
    fn the_edge_grid_finds_the_nearest_edge_as_a_full_search_does() {
        let ring = |pts: &[(f64, f64)]| Polygon::new(pts.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect());
        // An L with a square hole, a sliver, and a square whose corners sit on the grid's lines.
        let mut l = ExPolygon::new(ring(&[(0.0, 0.0), (6.0, 0.0), (6.0, 2.0), (2.0, 2.0), (2.0, 6.0), (0.0, 6.0)]));
        l.holes.push(ring(&[(0.5, 0.5), (0.5, 1.5), (1.5, 1.5), (1.5, 0.5)]));
        let sliver = ExPolygon::new(ring(&[(8.0, 1.0), (12.5, 1.001), (8.0, 1.003)]));
        let square = ExPolygon::new(ring(&[(3.0, 3.0), (5.0, 3.0), (5.0, 5.0), (3.0, 5.0)]));
        let set = vec![l, sliver, square];
        let grid = Edges::new(&set);
        let mut checked = 0;
        for i in -20..=150 {
            for j in -20..=90 {
                let p = (i as f64 * 0.1 + 0.013, j as f64 * 0.1 - 0.007);
                // Points the same distance from two edges: the middle of the hole, the square's
                // diagonal.
                let tie = (1.0, 1.0);
                for q in [p, tie, (4.0, 4.0), (4.0, 3.5)] {
                    let want = every_edge(&set, q).expect("edges");
                    for reach in [0.3, 1.0, 2.6, 3.75] {
                        let got = grid.nearest(q, reach);
                        if want.1 < reach {
                            assert_eq!(got, Some(want), "at {q:?} within {reach}");
                            checked += 1;
                        } else {
                            assert!(got.is_none_or(|g| g.1 >= reach), "at {q:?}: nothing nearer than {reach}");
                        }
                    }
                }
            }
        }
        assert!(checked > 10_000, "{checked} points within reach");
    }

    /// Every element's placed point, in float mm.
    fn placed(layers: &[Vec<Element>]) -> Vec<Vec<P>> {
        layers.iter().map(|l| l.iter().map(|e| to_float_mm(e.placed.expect("placed"))).collect()).collect()
    }

    /// §7 set-up: tips and roots on the part are locked, a root on the bed is free. A sphere tests
    /// up to the float z its radius reaches, no higher than its lowest tip's float z, floored to
    /// a layer from 0.25 in 0.2 steps; a tip on #1 or #9 (float z 0.4499999, 2.0499999) floors a
    /// layer low.
    #[test]
    fn spheres_lock_tips_and_raised_roots_and_cap_their_spans() {
        let s = r14(0.8);
        let mut layers: Vec<Vec<Element>> = vec![Vec::new(); 10];
        // A bed root on #0 carrying a tip on #1.
        layers[0].push(at(&s, 0, 9.0, 1, vec![0]));
        layers[1].push(at(&s, 1, 9.0, 0, Vec::new()));
        // A root on the part on #2 up to a tip on #9, with a side tip on #5 that #4 carries too.
        layers[2].push(at(&s, 2, 0.0, 6, vec![0]));
        layers[3].push(at(&s, 3, 0.0, 4, vec![0]));
        layers[4].push(at(&s, 4, 0.0, 3, vec![0, 1]));
        layers[5].push(at(&s, 5, 0.0, 1, vec![0]));
        layers[5].push(at(&s, 5, 1.0, 0, Vec::new()));
        layers[6].push(at(&s, 6, 0.0, 3, vec![0]));
        layers[7].push(at(&s, 7, 0.0, 2, vec![0]));
        layers[8].push(at(&s, 8, 0.0, 1, vec![0]));
        layers[9].push(at(&s, 9, 0.0, 0, Vec::new()));
        let sp = spheres(&s, &layers, &print_z(10));
        let of = |l: usize, i: usize| sp.iter().find(|p| p.l == l && p.i == i).expect("a sphere per element");
        assert!(of(1, 0).locked && of(5, 1).locked && of(9, 0).locked, "tips are locked");
        assert!(of(2, 0).locked, "a root on the part is locked");
        assert!(!of(0, 0).locked && (3..=8).all(|l| !of(l, 0).locked), "the rest are free, the bed root too");
        assert_eq!(of(3, 0).top, 5, "r 0.88 reaches z 1.73 (#7), the side tip on #5 caps it");
        assert_eq!(of(4, 0).top, 5, "r 0.76 reaches z 1.81 (#7), the side tip on #5 caps it");
        assert_eq!(of(5, 0).top, 7, "r 0.52 reaches z 1.77: #7, under the tip");
        assert_eq!(of(6, 0).top, 8, "r 0.76 reaches z 2.21, the tip's float z 2.0499999 floors to #8");
        assert_eq!(of(0, 0).top, 0, "the bed root's tip's float z 0.4499999 floors to #0");
        assert_eq!((of(3, 0).child, of(5, 0).child, of(5, 1).child, of(2, 0).child), (Some(0), Some(0), Some(0), None), "children");
    }

    /// §7 step 2 on R14's numbers: half way to the average of the carried and the child (the
    /// child weighing as much as all the carried), by at most 0.2 mm.
    #[test]
    fn smoothing_goes_half_way_by_at_most_two_tenths() {
        let s = r14(0.8);
        let lines = no_lines(3);
        for (from, want) in [(1.0, 0.8), (0.3, 0.2)] {
            let layers = vec![vec![at(&s, 0, 0.0, 2, vec![0])], vec![at(&s, 1, from, 1, vec![0])], vec![at(&s, 2, 0.2, 0, Vec::new())]];
            let sp = Sphere { l: 1, i: 0, r: layers[1][0].drawn_radius(&s) as f32 as f64, w: layers[1][0].width(&s) as f32 as f64, locked: false, top: 2, child: Some(0) };
            // The average is (0.2 + 0) / 2 = 0.1.
            let (p, cut) = step(&s, &layers, |k| &lines[k], &placed(&layers), &sp);
            near(&format!("from {from}"), p, (want, 0.0));
            assert!(!cut, "no cut: the rounds stop");
        }
    }

    /// §7 step 3: no farther from the child than (its radius less this one's, if more) plus the
    /// slow move, nor from each carried one than (this radius less its, if more) plus the slow
    /// move; a point already farther keeps its distance as the limit.
    #[test]
    fn anchors_hold_a_point_within_the_slow_move() {
        let s = r14(0.8);
        let lines = no_lines(3);
        // Child and point at depth 1 (equal radii): the child's reach is the slow move, 0.093261.
        for (from, up, want) in [(0.0, 1.0, 0.093_261), (0.5, 2.0, 0.5)] {
            let layers = vec![vec![at(&s, 0, 0.0, 1, vec![0])], vec![at(&s, 1, from, 1, vec![0])], vec![at(&s, 2, up, 0, Vec::new())]];
            let sp = Sphere { l: 1, i: 0, r: layers[1][0].drawn_radius(&s) as f32 as f64, w: layers[1][0].width(&s) as f32 as f64, locked: false, top: 2, child: Some(0) };
            near(&format!("from {from} toward {up}"), step(&s, &layers, |k| &lines[k], &placed(&layers), &sp).0, (want, 0.0));
        }
    }

    /// D23: from the bottom up, a link with a fork at either end is held within its bound of the
    /// point under it as this round left it, then of the locked ones the fork carries, so a
    /// fork's links to its tips always end within their bound and the link under it gives where
    /// both cannot; a link along a chain is not held. Radii step 0.12 mm a layer, so a held link
    /// is 0.12 plus the slow move, 0.213261; one placed longer keeps its length.
    #[test]
    fn links_at_forks_are_held_from_the_bottom_up() {
        let s = r14(0.8);
        // A bed root on #0, a free sphere on #1, a fork on #2 and its two tips on #3, x = -/+0.1.
        let tree = |fork: f64| -> Vec<Vec<Element>> {
            vec![
                vec![at(&s, 0, 0.0, 3, vec![0])],
                vec![at(&s, 1, 0.0, 2, vec![0])],
                vec![at(&s, 2, fork, 1, vec![0, 1])],
                vec![at(&s, 3, fork - 0.1, 0, Vec::new()), at(&s, 3, fork + 0.1, 0, Vec::new())],
            ]
        };
        let layers = tree(0.0);
        let all = spheres(&s, &layers, &print_z(4));
        let first = firsts(&layers);
        let bound = links(&s, &layers, &all, &first);
        assert_eq!(bound[1], f64::INFINITY, "a chain's link is not held");
        for k in 2..5 {
            assert!((bound[k] - 0.213_261).abs() < 1e-6, "#{k}'s link {}", bound[k]);
        }
        let mut pos: Vec<Vec<P>> = vec![vec![(0.0, 0.0)], vec![(0.5, 0.0)], vec![(1.0, 0.0)], vec![(-0.1, 0.0), (0.1, 0.0)]];
        hold(&layers, &all, &first, &bound, &mut pos, &[0, 1, 2]);
        // #1 stays 0.5 off its root; the fork comes to 0.713261 from #1, then to 0.213261 from its
        // tip at -0.1, and its tip at 0.1 is nearer than that.
        near("#1, along a chain", pos[1][0], (0.5, 0.0));
        near("#2, the fork", pos[2][0], (0.113_261, 0.0));
        let link = |pos: &[Vec<P>], (l, i): (usize, usize), (m, j): (usize, usize)| dist(pos[l][i], pos[m][j]);
        for t in 0..2 {
            assert!(link(&pos, (2, 0), (3, t)) <= bound[3 + t] + 1e-6, "the fork's link to tip {t}: {}", link(&pos, (2, 0), (3, t)));
        }
        // The tips are held last, so the link under the fork gives: 0.386739, past its 0.213261.
        assert!((link(&pos, (2, 0), (1, 0)) - 0.386_739).abs() < 1e-6, "the link under the fork: {}", link(&pos, (2, 0), (1, 0)));
        // The fork placed 0.6 off #1: its link may stretch to 0.6 and no further.
        let bent = tree(0.6);
        let all = spheres(&s, &bent, &print_z(4));
        let bound = links(&s, &bent, &all, &first);
        assert!((bound[2] - 0.6).abs() < 1e-6, "{bound:?}");
        let mut pos: Vec<Vec<P>> = vec![vec![(0.0, 0.0)], vec![(0.0, 0.0)], vec![(0.8, 0.0)], vec![(0.5, 0.0), (0.7, 0.0)]];
        hold(&bent, &all, &first, &bound, &mut pos, &[0, 1, 2]);
        near("#2, placed 0.6 off", pos[2][0], (0.6, 0.0));
        assert!(link(&pos, (2, 0), (1, 0)) <= 0.6 + 1e-6, "the link under the fork, placed 0.6: {}", link(&pos, (2, 0), (1, 0)));
        for t in 0..2 {
            assert!(link(&pos, (2, 0), (3, t)) <= bound[3 + t] + 1e-6, "the fork's link to tip {t}: {}", link(&pos, (2, 0), (3, t)));
        }
    }

    /// §7 step 1 on R14's numbers: pushed straight off the nearest outline point by
    /// n = min(overlap + 0.1, 0.5) mm, applied as n × n, on its own layer and the layers above
    /// (the cross-section shrinking by layer counts × 0.2), never a layer below (Q83, copied); a
    /// point inside the outline is pushed deeper in. Each sphere carries one element straight
    /// above where it started, so the smoothing after the push takes back half of it.
    #[test]
    fn a_sphere_cutting_the_outline_is_pushed_by_n_squared() {
        let s = r14(0.8);
        let wall = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-10.0, -10.0), Pt::from_mm(0.0, 10.0)))];
        // (the layer the wall is on, x and r of the sphere on #1, what, its push)
        let cases = [
            // Same layer: overlap 0.5 - 0.3 = 0.2, n = 0.3, push 0.09.
            (1, 0.3, 0.5, "same layer", 0.09),
            // A layer up: cross-section sqrt(0.25 - 0.04) = 0.458258, overlap 0.158258, n 0.258258.
            (2, 0.3, 0.5, "a layer up", 0.258_257_569_495_584_f64.powi(2)),
            // Deep: overlap 0.75, n capped at 0.5, push 0.25.
            (1, 0.05, 0.8, "capped", 0.25),
            // A layer below never counts.
            (0, 0.3, 0.5, "a layer down", 0.0),
            // Inside, 0.1 from the edge: overlap 0.7, n capped at 0.5, pushed 0.25 deeper in.
            (1, -0.1, 0.8, "inside", -0.25),
        ];
        for (on, x, r, what, push) in cases {
            let layers = vec![vec![at(&s, 0, x, 0, Vec::new())], vec![at(&s, 1, x, 0, vec![0])], vec![at(&s, 2, x, 0, Vec::new())]];
            let lines: Vec<Edges> = (0..3).map(|k| Edges::new(if k == on { &wall } else { &[] })).collect();
            let sp = Sphere { l: 1, i: 0, r, w: r, locked: false, top: 2, child: None };
            let (p, cut) = step(&s, &layers, |k| &lines[k], &placed(&layers), &sp);
            near(what, p, (x + push / 2.0, 0.0));
            assert_eq!(cut, push != 0.0, "{what}: a cut keeps the rounds going");
        }
    }

    /// D24: a sphere is pushed by its counters' drawn radius, the one the descent kept clear, and
    /// its anchors reach by its width. A middle lagging under a wider branch (counters at the
    /// tip's 0.40 mm, carrying 0.88 mm) placed 0.6 mm off a wall is clear at 0.40 and stays where
    /// it is, though its width cuts 0.28 mm into the wall: the draw cuts that off.
    #[test]
    fn a_lagging_sphere_is_pushed_by_its_counters_radius() {
        let s = r14(0.8);
        let wall = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-10.0, -10.0), Pt::from_mm(0.0, 10.0)))];
        let mut layers = vec![vec![at(&s, 0, 0.6, 7, vec![0])], vec![at(&s, 1, 0.6, 6, vec![0])], vec![at(&s, 2, 0.6, 4, Vec::new())]];
        for l in layers.iter_mut().take(2) {
            (l[0].ramp, l[0].carried) = (0, 0.88);
        }
        let sp = spheres(&s, &layers, &print_z(3)).swap_remove(1);
        assert_eq!((sp.r, sp.w), (0.4_f32 as f64, 0.88_f32 as f64), "pushed by its counters' radius, held by its width");
        let lines: Vec<Edges> = (0..3).map(|k| Edges::new(if k == 1 { &wall } else { &[] })).collect();
        let (p, cut) = step(&s, &layers, |k| &lines[k], &placed(&layers), &sp);
        near("clear at 0.40", p, (0.6, 0.0));
        assert!(!cut, "no cut at its counters' radius: the rounds stop");
    }

    /// §7 stopping (Q90, copied): only cuts keep the rounds going, so a tree that touches nothing
    /// is smoothed exactly once; the bed root is free and moves; every point is written back
    /// from its float, a locked one 1 nm off.
    #[test]
    fn a_tree_touching_nothing_is_smoothed_once() {
        let s = Settings::new(Keys { bed_only: true, ..r14(0.8).keys });
        let v = Volumes::new(ctl(), &s, &vec![Vec::new(); 5], &[]).unwrap();
        let mut layers: Vec<Vec<Element>> = vec![Vec::new(); 5];
        // A bed root at 5 carrying a tip at 6, and a lone tip at 87.654321 mm.
        layers[0].push(at(&s, 0, 5.0, 1, vec![0]));
        layers[1].push(at(&s, 1, 6.0, 0, Vec::new()));
        layers[1].push(at(&s, 1, 0.0, 0, Vec::new()));
        layers[1][1].placed = Some(Pt::new(87_654_321, 0));
        // A root on the part, a bent middle and a tip.
        layers[2].push(at(&s, 2, 0.0, 2, vec![0]));
        layers[3].push(at(&s, 3, 0.6, 1, vec![0]));
        layers[4].push(at(&s, 4, 0.0, 0, Vec::new()));
        smooth(ctl(), &s, &v, &mut layers, &print_z(5), &mut Memo::default()).unwrap();
        let x = |l: usize, i: usize| layers[l][i].placed.expect("placed").x;
        assert_eq!((x(2, 0), x(4, 0)), (0, 0), "the root on the part and the tip stay");
        assert!((x(3, 0) - 400_000).abs() <= 1, "smoothed once, half way to 0 capped at 0.2: 0.6 to 0.4, not straightened ({})", x(3, 0));
        assert!((x(0, 0) - 5_200_000).abs() <= 1, "the bed root is free: half way to its tip, capped, 5.0 to 5.2 ({})", x(0, 0));
        assert_eq!(x(1, 1), 87_654_320, "the lone tip comes back from its float 1 nm off");
    }

    /// §7 stopping with the trees at rest left out: the same points as stepping every free sphere
    /// every round, and holding its links (D23), until a round in which nothing cut. A tree
    /// against a wall keeps the rounds going; a bent tree far from it is smoothed in every one of
    /// them until it is straight and at rest, and a straight one is at rest from the first.
    #[test]
    fn trees_at_rest_come_out_as_if_stepped_every_round() {
        let (s, v) = walled();
        let mut layers = three_trees(&s, true);
        // Every free sphere stepped every round.
        let shared: &[Vec<Element>] = &layers;
        let all = spheres(&s, shared, &print_z(5));
        let zero = s.rung_for(0.0, false);
        let lines: Vec<Edges> = (0..5).map(|k| Edges::new(v.collision(ctl(), k, zero).unwrap())).collect();
        let mut want = placed(shared);
        let first = firsts(shared);
        let links = links(&s, shared, &all, &first);
        let free: Vec<usize> = (0..all.len()).filter(|&k| !all[k].locked).collect();
        let mut rounds = 0;
        while rounds < ROUNDS {
            rounds += 1;
            let moved: Vec<Option<(P, bool)>> = all.iter().map(|sp| (!sp.locked).then(|| step(&s, shared, |k| &lines[k], &want, sp))).collect();
            let mut cut = false;
            for (sp, (p, deep)) in all.iter().zip(moved).filter_map(|(sp, m)| m.map(|m| (sp, m))) {
                want[sp.l][sp.i] = p;
                cut |= deep;
            }
            hold(shared, &all, &first, &links, &mut want, &free);
            if !cut {
                break;
            }
        }
        assert!(want[3][0] == (20.0, 0.0) && rounds > 20, "the bent tree comes to rest straight before the rounds end ({:?} after {rounds})", want[3][0]);
        smooth(ctl(), &s, &v, &mut layers, &print_z(5), &mut Memo::default()).unwrap();
        for (l, row) in layers.iter().enumerate() {
            for (i, e) in row.iter().enumerate() {
                assert_eq!(e.placed, Some(to_nm(want[l][i])), "#{l} {i}");
            }
        }
    }

    /// R14's profile over 5 layers of a wall's outline, from x = 0 mm down.
    fn walled() -> (Settings, Volumes) {
        let s = Settings::new(Keys { bed_only: true, ..r14(0.8).keys });
        let wall = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-10.0, -10.0), Pt::from_mm(0.0, 10.0)))];
        let v = Volumes::new(ctl(), &s, &vec![wall; 5], &[]).unwrap();
        (s, v)
    }

    /// A tree against the wall of [`walled`], if `wall`, and a bent and a straight one far from
    /// it.
    fn three_trees(s: &Settings, wall: bool) -> Vec<Vec<Element>> {
        let mut layers: Vec<Vec<Element>> = vec![Vec::new(); 5];
        // Against the wall: a bed root and a middle cutting into its outline (the wall grown by
        // xy_min), under a tip.
        if wall {
            let x = s.xy_min + 0.2;
            layers[0].push(at(s, 0, x, 2, vec![0]));
            layers[1].push(at(s, 1, x, 1, vec![0]));
            layers[2].push(at(s, 2, x, 0, Vec::new()));
        }
        // Far from it: roots on the part, under a bent middle and a straight one, under tips.
        let k = layers[2].len();
        layers[2].push(at(s, 2, 20.0, 2, vec![0]));
        layers[3].push(at(s, 3, 20.6, 1, vec![0]));
        layers[4].push(at(s, 4, 20.0, 0, Vec::new()));
        layers[2].push(at(s, 2, 30.0, 2, vec![1]));
        layers[3].push(at(s, 3, 30.0, 1, vec![1]));
        layers[4].push(at(s, 4, 30.0, 0, Vec::new()));
        assert_eq!(layers[2].len(), k + 2);
        layers
    }

    /// The memo: a tree the same as one smoothed before comes out as smoothing it afresh does,
    /// whether the rounds now go on past the ones it was stepped through (the wall's tree added)
    /// or stop short of them (taken out again); and a tree kept is taken as it was kept.
    #[test]
    fn a_tree_smoothed_before_comes_out_as_smoothed_afresh() {
        let (s, v) = walled();
        let placed = |l: &[Vec<Element>]| -> Vec<Vec<Option<Pt>>> { l.iter().map(|r| r.iter().map(|e| e.placed).collect()).collect() };
        let fresh = |wall: bool| {
            let mut l = three_trees(&s, wall);
            smooth(ctl(), &s, &v, &mut l, &print_z(5), &mut Memo::default()).unwrap();
            placed(&l)
        };
        let mut memo = Memo::default();
        let again = |wall: bool, memo: &mut Memo| {
            let mut l = three_trees(&s, wall);
            smooth(ctl(), &s, &v, &mut l, &print_z(5), memo).unwrap();
            placed(&l)
        };
        // Without the wall's tree nothing cuts: one round, and the bent tree moves in it.
        assert_eq!(again(false, &mut memo), fresh(false));
        // A tree by its root's x.
        let root_at = |t: &Smoothed, x: f64| (t.pos[0].0 - x).abs() < 1e-3;
        let bent = |memo: &Memo| memo.trees.values().map(|(t, _)| t).find(|t| root_at(t, 20.0)).map(|t| (t.ran, t.rested)).expect("the bent tree is kept");
        assert_eq!((memo.trees.len(), bent(&memo)), (2, (1, false)));
        // With it, the bent tree goes on from there until it is at rest.
        assert_eq!(again(true, &mut memo), fresh(true));
        let (ran, rested) = bent(&memo);
        assert!(ran > 20 && rested, "the bent tree came to rest after {ran} rounds");
        // Without it again, the bent tree is smoothed afresh through the one round.
        assert_eq!(again(false, &mut memo), fresh(false));
        assert_eq!(bent(&memo), (1, false));
        // The straight tree's middle, kept 1 mm on, comes out 1 mm on.
        let straight = memo.trees.values_mut().map(|(t, _)| t).find(|t| root_at(t, 30.0)).expect("the straight tree is kept");
        straight.pos[1].0 += 1.0;
        let kept = straight.pos[1];
        let moved = again(false, &mut memo);
        assert_eq!(moved[3][1], Some(to_nm(kept)), "the straight middle as kept");
        assert_eq!(moved[3][0], fresh(false)[3][0], "the bent middle as smoothed");
    }

    /// A tree is taken from the memo only where the volumes have every outline layer its rounds
    /// read as they read it. With the wall wider on layer 0 alone, which the collision reaches on
    /// layers 0 and 1 (the bottom gap), the wall's tree, which reads them, is smoothed afresh, and
    /// the straight tree, whose middle reads layer 3 alone, is taken as it was kept; with a block
    /// by the straight middle on layer 4, which the collision reaches from layer 3 up (the top
    /// gap), the straight tree is smoothed afresh and the wall's tree is taken.
    #[test]
    fn a_tree_is_taken_only_where_the_outline_it_read_is_as_it_was() {
        let (s, v) = walled();
        let rect = |x0: f64, y0: f64, x1: f64, y1: f64| ExPolygon::new(Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1)));
        // The trees smoothed on `v` and kept every one 1 mm on, so a tree taken comes out 1 mm
        // on; then smoothed afresh and with that memo on volumes with `more` on layer `l`.
        let edit = |l: usize, more: ExPolygon| {
            let mut memo = Memo::default();
            smooth(ctl(), &s, &v, &mut three_trees(&s, true), &print_z(5), &mut memo).unwrap();
            // Taking the layers the smoothing built.
            let part: Vec<Vec<ExPolygon>> = (0..5).map(|k| [Some(rect(-10.0, -10.0, 0.0, 10.0)), (k == l).then(|| more.clone())].into_iter().flatten().collect()).collect();
            let edited = Volumes::new(ctl(), &s, &part, &[]).unwrap().taking(&v);
            for (t, _) in memo.trees.values_mut() {
                t.pos.iter_mut().for_each(|p| p.0 += 1.0);
            }
            let (mut fresh, mut warm) = (three_trees(&s, true), three_trees(&s, true));
            smooth(ctl(), &s, &edited, &mut fresh, &print_z(5), &mut Memo::default()).unwrap();
            smooth(ctl(), &s, &edited, &mut warm, &print_z(5), &mut memo).unwrap();
            (fresh, warm)
        };
        let on = |p: Option<Pt>| p.map(|p| Pt::from_mm(p.x_mm() + 1.0, p.y_mm()));
        let (fresh, warm) = edit(0, rect(-10.0, -10.0, 0.1, 10.0));
        assert!(fresh[0][0].placed != three_trees(&s, true)[0][0].placed, "the wider wall pushes the wall's tree");
        for l in 0..3 {
            assert_eq!(warm[l][0].placed, fresh[l][0].placed, "the wall's tree on layer {l} smoothed afresh");
        }
        assert_eq!(warm[3][1].placed, on(fresh[3][1].placed), "the straight middle as kept");
        let (fresh, warm) = edit(4, rect(30.3, -1.0, 31.3, 1.0));
        assert!(fresh[3][1].placed != three_trees(&s, true)[3][1].placed, "the block pushes the straight middle");
        assert_eq!(warm[3][1].placed, fresh[3][1].placed, "the straight middle smoothed afresh");
        for l in 0..3 {
            assert_eq!(warm[l][0].placed, on(fresh[l][0].placed), "the wall's tree on layer {l} as kept");
        }
    }
}

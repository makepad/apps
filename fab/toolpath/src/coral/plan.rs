//! The branch plan: where every stem of a support stands on each layer and how wide it is, and
//! how its crown fans out to the part. It is a request to the line, never material.
//!
//! Top-down. Each face's contacts split into the fewest groups a branch top can fan out to within
//! the fan reach. Roots are as few as let every branch top get down to one. Branches meet two at a
//! time, the pair that can meet highest first, as long as the meeting still gets down to its root;
//! what is left goes down to its root, round what keeps it off. Stems that leave the plate
//! together share a trunk until they are far enough apart to part. Every stem is sized from the
//! printed section of its turn: as stiff as a column from the plate to its highest contact has to
//! be, with no credit for its fork and no safety factor.

use super::reach::{nearest_off, Reach};
use super::stem::{Sizes, Stiffness, Turn};
use crate::par::{Cancelled, Ctl};
use crate::support_rules::Legal;
use makepad_fab_slicer::geom::*;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::f64::consts::PI;

/// A point in the plan's frame (mm).
pub type P2 = (f64, f64);

/// Of the steep angle's reach a layer, what the plan may use: the rest is the line's.
pub const CAP: f64 = 0.8;
/// Of the steep angle's reach a layer, how far a stem's turn follows its branch.
pub const FOLLOW: f64 = 0.85;
/// Of what a stem follows a layer, how fast it widens under a fork.
pub const FLARE: f64 = 0.6;
/// How far (mm) under its contact's top a fan has arrived over it at most; from there it stands
/// straight, and the crown closes in.
const FULL_BELOW: f64 = 1.6;
/// k-means rounds.
const MEANS: usize = 16;
/// The fewest layers a shared trunk stands.
const TRUNK_LAYERS: usize = 4;
/// A detour's cell (mm).
const CELL: f64 = 0.5;
/// How much lower (mm) than its first guess a meeting may come out and still be looked for on its
/// detour.
const SPAN_MARGIN: f64 = 2.0;
/// Two contacts nearer than this share of the contact spacing, and as near in height as the
/// spacing, are one.
const SAME: f64 = 0.75;
/// How many times a group's branch top and fans are settled before it gives up.
const TRIES: usize = 6;

/// The settings of coral supports (the prototype's defaults).
#[derive(Clone, Debug)]
pub struct Coral {
    /// Contacts lie this far apart (mm).
    pub contact_spacing: f64,
    /// The farthest a branch top fans out to its contacts (mm).
    pub fan: f64,
    /// The steepest a branch leans (deg), under the cap.
    pub spread_deg: f64,
    /// The share of each branch's climb spent easing into and out of its lean.
    pub ease: f64,
    /// The nozzle's side push (N).
    pub drag: f64,
    /// The material's modulus (N/mm2).
    pub modulus: f64,
    /// The wobble allowed under the drag (mm).
    pub wobble: f64,
    /// The thinnest stem (mm).
    pub stem_min: f64,
    /// How far past its start a stem's turn runs (rad).
    pub overlap: f64,
    /// How much a stem's turn is lobed.
    pub lobe: f64,
    /// Stems that leave the plate together share a trunk until they part.
    pub trunks: bool,
}

impl Default for Coral {
    fn default() -> Self {
        Coral { contact_spacing: 3.0, fan: 16.0, spread_deg: 38.0, ease: 0.0, drag: 0.15, modulus: 2000.0, wobble: 0.1, stem_min: 1.0, overlap: 0.9, lobe: 0.06, trunks: true }
    }
}

/// A point of the part's underside a support carries.
#[derive(Clone, Debug)]
pub struct Contact {
    pub at: P2,
    /// The part layer it holds.
    pub part: usize,
    /// The support layer under it, a top gap down: its contact layer.
    pub top: usize,
    /// That layer's top (mm).
    pub z_top: f64,
    /// Where (mm) its fan has arrived over it.
    pub z_full: f64,
}

/// Why a contact is lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// Paint blocks support there.
    Blocked,
    /// No support gets there at the plan's lean.
    Unreachable,
    /// The plan carries it, but no printed material comes within half the contact gap of it.
    Unprinted,
    /// What held it wobbled past what is allowed under the nozzle, or stood on what did, and
    /// went, and nothing else holds it.
    Wobbles,
    /// What held it would print with nothing under it, packed off the part's top it was to stand
    /// on, and went, and nothing else holds it.
    OnAir,
}

/// A contact no support carries, told once.
#[derive(Clone, Debug)]
pub struct Lost {
    pub at: P2,
    pub part: usize,
    pub cause: Cause,
}

/// A way across the plate, with how far along it each point is.
#[derive(Clone, Debug)]
pub struct Route {
    pub pts: Vec<P2>,
    cum: Vec<f64>,
}

impl Route {
    pub fn new(pts: Vec<P2>) -> Self {
        let mut cum = Vec::with_capacity(pts.len());
        let mut s = 0.0;
        for (i, &p) in pts.iter().enumerate() {
            if i > 0 {
                s += dist(pts[i - 1], p);
            }
            cum.push(s);
        }
        Route { pts, cum }
    }

    pub fn len(&self) -> f64 {
        self.cum.last().copied().unwrap_or(0.0)
    }

    /// The point `l` (mm) along it.
    pub fn along(&self, l: f64) -> P2 {
        if l <= 0.0 || self.pts.len() < 2 {
            return self.pts[0];
        }
        let i = self.cum.partition_point(|&c| c < l).min(self.pts.len() - 1).max(1);
        let span = (self.cum[i] - self.cum[i - 1]).max(1e-12);
        mix(self.pts[i - 1], self.pts[i], ((l - self.cum[i - 1]) / span).min(1.0))
    }

    /// Its stretch from `a` to `b` (mm along it), the other way round when `b` < `a`.
    fn part(&self, a: f64, b: f64) -> Route {
        let (lo, hi) = (a.min(b), a.max(b));
        let mut pts = vec![self.along(lo)];
        pts.extend(self.pts.iter().zip(&self.cum).filter(|(_, &c)| c > lo + 1e-9 && c < hi - 1e-9).map(|(&p, _)| p));
        pts.push(self.along(hi));
        if b < a {
            pts.reverse();
        }
        Route::new(pts)
    }
}

/// A fan: from its group's branch top out to one contact, then straight up under it.
#[derive(Clone, Debug)]
pub struct Fan {
    pub contact: usize,
    pub group: usize,
    /// From the branch top to the contact.
    pub route: Route,
    /// Where (mm) it starts: its branch top.
    pub z_node: f64,
    /// Where (mm) it leaves the branch top.
    pub z_dep: f64,
    /// Where (mm) it has arrived over its contact.
    pub z_full: f64,
    pub ease: f64,
    /// How many fans its group has.
    pub count: usize,
}

impl Fan {
    /// Where it is at height `z` (mm).
    pub fn at(&self, z: f64) -> P2 {
        lean_at(&self.route, self.z_dep, self.z_full, self.ease, z)
    }
}

/// A stem's centre and radius (mm) on one layer.
#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub at: P2,
    pub r: f64,
}

/// A stem: one tube from where it stands to its fork, or to its branch top.
#[derive(Clone, Debug)]
pub struct Stem {
    /// Its first layer.
    pub first: usize,
    /// Per layer from `first`.
    pub rows: Vec<Row>,
    /// The two stems it forks into; none at a branch top.
    pub kids: Vec<usize>,
    /// A trunk the two stems it stands in for share until they part.
    pub trunk: bool,
    /// The top (mm) of the highest contact over it: it is as stiff as a column to there.
    pub top: f64,
    /// The contacts it carries.
    pub contacts: Vec<usize>,
    /// At a branch top, its group: the crown grows from its last layer.
    pub group: Option<usize>,
    /// The fork's axis (rad), from kid 0 to kid 1, 3 mm over the fork.
    pub axis: f64,
    /// Its kids' first radii (mm).
    pub lobes: (f64, f64),
    /// Over how many of its last layers it pinches into its kids' lobes.
    pub pinch: usize,
    /// Its first radius (mm), or what its kids need it to be where it has no layers.
    pub r0: f64,
}

impl Stem {
    /// One past its last layer.
    pub fn end(&self) -> usize {
        self.first + self.rows.len()
    }
}

/// The plan of one part's supports.
pub struct Plan {
    pub contacts: Vec<Contact>,
    pub lost: Vec<Lost>,
    pub fans: Vec<Fan>,
    pub stems: Vec<Stem>,
    /// The stems that stand on their footing.
    pub roots: Vec<usize>,
    pub groups: usize,
    /// The steepest a fan leans (mm a mm up), and the cap a stem's lean and taper share.
    pub lean: f64,
    pub cap: f64,
    pub sizes: Sizes,
    pub stiffness: Stiffness,
    pub turn: Turn,
}

impl Plan {
    /// How steep (mm a mm up) a stem standing to `top` may lean at height `z`: the cap less how
    /// fast its stiffness lets it narrow there, at most the spread.
    pub fn stem_lean(&self, z: f64, top: f64) -> f64 {
        stem_lean(&self.sizes, &self.stiffness, self.lean, self.cap, z, top)
    }
}

fn dist(a: P2, b: P2) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn mix(a: P2, b: P2, t: f64) -> P2 {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn pt(p: P2) -> Pt {
    Pt::from_mm(p.0, p.1)
}

fn mm(p: Pt) -> P2 {
    (p.x_mm(), p.y_mm())
}

fn inside(set: &[ExPolygon], p: Pt) -> bool {
    set.iter().any(|e| {
        let b = e.bbox();
        p.x >= b.min.x && p.x <= b.max.x && p.y >= b.min.y && p.y <= b.max.y && e.contains(p)
    })
}

/// How far along its edge a branch is at `u` of its climb: cosine ramps of `e` at both ends, so it
/// leaves upright and arrives upright, and the steepest lean between.
pub fn glide(u: f64, e: f64) -> f64 {
    if e <= 0.0 {
        return u;
    }
    let w = |t: f64| (t - (e / PI) * (PI * t / e).sin()) / 2.0;
    let d = if u < e { w(u) } else if u > 1.0 - e { 1.0 - e - w(1.0 - u) } else { e / 2.0 + (u - e) };
    d / (1.0 - e)
}

/// Where an edge along `route` is at height `z`: at its start up to `dep`, along it gliding with
/// `ease`, at its end from `to` on.
fn lean_at(route: &Route, dep: f64, to: f64, ease: f64, z: f64) -> P2 {
    let u = ((z - dep) / (to - dep).max(1e-6)).clamp(0.0, 1.0);
    route.along(glide(u, ease) * route.len())
}

fn stem_lean(sizes: &Sizes, stiff: &Stiffness, lean: f64, cap: f64, z: f64, top: f64) -> f64 {
    // A stem narrows fastest just under where it reaches the thinnest, and not at all above; over
    // an edge ending at `z` it narrows at most as fast as at `z`, or there.
    let at = z.min(stiff.below(sizes.least(), top));
    let dz = 0.1;
    let taper = ((sizes.radius(stiff.need(at - dz, top)) - sizes.radius(stiff.need(at, top))) / dz).max(0.0);
    lean.min(cap - taper).max(0.1 * cap)
}

/// How an edge goes.
#[derive(Clone, Debug)]
enum Way {
    /// Stands at its route's start up to `dep`, then glides along it to its end at the edge's
    /// top.
    Lean { route: Route, dep: f64, ease: f64 },
    /// Where it stands on each layer from `first`.
    Layers { first: usize, pts: Vec<P2> },
    /// Midway between its two kids.
    Trunk,
}

/// A stem before it is sized.
#[derive(Clone, Debug)]
struct Edge {
    way: Way,
    /// Its top (mm): where its node is.
    z: f64,
    /// The top of its highest contact (mm).
    top: f64,
    kids: Vec<usize>,
    contacts: Vec<usize>,
    group: Option<usize>,
    /// Where it stands, for one that stands on its footing.
    base: usize,
}

/// A branch top, a meeting, or what is left to go down to its root.
#[derive(Clone, Debug)]
struct Node {
    at: P2,
    z: f64,
    contacts: Vec<usize>,
    /// The two edges up from it, for a meeting.
    kids: Vec<usize>,
    top: f64,
    root: usize,
    group: Option<usize>,
}

/// A group: its branch top, its height (mm), its contacts, and per contact its fan's route and
/// where (mm) it leaves the top.
type Group = (P2, f64, Vec<usize>, Vec<(Route, f64)>);

struct Meet {
    z: f64,
    at: P2,
    ea: Way,
    eb: Way,
}

/// A detour's search order: least estimated cost first.
#[derive(PartialEq)]
struct Open(f64, usize);

impl Eq for Open {}

impl PartialOrd for Open {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Open {
    fn cmp(&self, o: &Self) -> Ordering {
        o.0.total_cmp(&self.0).then(o.1.cmp(&self.1))
    }
}

struct Planner<'a> {
    ctl: Ctl<'a>,
    legal: &'a Legal<'a>,
    reach: &'a Reach,
    s: &'a Coral,
    z: Vec<f64>,
    w: f64,
    tan: f64,
    lean: f64,
    cap: f64,
    sizes: Sizes,
    stiff: Stiffness,
    turn: Turn,
    edges: Vec<Edge>,
}

/// The plan for a part whose supports keep `legal`, from `demand` (per part layer, what of its
/// underside needs support) and `reach`, `legal`'s at [`CAP`] of the steep angle's reach for a
/// bead of the line width.
pub fn plan(ctl: Ctl, legal: &Legal, reach: &Reach, demand: &[Vec<ExPolygon>], s: &Coral) -> Result<Plan, Cancelled> {
    let p = legal.params();
    let tan = p.angle_max_deg.to_radians().tan();
    let cap = CAP * tan;
    let turn = Turn { width: p.line_width, overlap: s.overlap, lobe: s.lobe };
    let mut pl = Planner {
        ctl,
        legal,
        reach,
        s,
        z: legal.heights()[..legal.layers()].iter().map(|h| h.0).collect(),
        w: p.line_width,
        tan,
        lean: s.spread_deg.to_radians().tan().min(cap),
        cap,
        sizes: Sizes::new(&turn, s.stem_min),
        stiff: Stiffness::new(s.drag, s.modulus, s.wobble),
        turn,
        edges: Vec::new(),
    };
    let (contacts, mut lost) = pl.contacts(demand);
    let faces = pl.faces(&contacts);
    let mut grouped = Vec::new();
    let mut dropped = vec![false; contacts.len()];
    for face in &faces {
        if pl.ctl.cancelled() {
            return Err(Cancelled);
        }
        let (groups, gone) = pl.groups(&contacts, face);
        for q in gone {
            dropped[q] = true;
            lost.push(Lost { at: contacts[q].at, part: contacts[q].part, cause: Cause::Unreachable });
        }
        grouped.extend(groups);
    }
    // The contacts the groups carry, numbered again.
    let mut index = vec![usize::MAX; contacts.len()];
    let contacts: Vec<Contact> = contacts.into_iter().enumerate().filter(|&(i, _)| !dropped[i]).enumerate().map(|(k, (i, c))| {
        index[i] = k;
        c
    }).collect();
    let mut fans: Vec<Fan> = Vec::new();
    let mut nodes: Vec<Node> = Vec::new();
    for (at, z, members, routes) in grouped {
        let members: Vec<usize> = members.iter().map(|&q| index[q]).collect();
        let g = nodes.len();
        let count = members.len();
        for (&q, (route, dep)) in members.iter().zip(routes) {
            fans.push(Fan { contact: q, group: g, route, z_node: z, z_dep: dep, z_full: contacts[q].z_full, ease: pl.ease(), count });
        }
        let top = members.iter().map(|&q| contacts[q].z_top).fold(0.0, f64::max);
        nodes.push(Node { at, z, contacts: members, kids: Vec::new(), top, root: 0, group: Some(g) });
    }
    let groups = nodes.len();
    let roots = pl.roots(&mut nodes);
    let nodes = pl.meetings(nodes, &roots)?;
    let mut base: Vec<usize> = Vec::new();
    for n in &nodes {
        match pl.descend(n, roots[n.root]) {
            Some((first, way)) => {
                base.push(pl.edges.len());
                pl.edges.push(Edge { way, z: n.z, top: n.top, kids: n.kids.clone(), contacts: n.contacts.clone(), group: n.group, base: first });
            }
            None => {
                for &q in &n.contacts {
                    lost.push(Lost { at: contacts[q].at, part: contacts[q].part, cause: Cause::Unreachable });
                }
                let gone: Vec<usize> = n.contacts.clone();
                fans.retain(|f| !gone.contains(&f.contact));
            }
        }
    }
    if s.trunks {
        base = pl.trunks(base);
    }
    let mut stems: Vec<Option<Stem>> = vec![None; pl.edges.len()];
    for &e in &base {
        let k0 = pl.edges[e].base;
        pl.size(e, k0, &mut stems);
    }
    let stems: Vec<Stem> = stems.into_iter().map(|s| s.unwrap_or_else(|| Stem { first: 0, rows: Vec::new(), kids: Vec::new(), trunk: false, top: 0.0, contacts: Vec::new(), group: None, axis: 0.0, lobes: (0.0, 0.0), pinch: 0, r0: 0.0 })).collect();
    Ok(Plan { contacts, lost, fans, stems, roots: base, groups, lean: pl.lean, cap: pl.cap, sizes: pl.sizes, stiffness: pl.stiff, turn: pl.turn })
}

impl<'a> Planner<'a> {
    fn ease(&self) -> f64 {
        self.s.ease.clamp(0.0, 0.45)
    }

    /// How many layers lie under `z` (mm).
    fn below(&self, z: f64) -> usize {
        self.z.partition_point(|&zl| zl < z - 1e-9)
    }

    /// The layer at height `z` (mm): the last whose top is not over it, the first at least.
    fn layer_of(&self, z: f64) -> usize {
        self.z.partition_point(|&zl| zl <= z + 1e-9).saturating_sub(1)
    }

    fn reached(&self, p: P2, l: usize) -> bool {
        self.reach.reached(pt(p), l)
    }

    /// m_l at the plan's share (mm): how far a layer may lie past the one under it.
    fn step(&self, l: usize) -> f64 {
        if l == 0 { 0.0 } else { self.cap * (self.z[l] - self.z[l - 1]).max(0.0) }
    }

    /// The plan's lean (mm a mm up) for a stem to `top` whose edge ends at `z`, eased.
    fn stem_lean(&self, z: f64, top: f64) -> f64 {
        stem_lean(&self.sizes, &self.stiff, self.lean, self.cap, z, top) * (1.0 - self.ease())
    }

    fn fan_lean(&self) -> f64 {
        self.lean * (1.0 - self.ease())
    }

    fn radius(&self, z: f64, top: f64) -> f64 {
        self.sizes.radius(self.stiff.need(z, top))
    }

    /// Contacts: each demand island sampled at the contact spacing from its corner, a cell's
    /// centre where it lies on the island, else (an island too thin for any centre) the island's
    /// nearest point in each cell it crosses. Bottom up, a place already held from close by and
    /// close under is held once. A contact not reached at its layer moves to the nearest reached
    /// point within half the spacing, or is lost, with its cause.
    fn contacts(&self, demand: &[Vec<ExPolygon>]) -> (Vec<Contact>, Vec<Lost>) {
        let c = self.s.contact_spacing;
        let key = |p: P2| ((p.0 / c).floor() as i64, (p.1 / c).floor() as i64);
        let mut kept: Vec<Contact> = Vec::new();
        let mut lost: Vec<Lost> = Vec::new();
        let mut seen: HashMap<(i64, i64), Vec<(P2, f64)>> = HashMap::new();
        for (d, layer) in demand.iter().enumerate().take(self.legal.layers()) {
            if layer.is_empty() {
                continue;
            }
            let top = self.legal.contact(d);
            let zd = self.z[d];
            for e in layer {
                let bb = e.bbox();
                let (x0, y0) = (bb.min.x_mm(), bb.min.y_mm());
                let nx = ((bb.max.x_mm() - x0) / c).ceil().max(1.0) as usize;
                let ny = ((bb.max.y_mm() - y0) / c).ceil().max(1.0) as usize;
                let centre = |i: usize, j: usize| (x0 + (i as f64 + 0.5) * c, y0 + (j as f64 + 0.5) * c);
                let mut found: Vec<P2> = Vec::new();
                for j in 0..ny {
                    for i in 0..nx {
                        let q = centre(i, j);
                        if e.contains(pt(q)) {
                            found.push(q);
                        }
                    }
                }
                if found.is_empty() {
                    for j in 0..ny {
                        for i in 0..nx {
                            let q = centre(i, j);
                            let n = nearest_on_island(e, q);
                            if (n.0 - q.0).abs() <= c / 2.0 && (n.1 - q.1).abs() <= c / 2.0 {
                                found.push(n);
                            }
                        }
                    }
                }
                for q in found {
                    let k = key(q);
                    let near = (-1..=1).any(|a| (-1..=1).any(|b| seen.get(&(k.0 + a, k.1 + b)).is_some_and(|v| v.iter().any(|&(p, z)| dist(p, q) < SAME * c && (zd - z).abs() < c))));
                    if near {
                        continue;
                    }
                    seen.entry(k).or_default().push((q, zd));
                    let at = top.and_then(|t| self.place(q, t));
                    match (top, at) {
                        (Some(t), Some(at)) => kept.push(Contact { at, part: d, top: t, z_top: self.z[t], z_full: self.full(at, t) }),
                        _ => {
                            let blocked = top.is_some_and(|t| inside(self.legal.blocked(t), pt(q)));
                            lost.push(Lost { at: q, part: d, cause: if blocked { Cause::Blocked } else { Cause::Unreachable } });
                        }
                    }
                }
            }
        }
        (kept, lost)
    }

    /// Where a contact asked for at `q` on layer `t` goes: where its column is open from where its
    /// fan arrives up to `t`, there or within half the spacing; else where layer `t` is reached,
    /// there or within half the spacing, its column open only higher up; none when neither is.
    fn place(&self, q: P2, t: usize) -> Option<P2> {
        let lf = self.below(self.band(t)).min(t);
        let open = |p: P2| (lf..=t).all(|l| self.reached(p, l));
        let half = self.s.contact_spacing / 2.0;
        if open(q) {
            return Some(q);
        }
        let moved = [t, lf].into_iter().find_map(|l| self.reach.nearest(pt(q), l, half).map(mm).filter(|&p| open(p) && dist(p, q) <= half));
        moved.or_else(|| if self.reached(q, t) { Some(q) } else { self.reach.nearest(pt(q), t, half).map(mm) })
    }

    /// Where a fan to a contact at `p` on layer `t` has arrived: 1.6 mm under its top, half its
    /// height for a low one, and over every layer it stands on straight to its top reached.
    fn full(&self, p: P2, t: usize) -> f64 {
        let mut zf = self.band(t);
        let lf = self.below(zf);
        if let Some(u) = (lf..t).rev().find(|&l| !self.reached(p, l)) {
            zf = self.z[u + 1];
        }
        zf
    }

    /// Where a fan to a contact on layer `t` arrives when its column is open: 1.6 mm under its
    /// top, or half its height for a low one.
    fn band(&self, t: usize) -> f64 {
        let zt = self.z[t];
        (0.5 * zt).max(zt - FULL_BELOW)
    }

    /// A contact only its own top layers reach, as in a wall's side gap: no fan gets to it unless
    /// it comes in along those layers, and nothing stands under it.
    fn shallow(&self, q: &Contact) -> bool {
        q.z_full > self.band(q.top) + 1e-9
    }

    /// Contacts that lie together: on neighbouring places and heights.
    fn faces(&self, contacts: &[Contact]) -> Vec<Vec<usize>> {
        let c = 1.5 * self.s.contact_spacing;
        let mut id: Vec<usize> = (0..contacts.len()).collect();
        fn find(id: &mut [usize], i: usize) -> usize {
            let mut r = i;
            while id[r] != r {
                r = id[r];
            }
            let mut k = i;
            while id[k] != r {
                let n = id[k];
                id[k] = r;
                k = n;
            }
            r
        }
        let mut cells: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
        for (i, q) in contacts.iter().enumerate() {
            cells.entry(((q.at.0 / c).floor() as i64, (q.at.1 / c).floor() as i64)).or_default().push(i);
        }
        for (i, q) in contacts.iter().enumerate() {
            let k = ((q.at.0 / c).floor() as i64, (q.at.1 / c).floor() as i64);
            for a in -1..=1 {
                for b in -1..=1 {
                    for &j in cells.get(&(k.0 + a, k.1 + b)).map_or(&[][..], Vec::as_slice) {
                        if j > i && dist(q.at, contacts[j].at) <= c && (q.z_top - contacts[j].z_top).abs() <= c {
                            let (ri, rj) = (find(&mut id, i), find(&mut id, j));
                            if ri != rj {
                                id[ri.max(rj)] = ri.min(rj);
                            }
                        }
                    }
                }
            }
        }
        let mut faces: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..contacts.len() {
            let r = find(&mut id, i);
            faces.entry(r).or_default().push(i);
        }
        let mut out: Vec<Vec<usize>> = faces.into_values().collect();
        out.sort_by_key(|f| f[0]);
        out
    }

    /// A face's groups, the fewest such that every contact is within the fan reach of its group's
    /// branch top along a way its fan can take: per group its branch top, its height, its contacts
    /// and per contact its fan's route and where it leaves the top; and the contacts no fan gets
    /// to and nothing stands under, which are lost.
    fn groups(&self, contacts: &[Contact], face: &[usize]) -> (Vec<Group>, Vec<usize>) {
        let c = self.s.contact_spacing;
        let mut face = face.to_vec();
        let mut lost = Vec::new();
        'grouping: loop {
            let pts: Vec<P2> = face.iter().map(|&q| contacts[q].at).collect();
            let n = pts.len();
            if n == 0 {
                return (Vec::new(), lost);
            }
            let centroid = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 / n as f64, a.1 + p.1 / n as f64));
            let per = PI * (self.s.fan + c / std::f64::consts::SQRT_2).powi(2) / (c * c);
            let least = ((n as f64 / per).ceil() as usize).clamp(1, n);
            for kk in least..n {
                let (cs, asg) = kmeans(&pts, &vec![1.0; n], kk, centroid);
                let mut out = Vec::new();
                let mut ok = true;
                for (g, &centre) in cs.iter().enumerate() {
                    let members: Vec<usize> = (0..n).filter(|&i| asg[i] == g).map(|i| face[i]).collect();
                    if members.is_empty() {
                        continue;
                    }
                    match self.group(contacts, centre, &members) {
                        Ok((at, z, routes)) => out.push((at, z, members, routes)),
                        // A contact no fan gets to: the group goes on without it.
                        Err(Some(m)) if self.shallow(&contacts[members[m]]) => {
                            lost.push(members[m]);
                            face.retain(|&q| q != members[m]);
                            continue 'grouping;
                        }
                        Err(_) => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    return (out, lost);
                }
            }
            // Each contact on its own: its branch top under it, but one that nothing stands under
            // is lost.
            let (alone, gone): (Vec<usize>, Vec<usize>) = face.iter().partition(|&&q| !self.shallow(&contacts[q]));
            lost.extend(gone);
            return (alone.iter().map(|&q| (contacts[q].at, contacts[q].z_full, vec![q], vec![(Route::new(vec![contacts[q].at, contacts[q].at]), contacts[q].z_full)])).collect(), lost);
        }
    }

    /// One group's branch top near `centre`, where the layer under it is reached, its height, and
    /// its fans. Fails when the fans cannot get there, naming the member when it is that one's fan
    /// alone that cannot.
    fn group(&self, contacts: &[Contact], centre: P2, members: &[usize]) -> Result<(P2, f64, Vec<(Route, f64)>), Option<usize>> {
        let lean = self.fan_lean();
        let zf = members.iter().map(|&q| contacts[q].z_full).fold(f64::INFINITY, f64::min);
        let mut at = mm(self.reach.nearest(pt(centre), self.layer_of(zf), self.s.fan).ok_or(None)?);
        let straight = |at: P2| -> Vec<(Route, f64)> {
            members.iter().map(|&q| {
                let r = Route::new(vec![at, contacts[q].at]);
                let dep = contacts[q].z_full - r.len() / lean;
                (r, dep)
            }).collect()
        };
        let mut routes = straight(at);
        for _ in 0..TRIES {
            let z = routes.iter().map(|r| r.1).fold(f64::INFINITY, f64::min);
            if z < 0.0 || routes.iter().any(|r| r.0.len() > self.s.fan) {
                return Err(None);
            }
            let lt = self.below(z);
            if lt == 0 {
                return Err(None);
            }
            // The branch top stands where the layer under it is reached, its fans straight again.
            if !self.reached(at, lt - 1) {
                at = mm(self.reach.nearest(pt(at), lt - 1, self.s.fan).ok_or(None)?);
                routes = straight(at);
                continue;
            }
            let mut again = false;
            for (k, &q) in members.iter().enumerate() {
                let q = &contacts[q];
                let fine = |r: &Route, dep: f64| (lt..self.below(q.z_full)).all(|l| self.reached(lean_at(r, dep, q.z_full, self.ease(), self.z[l]), l));
                if fine(&routes[k].0, routes[k].1) {
                    continue;
                }
                let r = self.detour(at, q.at, self.layer_of(z - SPAN_MARGIN), self.below(q.z_full)).ok_or(Some(k))?;
                let dep = q.z_full - r.len() / lean;
                if !fine(&r, dep) {
                    return Err(Some(k));
                }
                again |= dep < z;
                routes[k] = (r, dep);
            }
            if !again {
                return Ok((at, z, routes));
            }
        }
        Err(None)
    }

    /// Roots: as few as let every branch top get down to its own at the lean, each where its
    /// branch tops weigh in, moved onto the bed where a stem as wide as the tallest needs stands.
    /// Sets each node's root.
    fn roots(&self, nodes: &mut [Node]) -> Vec<P2> {
        if nodes.is_empty() {
            return Vec::new();
        }
        let top = nodes.iter().map(|n| n.top).fold(0.0, f64::max);
        let keep = self.legal.keep_off(0, self.w + 2.0 * self.radius(0.0, top));
        let ok = |p: Pt| !inside(&keep, p) && self.reach.reached(p, 0);
        let onto = |p: P2| -> P2 {
            if ok(pt(p)) {
                return p;
            }
            nearest_off(&keep, pt(p), 1e3, ok).or_else(|| self.reach.nearest(pt(p), 0, 1e3)).map_or(p, mm)
        };
        let pts: Vec<P2> = nodes.iter().map(|n| n.at).collect();
        let wts: Vec<f64> = nodes.iter().map(|n| n.contacts.len() as f64).collect();
        let total: f64 = wts.iter().sum();
        let centroid = pts.iter().zip(&wts).fold((0.0, 0.0), |a, (p, w)| (a.0 + p.0 * w / total, a.1 + p.1 * w / total));
        let mut last = (Vec::new(), Vec::new());
        for k in 1..=nodes.len() {
            let (cs, asg) = kmeans(&pts, &wts, k, centroid);
            let roots: Vec<P2> = cs.iter().map(|&c| onto(c)).collect();
            let fits = nodes.iter().zip(&asg).all(|(n, &r)| dist(n.at, roots[r]) <= self.stem_lean(n.z, n.top) * n.z);
            last = (roots, asg);
            if fits {
                break;
            }
        }
        for (n, &r) in nodes.iter_mut().zip(&last.1) {
            n.root = r;
        }
        last.0
    }

    /// Where two nodes meet along `len` (mm) between them: the height both can reach, and how far
    /// from `a`.
    fn meeting(&self, a: &Node, b: &Node, len: f64) -> (f64, f64) {
        let (la, lb) = (self.stem_lean(a.z, a.top), self.stem_lean(b.z, b.top));
        let x = ((a.z - b.z + len / lb) / (1.0 / la + 1.0 / lb)).clamp(0.0, len);
        ((a.z - x / la).min(b.z - (len - x) / lb), x)
    }

    /// Where `a` and `b` meet, and the two edges up from there; `None` when they cannot, or the
    /// meeting cannot get down to their root.
    fn meet(&self, a: &Node, b: &Node, root: P2) -> Option<Meet> {
        let top = a.top.max(b.top);
        let (guess, _) = self.meeting(a, b, dist(a.at, b.at));
        let straight = Route::new(vec![a.at, b.at]);
        let try_one = |route: &Route| -> Option<Meet> {
            let (z, x) = self.meeting(a, b, route.len());
            if z <= self.z[0] {
                return None;
            }
            let at = route.along(x);
            let lz = self.below(z);
            if lz == 0 || !self.reached(at, lz - 1) || dist(at, root) > self.stem_lean(z, top) * z {
                return None;
            }
            let way = |child: &Node, r: Route| {
                let dep = child.z - r.len() / self.stem_lean(child.z, child.top);
                let fine = (lz..self.below(child.z)).all(|l| self.reached(lean_at(&r, dep, child.z, self.ease(), self.z[l]), l));
                fine.then_some(Way::Lean { route: r, dep, ease: self.ease() })
            };
            let ea = way(a, route.part(x, 0.0))?;
            let eb = way(b, route.part(x, route.len()))?;
            Some(Meet { z, at, ea, eb })
        };
        try_one(&straight).or_else(|| {
            let r = self.detour(a.at, b.at, self.layer_of(guess - SPAN_MARGIN), self.below(a.z.max(b.z)))?;
            try_one(&r)
        })
    }

    /// Branches meet two at a time, the pair that can meet highest first; what is left goes down
    /// to its root.
    fn meetings(&mut self, mut nodes: Vec<Node>, roots: &[P2]) -> Result<Vec<Node>, Cancelled> {
        let mut alive: Vec<bool> = vec![true; nodes.len()];
        let mut tried: HashMap<(usize, usize), Option<Meet>> = HashMap::new();
        loop {
            if self.ctl.cancelled() {
                return Err(Cancelled);
            }
            // Each pair's highest meeting were it straight: no way round is higher.
            let mut cand: Vec<(f64, usize, usize)> = Vec::new();
            for i in 0..nodes.len() {
                for k in i + 1..nodes.len() {
                    if alive[i] && alive[k] && nodes[i].root == nodes[k].root {
                        let (z, _) = self.meeting(&nodes[i], &nodes[k], dist(nodes[i].at, nodes[k].at));
                        if z > self.z[0] {
                            cand.push((z, i, k));
                        }
                    }
                }
            }
            cand.sort_by(|a, b| b.0.total_cmp(&a.0).then((a.1, a.2).cmp(&(b.1, b.2))));
            let mut best: Option<(f64, usize, usize)> = None;
            for &(bound, i, k) in &cand {
                if best.is_some_and(|b| b.0 >= bound) {
                    break;
                }
                let m = tried.entry((i, k)).or_insert_with(|| self.meet(&nodes[i], &nodes[k], roots[nodes[i].root]));
                if let Some(m) = m {
                    if best.is_none_or(|b| m.z > b.0) {
                        best = Some((m.z, i, k));
                    }
                }
            }
            let Some((_, i, k)) = best else { break };
            let m = tried.remove(&(i, k)).flatten().expect("the best meeting was tried");
            let (a, b) = (&nodes[i], &nodes[k]);
            let ea = self.edges.len();
            self.edges.push(Edge { way: m.ea, z: a.z, top: a.top, kids: a.kids.clone(), contacts: a.contacts.clone(), group: a.group, base: 0 });
            self.edges.push(Edge { way: m.eb, z: b.z, top: b.top, kids: b.kids.clone(), contacts: b.contacts.clone(), group: b.group, base: 0 });
            let merged = Node { at: m.at, z: m.z, contacts: [a.contacts.clone(), b.contacts.clone()].concat(), kids: vec![ea, ea + 1], top: a.top.max(b.top), root: a.root, group: None };
            alive[i] = false;
            alive[k] = false;
            nodes.push(merged);
            alive.push(true);
        }
        Ok(nodes.into_iter().zip(alive).filter(|(_, a)| *a).map(|(n, _)| n).collect())
    }

    /// How `n` gets down toward `root`: straight, round what keeps it off, or by the reach's
    /// witnesses wherever that lets it stand; its first layer, and its way. `None` when it does
    /// not get down at all.
    fn descend(&self, n: &Node, root: P2) -> Option<(usize, Way)> {
        let lt = self.below(n.z);
        if lt == 0 {
            return Some((0, Way::Layers { first: 0, pts: Vec::new() }));
        }
        let lean = self.stem_lean(n.z, n.top);
        // From the top down: the layer it stands from, if every layer over it is reached.
        let stands = |at: &dyn Fn(usize) -> P2| -> Option<usize> {
            for l in (0..lt).rev() {
                let p = at(l);
                if !self.reached(p, l) {
                    return None;
                }
                if self.reach.rests_on(pt(p), l) {
                    return Some(l);
                }
            }
            Some(0)
        };
        let lean_way = |route: Route| -> Option<(usize, Way)> {
            let dep = n.z - route.len() / lean;
            let first = stands(&|l| lean_at(&route, dep, n.z, self.ease(), self.z[l]))?;
            Some((first, Way::Lean { route, dep, ease: self.ease() }))
        };
        if let Some(w) = lean_way(Route::new(vec![root, n.at])) {
            return Some(w);
        }
        if let Some(w) = self.detour(root, n.at, 0, lt - 1).and_then(lean_way) {
            return Some(w);
        }
        // Down by the reach's witnesses, heading for the root.
        let mut p = n.at;
        if !self.reached(p, lt - 1) {
            p = mm(self.reach.nearest(pt(p), lt - 1, self.step(lt))?);
        }
        let mut pts = vec![p];
        let mut first = 0;
        if self.reach.rests_on(pt(p), lt - 1) {
            return Some((lt - 1, Way::Layers { first: lt - 1, pts }));
        }
        for l in (0..lt - 1).rev() {
            let (step, most) = (lean * (self.z[l + 1] - self.z[l]), self.step(l + 1) + 1e-5);
            let d = dist(p, root);
            let mut q = if d > step { mix(p, root, step / d) } else { root };
            if !self.reached(q, l) {
                q = self.reach.nearest(pt(q), l, step).map(mm).filter(|&s| dist(s, p) <= most).or_else(|| self.reach.nearest(pt(p), l, most).map(mm))?;
            }
            pts.push(q);
            p = q;
            if self.reach.rests_on(pt(q), l) {
                first = l;
                break;
            }
        }
        pts.reverse();
        Some((first, Way::Layers { first, pts }))
    }

    /// A way from `a` to `b` round what keeps a support off on layers `l0..=l1`, by the broad
    /// phase's cells: the shortest through cells clear on all of them, pulled straight.
    fn detour(&self, a: P2, b: P2, l0: usize, l1: usize) -> Option<Route> {
        let l1 = l1.min(self.z.len().saturating_sub(1));
        let margin = (0.5 * dist(a, b)).max(6.0);
        let (x0, y0) = (a.0.min(b.0) - margin, a.1.min(b.1) - margin);
        let nx = ((a.0.max(b.0) + margin - x0) / CELL).ceil() as usize + 1;
        let ny = ((a.1.max(b.1) + margin - y0) / CELL).ceil() as usize + 1;
        let centre = |c: usize| (x0 + ((c % nx) as f64 + 0.5) * CELL, y0 + ((c / nx) as f64 + 0.5) * CELL);
        let cell = |p: P2| -> Option<usize> {
            let (i, j) = (((p.0 - x0) / CELL).floor(), ((p.1 - y0) / CELL).floor());
            (i >= 0.0 && j >= 0.0 && (i as usize) < nx && (j as usize) < ny).then(|| j as usize * nx + i as usize)
        };
        let mut free: Vec<bool> = (0..nx * ny).map(|c| (l0..=l1).all(|l| self.reach.clear(pt(centre(c)), l))).collect();
        let (s, g) = (cell(a)?, cell(b)?);
        free[s] = true;
        free[g] = true;
        let mut cost = vec![f64::INFINITY; nx * ny];
        let mut from = vec![usize::MAX; nx * ny];
        let mut open = BinaryHeap::new();
        cost[s] = 0.0;
        open.push(Open(dist(centre(s), centre(g)), s));
        while let Some(Open(_, c)) = open.pop() {
            if c == g {
                break;
            }
            let (i, j) = ((c % nx) as i64, (c / nx) as i64);
            for (di, dj) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
                let (ni, nj) = (i + di, j + dj);
                if ni < 0 || nj < 0 || ni >= nx as i64 || nj >= ny as i64 {
                    continue;
                }
                let o = nj as usize * nx + ni as usize;
                // A diagonal step passes between its two side cells: both must be clear.
                if !free[o] || (di != 0 && dj != 0 && (!free[j as usize * nx + ni as usize] || !free[nj as usize * nx + i as usize])) {
                    continue;
                }
                let nc = cost[c] + if di != 0 && dj != 0 { std::f64::consts::SQRT_2 } else { 1.0 } * CELL;
                if nc < cost[o] {
                    cost[o] = nc;
                    from[o] = c;
                    open.push(Open(nc + dist(centre(o), centre(g)), o));
                }
            }
        }
        if from[g] == usize::MAX && s != g {
            return None;
        }
        let mut chain = vec![g];
        while *chain.last().unwrap_or(&s) != s {
            chain.push(from[*chain.last()?]);
        }
        chain.reverse();
        let mut way: Vec<P2> = vec![a];
        // Endpoints in one cell leave no interior centres.
        way.extend(chain.iter().skip(1).take(chain.len().saturating_sub(2)).map(|&c| centre(c)));
        way.push(b);
        // Pulled straight: from each kept point, on to the farthest one in plain sight.
        let sight = |p: P2, q: P2| {
            let k = (dist(p, q) / (0.5 * CELL)).ceil().max(1.0) as usize;
            (1..k).all(|t| cell(mix(p, q, t as f64 / k as f64)).is_some_and(|c| free[c]))
        };
        let mut out = vec![a];
        let mut i = 0;
        while i + 1 < way.len() {
            let j = (i + 1..way.len()).rev().find(|&j| sight(way[i], way[j])).unwrap_or(i + 1);
            out.push(way[j]);
            i = j;
        }
        Some(Route::new(out))
    }

    /// Where edge `e` stands on layer `l`.
    fn at(&self, e: usize, l: usize) -> P2 {
        let edge = &self.edges[e];
        match &edge.way {
            Way::Lean { route, dep, ease } => lean_at(route, *dep, edge.z, *ease, self.z[l.min(self.z.len() - 1)]),
            Way::Layers { first, pts } => pts.get(l.saturating_sub(*first).min(pts.len().saturating_sub(1))).copied().unwrap_or((0.0, 0.0)),
            Way::Trunk => mix(self.at(edge.kids[0], l), self.at(edge.kids[1], l), 0.5),
        }
    }

    /// Where edge `e` is at height `z` (mm).
    fn at_z(&self, e: usize, z: f64) -> P2 {
        let edge = &self.edges[e];
        match &edge.way {
            Way::Lean { route, dep, ease } => lean_at(route, *dep, edge.z, *ease, z),
            Way::Layers { .. } => self.at(e, self.layer_of(z)),
            Way::Trunk => mix(self.at_z(edge.kids[0], z), self.at_z(edge.kids[1], z), 0.5),
        }
    }

    /// Stems that leave the plate together pair off, the pair that stays together longest first:
    /// each pair's trunk stands in for both up to where they are far enough apart to pinch off,
    /// as stiff as the taller needs. The stems that then stand on their footing.
    fn trunks(&mut self, mut base: Vec<usize>) -> Vec<usize> {
        // The layer two standing stems part on: the first where they are two stems' width apart,
        // or where the trunk between them would not be reached.
        let part = |pl: &Planner, a: usize, b: usize| -> usize {
            let (ea, eb) = (&pl.edges[a], &pl.edges[b]);
            if ea.base != eb.base {
                return ea.base.max(eb.base);
            }
            let end = pl.below(ea.z.min(eb.z)).saturating_sub(TRUNK_LAYERS);
            for l in ea.base..end {
                let (pa, pb) = (pl.at(a, l), pl.at(b, l));
                let width = pl.radius(pl.z[l], ea.top) + pl.radius(pl.z[l], eb.top) + 2.0 * pl.w;
                if dist(pa, pb) >= width || !pl.reached(mix(pa, pb, 0.5), l) {
                    return l;
                }
            }
            end.max(ea.base)
        };
        while base.len() > 1 {
            let mut best: Option<(usize, usize, usize)> = None;
            for i in 0..base.len() {
                for j in i + 1..base.len() {
                    let l = part(self, base[i], base[j]);
                    if best.is_none_or(|b| l > b.0) {
                        best = Some((l, i, j));
                    }
                }
            }
            let Some((l, i, j)) = best else { break };
            let (a, b) = (base[i], base[j]);
            if l < self.edges[a].base + TRUNK_LAYERS {
                break;
            }
            let e = self.edges.len();
            let (ea, eb) = (&self.edges[a], &self.edges[b]);
            self.edges.push(Edge { way: Way::Trunk, z: self.z[l], top: ea.top.max(eb.top), kids: vec![a, b], contacts: [ea.contacts.clone(), eb.contacts.clone()].concat(), group: None, base: ea.base });
            base.retain(|&q| q != a && q != b);
            base.push(e);
        }
        base
    }

    /// Sizes stem `e` from layer `k0`, its kids first: as stiff as a column from the plate to its
    /// top, and under a fork widening to hold both kids side by side.
    fn size(&self, e: usize, k0: usize, out: &mut Vec<Option<Stem>>) {
        let edge = &self.edges[e];
        let last = self.below(edge.z) as isize - 1;
        let k1 = last.max(k0 as isize - 1);
        let next = (k1 + 1) as usize;
        for &q in &edge.kids {
            self.size(q, next, out);
        }
        let r0_of = |q: usize, out: &Vec<Option<Stem>>| out[q].as_ref().map_or(self.sizes.min(), |s| s.r0);
        let (mut axis, mut fork, mut lobes) = (0.0, 0.0, (0.0, 0.0));
        if edge.kids.len() == 2 {
            let zf = self.z[next.min(self.z.len() - 1)] + 3.0;
            let (a, b) = (self.at_z(edge.kids[0], zf), self.at_z(edge.kids[1], zf));
            axis = (b.1 - a.1).atan2(b.0 - a.0);
            lobes = (r0_of(edge.kids[0], out), r0_of(edge.kids[1], out));
            fork = lobes.0 + lobes.1 + self.w;
        }
        let flare = FLARE * FOLLOW * self.tan;
        let rows: Vec<Row> = (k0 as isize..=k1).map(|k| {
            let k = k as usize;
            let z = self.z[k];
            let r = self.radius(z, edge.top).max(fork - (self.z[k1 as usize] - z) * flare);
            Row { at: self.at(e, k), r }
        }).collect();
        let pinch = if edge.kids.len() == 2 {
            let h = if k1 >= 1 { self.z[k1 as usize] - self.z[k1 as usize - 1] } else { self.z[0] };
            rows.len().min(((lobes.0 + lobes.1) / (0.5 * FOLLOW * self.tan * h.max(1e-6))).ceil() as usize)
        } else {
            0
        };
        let r0 = rows.first().map_or(self.sizes.min().max(fork), |r| r.r);
        out[e] = Some(Stem { first: k0, rows, kids: edge.kids.clone(), trunk: matches!(edge.way, Way::Trunk), top: edge.top, contacts: edge.contacts.clone(), group: edge.group, axis, lobes, pinch, r0 });
    }
}

/// The point of island `e`'s outline nearest `q`.
fn nearest_on_island(e: &ExPolygon, q: P2) -> P2 {
    let mut best = (f64::INFINITY, q);
    for ring in e.rings() {
        let m = ring.pts.len();
        for i in 0..m {
            let (a, b) = (mm(ring.pts[i]), mm(ring.pts[(i + 1) % m]));
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l2 = dx * dx + dy * dy;
            let t = if l2 > 0.0 { (((q.0 - a.0) * dx + (q.1 - a.1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
            let p = (a.0 + dx * t, a.1 + dy * t);
            let d = dist(p, q);
            if d < best.0 {
                best = (d, p);
            }
        }
    }
    best.1
}

/// k-means of `pts` weighted by `wts` into `k`: seeds farthest from `from`, then farthest from the
/// seeds; the centres, and each point's.
fn kmeans(pts: &[P2], wts: &[f64], k: usize, from: P2) -> (Vec<P2>, Vec<usize>) {
    let n = pts.len();
    let first = (0..n).fold(0, |m, i| if dist(pts[i], from) > dist(pts[m], from) { i } else { m });
    let mut seeds = vec![first];
    let min_to = |i: usize, seeds: &[usize]| seeds.iter().map(|&s| dist(pts[i], pts[s])).fold(f64::INFINITY, f64::min);
    while seeds.len() < k.min(n) {
        let next = (0..n).fold(0, |m, i| if min_to(i, &seeds) > min_to(m, &seeds) { i } else { m });
        seeds.push(next);
    }
    let mut cs: Vec<P2> = seeds.iter().map(|&s| pts[s]).collect();
    let mut asg = vec![0; n];
    for _ in 0..MEANS {
        for i in 0..n {
            asg[i] = (0..cs.len()).fold(0, |m, c| if dist(pts[i], cs[c]) < dist(pts[i], cs[m]) { c } else { m });
        }
        for (c, centre) in cs.iter_mut().enumerate() {
            let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
            for i in (0..n).filter(|&i| asg[i] == c) {
                sx += pts[i].0 * wts[i];
                sy += pts[i].1 * wts[i];
                sw += wts[i];
            }
            if sw > 0.0 {
                *centre = (sx / sw, sy / sw);
            }
        }
    }
    (cs, asg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coral::scenes::{rect, Scene, Solid, LW};
    use crate::coral::stem::imin;

    const SIX: [&str; 6] = ["shelf", "bar", "branches", "tower", "pads", "mushroom"];

    fn planned(s: &Scene, coral: &Coral) -> Plan {
        let legal = s.legal();
        let reach = crate::coral::reach_of(Ctl::default(), &legal, &s.demand()).unwrap();
        plan(Ctl::default(), &legal, &reach, &s.demand(), coral).unwrap()
    }

    /// The contacts carried by the stems standing on their footing.
    fn carried(p: &Plan) -> Vec<usize> {
        let mut c: Vec<usize> = p.roots.iter().flat_map(|&r| p.stems[r].contacts.clone()).collect();
        c.sort();
        c
    }

    fn z(s: &Scene, l: usize) -> f64 {
        s.heights[l].0
    }

    /// Three shelves at three heights: three groups whose branches meet two at a time, down to
    /// one stem on the plate that carries every contact.
    #[test]
    fn three_shelves_fork_two_at_a_time_from_one_root() {
        let p = planned(&Scene::named("branches"), &Coral::default());
        assert_eq!(p.groups, 3, "a group per shelf");
        assert!(p.lost.is_empty(), "lost: {:?}", p.lost);
        assert!(p.stems.iter().all(|s| s.kids.is_empty() || s.kids.len() == 2), "every fork is in two");
        assert_eq!(p.stems.iter().filter(|s| s.kids.len() == 2 && !s.trunk).count(), 2, "two meetings for three shelves");
        assert_eq!(p.roots.len(), 1, "one stem stands on the plate");
        assert_eq!(carried(&p), (0..p.contacts.len()).collect::<Vec<_>>(), "it carries every contact");
    }

    /// Five pads at five heights round one root: every stem on the plate stands where the others
    /// do, and together they carry every pad.
    #[test]
    fn five_pads_share_one_root() {
        let p = planned(&Scene::named("pads"), &Coral { trunks: false, ..Coral::default() });
        assert_eq!(p.groups, 5, "a group per pad");
        assert!(p.lost.is_empty(), "lost: {:?}", p.lost);
        assert_eq!(carried(&p), (0..p.contacts.len()).collect::<Vec<_>>(), "every contact is carried");
        let feet: Vec<P2> = p.roots.iter().map(|&r| p.stems[r].rows[0].at).collect();
        assert!(feet.len() > 1, "more than one stem leaves the root: {feet:?}");
        for a in &feet {
            for b in &feet {
                assert!(dist(*a, *b) < 0.05, "the stems on the plate stand apart: {feet:?}");
            }
        }
    }

    /// A cap on a stem: contacts all round the stem are carried, every plan point is reached, and
    /// the way to the far side goes round the stem.
    #[test]
    fn a_cap_is_reached_around_its_stem() {
        let s = Scene::named("mushroom");
        let p = planned(&s, &Coral::default());
        let legal = s.legal();
        let reach = crate::coral::reach_of(Ctl::default(), &legal, &s.demand()).unwrap();
        // Only what lies within the side gap of the stem (2 mm, and half the spacing it may move)
        // is lost.
        for l in &p.lost {
            assert!(l.at.0.hypot(l.at.1) < 4.0 + 2.0 + 1.5 + LW, "lost away from the stem: {l:?}");
        }
        let quadrants: Vec<usize> = p.contacts.iter().map(|q| ((q.at.1.atan2(q.at.0) + PI) / (PI / 2.0)) as usize % 4).collect();
        assert!((0..4).all(|k| quadrants.contains(&k)), "contacts all round");
        assert_eq!(carried(&p).len(), p.contacts.len(), "every contact is carried");
        for st in &p.stems {
            for (k, row) in st.rows.iter().enumerate() {
                assert!(reach.reached(pt(row.at), st.first + k), "a stem at {:?} on layer {} is not reached", row.at, st.first + k);
            }
        }
        for f in &p.fans {
            let q = &p.contacts[f.contact];
            for l in (0..s.heights.len()).filter(|&l| z(&s, l) >= f.z_node && l <= q.top) {
                assert!(reach.reached(pt(f.at(z(&s, l))), l), "a fan at {:?} on layer {l} is not reached", f.at(z(&s, l)));
            }
        }
        let round = p.fans.iter().any(|f| f.route.pts.len() > 2) || p.stems.iter().any(|st| st.rows.windows(2).any(|w| {
            let (a, b) = (w[0].at, w[1].at);
            a.0.hypot(a.1) < 9.0 && (a.1.atan2(a.0) - b.1.atan2(b.0)).abs() > 1e-3
        }));
        assert!(round, "something goes round the stem");
    }

    /// Every layer, a stem's centre moves at most its lean, and with how much it narrows by its
    /// stiffness at most the cap; a fan's at most its lean. With easing too.
    #[test]
    fn peak_lean_never_exceeds_the_cap() {
        for (name, ease) in SIX.iter().map(|&n| (n, 0.0)).chain([("branches", 0.25), ("mushroom", 0.25)]) {
            let s = Scene::named(name);
            let p = planned(&s, &Coral { ease, ..Coral::default() });
            for st in &p.stems {
                for (k, w) in st.rows.windows(2).enumerate() {
                    let l = st.first + k + 1;
                    let h = z(&s, l) - z(&s, l - 1);
                    let moved = dist(w[0].at, w[1].at);
                    let taper = (p.sizes.radius(p.stiffness.need(z(&s, l), st.top)) - p.sizes.radius(p.stiffness.need(z(&s, l - 1), st.top))).abs();
                    assert!(moved + if st.trunk { 0.0 } else { taper } <= p.cap * h * 1.02 + 1e-5, "{name} ease {ease}: a stem moves {moved:.4} and narrows {taper:.4} onto layer {l}, the cap {:.4}", p.cap * h);
                }
            }
            for f in &p.fans {
                let q = &p.contacts[f.contact];
                for l in (1..=q.top).filter(|&l| z(&s, l - 1) >= f.z_node) {
                    let moved = dist(f.at(z(&s, l)), f.at(z(&s, l - 1)));
                    assert!(moved <= p.lean * (z(&s, l) - z(&s, l - 1)) + 1e-9, "{name} ease {ease}: a fan moves {moved:.4} onto layer {l}");
                }
            }
        }
    }

    /// Eased, every branch leaves its fork upright and arrives upright, and leans its most only
    /// between.
    #[test]
    fn every_fork_is_upright() {
        let s = Scene::named("branches");
        let p = planned(&s, &Coral { ease: 0.25, ..Coral::default() });
        let mut forks = 0;
        for st in p.stems.iter().filter(|st| st.kids.len() == 2 && !st.trunk) {
            for &k in &st.kids {
                let kid = &p.stems[k];
                let moves: Vec<f64> = kid.rows.windows(2).map(|w| dist(w[0].at, w[1].at)).collect();
                let most = moves.iter().copied().fold(0.0, f64::max);
                if most == 0.0 {
                    continue;
                }
                assert!(moves[0] <= 0.1 * most && moves[moves.len() - 1] <= 0.1 * most, "a kid leaves or arrives leaning: {:.4} {:.4} of {most:.4}", moves[0], moves[moves.len() - 1]);
                forks += 1;
            }
        }
        assert!(forks >= 2, "{forks} leaning kids");
    }

    /// A roof over a room with a floor and one window: under the roof, contacts as near the
    /// window as a support leans out through it in time are carried; the rest are lost and told
    /// once each, as unreachable; those a blocker covers, as blocked.
    #[test]
    fn a_contact_too_far_is_lost_and_told() {
        let mut s = Scene::of(&[
            Solid::Box { cx: 0.0, cy: 0.0, w: 30.0, d: 30.0, z0: 0.0, z1: 30.0 },
            Solid::Cut { cx: 0.0, cy: 0.0, w: 26.0, d: 26.0, z0: 2.0, z1: 30.0 },
            Solid::Cut { cx: 14.0, cy: 0.0, w: 4.0, d: 16.0, z0: 2.0, z1: 12.0 },
            Solid::Box { cx: 0.0, cy: 0.0, w: 30.0, d: 30.0, z0: 30.0, z1: 32.0 },
        ]);
        let n = s.slices.len();
        s.blocks = vec![vec![rect(5.0, -13.0, 13.0, -8.0)]; n];
        let p = planned(&s, &Coral::default());
        assert!(!p.contacts.is_empty(), "near the window, contacts are carried");
        assert_eq!(carried(&p).len(), p.contacts.len(), "every contact kept is carried");
        let far = p.lost.iter().filter(|l| l.cause == Cause::Unreachable).count();
        let blocked: Vec<&Lost> = p.lost.iter().filter(|l| l.cause == Cause::Blocked).collect();
        assert!(far > 10 && !blocked.is_empty(), "{far} unreachable, {} blocked", blocked.len());
        for l in &blocked {
            assert!(l.at.0 >= 5.0 && l.at.0 <= 13.0 && l.at.1 >= -13.0 && l.at.1 <= -8.0, "blocked outside the blocker: {l:?}");
        }
        // Along the walls, within their side gap and half the spacing, nothing gets in.
        let nearest_kept = p.contacts.iter().map(|q| q.at.0).fold(f64::INFINITY, f64::min);
        let open = 13.0 - 2.0 - 1.5 - LW;
        for l in p.lost.iter().filter(|l| l.cause == Cause::Unreachable && l.at.0.abs() < open && l.at.1.abs() < open) {
            assert!(l.at.0 < nearest_kept + 3.0, "lost {l:?} nearer the window than a kept contact at x {nearest_kept}");
        }
        // Each is told where it was asked for, or where it moved to within half the spacing.
        for (i, a) in p.lost.iter().enumerate() {
            for b in &p.lost[i + 1..] {
                assert!(dist(a.at, b.at) >= SAME * 3.0 - 1.5 - 1e-9 || a.part != b.part, "told twice: {a:?} {b:?}");
            }
        }
    }

    /// Five pads' stems leave their root together: they pair off into trunks, each standing in for
    /// its two until they are two stems' width apart, and one stem stands on the plate.
    #[test]
    fn roots_that_leave_together_share_a_trunk() {
        let s = Scene::named("pads");
        let p = planned(&s, &Coral::default());
        let trunks: Vec<&Stem> = p.stems.iter().filter(|st| st.trunk).collect();
        assert!(!trunks.is_empty(), "the pads' stems share a trunk");
        assert_eq!(p.roots.len(), 1, "one stem stands on the plate");
        for t in trunks {
            assert!(t.rows.len() >= TRUNK_LAYERS, "a trunk stands {} layers", t.rows.len());
            let (a, b) = (&p.stems[t.kids[0]], &p.stems[t.kids[1]]);
            assert_eq!((a.first, b.first), (t.end(), t.end()), "its two go on from its top");
            let l = t.end();
            let width = p.sizes.radius(p.stiffness.need(z(&s, l), a.top)) + p.sizes.radius(p.stiffness.need(z(&s, l), b.top)) + 2.0 * LW;
            let apart = dist(a.rows.first().map_or(t.rows[0].at, |r| r.at), b.rows.first().map_or(t.rows[0].at, |r| r.at));
            assert!(apart >= width - 1e-6, "they part {apart:.3} mm apart, closer than two stems {width:.3}");
        }
    }

    /// A trunk is as stiff as the taller of its two needs, not as both together: on every layer
    /// it is that, or under its fork as wide as holding its two side by side takes.
    #[test]
    fn a_trunk_is_as_stiff_as_its_tallest_branch_needs() {
        let s = Scene::named("pads");
        let p = planned(&s, &Coral::default());
        let mut rows = 0;
        for t in p.stems.iter().filter(|st| st.trunk) {
            let (a, b) = (&p.stems[t.kids[0]], &p.stems[t.kids[1]]);
            assert_eq!(t.top, a.top.max(b.top), "a trunk stands as tall as the taller");
            // Under its fork it widens to hold both its two side by side (at 45 degrees).
            let last = z(&s, t.end() - 1);
            let fork = |l: usize| t.lobes.0 + t.lobes.1 + LW - (last - z(&s, l)) * FLARE * FOLLOW;
            for (k, row) in t.rows.iter().enumerate() {
                let l = t.first + k;
                let tallest = p.sizes.radius(p.stiffness.need(z(&s, l), t.top));
                assert!((row.r - tallest.max(fork(l))).abs() < 1e-9, "on layer {l} a trunk is {:.3} mm, the taller needs {tallest:.3} and the fork {:.3}", row.r, fork(l));
                rows += 1;
            }
        }
        assert!(rows > 0, "no trunk");
    }

    /// Every stem's radius is the one whose printed turn is just as stiff as the column needs:
    /// the turn 0.05 mm thinner is not.
    #[test]
    fn stem_radius_comes_from_the_printed_section() {
        let mut sized = 0;
        for name in ["bar", "pads"] {
            let s = Scene::named(name);
            let p = planned(&s, &Coral::default());
            let section = |r: f64| imin([&p.turn.points((0.0, 0.0), r, 0.0, 0.0, p.turn.overlap)[..]], LW);
            for st in &p.stems {
                for (k, row) in st.rows.iter().enumerate() {
                    let need = p.stiffness.need(z(&s, st.first + k), st.top);
                    let sizing = p.sizes.radius(need);
                    if row.r > sizing + 1e-9 || sizing <= p.sizes.min() + 1e-9 {
                        continue;
                    }
                    assert!(section(row.r) >= need * (1.0 - 2e-3), "{name}: a turn of {:.3} mm prints {:.4} mm4, the column needs {need:.4}", row.r, section(row.r));
                    assert!(section(row.r - 0.05) < need, "{name}: a turn 0.05 mm thinner than {:.3} would do", row.r);
                    sized += 1;
                }
            }
        }
        assert!(sized > 100, "{sized} rows sized");
    }
}

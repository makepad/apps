//! The supports from their points (R7 §3, §4): a head on each point, straight pillars down to
//! the pad, bridges from the heads beside a pillar onto it, a base under each pillar, and a pad
//! under each part's bases. The parts stand [`SupportSettings::lift`] above the plate on them.
//!
//! - **Heads.** A tip ball sunk into the part at the point, a cone, and the back ball the struts
//!   join (the junction): the profile's pin, on a pillar that is mostly its cone foot where the
//!   part sits low. The head leaves along the surface's normal, leaned no more than
//!   [`SupportSettings::lean`] from straight down; a point whose normal is within 30° of up gets
//!   none. A head that doesn't clear the part is turned toward the clearest way down; failing
//!   that, its point is dropped. The tip is the weak point a support breaks at.
//! - **Islands held from their edge.** An island none of whose points has a head (a part close
//!   under it leaves no room, as under a hinge's plate in its pocket) is held from places round
//!   its first layers' outline instead: a head there that clears the parts, its tip ball curing
//!   into the island's first layer and keeping off the other parts.
//! - **Pillars.** A head with a clear drop to the pad stands on a pillar. Heads near each other
//!   share one: the one nearest their middle drops, the others bridge to it, each bridge
//!   falling at least 45° so it prints on its own.
//! - **Heads with no clear drop** bridge to the nearest pillar that takes them, else are
//!   dropped (and said so).
//! - **Sizes.** Each tip, pillar and bridge is as wide as the peel it carries needs
//!   ([`crate::load`]), never under the settings' and no tip past the profile's most.
//! - **Pads.** One per part: its bases' own outline, closed over the gaps between them and grown
//!   by a brim, clusters apart joined by strips; its edge a chamfer narrowing to the plate.
//!
//! Every clearance is [`crate::clear`]'s exact march, against fields and meshes alike. The
//! supports are solids with exact distances ([`crate::solid`]): they cut into the layers as a
//! field does ([`SupportSection`]) and draw as one mesh ([`Supports::mesh`]).

use crate::clear::{box_distance, clearance, Clear, Probe};
use crate::grid::{Layers, PixelGrid, PixelRect};
use crate::image::LayerStack;
use crate::load::{part_of, Bodies, Load};
use crate::parts::{links, LayerParts, Part, CURES};
use crate::points::{nearest, points_of, PointKind, PointSettings, SupportPoint};
use crate::section::{field_cover, outline_cover, Box3, Field, Lifted, Near, Section};
use crate::solid::{add, dot, norm, scale, sub, Pad, Solid, PAD_CHORD, V3};
use makepad_fab_slicer::clip::{self, FillRule};
use makepad_fab_slicer::geom::{clean_ring, simplify_ring, ExPolygon, Polygon, Pt, SCALE};
use fab_plate::PartMesh;
use makepad_fab_slicer::offset::{self, Join};
use std::f64::consts::{PI, TAU};
use std::sync::Arc;

/// How the supports are built (mm, angles in radians).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SupportSettings {
    /// The tip ball's radius, and how deep its centre's far side sits in the part.
    pub tip_radius: f64,
    pub penetration: f64,
    /// The gap between the tip ball and the back ball.
    pub head_width: f64,
    /// A pillar's and a bridge's radius; the back ball's too.
    pub pillar_radius: f64,
    /// A pillar's base: a cone this wide at the pad, this tall.
    pub base_radius: f64,
    pub base_height: f64,
    /// A head's most lean from straight down.
    pub lean: f64,
    /// What every strut keeps from the parts.
    pub gap: f64,
    /// Heads share a pillar within this in plan and this at all; a pillar takes this many
    /// bridges.
    pub share_plan: f64,
    pub share_reach: f64,
    pub bridges: usize,
    /// The parts' lowest point above the pad, the pad's thickness, its brim beyond the bases,
    /// how near pad pieces join, and its edge's slope from the plate (a chamfer under 90°).
    pub elevation: f64,
    pub pad: f64,
    pub brim: f64,
    pub pad_join: f64,
    pub pad_slope: f64,
}

/// The normal supports (R7 §3.1, the printer profile's column): a 4 mm pin, 0.5 mm at the part
/// with its tip ball sunk, on a 1 mm pillar and a 3 mm cone foot; a 1 mm pad with a 45° edge.
impl Default for SupportSettings {
    fn default() -> Self {
        SupportSettings {
            tip_radius: 0.25,
            penetration: 0.5,
            head_width: 3.0,
            pillar_radius: 0.5,
            base_radius: 1.5,
            base_height: 1.0,
            lean: PI / 4.0,
            gap: 0.5,
            share_plan: 3.0,
            share_reach: 10.0,
            bridges: 3,
            elevation: 5.0,
            pad: 1.0,
            brim: 1.6,
            pad_join: 50.0,
            pad_slope: PI / 4.0,
        }
    }
}

impl SupportSettings {
    /// How far the parts stand above the plate: the elevation over the pad.
    pub fn lift(&self) -> f64 {
        self.elevation + self.pad
    }

    /// The pad's top, where the pillars stand.
    pub fn ground(&self) -> f64 {
        self.pad
    }

    /// Where the pillar under `top` meets its base.
    pub fn foot(&self, top: V3) -> V3 {
        [top[0], top[1], self.ground() + self.base_height]
    }

    /// From the point along the axis, for a tip of radius `tip`: the tip ball's centre and the
    /// junction's.
    pub(crate) fn head_span(&self, tip: f64) -> (f64, f64) {
        let centre = tip - self.penetration;
        (centre, centre + tip + self.head_width + self.pillar_radius)
    }

    /// The lowest a bridge may land on a pillar: clear of its base.
    pub(crate) fn lowest_landing(&self) -> f64 {
        self.ground() + self.base_height.max(4.0 * self.pillar_radius)
    }
}

/// The plate's parts as they print: each raised by the settings' lift, held by supports to the
/// points their layers need and sized for the load they carry, and the supports one more section
/// of the same layers.
pub struct Supported {
    /// The parts (lifted, in plate order) and then the supports, in layers from the plate.
    pub stack: LayerStack,
    pub supports: Arc<Supports>,
    /// The points the supports were built to.
    pub points: usize,
    /// The parts' cured pixels in each layer, without the supports ([`crate::parts::cured`]).
    pub cured: Vec<LayerParts>,
    /// The parts' covered pixels over every layer, each by its share: their resin, in pixels a
    /// layer thick ([`crate::estimate::resin_ml`]).
    pub covered: f64,
}

/// `parts` (as they rest on the plate) on supports, cut on `grid` in layers `height` thick (mm).
pub fn supported(parts: Vec<Arc<dyn Section>>, grid: PixelGrid, height: f64, s: &SupportSettings, p: &PointSettings, load: &Load) -> Supported {
    let lifted: Vec<Arc<dyn Section>> = parts.into_iter().map(|section| Arc::new(Lifted { section, lift: s.lift() }) as Arc<dyn Section>).collect();
    let top = lifted.iter().map(|x| x.bounds().1[2]).fold(0.0f64, f64::max);
    let mut stack = LayerStack { sections: lifted, grid, layers: Layers::up_to(top, height) };
    let (cured, covered): (Vec<LayerParts>, Vec<f64>) = stack.map(|l| (LayerParts::of(&l.spans), l.spans.covered())).into_iter().unzip();
    let points = points_of(&stack, &cured, p);
    let supports = Arc::new(Supports::new(&points, &stack.sections, s, load, &cured, &grid, &stack.layers));
    if let Some(section) = SupportSection::new(supports.clone()) {
        stack.sections.push(Arc::new(section));
    }
    Supported { stack, supports, points: points.len(), cured, covered: covered.iter().sum() }
}

/// Why a point got no support.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dropped {
    /// Its surface faces up: no head reaches it from below.
    FacesUp,
    /// No head clears the part from it.
    NoHead,
    /// Its head's junction is another's (within 0.1 mm): that one holds it.
    Twin,
    /// Its head can't drop to the pad nor bridge to a pillar.
    NoRoute,
}

impl Dropped {
    /// Why no support reaches its point, in a few words.
    pub fn why(self) -> &'static str {
        match self {
            Dropped::FacesUp => "its surface faces up",
            Dropped::NoHead => "no head clears the parts round it",
            Dropped::Twin => "another head holds it",
            Dropped::NoRoute => "its head has no way down to the pad",
        }
    }
}

/// A head: from its point (on the part `part`, placed on layer `layer`) along `axis` (unit,
/// leaning down), the tip ball's centre and the junction; the tip's radius from the share of the
/// peel it carries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Head {
    pub point: usize,
    pub part: usize,
    pub layer: u32,
    pub at: V3,
    /// The part's surface normal at the point (outward).
    pub normal: V3,
    pub axis: V3,
    pub tip: V3,
    pub tip_radius: f64,
    pub junction: V3,
}

impl Head {
    /// How far it leans from vertical (radians).
    pub fn lean(&self) -> f64 {
        (-self.axis[2]).clamp(-1.0, 1.0).acos()
    }

    /// From its point along its axis: its tip ball's centre and its junction.
    pub fn span(&self) -> (f64, f64) {
        (dot(sub(self.tip, self.at), self.axis), dot(sub(self.junction, self.at), self.axis))
    }
}

/// A pillar: its top (its own head's junction), the heads it holds (its own first, then those
/// that bridge to it), and its radius from what hangs on it.
#[derive(Clone, Debug, PartialEq)]
pub struct Pillar {
    pub top: V3,
    pub heads: Vec<usize>,
    pub radius: f64,
}

/// A bridge: from its head's junction to where it lands on its pillar, its radius from the
/// head's share.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bridge {
    pub head: usize,
    pub pillar: usize,
    pub at: V3,
    pub radius: f64,
}

impl Bridge {
    /// How steeply it rises to its head (radians above the horizontal).
    pub fn rise(&self, heads: &[Head]) -> f64 {
        let j = heads[self.head].junction;
        (j[2] - self.at[2]).atan2((j[0] - self.at[0]).hypot(j[1] - self.at[1]))
    }
}

/// The supports of a plate.
#[derive(Clone, Debug)]
pub struct Supports {
    pub settings: SupportSettings,
    pub load: Load,
    pub heads: Vec<Head>,
    pub pillars: Vec<Pillar>,
    pub bridges: Vec<Bridge>,
    /// The points the supports were built to (a head's [`Head::point`]; a head that holds an
    /// island from its edge is elsewhere on it).
    pub points: Vec<SupportPoint>,
    /// The points that got no support, and why.
    pub dropped: Vec<(usize, Dropped)>,
    pub solids: Vec<Solid>,
    pub pads: Vec<Pad>,
}

/// How many ways a head may turn when its first doesn't clear: these leans from straight down,
/// each all the way round in this many steps.
const LEANS: [f64; 4] = [0.0, 15.0, 30.0, 45.0];
const TURNS: usize = 12;
/// Where a head's check starts: its ball this far (mm) off the surface's plane. Nearer, the
/// head is its own root on the part; a field steeper than a distance proves a ball clear only
/// with room to spare.
const HEAD_OFF: f64 = 0.15;
/// A head along a surface steeper to its axis than this doesn't leave it (cos of the angle).
const LEAVES: f64 = 0.2;
/// An island held from its edge: the places tried round each of its first layers' outline (at
/// most), the layers from its first, how far apart the heads it takes stand (mm), and what their
/// tip balls keep from the other parts (mm), so they cure onto none.
const EDGE_PLACES: usize = 16;
const EDGE_LAYERS: u32 = 3;
const EDGE_APART: f64 = 0.5;
const TIP_OFF: f64 = 0.2;

impl Supports {
    /// The supports of `points` on `parts` (both as they stand on the supports: lifted), sized
    /// for the peel `load` on the parts' cured layers `cured` (on `grid`, in `layers`).
    pub fn new(points: &[SupportPoint], parts: &[Arc<dyn Section>], s: &SupportSettings, load: &Load, cured: &[LayerParts], grid: &PixelGrid, layers: &Layers) -> Supports {
        let mut dropped = Vec::new();
        let bodies = Bodies::new(cured, grid);
        // 1. heads: each along its normal leaned to the most allowed, else the clearest of the
        // other ways down; each tip as wide as the share of the peel its point carries with every
        // point held (R7 §3.11.8: the tips first), within the profile's least and most
        let up = (PI / 6.0).cos();
        let mut wanted: Vec<(usize, V3)> = Vec::new();
        for (i, p) in points.iter().enumerate() {
            if p.normal[2] > up {
                dropped.push((i, Dropped::FacesUp));
            } else {
                wanted.push((i, leaned(p.normal, s.lean)));
            }
        }
        let on: Vec<Option<(u32, u32)>> = wanted.iter().map(|&(i, _)| part_of(cured, grid, points[i].layer, [points[i].at[0], points[i].at[1]]).map(|k| (points[i].layer, k))).collect();
        let mut found = bodies.shares(&on.iter().flatten().copied().collect::<Vec<_>>()).into_iter();
        let share: Vec<f64> = on.iter().map(|o| o.map_or(0.0, |_| found.next().expect("a share per head").0)).collect();
        let least = load.least_tip(grid);
        let tip = |w: usize, axis: V3| 0.5 * load.tip_for(share[w], (-axis[2]).clamp(-1.0, 1.0).acos()).max(2.0 * s.tip_radius).max(least).min(load.tip_max);
        let part: Vec<usize> = wanted.iter().map(|&(i, _)| part_at(parts, points[i].at)).collect();
        let make = |w: usize, axis: V3| {
            let i = wanted[w].0;
            head(&points[i], i, part[w], axis, tip(w, axis), s)
        };
        let probe = |w: usize, axis: V3| {
            let (i, r) = (wanted[w].0, tip(w, axis));
            head_probe(points[i].at, points[i].normal, axis, r, s.head_span(r), s, s.gap)
        };
        let first: Vec<Option<Probe>> = wanted.iter().enumerate().map(|(w, &(_, axis))| probe(w, axis)).collect();
        let probes: Vec<Probe> = first.iter().flatten().copied().collect();
        let mut fit = clearance(parts, &probes).into_iter();
        let mut heads: Vec<Head> = Vec::new();
        let mut again: Vec<usize> = Vec::new();
        for (w, (&(_, axis), probe)) in wanted.iter().zip(&first).enumerate() {
            match probe.map(|_| fit.next().expect("a verdict per probe")) {
                Some(c) if c.is_clear() => heads.push(make(w, axis)),
                _ => again.push(w),
            }
        }
        let ways = ways_down(s.lean);
        let mut tries: Vec<(usize, V3, Probe)> = Vec::new();
        for &w in &again {
            for &axis in &ways {
                if let Some(pr) = probe(w, axis) {
                    tries.push((w, axis, pr));
                }
            }
        }
        let verdicts = clearance(parts, &tries.iter().map(|t| t.2).collect::<Vec<_>>());
        let mut lost: Vec<usize> = Vec::new();
        for &w in &again {
            let best = tries
                .iter()
                .zip(&verdicts)
                .filter(|(t, _)| t.0 == w)
                .filter_map(|(t, v)| match v {
                    Clear::Clear { least } => Some((t.1, *least)),
                    Clear::Blocked { .. } => None,
                })
                .max_by(|a, b| a.1.total_cmp(&b.1));
            match best {
                Some((axis, _)) => heads.push(make(w, axis)),
                None => lost.push(w),
            }
        }
        // 1b. an island none of whose points has a head is held from its edge: the places round
        // its first layers' outline whose head clears the parts and whose tip cures into its
        // first layer, the clearest first, as many as it had points
        let island = |w: usize| on[w].filter(|_| points[wanted[w].0].kind == PointKind::Island);
        let headed: std::collections::HashSet<(u32, u32)> = (0..wanted.len()).filter(|w| !lost.contains(w)).filter_map(island).collect();
        let mut stranded: Vec<((u32, u32), Vec<usize>)> = Vec::new();
        for &w in &lost {
            match island(w).filter(|i| !headed.contains(i)) {
                Some(i) => match stranded.iter_mut().find(|x| x.0 == i) {
                    Some(x) => x.1.push(w),
                    None => stranded.push((i, vec![w])),
                },
                None => dropped.push((wanted[w].0, Dropped::NoHead)),
            }
        }
        let mut edge: Vec<(usize, SupportPoint, V3, Probe)> = Vec::new();
        for (x, ((k, p), ws)) in stranded.iter().enumerate() {
            let (k, p, w) = (*k, *p, ws[0]);
            let island = &cured[k as usize].parts[p as usize];
            for (at, normal, layer) in edge_places(parts, cured, grid, layers, k, p) {
                let spot = SupportPoint { at, normal, kind: PointKind::Island, layer };
                for axis in std::iter::once(leaned(normal, s.lean)).chain(ways.iter().copied()) {
                    let r = tip(w, axis);
                    let centre = add(at, scale(axis, r - s.penetration));
                    if !holds(island, grid, centre, r, layers.z(k)) || !off_others(parts, part[w], centre, r + TIP_OFF) {
                        continue;
                    }
                    if let Some(pr) = head_probe(at, normal, axis, r, s.head_span(r), s, s.gap) {
                        edge.push((x, spot, axis, pr));
                    }
                }
            }
        }
        let verdicts = clearance(parts, &edge.iter().map(|e| e.3).collect::<Vec<_>>());
        for (x, (_, ws)) in stranded.iter().enumerate() {
            let mut clear: Vec<(&SupportPoint, V3, f64)> = edge
                .iter()
                .zip(&verdicts)
                .filter(|(e, _)| e.0 == x)
                .filter_map(|(e, v)| match v {
                    Clear::Clear { least } => Some((&e.1, e.2, *least)),
                    Clear::Blocked { .. } => None,
                })
                .collect();
            clear.sort_by(|a, b| b.2.total_cmp(&a.2));
            let mut taken: Vec<V3> = Vec::new();
            let mut held = ws.iter();
            for (spot, axis, _) in clear {
                if taken.iter().any(|t| norm(sub(*t, spot.at)) < EDGE_APART) {
                    continue;
                }
                let Some(&w) = held.next() else { break };
                taken.push(spot.at);
                heads.push(head(spot, wanted[w].0, part[w], axis, tip(w, axis), s));
            }
            for &w in held {
                dropped.push((wanted[w].0, Dropped::NoHead));
            }
        }
        heads.sort_by_key(|h| h.point);
        // 2. twins: a junction within 0.1 mm of an earlier one is held by it
        let mut kept: Vec<Head> = Vec::with_capacity(heads.len());
        {
            let mut order: Vec<usize> = (0..heads.len()).collect();
            order.sort_by(|&a, &b| heads[a].junction[0].total_cmp(&heads[b].junction[0]));
            let mut twin = vec![false; heads.len()];
            for (k, &a) in order.iter().enumerate() {
                if twin[a] {
                    continue;
                }
                for &b in &order[k + 1..] {
                    if heads[b].junction[0] - heads[a].junction[0] > 0.1 {
                        break;
                    }
                    if !twin[b] && norm(sub(heads[b].junction, heads[a].junction)) <= 0.1 {
                        twin[b] = true;
                    }
                }
            }
            for (h, t) in heads.iter().zip(&twin) {
                if *t {
                    dropped.push((h.point, Dropped::Twin));
                } else {
                    kept.push(*h);
                }
            }
        }
        let mut heads = kept;
        // 3. which heads drop clear to the pad
        let foot = |j: V3| s.foot(j);
        let drops: Vec<Probe> = heads.iter().map(|h| Probe::even(h.junction, foot(h.junction), s.pillar_radius, s.gap)).collect();
        let clear: Vec<bool> = clearance(parts, &drops).iter().map(Clear::is_clear).collect();
        // 4. clusters: heads near each other (within the plan reach of a member and the reach at
        // all), grown member by member up to the pillar's count of heads, its own among them; the
        // pillar under the cluster's medoid, the others bridge to it (R7 §3.3)
        let mut pillars: Vec<Pillar> = Vec::new();
        let mut bridges: Vec<Bridge> = Vec::new();
        let mut pillar_of: Vec<Option<usize>> = vec![None; heads.len()];
        let plan = |a: V3, b: V3| (a[0] - b[0]).hypot(a[1] - b[1]);
        let mut order: Vec<usize> = (0..heads.len()).filter(|&h| clear[h]).collect();
        order.sort_by(|&a, &b| {
            let (p, q) = (heads[a].junction, heads[b].junction);
            p[0].total_cmp(&q[0]).then(p[1].total_cmp(&q[1]))
        });
        let mut taken = vec![false; heads.len()];
        let mut tries: Vec<(usize, usize, V3)> = Vec::new();
        for &h in &order {
            if taken[h] {
                continue;
            }
            taken[h] = true;
            let mut group: Vec<usize> = vec![h];
            let mut grown = 0;
            while grown < group.len() && group.len() < s.bridges {
                let j = heads[group[grown]].junction;
                let mut near: Vec<(f64, usize)> = order
                    .iter()
                    .filter(|&&o| !taken[o])
                    .map(|&o| (plan(heads[o].junction, j), o))
                    .filter(|&(d, o)| d <= s.share_plan && norm(sub(heads[o].junction, j)) <= s.share_reach)
                    .collect();
                near.sort_by(|a, b| a.0.total_cmp(&b.0));
                for (_, o) in near.into_iter().take(s.bridges - group.len()) {
                    taken[o] = true;
                    group.push(o);
                }
                grown += 1;
            }
            // the medoid: least plan distance to the others in all (of two, the first)
            let spread = |a: usize| group.iter().map(|&g| plan(heads[g].junction, heads[a].junction)).sum::<f64>();
            let centre = *group.iter().min_by(|&&a, &&b| spread(a).total_cmp(&spread(b))).expect("a group has its head");
            group.retain(|&g| g != centre);
            let k = pillars.len();
            pillars.push(Pillar { top: heads[centre].junction, heads: vec![centre], radius: s.pillar_radius });
            pillar_of[centre] = Some(k);
            taken[centre] = true;
            for g in group {
                taken[g] = true;
                if let Some(at) = landing(heads[g].junction, pillars[k].top, s) {
                    tries.push((g, k, at));
                }
            }
        }
        let verdicts = clearance(parts, &tries.iter().map(|&(g, _, at)| Probe::even(heads[g].junction, at, s.pillar_radius, s.gap)).collect::<Vec<_>>());
        for (&(g, k, at), v) in tries.iter().zip(&verdicts) {
            if v.is_clear() {
                pillars[k].heads.push(g);
                pillar_of[g] = Some(k);
                bridges.push(Bridge { head: g, pillar: k, at, radius: s.pillar_radius });
            }
        }
        // a grouped head whose bridge failed bridges to the nearest other pillar that takes it,
        // else stands on its own
        let failed: Vec<usize> = order.iter().copied().filter(|&h| pillar_of[h].is_none()).collect();
        for (&h, route) in failed.iter().zip(bridge_nearest(&failed, &heads, &mut pillars, parts, s)) {
            match route {
                Some((k, at)) => {
                    pillar_of[h] = Some(k);
                    bridges.push(Bridge { head: h, pillar: k, at, radius: s.pillar_radius });
                }
                None => {
                    pillar_of[h] = Some(pillars.len());
                    pillars.push(Pillar { top: heads[h].junction, heads: vec![h], radius: s.pillar_radius });
                }
            }
        }
        // 5. heads with no clear drop bridge to the nearest pillars that take them
        let blocked: Vec<usize> = (0..heads.len()).filter(|&h| !clear[h]).collect();
        for (&h, route) in blocked.iter().zip(bridge_nearest(&blocked, &heads, &mut pillars, parts, s)) {
            match route {
                Some((k, at)) => {
                    pillar_of[h] = Some(k);
                    bridges.push(Bridge { head: h, pillar: k, at, radius: s.pillar_radius });
                }
                None => dropped.push((heads[h].point, Dropped::NoRoute)),
            }
        }
        // 6. the load now the routing has dropped what it couldn't hold (its area falls on the
        // heads that hold): each tip that now carries more grown, its junction kept (its gap to
        // the back ball the less); each pillar as wide as what hangs on it and its height
        // standing unbraced need, else its load split; each bridge as wide as its head's share
        // needs; each kept at its size only where it still clears the parts (else as it was, and
        // the rules say it is too thin)
        let held: Vec<usize> = (0..heads.len()).filter(|&h| pillar_of[h].is_some()).collect();
        let shares = head_shares(&bodies, cured, grid, &heads, &held);
        let grown: Vec<(usize, f64, (f64, f64), Probe)> = held
            .iter()
            .filter_map(|&h| {
                let head = &heads[h];
                let r = 0.5 * load.tip_for(shares[h].0, head.lean()).max(least).min(load.tip_max);
                let span = (r - s.penetration, head.span().1);
                let room = span.1 - span.0 - r - s.pillar_radius;
                let fits = r > head.tip_radius && room >= 0.0;
                fits.then(|| head_probe(head.at, head.normal, head.axis, r, span, s, s.gap).map(|p| (h, r, span, p))).flatten()
            })
            .collect();
        for (&(h, r, span, _), v) in grown.iter().zip(clearance(parts, &grown.iter().map(|g| g.3).collect::<Vec<_>>())) {
            if v.is_clear() {
                heads[h].tip = add(heads[h].at, scale(heads[h].axis, span.0));
                heads[h].tip_radius = r;
            }
        }
        // a pillar that can't be as wide as its load needs (the width meets a part, or passes
        // its base's) splits the load: each round it passes on the heaviest head bridged to it
        // that it hasn't yet, onto a pillar of the head's own where its drop is clear, else to
        // the nearest other pillar that takes it and still clears at the width it then needs,
        // until what it keeps fits or nothing takes more
        let hang = |p: &Pillar| p.heads.iter().map(|&h| shares[h].0).sum::<f64>();
        let width = |p: &Pillar, hang: f64| 0.5 * load.pillar_for(hang, p.top[2] - s.foot(p.top)[2]);
        let mut passed: Vec<(usize, usize)> = Vec::new();
        loop {
            let wider: Vec<(usize, f64)> = pillars.iter().enumerate().map(|(k, p)| (k, width(p, hang(p)))).filter(|&(_, r)| r > s.pillar_radius).collect();
            let fits = clearance(parts, &wider.iter().map(|&(k, r)| Probe::even(pillars[k].top, s.foot(pillars[k].top), r.min(s.base_radius), s.gap)).collect::<Vec<_>>());
            let mut short = vec![false; pillars.len()];
            for p in pillars.iter_mut() {
                p.radius = s.pillar_radius;
            }
            for (&(k, r), v) in wider.iter().zip(&fits) {
                if v.is_clear() {
                    pillars[k].radius = r.min(s.base_radius);
                }
                short[k] = !v.is_clear() || r > s.base_radius;
            }
            let passing: Vec<(usize, usize)> = (0..pillars.len())
                .filter(|&k| short[k])
                .filter_map(|k| pillars[k].heads[1..].iter().filter(|&&h| !passed.contains(&(h, k))).max_by(|&&a, &&b| shares[a].0.total_cmp(&shares[b].0)).map(|&h| (h, k)))
                .collect();
            if passing.is_empty() {
                break;
            }
            passed.extend(&passing);
            // the bridged heads' ways: the four nearest other pillars that take another, each
            // its bridge and the pillar at the width it would need
            let plan = |a: V3, b: V3| (a[0] - b[0]).hypot(a[1] - b[1]);
            let mut tries: Vec<(usize, usize, usize, V3)> = Vec::new();
            for (i, &(h, from)) in passing.iter().enumerate() {
                if clear[h] {
                    continue;
                }
                let j = heads[h].junction;
                let mut near: Vec<(f64, usize)> = (0..pillars.len())
                    .filter(|&k| k != from && !short[k] && pillars[k].heads.len() <= s.bridges && !passed.contains(&(h, k)))
                    .map(|k| (plan(pillars[k].top, j), k))
                    .filter(|&(d, k)| d <= s.share_reach && norm(sub(pillars[k].top, j)) <= s.share_reach && width(&pillars[k], hang(&pillars[k]) + shares[h].0) <= s.base_radius)
                    .collect();
                near.sort_by(|a, b| a.0.total_cmp(&b.0));
                for &(_, k) in near.iter().take(4) {
                    if let Some(at) = landing(j, pillars[k].top, s) {
                        tries.push((i, h, k, at));
                    }
                }
            }
            let probes: Vec<Probe> = tries
                .iter()
                .flat_map(|&(_, h, k, at)| {
                    let p = &pillars[k];
                    [Probe::even(heads[h].junction, at, s.pillar_radius, s.gap), Probe::even(p.top, s.foot(p.top), width(p, hang(p) + shares[h].0).max(s.pillar_radius), s.gap)]
                })
                .collect();
            let verdicts = clearance(parts, &probes);
            // a pillar takes one passed head a round: its width was proven for that one alone
            let mut took = vec![false; pillars.len()];
            for (i, &(h, from)) in passing.iter().enumerate() {
                let b = bridges.iter().position(|b| b.head == h).expect("a bridged head has its bridge");
                if clear[h] {
                    pillars[from].heads.retain(|&g| g != h);
                    bridges.remove(b);
                    pillars.push(Pillar { top: heads[h].junction, heads: vec![h], radius: s.pillar_radius });
                    continue;
                }
                let Some(&(_, _, k, at)) = tries.iter().zip(verdicts.chunks(2)).find(|((t, _, k, _), v)| *t == i && !took[*k] && v.iter().all(Clear::is_clear)).map(|(t, _)| t) else {
                    continue;
                };
                took[k] = true;
                pillars[from].heads.retain(|&g| g != h);
                pillars[k].heads.push(h);
                bridges[b] = Bridge { head: h, pillar: k, at, radius: s.pillar_radius };
            }
        }
        let hangs: Vec<f64> = pillars.iter().map(hang).collect();
        let wider: Vec<(usize, f64)> = bridges.iter().enumerate().map(|(b, x)| (b, 0.5 * load.bridge_for(shares[x.head].0, x.rise(&heads)))).filter(|&(_, r)| r > s.pillar_radius).collect();
        let fits = clearance(parts, &wider.iter().map(|&(b, r)| Probe::even(heads[bridges[b].head].junction, bridges[b].at, r, s.gap)).collect::<Vec<_>>());
        for (&(b, r), v) in wider.iter().zip(&fits) {
            if v.is_clear() {
                bridges[b].radius = r;
            }
        }
        // 7. the solids, and each part's pads under its pillars' bases
        let mut solids = Vec::new();
        for &h in &held {
            let h = &heads[h];
            solids.push(Solid::Round { a: h.tip, ra: h.tip_radius, b: h.junction, rb: s.pillar_radius });
        }
        for p in &pillars {
            let top = foot(p.top);
            solids.push(Solid::Round { a: p.top, ra: p.radius, b: top, rb: p.radius });
            solids.push(Solid::Cut { a: [p.top[0], p.top[1], s.ground()], ra: s.base_radius, b: top, rb: p.radius });
        }
        for b in &bridges {
            solids.push(Solid::Round { a: heads[b.head].junction, ra: b.radius, b: b.at, rb: b.radius });
        }
        let mut feet: std::collections::BTreeMap<usize, Vec<([f64; 2], f64)>> = std::collections::BTreeMap::new();
        for (p, &hang) in pillars.iter().zip(&hangs) {
            feet.entry(heads[p.heads[0]].part).or_default().push(([p.top[0], p.top[1]], hang));
        }
        let pads: Vec<Pad> = feet.values().flat_map(|f| pads(f, s, load, grid)).collect();
        dropped.sort_by_key(|d| d.0);
        Supports { settings: *s, load: *load, heads, pillars, bridges, points: points.to_vec(), dropped, solids, pads }
    }

    /// The heads that stand on something.
    pub fn held(&self) -> usize {
        self.pillars.iter().map(|p| p.heads.len()).sum()
    }

    /// Every solid and pad as one mesh.
    pub fn mesh(&self) -> PartMesh {
        let mut m = PartMesh { positions: Vec::new(), triangles: Vec::new() };
        for s in &self.solids {
            s.mesh(16, &mut m);
        }
        for p in &self.pads {
            p.mesh(&mut m);
        }
        m
    }

    /// Their box (printer mm), or none.
    pub fn bounds(&self) -> Option<Box3> {
        let boxes = self.solids.iter().map(Solid::bounds).chain(self.pads.iter().map(Pad::bounds));
        boxes.reduce(|a, b| (std::array::from_fn(|k| a.0[k].min(b.0[k])), std::array::from_fn(|k| a.1[k].max(b.1[k]))))
    }
}

/// `n` (unit) leaned to no more than `lean` from straight down, its way round kept.
fn leaned(n: V3, lean: f64) -> V3 {
    let down = -n[2];
    if down >= lean.cos() {
        return n;
    }
    let h = n[0].hypot(n[1]);
    if h < 1e-12 {
        return [0.0, 0.0, -1.0];
    }
    [n[0] / h * lean.sin(), n[1] / h * lean.sin(), -lean.cos()]
}

/// The ways a head may turn: straight down, and each of [`LEANS`] all the way round.
fn ways_down(lean: f64) -> Vec<V3> {
    let mut ways = vec![[0.0, 0.0, -1.0]];
    for &d in LEANS.iter().skip(1) {
        let a = d.to_radians().min(lean);
        for k in 0..TURNS {
            let t = TAU * k as f64 / TURNS as f64;
            ways.push([a.sin() * t.cos(), a.sin() * t.sin(), -a.cos()]);
        }
    }
    ways
}

/// Places round island `p` of layer `k`'s outline and its own outline on the layers up to
/// [`EDGE_LAYERS`] over it, on the parts' surface, with the surface's normal there and their layer:
/// at most [`EDGE_PLACES`] a layer, spread round it.
fn edge_places(parts: &[Arc<dyn Section>], cured: &[LayerParts], grid: &PixelGrid, layers: &Layers, k: u32, p: u32) -> Vec<(V3, V3, u32)> {
    let mut start: Vec<(V3, u32)> = Vec::new();
    let mut ids = vec![p];
    for layer in k..(k + EDGE_LAYERS + 1).min(cured.len() as u32) {
        let z = layers.z(layer);
        for &id in &ids {
            let part = &cured[layer as usize].parts[id as usize];
            // the pixels at either end of each run, round the part's middle
            let ends: Vec<[f64; 2]> = part.runs.iter().flat_map(|r| [grid.center(r.x0, r.y), grid.center(r.x1 - 1, r.y)]).collect();
            let mid = ends.iter().fold([0.0, 0.0], |m, e| [m[0] + e[0] / ends.len() as f64, m[1] + e[1] / ends.len() as f64]);
            let mut round: Vec<(f64, [f64; 2])> = ends.iter().map(|e| ((e[1] - mid[1]).atan2(e[0] - mid[0]), *e)).collect();
            round.sort_by(|a, b| a.0.total_cmp(&b.0));
            let step = round.len().div_ceil(EDGE_PLACES).max(1);
            start.extend(round.iter().step_by(step).map(|(_, e)| ([e[0], e[1], z], layer)));
        }
        if layer + 1 < cured.len() as u32 {
            let on = links(&cured[layer as usize], &cured[layer as usize + 1]);
            ids = (0..on.len() as u32).filter(|&a| on[a as usize].iter().any(|b| ids.contains(b))).collect();
        }
    }
    // onto the nearest place on the surface
    let mut q: Vec<V3> = start.iter().map(|s| s.0).collect();
    for _ in 0..8 {
        let near = nearest(parts, &q);
        for (q, n) in q.iter_mut().zip(near) {
            for a in 0..3 {
                q[a] -= n.distance * n.normal[a];
            }
        }
    }
    let h = layers.height;
    q.iter().zip(nearest(parts, &q)).zip(&start).filter(|((q, n), s)| n.distance.abs() < 0.01 && (q[2] - s.0[2]).abs() <= h).map(|((q, n), s)| (*q, n.normal, s.1)).collect()
}

/// Whether a tip ball at `centre`, `r` round, cures into `island`'s pixels on its layer at `z`:
/// its section there is two pixels across at least and covers one of them more than half.
fn holds(island: &Part, grid: &PixelGrid, centre: V3, r: f64, z: f64) -> bool {
    let [px, py] = grid.pitch;
    let across = r * r - (centre[2] - z).powi(2);
    if across < (px.max(py)).powi(2) {
        return false;
    }
    let reach = across.sqrt() - 0.5 * px.max(py);
    island.runs.iter().any(|run| {
        let dy = (run.y as f64 + 0.5) * py - centre[1];
        if dy.abs() > reach {
            return false;
        }
        let half = (reach * reach - dy * dy).sqrt();
        centre[0] + half >= (run.x0 as f64 + 0.5) * px && centre[0] - half <= (run.x1 as f64 - 0.5) * px
    })
}

/// Whether a ball at `centre` keeps `off` from every part but `own`.
fn off_others(parts: &[Arc<dyn Section>], own: usize, centre: V3, off: f64) -> bool {
    let mut d = [0.0];
    parts.iter().enumerate().filter(|(k, _)| *k != own).all(|(_, p)| {
        if box_distance(&p.bounds(), centre) >= off {
            return true;
        }
        p.clear(&[centre], &[off], &mut d);
        d[0] >= off
    })
}

fn head(p: &SupportPoint, point: usize, part: usize, axis: V3, tip_radius: f64, s: &SupportSettings) -> Head {
    let (tip, junction) = s.head_span(tip_radius);
    Head { point, part, layer: p.layer, at: p.at, normal: p.normal, axis, tip: add(p.at, scale(axis, tip)), tip_radius, junction: add(p.at, scale(axis, junction)) }
}

/// The part a point on the parts' surfaces is on: the one that proves it nearest (none nearer
/// than its box).
fn part_at(parts: &[Arc<dyn Section>], p: V3) -> usize {
    let mut d = [0.0];
    (0..parts.len())
        .map(|k| {
            let far = box_distance(&parts[k].bounds(), p);
            if far > 0.0 {
                return (k, far);
            }
            parts[k].clear(&[p], &[0.0], &mut d);
            (k, d[0])
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(0, |(k, _)| k)
}

/// Each head's share of the peel (mm²) and the layer it takes the most on, for the heads
/// `held` with only them holding (none for the others).
pub(crate) fn head_shares(bodies: &Bodies, cured: &[LayerParts], grid: &PixelGrid, heads: &[Head], held: &[usize]) -> Vec<(f64, u32)> {
    let on: Vec<(usize, (u32, u32))> = held.iter().filter_map(|&h| part_of(cured, grid, heads[h].layer, [heads[h].at[0], heads[h].at[1]]).map(|k| (h, (heads[h].layer, k)))).collect();
    let mut out = vec![(0.0, 0); heads.len()];
    for ((h, _), share) in on.iter().zip(bodies.shares(&on.iter().map(|o| o.1).collect::<Vec<_>>())) {
        out[*h] = share;
    }
    out
}

/// What a head along `axis` from the point `at` (the surface's `normal` there), its tip of
/// `tip_radius` and its `span` (its tip's centre and its junction along the axis), must keep clear:
/// the cone from where it leaves the surface (on the surface's plane at the point) to the
/// junction, its gap growing from nothing there to `gap` at the junction. None where the head
/// runs along the surface.
pub(crate) fn head_probe(at: V3, normal: V3, axis: V3, tip_radius: f64, span: (f64, f64), s: &SupportSettings, gap: f64) -> Option<Probe> {
    let (tip, junction) = span;
    let slope = (s.pillar_radius - tip_radius) / (junction - tip);
    let r = |d: f64| tip_radius + slope * (d - tip);
    let c = dot(axis, normal);
    if c - slope < LEAVES {
        return None;
    }
    // a ball at d along the axis is c d - r(d) off the plane: it leaves at d0, and a little on
    // it is HEAD_OFF off
    let d0 = (tip_radius - slope * tip) / (c - slope) + HEAD_OFF / (c - slope);
    if d0 >= junction {
        return None;
    }
    Some(Probe { a: add(at, scale(axis, d0)), b: add(at, scale(axis, junction)), ra: r(d0), rb: s.pillar_radius, ga: 0.0, gb: gap })
}

/// Bridges for heads `from`, each to the nearest pillar (in plan, trying the four nearest within
/// the reach, outward) whose bridge clears the parts and that takes another; each head's pillar
/// and landing (none: no pillar takes it). The pillars take them in turn.
fn bridge_nearest(from: &[usize], heads: &[Head], pillars: &mut [Pillar], parts: &[Arc<dyn Section>], s: &SupportSettings) -> Vec<Option<(usize, V3)>> {
    let plan = |a: V3, b: V3| (a[0] - b[0]).hypot(a[1] - b[1]);
    let mut tries: Vec<(usize, usize, V3)> = Vec::new();
    for &h in from {
        let j = heads[h].junction;
        let mut near: Vec<(f64, usize)> = pillars.iter().enumerate().map(|(k, p)| (plan(p.top, j), k)).filter(|&(d, k)| d <= s.share_reach && norm(sub(pillars[k].top, j)) <= s.share_reach).collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        for &(_, k) in near.iter().take(4) {
            if let Some(at) = landing(j, pillars[k].top, s) {
                tries.push((h, k, at));
            }
        }
    }
    let verdicts = clearance(parts, &tries.iter().map(|&(h, _, at)| Probe::even(heads[h].junction, at, s.pillar_radius, s.gap)).collect::<Vec<_>>());
    from.iter()
        .map(|&h| {
            let (&(_, k, at), _) = tries.iter().zip(&verdicts).find(|((g, k, _), v)| *g == h && v.is_clear() && pillars[*k].heads.len() <= s.bridges)?;
            pillars[k].heads.push(h);
            Some((k, at))
        })
        .collect()
}

/// Where a bridge from junction `j` lands on the pillar from `top`: no higher than its top,
/// falling at least 45°, and above its base. None where it would land too low.
fn landing(j: V3, top: V3, s: &SupportSettings) -> Option<V3> {
    let d = (j[0] - top[0]).hypot(j[1] - top[1]);
    let z = top[2].min(j[2] - d);
    (z >= s.lowest_landing() && d > 1e-9).then_some([top[0], top[1], z])
}

/// The narrowest join between pad pieces before the closing grows it (mm): the reference's
/// spoke's base.
const JOIN: f64 = 2.0;

/// The pads under one part's `feet` (each a base's centre and the shares hanging on it, mm²;
/// R7 §4.2, ours the leaner join): the bases' discs closed over the gaps between them and grown
/// by the brim, each piece a cluster; the clusters joined along their shortest spanning tree,
/// each join a strip between the nearest two bases of the clusters it joins, as wide as the pad's
/// thickness needs to hold the lighter side's peel (never under [`JOIN`]); then the discs and the
/// strips closed again, whole. A cluster whose nearest is beyond the reach (between their bases'
/// outlines) stands alone. Each outer outline is one pad: a ring of bases stands on a solid pad,
/// and the space between clusters stays open, never a hull of them all. A piece of a pad whose
/// bottom cures no pixel of `grid` stands on nothing: none of it prints, and a pad left with no
/// bottom is none.
pub(crate) fn pads(feet: &[([f64; 2], f64)], s: &SupportSettings, load: &Load, grid: &PixelGrid) -> Vec<Pad> {
    let round = Join::Round(0.5 * PAD_CHORD * SCALE);
    let disc = |c: [f64; 2], r: f64| {
        let n = ((TAU / (2.0 * (1.0 - 0.5 * PAD_CHORD / r).acos())).ceil() as usize).max(8);
        Polygon::new((0..n).map(|k| TAU * k as f64 / n as f64).map(|a| Pt::from_mm(c[0] + r * a.cos(), c[1] + r * a.sin())).collect())
    };
    let close = |rings: &[Polygon]| offset::closing(&clip::union(rings, FillRule::NonZero), 2.0 * s.brim * SCALE, s.brim * SCALE, round);
    let mut rings: Vec<Polygon> = feet.iter().map(|f| disc(f.0, s.base_radius)).collect();
    let clusters: Vec<Polygon> = close(&rings).into_iter().map(|e| e.contour).collect();
    if clusters.len() > 1 {
        let of: Vec<usize> = feet.iter().map(|f| clusters.iter().position(|c| c.contains(Pt::from_mm(f.0[0], f.0[1]))).expect("every base is in a cluster")).collect();
        // the nearest two bases of each two clusters, nearest first
        let mut nearest: std::collections::HashMap<(usize, usize), (f64, usize, usize)> = std::collections::HashMap::new();
        for a in 0..feet.len() {
            for b in a + 1..feet.len() {
                if of[a] != of[b] {
                    let d = (feet[a].0[0] - feet[b].0[0]).hypot(feet[a].0[1] - feet[b].0[1]);
                    let e = nearest.entry((of[a].min(of[b]), of[a].max(of[b]))).or_insert((d, a, b));
                    if d < e.0 {
                        *e = (d, a, b);
                    }
                }
            }
        }
        let mut pairs: Vec<(f64, usize, usize)> = nearest.into_values().collect();
        pairs.sort_by(|x, y| x.0.total_cmp(&y.0).then((x.1, x.2).cmp(&(y.1, y.2))));
        let reach = 2.0 * 1.8 * s.pad + s.pad_join;
        let mut up: Vec<usize> = (0..clusters.len()).collect();
        fn root(up: &mut [usize], mut a: usize) -> usize {
            while up[a] != a {
                up[a] = up[up[a]];
                a = up[a];
            }
            a
        }
        let mut tree: Vec<(usize, usize)> = Vec::new();
        for &(d, a, b) in &pairs {
            let (ra, rb) = (root(&mut up, of[a]), root(&mut up, of[b]));
            if ra != rb && d - 2.0 * s.base_radius <= reach {
                up[ra] = rb;
                tree.push((a, b));
            }
        }
        // what each join holds: the lighter of the two sides it parts
        let mut held = vec![0.0f64; clusters.len()];
        for (f, &c) in feet.iter().zip(&of) {
            held[c] += f.1;
        }
        for (i, &(a, b)) in tree.iter().enumerate() {
            let side = |from: usize, not: usize| {
                let (mut seen, mut stack, mut sum) = (vec![false; clusters.len()], vec![from], 0.0);
                seen[from] = true;
                while let Some(c) = stack.pop() {
                    sum += held[c];
                    for (j, &(x, y)) in tree.iter().enumerate() {
                        let (p, q) = (of[x], of[y]);
                        for (u, v) in [(p, q), (q, p)] {
                            if j != not && u == c && !seen[v] {
                                seen[v] = true;
                                stack.push(v);
                            }
                        }
                    }
                }
                sum
            };
            let lighter = side(of[a], i).min(side(of[b], i));
            let w = (load.section_for(lighter) / s.pad - 2.0 * s.brim).max(JOIN);
            let (p, q) = (feet[a].0, feet[b].0);
            let l = (q[0] - p[0]).hypot(q[1] - p[1]);
            let n = [-(q[1] - p[1]) / l * 0.5 * w, (q[0] - p[0]) / l * 0.5 * w];
            let strip = [[p[0] - n[0], p[1] - n[1]], [q[0] - n[0], q[1] - n[1]], [q[0] + n[0], q[1] + n[1]], [p[0] + n[0], p[1] + n[1]]];
            rings.push(Polygon::new(strip.iter().map(|c| Pt::from_mm(c[0], c[1])).collect()));
        }
    }
    let joined = if rings.len() > feet.len() { close(&rings) } else { clusters.into_iter().map(ExPolygon::new).collect() };
    let inset = s.pad / s.pad_slope.tan();
    joined
        .iter()
        .map(|e| ring_of(&e.contour))
        .filter(|r| r.len() >= 3)
        .map(|r| {
            let mut pad = Pad::new(r, [0.0, s.ground()], inset);
            pad.bottom.retain(|b| cures_on(b, grid));
            pad
        })
        .filter(|p| !p.bottom.is_empty())
        .collect()
}

/// Whether `piece` (bed mm) cures a pixel of `grid` ([`CURES`]).
fn cures_on(piece: &ExPolygon, grid: &PixelGrid) -> bool {
    let rings: Vec<Vec<[f64; 2]>> = piece.rings().map(|r| r.pts.iter().map(|q| [q.x_mm(), q.y_mm()]).collect()).collect();
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for p in rings.iter().flatten() {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let rect = grid.rect_of(lo, hi);
    let mut cov = vec![0u8; rect.pixels()];
    outline_cover(&rings, grid, rect, &mut cov);
    cov.iter().any(|&c| c >= CURES)
}

/// An outline in bed mm, counter-clockwise, its corners within the chord of its line: the
/// offsets' joins leave jogs a few microns long where arcs meet, too sharp for a chamfer to
/// draw in along.
fn ring_of(p: &Polygon) -> Vec<[f64; 2]> {
    let mut pts = p.pts.clone();
    clean_ring(&mut pts);
    let mut r: Vec<[f64; 2]> = simplify_ring(&pts, PAD_CHORD * SCALE).iter().map(|q| [q.x_mm(), q.y_mm()]).collect();
    if !p.is_ccw() {
        r.reverse();
    }
    r
}

/// The `pads` cut at height `z` drawn into the pixels of `rect`, taken into `cov` where more.
pub(crate) fn pad_cover(pads: &[Pad], z: f64, grid: &PixelGrid, rect: PixelRect, cov: &mut [u8]) {
    for pad in pads.iter().filter(|p| z >= p.z[0] && z <= p.z[1]) {
        outline_cover(&pad.cut_at(z), grid, rect, cov);
    }
}

/// Solids registered on cells this far beyond their boxes (mm): a point's distance to a solid
/// not on its cell is at least this plus its distance to the cell's side.
const MARGIN: f64 = 1.0;
/// The cells' size (mm).
const CELL: f64 = 2.0;

/// The supports as a section: the struts' exact distance, read on the cells near a point, and
/// the pads drawn from their outlines.
pub struct SupportSection {
    supports: Arc<Supports>,
    bounds: Box3,
    /// The struts' box: the solids', the pads left out.
    struts: Box3,
    origin: V3,
    cells: [usize; 3],
    /// Per cell, its solids' numbers, as runs in `items`.
    start: Vec<u32>,
    items: Vec<u32>,
}

impl SupportSection {
    pub fn new(supports: Arc<Supports>) -> Option<SupportSection> {
        let bounds = supports.bounds()?;
        let boxes: Vec<Box3> = supports.solids.iter().map(Solid::bounds).collect();
        let struts = boxes.iter().copied().reduce(|a, b| (std::array::from_fn(|k| a.0[k].min(b.0[k])), std::array::from_fn(|k| a.1[k].max(b.1[k]))))?;
        let origin = struts.0.map(|v| v - MARGIN);
        let cells: [usize; 3] = std::array::from_fn(|k| (((struts.1[k] + MARGIN - origin[k]) / CELL).ceil() as usize).max(1));
        let span = |b: &Box3, k: usize| {
            let lo = (((b.0[k] - MARGIN - origin[k]) / CELL).floor().max(0.0) as usize).min(cells[k] - 1);
            let hi = (((b.1[k] + MARGIN - origin[k]) / CELL).floor().max(0.0) as usize).min(cells[k] - 1);
            lo..=hi
        };
        let at = |x: usize, y: usize, z: usize| (z * cells[1] + y) * cells[0] + x;
        let mut count = vec![0u32; cells[0] * cells[1] * cells[2] + 1];
        for b in &boxes {
            for z in span(b, 2) {
                for y in span(b, 1) {
                    for x in span(b, 0) {
                        count[at(x, y, z) + 1] += 1;
                    }
                }
            }
        }
        for i in 1..count.len() {
            count[i] += count[i - 1];
        }
        let mut fill = count.clone();
        let mut items = vec![0u32; *count.last().unwrap() as usize];
        for (i, b) in boxes.iter().enumerate() {
            for z in span(b, 2) {
                for y in span(b, 1) {
                    for x in span(b, 0) {
                        let c = at(x, y, z);
                        items[fill[c] as usize] = i as u32;
                        fill[c] += 1;
                    }
                }
            }
        }
        Some(SupportSection { supports, bounds, struts, origin, cells, start: count, items })
    }

    /// The supports' distance at `p`: the struts' and the pads' least.
    pub fn distance(&self, p: V3) -> f64 {
        self.supports.pads.iter().map(|pad| pad.distance(p)).fold(self.strut_distance(p), f64::min)
    }

    /// The struts' distance at `p`: exact near them, a proven lower bound farther off.
    fn strut_distance(&self, p: V3) -> f64 {
        let mut c = [0usize; 3];
        let mut side = f64::INFINITY;
        for k in 0..3 {
            let u = (p[k] - self.origin[k]) / CELL;
            if u < 0.0 || u >= self.cells[k] as f64 {
                // outside the cells: every strut lies within their box
                let b = &self.struts;
                let d: V3 = std::array::from_fn(|k| (b.0[k] - p[k]).max(p[k] - b.1[k]).max(0.0));
                return norm(d);
            }
            c[k] = u as usize;
            let f = u - c[k] as f64;
            side = side.min(f.min(1.0 - f) * CELL);
        }
        let cell = (c[2] * self.cells[1] + c[1]) * self.cells[0] + c[0];
        let mut d = MARGIN + side;
        for &i in &self.items[self.start[cell] as usize..self.start[cell + 1] as usize] {
            d = d.min(self.supports.solids[i as usize].distance(p));
        }
        d
    }
}

/// The struts as a field; the pads draw from their outlines.
impl Field for SupportSection {
    fn field_bounds(&self) -> Box3 {
        self.struts
    }

    fn lipschitz(&self) -> f64 {
        1.0
    }

    fn read(&self, grid: &PixelGrid, z: f64, uv: &[[f32; 2]], value: &mut Vec<f32>, outside: &mut Vec<f32>, inside: &mut Vec<f32>) {
        value.clear();
        value.extend(uv.iter().map(|q| {
            let b = grid.at(q[0] as f64, q[1] as f64);
            self.strut_distance([b[0], b[1], z]) as f32
        }));
        outside.clear();
        outside.extend(value.iter().map(|v| v.max(0.0)));
        inside.clear();
        inside.extend(value.iter().map(|v| (-v).max(0.0)));
    }
}

impl Section for SupportSection {
    fn bounds(&self) -> Box3 {
        self.bounds
    }

    fn cover(&self, z: f64, grid: &PixelGrid, rect: PixelRect, cov: &mut [u8]) {
        field_cover(self, z, grid, rect, cov);
        pad_cover(&self.supports.pads, z, grid, rect, cov);
    }

    fn near(&self, p: &[V3], out: &mut [Near]) {
        const H: f64 = 1e-4;
        for (q, o) in p.iter().zip(out.iter_mut()) {
            let d = self.distance(*q);
            let g: V3 = std::array::from_fn(|k| {
                let (mut a, mut b) = (*q, *q);
                a[k] += H;
                b[k] -= H;
                (self.distance(a) - self.distance(b)) / (2.0 * H)
            });
            let l = norm(g);
            *o = Near { distance: d, normal: if l > 0.0 { scale(g, 1.0 / l) } else { [0.0, 0.0, 1.0] } };
        }
    }

    fn clear(&self, p: &[V3], _need: &[f64], out: &mut [f64]) {
        for (q, o) in p.iter().zip(out.iter_mut()) {
            *o = self.distance(*q);
        }
    }
}

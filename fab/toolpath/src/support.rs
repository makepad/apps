//! Organic tree supports, v1 (R5 §10.1). Prints plan their tree supports with R14's
//! ([`crate::tree`]); this generator is left only for the tests that still call it, and goes with
//! them.
//!
//! 1. Demand: per layer, the part of the slice the layer below cannot carry
//!    at the threshold angle; slivers and specks dropped, unless nothing of
//!    the layer below is under them at all.
//! 2. Roof: the demand area itself, `interface_top_layers` thick, one top
//!    gap below the overhang. Demand islands too narrow for roof lines (the
//!    strips under a shallow slope) get no roof: thin bare tips start right
//!    under the top gap instead.
//! 3. Branches: tips sampled over the demand area descend layer by layer.
//!    Each node drifts toward its neighbours at the preferred angle, merges with
//!    nodes it overlaps, and grows from its tip radius to the branch diameter and
//!    then by the taper angle, up to a trunk limit. It meets the part only at its
//!    tip. Nothing comes nearer the part than xy_min ([`SupportParams::xy_min`], R5): a tip
//!    starts with the room it has there, and from it the distance a branch keeps grows at the
//!    steep angle to the side gap, then to [`BRANCH_GAP`] (pushed out along the nearest
//!    boundary), so it leaves the surface instead of riding along it. The side gap is where a
//!    branch keeps to when it has room, not a keep. A section stays a circle and does not narrow going down for want
//!    of room: it grows into the room it has, and where the part comes closer than that,
//!    the clearance cuts its side. Not with its centre in the clearance, nor with one of
//!    the part's pores or slots in its reach: cut there it would fill them and lock its
//!    piece in, so there it narrows to the room. A node the model outruns stops moving and drops straight
//!    down until the model covers it, onto the surface. Cut to its room it would fill
//!    the part's pores all the way down, so from the bed only it narrows to its room at
//!    once (and ends where it has none), onto the part at the foot's slope.
//!
//!    Under a roof there are no tips (see [`tapers`]): the support's top is what the roof
//!    covers, and each layer under it is the one above shrunk by the steep angle, clear of the
//!    part, until it fits in a trunk, where one trunk starts round all of it (a strip that would
//!    taper away first gets trunks along it). The taper prints solid, so the roof lies on
//!    support all over; only what has no roof gets tips.
//!
//!    A merge changes the tree, not the outline: the two sections go on drawing,
//!    moving onto the merged branch until they lie inside it, and the branch's
//!    own section grows from inside them. Whatever moves it, a section's outline
//!    moves at most the steep angle per layer, so consecutive layers overlap by
//!    most of a bead. Where the model approaches faster than that, the section
//!    narrows for as long as it takes: there the outline follows the part.
//! 4. Output per layer: base = the union of the sections and pads, closed so what meets is
//!    blended, minus what keeps support off ([`crate::support_rules::Legal`]) minus the roof;
//!    roof as interface.
//!    What has no room for a wall is opened away, which also bevels the corners
//!    the cuts leave. Trunks standing on the bed flare into a foot
//!    over the first layers.
//! 5. Inside the part: a branch only goes where it gets down to its footing from, a layer at a
//!    time at the steep angle round what it keeps off (see [`Fields`]): to the bed, or not from
//!    the bed only to a part top it may stand on. A trunk stands only where one that wide gets
//!    down, and a branch going down moves only where its way down makes it, to the nearest place
//!    that has one and by no more than the steep angle, narrowing where only a narrower one gets
//!    down. So it goes into the part's hollows and lattice cells through any window it fits, and
//!    the overhang no branch gets to, not even the thinnest, is what the part closes in: reported
//!    as that.
//! 6. What stands: going up from the bed, a section or island stays only if what
//!    prints below holds it, or (unless the plate says from the bed only) the part
//!    is right under it, and a roof only over its roof below or the support below (within
//!    [`CARRIED`] of a line). So a branch that can't get down is gone from tip to root,
//!    with the roof it held, and what it would have held is reported as given up.
//!    Going down from the top, an island stays only if it meets an overhang within
//!    the top gap or carries one that does, so nothing prints that holds nothing;
//!    the skeleton keeps only the sections that print.
//!
//! All steps run in a fixed order over sorted nodes, so the result is
//! deterministic.

use makepad_fab_slicer::clip::{difference_ex, intersection_ex, union, union_ex, FillRule};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::grid::SegGrid;
use makepad_fab_slicer::offset::{offset_ex, Join};
use std::sync::Arc;

use crate::coral::reach::Reach;
use crate::skeleton::{GivenUp, Skeleton};
use crate::support_paint::Masks;
use crate::support_rules::Legal;

#[derive(Clone, Debug, PartialEq)]
pub struct SupportParams {
    pub threshold_deg: f64,
    pub xy_gap: f64,
    pub top_gap: f64,
    pub bottom_gap: f64,
    pub interface_top_layers: usize,
    pub line_width: f64,
    /// The part's outer wall line width (mm): no support comes nearer the part's side than half
    /// of it (see [`SupportParams::xy_min`]).
    pub wall_width: f64,
    pub tip_diameter: f64,
    pub branch_diameter: f64,
    pub taper_deg: f64,
    pub angle_max_deg: f64,
    pub angle_slow_deg: f64,
    pub build_plate_only: bool,
    pub remove_small: bool,
    /// The first layer's support grows outward by this much (mm) for bed
    /// adhesion.
    pub first_expansion: f64,
    /// Branches may stand inside the part: in its hollows, pores and lattice cells.
    pub stand_inside: bool,
}

impl SupportParams {
    /// xy_min (mm), the nearest support comes to the part's side (R5): the side gap, or half the
    /// outer wall's width where that is less; with no top gap at least [`XY_MIN_SOLUBLE`], so the
    /// keep's offsets cannot tunnel through a wall. The side gap itself is where a branch keeps
    /// to when it has room.
    pub fn xy_min(&self) -> f64 {
        let f = self.xy_gap.min(self.wall_width / 2.0).max(0.0);
        if self.top_gap > 0.0 { f } else { f.max(XY_MIN_SOLUBLE) }
    }

    /// The radius (mm) of the thinnest branch that goes down: [`COLUMN_WIDTHS`] lines across,
    /// and no thinner than a bare tip.
    pub fn column(&self) -> f64 {
        (self.tip_diameter / 2.0).max(0.05).max(COLUMN_WIDTHS * self.line_width / 2.0)
    }
}

/// The least xy_min (mm) with no top gap, as under a soluble interface.
const XY_MIN_SOLUBLE: f64 = 0.1;

#[derive(Clone, Debug, Default)]
pub struct SupportLayer {
    pub base: Vec<ExPolygon>,
    pub interface: Vec<ExPolygon>,
    /// What of the base prints solid, not as a sheath round it: the support's top, the layer
    /// under it, and whatever a roof lies on.
    pub solid: Vec<ExPolygon>,
    /// What of the base is the inside of the support under its top, filled sparse.
    pub inside: Vec<ExPolygon>,
}

/// Trunks stop widening at this radius (mm): in reference output, trunk
/// sections level off at about 4.9 mm across however tall the tree is.
const TRUNK_MAX_R: f64 = 2.45;
/// Bed-standing trunks flare into a foot of this radius (mm) on the second
/// layer, narrowing by `FOOT_SLOPE` mm per mm of height above it and never
/// more than `FOOT_MAX_EXTRA` beyond the trunk (reference feet: 7.0 mm
/// across, flaring from about 1.7 mm up).
const FOOT_R: f64 = 3.5;
const FOOT_SLOPE: f64 = 0.8;
const FOOT_MAX_EXTRA: f64 = 1.2;
/// Overlapping branches merge once their centres are within this share of their
/// radii's sum (fitted against reference output); further apart they only touch
/// and stand side by side.
const JOIN: f64 = 0.4;
/// How much of its new heading a branch takes per layer: a change of direction
/// spreads over a few layers, so the centre follows a curve.
const TURN: f64 = 0.3;
/// A section this far (mm) past the edge of another still counts as inside it:
/// under the G-code's resolution, so taking it away moves no bead.
const INSIDE: f64 = 0.01;
/// The chord deviation of drawn curves (mm): within the G-code's resolution, so
/// the writer's arc fitting turns them back into arcs.
const CURVE_TOL: f64 = 0.008;
/// The chord deviation of the curves a closing draws (mm): on these small radii,
/// fine enough that no chord turns visibly.
const BLEND_TOL: f64 = 0.001;
/// Outlines are closed by this many line widths: where sections meet the outline
/// bends no tighter than that radius, and a pocket that cannot hold its circle
/// is gone, so no wall loop closes round a blob.
const CLOSE_WIDTHS: f64 = 1.0;
/// Sections blend only if that leaves a neck this many line widths across: under
/// it the two beads of its sides would overlap by more than a quarter.
const NECK_WIDTHS: f64 = 1.5;
/// What the cuts leave is opened by this many line widths: anything narrower has no
/// room for its wall, and the margin over half keeps the wall's two sides apart at
/// a neck, over the resolution of the G-code.
const OPEN_WIDTHS: f64 = 0.55;
/// What erosion leaves under this area (mm²) gets no wall worth printing: a loop this
/// small is a hairpin, a thin tip's is well over it.
const MIN_CORE: f64 = 0.05;
/// A left turn sharper than this (radians) at one vertex of an outline is a corner:
/// something cut it. Drawn curves turn less.
const CORNER_TURN: f64 = 0.5;
/// Below this area (mm²) an island is a speck: the printability checks skip it too.
pub const SPECK: f64 = 0.05;

/// Where the support pass is, as a share of it, when each of its steps begins: what it tells as it
/// goes (see [`crate::par::Ctl::phase`]). Measured on three lattice cubes (27, 25 and 29 mm at
/// 1.6x, from the bed, 45 deg, a 2 mm side gap), over three runs.
const AT_REACH: f64 = 0.02;
const AT_ROOFS: f64 = 0.07;
const AT_TREES: f64 = 0.08;
const AT_PADS: f64 = 0.19;
const AT_DOWN: f64 = 0.23;
const AT_DRAWING: f64 = 0.24;
const AT_CHECK: f64 = 0.25;
const AT_CHECK_UP: f64 = 0.26;
const AT_IDLE: f64 = 0.52;
const AT_TRIM: f64 = 0.53;
const AT_TRIM_UP: f64 = 0.76;
const AT_TELL: f64 = 0.82;
/// How far past the side gap (mm) an enforced face's narrow strip reaches: room for a tip.
const TIP_ROOM: f64 = 0.05;
/// How many layers under its place a tip can start and still reach what it is under, by the opening
/// that takes away a section with no room for a wall.
const TIP_CLIPPED: usize = 3;
/// Layers past the top gap that a support top may stop short of what it holds and still count as
/// touching it: the rounding of the gap to whole layers.
pub(crate) const TOUCH_SLACK: usize = 1;
/// How far past the bottom gap (mm) a section may end above the part and still count as
/// resting on it: a tip gap's way off a steep slope. Further up it hangs.
pub(crate) const LAND_AIR: f64 = 0.6;
/// A gap in a layer's outline up to this wide (mm) is inside the part, the same as a printer's
/// default bridge limit: a lattice's cells close, the room under an arch wider than this stays
/// open. A pore wider than this lets a trunk in and out sideways.
pub(crate) const INSIDE_GAP: f64 = 10.0;
/// What closing a layer adds thinner than twice this (mm) is its own rounding, not something the
/// part encloses.
const INSIDE_SLIVER: f64 = 0.1;
/// How far round a piece of the part (degrees) a roof may reach and still come off it: past half
/// a turn it holds on, and its ends must not graze it.
const WRAP_MOST: f64 = 200.0;
/// The walls that cut such a roof, this many line widths thick: more than the blend between two
/// branches, so what holds each arc stays apart from the next.
pub(crate) const WALL_WIDTHS: f64 = 3.0;
/// The arcs such a roof prints in, each a quarter turn: with three, an arc under a vase's lip
/// still came out but left no straight way in for pliers.
const RING_ARCS: usize = 4;
/// A tip's gap (mm): how far beside part underside a support's top may stand and still hold it,
/// as under a slope (R4, see [`crate::support_rules::ASIDE`]). It keeps nothing off: supports come
/// to xy_min ([`SupportParams::xy_min`]).
pub(crate) const TIP_GAP: f64 = 0.35;
/// What a branch keeps off the part beside it once it has left its tip (mm), unless the side gap
/// asks for more: further off than it can fuse to the part or scar it where it comes away.
const BRANCH_GAP: f64 = 1.5;
/// A section with less than this share of what it prints over the layer below hangs off it.
const HELD_SHARE: f64 = 0.5;

#[derive(Clone, Copy, Debug)]
struct Node {
    /// Its slot in the record of what merged into what.
    id: usize,
    x: f64,
    y: f64,
    r: f64,
    /// The radius it started from (bare tip or roof tip).
    r0: f64,
    /// Layers below its tip.
    age: usize,
    alive: bool,
    /// The model outran it: falls straight down without moving or merging
    /// until the model's clearance covers it.
    dropped: bool,
    /// What it keeps off the clearance (mm): nothing at its tip, then growing.
    off: f64,
    /// How far it was into the clearance, a layer up.
    was_into: f64,
    /// Its heading: the smoothed move per layer (mm) its exit and drift ask for.
    vx: f64,
    vy: f64,
    /// Its centre and radius a layer up.
    was: (f64, f64, f64),
    /// Pressed into the clearance by more than a layer's move: it does not grow
    /// until it has room.
    pressed: bool,
    /// The section it draws: its radius, flared at the foot.
    drawn: f64,
    /// The stretch of the skeleton it is drawing.
    chain: usize,
    /// Its section is clear of what keeps support off, so all of it prints.
    whole: bool,
}

/// A section still drawing after its branch merged: it moves onto the merged
/// branch and is gone once it lies inside that branch's section.
#[derive(Clone, Copy, Debug)]
struct Joining {
    x: f64,
    y: f64,
    r: f64,
    /// The id of the node it joins.
    into: usize,
    /// The stretch of the skeleton it goes on drawing.
    chain: usize,
}

/// A section as a layer draws it.
#[derive(Clone, Copy, Debug)]
struct Drawn {
    /// (x, y, r) in mm.
    s: (f64, f64, f64),
    /// The stretch of the skeleton it is on.
    chain: usize,
}

/// Overhang demand per layer.
///
/// Slices are first opened by half a line width (what a nozzle can
/// print); the overhang is what the layer below, grown by the threshold
/// angle, does not carry. Overhang islands cluster across layers where
/// they touch (within a line width) one layer apart; with small-overhang
/// removal, a cluster goes when its total area is under 4 mm² or its
/// footprint vanishes under an erosion of 2.5 line widths. Thin strips of
/// a shallow slope survive as a cluster although no single layer's strip
/// is a line wide.
///
/// An island with nothing of the layer below under it is demand whatever its size:
/// see [`floating`].
pub fn demand(ctl: crate::par::Ctl, slices: &[Vec<ExPolygon>], heights: &[(f64, f64)], p: &SupportParams) -> Result<Vec<Vec<ExPolygon>>, crate::par::Cancelled> {
    Ok(demand_and_floats(ctl, slices, heights, p)?.0)
}

/// [`demand`], and of it the islands with nothing under them.
fn demand_and_floats(ctl: crate::par::Ctl, slices: &[Vec<ExPolygon>], heights: &[(f64, f64)], p: &SupportParams) -> Result<(Vec<Vec<ExPolygon>>, Vec<Vec<ExPolygon>>), crate::par::Cancelled> {
    let mut out = overhangs(ctl, slices, heights, p)?;
    let floats = crate::par::try_map(ctl, slices.len(), |l| if l == 0 { Vec::new() } else { floating(&slices[l], &slices[l - 1]) })?;
    for (o, f) in out.iter_mut().zip(&floats) {
        if !f.is_empty() {
            o.extend(f.iter().cloned());
            *o = union_ex(o);
        }
    }
    Ok((out, floats))
}

/// The islands of `region` with nothing of `below` under them, however thin: the sliver
/// where a curved wall first breaks a layer is narrower than a bead, so the opening
/// that finds overhangs never sees it, yet its bead lands on air unless a support
/// reaches it. Under [`SPECK`] nothing prints.
pub(crate) fn floating(region: &[ExPolygon], below: &[ExPolygon]) -> Vec<ExPolygon> {
    region.iter().filter(|e| {
        if e.area_mm2() < SPECK {
            return false;
        }
        let b = e.bbox();
        let near: Vec<ExPolygon> = below.iter().filter(|x| x.bbox().overlaps(&b)).cloned().collect();
        near.is_empty() || total_area_mm2(&intersection_ex(std::slice::from_ref(e), &near)) <= 1e-6
    }).cloned().collect()
}

fn overhangs(ctl: crate::par::Ctl, slices: &[Vec<ExPolygon>], heights: &[(f64, f64)], p: &SupportParams) -> Result<Vec<Vec<ExPolygon>>, crate::par::Cancelled> {
    let n = slices.len();
    if ctl.cancelled() { return Err(crate::par::Cancelled); }
    let mut out: Vec<Vec<ExPolygon>> = vec![Vec::new(); n];
    let tan = (p.threshold_deg + 1.0).min(89.0).to_radians().tan();
    let half = p.line_width / 2.0 * SCALE;
    let ext: Vec<Vec<ExPolygon>> = crate::par::try_map(ctl, n, |l| offset_ex(&offset_ex(&slices[l], -half, Join::Miter(3.0)), half, Join::Miter(3.0)))?;
    let found: Vec<Vec<ExPolygon>> = crate::par::try_map(ctl, n, |l| {
        if l == 0 || ext[l].is_empty() {
            return Vec::new();
        }
        let h = heights[l].1;
        let grow = h / tan.max(1e-3);
        let carried = offset_ex(&slices[l - 1], grow * SCALE, Join::Miter(3.0));
        let mut o = difference_ex(&ext[l], &carried);
        o.retain(|e| e.area_mm2() > 0.01);
        o
    })?;
    for (l, o) in found.into_iter().enumerate() {
        out[l] = o;
    }
    if !p.remove_small {
        return Ok(out);
    }
    let small = small_groups(ctl, &out, p.line_width)?;
    for (o, small) in out.iter_mut().zip(&small) {
        let mut i = 0;
        o.retain(|_| {
            let keep = !small[i];
            i += 1;
            keep
        });
    }
    Ok(out)
}

/// Which overhang pieces are too small to need support, per layer and piece: the pieces grouped
/// up the layers (a piece joins every piece of the layer below that it meets once grown by a line
/// `line_width` mm wide), and a group is small when it is under 4 mm², or when its footprint, its
/// pieces grown by a line, is gone once shrunk by two and a quarter lines.
pub(crate) fn small_groups(ctl: crate::par::Ctl, pieces: &[Vec<ExPolygon>], line_width: f64) -> Result<Vec<Vec<bool>>, crate::par::Cancelled> {
    let n = pieces.len();
    // Clusters across layers (union-find over (layer, island)).
    let mut ids: Vec<(usize, usize)> = Vec::new();
    let mut first: Vec<usize> = vec![0; n + 1];
    for l in 0..n {
        if ctl.cancelled() { return Err(crate::par::Cancelled); }
        first[l] = ids.len();
        for i in 0..pieces[l].len() {
            ids.push((l, i));
        }
    }
    first[n] = ids.len();
    let mut parent: Vec<usize> = (0..ids.len()).collect();
    fn find(p: &mut Vec<usize>, mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let reach = line_width * SCALE;
    let quiet = crate::par::Ctl { tick: None, ..ctl };
    // Which piece of each layer meets which of the layer below, side by side: the groups they
    // make are the same in any order.
    let meetings = crate::par::try_map(quiet, n, |l| {
        let mut found: Vec<(usize, usize)> = Vec::new();
        if l == 0 || pieces[l - 1].is_empty() {
            return found;
        }
        for i in 0..pieces[l].len() {
            let grown = offset_ex(std::slice::from_ref(&pieces[l][i]), reach, Join::Miter(3.0));
            let gb = bbox_of(&grown);
            for j in 0..pieces[l - 1].len() {
                if gb.overlaps(&pieces[l - 1][j].bbox()) && !intersection_ex(&grown, std::slice::from_ref(&pieces[l - 1][j])).is_empty() {
                    found.push((i, j));
                }
            }
        }
        found
    })?;
    for (l, found) in meetings.iter().enumerate() {
        for &(i, j) in found {
            let (a, b) = (find(&mut parent, first[l] + i), find(&mut parent, first[l - 1] + j));
            if a != b {
                parent[a] = b;
            }
        }
    }
    let mut members: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for k in 0..ids.len() {
        let r = find(&mut parent, k);
        members.entry(r).or_default().push(k);
    }
    let mut small: Vec<Vec<bool>> = pieces.iter().map(|p| vec![false; p.len()]).collect();
    let erode = 1.25 * line_width * SCALE;
    let groups: Vec<Vec<usize>> = members.into_values().collect();
    let is_small = crate::par::try_map(quiet, groups.len(), |g| {
        let m = &groups[g];
        let area: f64 = m.iter().map(|&k| pieces[ids[k].0][ids[k].1].area_mm2()).sum();
        area < 4.0 || {
            // The footprint closes the gaps between a slope's strips.
            let foot = offset_ex(&m.iter().map(|&k| pieces[ids[k].0][ids[k].1].clone()).collect::<Vec<_>>(), reach, Join::Miter(3.0));
            offset_ex(&foot, -(erode + reach), Join::Miter(3.0)).is_empty()
        }
    })?;
    for (m, _) in groups.iter().zip(&is_small).filter(|(_, is)| **is) {
        for &k in m {
            small[ids[k].0][ids[k].1] = true;
        }
    }
    Ok(small)
}

/// How far apart tips of radius `r0` start (mm): a line width over the top rate (30 %), plus what
/// a tip is wider than a line.
fn tip_spacing(line_width: f64, r0: f64) -> f64 {
    line_width / 0.3 + (2.0 * r0 - line_width).max(0.0)
}

/// How far a roof's edge may lie past the support under it, in line widths: a bead's half
/// rests on its neighbour.
const CARRIED: f64 = 0.5;

/// The point of `edges` nearest `p`, if one is nearer than `within` (units).
fn nearest_within(edges: &SegGrid<u32>, p: Pt, within: f64) -> Option<(Pt, f64)> {
    let mut best = None;
    let mut bound = within;
    edges.nearest_with(p, |a, b, _| {
        let ab = b.sub(a);
        let l2 = dot(ab, ab);
        let q = if l2 == 0 { a } else { lerp(a, b, (dot(p.sub(a), ab) as f64 / l2 as f64).clamp(0.0, 1.0)) };
        let d = p.dist(q);
        if d < bound {
            bound = d;
            best = Some((q, d));
        }
        bound
    });
    best
}

pub(crate) fn circle(x: f64, y: f64, r: f64) -> Polygon {
    // Fine enough that the chords stay within the G-code resolution of the
    // circle, so the writer's arc fitting turns it back into arcs.
    let n = ((std::f64::consts::PI / (1.0 - CURVE_TOL / r.max(0.01)).clamp(-1.0, 1.0).acos()).ceil() as usize).clamp(8, 256);
    Polygon::new((0..n).map(|k| {
        let a = k as f64 / n as f64 * std::f64::consts::TAU;
        Pt::from_mm(x + r * a.cos(), y + r * a.sin())
    }).collect())
}

/// Whether closing by `by` (mm) leaves circles `a` and `b` (x, y, r) a neck at least
/// `neck` (mm) wide: its half width is how far the two arcs of radius `by` that
/// blend them stay off the line between the centres.
pub(crate) fn neck_holds(a: (f64, f64, f64), b: (f64, f64, f64), by: f64, neck: f64) -> bool {
    let d = (a.0 - b.0).hypot(a.1 - b.1);
    let (ca, cb) = (a.2 + by, b.2 + by);
    if d >= ca + cb {
        return false;
    }
    if d <= (a.2 - b.2).abs() {
        return true;
    }
    let along = (d * d + ca * ca - cb * cb) / (2.0 * d);
    (ca * ca - along * along).sqrt() - by >= neck / 2.0
}

/// A layer's sections as an outline: sections whose neck holds are blended, closed
/// by `by` (mm), so where they meet the outline bends no tighter than that. The way
/// in leaves the rest of a grown circle vertex for vertex, on a circle, for the
/// writer's arc fitting. Sections closer than that but with too
/// thin a neck stay apart: a neck that was opened away would leave a horn on each.
fn sections_outline(sections: &[(f64, f64, f64)], by: f64, neck: f64) -> Vec<ExPolygon> {
    // Clusters of sections linked by a neck, over a sweep along x.
    let mut order: Vec<usize> = (0..sections.len()).collect();
    order.sort_by(|&i, &j| sections[i].0.total_cmp(&sections[j].0));
    let widest = sections.iter().fold(0.0f64, |m, s| m.max(s.2));
    let mut root: Vec<usize> = (0..sections.len()).collect();
    fn find(root: &mut [usize], mut i: usize) -> usize {
        while root[i] != i {
            root[i] = root[root[i]];
            i = root[i];
        }
        i
    }
    for (k, &i) in order.iter().enumerate() {
        for &j in &order[k + 1..] {
            if sections[j].0 - sections[i].0 >= 2.0 * (widest + by) {
                break;
            }
            if neck_holds(sections[i], sections[j], by, neck) {
                let (ri, rj) = (find(&mut root, i), find(&mut root, j));
                root[ri.max(rj)] = ri.min(rj);
            }
        }
    }
    let mut clusters: Vec<Vec<(f64, f64, f64)>> = vec![Vec::new(); sections.len()];
    for (i, s) in sections.iter().enumerate() {
        clusters[find(&mut root, i)].push(*s);
    }
    let mut out: Vec<ExPolygon> = Vec::new();
    for cluster in clusters {
        if cluster.len() > 1 {
            // The union of the circles grown by `by` is their union grown by it: only the
            // way in is left to do.
            let rings: Vec<Polygon> = cluster.iter().map(|s| circle(s.0, s.1, s.2 + by)).collect();
            let u = union(&rings, FillRule::NonZero);
            out.extend(offset_ex(&u, -by * SCALE, Join::Round(BLEND_TOL * SCALE)));
        } else if let Some(s) = cluster.first() {
            out.push(ExPolygon::new(circle(s.0, s.1, s.2)));
        }
    }
    out
}

/// Whether an outline has a corner a bead could not follow: a sharp left turn, with
/// the region on the left of its rings.
fn has_corner(e: &ExPolygon) -> bool {
    e.rings().any(|ring| {
        let n = ring.pts.len();
        (0..n).any(|i| {
            let (a, b, c) = (ring.pts[(i + n - 1) % n], ring.pts[i], ring.pts[(i + 1) % n]);
            let (ux, uy, vx, vy) = ((b.x - a.x) as f64, (b.y - a.y) as f64, (c.x - b.x) as f64, (c.y - b.y) as f64);
            (ux * vy - uy * vx).atan2(ux * vx + uy * vy) > CORNER_TURN
        })
    })
}

/// `base` without what has no room for a wall. The mitres restore a curved outline
/// vertex for vertex, so the writer's arc fitting still finds its arcs. What erosion
/// left with a corner (a cut by the clearance) grows back bevelled, with the coarsest
/// round join: a mitre there would stick out past `base`, and a finer curve is many
/// short moves the printer slows down for, on a wall too tight to show them.
fn opened(base: &[ExPolygon], by: f64) -> Vec<ExPolygon> {
    let (cornered, curved): (Vec<ExPolygon>, Vec<ExPolygon>) = offset_ex(base, -by, Join::Miter(3.0)).into_iter().filter(|e| e.area_mm2() >= MIN_CORE).partition(has_corner);
    let mut grown = offset_ex(&curved, by, Join::Miter(3.0));
    if !cornered.is_empty() {
        grown.extend(offset_ex(&cornered, by, Join::Round(by)));
        grown = union_ex(&grown);
    }
    grown
}

/// (x, y) shortened to at most `max` long.
fn capped(x: f64, y: f64, max: f64) -> (f64, f64) {
    let l = (x * x + y * y).sqrt();
    if l <= max.max(0.0) {
        (x, y)
    } else {
        let s = max.max(0.0) / l;
        (x * s, y * s)
    }
}

/// Whether the circle at (x, y) with radius r lies inside `region`, tested
/// at its centre and twelve rim points.
/// Whether anything of section (x, y, r) prints in `printed`, by points across it.
fn prints(printed: &SegGrid<u32>, (x, y, r): (f64, f64, f64)) -> bool {
    if printed.is_empty() {
        return false;
    }
    let at = |f: f64, k: usize, of: usize| {
        let a = std::f64::consts::TAU * k as f64 / of as f64;
        printed.contains_any(Pt::from_mm(x + f * r * a.cos(), y + f * r * a.sin()))
    };
    printed.contains_any(Pt::from_mm(x, y)) || (0..8).any(|k| at(0.5, k, 8)) || (0..12).any(|k| at(0.9, k, 12))
}

/// What of `set` lies in `bb`, cut to the box ring by ring: far cheaper than a boolean with all
/// of a big island (a lattice's layer, a vase's wall) when only a pad's worth of it matters.
fn windowed(set: &[ExPolygon], bb: &BBox) -> Vec<ExPolygon> {
    let mut out = Vec::new();
    for e in set {
        let b = e.bbox();
        if !b.overlaps(bb) {
            continue;
        }
        if b.min.x >= bb.min.x && b.min.y >= bb.min.y && b.max.x <= bb.max.x && b.max.y <= bb.max.y {
            out.push(e.clone());
            continue;
        }
        let Some(contour) = ring_in_box(&e.contour.pts, bb) else { continue };
        let holes = e.holes.iter().filter_map(|h| ring_in_box(&h.pts, bb)).map(Polygon::new).collect();
        out.push(ExPolygon { contour: Polygon::new(contour), holes });
    }
    out
}

/// A ring cut to a box, side by side (Sutherland-Hodgman): what is left keeps the ring's turn,
/// and runs along the box where the ring left it.
fn ring_in_box(pts: &[Pt], bb: &BBox) -> Option<Vec<Pt>> {
    let mut ring = pts.to_vec();
    for side in 0..4 {
        if ring.len() < 3 {
            return None;
        }
        let inside = |p: Pt| match side {
            0 => p.x >= bb.min.x,
            1 => p.x <= bb.max.x,
            2 => p.y >= bb.min.y,
            _ => p.y <= bb.max.y,
        };
        let cross = |a: Pt, b: Pt| -> Pt {
            let at = |k: i64, a0: i64, a1: i64, b0: i64, b1: i64| b0 + ((b1 - b0) as f64 * (k - a0) as f64 / (a1 - a0) as f64).round() as i64;
            match side {
                0 => Pt::new(bb.min.x, at(bb.min.x, a.x, b.x, a.y, b.y)),
                1 => Pt::new(bb.max.x, at(bb.max.x, a.x, b.x, a.y, b.y)),
                2 => Pt::new(at(bb.min.y, a.y, b.y, a.x, b.x), bb.min.y),
                _ => Pt::new(at(bb.max.y, a.y, b.y, a.x, b.x), bb.max.y),
            }
        };
        let mut out = Vec::with_capacity(ring.len() + 4);
        for i in 0..ring.len() {
            let (a, b) = (ring[(i + ring.len() - 1) % ring.len()], ring[i]);
            match (inside(a), inside(b)) {
                (true, true) => out.push(b),
                (true, false) => out.push(cross(a, b)),
                (false, true) => {
                    out.push(cross(a, b));
                    out.push(b);
                }
                (false, false) => {}
            }
        }
        ring = out;
    }
    (ring.len() >= 3).then_some(ring)
}

/// `set` less what of `cut` lies round it.
fn clear_of(set: Vec<ExPolygon>, cut: &[ExPolygon]) -> Vec<ExPolygon> {
    if set.is_empty() {
        return set;
    }
    let mut bb = bbox_of(&set);
    let d = SCALE as i64;
    (bb.min.x, bb.min.y, bb.max.x, bb.max.y) = (bb.min.x - d, bb.min.y - d, bb.max.x + d, bb.max.y + d);
    let cut = windowed(cut, &bb);
    if cut.is_empty() { set } else { difference_ex(&set, &cut) }
}

/// How far (mm) `p` lies from `set`: 0 in it, and at most `max`.
fn dist_to(set: &[ExPolygon], p: Pt, max: f64) -> f64 {
    if set.iter().any(|e| e.contains(p)) {
        return 0.0;
    }
    let mut d = max * SCALE;
    for ring in set.iter().flat_map(|e| e.rings()) {
        let pts = &ring.pts;
        for i in 0..pts.len() {
            d = d.min(dist_point_segment(p, pts[i], pts[(i + 1) % pts.len()]));
        }
    }
    d / SCALE
}

/// The room (mm) round `q` that `cut` leaves, up to `max`.
fn room(q: Pt, cut: &[ExPolygon], max: f64) -> f64 {
    let d = ((max + 0.1) * SCALE) as i64;
    let bb = BBox { min: Pt::new(q.x - d, q.y - d), max: Pt::new(q.x + d, q.y + d) };
    dist_to(&windowed(cut, &bb), q, max)
}

/// How far apart (mm) `held` and the disc of `r` round `q` are: the farthest a point of either
/// lies from the other.
fn apart(held: &[ExPolygon], q: Pt, r: f64) -> f64 {
    let out = held.iter().flat_map(|e| e.contour.pts.iter()).map(|p| p.dist(q) / SCALE - r).fold(0.0, f64::max);
    let far = 2.0 * r + out + 1.0;
    let mut inn = dist_to(held, q, far);
    for (f, m) in [(0.5, 12), (1.0, 24)] {
        for i in 0..m {
            let a = i as f64 / m as f64 * std::f64::consts::TAU;
            inn = inn.max(dist_to(held, Pt::from_mm(q.x_mm() + f * r * a.cos(), q.y_mm() + f * r * a.sin()), far));
        }
    }
    out.max(inn)
}

/// The middle of `e`'s area.
fn centroid(e: &ExPolygon) -> Pt {
    let (mut sx, mut sy, mut sa) = (0.0, 0.0, 0.0);
    for ring in e.rings() {
        let pts = &ring.pts;
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            let (ax, ay, bx, by) = (a.x_mm(), a.y_mm(), b.x_mm(), b.y_mm());
            let c = ax * by - bx * ay;
            sx += (ax + bx) * c;
            sy += (ay + by) * c;
            sa += c;
        }
    }
    if sa.abs() < 1e-12 { e.bbox().center() } else { Pt::from_mm(sx / (3.0 * sa), sy / (3.0 * sa)) }
}

/// What the support's top is on a layer: one top gap (and the roof) under what it holds.
struct Region {
    area: Vec<ExPolygon>,
}

/// What [`tapers`] keeps off and how far a layer may reach past the one under it.
struct Tapers<'a> {
    /// What keeps support off, per layer (see [`crate::support_rules::Legal`]).
    keep: &'a [Vec<ExPolygon>],
    /// How far a layer of support may reach past the one under it (mm per layer).
    grow: f64,
    /// The thinnest a trunk starts (mm).
    r_min: f64,
    /// The widest a trunk starts (mm).
    r_max: f64,
    /// How far (mm) past the layer under it a layer is still carried.
    carried: f64,
    /// What is narrower than twice this (mm) carries nothing of its own.
    width: f64,
    /// How much further (mm) than `keep` a trunk keeps off the part, and the support once it is
    /// under what it holds, leaving the part at the steep angle: the side gap or the branch gap.
    /// Until the descent routes the side gap as a preference, a trunk keeps it whole.
    pull: f64,
    /// Per layer: where a support would be locked in the part (see [`locked_in`]), the part's
    /// pores and slots among it. Filled, they would lock their piece in. None where the part lets
    /// its supports be locked in.
    pores: &'a [Vec<ExPolygon>],
    /// Where the part is under a trunk.
    shadow: &'a Shadow,
    /// Where a trunk of each width gets down to its footing from.
    fields: &'a Fields,
    /// Whether the support stands on the bed only.
    bed_only: bool,
    /// Not from the bed only: per layer the part the support may stand on, that many layers
    /// under it (the bottom gap).
    rests: Option<(&'a [Vec<ExPolygon>], usize)>,
}

/// What a footprint leaves unreached (mm2) before it gets another trunk.
const HANG_MIN: f64 = 0.2;
/// How far (mm) past its trunk the support widening into a footprint reaches: past it, another
/// trunk is fewer layers of solid support than a longer warp.
const WARP: f64 = 10.0;
/// What a layer holds within this far (mm) and this many layers of what the layer over it holds
/// is one footprint with it, as under a slope.
const LINK: f64 = 2.5;
const LINK_LAYERS: usize = 2;
/// The most trunks one footprint gets.
const TRUNKS_MAX: usize = 64;

/// Where a trunk under `piece` goes, and how wide it is: as wide as the part leaves it room for
/// `pull` further off than `cut`, up to `r_max`, and no wider than all of `piece` round it, where
/// the warp up to all of `piece` is the shortest. A narrow trunk counts against it. From the bed
/// only, it stands where a trunk that wide gets down to the bed from on `piece`'s lowest layer
/// `lo` (round the part, at the steep angle), out to the warp's length, and leans in from there;
/// with nowhere like that, there is no trunk.
fn place(piece: &ExPolygon, cut: &[ExPolygon], pull: f64, lo: usize, t: &Tapers) -> Option<(Pt, f64)> {
    let c = centroid(piece);
    let far = |s: Pt| piece.contour.pts.iter().map(|p| p.dist(s) / SCALE).fold(0.0, f64::max);
    let mut best: Option<(f64, Pt, f64)> = None;
    let mut at = |s: Pt| {
        let room = room(s, cut, t.r_max + pull) - pull;
        if room < t.r_min {
            return;
        }
        let f = far(s);
        let r = room.min(f.max(t.r_min));
        // From the bed only, as wide as gets down from there, if any width does.
        let Some(r) = (if t.bed_only { t.fields.widest(s, lo, r) } else { Some(r) }) else { return };
        let score = r - f - 3.0 * (t.r_max.min(f) - r);
        if best.is_none_or(|b| score > b.0) {
            best = Some((score, s, r));
        }
    };
    at(c);
    for d in [1.0, 2.0, 3.0, 4.5, 6.0, 8.0, WARP] {
        for k in 0..16 {
            let a = k as f64 / 16.0 * std::f64::consts::TAU;
            at(Pt::from_mm(c.x_mm() + d * a.cos(), c.y_mm() + d * a.sin()));
        }
    }
    best.map(|b| (b.1, b.2))
}

/// The support under what it holds, top down, with the fewest trunks. Its top layer is what it
/// holds, one top gap (and the roof) under it: a footprint, with what the layers just under it
/// hold, as under a slope. Each footprint gets one trunk, and another only for what the first
/// cannot reach: past [`WARP`] of it, or round the part. Between a footprint and its trunk the
/// layers warp the one shape into the other: each layer holds what lies within the steep angle
/// of the footprint over it and of the trunk under it, clear of the part. So every layer is
/// carried by the one under it and reaches no further than the one over it, the round trunk
/// widens into the footprint's shape, and what a trunk cannot get to it does not hold. A footprint
/// too near the bed for its warp stands on the bed. The warp prints solid at its top.
///
/// Returns the trunk tops per layer (centre and radius), the support per layer, and what of it
/// each layer holds right under the part, and empties `regions`.
fn tapers(ctl: crate::par::Ctl, regions: &mut [Vec<Region>], t: &Tapers) -> Result<Warps, crate::par::Cancelled> {
    let n = regions.len();
    let g = t.grow;
    let mut trunks: Vec<Vec<(Pt, f64)>> = vec![Vec::new(); n];
    let mut pads: Vec<Vec<ExPolygon>> = vec![Vec::new(); n];
    let mut tops: Vec<Vec<ExPolygon>> = vec![Vec::new(); n];
    let round = Join::Round(0.02 * SCALE);
    let disc = |q: Pt, r: f64| ExPolygon::new(circle(q.x_mm(), q.y_mm(), r));
    let clip = |l: usize| t.keep[l].as_slice();
    // What each layer holds, as islands, and which footprint each is in.
    let mut islands: Vec<(usize, ExPolygon)> = Vec::new();
    for (l, rs) in regions.iter_mut().enumerate() {
        let mut held: Vec<ExPolygon> = Vec::new();
        for r in rs.iter_mut() {
            held.extend(std::mem::take(&mut r.area));
        }
        if !held.is_empty() {
            islands.extend(union_ex(&held).into_iter().filter(|e| e.area_mm2() >= SPECK).map(|e| (l, e)));
        }
    }
    let mut root: Vec<usize> = (0..islands.len()).collect();
    fn find(root: &mut [usize], mut i: usize) -> usize {
        while root[i] != i {
            root[i] = root[root[i]];
            i = root[i];
        }
        i
    }
    let d = (LINK * SCALE) as i64;
    let boxes: Vec<BBox> = islands.iter().map(|(_, e)| { let b = e.bbox(); BBox { min: Pt::new(b.min.x - d, b.min.y - d), max: Pt::new(b.max.x + d, b.max.y + d) } }).collect();
    for i in 0..islands.len() {
        let grown = offset_ex(std::slice::from_ref(&islands[i].1), LINK * SCALE, round);
        for j in i + 1..islands.len() {
            if islands[j].0 > islands[i].0 + LINK_LAYERS {
                break;
            }
            if islands[j].0 == islands[i].0 || !boxes[i].overlaps(&islands[j].1.bbox()) || find(&mut root, i) == find(&mut root, j) {
                continue;
            }
            if !intersection_ex(&grown, std::slice::from_ref(&islands[j].1)).is_empty() {
                let (a, b) = (find(&mut root, i), find(&mut root, j));
                root[a] = b;
            }
        }
    }
    let mut footprints: std::collections::BTreeMap<usize, Vec<usize>> = std::collections::BTreeMap::new();
    for i in 0..islands.len() {
        let r = find(&mut root, i);
        footprints.entry(r).or_default().push(i);
    }
    let count = footprints.len();
    for (k, members) in footprints.values().enumerate() {
        ctl.phase("planning trees", Some((k as u32 + 1, count as u32)), AT_TREES + (AT_PADS - AT_TREES) * k as f64 / count as f64);
        let lo = members.iter().map(|&i| islands[i].0).min().unwrap_or(0);
        let hi = members.iter().map(|&i| islands[i].0).max().unwrap_or(0);
        // A piece no trunk takes goes from what is left (what has no support is told, once, at
        // the end).
        let give_up = |left: &mut Vec<Vec<ExPolygon>>, piece: &ExPolygon| {
            for set in left.iter_mut() {
                if !set.is_empty() {
                    *set = difference_ex(set, std::slice::from_ref(piece));
                }
            }
        };
        // What of it no trunk holds yet, per layer from `lo`.
        let mut left: Vec<Vec<ExPolygon>> = vec![Vec::new(); hi + 1 - lo];
        for &i in members {
            left[islands[i].0 - lo].push(islands[i].1.clone());
        }
        let mut first = true;
        let mut bb = BBox::empty();
        for (_, e) in members.iter().map(|&i| &islands[i]) {
            bb.merge(&e.bbox());
        }
        let d = ((t.r_max + WARP + 1.0) * SCALE) as i64;
        let bb = BBox { min: Pt::new(bb.min.x - d, bb.min.y - d), max: Pt::new(bb.max.x + d, bb.max.y + d) };
        for _ in 0..TRUNKS_MAX {
            if ctl.cancelled() {
                return Err(crate::par::Cancelled);
            }
            // The biggest piece of what is left, seen from above, and of it what one trunk
            // can reach: all of it, or from one end of it.
            // Seen from above, what a slope holds layer by layer is one piece.
            let seen = union_ex(&offset_ex(&left.concat(), LINK / 2.0 * SCALE, round));
            let Some(piece) = seen.into_iter().filter(|e| total_area_mm2(&intersection_ex(&left.concat(), std::slice::from_ref(e))) >= HANG_MIN).max_by(|a, b| a.area_mm2().total_cmp(&b.area_mm2())) else { break };
            // What of it each layer holds, seen from above: a ledge of the part's own on a
            // layer or two, as narrow as a line or two, is held by the part, not by another
            // trunk; and its bulk, not its thin edges, says where its ends are.
            let raw: Vec<ExPolygon> = left.iter().flat_map(|set| intersection_ex(set, std::slice::from_ref(&piece))).collect();
            let spans = left.iter().filter(|set| !intersection_ex(set, std::slice::from_ref(&piece)).is_empty()).count();
            let bulk = opened(&union_ex(&raw), t.width * SCALE);
            if !first && spans <= 2 && bulk.is_empty() {
                for set in left.iter_mut() {
                    if !set.is_empty() {
                        *set = difference_ex(set, std::slice::from_ref(&piece));
                    }
                }
                continue;
            }
            first = false;
            let reach = t.r_max + WARP;
            let body = bulk.iter().max_by(|a, b| a.area_mm2().total_cmp(&b.area_mm2())).unwrap_or(&piece);
            let m = centroid(body);
            let piece = if piece.contour.pts.iter().all(|p| p.dist(m) / SCALE <= reach) {
                piece
            } else {
                let Some(&end) = body.contour.pts.iter().max_by_key(|q| q.dist2(m)) else { break };
                let (dx, dy) = (m.x_mm() - end.x_mm(), m.y_mm() - end.y_mm());
                let dl = dx.hypot(dy).max(1e-9);
                let c = Pt::from_mm(end.x_mm() + dx / dl * 0.9 * reach, end.y_mm() + dy / dl * 0.9 * reach);
                let Some(near) = intersection_ex(std::slice::from_ref(&piece), &[disc(c, reach)]).into_iter().max_by(|a, b| a.area_mm2().total_cmp(&b.area_mm2())) else { break };
                near
            };
            let pull = t.pull;
            let Some((q, r)) = place(&piece, clip(lo.saturating_sub(1)), pull, lo, t) else {
                // Nowhere for a trunk: it is given up, and told.
                give_up(&mut left, &piece);
                continue;
            };
            // The trunk starts low enough for its warp to reach all of the piece on every
            // layer it is on, and no lower than the longest warp.
            let mut s = hi as isize;
            for (k, set) in left.iter().enumerate() {
                let part = intersection_ex(set, std::slice::from_ref(&piece));
                if !part.is_empty() {
                    let need = (apart(&part, q, r).min(WARP) / g - 1e-6).ceil() as isize;
                    s = s.min((lo + k) as isize - need);
                }
            }
            // From the bed only, no trunk starts where it cannot get down to the bed (round the
            // part, at the steep angle). Otherwise it stands on the part where the part is under
            // it.
            let under = t.shadow.below(q.x_mm(), q.y_mm(), lo);
            let on_part = !t.bed_only && under.is_some_and(|k| s <= k as isize);
            if on_part {
                s = under.map_or(s, |k| (k + t.rests.map_or(1, |r| r.1)) as isize);
            }
            // The trunk fits clear of the part where it starts: else it starts higher.
            while !on_part && s >= 0 && (s as usize) < lo && room(q, clip(s as usize), r + pull) < r + pull - 1e-3 {
                s += 1;
            }
            // From the bed only it starts where it gets down to the bed from, as wide as does
            // there: higher, if it must, and narrower; with nowhere up to the footprint, it is
            // doomed.
            let (doomed, r) = if s >= 0 && t.bed_only {
                match (s as usize..=lo).find_map(|k| t.fields.widest(q, k, r).map(|w| (k, w))) {
                    Some((k, w)) => {
                        s = k as isize;
                        (false, w)
                    }
                    None => (true, r),
                }
            } else {
                (false, r)
            };
            // What the trunk reaches, going up from it at the steep angle round the part.
            let b0 = s.max(0) as usize;
            let mut reached: Vec<Vec<ExPolygon>> = Vec::with_capacity(hi + 1 - b0);
            if !doomed {
                // Not from the bed only, it also stands on the part where the part is under it.
                let mut b = vec![disc(q, r + (b0 as isize - s) as f64 * g)];
                for l in b0..=hi {
                    if l > b0 && !b.is_empty() {
                        b = offset_ex(&b, g * SCALE, round);
                    }
                    if let Some((rests, gap)) = t.rests.filter(|&(_, gap)| l >= gap) {
                        let on = windowed(&rests[l - gap], &bb);
                        if !on.is_empty() {
                            b.extend(on);
                            b = union_ex(&b);
                        }
                    }
                    b = clear_of(b, clip(l));
                    reached.push(b.clone());
                }
            }
            // What of the footprint it holds.
            let mut holds: Vec<Vec<ExPolygon>> = vec![Vec::new(); hi + 1 - lo];
            let mut held = 0.0;
            if !doomed {
                for (k, set) in left.iter().enumerate() {
                    if lo + k >= b0 && !set.is_empty() && !reached[lo + k - b0].is_empty() {
                        holds[k] = intersection_ex(set, &reached[lo + k - b0]).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
                    }
                }
            }
            // Of what it holds, an island mostly inside the part's pores and slots holds only
            // what a bare tip on its trunk holds there: wider, it would fill them and lock its
            // piece in. The rest of it there is given up, and told.
            let mut pored: Vec<Vec<ExPolygon>> = vec![Vec::new(); hi + 1 - lo];
            for (k, h) in holds.iter_mut().enumerate() {
                let pores = if h.is_empty() { Vec::new() } else { windowed(&t.pores[lo + k], &bbox_of(h)) };
                if pores.is_empty() {
                    continue;
                }
                let mut kept = Vec::new();
                for e in std::mem::take(h) {
                    let inside = intersection_ex(std::slice::from_ref(&e), &pores);
                    if total_area_mm2(&inside) > e.area_mm2() / 2.0 {
                        kept.extend(difference_ex(std::slice::from_ref(&e), &pores));
                        kept.extend(intersection_ex(&inside, &[disc(q, t.r_min)]));
                        pored[k].extend(inside);
                    } else {
                        kept.push(e);
                    }
                }
                *h = union_ex(&kept).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
            }
            held += holds.iter().map(|h| total_area_mm2(h)).sum::<f64>();
            if held < HANG_MIN {
                // Nothing holds this piece: it is given up, and told.
                give_up(&mut left, &piece);
                continue;
            }
            // The warp, top down: within the steep angle of what it holds over it, and of
            // the trunk under it, and within the hull of the two, so the tube squashes from
            // the one shape into the other and never bulges past both.
            let hull = {
                let mut pts: Vec<Pt> = holds.iter().flatten().flat_map(|e| e.contour.pts.iter().copied()).collect();
                pts.extend(circle(q.x_mm(), q.y_mm(), r).pts);
                [ExPolygon::new(Polygon::new(convex_hull(&mut pts)))]
            };
            let mut warp: Vec<ExPolygon> = Vec::new();
            let mut since = 0usize;
            for l in (b0..=hi).rev() {
                if !warp.is_empty() {
                    // It grows into none of the part's pores and slots: filled, they would
                    // lock it in.
                    let grown = offset_ex(&warp, g * SCALE, round);
                    let pores = windowed(&t.pores[l], &bbox_of(&grown));
                    let grown = if pores.is_empty() { grown } else { union_ex(&[difference_ex(&grown, &pores), warp].concat()) };
                    warp = clear_of(grown, clip(l));
                    // Under what it holds it leaves the part, out to the branch gap.
                    since += 1;
                    let off = (since as f64 * g).min(pull);
                    if off > 0.01 && !warp.is_empty() {
                        let mut wb = bbox_of(&warp);
                        let d = ((off + 1.0) * SCALE) as i64;
                        (wb.min.x, wb.min.y, wb.max.x, wb.max.y) = (wb.min.x - d, wb.min.y - d, wb.max.x + d, wb.max.y + d);
                        let by = windowed(clip(l), &wb);
                        if !by.is_empty() {
                            warp = difference_ex(&warp, &offset_ex(&by, off * SCALE, round));
                        }
                    }
                }
                if l >= lo && !holds[l - lo].is_empty() {
                    warp.extend(holds[l - lo].iter().cloned());
                    warp = union_ex(&warp);
                    since = 0;
                }
                if warp.is_empty() {
                    continue;
                }
                warp = intersection_ex(&intersection_ex(&warp, &reached[l - b0]), &hull);
                if s >= 0 && !on_part && l as isize > s {
                    let rc = r - (l as isize - s) as f64 * g;
                    if rc > 0.0 {
                        warp.extend(clear_of(vec![disc(q, rc)], clip(l)));
                        warp = union_ex(&warp);
                    }
                }
                pads[l].extend(warp.iter().cloned());
            }
            if s >= 0 && !on_part {
                let s = s as usize;
                trunks[s].push((q, r));
                pads[s].extend(clear_of(vec![disc(q, r)], clip(s)));
            }
            for (k, (set, h)) in left.iter_mut().zip(&holds).enumerate() {
                tops[lo + k].extend(h.iter().cloned());
                if !set.is_empty() && !h.is_empty() {
                    *set = difference_ex(set, h).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
                }
                if !set.is_empty() && !pored[k].is_empty() {
                    *set = difference_ex(set, &pored[k]).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
                }
            }
        }
    }
    if count > 0 {
        ctl.phase("planning trees", Some((count as u32, count as u32)), AT_PADS);
    }
    let mut pads = crate::par::try_map(ctl, n, |l| if pads[l].is_empty() { Vec::new() } else { union_ex(&pads[l]) })?;
    // From the bed only, the support keeps only what stands on the bed: what the layer under it
    // carries, or a trunk.
    if t.bed_only {
        for l in 1..n {
            if ctl.cancelled() {
                return Err(crate::par::Cancelled);
            }
            ctl.phase("standing trees on the bed", Some((l as u32 + 1, n as u32)), AT_PADS + (AT_DOWN - AT_PADS) * l as f64 / n as f64);
            if pads[l].is_empty() {
                continue;
            }
            let mut on = if pads[l - 1].is_empty() { Vec::new() } else { offset_ex(&pads[l - 1], (g + t.carried) * SCALE, round) };
            on.extend(trunks[l].iter().map(|(q, r)| disc(*q, *r + t.carried)));
            pads[l] = if on.is_empty() { Vec::new() } else { intersection_ex(&pads[l], &union_ex(&on)) };
            pads[l].retain(|e| e.area_mm2() >= SPECK);
        }
    }
    for rs in regions.iter_mut() {
        rs.retain(|r| !r.area.is_empty());
    }
    let tops = crate::par::try_map(ctl, n, |l| if tops[l].is_empty() { Vec::new() } else { union_ex(&tops[l]) })?;
    Ok(Warps { trunks, pads, tops })
}

/// What [`tapers`] makes.
struct Warps {
    /// The trunk tops per layer: centre and radius.
    trunks: Vec<Vec<(Pt, f64)>>,
    /// The support per layer.
    pads: Vec<Vec<ExPolygon>>,
    /// What of it each layer holds right under the part.
    tops: Vec<Vec<ExPolygon>>,
}

/// What of `region` lies clear of every one of `clips`.
fn clear_of_all<'a>(region: Vec<ExPolygon>, clips: impl Iterator<Item = &'a [ExPolygon]>) -> Vec<ExPolygon> {
    let mut left = region;
    for c in clips {
        if left.is_empty() {
            break;
        }
        left = difference_ex(&left, c);
    }
    left
}

fn covered(x: f64, y: f64, r: f64, region: &SegGrid<u32>) -> bool {
    let inside = |px: f64, py: f64| region.contains_any(Pt::from_mm(px, py));
    inside(x, y)
        && (0..12).all(|k| {
            let a = k as f64 / 12.0 * std::f64::consts::TAU;
            inside(x + r * a.cos(), y + r * a.sin())
        })
}

/// The widths (radius, mm, past the thinnest) a branch's way down is found for: a branch takes the
/// narrowest at least as wide as it is.
const CLASSES: [f64; 2] = [1.0, 1.75];

/// The thinnest a branch goes down, in lines across: a little more than the two lines anything
/// printed must be, so its drawn outline, a polygon inside its circle, is never narrower than them
/// (a column exactly two lines wide is cut away as too thin, with all it carries).
const COLUMN_WIDTHS: f64 = 2.2;

/// How far (mm) the chords of a field's rounded keep may lie inside its arcs. The keep is grown
/// this much more than a branch's radius, so a branch on a field's edge comes no nearer than its
/// radius to what it keeps off: else a section there is nicked when it is drawn, and a bare tip,
/// two lines wide, is then too narrow to print and goes with all it carries.
const FIELD_ARC: f64 = 0.02;

/// Where a branch gets down from to its footing (the bed, or not from the bed only a part top it
/// may stand on), per width: for each radius class, the [`Reach`] of a disc that wide over what the
/// branch keeps off, at the steep angle. The classes nest: a wider branch gets down from no more
/// than a narrower one. What no branch gets down to, not even the thinnest, the part closes in.
struct Fields {
    /// The classes' radii (mm), narrowest first.
    radii: Vec<f64>,
    reach: Vec<Reach>,
}

impl Fields {
    /// `keep`, per layer: what a branch keeps off. `rests`, per layer: the part tops a branch on
    /// that layer may stand on (none from the bed only). `lean`, per layer: how far (mm) a branch
    /// on it may lie past the one under it. `ctl`'s tick is told once per radius mapped.
    fn new(ctl: crate::par::Ctl, keep: &[Vec<ExPolygon>], rests: &[Vec<ExPolygon>], lean: &[f64], radii: Vec<f64>) -> Result<Self, crate::par::Cancelled> {
        let n = keep.len();
        let maps = crate::par::Ctl { tick: None, ..ctl };
        let mut reach = Vec::with_capacity(radii.len());
        for &r in &radii {
            let grown = crate::par::try_map(maps, n, |l| if keep[l].is_empty() { Vec::new() } else { offset_ex(&keep[l], (r + FIELD_ARC) * SCALE, Join::Round(FIELD_ARC * SCALE)) })?;
            reach.push(Reach::new(maps, &grown, rests, lean)?);
            ctl.done(1);
        }
        Ok(Fields { radii, reach })
    }

    /// The class of a branch `r` (mm) wide: the narrowest at least as wide.
    fn class(&self, r: f64) -> usize {
        self.radii.iter().position(|&c| c >= r - 1e-9).unwrap_or(self.radii.len() - 1)
    }

    /// How wide, up to `r`, a branch with its centre at `p` on layer `l` gets down to its footing
    /// from there: the widest class that does, or `r` if it is narrower. `None`: not even the
    /// thinnest does.
    fn widest(&self, p: Pt, l: usize, r: f64) -> Option<f64> {
        (0..=self.class(r)).rev().find(|&c| self.reach[c].reached(p, l)).map(|c| r.min(self.radii[c]))
    }

    /// Where a branch `r` wide, at `was` on the layer over `l`, goes on layer `l`, nearest `to`:
    /// `to` itself where it gets down from there, else the nearest point it gets down from within
    /// `lean` of `was` (one is there whenever `was` got down). Narrower, if it must: with the
    /// radius it then has. `None` when not even the thinnest gets down from within `lean` of `was`.
    fn step(&self, was: Pt, to: Pt, l: usize, r: f64, lean: f64) -> Option<(Pt, f64)> {
        let reach = lean * SCALE + 10.0;
        for c in (0..=self.class(r)).rev() {
            let f = &self.reach[c];
            let r = r.min(self.radii[c]);
            if f.reached(to, l) {
                return Some((to, r));
            }
            if let Some(q) = f.nearest(to, l, lean).filter(|q| q.dist(was) <= reach) {
                return Some((q, r));
            }
            if let Some(q) = f.nearest(was, l, lean) {
                return Some((q, r));
            }
        }
        None
    }

    /// What no branch, not even the thinnest, comes within `by` (mm) of on layer `l`. A branch on
    /// the field's edge counts as coming out to the keep: the [`FIELD_ARC`] it stays further off
    /// only keeps its drawn section whole, so it is no room it leaves unheld.
    fn beyond(&self, l: usize, by: f64) -> Vec<ExPolygon> {
        self.reach[0].beyond(l, by + FIELD_ARC)
    }
}

/// Which layers' keep (what keeps support off) covers each cell of a grid over the part (one
/// bit per layer per cell): the model below a branch, found without scanning polygons layer by
/// layer.
struct Shadow {
    x0: f64,
    y0: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    words: usize,
    bits: Vec<u64>,
    /// Per cell, the lowest layer covering it (`u32::MAX`: none).
    first: Vec<u32>,
}

impl Shadow {
    fn new(ctl: crate::par::Ctl, keep: &[Vec<ExPolygon>], cell: f64) -> Result<Self, crate::par::Cancelled> {
        let mut bb = BBox::empty();
        for l in keep {
            if ctl.cancelled() { return Err(crate::par::Cancelled); }
            for e in l {
                bb.merge(&e.bbox());
            }
        }
        let n = keep.len();
        let words = n.div_ceil(64);
        if bb.is_empty() {
            return Ok(Shadow { x0: 0.0, y0: 0.0, cell, nx: 0, ny: 0, words, bits: Vec::new(), first: Vec::new() });
        }
        let (x0, y0) = (bb.min.x_mm(), bb.min.y_mm());
        let nx = (((bb.max.x_mm() - x0) / cell).ceil() as usize).max(1);
        let ny = (((bb.max.y_mm() - y0) / cell).ceil() as usize).max(1);
        let rows: Vec<Vec<(usize, usize, usize)>> = crate::par::try_map(ctl, n, |l| crate::footing::cell_spans(&keep[l], (x0, y0, cell), (nx, ny)))?;
        let mut bits = vec![0u64; nx * ny * words];
        let mut first = vec![u32::MAX; nx * ny];
        for (l, spans) in rows.into_iter().enumerate() {
            if ctl.cancelled() { return Err(crate::par::Cancelled); }
            for (j, i0, i1) in spans {
                for i in i0..=i1 {
                    bits[(j * nx + i) * words + l / 64] |= 1 << (l % 64);
                    first[j * nx + i] = first[j * nx + i].min(l as u32);
                }
            }
        }
        Ok(Shadow { x0, y0, cell, nx, ny, words, bits, first })
    }

    /// The nearest cell centre (x, y, distance) within `max` mm of (x, y)
    /// that nothing below layer `l` covers: where a branch over the model
    /// gets past it. Rings of cells outward, nearest first.
    fn exit(&self, x: f64, y: f64, l: usize, max: f64) -> Option<(f64, f64, f64)> {
        let (ci, cj) = (((x - self.x0) / self.cell).floor() as i64, ((y - self.y0) / self.cell).floor() as i64);
        let free = |i: i64, j: i64| i < 0 || j < 0 || i >= self.nx as i64 || j >= self.ny as i64 || self.first[j as usize * self.nx + i as usize] as usize >= l;
        let rmax = (max / self.cell).ceil() as i64 + 1;
        let mut best: Option<(f64, f64, f64)> = None;
        for r in 1..=rmax {
            if let Some(b) = best {
                if b.2 < (r - 1) as f64 * self.cell {
                    break;
                }
            }
            for j in cj - r..=cj + r {
                for i in ci - r..=ci + r {
                    if (j - cj).abs() != r && (i - ci).abs() != r {
                        continue;
                    }
                    if free(i, j) {
                        let (px, py) = (self.x0 + (i as f64 + 0.5) * self.cell, self.y0 + (j as f64 + 0.5) * self.cell);
                        let d = (px - x).hypot(py - y);
                        if d <= max && best.is_none_or(|b| d < b.2) {
                            best = Some((px, py, d));
                        }
                    }
                }
            }
        }
        best
    }

    /// The highest layer below `l` whose keep covers (x, y).
    fn below(&self, x: f64, y: f64, l: usize) -> Option<usize> {
        let (i, j) = (((x - self.x0) / self.cell).floor(), ((y - self.y0) / self.cell).floor());
        if i < 0.0 || j < 0.0 || i as usize >= self.nx || j as usize >= self.ny || l == 0 {
            return None;
        }
        let w = &self.bits[(j as usize * self.nx + i as usize) * self.words..][..self.words];
        let mut k = l - 1;
        loop {
            let word = w[k / 64] & (u64::MAX >> (63 - k % 64));
            if word != 0 {
                return Some(k / 64 * 64 + 63 - word.leading_zeros() as usize);
            }
            if k < 64 {
                return None;
            }
            k = k / 64 * 64 - 1;
        }
    }
}

/// A part's tree supports, from [`generate_recording`].
pub struct Tree {
    /// Per part layer, what prints.
    pub layers: Vec<SupportLayer>,
    /// Per part layer, the ways ring walls keep support out of (see [`ring_walls`]): with the part
    /// and its paint, what keeps the support off (see [`crate::support_rules::Legal`]).
    pub rings: Vec<Vec<ExPolygon>>,
    /// Per part layer, the overhang it was asked to hold: what paint leaves of it and what paint
    /// adds. What it holds of it is measured on what prints (see [`crate::support_rules::hold`]).
    pub asked: Vec<Vec<ExPolygon>>,
}

pub fn generate(ctl: crate::par::Ctl, slices: &[Vec<ExPolygon>], heights: &[(f64, f64)], p: &SupportParams) -> Result<Vec<SupportLayer>, crate::par::Cancelled> {
    Ok(generate_recording(ctl, slices, heights, p, &mut Skeleton::default())?.layers)
}

/// [`generate`], recording the trees it grows in `skeleton`.
pub fn generate_recording(ctl: crate::par::Ctl, slices: &[Vec<ExPolygon>], heights: &[(f64, f64)], p: &SupportParams, skeleton: &mut Skeleton) -> Result<Tree, crate::par::Cancelled> {
    generate_painted(ctl, slices, heights, p, &Masks::default(), skeleton)
}

/// How many layers `h` (mm) high keep a gap of `gap` (mm): the fewest at least as thick, as
/// [`crate::support_rules::Legal`] counts it by actual z.
pub fn gap_layers(gap: f64, h: f64) -> usize {
    if gap <= 0.0 { 0 } else { ((gap - 1e-6) / h).ceil().max(1.0) as usize }
}

/// [`generate_recording`] under the part's support paint.
pub fn generate_painted(ctl: crate::par::Ctl, slices: &[Vec<ExPolygon>], heights: &[(f64, f64)], p: &SupportParams, paint: &Masks, skeleton: &mut Skeleton) -> Result<Tree, crate::par::Cancelled> {
    let n = slices.len();
    if ctl.cancelled() { return Err(crate::par::Cancelled); }
    let mut out = vec![SupportLayer::default(); n];
    if n < 3 {
        return Ok(Tree { layers: out, rings: Vec::new(), asked: Vec::new() });
    }
    // Nothing comes nearer the part than xy_min (R5); the side gap is where a branch keeps to.
    let f = p.xy_min();
    ctl.phase("finding overhangs", None, 0.0);
    let (demand, floats) = demand_and_floats(ctl, slices, heights, p)?;
    let painted = paint.demand(ctl, slices, demand, p.line_width, f.max(p.line_width / 2.0) + TIP_ROOM, &mut skeleton.blocked)?;
    let (demand, thin, added) = (painted.demand, painted.thin, painted.added);
    if demand.iter().chain(&thin).all(|d| d.is_empty()) {
        return Ok(Tree { layers: out, rings: Vec::new(), asked: Vec::new() });
    }
    // What it is asked to hold, per part layer: the overhang paint leaves and what paint adds.
    let wanted: Vec<Vec<ExPolygon>> = if added.iter().all(|a| a.is_empty()) {
        demand.clone()
    } else {
        crate::par::try_map(ctl, n, |l| match added.get(l).filter(|a| !a.is_empty()) {
            Some(a) => union_ex(&[demand[l].clone(), a.clone()].concat()),
            None => demand[l].clone(),
        })?
    };
    let h = heights.get(1).map(|x| x.1).unwrap_or(0.2).max(0.05);
    let gap_top = gap_layers(p.top_gap, h);
    let gap_bottom = gap_layers(p.bottom_gap, h);
    let m_fast = h * p.angle_max_deg.to_radians().tan();
    // What keeps support off, per layer (see [`crate::support_rules::Legal`]): the part's, with
    // the paint's blocks, and the ways of the ring walls what the support would close round makes.
    let add = |a: Vec<ExPolygon>, b: &[ExPolygon]| if b.is_empty() { a } else if a.is_empty() { b.to_vec() } else { union_ex(&[a, b.to_vec()].concat()) };
    fn layer_of(set: &[Vec<ExPolygon>], l: usize) -> &[ExPolygon] {
        set.get(l).map_or(&[], Vec::as_slice)
    }
    let of_part = {
        let legal = Legal::new(slices, heights, p, &[], &[]);
        crate::par::try_map(ctl, n, |l| legal.keeps(l))?
    };
    let (unringed, of_part) = if paint.blocks() {
        (crate::par::try_map(ctl, n, |l| add(of_part[l].clone(), layer_of(paint.block_layers(), l)))?, Some(of_part))
    } else {
        (of_part, None)
    };
    let rings = ring_walls(ctl, &demand, &unringed, slices, gap_top, WALL_WIDTHS * p.line_width, p.xy_gap + (p.branch_diameter / 2.0).max(p.tip_diameter / 2.0), m_fast)?;
    let legal = Legal::new(slices, heights, p, paint.block_layers(), &rings);
    let keep: Vec<Vec<ExPolygon>> = crate::par::try_map_vec(ctl, unringed, |l, k| add(k, layer_of(&rings, l)))?;
    // How much further than xy_min a branch keeps off the part once it has the room: the side gap,
    // and once it has grown, the branch gap.
    let side_off = (p.xy_gap - f).max(0.0);
    let branch_off = (p.xy_gap.max(BRANCH_GAP) - f).max(0.0);
    // The thinnest a trunk starts.
    let r_bare = (p.tip_diameter / 2.0).max(0.05);
    let r_branch = (p.branch_diameter / 2.0).max(r_bare);
    let r_col = p.column();
    let r_max = TRUNK_MAX_R.max(r_branch);
    // Where a support would be locked in the part (see [`locked_in`]). It keeps no branch off:
    // what of the overhang only a support standing there would hold is told as that, unless the
    // part lets its supports be locked in, and then what stands there is told as that.
    let locked = locked_in(ctl, &keep.iter().map(Vec::as_slice).collect::<Vec<_>>(), &[], None, slices, window(p), p.xy_gap + r_branch)?.locked;
    // Where a branch of each width gets down from, at the steep angle, round what keeps it off: to
    // the bed, or not from the bed only to a part top it may stand on, one bottom gap under it.
    let radii: Vec<f64> = std::iter::once(r_col).chain(CLASSES.into_iter().filter(|&r| r > r_col && r < r_max)).chain(std::iter::once(r_max)).collect();
    let step = crate::par::Step::new(ctl, "mapping where branches reach", AT_REACH, AT_ROOFS, radii.len());
    let rests: Vec<Vec<ExPolygon>> = crate::par::try_map(ctl, n, |l| legal.rests(l))?;
    let lean: Vec<f64> = (0..n).map(|l| legal.lean(l)).collect();
    let tick = |k: usize| step.tick(k);
    let fields = Fields::new(crate::par::Ctl { tick: Some(&tick), ..ctl }, &keep, &rests, &lean, radii)?;
    // What no branch gets to, not even the thinnest, the part closes in: of what it is asked to
    // hold, what nothing that gets down to a footing comes within half the contact gap of on its
    // contact's layer (see [`Fields::beyond`]), round what keeps support off but the paint (what
    // the paint keeps branches off of is the paint's to tell). Told as that.
    let unpainted = match &of_part {
        Some(of_part) => {
            let bare = Legal::new(slices, heights, p, &[], &rings);
            let keep: Vec<Vec<ExPolygon>> = crate::par::try_map(ctl, n, |l| add(of_part[l].clone(), layer_of(&rings, l)))?;
            let rests: Vec<Vec<ExPolygon>> = crate::par::try_map(ctl, n, |l| bare.rests(l))?;
            Some(Fields::new(ctl, &keep, &rests, &lean, vec![r_col])?)
        }
        None => None,
    };
    let shut = unpainted.as_ref().unwrap_or(&fields);
    let inside: Vec<Vec<ExPolygon>> = crate::par::try_map(ctl, n, |d| {
        let Some(c) = legal.contact(d).filter(|_| !wanted[d].is_empty()) else { return Vec::new() };
        let far = shut.beyond(c, r_col + crate::support_rules::CONTACT_GAP / 2.0);
        if far.is_empty() { far } else { intersection_ex(&wanted[d], &far) }
    })?;
    // Of the rest, what has its contact where a support would be locked in, unless the part lets
    // its supports be: no tip goes under it, and it is told as that. Branches from elsewhere still
    // go through there.
    let shut_in: Vec<Vec<ExPolygon>> = if p.stand_inside || locked.iter().all(|x| x.is_empty()) {
        vec![Vec::new(); n]
    } else {
        crate::par::try_map(ctl, n, |d| {
            let Some(c) = legal.contact(d).filter(|&c| !wanted[d].is_empty() && !locked[c].is_empty()) else { return Vec::new() };
            let rest = if inside[d].is_empty() { wanted[d].clone() } else { difference_ex(&wanted[d], &inside[d]) };
            if rest.is_empty() { rest } else { intersection_ex(&rest, &locked[c]) }
        })?
    };
    let told = |of: &[Vec<ExPolygon>]| -> Vec<GivenUp> {
        of.iter().enumerate().filter_map(|(d, x)| GivenUp::of(d, &x.iter().filter(|e| e.area_mm2() >= SPECK).cloned().collect::<Vec<ExPolygon>>())).collect()
    };
    skeleton.inside = told(&inside);
    skeleton.locked = told(&shut_in);
    // `of` less `x`, per layer.
    let without = |of: &[Vec<ExPolygon>], x: &[Vec<ExPolygon>]| -> Vec<Vec<ExPolygon>> {
        of.iter().enumerate().map(|(l, d)| match x.get(l) { Some(x) if !d.is_empty() && !x.is_empty() => difference_ex(d, x), _ => d.clone() }).collect()
    };
    let lost: Vec<Vec<ExPolygon>> = inside.into_iter().zip(&shut_in).map(|(a, b)| if b.is_empty() { a } else if a.is_empty() { b.clone() } else { union_ex(&[a, b.clone()].concat()) }).collect();
    let open = without(&demand, &lost);
    // Demand islands narrower than two lines cannot hold roof lines; they
    // get bare tips right under the top gap (the reference prints no roof
    // under a 20° slope, only 0.8 mm tips about 1.8 mm apart).
    let roofable = |e: &ExPolygon| p.interface_top_layers > 0 && !offset_ex(std::slice::from_ref(e), -p.line_width * SCALE, Join::Miter(3.0)).is_empty();
    let split: Vec<(Vec<ExPolygon>, Vec<ExPolygon>)> = crate::par::try_map(ctl, n, |l| demand[l].iter().cloned().partition(|e| roofable(e)))?;
    let (roofed, mut bare): (Vec<Vec<ExPolygon>>, Vec<Vec<ExPolygon>>) = split.into_iter().unzip();
    for (b, t) in bare.iter_mut().zip(thin) {
        b.extend(t);
    }
    let (roofed, bare) = if shut_in.iter().all(|x| x.is_empty()) { (roofed, bare) } else { (without(&roofed, &shut_in), without(&bare, &shut_in)) };
    // What the tips hold, per layer.
    let wants = crate::par::try_map(ctl, n, |l| if bare[l].is_empty() { roofed[l].clone() } else { union_ex(&[roofed[l].clone(), bare[l].clone()].concat()) })?;
    let m_slow = h * p.angle_slow_deg.to_radians().tan();
    // To the bed where it can get there: a branch over the model moves out
    // from over it while it has the layers to, instead of landing on it.
    let shadow = Shadow::new(ctl, &keep, 0.5)?;
    let ramp = |r0: f64| (((r_branch - r0) / m_slow.max(1e-3)).ceil() as usize).max(1);
    let spacing_of = |r0: f64| tip_spacing(p.line_width, r0);
    let taper = h * p.taper_deg.to_radians().tan();
    let z_foot = heights[1].0;
    let mut nodes: Vec<Node> = Vec::new();
    let mut joining: Vec<Joining> = Vec::new();
    // Per node id: the id it merged into, its own while it has not.
    let mut merged_into: Vec<usize> = Vec::new();
    let mut sections: Vec<Vec<Drawn>> = vec![Vec::new(); n];
    // Per stretch, per section: all of it prints.
    let mut whole: Vec<Vec<bool>> = Vec::new();
    // What keeps support off as a spatial index, kept while the layers below have an equal one.
    let mut grid: Option<(usize, SegGrid<u32>)> = None;
    // Per layer: the highest layer at or above it with an equal keep (the descent runs
    // downwards).
    let mut canon: Vec<usize> = (0..n).collect();
    for l in (0..n.saturating_sub(1)).rev() {
        if ctl.cancelled() { return Err(crate::par::Cancelled); }
        if keep[l] == keep[l + 1] {
            canon[l] = canon[l + 1];
        }
    }
    // Per layer: where a support would be locked in the part, the part's pores and slots among
    // it. A section the keep cuts where one of them is in its reach would fill it and lock its
    // piece in. None where the part lets its supports be locked in.
    let pores: Vec<Vec<ExPolygon>> = if p.stand_inside { vec![Vec::new(); n] } else { locked.clone() };
    let mut pore_grid: Option<(usize, SegGrid<u32>)> = None;
    ctl.phase("laying roofs", None, AT_ROOFS);
    // Per layer, in parallel: its roofs and what the support holds right under the part. A roof
    // layer, or what holds, keeps only what lies clear of what keeps support off on the layers up
    // to the top gap of the same overhang, so nothing of a roof column stops short of it.
    let clear_up = |region: Vec<ExPolygon>, l: usize, top: usize| clear_of_all(region, (l..=top).map(|j| keep[j].as_slice()));
    let roofs: Vec<(Vec<ExPolygon>, Vec<Region>)> = crate::par::try_map(ctl, n, |l| {
        let roof_top = l + 1 + gap_top;
        let mut roof = Vec::new();
        for k in 0..p.interface_top_layers {
            let src = roof_top + k;
            if src < n && !roofed[src].is_empty() {
                roof.extend(clear_up(roofed[src].clone(), l, l + k));
            }
            // One too thin for a roof of its own gets one a line wider all round: the tip under
            // it may stand inside a trunk, whose sheath prints nothing in its middle.
            if src < n && !floats[src].is_empty() && !bare[src].is_empty() {
                let thin = intersection_ex(&bare[src], &floats[src]);
                if !thin.is_empty() {
                    roof.extend(difference_ex(&offset_ex(&thin, p.line_width * SCALE, Join::Miter(3.0)), &keep[l]));
                    roof = union_ex(&roof);
                }
            }
        }
        let tip_src = roof_top + p.interface_top_layers;
        let mut regions = Vec::new();
        for (src, set, thin) in [(tip_src, &roofed, false), (roof_top, &bare, true)] {
            if src < n && !set[src].is_empty() {
                let top = src - 1 - gap_top;
                // What is too thin for two lines is held two lines wider all round, so two lines
                // of what holds it print where the part leaves room on one side (see
                // [`crate::support_rules`]).
                let held = if thin { offset_ex(&set[src], 2.0 * p.line_width * SCALE, Join::Miter(3.0)) } else { set[src].clone() };
                regions.push(Region { area: clear_up(held, l, top) });
            }
        }
        regions.retain(|r| !r.area.is_empty());
        (roof, regions)
    })?;
    let (roofs, mut regions): (Vec<Vec<ExPolygon>>, Vec<Vec<Region>>) = roofs.into_iter().unzip();
    // The support is what it holds tapering down to trunks (see [`tapers`]): no tips.
    let reach = gap_bottom + 1 + (LAND_AIR / h).ceil() as usize;
    let rests_on = paint.rests_on(ctl, slices, reach)?;
    let Warps { mut trunks, pads, tops } = tapers(ctl, &mut regions, &Tapers { keep: &keep, grow: m_fast, r_min: r_bare, r_max, carried: CARRIED * p.line_width, width: p.line_width, pull: branch_off, pores: &pores, shadow: &shadow, fields: &fields, bed_only: p.build_plate_only, rests: (!p.build_plate_only).then_some((&rests_on[..], gap_bottom + 1)) })?;
    for (l, roof) in roofs.into_iter().enumerate().rev() {
        if ctl.cancelled() { return Err(crate::par::Cancelled); }
        ctl.phase("bringing branches down", Some(((n - l) as u32, n as u32)), AT_DOWN + (AT_DRAWING - AT_DOWN) * (n - 1 - l) as f64 / n as f64);
        out[l].interface.extend(roof);
        let tips: Vec<(Pt, f64)> = std::mem::take(&mut trunks[l]);
        if nodes.is_empty() && tips.is_empty() {
            continue;
        }
        if grid.as_ref().is_none_or(|g| g.0 != canon[l]) {
            grid = Some((canon[l], SegGrid::of_regions(&keep[l], |k| k as u32)));
        }
        let edges = &grid.as_ref().expect("just made").1;
        if pore_grid.as_ref().is_none_or(|g| pores[g.0] != pores[l]) {
            pore_grid = Some((l, SegGrid::of_regions(&pores[l], |k| k as u32)));
        }
        let in_pores = &pore_grid.as_ref().expect("just made").1;
        for (t, r0) in tips {
            let (x, y) = (t.x_mm(), t.y_mm());
            let id = merged_into.len();
            merged_into.push(id);
            // A tip starts with what room it has of the side gap past xy_min, and grows the rest.
            let off = nearest_within(edges, t, (r0 + side_off) * SCALE).map_or(side_off, |(_, d)| (d / SCALE - r0).clamp(0.0, side_off));
            nodes.push(Node { id, x, y, r: r0, r0, age: 0, alive: true, dropped: false, off, was_into: f64::INFINITY, vx: 0.0, vy: 0.0, was: (x, y, r0), pressed: false, drawn: r0, chain: skeleton.start(l), whole: false });
        }
        // Deterministic order.
        nodes.sort_by(|a, b| (a.x, a.y).partial_cmp(&(b.x, b.y)).unwrap());
        // Drift toward neighbours (slow), or get out from over the model while
        // there is time to. Then clear the model.
        let snapshot: Vec<Node> = nodes.iter().filter(|o| !o.dropped).copied().collect();
        let reach = (r_branch * 6.0).max(6.0);
        for nd in nodes.iter_mut() {
            nd.age += 1;
            nd.was = (nd.x, nd.y, nd.r);
            if nd.dropped {
                continue;
            }
            // Away from the part at the steep angle, pressed or not: a pressed branch narrows to
            // keep it and widens again once it has room. Grow: tip ramp, then the taper, up to
            // the trunk limit, once it has room.
            nd.off = (nd.off + m_fast).min(branch_off);
            if !nd.pressed {
                let tl = ramp(nd.r0);
                let grown = if nd.age < tl { nd.r.max(nd.r0 + (r_branch - nd.r0) * nd.age as f64 / tl as f64) } else { (nd.r + taper).max(r_branch) };
                nd.r = grown.min(r_max);
            }
            if nd.age <= 1 {
                continue;
            }
            let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
            for o in &snapshot {
                let (dx, dy) = (o.x - nd.x, o.y - nd.y);
                let d = (dx * dx + dy * dy).sqrt();
                if d > 1e-9 && d < reach {
                    let wgt = 1.0 / (d * d);
                    sx += dx * wgt;
                    sy += dy * wgt;
                    sw += wgt;
                }
            }
            let (mut wx, mut wy) = (0.0, 0.0);
            // Not from the bed only, over the model: head for the edge of what lies below, when
            // the free layers above the model leave the time to get past it, no faster than that
            // takes. From the bed only it goes straight on down, and moves where its way down
            // makes it (below).
            let under = if p.build_plate_only { None } else { shadow.below(nd.x, nd.y, l) };
            let out = under.and_then(|k| shadow.exit(nd.x, nd.y, l, m_fast * (l - k) as f64 * 0.9).map(|e| (e, l - k))).filter(|((_, _, d), _)| *d > 1e-6);
            // With no way past the part under it, a branch ends on the part where it may: it comes
            // down onto it as close as a tip, instead of riding the slope at the branch gap.
            if under.is_some() && out.is_none() && !p.build_plate_only {
                nd.off = 0.0;
            }
            if let Some(((qx, qy, d), layers)) = out {
                let speed = (1.5 * d / layers as f64).clamp(m_slow, m_fast).min(d + 0.05);
                wx = (qx - nd.x) / d * speed;
                wy = (qy - nd.y) / d * speed;
            } else if sw > 0.0 {
                let (dx, dy) = (sx / sw, sy / sw);
                let dl = (dx * dx + dy * dy).sqrt();
                if dl > 1e-9 {
                    // Tips settle at the preferred angle, grown branches
                    // converge at the steep one. Fitted against reference
                    // output: 0.45 of the angle's move keeps branches apart as
                    // long as the reference's.
                    let speed = if nd.age <= ramp(nd.r0) { m_slow } else { m_fast };
                    let mv = (speed * 0.45).min(dl);
                    wx = dx / dl * mv;
                    wy = dy / dl * mv;
                }
            }
            nd.vx += (wx - nd.vx) * TURN;
            nd.vy += (wy - nd.vy) * TURN;
            let (mx, my) = capped(nd.vx, nd.vy, m_fast - (nd.r - nd.was.2));
            nd.x += mx;
            nd.y += my;
            // From the bed only it keeps to where it gets down to the bed from: where it got there
            // is no such place on this layer, it moves to the nearest within the steep angle, by
            // no more than that, and narrows where only a narrower branch gets down.
            if p.build_plate_only {
                let was = Pt::from_mm(nd.was.0, nd.was.1);
                if let Some((q, r)) = fields.step(was, Pt::from_mm(nd.x, nd.y), l, nd.r, heights.get(l + 1).map_or(h, |z| z.1) * p.angle_max_deg.to_radians().tan()) {
                    nd.x = q.x_mm();
                    nd.y = q.y_mm();
                    nd.r = r;
                }
            }
        }
        // Clear the model: how far each section is into what it keeps off, from its
        // centre's distance to the edge of what keeps support off.
        let tests = |nd: &Node| !nd.dropped && nd.age > 1;
        let z = heights[l].0;
        let foot = FOOT_R - FOOT_SLOPE * (z - z_foot).max(0.0);
        for nd in nodes.iter_mut() {
            // A new tip was sampled clear of the model and does not move yet.
            let mut free = false;
            if tests(nd) {
                nd.pressed = false;
                let pt = Pt::from_mm(nd.x, nd.y);
                let inside = edges.contains_any(pt);
                let keep = nd.r + nd.off;
                if let Some((q, d)) = nearest_within(edges, pt, if inside { f64::INFINITY } else { keep * SCALE }) {
                    let into = keep + if inside { d } else { -d } / SCALE;
                    // Over the model itself it stands there. Too far into what keeps it off, and
                    // not getting out, the model is outrunning it: it drops straight down from
                    // here onto the surface, at the size it has. A branch that is getting out goes
                    // on doing so, and what it keeps off past xy_min only pushes it.
                    let deep = into - nd.off;
                    if deep > (2.0 * m_fast).max(nd.r * 0.5) && deep >= nd.was_into || inside && slices[l].iter().any(|e| e.contains(pt)) {
                        // It no longer moves away from the part, so it drops at the room it has: cut to
                        // what the part leaves round it, it would fill the part's pores all the way
                        // down. From the bed only it narrows at once, and with its centre in what
                        // keeps it off it has nowhere to go and ends there; onto the part it narrows
                        // at the foot's slope, so what it holds stays on it.
                        nd.dropped = true;
                        nd.off = 0.0;
                        let room = keep - into;
                        nd.r = nd.drawn.min(room.max(r_bare));
                        if p.build_plate_only {
                            nd.alive = room > 0.0;
                            nd.drawn = nd.r;
                        }
                    } else {
                        let (dx, dy) = if inside { (q.x - pt.x, q.y - pt.y) } else { (pt.x - q.x, pt.y - q.y) };
                        let dl = (dx as f64).hypot(dy as f64).max(1.0);
                        let push = into + 0.02;
                        let (mx, my) = (nd.x + dx as f64 / dl * push - nd.was.0, nd.y + dy as f64 / dl * push - nd.was.1);
                        let room = m_fast - (nd.r - nd.was.2);
                        nd.pressed = (mx * mx + my * my).sqrt() > room;
                        let (mx, my) = capped(mx, my, room);
                        nd.x = nd.was.0 + mx;
                        nd.y = nd.was.1 + my;
                    }
                    nd.was_into = deep;
                } else {
                    nd.was_into = 0.0;
                    free = true;
                }
            }
            // A dropped branch ends where it lands: once its centre is on the part, what it would
            // draw further down stands beside the part and holds nothing up.
            if nd.dropped && (covered(nd.x, nd.y, nd.r, edges) || slices[l].iter().any(|e| e.contains(Pt::from_mm(nd.x, nd.y)))) {
                nd.alive = false;
            }
            // Foot: a trunk with nothing of the model under it widens over the
            // first layers, and takes the flare's own slope to change its mind.
            let mut section = nd.r;
            if foot > nd.r && !nd.dropped {
                let pt = Pt::from_mm(nd.x, nd.y);
                if !(0..l).any(|k| slices[k].iter().any(|e| e.contains(pt))) {
                    section = foot.min(nd.r + FOOT_MAX_EXTRA);
                }
            }
            // A section narrower than its branch widens at the foot's slope whatever moved it:
            // going down, widening only steps its outline out over what it carries, and a
            // branch that moves all the way (out from over the part, or pushed off it) would
            // otherwise stay as thin as the part once pressed it, for as long as it moves.
            let widen = (FOOT_SLOPE * h).max(nd.r - nd.was.2);
            let was_drawn = nd.drawn;
            nd.drawn = section.clamp(nd.drawn - FOOT_SLOPE * h, nd.drawn + widen);
            // The room it has: its section keeps off as far as the branch does.
            nd.whole = !nd.dropped;
            if !nd.dropped && !(free && nd.drawn <= nd.r) {
                let pt = Pt::from_mm(nd.x, nd.y);
                let centre = if edges.contains_any(pt) { Some(0.0) } else { nearest_within(edges, pt, (nd.drawn + nd.off) * SCALE).map(|(_, d)| d / SCALE) };
                if let Some(centre) = centre {
                    // It grows only into the room it has, and never narrows for want of room: what
                    // it lacks, what keeps support off cuts off its side. Not with its centre in
                    // it, nor with a pore in its reach: cut there, it would fill the pore and lock
                    // its piece in, so there it narrows to the room.
                    let keep = was_drawn.min(nd.drawn);
                    let room = centre - nd.off;
                    let pored = || in_pores.contains_any(pt) || nearest_within(in_pores, pt, keep * SCALE).is_some();
                    let floor = if room < keep && centre > 1e-6 && !pored() { keep } else { r_bare };
                    nd.drawn = nd.drawn.min(room.max(floor));
                    nd.whole = centre >= nd.drawn;
                }
            }
        }
        nodes.retain(|nd| nd.alive);
        // What merged earlier moves onto its branch. A branch that has ended on the
        // model leaves what joined it standing, to end the same way.
        if !joining.is_empty() {
            let mut at = vec![usize::MAX; merged_into.len()];
            for (i, nd) in nodes.iter().enumerate() {
                at[nd.id] = i;
            }
            let mut stranded: Vec<Joining> = Vec::new();
            joining.retain_mut(|g| {
                while merged_into[g.into] != g.into {
                    g.into = merged_into[g.into];
                }
                let Some(s) = nodes.get(at[g.into]) else {
                    stranded.push(*g);
                    return false;
                };
                let shrink = (g.r - s.r.max(s.drawn)).clamp(0.0, m_slow);
                let (mx, my) = capped(s.x - g.x, s.y - g.y, m_fast - shrink);
                g.r -= shrink;
                g.x += mx;
                g.y += my;
                (s.x - g.x).hypot(s.y - g.y) + g.r > s.drawn + INSIDE
            });
            for g in stranded {
                let id = merged_into.len();
                merged_into.push(id);
                nodes.push(Node { id, x: g.x, y: g.y, r: g.r, r0: g.r, age: 0, alive: true, dropped: true, off: 0.0, was_into: 0.0, vx: 0.0, vy: 0.0, was: (g.x, g.y, g.r), pressed: false, drawn: g.r, chain: g.chain, whole: false });
            }
        }
        // Merge overlapping nodes (bigger absorbs smaller); dropped ones stay where
        // they are. The merged branch keeps the larger section (reference trunks
        // follow the taper from the oldest tip, 3.25 mm across 7 mm under a 2 mm
        // roof tip, however many branches joined them), but draws from inside what
        // it replaces.
        let mut merged: Vec<Node> = Vec::with_capacity(nodes.len());
        let mut gone = vec![false; nodes.len()];
        for i in 0..nodes.len() {
            if gone[i] {
                continue;
            }
            let mut a = nodes[i];
            let mut joined = false;
            for j in i + 1..nodes.len() {
                if a.dropped || gone[j] || nodes[j].dropped {
                    continue;
                }
                let b = nodes[j];
                let d = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
                if d < (a.r + b.r) * JOIN {
                    if !joined {
                        joining.push(Joining { x: a.x, y: a.y, r: a.drawn, into: a.id, chain: a.chain });
                        joined = true;
                        // Below a joint the merged branch is a stretch of its own.
                        let fresh = skeleton.start(l);
                        skeleton.chains[a.chain].into = Some(fresh);
                        a.chain = fresh;
                    }
                    joining.push(Joining { x: b.x, y: b.y, r: b.drawn, into: a.id, chain: b.chain });
                    skeleton.chains[b.chain].into = Some(a.chain);
                    merged_into[b.id] = a.id;
                    let (wa, wb) = (a.r * a.r, b.r * b.r);
                    let (x, y) = ((a.x * wa + b.x * wb) / (wa + wb), (a.y * wa + b.y * wb) / (wa + wb));
                    a.drawn = (a.drawn - (x - a.x).hypot(y - a.y)).max(b.drawn - (x - b.x).hypot(y - b.y)).max(0.05);
                    a.vx = (a.vx * wa + b.vx * wb) / (wa + wb);
                    a.vy = (a.vy * wa + b.vy * wb) / (wa + wb);
                    a.x = x;
                    a.y = y;
                    a.whole = false;
                    a.off = a.off.max(b.off);
                    a.r = a.r.max(b.r);
                    a.r0 = a.r0.max(b.r0);
                    a.age = a.age.max(b.age);
                    gone[j] = true;
                }
            }
            merged.push(a);
        }
        nodes = merged;
        sections[l].extend(nodes.iter().map(|nd| Drawn { s: (nd.x, nd.y, nd.drawn), chain: nd.chain }));
        sections[l].extend(joining.iter().map(|g| Drawn { s: (g.x, g.y, g.r), chain: g.chain }));
        whole.resize(skeleton.chains.len(), Vec::new());
        for nd in &nodes {
            skeleton.chains[nd.chain].sections.push((nd.x, nd.y, nd.drawn));
            whole[nd.chain].push(nd.whole);
        }
        for g in &joining {
            skeleton.chains[g.chain].sections.push((g.x, g.y, g.r));
            whole[g.chain].push(false);
        }
    }
    skeleton.close = p.line_width * CLOSE_WIDTHS;
    skeleton.neck = p.line_width * NECK_WIDTHS;
    // Areas per layer: branches minus what keeps support off minus roofs.
    let draw = |l: usize, drawn: &[Drawn], interface: &[ExPolygon]| -> Vec<ExPolygon> {
        if drawn.is_empty() && pads[l].is_empty() {
            return Vec::new();
        }
        let close = p.line_width * CLOSE_WIDTHS;
        let circles: Vec<(f64, f64, f64)> = drawn.iter().map(|d| d.s).collect();
        let mut u = if circles.is_empty() { Vec::new() } else { sections_outline(&circles, close, p.line_width * NECK_WIDTHS) };
        if !pads[l].is_empty() {
            // Blended into the sections as they blend with each other.
            u.extend(pads[l].iter().cloned());
            u = offset_ex(&offset_ex(&u, close * SCALE, Join::Round(0.01 * SCALE)), -close * SCALE, Join::Round(0.01 * SCALE));
        }
        let mut base = difference_ex(&u, &keep[l]);
        if !interface.is_empty() {
            base = difference_ex(&base, interface);
        }
        // Only what the cuts left needs opening: a neck between sections holds.
        let (cut, whole): (Vec<ExPolygon>, Vec<ExPolygon>) = base.into_iter().partition(|e| !e.holes.is_empty() || has_corner(e));
        base = whole;
        if !cut.is_empty() {
            base.extend(opened(&cut, p.line_width * OPEN_WIDTHS * SCALE));
        }
        if l == 0 && p.first_expansion > 0.0 {
            let grown = offset_ex(&base, (p.first_expansion + close) * SCALE, Join::Round(0.01 * SCALE));
            // The flare keeps as far off the part as the branches do.
            base = difference_ex(&offset_ex(&grown, -close * SCALE, Join::Round(0.01 * SCALE)), &offset_ex(&keep[0], branch_off * SCALE, Join::Miter(3.0)));
        }
        base.retain(|e| e.area_mm2() > 0.2);
        base
    };
    ctl.phase("drawing branches", None, AT_DRAWING);
    let mut out = crate::par::try_map_vec(ctl, out, |l, mut layer| {
        layer.interface = union_ex(&layer.interface);
        layer.base = draw(l, &sections[l], &layer.interface);
        layer
    })?;
    // What stands. Most plates have nothing to take away: all layers are checked at once against
    // the layer below as drawn, and only a layer that fails, or whose layer below changed, is
    // gone through again, going up, with what is left under it.
    let stand = Stand { slices: &rests_on, keep: &keep, reach, bed_only: p.build_plate_only, chains: &skeleton.chains, whole: &whole };
    // A roof keeps what the layer below carries: the roof under it, and the support under it
    // within [`CARRIED`] of a line. Reaching past the roof below too would let each of its layers
    // reach further out, and a branch narrowed by the part, or blocked, carries only that much of
    // the roof it was under.
    let reach_out = (CARRIED * p.line_width).min(m_fast);
    let roof_held = |layer: &SupportLayer, below: &SupportLayer| -> Option<Vec<ExPolygon>> {
        if layer.interface.is_empty() {
            return None;
        }
        let mut holds = offset_ex(&below.base, reach_out * SCALE, Join::Miter(3.0));
        holds.extend(below.interface.iter().cloned());
        let mut left = intersection_ex(&layer.interface, &union_ex(&holds));
        left.retain(|e| e.area_mm2() >= SPECK);
        (total_area_mm2(&left) < total_area_mm2(&layer.interface) - SPECK).then_some(left)
    };
    // No layer reaches past what prints under it (the support, or the part it may stand on) by
    // more than the steep angle: cut by the part's gap, it grows back a layer's reach at a time.
    // And nothing of it is narrower than two lines but on what it lies on (see
    // [`crate::support_rules`]).
    // The part a layer may stand on, near `of`: none from the bed only.
    let part_under = |l: usize, of: &[ExPolygon]| -> Vec<ExPolygon> {
        if p.build_plate_only || l == 0 {
            return Vec::new();
        }
        crate::support_rules::near_to((l - reach.min(l)..l).map(|k| &rests_on[k][..]), of, m_fast + crate::support_rules::BRIDGE * p.line_width + 0.5)
    };
    let contain = |l: usize, layer: &SupportLayer, below: &SupportLayer| -> Option<(Vec<ExPolygon>, Vec<ExPolygon>)> {
        if layer.base.is_empty() && layer.interface.is_empty() {
            return None;
        }
        let part = part_under(l, &[layer.base.clone(), layer.interface.clone()].concat());
        let mut on = below.base.clone();
        on.extend(below.interface.iter().cloned());
        on.extend(part.iter().cloned());
        let kept: Vec<ExPolygon> = if on.is_empty() { Vec::new() } else { intersection_ex(&layer.base, &crate::support_rules::carried(&union_ex(&on), m_fast, p.line_width)) };
        let wide = crate::support_rules::wide(&union_ex(&[kept.clone(), layer.interface.clone()].concat()), &part, p.line_width);
        // The base is the rest of what is wide, so it meets the interface edge to edge. Cut to it
        // on its own, the base keeps a seam against the interface, or loses a speck of interface
        // it leant on, and is narrower than two lines there.
        let interface: Vec<ExPolygon> = intersection_ex(&layer.interface, &wide).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
        let base: Vec<ExPolygon> = difference_ex(&wide, &interface).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
        (total_area_mm2(&base) < total_area_mm2(&layer.base) - 1e-3 || total_area_mm2(&interface) < total_area_mm2(&layer.interface) - 1e-3).then_some((base, interface))
    };
    let step = crate::par::Step::new(ctl, "checking what stands", AT_CHECK, AT_CHECK_UP, n);
    let tick = |k: usize| step.tick(k);
    let fine = crate::par::try_map(crate::par::Ctl { tick: Some(&tick), ..ctl }, n, |l| l == 0 || stand.layer(l, &sections[l], &out[l], &out[l - 1], &[]).is_none() && roof_held(&out[l], &out[l - 1]).is_none() && contain(l, &out[l], &out[l - 1]).is_none())?;
    let mut changed = vec![false; n];
    let mut gone: Vec<Vec<bool>> = Vec::new();
    for l in 1..n {
        if fine[l] && !changed[l - 1] {
            continue;
        }
        if ctl.cancelled() { return Err(crate::par::Cancelled); }
        ctl.phase("standing it up", Some((l as u32, n as u32)), AT_CHECK_UP + (AT_IDLE - AT_CHECK_UP) * l as f64 / n as f64);
        let roof = roof_held(&out[l], &out[l - 1]);
        let cut = stand.layer(l, &sections[l], &out[l], &out[l - 1], &gone);
        if cut.is_none() && roof.is_none() {
            if let Some((base, interface)) = contain(l, &out[l], &out[l - 1]) {
                (out[l].base, out[l].interface) = (base, interface);
                changed[l] = true;
            }
            continue;
        }
        if gone.is_empty() {
            gone = skeleton.chains.iter().map(|c| vec![false; c.sections.len()]).collect();
        }
        changed[l] = true;
        let mut drawn_again = false;
        if let Some(roof) = roof {
            out[l].interface = roof;
            drawn_again = true;
        }
        if let Some(cut) = cut {
            let mut k = 0;
            sections[l].retain(|d| {
                let held = cut[k];
                k += 1;
                if !held {
                    gone[d.chain][skeleton.chains[d.chain].top - l] = true;
                    drawn_again = true;
                }
                held
            });
        }
        if drawn_again {
            out[l].base = draw(l, &sections[l], &out[l].interface);
        }
        // The islands that still hang go whole.
        let over = stand.below(&out[l - 1]);
        let hanging = stand.hanging(l, &out[l], &out[l - 1], &over);
        if !hanging.is_empty() {
            let within = |e: &ExPolygon| hanging.iter().any(|g| g.bbox().overlaps(&e.bbox()) && total_area_mm2(&intersection_ex(std::slice::from_ref(e), std::slice::from_ref(g))) > 0.0);
            out[l].base.retain(|e| !within(e));
            out[l].interface.retain(|e| !within(e));
            sections[l].retain(|d| {
                let inside = hanging.iter().any(|g| g.contains(Pt::from_mm(d.s.0, d.s.1)));
                if inside {
                    gone[d.chain][skeleton.chains[d.chain].top - l] = true;
                }
                !inside
            });
        }
        if let Some((base, interface)) = contain(l, &out[l], &out[l - 1]) {
            (out[l].base, out[l].interface) = (base, interface);
        }
    }
    // Nothing prints that holds nothing up: a branch whose top stops short of the part, and what a
    // branch left standing on the bed after the rest of it was taken away, go whole.
    // A top holds what lies over it a little aside, as a slope over it does (see
    // [`crate::support_rules::ASIDE`]).
    ctl.phase("taking away what holds nothing", None, AT_IDLE);
    let idle = unheld(ctl, &out, &wants, gap_top + 1, crate::support_rules::ASIDE)?;
    for (l, idle) in idle.into_iter().enumerate() {
        if idle.is_empty() {
            continue;
        }
        if gone.is_empty() {
            gone = skeleton.chains.iter().map(|c| vec![false; c.sections.len()]).collect();
        }
        let within = |e: &ExPolygon| idle.iter().any(|g| g.bbox().overlaps(&e.bbox()) && total_area_mm2(&intersection_ex(std::slice::from_ref(e), std::slice::from_ref(g))) > 0.0);
        out[l].base.retain(|e| !within(e));
        out[l].interface.retain(|e| !within(e));
    }
    // Nor does any of it top out holding nothing: going down, a layer keeps the islands that carry
    // what it keeps over them or reach the part over them, and of their tops what less a line
    // lies in [`crate::support_rules::holds_band`]. Most layers keep all of themselves; only
    // those, and the layers under one that lost some, are gone through.
    ctl.phase("trimming tops", None, AT_TRIM);
    let w = p.line_width;
    let printed = crate::par::try_map(ctl, n, |l| union_ex(&[out[l].base.clone(), out[l].interface.clone()].concat()))?;
    // The part over each layer and its band, made only for the layers that ask.
    let over: Vec<std::sync::OnceLock<Vec<ExPolygon>>> = (0..n).map(|_| std::sync::OnceLock::new()).collect();
    let band: Vec<std::sync::OnceLock<Vec<ExPolygon>>> = (0..n).map(|_| std::sync::OnceLock::new()).collect();
    let over_of = |l: usize| over[l].get_or_init(|| if printed[l].is_empty() || l + 1 >= n { Vec::new() } else { crate::support_rules::part_over(slices, l, &printed[l], gap_top, w) });
    let band_of = |l: usize| band[l].get_or_init(|| crate::support_rules::holds_band(over_of(l), w));
    let meets = |e: &ExPolygon, set: &[ExPolygon]| { let bb = e.bbox(); set.iter().any(|a| a.bbox().overlaps(&bb)) && !intersection_ex(std::slice::from_ref(e), set).is_empty() };
    let holds = |e: &ExPolygon, l: usize, above: &[ExPolygon]| meets(e, above) || crate::support_rules::reaches(e, over_of(l));
    // What of the top under `above`, less a line, lies past the band. A top less a line is first
    // looked for under `above` itself, which most layers have none of.
    let past = |l: usize, of: &[ExPolygon], above: &[ExPolygon]| -> Vec<ExPolygon> {
        let mut less = crate::support_rules::less_a_line(&if above.is_empty() { of.to_vec() } else { difference_ex(of, above) }, w);
        if !less.is_empty() && !above.is_empty() {
            less = crate::support_rules::less_a_line(&difference_ex(of, &crate::support_rules::covers(above, w)), w);
        }
        if less.is_empty() {
            return less;
        }
        difference_ex(&less, band_of(l)).into_iter().filter(|e| e.area_mm2() >= crate::support_rules::SLACK).collect()
    };
    // A layer keeps what lies within two lines of `above`, and within a line of the band (so a top
    // less a line lies in it, but where the part's outline closes it in).
    let window = |l: usize, above: &[ExPolygon]| -> Vec<ExPolygon> {
        let mut keep = if above.is_empty() { Vec::new() } else { offset_ex(above, (2.0 * w - 0.05) * SCALE, Join::Round(0.05 * SCALE)) };
        if !band_of(l).is_empty() {
            keep.extend(offset_ex(band_of(l), (w - 0.05) * SCALE, Join::Miter(3.0)));
        }
        keep
    };
    // Whether a layer loses any of itself under `above`: an island that holds nothing, or a top
    // past the band.
    let loses = |l: usize, above: &[ExPolygon]| printed[l].iter().any(|e| !holds(e, l, above)) || !past(l, &printed[l], above).is_empty();
    // What is left keeps two lines' width, as everything does, and holds.
    let keep = |l: usize, above: &[ExPolygon]| -> Vec<ExPolygon> {
        let held: Vec<ExPolygon> = printed[l].iter().filter(|e| holds(e, l, above)).cloned().collect();
        let mut kept = if held.is_empty() { held } else { intersection_ex(&held, &union_ex(&window(l, above))) };
        // What is past goes with a line round it, but not what carries `above` or lies within two
        // lines of it (less a line, that lies in nothing).
        let past = past(l, &kept, above);
        if !past.is_empty() {
            let cut = offset_ex(&past, (w + 0.02) * SCALE, Join::Round(0.01 * SCALE));
            let cut = if above.is_empty() { cut } else { difference_ex(&cut, &offset_ex(above, (2.0 * w - 0.05) * SCALE, Join::Round(0.05 * SCALE))) };
            kept = difference_ex(&kept, &cut);
        }
        let mut kept = crate::support_rules::wide(&kept, &part_under(l, &kept), w);
        kept.retain(|e| e.area_mm2() >= SPECK && holds(e, l, above));
        kept
    };
    let trims = crate::par::try_map(ctl, n, |l| l > 0 && l + 1 < n && !printed[l].is_empty() && loses(l, &printed[l + 1]))?;
    let mut above_cut: Option<Vec<ExPolygon>> = None;
    let mut cut = vec![false; n];
    for l in (1..n.saturating_sub(1)).rev() {
        if ctl.cancelled() { return Err(crate::par::Cancelled); }
        ctl.phase("trimming tops", Some(((n - l) as u32, n as u32)), AT_TRIM + (AT_TRIM_UP - AT_TRIM) * (n - l) as f64 / n as f64);
        let above = above_cut.as_deref().unwrap_or(&printed[l + 1]);
        if !(trims[l] || above_cut.is_some()) || !loses(l, above) {
            above_cut = None;
            continue;
        }
        let kept = keep(l, above);
        // The base is the rest of what is kept, so it meets the interface edge to edge: each cut
        // to it on its own leaves a hairline seam between the two that is not two lines wide.
        let interface: Vec<ExPolygon> = intersection_ex(&out[l].interface, &kept).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
        out[l].base = difference_ex(&kept, &interface).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
        out[l].interface = interface;
        above_cut = Some(kept);
        cut[l] = true;
    }
    // What the trim takes can leave the layer over it past the steep angle of what is left: a top
    // that held nothing can still have been one side of a gap the layer over it bridged (see
    // [`crate::support_rules::carried`]). Going up from each layer it cut, what of a layer lies past
    // what is left under it, as the rules count it, goes, and the layer over that is looked at too.
    for l in 1..n {
        if ctl.cancelled() { return Err(crate::par::Cancelled); }
        ctl.phase("checking what the trim left", Some((l as u32 + 1, n as u32)), AT_TRIM_UP + (AT_TELL - AT_TRIM_UP) * l as f64 / n as f64);
        if !cut[l - 1] || (out[l].base.is_empty() && out[l].interface.is_empty()) {
            continue;
        }
        let here = union_ex(&[out[l].base.clone(), out[l].interface.clone()].concat());
        let part = part_under(l, &here);
        let mut on = out[l - 1].base.clone();
        on.extend(out[l - 1].interface.iter().cloned());
        on.extend(part.iter().cloned());
        let held = crate::support_rules::carried(&union_ex(&on), m_fast + 0.01, w);
        let past: Vec<ExPolygon> = difference_ex(&here, &held).into_iter().filter(|e| e.area_mm2() >= crate::support_rules::SLACK).collect();
        if past.is_empty() {
            continue;
        }
        let wide = crate::support_rules::wide(&difference_ex(&here, &past), &part, w);
        out[l].interface = intersection_ex(&out[l].interface, &wide).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
        out[l].base = difference_ex(&wide, &out[l].interface).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
        cut[l] = true;
    }
    // No foot without a support standing on it: on the first layer, what holds nothing and
    // carries nothing printed over it goes.
    if n > 1 && !printed[0].is_empty() {
        let over: Vec<ExPolygon> = [out[1].base.clone(), out[1].interface.clone()].concat();
        out[0].base.retain(|e| holds(e, 0, &over));
        out[0].interface.retain(|e| holds(e, 0, &over));
    }
    // The skeleton is what prints: a section with nothing of it printed (inside what keeps support
    // off, or gone with what held nothing) goes from it, so no piece is made of it.
    let blank = crate::par::try_map(ctl, n, |l| {
        if sections[l].is_empty() {
            return Vec::new();
        }
        let printed = SegGrid::of_regions(&[out[l].base.clone(), out[l].interface.clone()].concat(), |k| k as u32);
        sections[l].iter().filter(|d| !prints(&printed, d.s)).map(|d| d.chain).collect::<Vec<usize>>()
    })?;
    if blank.iter().any(|b| !b.is_empty()) && gone.is_empty() {
        gone = skeleton.chains.iter().map(|c| vec![false; c.sections.len()]).collect();
    }
    for (l, b) in blank.iter().enumerate() {
        for &c in b {
            gone[c][skeleton.chains[c].top - l] = true;
        }
    }
    if !gone.is_empty() {
        skeleton.remove_sections(&gone);
    }
    ctl.phase("telling what has none", None, AT_TELL);
    // What has no support under it is told, once: what no trunk got to and what was taken away
    // alike.
    let reach = spacing_of(r_bare) / 2.0;
    if paint.is_empty() {
        skeleton.given_up = not_reached(ctl, &open, &out, gap_top, reach)?;
    } else {
        // What the inside rule told is not the paint's to tell again.
        let added = without(&added, &lost);
        let answers_for = paint.kept_away(ctl, &open, &added, p.interface_top_layers + gap_bottom + gap_top + 3, reach)?;
        let rest: Vec<Vec<ExPolygon>> = open.iter().zip(&answers_for).map(|(d, a)| if a.is_empty() { d.clone() } else { difference_ex(d, a) }).collect();
        skeleton.given_up = not_reached(ctl, &answers_for, &out, gap_top, reach)?;
        skeleton.given_up.extend(not_reached(ctl, &rest, &out, gap_top, reach)?);
        skeleton.given_up.sort_by_key(|g| g.layer);
    }
    // Where the part lets its supports be locked in, what of them is: told as that.
    if p.stand_inside {
        skeleton.unremovable = crate::par::try_map(ctl, n, |l| {
            if locked[l].is_empty() || out[l].base.is_empty() && out[l].interface.is_empty() {
                return None;
            }
            let held: Vec<ExPolygon> = intersection_ex(&[out[l].base.clone(), out[l].interface.clone()].concat(), &locked[l]).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
            GivenUp::of(l, &held)
        })?.into_iter().flatten().collect();
    }
    // The bed layer as it prints: its foot joins what stands on it.
    skeleton.foot = out.first().map(|l| union_ex(&l.base)).unwrap_or_default();
    // The pads as they print, with whatever took part of them away, and joined where their outline
    // closes them together.
    let close = p.line_width * CLOSE_WIDTHS;
    skeleton.pads = crate::par::try_map(ctl, n, |l| if pads[l].is_empty() || out[l].base.is_empty() { Vec::new() } else { intersection_ex(&offset_ex(&pads[l], close * SCALE, Join::Round(0.01 * SCALE)), &out[l].base) })?;
    // The support's top prints solid, on a solid layer, as does what a roof lies on; the rest of
    // the warp under it is filled sparse.
    let filled = crate::par::try_map(ctl, n, |l| {
        if out[l].base.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let mut on: Vec<ExPolygon> = tops[l].clone();
        if let Some(up) = out.get(l + 1) {
            on.extend(up.interface.iter().cloned());
            on.extend(tops[l + 1].iter().cloned());
        }
        let solid = if on.is_empty() { Vec::new() } else { intersection_ex(&out[l].base, &union_ex(&on)) };
        let inside = if pads[l].is_empty() { Vec::new() } else { difference_ex(&intersection_ex(&out[l].base, &pads[l]), &solid) };
        (solid, inside)
    })?;
    for (layer, (solid, inside)) in out.iter_mut().zip(filled) {
        layer.solid = solid;
        layer.inside = inside;
    }
    Ok(Tree { layers: out, rings, asked: wanted })
}

/// Each layer's outline with what it encloses: closed across gaps up to [`INSIDE_GAP`] wide, its
/// holes filled. Except under an island with nothing below it, or no more than a speck, down to
/// the bed and `room` (mm) round it: that island would print on air, so its branch may stand
/// inside.
pub(crate) fn with_inside(ctl: crate::par::Ctl, slices: &[Vec<ExPolygon>], room: f64) -> Result<Vec<Vec<ExPolygon>>, crate::par::Cancelled> {
    let (by, round) = (INSIDE_GAP / 2.0 * SCALE, Join::Round(0.05 * SCALE));
    let n = slices.len();
    let under_floats = under_floats(ctl, slices)?;
    crate::par::try_map(ctl, n, |l| {
        let closed = offset_ex(&offset_ex(&slices[l], by, round), -by, round);
        let mut filled: Vec<ExPolygon> = closed.into_iter().map(|e| ExPolygon::new(e.contour)).collect();
        if !under_floats[l].is_empty() {
            filled = difference_ex(&filled, &offset_ex(&under_floats[l], room * SCALE, round));
        }
        // Past its own outline only what is more than the closing's rounding.
        let extra: Vec<ExPolygon> = difference_ex(&filled, &slices[l]).into_iter().filter(|e| !offset_ex(std::slice::from_ref(e), -INSIDE_SLIVER * SCALE, Join::Miter(3.0)).is_empty()).collect();
        if extra.is_empty() { slices[l].clone() } else { union_ex(&[extra, slices[l].clone()].concat()) }
    })
}

/// Per layer, the islands over it, on any layer above, with nothing below them (or no more than a
/// speck): they would print on air.
fn under_floats(ctl: crate::par::Ctl, slices: &[Vec<ExPolygon>]) -> Result<Vec<Vec<ExPolygon>>, crate::par::Cancelled> {
    let n = slices.len();
    // An island that touches the layer below by less than a speck prints on air all the same.
    let floats = crate::par::try_map(ctl, n, |l| {
        if l == 0 {
            return Vec::new();
        }
        slices[l].iter().filter(|e| {
            let b = e.bbox();
            let near: Vec<ExPolygon> = slices[l - 1].iter().filter(|x| x.bbox().overlaps(&b)).cloned().collect();
            e.area_mm2() >= SPECK && (near.is_empty() || total_area_mm2(&intersection_ex(std::slice::from_ref(e), &near)) < SPECK)
        }).cloned().collect::<Vec<ExPolygon>>()
    })?;
    let mut under_floats: Vec<Vec<ExPolygon>> = vec![Vec::new(); n];
    for l in (1..n).rev() {
        if ctl.cancelled() { return Err(crate::par::Cancelled); }
        under_floats[l - 1] = if floats[l].is_empty() { under_floats[l].clone() } else { union_ex(&[under_floats[l].clone(), floats[l].clone()].concat()) };
    }
    Ok(under_floats)
}

/// The window (mm) a support is taken to come out of the part through, once printed, as `p`
/// plans it: what has no way to the open as wide is locked in. The thinnest column's width
/// ([`SupportParams::column`]): what not even that gets out of is locked in for every route.
pub(crate) fn window(p: &SupportParams) -> f64 {
    2.0 * p.column()
}

/// What the locked-in room was made from, for the part's next run.
pub(crate) struct Locked {
    frame: BBox,
    fits: Vec<Arc<Vec<ExPolygon>>>,
    out: Vec<Vec<bool>>,
    under: Vec<Vec<ExPolygon>>,
    pub(crate) locked: Vec<Vec<ExPolygon>>,
}

/// Per layer, where a support would be locked in the part: the room round the part that a disc
/// `w` (mm) wide does not get to from the open, moving from layer to layer up or down and
/// within a layer through any room it fits (so through windows at least `w` wide), and does
/// not cover from there. Not under an island with nothing below it, down to the bed and `room`
/// (mm) round it: that island would print on air, so its branch may stand there. `keep` is what
/// the support keeps off, per layer.
pub(crate) fn locked_in(ctl: crate::par::Ctl, keep: &[&[ExPolygon]], taken: &[bool], before: Option<&Locked>, slices: &[Vec<ExPolygon>], w: f64, room: f64) -> Result<Locked, crate::par::Cancelled> {
    let n = keep.len();
    let mut all = BBox::empty();
    for k in keep {
        for e in *k {
            all.merge(&e.bbox());
        }
    }
    if all.is_empty() {
        return Ok(Locked { frame: all, fits: Vec::new(), out: Vec::new(), under: Vec::new(), locked: vec![Vec::new(); n] });
    }
    let pad = ((w + 1.0) * SCALE) as i64;
    let frame = BBox { min: Pt::new(all.min.x - pad, all.min.y - pad), max: Pt::new(all.max.x + pad, all.max.y + pad) };
    let before = before.filter(|b| b.frame == frame);
    let was = |l: usize| before.filter(|b| taken.get(l) == Some(&true) && l < b.fits.len());
    let round = Join::Round(0.05 * SCALE);
    let r = w / 2.0 * SCALE;
    let rect = ExPolygon::new(Polygon::rect(frame.min, frame.max));
    // Per layer, where a disc's centre fits, by piece: the run before's where its keep is.
    let fits = crate::par::try_map(ctl, n, |l| match was(l) {
        Some(b) => b.fits[l].clone(),
        None => Arc::new(offset_ex(&difference_ex(std::slice::from_ref(&rect), keep[l]), -r, round)),
    })?;
    // From the open: what reaches the frame, and the top layer (open above).
    let edge = |e: &ExPolygon| { let b = e.bbox(); b.min.x <= all.min.x || b.min.y <= all.min.y || b.max.x >= all.max.x || b.max.y >= all.max.y };
    let mut out: Vec<Vec<bool>> = (0..n).map(|l| fits[l].iter().map(|e| l + 1 == n || edge(e)).collect()).collect();
    let meets = |a: &ExPolygon, b: &ExPolygon| a.bbox().overlaps(&b.bbox()) && !intersection_ex(std::slice::from_ref(a), std::slice::from_ref(b)).is_empty();
    // Up and down, until nothing more gets out.
    loop {
        let mut changed = false;
        for pass in 0..2 {
            let order: Vec<usize> = if pass == 0 { (1..n).collect() } else { (0..n.saturating_sub(1)).rev().collect() };
            for l in order {
                if ctl.cancelled() { return Err(crate::par::Cancelled); }
                let o = if pass == 0 { l - 1 } else { l + 1 };
                for i in 0..fits[l].len() {
                    if out[l][i] {
                        continue;
                    }
                    if (0..fits[o].len()).any(|j| out[o][j] && meets(&fits[l][i], &fits[o][j])) {
                        out[l][i] = true;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    let under = under_floats(ctl, slices)?;
    let locked = crate::par::try_map(ctl, n, |l| {
        if let Some(b) = was(l).filter(|b| b.out[l] == out[l] && b.under[l] == under[l]) {
            return b.locked[l].clone();
        }
        let free = difference_ex(std::slice::from_ref(&rect), keep[l]);
        let got: Vec<ExPolygon> = fits[l].iter().zip(&out[l]).filter(|(_, o)| **o).map(|(e, _)| e.clone()).collect();
        let reached = if got.is_empty() { got } else { offset_ex(&got, r, round) };
        let shut = if reached.is_empty() { free } else { difference_ex(&free, &reached) };
        let shut = opened(&shut, 0.05 * SCALE);
        if shut.is_empty() || under[l].is_empty() { shut } else { difference_ex(&shut, &offset_ex(&under[l], room * SCALE, round)) }
    })?;
    Ok(Locked { frame, fits, out, under, locked })
}

/// Where the overhang support holds would close round what is right under it, and the part
/// widens again below so the ring can't slide off (a lip round a vase's neck, a cap round a stem
/// off a wider foot): [`RING_ARCS`] thin walls (`width`, mm) from the middle of what it closes
/// round out across the overhang and `reach` (mm) past it, per layer from the overhang's own
/// down to the lowest the part within its box goes (under that the ring would close round
/// nothing), for the clearance. They lengthen by `lean` (mm) a layer, a branch's steepest (none for
/// normal support's columns), so nothing that holds the overhang gets round their ends. Its roof prints in arcs, and what holds each
/// stays apart from the next, so each comes off on its own.
pub(crate) fn ring_walls(ctl: crate::par::Ctl, demand: &[Vec<ExPolygon>], clearance: &[Vec<ExPolygon>], slices: &[Vec<ExPolygon>], gap_top: usize, width: f64, reach: f64, lean: f64) -> Result<Vec<Vec<ExPolygon>>, crate::par::Cancelled> {
    let n = slices.len();
    // Per layer, the walls' centres and lengths (mm) where a ring starts on it.
    let starts = crate::par::try_map(ctl, n, |l| {
        let src = l + 1 + gap_top;
        let mut found: Vec<(Pt, f64, usize)> = Vec::new();
        let held = match demand.get(src) { Some(d) if !d.is_empty() => difference_ex(d, &clearance[l]), _ => Vec::new() };
        for roof in &held {
            let bb = roof.bbox();
            // What it closes round, taken as one: the pieces inside it that it wraps.
            let mut round: Option<BBox> = None;
            for piece in &slices[l] {
                let pb = piece.bbox();
                if pb.min.x < bb.min.x || pb.max.x > bb.max.x || pb.min.y < bb.min.y || pb.max.y > bb.max.y || wrap_deg(roof, pb.center()) <= WRAP_MOST {
                    continue;
                }
                round = Some(round.map_or(pb, |mut r| { r.merge(&pb); r }));
            }
            // What it closes round may reach out past it (a stem joined to a rib through its
            // gap): then the middle of its hole, or of itself, where that is the part's material.
            let in_part = |c: Pt| slices[l].iter().any(|e| e.bbox().overlaps(&BBox { min: c, max: c }) && e.contains(c));
            let Some(c) = round.map(|r| r.center()).filter(|&c| wrap_deg(roof, c) > WRAP_MOST)
                .or_else(|| roof.holes.iter().map(|h| h.bbox().center()).chain(std::iter::once(bb.center())).find(|&c| wrap_deg(roof, c) > WRAP_MOST && in_part(c))) else { continue };
            // Off a straight stem it slides down; over what widens below it is held.
            if (0..l).rev().any(|k| slices[k].iter().any(|e| e.bbox().overlaps(&bb)) && total_area_mm2(&intersection_ex(&slices[k], std::slice::from_ref(roof))) > SPECK) {
                let far = roof.contour.pts.iter().map(|q| q.dist(c)).fold(0.0, f64::max) / SCALE + reach;
                // It closes round the part only down to where the part within its box ends (the
                // box, as what it wraps may lie in a gap of its outline): under that it wraps
                // nothing, so its walls stop there and keep no support off what else of the part
                // is over them.
                let within = [ExPolygon::new(Polygon::rect(bb.min, bb.max))];
                let wraps = |k: usize| slices[k].iter().any(|e| e.bbox().overlaps(&bb)) && total_area_mm2(&intersection_ex(&slices[k], &within)) > SPECK;
                let mut bottom = l;
                while bottom > 0 && wraps(bottom - 1) {
                    bottom -= 1;
                }
                found.push((c, far, bottom));
            }
        }
        found
    })?;
    // Per ring: the layer it starts on, its centre, its walls' length there, and the lowest layer
    // it closes round the part on.
    let mut live: Vec<(usize, Pt, f64, usize)> = Vec::new();
    let mut at_layer: Vec<Vec<(Pt, f64)>> = vec![Vec::new(); n];
    for l in (0..n).rev() {
        for &(c, far, bottom) in &starts[l] {
            // A lip a layer at a time starts a ring on each: one set of walls holds them all.
            match live.iter_mut().find(|&&mut (from, at, was, _)| at.dist(c) < width * SCALE && was + (from - l) as f64 * lean >= far) {
                Some(ring) => ring.3 = ring.3.min(bottom),
                None => live.push((l, c, far, bottom)),
            }
        }
        at_layer[l] = live.iter().filter(|r| l >= r.3).map(|&(from, c, far, _)| (c, far + (from - l) as f64 * lean)).collect();
    }
    crate::par::try_map(ctl, n, |l| {
        let strips: Vec<ExPolygon> = at_layer[l].iter().flat_map(|&(c, far)| {
            (0..RING_ARCS).map(move |k| {
                let a = (90.0 + 360.0 / RING_ARCS as f64 * k as f64).to_radians();
                let (dx, dy) = (a.cos(), a.sin());
                let (nx, ny) = (-dy * width / 2.0, dx * width / 2.0);
                let (cx, cy) = (c.x_mm(), c.y_mm());
                let at = |along: f64, side: f64| Pt::from_mm(cx + dx * along + nx * side, cy + dy * along + ny * side);
                ExPolygon::new(Polygon::new(vec![at(0.0, -1.0), at(far, -1.0), at(far, 1.0), at(0.0, 1.0)]))
            })
        }).collect();
        if strips.is_empty() { Vec::new() } else { union_ex(&strips) }
    })
}

/// How far round `c` (degrees) `region` reaches, to 5°: the turns its outline's edges sweep.
fn wrap_deg(region: &ExPolygon, c: Pt) -> f64 {
    const BINS: usize = 72;
    let mut hit = [false; BINS];
    let bin = |a: f64| ((a.rem_euclid(std::f64::consts::TAU) / std::f64::consts::TAU * BINS as f64) as usize).min(BINS - 1);
    for ring in region.rings() {
        let n = ring.pts.len();
        for i in 0..n {
            let (p, q) = (ring.pts[i], ring.pts[(i + 1) % n]);
            let a0 = ((p.y - c.y) as f64).atan2((p.x - c.x) as f64);
            let mut sweep = ((q.y - c.y) as f64).atan2((q.x - c.x) as f64) - a0;
            if sweep > std::f64::consts::PI { sweep -= std::f64::consts::TAU; }
            if sweep < -std::f64::consts::PI { sweep += std::f64::consts::TAU; }
            let steps = (sweep.abs() / std::f64::consts::TAU * BINS as f64).ceil() as usize + 1;
            for k in 0..=steps {
                hit[bin(a0 + sweep * k as f64 / steps as f64)] = true;
            }
        }
    }
    hit.iter().filter(|h| **h).count() as f64 * 360.0 / BINS as f64
}

/// What of `wanted`, the overhang per model layer the paint answers for, no support comes near:
/// no room for a tip, or what the tip grew into could not stand on anything.
fn not_reached(ctl: crate::par::Ctl, wanted: &[Vec<ExPolygon>], out: &[SupportLayer], gap_top: usize, reach: f64) -> Result<Vec<GivenUp>, crate::par::Cancelled> {
    let found = crate::par::try_map(ctl, wanted.len(), |src| {
        if wanted[src].is_empty() {
            return None;
        }
        // A thin tip's first layers can be clipped away: it starts a little lower down.
        let top = src.checked_sub(1 + gap_top);
        let under: Vec<ExPolygon> = top.map_or(Vec::new(), |top| (top.saturating_sub(TIP_CLIPPED)..=top).flat_map(|k| out[k].base.iter().chain(&out[k].interface).cloned()).collect());
        let near = offset_ex(&union_ex(&under), reach * SCALE, Join::Round(0.05 * SCALE));
        let left: Vec<ExPolygon> = difference_ex(&wanted[src], &near).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
        GivenUp::of(src, &left)
    })?;
    Ok(found.into_iter().flatten().collect())
}

/// Per layer, the islands of the support that hold nothing up. Going down from the top, an island
/// holds when it comes within `aside` (mm) of the overhang `wants` it under, `up` layers over it
/// (or `TOUCH_SLACK` more), or it carries an island that holds.
fn unheld(ctl: crate::par::Ctl, out: &[SupportLayer], wants: &[Vec<ExPolygon>], up: usize, aside: f64) -> Result<Vec<Vec<ExPolygon>>, crate::par::Cancelled> {
    let n = out.len();
    let islands = crate::par::try_map(ctl, n, |l| {
        let layer = &out[l];
        if layer.interface.is_empty() { union_ex(&layer.base) } else { union_ex(&[layer.base.clone(), layer.interface.clone()].concat()) }
    })?;
    let meet = |a: &ExPolygon, b: &ExPolygon| a.bbox().overlaps(&b.bbox()) && total_area_mm2(&intersection_ex(std::slice::from_ref(a), std::slice::from_ref(b))) > 0.0;
    // Per island, the islands of the layer above that it carries.
    let carries = crate::par::try_map(ctl, n, |l| {
        islands[l].iter().map(|e| if l + 1 < n { islands[l + 1].iter().enumerate().filter(|(_, a)| meet(e, a)).map(|(j, _)| j).collect() } else { Vec::new() }).collect::<Vec<Vec<usize>>>()
    })?;
    let under_part = |l: usize, e: &ExPolygon| {
        let mut grown: Option<Vec<ExPolygon>> = None;
        let (b, d) = (e.bbox(), (aside * SCALE) as i64);
        let bb = BBox { min: Pt { x: b.min.x - d, y: b.min.y - d }, max: Pt { x: b.max.x + d, y: b.max.y + d } };
        (l + up..=(l + up + TOUCH_SLACK).min(n - 1)).any(|k| {
            let over = near(&wants[k], &bb);
            !over.is_empty() && total_area_mm2(&intersection_ex(grown.get_or_insert_with(|| offset_ex(std::slice::from_ref(e), aside * SCALE, Join::Miter(3.0))), &over)) > 0.0
        })
    };
    // The tops, carrying nothing, are most of what needs the overhang looked up; those are asked at once.
    let tops = crate::par::try_map(ctl, n, |l| islands[l].iter().zip(&carries[l]).map(|(e, c)| c.is_empty() && under_part(l, e)).collect::<Vec<bool>>())?;
    let mut holds: Vec<Vec<bool>> = vec![Vec::new(); n];
    for l in (0..n).rev() {
        if ctl.cancelled() { return Err(crate::par::Cancelled); }
        holds[l] = (0..islands[l].len()).map(|i| {
            let c = &carries[l][i];
            if c.is_empty() { tops[l][i] } else { c.iter().any(|&j| holds[l + 1][j]) || under_part(l, &islands[l][i]) }
        }).collect();
    }
    Ok(islands.into_iter().zip(holds).map(|(isl, h)| isl.into_iter().zip(h).filter(|(_, h)| !*h).map(|(e, _)| e).collect()).collect())
}

/// What holds up what the generator drew.
struct Stand<'a> {
    slices: &'a [Vec<ExPolygon>],
    /// What keeps support off, per layer.
    keep: &'a [Vec<ExPolygon>],
    /// How many layers down the part counts as under a section.
    reach: usize,
    bed_only: bool,
    chains: &'a [crate::skeleton::Chain],
    whole: &'a [Vec<bool>],
}

/// The printed layer below, to look points up in.
struct Below {
    base: SegGrid<u32>,
    roof: SegGrid<u32>,
}

impl Below {
    fn has(&self, p: Pt) -> bool {
        self.base.contains_any(p) || self.roof.contains_any(p)
    }
}

fn near<'a>(region: &'a [ExPolygon], bb: &BBox) -> Vec<ExPolygon> {
    region.iter().filter(|e| e.bbox().overlaps(bb)).cloned().collect()
}

impl Stand<'_> {
    fn below(&self, layer: &SupportLayer) -> Below {
        Below { base: SegGrid::of_regions(&layer.base, |k| k as u32), roof: SegGrid::of_regions(&layer.interface, |k| k as u32) }
    }

    /// Whether the part is right under `e`, on layer `l`.
    fn on_part(&self, e: &[ExPolygon], l: usize) -> bool {
        if self.bed_only {
            return false;
        }
        let bb = bbox_of(e);
        (1..=self.reach).any(|k| l >= k && {
            let under = near(&self.slices[l - k], &bb);
            !under.is_empty() && total_area_mm2(&intersection_ex(e, &under)) > 1e-3
        })
    }

    /// The islands of `layer` hanging in the air.
    fn hanging(&self, l: usize, layer: &SupportLayer, below: &SupportLayer, over: &Below) -> Vec<ExPolygon> {
        let all = if layer.interface.is_empty() { layer.base.clone() } else { union_ex(&[layer.base.clone(), layer.interface.clone()].concat()) };
        all.into_iter().filter(|e| {
            // Under a speck nothing prints, and a union leaves such slivers between a roof and a branch.
            if e.area_mm2() < SPECK {
                return false;
            }
            // Some of its outline over the layer below is the usual case, cheap to see.
            let ring = &e.contour.pts;
            let step = (ring.len() / 16).max(1);
            if ring.iter().step_by(step).filter(|p| over.has(**p)).count() >= 3 {
                return false;
            }
            let one = std::slice::from_ref(e);
            let bb = e.bbox();
            let under: f64 = [&below.base, &below.interface].into_iter().map(|r| {
                let close = near(r, &bb);
                if close.is_empty() { 0.0 } else { total_area_mm2(&intersection_ex(one, &close)) }
            }).sum();
            under <= SPECK && !self.on_part(one, l)
        }).collect()
    }

    /// Whether a section stays. `gone`, when not empty: what has gone from the layers below.
    fn holds(&self, l: usize, d: &Drawn, over: &Below, clear: &SegGrid<u32>, gone: &[Vec<bool>]) -> bool {
        let chain = &self.chains[d.chain];
        let k = chain.top - l;
        // All of it on all of its own section below is the usual case, known without looking.
        let on_own = k + 1 < chain.sections.len() && gone.get(d.chain).is_none_or(|g| !g[k + 1]);
        if on_own && self.whole[d.chain][k] && self.whole[d.chain][k + 1] {
            return true;
        }
        // Points across it: what of it prints, and how much of that the layer below holds up.
        let (x, y, r) = d.s;
        let (mut printed, mut held) = (0, 0);
        let step = r / 3.0;
        for i in -3..=3i32 {
            for j in -3..=3i32 {
                if i * i + j * j > 9 {
                    continue;
                }
                let p = Pt::from_mm(x + i as f64 * step, y + j as f64 * step);
                if !clear.contains_any(p) {
                    printed += 1;
                    if over.has(p) {
                        held += 1;
                    }
                }
            }
        }
        // All inside what keeps support off: it prints nothing, so it holds nothing up either.
        if printed == 0 || held as f64 >= HELD_SHARE * printed as f64 {
            return true;
        }
        if self.bed_only {
            return false;
        }
        let bb = BBox { min: Pt::from_mm(x - r, y - r), max: Pt::from_mm(x + r, y + r) };
        let cut = difference_ex(&[ExPolygon::new(circle(x, y, r))], &near(&self.keep[l], &bb));
        self.on_part(&cut, l)
    }

    /// Per section of layer `l`, whether it stays; `None` when all of the layer stands.
    fn layer(&self, l: usize, drawn: &[Drawn], layer: &SupportLayer, below: &SupportLayer, gone: &[Vec<bool>]) -> Option<Vec<bool>> {
        let over = self.below(below);
        let clear = SegGrid::of_regions(&self.keep[l], |k| k as u32);
        let held: Vec<bool> = drawn.iter().map(|d| self.holds(l, d, &over, &clear, gone)).collect();
        if held.iter().all(|h| *h) && self.hanging(l, layer, below, &over).is_empty() {
            return None;
        }
        Some(held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    const H: f64 = 0.2;
    const W: f64 = 0.4;
    const LAYERS: usize = 70;

    fn square(cx: f64, cy: f64, side: f64) -> ExPolygon {
        let (a, b) = (side / 2.0, side / 2.0);
        ExPolygon::new(Polygon::new(vec![Pt::from_mm(cx - a, cy - b), Pt::from_mm(cx + a, cy - b), Pt::from_mm(cx + a, cy + b), Pt::from_mm(cx - a, cy + b)]))
    }

    #[test]
    fn locked_room_takes_the_previous_run_when_a_window_opens_and_closes() {
        let run = |slices: &[Vec<ExPolygon>], taken: &[bool], before: Option<&Locked>| {
            let keep: Vec<&[ExPolygon]> = slices.iter().map(Vec::as_slice).collect();
            locked_in(crate::par::Ctl::default(), &keep, taken, before, slices, 2.0, 1.0).unwrap()
        };
        let outer = vec![square(0.0, 0.0, 20.0)];
        let wall = difference_ex(&outer, &[square(0.0, 0.0, 12.0)]);
        let mut previous = vec![wall; 9];
        previous[0] = outer.clone();
        previous[8] = outer;
        let mut before = run(&previous, &[], None);
        let centre = Pt::new(0, 0);
        for l in 1..8 {
            assert!(before.locked[l].iter().any(|e| e.contains(centre)), "layer {l}: the sealed room is locked");
        }

        let mut away = previous.clone();
        away[0] = difference_ex(&away[0], &[rect(-10.0, -10.0, -8.0, -8.0)]);
        let mut open = away.clone();
        // Four mm wide through one wall on just one layer: wider than the two mm disc.
        open[4] = difference_ex(&open[4], &[rect(4.0, -2.0, 11.0, 2.0)]);
        let closed = away.clone();
        let mut grown = closed.clone();
        grown[0] = vec![rect(-14.0, -10.0, 10.0, 10.0)];
        for (edit, slices, opened) in [("away from the room", away, false), ("window opened", open, true), ("window closed", closed, false), ("frame moved", grown, false)] {
            let taken: Vec<bool> = slices.iter().zip(&previous).map(|(a, b)| a == b).collect();
            let warm = run(&slices, &taken, Some(&before));
            let fresh = run(&slices, &[], None);
            for l in 0..slices.len() {
                assert_eq!(warm.locked[l], fresh.locked[l], "{edit}, layer {l}: warm and fresh room agree");
            }
            for l in 1..8 {
                assert_eq!(warm.locked[l].iter().any(|e| e.contains(centre)), !opened, "{edit}, layer {l}: the window opens the room even on unchanged layers");
            }
            previous = slices;
            before = warm;
        }

        // An empty run keeps an empty result, and offers no fitted regions to its next run.
        let empty = vec![Vec::new(); previous.len()];
        let empty_room = run(&empty, &[], None);
        assert_eq!(empty_room.locked, empty);
        let taken = vec![false; previous.len()];
        assert_eq!(run(&previous, &taken, Some(&empty_room)).locked, before.locked);
    }

    /// A stem carrying a wide cap: a ring of branches closes in and merges on the way down.
    fn plate() -> (Vec<Vec<ExPolygon>>, Vec<(f64, f64)>) {
        let slices = (0..LAYERS + 10).map(|l| vec![square(50.0, 50.0, if l < LAYERS { 6.0 } else { 26.0 })]).collect();
        let heights = (0..LAYERS + 10).map(|l| ((l + 1) as f64 * H, H)).collect();
        (slices, heights)
    }

    fn tree() -> &'static Vec<SupportLayer> {
        static TREE: OnceLock<Vec<SupportLayer>> = OnceLock::new();
        TREE.get_or_init(|| {
            let (slices, heights) = plate();
            let p = SupportParams { threshold_deg: 30.0, xy_gap: 0.35, top_gap: 0.2, bottom_gap: 0.2, interface_top_layers: 2, line_width: W, wall_width: W, tip_diameter: 0.8, branch_diameter: 5.0, taper_deg: 5.0, angle_max_deg: 40.0, angle_slow_deg: 25.0, build_plate_only: false, remove_small: true, first_expansion: 2.0, stand_inside: false };
            generate(crate::par::Ctl::default(), &slices, &heights, &p).unwrap()
        })
    }

    /// A layer's wall loops as the print draws them, each closed: the outer ones, or
    /// all of them with the holes' too.
    fn walls(layer: &SupportLayer, holes: bool) -> Vec<Vec<(f64, f64)>> {
        offset_ex(&layer.base, -W / 2.0 * SCALE, Join::Miter(3.0)).iter().flat_map(|e| e.rings().take(if holes { usize::MAX } else { 1 }).map(|r| {
            let mut ring: Vec<(f64, f64)> = r.pts.iter().map(|p| (p.x_mm(), p.y_mm())).collect();
            ring.push(ring[0]);
            ring
        }).collect::<Vec<_>>()).collect()
    }

    /// Points along a closed loop, `ds` apart.
    fn along(ring: &[(f64, f64)], ds: f64) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        let mut left = 0.0;
        for w in ring.windows(2) {
            let l = (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
            while left <= l && l > 0.0 {
                let t = left / l;
                out.push((w[0].0 + (w[1].0 - w[0].0) * t, w[0].1 + (w[1].1 - w[0].1) * t));
                left += ds;
            }
            left -= l;
        }
        out
    }

    fn to_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy).max(1e-12)).clamp(0.0, 1.0);
        (p.0 - a.0 - dx * t).hypot(p.1 - a.1 - dy * t)
    }

    #[test]
    fn wall_loops_step_little_from_layer_to_layer() {
        // The steep angle at this layer height, what a branch's outline may move per layer.
        let m_fast = H * 40f64.to_radians().tan();
        let tree = tree();
        let mut steps = Vec::new();
        // Above the first layer's brim, and below the layers a new tip starts on.
        for l in 2..LAYERS - 30 {
            let below = walls(&tree[l - 1], true);
            for ring in walls(&tree[l], false) {
                for p in along(&ring, 0.5) {
                    let d = below.iter().flat_map(|r| r.windows(2)).map(|s| to_segment(p, s[0], s[1])).fold(f64::MAX, f64::min);
                    steps.push(d);
                }
            }
        }
        steps.sort_by(f64::total_cmp);
        let at = |q: f64| steps[((steps.len() - 1) as f64 * q) as usize];
        assert!(steps.len() > 1000);
        assert!(at(0.99) <= 1.5 * m_fast, "p99 step {} against the steep angle's {m_fast}", at(0.99));
        assert!(at(0.999) <= 2.0 * W, "p99.9 step {} over two beads", at(0.999));
    }

    #[test]
    fn merged_branches_blend_without_a_cusp() {
        let tree = tree();
        let (mut worst, mut merged) = (0.0f64, 0usize);
        for layer in &tree[2..LAYERS - 30] {
            for ring in walls(layer, false) {
                let s = along(&ring, 0.1);
                let n = s.len();
                if n < 16 {
                    continue;
                }
                let area: f64 = ring.windows(2).map(|w| w[0].0 * w[1].1 - w[1].0 * w[0].1).sum::<f64>() / 2.0;
                let per = n as f64 * 0.1;
                if 4.0 * std::f64::consts::PI * area / (per * per) < 0.9 {
                    merged += 1;
                }
                for i in 0..n {
                    let (a, b, c) = (s[(i + n - 3) % n], s[i], s[(i + 3) % n]);
                    let (ux, uy, vx, vy) = (b.0 - a.0, b.1 - a.1, c.0 - b.0, c.1 - b.1);
                    worst = worst.max(-(ux * vy - uy * vx).atan2(ux * vx + uy * vy).to_degrees());
                }
            }
        }
        assert!(merged > 20, "the plate should merge branches: {merged} merged loops");
        assert!(worst < 45.0, "a concave turn of {worst} degrees over 0.6 mm is a cusp");
    }

    /// A plate of `layers` slices made by `at`, at this file's layer height.
    fn plate_of(layers: usize, at: impl Fn(usize) -> Vec<ExPolygon>) -> (Vec<Vec<ExPolygon>>, Vec<(f64, f64)>) {
        ((0..layers).map(at).collect(), (0..layers).map(|l| ((l + 1) as f64 * H, H)).collect())
    }

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon::new(vec![Pt::from_mm(x0, y0), Pt::from_mm(x1, y0), Pt::from_mm(x1, y1), Pt::from_mm(x0, y1)]))
    }

    fn params(xy_gap: f64, bed_only: bool) -> SupportParams {
        SupportParams { threshold_deg: 30.0, xy_gap, top_gap: 0.2, bottom_gap: 0.2, interface_top_layers: 2, line_width: W, wall_width: W, tip_diameter: 0.8, branch_diameter: 5.0, taper_deg: 5.0, angle_max_deg: 40.0, angle_slow_deg: 25.0, build_plate_only: bed_only, remove_small: true, first_expansion: 2.0, stand_inside: false }
    }

    fn planned(slices: &[Vec<ExPolygon>], heights: &[(f64, f64)], xy_gap: f64, bed_only: bool) -> (Vec<SupportLayer>, Skeleton) {
        let p = params(xy_gap, bed_only);
        let mut skeleton = Skeleton::default();
        let layers = generate_recording(crate::par::Ctl::default(), slices, heights, &p, &mut skeleton).unwrap().layers;
        (layers, skeleton)
    }

    /// A plan tells a person waiting on it where it is, step by step in the pass's order, with a
    /// count where a step has one, and never goes back.
    #[test]
    fn a_plan_tells_its_steps_in_order_never_going_back() {
        // A cap on a stem: an overhang all round, held from the bed.
        let (slices, heights) = plate_of(60, |l| if l < 40 { vec![rect(18.0, 18.0, 22.0, 22.0)] } else { vec![rect(10.0, 10.0, 30.0, 30.0)] });
        let told = std::sync::Mutex::new(Vec::new());
        let phase = |what: &str, step: Option<(u32, u32)>, f: f64| told.lock().unwrap().push((what.to_string(), step, f));
        let mut skeleton = Skeleton::default();
        let ctl = crate::par::Ctl { phase: Some(&phase), ..Default::default() };
        let layers = generate_recording(ctl, &slices, &heights, &params(0.8, true), &mut skeleton).unwrap().layers;
        assert!(layers[..40].iter().any(|l| l.base.iter().chain(&l.interface).map(|e| e.area_mm2()).sum::<f64>() > 10.0), "the cap is held");
        let told = told.into_inner().unwrap();
        assert!(told.windows(2).all(|w| w[0].2 <= w[1].2), "never back: {told:?}");
        let mut steps: Vec<&str> = told.iter().map(|t| t.0.as_str()).collect();
        steps.dedup();
        let want = ["finding overhangs", "mapping where branches reach", "laying roofs", "planning trees", "standing trees on the bed", "bringing branches down", "drawing branches", "checking what stands", "taking away what holds nothing", "trimming tops", "checking what the trim left", "telling what has none"];
        let order: Vec<&str> = steps.iter().copied().filter(|s| want.contains(s)).collect();
        assert_eq!(order, want, "in the pass's order: {steps:?}");
        assert!(told.iter().any(|t| t.0 == "planning trees" && t.1.is_some_and(|(i, n)| i >= 1 && i <= n)), "trees are counted");
        assert!(told.iter().any(|t| t.0 == "checking what stands" && t.1 == Some((60, 60))), "every layer is checked and counted");
        assert!(told.iter().any(|t| t.0 == "mapping where branches reach" && t.1.is_some_and(|(i, n)| i == n && n >= 2)), "every width's reach is mapped and counted");
        assert!(told.iter().any(|t| t.0 == "bringing branches down" && t.1 == Some((60, 60))), "the descent counts every layer");
        assert_eq!(told[0].2, 0.0);
        assert!(told.last().unwrap().2 < 1.0, "the pass's own steps end before it does");
    }

    /// [`planned`] under boxes (min, max) of support paint, at the side gap the defaults had.
    fn painted(slices: &[Vec<ExPolygon>], heights: &[(f64, f64)], block: Option<[[f64; 3]; 2]>, enforce: Option<[[f64; 3]; 2]>, bed_only: bool) -> (Vec<SupportLayer>, Skeleton) {
        let zs: Vec<f64> = heights.iter().map(|&(z, h)| z - h / 2.0).collect();
        let region = |b: Option<[[f64; 3]; 2]>| b.map(|b| crate::support_paint::box_region(b[0], b[1]));
        let paint = makepad_fab_slicer::source::SupportPaint { block: region(block), enforce: region(enforce), no_trees: None };
        let masks = Masks::of(crate::par::Ctl::default(), Some(paint), &zs, ([0.0; 3], [150.0, 150.0, 40.0]), crate::support_paint::round_the_part(([0.0; 3], [150.0, 150.0, 40.0]))).unwrap();
        let mut skeleton = Skeleton::default();
        let layers = generate_painted(crate::par::Ctl::default(), slices, heights, &params(0.35, bed_only), &masks, &mut skeleton).unwrap().layers;
        (layers, skeleton)
    }

    /// What prints in a block (mm2) by its outline's rounding alone: the support comes right up to
    /// it, to its outline, which lies within a thousandth of a millimetre of the box.
    const OUTLINE: f64 = 0.05;

    /// What a layer prints inside `r`, in mm2.
    fn printed_in(layer: &SupportLayer, r: &ExPolygon) -> f64 {
        total_area_mm2(&intersection_ex(&[layer.base.clone(), layer.interface.clone()].concat(), std::slice::from_ref(r)))
    }

    /// How far down (mm) the generator lets a section rest on the part: its own layer, the bottom
    /// gap and [`LAND_AIR`].
    const REST: f64 = H + 0.2 + LAND_AIR;

    /// The islands hanging in the air, (layer, area): nothing of the support under them and, where
    /// standing on the part is allowed, nothing of the part in `reach` mm under them.
    fn hanging(layers: &[SupportLayer], slices: &[Vec<ExPolygon>], reach: Option<f64>) -> Vec<(usize, f64)> {
        let mut out = Vec::new();
        for l in 1..layers.len() {
            let below = [layers[l - 1].base.clone(), layers[l - 1].interface.clone()].concat();
            for island in union_ex(&[layers[l].base.clone(), layers[l].interface.clone()].concat()) {
                let one = std::slice::from_ref(&island);
                let held = total_area_mm2(&intersection_ex(one, &below)) > SPECK;
                let on_part = reach.is_some_and(|r| (1..=(r / H).ceil() as usize).any(|k| l >= k && total_area_mm2(&intersection_ex(one, &slices[l - k])) > 1e-3));
                if !held && !on_part {
                    out.push((l, island.area_mm2()));
                }
            }
        }
        out
    }

    /// The islands that hold nothing up, (layer, area): nothing of the support over them, and
    /// nothing of the part within the top gap and a layer over them, a tip's gap aside of them.
    fn idle(layers: &[SupportLayer], slices: &[Vec<ExPolygon>]) -> Vec<(usize, f64)> {
        let mut out = Vec::new();
        for l in 0..layers.len() {
            let above = if l + 1 < layers.len() { [layers[l + 1].base.clone(), layers[l + 1].interface.clone()].concat() } else { Vec::new() };
            for island in union_ex(&[layers[l].base.clone(), layers[l].interface.clone()].concat()) {
                let one = std::slice::from_ref(&island);
                if total_area_mm2(&intersection_ex(one, &above)) > 0.0 {
                    continue;
                }
                let near = offset_ex(one, (TIP_GAP + 0.05) * SCALE, Join::Miter(3.0));
                if !(l + 1..=(l + 3).min(slices.len() - 1)).any(|k| total_area_mm2(&intersection_ex(&near, &slices[k])) > 1e-3) {
                    out.push((l, island.area_mm2()));
                }
            }
        }
        out
    }

    /// What of the overhang over layer `l + 2` no support reaches, in mm2: nothing printed within
    /// half a bare tip's spacing of it on the layers a tip may start on under it.
    fn unreached(layers: &[SupportLayer], slices: &[Vec<ExPolygon>], heights: &[(f64, f64)], p: &SupportParams) -> Vec<(usize, f64)> {
        let wanted = demand(crate::par::Ctl::default(), slices, heights, p).unwrap();
        (2..slices.len()).filter_map(|src| {
            let under: Vec<ExPolygon> = (src.saturating_sub(2 + TIP_CLIPPED)..=src - 2).flat_map(|k| layers[k].base.iter().chain(&layers[k].interface).cloned().collect::<Vec<_>>()).collect();
            let near = offset_ex(&union_ex(&under), tip_spacing(W, p.tip_diameter / 2.0) / 2.0 * SCALE, Join::Round(0.05 * SCALE));
            let left = total_area_mm2(&difference_ex(&wanted[src], &near));
            (left > SPECK).then_some((src, left))
        }).collect()
    }

    /// A face leaning out over the bed is held whatever the side gap: the tips under it sit at the
    /// top gap, nothing printed stops short of it, and the wall under the face keeps the side gap
    /// below where the support under the face moves out to it from xy_min at the steep angle.
    #[test]
    fn an_overhang_face_is_held_whatever_the_side_gap() {
        let (slices, heights) = plate_of(60, |l| vec![rect(40.0, 40.0, if l < 20 { 60.0 } else { 60.0 + 0.5 * (l - 19) as f64 }, 60.0)]);
        let m_fast = H * 40f64.to_radians().tan();
        for gap in [0.35, 2.0] {
            let (layers, _) = planned(&slices, &heights, gap, true);
            let root = 20 + (2.0 * gap).ceil() as usize;
            let left: Vec<(usize, f64)> = unreached(&layers, &slices, &heights, &params(gap, true)).into_iter().filter(|x| x.0 > root).collect();
            let missed: f64 = left.iter().map(|x| x.1).sum();
            assert!(missed < 5.0, "side gap {gap}: the face is left without support (layer, mm2): {left:?}");
            assert_eq!(idle(&layers, &slices), Vec::new(), "side gap {gap}: these stop short of the face");
            assert_eq!(hanging(&layers, &slices, None), Vec::new(), "side gap {gap}: something stops short of the bed");
            // The face's contact is two layers under it; from there the support moves out.
            let out = ((gap - params(gap, true).xy_min()) / m_fast).ceil() as usize;
            let riding: Vec<(usize, f64)> = (0..18 - out).map(|l| (l, within(&layers, &slices, l, gap - 0.05))).filter(|r| r.1 >= 0.01).collect();
            assert_eq!(riding, Vec::new(), "side gap {gap}: the support rides the wall under the face (layer, mm2)");
        }
    }

    /// Per tip: its top's layer and how wide (mm) its branch is `depth` mm under it, along what it
    /// merges into.
    fn stems(skeleton: &Skeleton, depth: f64) -> Vec<(usize, f64)> {
        let pointed: std::collections::BTreeSet<usize> = skeleton.chains.iter().filter_map(|c| c.into).collect();
        let down = (depth / H).round() as usize;
        skeleton.chains.iter().enumerate().filter(|(i, _)| !pointed.contains(i)).filter_map(|(_, c)| {
            let at = c.top.checked_sub(down)?;
            let mut chain = c;
            loop {
                if let Some(s) = chain.top.checked_sub(at).and_then(|k| chain.sections.get(k)) {
                    return Some((c.top, 2.0 * s.2));
                }
                chain = &skeleton.chains[chain.into?];
            }
        }).collect()
    }

    /// Under a frame round the top of a box, a face leaning out and a cap, from the bed only: 3 mm
    /// under what it holds a branch is well past a bare tip's 0.8 mm, not a cone narrowing to it,
    /// and going down no trunk beside these plain walls narrows, however little room they leave it.
    #[test]
    fn a_branch_is_a_stem_just_under_what_it_holds() {
        let frame = plate_of(64, |l| vec![if l < 54 { rect(40.0, 40.0, 60.0, 60.0) } else { rect(37.0, 37.0, 63.0, 63.0) }]);
        let face = plate_of(60, |l| vec![rect(40.0, 40.0, if l < 20 { 60.0 } else { 60.0 + 0.5 * (l - 19) as f64 }, 60.0)]);
        for (name, (slices, heights)) in [("frame", frame), ("face", face), ("cap", plate())] {
            for gap in [0.35, 2.0] {
                let (layers, skeleton) = planned(&slices, &heights, gap, true);
                assert_eq!(idle(&layers, &slices), Vec::new(), "{name}, side gap {gap}: these hold nothing up (layer, mm2)");
                let all = stems(&skeleton, 3.0);
                let mut thin: Vec<(usize, f64)> = all.iter().copied().filter(|s| s.1 < 1.4).collect();
                thin.sort_by(|a, b| a.1.total_cmp(&b.1));
                assert!(thin.is_empty(), "{name}, side gap {gap}: 3 mm under their tops these are under 1.4 mm across (top layer, mm): {thin:?} of {}", all.len());
                let narrows: Vec<(usize, f64, f64)> = skeleton.chains.iter().filter(|c| c.into.is_none()).flat_map(|c| c.sections.windows(2).enumerate().filter(|(_, w)| w[1].2 < w[0].2 - 1e-6).map(|(k, w)| (c.top - k - 1, w[0].2, w[1].2)).collect::<Vec<_>>()).collect();
                assert!(narrows.is_empty(), "{name}, side gap {gap}: trunks narrow going down (layer, radius above, radius): {:?} of {}", &narrows[..narrows.len().min(6)], narrows.len());
            }
        }
    }

    /// A cap over a column on a wide flange, from the bed only: what cannot get past the flange
    /// goes, and nothing of it stays standing on the bed beside the flange.
    #[test]
    fn nothing_is_left_beside_a_flange_a_branch_ended_on() {
        let (slices, heights) = plate_of(36, |l| vec![if l < 4 { square(50.0, 50.0, 32.0) } else if l < 30 { square(50.0, 50.0, 8.0) } else { square(50.0, 50.0, 60.0) }]);
        for gap in [0.35, 2.0] {
            let (layers, _) = planned(&slices, &heights, gap, true);
            assert_eq!(idle(&layers, &slices), Vec::new(), "side gap {gap}: these hold nothing up (layer, mm2)");
            assert_eq!(hanging(&layers, &slices, None), Vec::new(), "side gap {gap}: something stops short of the bed");
            assert!(!layers[10].base.is_empty(), "side gap {gap}: the branches past the flange stay");
        }
    }

    /// A stem on a plinth under a cap wider than the plinth: the branches over the plinth have
    /// nowhere to go but onto it, those past it reach the bed.
    #[test]
    fn from_the_bed_only_all_of_it_reaches_the_bed() {
        let (slices, heights) = plate_of(40, |l| vec![square(50.0, 50.0, if l < 10 { 30.0 } else if l < 30 { 6.0 } else { 40.0 })]);
        // Inside the plinth's edge: trunks past the edge may lean over it.
        let over_plinth = [square(50.0, 50.0, 24.0)];
        let on_plinth = |layers: &[SupportLayer]| total_area_mm2(&intersection_ex(&layers[12].base, &over_plinth));
        let (free, _) = planned(&slices, &heights, 0.35, false);
        assert!(on_plinth(&free) > 5.0, "where it may, the tree stands on the plinth: {}", on_plinth(&free));
        let (layers, skeleton) = planned(&slices, &heights, 0.35, true);
        assert_eq!(hanging(&layers, &slices, None), Vec::new(), "something stops short of the bed");
        assert_eq!(idle(&layers, &slices), Vec::new(), "these hold nothing up (layer, mm2)");
        assert!(on_plinth(&layers) < 1e-6, "{} mm2 still stands on the plinth", on_plinth(&layers));
        let printed: f64 = layers.iter().map(|l| total_area_mm2(&l.base)).sum();
        assert!(printed > 100.0, "the branches past the plinth stay: {printed} mm2 of base");
        // A branch's end may stand in the pads a trunk spreads into, which reach the bed.
        let on_pad = |c: &crate::skeleton::Chain| {
            let (end, &(x, y, _)) = (c.top + 1 - c.sections.len(), c.sections.last().unwrap());
            end > 0 && skeleton.pads[end - 1].iter().any(|e| e.contains(Pt::from_mm(x, y)))
        };
        for c in &skeleton.chains {
            assert!(c.into.is_some() || c.top + 1 == c.sections.len() || on_pad(c), "a chain from layer {} stops {} layers above the bed", c.top, c.top + 1 - c.sections.len());
        }
        let given_up: f64 = skeleton.given_up.iter().map(|g| g.area).sum();
        // What stood on the plinth is held from beside it, at the steep angle, or reported.
        let cap_held = total_area_mm2(&intersection_ex(&layers[28].interface, &[square(50.0, 50.0, 30.0)]));
        assert!(given_up + cap_held > 50.0, "what stood on the plinth is held or reported: {given_up} mm2 reported, {cap_held} mm2 held");
    }

    /// A flat bar held out from a block, from the bed: the simplest shape that holds its underside
    /// is one trunk widening into one flat roof the size of what it holds. No twigs, no cones
    /// beside it, no steps up its side.
    #[test]
    fn a_flat_bar_gets_one_trunk_widening_into_one_roof() {
        let (block, bar) = (rect(30.0, 44.0, 50.0, 56.0), rect(50.0, 46.0, 62.0, 54.0));
        let (slices, heights) = plate_of(60, |l| if l < 40 { vec![block.clone()] } else if l < 50 { union_ex(&[block.clone(), bar.clone()]) } else { Vec::new() });
        for gap in [0.35, 2.0] {
            let (layers, skeleton) = planned(&slices, &heights, gap, true);
            let trunks: Vec<&crate::skeleton::Chain> = skeleton.chains.iter().collect();
            assert_eq!(trunks.len(), 1, "side gap {gap}: one trunk and no tips: {:?}", trunks.iter().map(|c| (c.top, c.sections.len(), c.sections[0])).collect::<Vec<_>>());
            let trunk = trunks[0];
            assert_eq!(trunk.top + 1, trunk.sections.len(), "side gap {gap}: the trunk stands on the bed");
            // One piece a layer: the trunk, then the taper it widens into, each layer on the one
            // under it, reaching past it no further than the steep angle.
            let m_fast = H * 40f64.to_radians().tan();
            for (l, layer) in layers.iter().enumerate().take_while(|(_, layer)| layer.interface.is_empty()).skip(1) {
                assert!(layer.base.len() <= 1, "side gap {gap}: layer {l} prints {} pieces", layer.base.len());
                let past = total_area_mm2(&difference_ex(&layer.base, &offset_ex(&layers[l - 1].base, (m_fast + 0.05) * SCALE, Join::Miter(3.0))));
                assert!(past < 0.05, "side gap {gap}: layer {l} reaches {past} mm2 past the one under it");
            }
            // Its roof is what it holds: the bar's underside, but for xy_min to the block (the side
            // gap is a preference a roof does not keep).
            let held = total_area_mm2(&difference_ex(&[bar.clone()], &offset_ex(&[block.clone()], params(gap, true).xy_min() * SCALE, Join::Miter(3.0))));
            let roof = total_area_mm2(&layers[38].interface);
            let under = total_area_mm2(&intersection_ex(&layers[38].interface, &[bar.clone()]));
            assert!(under > 0.9 * held && roof < 1.05 * held, "side gap {gap}: a roof of {roof} mm2, {under} of it under the bar, for {held} mm2 held");
        }
    }

    /// A narrow column under a wide slab, a bar held out from a block, and an L held up at its
    /// corner, from the bed: under each the support's top is the whole of what it holds, one top
    /// gap under it, and every roof lies on solid support. No top of the support stops short of
    /// the part, and the L stands on the fewest trunks that reach all of it.
    #[test]
    fn every_roof_lies_on_solid_support_under_all_it_holds() {
        let (column, slab) = (rect(47.0, 47.0, 53.0, 53.0), rect(35.0, 44.0, 65.0, 56.0));
        let tee = plate_of(56, |l| vec![if l < 40 { column.clone() } else { slab.clone() }]);
        let (block, bar) = (rect(30.0, 44.0, 50.0, 56.0), rect(50.0, 46.0, 62.0, 54.0));
        let held_out = plate_of(60, |l| if l < 40 { vec![block.clone()] } else if l < 50 { union_ex(&[block.clone(), bar.clone()]) } else { Vec::new() });
        let (post, ell) = (rect(31.0, 31.0, 37.0, 37.0), union_ex(&[rect(30.0, 30.0, 60.0, 38.0), rect(30.0, 30.0, 38.0, 60.0)]));
        let el = plate_of(110, |l| if l < 100 { vec![post.clone()] } else { ell.clone() });
        for (name, (slices, heights), (under, over), at) in [("tee", tee, (vec![column.clone()], vec![slab.clone()]), 40), ("bar", held_out, (vec![block.clone()], vec![bar.clone()]), 40), ("L", el, (vec![post.clone()], ell.clone()), 100)] {
            for gap in [0.35, 2.0] {
                let (layers, skeleton) = planned(&slices, &heights, gap, true);
                // What a roof lies on: solid support, or the roof under it.
                for l in 1..layers.len() {
                    let mut on = layers[l - 1].solid.clone();
                    on.extend(layers[l - 1].interface.iter().cloned());
                    let off = total_area_mm2(&difference_ex(&layers[l].interface, &offset_ex(&union_ex(&on), (CARRIED * W + 0.05) * SCALE, Join::Miter(3.0))));
                    assert!(off < 0.05, "{name}, side gap {gap}: {off} mm2 of the roof on layer {l} lies on no solid support");
                }
                // Every top of the support has the part within the top gap over it (past the bed
                // layer's brim).
                for l in 1..layers.len() - 2 {
                    let here = union_ex(&[layers[l].base.clone(), layers[l].interface.clone()].concat());
                    let up = [layers[l + 1].base.clone(), layers[l + 1].interface.clone()].concat();
                    let top = offset_ex(&difference_ex(&here, &up), -W * SCALE, Join::Miter(3.0));
                    let part: Vec<ExPolygon> = (l + 1..=l + 2).flat_map(|k| slices[k].iter().cloned()).collect();
                    let short: f64 = difference_ex(&top, &offset_ex(&part, W * SCALE, Join::Miter(3.0))).iter().map(|e| e.area_mm2()).sum();
                    assert!(short < 0.05, "{name}, side gap {gap}: {short} mm2 of the support's top on layer {l} has nothing of the part over it");
                }
                // The top roof layer holds all of the overhang but its gap to the part under it.
                let held = difference_ex(&over, &offset_ex(&under, gap * SCALE, Join::Miter(3.0)));
                let carried = total_area_mm2(&intersection_ex(&layers[at - 2].interface, &held)) / total_area_mm2(&held);
                assert!(carried > 0.95, "{name}, side gap {gap}: the roof holds {:.0} % of the overhang", carried * 100.0);
                if name == "L" {
                    let starts: Vec<usize> = skeleton.chains.iter().filter(|c| c.into.is_none()).map(|c| c.top).collect();
                    assert!((1..=3).contains(&starts.len()), "side gap {gap}: the L stands on {} trunks (tops {starts:?})", starts.len());
                }
            }
        }
    }

    /// What stands in `region` (mm³), every layer.
    fn stands_in(layers: &[SupportLayer], region: &[ExPolygon]) -> f64 {
        layers.iter().map(|l| total_area_mm2(&intersection_ex(&[l.base.clone(), l.interface.clone()].concat(), region)) * H).sum()
    }

    fn planned_inside(slices: &[Vec<ExPolygon>], heights: &[(f64, f64)], stand_inside: bool) -> (Vec<SupportLayer>, Skeleton) {
        let p = SupportParams { threshold_deg: 30.0, xy_gap: 0.35, top_gap: 0.2, bottom_gap: 0.2, interface_top_layers: 2, line_width: W, wall_width: W, tip_diameter: 0.8, branch_diameter: 5.0, taper_deg: 5.0, angle_max_deg: 40.0, angle_slow_deg: 25.0, build_plate_only: false, remove_small: true, first_expansion: 2.0, stand_inside };
        let mut skeleton = Skeleton::default();
        let layers = generate_recording(crate::par::Ctl::default(), slices, heights, &p, &mut skeleton).unwrap().layers;
        (layers, skeleton)
    }

    /// Trees stay trees: no section moves further than the steep angle a layer, and nothing is left
    /// in the air.
    fn stays_a_tree(layers: &[SupportLayer], slices: &[Vec<ExPolygon>], skeleton: &Skeleton) {
        let m_fast = H * 40f64.to_radians().tan();
        for c in &skeleton.chains {
            for w in c.sections.windows(2) {
                let step = (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
                assert!(step <= m_fast + 1e-6, "a jump of {step} mm against the steep angle's {m_fast}");
            }
        }
        assert_eq!(hanging(layers, slices, Some(REST)), Vec::new(), "something hangs in the air");
    }

    fn given_inside(skeleton: &Skeleton) -> f64 {
        skeleton.inside.iter().map(|g| g.area).sum()
    }

    fn given_locked(skeleton: &Skeleton) -> f64 {
        skeleton.locked.iter().map(|g| g.area).sum()
    }

    /// A cup with a shelf inside it, shut by a lid with a collar round it: the shelf's overhang is
    /// over a hollow a support gets into only to be locked in, the collar's over the open. Asked
    /// to, the support stands in the hollow; else it is told as locked in, with where.
    #[test]
    fn nothing_stands_in_a_shut_cup() {
        let (lo, hi) = (30.0, 70.0);
        let wall = |extra: Vec<ExPolygon>| {
            let ring = difference_ex(&[rect(lo, lo, hi, hi)], &[rect(lo + 2.0, lo + 2.0, hi - 2.0, hi - 2.0)]);
            union_ex(&[ring, extra].concat())
        };
        let (slices, heights) = plate_of(90, |l| {
            if l < 6 {
                vec![rect(lo, lo, hi, hi)]
            } else if l < 50 {
                wall(Vec::new())
            } else if l < 60 {
                wall(vec![rect(lo + 2.0, lo + 2.0, lo + 18.0, hi - 2.0)])
            } else if l < 75 {
                wall(Vec::new())
            } else {
                wall(vec![rect(lo - 12.0, lo - 12.0, hi + 12.0, hi + 12.0)])
            }
        });
        let hollow = [rect(lo + 2.0, lo + 2.0, hi - 2.0, hi - 2.0)];
        let (free, _) = planned_inside(&slices, &heights, true);
        assert!(stands_in(&free, &hollow) > 100.0, "asked to, it stands in the hollow: {} mm3", stands_in(&free, &hollow));
        let (layers, skeleton) = planned_inside(&slices, &heights, false);
        assert!(stands_in(&layers, &hollow) < 1e-6, "{} mm3 stands in the hollow", stands_in(&layers, &hollow));
        let outside: f64 = layers[6..70].iter().map(|l| total_area_mm2(&l.base)).sum();
        assert!(outside > 100.0, "the collar keeps its branches, outside: {outside} mm2");
        // The shelf's underside, 16 by 36 mm, and whatever else hangs over the hollow.
        let given = given_locked(&skeleton);
        assert!(given > 500.0, "what's over the hollow is reported as locked in: {given} mm2");
        assert!(skeleton.locked.iter().any(|g| (g.at[0] - (lo + 10.0)).abs() < 8.0 && (g.at[1] - 50.0).abs() < 18.0), "it says where: {:?}", skeleton.locked);
        assert_eq!(given_inside(&skeleton), 0.0, "a branch gets to it, so it is not closed in");
        stays_a_tree(&layers, &slices, &skeleton);
    }

    /// Under an arch wider than the thinnest column (see [`window`]) a branch stands, and comes out
    /// of its open ends, at a span of 30 mm, 8 or 5: nothing under it is locked in, and nothing of
    /// it counts as inside.
    #[test]
    fn an_arch_wider_than_a_column_is_open() {
        let arch = |span: f64| plate_of(60, move |l| {
            if l < 40 {
                vec![rect(40.0, 40.0, 46.0, 70.0), rect(46.0 + span, 40.0, 52.0 + span, 70.0)]
            } else {
                vec![rect(40.0, 40.0, 52.0 + span, 70.0)]
            }
        });
        let (slices, heights) = arch(30.0);
        let (layers, skeleton) = planned_inside(&slices, &heights, false);
        let under = [rect(47.0, 41.0, 75.0, 69.0)];
        assert!(stands_in(&layers, &under) > 100.0, "the open arch keeps its branches: {} mm3", stands_in(&layers, &under));
        assert_eq!(given_inside(&skeleton), 0.0, "nothing of it counts as inside");
        stays_a_tree(&layers, &slices, &skeleton);
        for (span, under, most) in [(8.0, rect(47.0, 41.0, 53.0, 69.0), 20.0), (5.0, rect(46.5, 42.5, 50.5, 67.5), 5.0)] {
            let (slices, heights) = arch(span);
            let (layers, skeleton) = planned_inside(&slices, &heights, false);
            let under = [under];
            assert!(stands_in(&layers, &under) > most, "span {span}: the arch keeps its branches: {} mm3", stands_in(&layers, &under));
            assert_eq!(given_locked(&skeleton), 0.0, "span {span}: nothing under it is locked in");
            assert_eq!(given_inside(&skeleton), 0.0, "span {span}: nothing of it counts as inside");
        }
    }

    /// A cap on a stem off a wider foot: its roof would close round the stem into a ring that can
    /// neither slide down off it nor come away sideways. It prints in arcs, each piece apart, and
    /// the cap over the gaps never spans more than it can bridge.
    #[test]
    fn a_roof_round_a_stem_comes_off_in_pieces() {
        let disc = |r: f64| ExPolygon::new(circle(50.0, 50.0, r));
        let (slices, heights) = plate_of(70, |l| if l < 20 { vec![disc(12.0)] } else if l < 50 { vec![disc(6.0)] } else { vec![disc(14.0)] });
        let (layers, mut skeleton) = planned_inside(&slices, &heights, false);
        skeleton.finish_tree(&heights, layers.iter().map(|l| l.interface.clone()).collect());
        let roof_layer = (0..50).rev().find(|&l| !layers[l].interface.is_empty()).expect("the cap has a roof");
        let roof = union_ex(&layers[roof_layer].interface);
        // A roof lies only so far past the branches under it, so an arc may have a hole: what
        // matters is that none goes round the stem.
        let stem = Pt::from_mm(50.0, 50.0);
        assert!(roof.len() >= 3 && roof.iter().all(|e| !ExPolygon::new(e.contour.clone()).contains(stem)), "the roof is in arcs: {:?} (mm2, holes)", roof.iter().map(|e| (e.area_mm2(), e.holes.len())).collect::<Vec<_>>());
        for e in &roof {
            assert!(wrap_deg(e, stem) <= WRAP_MOST, "an arc wraps {} degrees", wrap_deg(e, stem));
        }
        let pieces = skeleton.pieces();
        assert!(pieces.len() >= 3, "the arcs and what holds them come apart: {} pieces", pieces.len());
        // As printed too: the pieces the removal check sees are the ones that print.
        let held = pieces.iter().filter(|pc| !pc.roofs.is_empty()).count();
        assert_eq!(printed_roofed_pieces(&layers), held, "the arcs print apart as the skeleton has them");
        // Round the ring the roof leaves no gap a bridge can't span.
        let mut gap = 0.0f64;
        let mut widest = 0.0f64;
        for k in 0..720 {
            let a = (k as f64 / 2.0).to_radians();
            let held = roof.iter().any(|e| e.contains(Pt::from_mm(50.0 + 10.0 * a.cos(), 50.0 + 10.0 * a.sin())));
            gap = if held { 0.0 } else { gap + 10.0 * 0.5f64.to_radians() };
            widest = widest.max(gap);
        }
        assert!(widest < 3.0, "the cap spans {widest} mm over a gap in its roof");
        stays_a_tree(&layers, &slices, &skeleton);
    }

    /// What of `layer` prints, near enough: the band a base's sheath prints round each outline, and
    /// the roof's lines across it.
    fn printed_of(layer: &SupportLayer) -> Vec<ExPolygon> {
        let band = difference_ex(&layer.base, &offset_ex(&layer.base, -W * SCALE, Join::Miter(3.0)));
        union_ex(&[band, layer.interface.clone()].concat())
    }

    /// A fin hanging from a cap, its bottom edge a sliver narrower than a line: a roof a line
    /// wider than it prints right under the gap, so whatever trunk its tip stands in, the roof's
    /// lines reach the sliver.
    #[test]
    fn a_sliver_too_thin_for_a_roof_gets_one_a_line_wider() {
        let (slices, heights) = plate_of(80, |l| if l < 30 { Vec::new() } else if l < 40 { let w = 0.065 * (l - 29) as f64; vec![rect(50.0 - w, 44.0, 50.0 + w, 56.0)] } else if l < 70 { vec![rect(49.35, 44.0, 50.65, 56.0)] } else { vec![rect(35.0, 35.0, 65.0, 65.0)] });
        for stand_inside in [false, true] {
            let (layers, _) = planned_inside(&slices, &heights, stand_inside);
            let under = 30 - 2;
            let held = total_area_mm2(&intersection_ex(&printed_of(&layers[under]), &slices[30]));
            assert!(held > 0.0, "inside allowed: {stand_inside}: nothing prints under the sliver on layer {under}");
            // Its roof is wider than the sliver by a line all round, so its lines cross under it.
            let wide = offset_ex(&slices[30], W * SCALE, Join::Miter(3.0));
            let roof = total_area_mm2(&intersection_ex(&layers[under].interface, &wide));
            assert!(roof > 0.8 * total_area_mm2(&wide), "inside allowed: {stand_inside}: {roof} of {} mm2 roofed under the sliver", total_area_mm2(&wide));
        }
    }

    /// A closed box with a peg hanging from its lid: the peg's end has nothing below it and would
    /// print on air, so its branch stands inside, and only it.
    #[test]
    fn an_island_on_air_inside_keeps_its_branch() {
        let (lo, hi) = (30.0, 70.0);
        let (slices, heights) = plate_of(90, |l| {
            let ring = difference_ex(&[rect(lo, lo, hi, hi)], &[rect(lo + 2.0, lo + 2.0, hi - 2.0, hi - 2.0)]);
            if l < 6 || l >= 80 {
                vec![rect(lo, lo, hi, hi)]
            } else if l >= 40 {
                union_ex(&[ring, vec![rect(48.0, 48.0, 52.0, 52.0)]].concat())
            } else {
                ring
            }
        });
        let (layers, skeleton) = planned_inside(&slices, &heights, false);
        let under_peg = [rect(47.0, 47.0, 53.0, 53.0)];
        for l in [20, 38] {
            let held = total_area_mm2(&intersection_ex(&[layers[l].base.clone(), layers[l].interface.clone()].concat(), &under_peg));
            assert!(held > 0.5, "layer {l}: the peg's end has a branch under it: {held} mm2");
        }
        let elsewhere = difference_ex(&[rect(lo + 2.0, lo + 2.0, hi - 2.0, hi - 2.0)], &[rect(40.0, 40.0, 60.0, 60.0)]);
        assert!(stands_in(&layers, &elsewhere) < 1e-6, "{} mm3 stands in the box away from the peg", stands_in(&layers, &elsewhere));
        assert!(given_locked(&skeleton) > 500.0, "the lid's underside is reported as locked in: {} mm2", given_locked(&skeleton));
        stays_a_tree(&layers, &slices, &skeleton);
    }

    /// How many pieces the support prints in that carry some of its roof: islands joined where
    /// they meet the layer below's.
    fn printed_roofed_pieces(layers: &[SupportLayer]) -> usize {
        let islands: Vec<Vec<ExPolygon>> = layers.iter().map(|l| union_ex(&[l.base.clone(), l.interface.clone()].concat())).collect();
        let mut first = Vec::with_capacity(islands.len());
        let mut parent: Vec<usize> = Vec::new();
        for i in &islands {
            first.push(parent.len());
            parent.extend(parent.len()..parent.len() + i.len());
        }
        fn root(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        let meet = |e: &ExPolygon, f: &[ExPolygon]| f.iter().any(|f| e.bbox().overlaps(&f.bbox()) && intersection_ex(std::slice::from_ref(e), std::slice::from_ref(f)).iter().any(|x| x.area_mm2() > 1e-4));
        for l in 1..islands.len() {
            for (a, e) in islands[l].iter().enumerate() {
                for (b, f) in islands[l - 1].iter().enumerate() {
                    if meet(e, std::slice::from_ref(f)) {
                        let (ra, rb) = (root(&mut parent, first[l] + a), root(&mut parent, first[l - 1] + b));
                        parent[ra] = rb;
                    }
                }
            }
        }
        let mut roofed = std::collections::BTreeSet::new();
        for (l, layer) in islands.iter().enumerate() {
            for (a, e) in layer.iter().enumerate() {
                if meet(e, &layers[l].interface) {
                    roofed.insert(root(&mut parent, first[l] + a));
                }
            }
        }
        roofed.len()
    }

    /// The peg again, its end touching a hair-thin post: held by less than a speck, it still
    /// prints on air, so its branch stands inside.
    #[test]
    fn an_island_held_by_a_hair_inside_keeps_its_branch() {
        let (lo, hi) = (30.0, 70.0);
        let (slices, heights) = plate_of(90, |l| {
            let ring = difference_ex(&[rect(lo, lo, hi, hi)], &[rect(lo + 2.0, lo + 2.0, hi - 2.0, hi - 2.0)]);
            if l < 6 || l >= 80 {
                vec![rect(lo, lo, hi, hi)]
            } else if l >= 40 {
                union_ex(&[ring, vec![rect(48.0, 48.0, 52.0, 52.0)]].concat())
            } else {
                union_ex(&[ring, vec![rect(51.9, 51.9, 52.0, 52.0)]].concat())
            }
        });
        let (layers, skeleton) = planned_inside(&slices, &heights, false);
        let under_peg = [rect(47.0, 47.0, 53.0, 53.0)];
        for l in [20, 38] {
            let held = total_area_mm2(&intersection_ex(&[layers[l].base.clone(), layers[l].interface.clone()].concat(), &under_peg));
            assert!(held > 0.5, "layer {l}: the peg's end has a branch under it: {held} mm2");
        }
        stays_a_tree(&layers, &slices, &skeleton);
    }

    /// Bars crossing in stacked bands, its cells open on every face, and a wing past one face. Its
    /// windows are wider than the thinnest column (see [`window`]) at 12 mm pitch and at 6: a
    /// branch gets into the cells and out again, so they keep their branches, the wing keeps its
    /// own, and nothing is locked in.
    #[test]
    fn a_lattice_keeps_its_branches_where_they_come_out() {
        let (lo, hi) = (30.0, 62.0);
        let lattice = |pitch: f64| plate_of(100, move |l| {
            let along_x = (l / 10) % 2 == 0;
            let k = ((hi - lo) / pitch).floor() as usize;
            let mut bars: Vec<ExPolygon> = (0..=k).map(|k| {
                let a = (lo + pitch * k as f64).min(hi - 1.5);
                if along_x { rect(lo, a, hi, a + 1.5) } else { rect(a, lo, a + 1.5, hi) }
            }).collect();
            if l >= 90 {
                bars.push(rect(hi, lo, hi + 16.0, hi));
            }
            union_ex(&bars)
        });
        let mouth = 2.5;
        let cells = [rect(lo + mouth, lo + mouth, hi - mouth, hi - mouth)];
        let wing = [rect(hi + 0.5, lo, hi + 16.0, hi)];
        for pitch in [12.0, 6.0] {
            let (slices, heights) = lattice(pitch);
            let (layers, skeleton) = planned_inside(&slices, &heights, false);
            assert!(stands_in(&layers, &cells) > 100.0, "pitch {pitch}: it stands in the cells: {} mm3", stands_in(&layers, &cells));
            assert!(stands_in(&layers, &wing) > 100.0, "pitch {pitch}: the wing keeps its branches: {} mm3", stands_in(&layers, &wing));
            assert_eq!(given_locked(&skeleton), 0.0, "pitch {pitch}: nothing is locked in");
            stays_a_tree(&layers, &slices, &skeleton);
        }
    }

    /// Whatever went from under it, a roof lies over its roof below or the support below: no layer
    /// of it reaches further out than the one under it.
    #[test]
    fn a_roof_reaches_no_further_than_the_branches_under_it() {
        let (slices, heights) = plate_of(40, |l| vec![square(50.0, 50.0, if l < 10 { 30.0 } else if l < 30 { 6.0 } else { 40.0 })]);
        for bed_only in [false, true] {
            let (layers, _) = planned(&slices, &heights, 0.35, bed_only);
            let roof: f64 = layers.iter().map(|l| total_area_mm2(&l.interface)).sum();
            assert!(roof > 100.0, "a roof stays: {roof} mm2");
            let reach = CARRIED * W + 0.05;
            let past: Vec<(usize, f64)> = (1..layers.len()).map(|l| {
                let mut holds = offset_ex(&layers[l - 1].base, reach * SCALE, Join::Miter(3.0));
                holds.extend(layers[l - 1].interface.iter().cloned());
                (l, total_area_mm2(&difference_ex(&layers[l].interface, &union_ex(&holds))))
            }).filter(|x| x.1 > 0.05).collect();
            assert_eq!(past, Vec::new(), "roof over nothing (layer, mm2), from the bed only: {bed_only}");
        }
    }

    /// A branch on a slope ends on the slope, whatever the side gap: a wide side gap leaves no
    /// column stopped that far above it.
    #[test]
    fn a_branch_on_a_slope_reaches_the_slope() {
        // The cap's shadow falls far inside the slope's foot: nothing here gets past it to the bed.
        let (slices, heights) = plate_of(110, |l| vec![square(50.0, 50.0, if l < 90 { 50.0 - 2.0 * l as f64 * H } else if l < 100 { 14.0 } else { 30.0 })]);
        let (layers, _) = planned(&slices, &heights, 2.0, false);
        assert_eq!(hanging(&layers, &slices, Some(REST)), Vec::new(), "a column stops over {} mm above the part", 0.2 + LAND_AIR);
        assert_eq!(idle(&layers, &slices), Vec::new(), "these hold nothing up (layer, mm2)");
        // Under the cap's edge the slope is on layer 50.
        let first = layers.iter().position(|l| !l.base.is_empty()).expect("columns stand on the slope");
        assert!(first < 50, "the columns get down the slope: the lowest is on layer {first}");
    }

    /// A rib sweeping along a wall sweeps its side gap across what stands beside it: nothing is
    /// left hanging over where the gap passed.
    #[test]
    fn a_passing_gap_leaves_nothing_hanging() {
        let (slices, heights) = plate_of(80, |l| {
            if l >= 70 {
                return vec![rect(40.0, 30.0, 64.0, 70.0)];
            }
            let y = 30.0 + 0.5 * l as f64;
            vec![union_ex(&[rect(40.0, 30.0, 44.0, 70.0), rect(44.0, y, 46.0, y + 3.0)]).remove(0)]
        });
        let (layers, _) = planned(&slices, &heights, 2.0, false);
        assert_eq!(hanging(&layers, &slices, Some(REST)), Vec::new(), "hanging in the air");
    }

    /// How much of `layers[l]` comes within `gap` mm of the part on its own layer (round its
    /// corners too).
    fn within(layers: &[SupportLayer], slices: &[Vec<ExPolygon>], l: usize, gap: f64) -> f64 {
        total_area_mm2(&intersection_ex(&layers[l].base, &offset_ex(&slices[l], gap * SCALE, Join::Round(0.01 * SCALE))))
    }

    /// A sliver standing free beside a wall, from the bed only under a wide side gap: a tip comes in
    /// under it as close as a tip may, and the branch it hangs from leaves the wall to the side gap.
    #[test]
    fn a_floating_island_beside_a_wall_gets_a_tip() {
        let (slices, heights) = plate_of(40, |l| {
            let wall = rect(40.0, 40.0, 60.0, 60.0);
            if l >= 30 { vec![wall, rect(60.3, 48.5, 61.3, 51.5)] } else { vec![wall] }
        });
        let (layers, _) = planned(&slices, &heights, 2.0, true);
        let island = [rect(60.3, 48.5, 61.3, 51.5)];
        let under: f64 = (28..30).map(|l| total_area_mm2(&intersection_ex(&[layers[l].base.clone(), layers[l].interface.clone()].concat(), &island))).sum();
        assert!(under > 0.01, "nothing under the island: {under} mm2");
        assert_eq!(hanging(&layers, &slices, None), Vec::new(), "something stops short of the bed");
        assert_eq!(idle(&layers, &slices), Vec::new(), "these hold nothing up (layer, mm2)");
        // The tip is as wide as the island reaches round it and never narrows: it leaves the wall
        // as fast as its outline may move, its width and the side gap in at most 22 layers.
        let riding: Vec<(usize, f64)> = (0..=7).map(|l| (l, within(&layers, &slices, l, 1.95))).filter(|r| r.1 >= 0.01).collect();
        assert_eq!(riding, Vec::new(), "the branch rides inside the side gap (layer, mm2)");
    }

    /// Under a ledge off a wall, the branches next to the wall meet it only at their tips: below
    /// them they keep the branch gap, however small the side gap, once they have moved out by
    /// their width and that gap (a full-size tip never narrows to leave sooner).
    #[test]
    fn branches_leave_the_wall_below_their_tips() {
        let (slices, heights) = plate_of(70, |l| vec![if l < 60 { rect(40.0, 40.0, 44.0, 60.0) } else { rect(40.0, 40.0, 60.0, 60.0) }]);
        let (layers, _) = planned(&slices, &heights, 0.35, true);
        // The tips reach the ledge right beside the wall.
        let beside = [rect(44.35, 42.0, 45.5, 58.0)];
        let tips = total_area_mm2(&intersection_ex(&[layers[58].base.clone(), layers[58].interface.clone()].concat(), &beside));
        assert!(tips > 5.0, "the ledge beside the wall has {tips} mm2 under it");
        let m_fast = H * 40f64.to_radians().tan();
        let out = ((2.5 + BRANCH_GAP) / m_fast).ceil() as usize;
        let riding: Vec<(usize, f64)> = (0..=58 - out).map(|l| (l, within(&layers, &slices, l, BRANCH_GAP - 0.05))).filter(|r| r.1 >= 0.01).collect();
        assert_eq!(riding, Vec::new(), "rides along the wall (layer, mm2)");
    }

    /// A block at the ceiling over half of a cap: that half has no roof or tip left and nothing
    /// prints in the block (but its outline's rounding, [`OUTLINE`]), the other half keeps its
    /// support, and what it took is told.
    #[test]
    fn a_block_takes_exactly_that_support_and_says_so() {
        let (slices, heights) = plate();
        let (free, plain) = painted(&slices, &heights, None, None, false);
        let (layers, skeleton) = painted(&slices, &heights, Some([[50.0, 30.0, 13.6], [70.0, 70.0, 14.4]]), None, false);
        let (blocked, kept) = (rect(50.0, 30.0, 70.0, 70.0), rect(30.0, 30.0, 49.0, 70.0));
        for l in 66..LAYERS + 4 {
            assert!(printed_in(&layers[l], &blocked) < OUTLINE, "layer {l} prints {} mm2 in the block", printed_in(&layers[l], &blocked));
        }
        let roof = |layers: &[SupportLayer], r: &ExPolygon| -> f64 { layers.iter().map(|l| total_area_mm2(&intersection_ex(&l.interface, std::slice::from_ref(r)))).sum() };
        assert!(roof(&free, &blocked) > 300.0, "the block's half has a roof without it: {}", roof(&free, &blocked));
        assert!(roof(&layers, &blocked) < OUTLINE, "{} mm2 of roof in the block", roof(&layers, &blocked));
        assert!((roof(&layers, &kept) - roof(&free, &kept)).abs() < 0.02 * roof(&free, &kept), "the other half's roof: {} against {}", roof(&layers, &kept), roof(&free, &kept));
        let took: f64 = skeleton.blocked.iter().map(|g| g.area).sum();
        assert!((300.0..330.0).contains(&took), "the half of the cap's overhang is told: {took} mm2");
        assert!(skeleton.blocked.iter().all(|g| g.layer == LAYERS && g.at[0] > 50.0), "{:?}", skeleton.blocked);
        assert!(plain.blocked.is_empty() && plain.given_up.is_empty());
    }

    /// A block standing under a cap: nothing stands in it, and nothing is left hanging over it.
    /// With time to, a branch goes round it; right under the cap it has none, so it goes from tip to
    /// root and is told.
    #[test]
    fn nothing_stands_in_a_block_on_its_way_down() {
        let (slices, heights) = plate();
        // Far under the cap it goes round; close under it nothing has room; right up to the cap not
        // even a roof was ever there to take away.
        for (block, top, told) in [([[57.0, 30.0, 0.0], [70.0, 70.0, 10.0]], 50, 0.0), ([[50.0, 30.0, 0.0], [70.0, 70.0, 13.0]], 65, 100.0), ([[50.0, 30.0, 0.0], [70.0, 70.0, 13.95]], 70, 100.0)] {
            let (layers, skeleton) = painted(&slices, &heights, Some(block), None, false);
            let inside = rect(block[0][0], 30.0, 70.0, 70.0);
            for l in 0..top {
                assert!(printed_in(&layers[l], &inside) < OUTLINE, "layer {l} stands in the block: {} mm2", printed_in(&layers[l], &inside));
            }
            assert_eq!(hanging(&layers, &slices, Some(REST)), Vec::new(), "hanging over the block");
            assert!(printed_in(&layers[20], &rect(30.0, 30.0, 47.0, 70.0)) > 20.0, "the rest of the tree stands");
            let lost: f64 = skeleton.given_up.iter().map(|g| g.area).sum();
            assert!(lost >= told, "what could not get round it is told: {lost} mm2");
        }
    }

    /// A branch does not end on a blocked top face, not even a little above it: it gets past
    /// the face or goes.
    #[test]
    fn a_branch_does_not_rest_on_a_blocked_face() {
        let (slices, heights) = plate_of(40, |l| vec![square(50.0, 50.0, if l < 10 { 30.0 } else if l < 30 { 6.0 } else { 40.0 })]);
        let over = square(50.0, 50.0, 24.0);
        let (free, _) = painted(&slices, &heights, None, None, false);
        assert!(printed_in(&free[12], &over) > 5.0, "without it the tree stands on the plinth: {}", printed_in(&free[12], &over));
        let (layers, skeleton) = painted(&slices, &heights, Some([[30.0, 30.0, 1.8], [70.0, 70.0, 2.2]]), None, false);
        for l in 10..20 {
            assert!(printed_in(&layers[l], &over) < 1e-6, "layer {l}: {} mm2 still rests on the blocked face", printed_in(&layers[l], &over));
        }
        assert!(skeleton.given_up.iter().map(|g| g.area).sum::<f64>() > 50.0, "what stood there is told");
    }

    /// A face too steep for the angle asks for nothing. Enforced, it gets support right there and
    /// nowhere else: it reaches the bed, and stands within a few layers under the face.
    #[test]
    fn enforce_adds_support_under_a_face_the_angle_leaves_alone() {
        const STEP: f64 = 0.115; // 30 degrees off vertical; the plate's threshold wants 31.
        let (slices, heights) = plate_of(120, |l| vec![rect(40.0, 40.0, 60.0 + STEP * l as f64, 60.0)]);
        let (free, _) = painted(&slices, &heights, None, None, false);
        assert!(free.iter().all(|l| l.base.is_empty() && l.interface.is_empty()), "the angle leaves this face alone");
        let (layers, skeleton) = painted(&slices, &heights, None, Some([[50.0, 46.0, 8.0], [90.0, 54.0, 30.0]]), false);
        assert!(skeleton.given_up.is_empty(), "every enforced strip has a branch near it: {:?}", skeleton.given_up);
        assert_eq!(hanging(&layers, &slices, None), Vec::new(), "all of it reaches the bed");
        assert!(!layers[0].base.is_empty() && !layers[60].base.is_empty());
        let band = rect(58.0, 43.0, 90.0, 57.0);
        for (l, layer) in layers.iter().enumerate().skip(6) {
            let all = total_area_mm2(&layer.base);
            assert!(all - printed_in(layer, &band) < 0.05, "layer {l} has {all} mm2 of which only {} under the enforced face", printed_in(layer, &band));
        }
        // How far each layer's closest bead is under the face over it.
        for l in 50..110 {
            let near = intersection_ex(&layers[l].base, &[rect(58.0, 48.0, 90.0, 52.0)]);
            let x = bbox_of(&near).min.x_mm() + 0.01;
            let over = (l + 1..slices.len()).find(|&k| slices[k].iter().any(|e| e.contains(Pt::from_mm(x, 50.0)))).expect("under the face");
            let air = (over - l - 1) as f64 * H;
            assert!(air <= 0.8 + 1e-9, "layer {l} stands {air} mm under the face");
        }
    }

    /// An enforced face with nothing to stand on: nothing hangs, and Printability has its area. With
    /// the part allowed under it, it stands there.
    #[test]
    fn enforce_out_of_reach_is_told_not_left_hanging() {
        let (slices, heights) = plate_of(80, |l| vec![if l < 10 { rect(-100.0, -100.0, 200.0, 200.0) } else { rect(40.0, 40.0, 60.0 + 0.115 * l as f64, 60.0) }]);
        let enforce = Some([[50.0, 46.0, 8.0], [90.0, 54.0, 30.0]]);
        let (layers, skeleton) = painted(&slices, &heights, None, enforce, true);
        assert_eq!(hanging(&layers, &slices, None), Vec::new(), "nothing reaches the bed under the plinth");
        assert!(layers.iter().all(|l| l.base.is_empty()), "so nothing prints");
        assert!(skeleton.given_up.iter().chain(&skeleton.inside).map(|g| g.area).sum::<f64>() > 1.0, "and it is told");
        let (layers, skeleton) = painted(&slices, &heights, None, enforce, false);
        assert!(skeleton.given_up.is_empty() && !layers[30].base.is_empty(), "on the plinth it stands: {:?}", skeleton.given_up);
    }

    /// Paint that holds where nothing is asked moves no support.
    #[test]
    fn paint_that_misses_moves_nothing() {
        let (slices, heights) = plate();
        let (free, _) = painted(&slices, &heights, None, None, false);
        let away = Some([[130.0, 130.0, 0.0], [140.0, 140.0, 20.0]]);
        let (layers, skeleton) = painted(&slices, &heights, away, away, false);
        assert!(skeleton.blocked.is_empty() && skeleton.given_up.is_empty());
        for (l, (a, b)) in layers.iter().zip(&free).enumerate() {
            assert!(a.base == b.base && a.interface == b.interface, "layer {l} moved");
        }
    }

    /// A ball leaves narrow rings no support reaches, paint or no paint: that is not the paint's to
    /// tell, so a block that misses tells what no paint does. A block on one side tells only of what
    /// reaches that side (the ball's bottom spans the middle).
    #[test]
    fn paint_tells_only_what_it_cost() {
        let (r, n) = (12.0, 120);
        let slices: Vec<Vec<ExPolygon>> = (0..n).map(|l| {
            let z = (l as f64 + 0.5) * H - r;
            vec![ExPolygon::new(circle(50.0, 50.0, (r * r - z * z).sqrt()))]
        }).collect();
        let heights: Vec<(f64, f64)> = (0..n).map(|l| ((l + 1) as f64 * H, H)).collect();
        let (free, plain) = painted(&slices, &heights, None, None, false);
        assert!(free.iter().any(|l| !l.base.is_empty()));
        let told = |g: &[GivenUp]| g.iter().map(|g| (g.layer, (g.area * 1e3).round() as i64)).collect::<Vec<_>>();
        let (layers, away) = painted(&slices, &heights, Some([[80.0, 80.0, 0.0], [90.0, 90.0, 30.0]]), None, false);
        assert!(away.blocked.is_empty() && told(&away.given_up) == told(&plain.given_up), "a block that misses tells {:?}, no paint {:?}", away.given_up, plain.given_up);
        assert!(layers.iter().zip(&free).all(|(a, b)| a.base == b.base && a.interface == b.interface));
        let (_, side) = painted(&slices, &heights, Some([[20.0, 20.0, 0.0], [48.0, 80.0, 30.0]]), None, false);
        assert!(!side.blocked.is_empty(), "the block is on the overhang");
        // What no paint tells is told anyway, of either side.
        let anyway = |g: &GivenUp| plain.given_up.iter().any(|p| p.layer == g.layer && p.bounds.min.x <= g.bounds.min.x && p.bounds.min.y <= g.bounds.min.y && g.bounds.max.x <= p.bounds.max.x && g.bounds.max.y <= p.bounds.max.y);
        for told in side.blocked.iter().chain(side.given_up.iter().filter(|g| !anyway(g))) {
            assert!(told.bounds.min.x_mm() < 50.0, "told of the other side: {told:?}");
        }
    }

    #[test]
    fn every_island_stands_on_the_layer_below_clear_of_the_part() {
        let tree = tree();
        let (slices, _) = plate();
        let f = params(0.35, false).xy_min();
        for l in 1..tree.len() {
            let clearance = offset_ex(&slices[l], (f - 0.01) * SCALE, Join::Miter(3.0));
            let hit: f64 = intersection_ex(&tree[l].base, &clearance).iter().map(|e| e.area_mm2()).sum();
            assert!(hit < 0.01, "layer {l}: {hit} mm2 of branch within xy_min of the part");
            for island in &tree[l].base {
                let under = intersection_ex(std::slice::from_ref(island), &tree[l - 1].base);
                assert!(!under.is_empty(), "layer {l}: an island of {} mm2 floats", island.area_mm2());
            }
        }
    }
}


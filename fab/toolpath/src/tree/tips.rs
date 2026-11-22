//! Tips and roofs (R14 §4): for each layer O with an overhang, the roofs under its faces and the
//! tips that hold them, as the reference places them. The work starts on the tip layer T, the top
//! gap and one more below O.
//!
//! - §4.1 F(L): what a tip may not stand in on layer L, grown by 5 µm.
//! - §4.2 The regular area R = A − F(T), and the leftover E inside the clearance, pushed out in
//!   steps into bands of R next to it.
//! - §4.3 Each connected piece of A − F(T) walks down as a roof while the layer under each roof
//!   layer still has 0.25 mm² to stand on; a roof with no lines is cut back.
//! - §4.4 Tips are sampled on the support-base fill of the area each case hands over, at the
//!   connect length, with the small-overhang rule's outline when too few come out.
//! - §4.5 Each point is classed by the first volume it lies outside; an invalid one splits its
//!   run.
//! - §4.6-4.7 The points become tips (the 45-corner 0.01 mm base circle), forced into the roof as
//!   discs (that circle scaled to r_tip) while they may still go down, de-duplicated per layer on
//!   0.04 mm cells, lowest O first.
//!
//! Fab's keep-out, which is not the reference's ([`super::Input::keep_out`]): what of A lies in it
//! but clear of the part's own F(T) gets tips that leave it, with no roof. Each is sampled as the
//! bands are and classed against the part alone, and stays only when it gets out at the fast
//! move: on the fewest layers K at whose bottom the reach to the bed, keep-out and all, is within
//! K fast moves, the straight way there clear of the part's own avoidance on each layer. It is
//! out of the keep-out by T − K ([`Element::exit`]). Its contact is a disc of its own on T, the tip
//! under it on T − 1 as under a roof's discs, when it gets out from there too; else it stays bare
//! on T ([`place`]). The rest of A goes on as the reference's.
//!
//! Two departures are Fab's: D13, tips along a roof's edge as well as on its fill ([`edge`]); and a
//! point stuck on a roof's top layer, which the reference lays no disc, is a contact disc of its own
//! there, so its top prints solid ([`place`]).
//!
//! A run after another on the same keys takes from it what each overhang layer found where the
//! layer's faces are the same and every volume it reads, from its tip layer down, is as that
//! run's ([`Before`]).

use std::sync::{Arc, OnceLock};

use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::Join;

use super::base_fill::{fill, Fill};
use super::clip::{boolean, difference_ex, intersection_ex, union, union_ex, BoolOp, FillRule};
use super::element::{dp_ring, roof_disc, Element};
use super::offset::{offset, ring_by_ring};
use super::sample::sample;
use super::settings::Settings;
use super::volumes::{Kind, To, Volumes};
use crate::par::{Cancelled, Ctl};

/// What F grows the forbidden volume by (mm): 5 µm.
const F_GROW: f64 = 0.005;
/// F's and the leftover's corners: mitred to 1.2, which the engine takes as 2 (§1.5).
const MITRE: Join = Join::Miter(1.2);
/// The outset's grow: round, its chords within 0.01 mm of the arc.
const OUTSET_ARC: f64 = 0.01;
/// What the outset's grown areas are simplified at (mm).
const OUTSET_SIMPLIFY: f64 = 0.015;
/// The safety offset roof candidates are taken out of the regular area with (mm): 10 nm.
const CANDIDATE_SAFETY: f64 = 1e-5;
/// The roof sampling fill's angle (degrees from +X): always −45° in the reference (§4.4 quirk).
const ROOF_ANGLE: f64 = -45.0;
/// The no-roof sampling fill's angle.
const BARE_ANGLE: f64 = 0.0;
/// A tip stays round only on a run of more than this many points (§4.6).
const ROUND_RUN: usize = 5;
/// A tip stays round only when r_tip is under this many lines (§4.6).
const ROUND_LINES: f64 = 3.0;
/// D13: a roof's edge turning by more than this (degrees) at a point is a corner, and gets a tip.
const CORNER_DEG: f64 = 20.0;

/// The tips and roofs of one part.
#[derive(Default)]
pub struct Tips {
    /// Per layer, the new tips on it, in the order they were inserted.
    pub tips: Vec<Vec<Element>>,
    /// Per layer, the top-contact roof (roof layer 0 and contact discs), appended unioned per
    /// piece: overlaps stay until the interface stage unions them.
    pub contact: Vec<Vec<ExPolygon>>,
    /// Per layer, the top-interface roof (roof layers 1 and on, and interface discs).
    pub interface: Vec<Vec<ExPolygon>>,
    /// The roofs that reached the bed (§4.3): each area on layer 0, with its depth in layers.
    pub bed: Vec<(Vec<ExPolygon>, usize)>,
}

/// A sampled point's class (§4.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Class {
    to_bed: bool,
    on_top: bool,
    hole_free: bool,
    /// For a tip leaving Fab's keep-out, the layer it is out of it by.
    exit: Option<usize>,
}

/// The volumes a point is classed against: the four avoidances, then the collision.
#[derive(Clone, Copy)]
enum Vol {
    BedHoleFree,
    Bed,
    ModelHoleFree,
    Model,
    Collision,
}

/// The tests of §4.1 and §4.5 at r_tip, min kind, each layer's polygons built on first ask.
struct Tests<'a> {
    ctl: Ctl<'a>,
    v: &'a Volumes,
    s: &'a Settings,
    rung: usize,
    forbidden: Vec<OnceLock<Vec<ExPolygon>>>,
    /// F(L) of the part alone, without Fab's keep-out: only asked when there is one.
    forbidden_free: Vec<OnceLock<Vec<ExPolygon>>>,
}

/// Whether `p` lies inside `set` or on its boundary.
fn inside_or_on(set: &[ExPolygon], p: Pt) -> bool {
    set.iter().any(|e| {
        let b = e.bbox();
        if p.x < b.min.x || p.x > b.max.x || p.y < b.min.y || p.y > b.max.y {
            return false;
        }
        e.contains(p)
            || e.rings().any(|r| {
                let n = r.pts.len();
                (0..n).any(|i| {
                    let (a, c) = (r.pts[i], r.pts[(i + 1) % n]);
                    orient(a, c, p) == 0 && p.x >= a.x.min(c.x) && p.x <= a.x.max(c.x) && p.y >= a.y.min(c.y) && p.y <= a.y.max(c.y)
                })
            })
    })
}

impl<'a> Tests<'a> {
    fn new(ctl: Ctl<'a>, s: &'a Settings, v: &'a Volumes) -> Tests<'a> {
        let n = v.layers();
        let cells = || (0..n).map(|_| OnceLock::new()).collect();
        Tests { ctl, v, s, rung: s.rung_for(s.r_tip, false), forbidden: cells(), forbidden_free: cells() }
    }

    /// A volume's polygons on layer `l`, as it is (§4.5): an avoidance as its layer stands,
    /// simplified (§3.3), and to the model with the placeable areas out even where the collision
    /// is.
    fn vol(&self, which: Vol, l: usize) -> Result<&[ExPolygon], Cancelled> {
        self.vol_of(self.v, which, l)
    }

    /// [`Tests::vol`] of the volumes `v`: these, or the part's own ([`Volumes::free`]).
    fn vol_of<'v>(&'v self, v: &'v Volumes, which: Vol, l: usize) -> Result<&'v [ExPolygon], Cancelled> {
        let (kind, to) = match which {
            Vol::BedHoleFree => (Kind::FastHoleFree, To::Bed),
            Vol::Bed => (Kind::Fast, To::Bed),
            Vol::ModelHoleFree => (Kind::FastHoleFree, To::Model),
            Vol::Model => (Kind::Fast, To::Model),
            Vol::Collision => return v.collision(self.ctl, l, self.rung),
        };
        v.avoidance(self.ctl, kind, to, self.rung).blocked(l)
    }

    fn outside(&self, which: Vol, l: usize, p: Pt) -> Result<bool, Cancelled> {
        Ok(!inside_or_on(self.vol(which, l)?, p))
    }

    /// F(L) (§4.1): bed only, the fast avoidance to the bed (the collision on layer 0); else the
    /// collision; unioned, then grown by 5 µm.
    fn forbidden(&self, l: usize) -> Result<&[ExPolygon], Cancelled> {
        self.forbidden_of(self.v, &self.forbidden[l], l)
    }

    /// F(L) of the part alone, without Fab's keep-out.
    fn forbidden_free(&self, l: usize) -> Result<&[ExPolygon], Cancelled> {
        self.forbidden_of(self.v.free(), &self.forbidden_free[l], l)
    }

    fn forbidden_of<'v>(&'v self, v: &'v Volumes, cell: &'v OnceLock<Vec<ExPolygon>>, l: usize) -> Result<&'v [ExPolygon], Cancelled> {
        if let Some(f) = cell.get() {
            return Ok(f);
        }
        let raw = self.vol_of(v, if self.s.keys.bed_only && l > 0 { Vol::Bed } else { Vol::Collision }, l)?;
        let f = offset(&union_ex(raw), F_GROW * SCALE, MITRE);
        Ok(cell.get_or_init(|| f))
    }

    /// The class of a point in Fab's keep-out on layer `l`, none when it is invalid: to the bed
    /// past the part alone, and getting out of the keep-out ([`Tests::exit`]).
    fn leaving_class(&self, p: Pt, l: usize) -> Result<Option<Class>, Cancelled> {
        let free = self.v.free();
        let hole_free = !inside_or_on(self.vol_of(free, Vol::BedHoleFree, l)?, p);
        if !hole_free && inside_or_on(self.vol_of(free, Vol::Bed, l)?, p) {
            return Ok(None);
        }
        Ok(self.exit(p, l)?.map(|exit| Class { to_bed: true, on_top: true, hole_free, exit: Some(exit) }))
    }

    /// The layer a tip at `p` on layer `l`, in Fab's keep-out, is out of it by, none when it
    /// does not get out: the fewest layers K for which the fast avoidance to the bed, keep-out
    /// and all, at the radius K layers under a tip with the full side gap, leaves a point within
    /// K fast moves of `p` on layer `l` − K; and the straight way there, K equal steps, clear of
    /// the part's own fast avoidance to the bed at each step's radius.
    fn exit(&self, p: Pt, l: usize) -> Result<Option<usize>, Cancelled> {
        let s = self.s;
        let kept = |k: usize| self.v.avoidance(self.ctl, Kind::Fast, To::Bed, s.rung_for(s.radius(k, 0.0), true));
        // No farther than a branch gets by the bed.
        let most = l as f64 * s.fast;
        let mut k = 0;
        let (k, q) = loop {
            let Some(e) = l.checked_sub(k) else { return Ok(None) };
            let Some(q) = kept(k).nearest(p, e, most)? else { return Ok(None) };
            let need = ((p.dist(q) / SCALE) / s.fast).ceil() as usize;
            if need <= k {
                break (k, q);
            }
            k = need;
        };
        let free = self.v.free();
        for j in 1..=k {
            let f = j as f64 / k as f64;
            let at = Pt::new(p.x + ((q.x - p.x) as f64 * f).round() as i64, p.y + ((q.y - p.y) as f64 * f).round() as i64);
            if !free.avoidance(self.ctl, Kind::Fast, To::Bed, s.rung_for(s.radius(j, 0.0), false)).reached(at, l - j)? {
                return Ok(None);
            }
        }
        Ok(Some(l - k))
    }

    /// The point's class on layer `l` (§4.5), none when it is invalid. Both to-bed classes are
    /// on-top too.
    fn class(&self, p: Pt, l: usize) -> Result<Option<Class>, Cancelled> {
        let c = |to_bed, on_top, hole_free| Some(Class { to_bed, on_top, hole_free, exit: None });
        if self.outside(Vol::BedHoleFree, l, p)? {
            return Ok(c(true, true, true));
        }
        if self.outside(Vol::Bed, l, p)? {
            return Ok(c(true, true, false));
        }
        if self.s.keys.bed_only {
            return Ok(None);
        }
        if self.outside(Vol::ModelHoleFree, l, p)? {
            return Ok(c(false, true, true));
        }
        if self.outside(Vol::Model, l, p)? {
            return Ok(c(false, true, false));
        }
        Ok(if self.outside(Vol::Collision, l, p)? { c(false, false, false) } else { None })
    }

    /// Whether a point of class `c` is still valid on layer `l` (§4.5): any class outside the
    /// to-bed avoidance there (hole-free for to-bed hole-free points) passes; then a to-model
    /// point tries its own volume.
    fn still_valid(&self, p: Pt, c: Class, l: usize) -> Result<bool, Cancelled> {
        if self.outside(if c.to_bed && c.hole_free { Vol::BedHoleFree } else { Vol::Bed }, l, p)? {
            return Ok(true);
        }
        if c.to_bed || self.s.keys.bed_only {
            return Ok(false);
        }
        if c.on_top {
            return self.outside(if c.hole_free { Vol::ModelHoleFree } else { Vol::Model }, l, p);
        }
        self.outside(Vol::Collision, l, p)
    }
}

/// A run of classed points: a line's points between invalid ones, in the sampler's order.
type Run = Vec<(Pt, Class)>;

/// What the placer is handed for one area (§4.7): its runs, where they go in, and the counts.
struct Handed {
    /// The roof piece it is: its roofs' and tips' ([`Pieces`]).
    piece: usize,
    runs: Vec<Run>,
    /// The insert layer I.
    at: usize,
    /// The roof-tip count K.
    k: usize,
    /// The roof layers above, D.
    d: usize,
    /// The hold H.
    hold: usize,
}

/// What one overhang layer gives, in the order the reference inserts it.
#[derive(Default)]
struct Found {
    tips: Vec<Element>,
    /// (layer, contact, polygons).
    roofs: Vec<(usize, bool, Vec<ExPolygon>)>,
    bed: Vec<(Vec<ExPolygon>, usize)>,
    /// The piece of each tip and of each roof, in their places.
    tip_pieces: Vec<usize>,
    roof_pieces: Vec<usize>,
    /// The pieces numbered so far.
    pieces: usize,
}

impl Found {
    /// A new piece's number.
    fn piece(&mut self) -> usize {
        self.pieces += 1;
        self.pieces - 1
    }

    fn tip(&mut self, e: Element, piece: usize) {
        self.tips.push(e);
        self.tip_pieces.push(piece);
    }

    fn roof(&mut self, l: usize, contact: bool, piece: usize, polys: Vec<ExPolygon>) {
        self.roofs.push((l, contact, polys));
        self.roof_pieces.push(piece);
    }
}

/// What each overhang layer found, lowest first, for a run after to take ([`Before`]), and on
/// how many layers with faces it found it afresh.
#[derive(Default)]
pub struct Under {
    found: Vec<Arc<Found>>,
    /// The overhang layers with faces it found tips and roofs under afresh, rather than took
    /// ([`super::Work::tipped`]).
    pub(super) made: usize,
}

/// The run before's tips on the same keys, for a run after to take from ([`tips`]): the faces it
/// found them under, whether its volumes kept anything out, and what it found.
pub struct Before<'a> {
    pub overhangs: &'a [Vec<ExPolygon>],
    pub keeps_out: bool,
    pub under: &'a Under,
}

/// Which roof piece each tip and roof polygon of a [`Tips`] came from, in the same places: one
/// piece per area §4.7's placer is handed (a roof candidate of §4.3, or a band of §4.2), so a
/// piece's roofs can go when all its tips do ([`without`]).
#[derive(Default)]
pub struct Pieces {
    pub tips: Vec<Vec<usize>>,
    pub contact: Vec<Vec<usize>>,
    pub interface: Vec<Vec<usize>>,
    /// How many pieces there are.
    pub count: usize,
}

/// `tips` without the tips `gone` marks (per layer, by place), and without the contact and
/// interface roofs of every piece that had tips and has none left.
pub fn without(tips: &Tips, pieces: &Pieces, gone: &[Vec<bool>]) -> Tips {
    let mut left = vec![(0usize, 0usize); pieces.count];
    for (l, at) in pieces.tips.iter().enumerate() {
        for (i, &p) in at.iter().enumerate() {
            left[p].0 += 1;
            left[p].1 += !gone[l][i] as usize;
        }
    }
    let goes = |p: usize| left[p].0 > 0 && left[p].1 == 0;
    let keep = |set: &[ExPolygon], of: &[usize]| set.iter().zip(of).filter(|&(_, &p)| !goes(p)).map(|(e, _)| e.clone()).collect::<Vec<_>>();
    Tips {
        tips: gone.iter().enumerate().map(|(l, g)| (0..g.len()).filter(|&i| !g[i]).map(|i| tips.tips[l][i].clone()).collect()).collect(),
        contact: tips.contact.iter().zip(&pieces.contact).map(|(c, of)| keep(c, of)).collect(),
        interface: tips.interface.iter().zip(&pieces.interface).map(|(c, of)| keep(c, of)).collect(),
        bed: tips.bed.clone(),
    }
}

/// The tips and roofs of the part whose volumes are `v`, under the overhangs of §2 per layer,
/// and the piece each came from; taking what it can from `before`, the run before's on the same
/// keys whose volumes `v` take from ([`Volumes::taking`]). With what each overhang layer found,
/// for a run after.
pub fn tips(ctl: Ctl, s: &Settings, v: &Volumes, overhangs: &[Vec<ExPolygon>], before: Option<&Before>) -> Result<(Tips, Pieces, Under), Cancelled> {
    crate::prof::scope_fanned(crate::prof::Stage::TreeTips, || {
        let t = Tests::new(ctl, s, v);
        let layers = v.layers();
        let of = overhangs.len().min(layers);
        // Overhang layer `o` reads its faces, whether anything is kept out, and the volumes from
        // its tip layer, the top gap and one more under it, down.
        let taken = |o: usize| before.filter(|b| b.keeps_out == v.keeps_out() && o < v.same_below() + s.top_gap_layers + 1 && b.overhangs.get(o) == Some(&overhangs[o])).and_then(|b| b.under.found.get(o).cloned());
        let found = crate::par::try_map(ctl, of, |o| -> Result<(Arc<Found>, bool), Cancelled> {
            let (f, fresh) = match taken(o) {
                Some(f) => (f, false),
                None => (Arc::new(overhang(&t, &overhangs[o], o)?), true),
            };
            super::tell(ctl, super::Stage::Tips, o, of, super::Grown::Tips { tips: &f.tips, roofs: &f.roofs });
            Ok((f, fresh))
        })?
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
        let made = found.iter().zip(overhangs).filter(|((_, fresh), faces)| *fresh && !faces.is_empty()).count();
        let found: Vec<Arc<Found>> = found.into_iter().map(|(f, _)| f).collect();
        let (tips, pieces) = gather(&found, s.dedup_cell, layers);
        Ok((tips, pieces, Under { found, made }))
    })
}

/// What each overhang layer found, lowest first, gathered per layer. A tip is de-duplicated on
/// its insert layer (§4.6): its cell is its nm coordinates each divided by `cell` and truncated
/// toward zero, and the first tip in a cell wins. The pieces are numbered on across the layers.
fn gather(found: &[Arc<Found>], cell: i64, layers: usize) -> (Tips, Pieces) {
    let mut out = Tips { tips: vec![Vec::new(); layers], contact: vec![Vec::new(); layers], interface: vec![Vec::new(); layers], bed: Vec::new() };
    let mut of = Pieces { tips: vec![Vec::new(); layers], contact: vec![Vec::new(); layers], interface: vec![Vec::new(); layers], count: 0 };
    let mut cells: Vec<std::collections::HashSet<(i64, i64)>> = vec![Default::default(); layers];
    for f in found {
        let first = of.count;
        for (e, &p) in f.tips.iter().zip(&f.tip_pieces) {
            if cells[e.layer].insert((e.point.x / cell, e.point.y / cell)) {
                of.tips[e.layer].push(first + p);
                out.tips[e.layer].push(e.clone());
            }
        }
        for ((l, contact, polys), &p) in f.roofs.iter().zip(&f.roof_pieces) {
            let (to, at) = if *contact { (&mut out.contact[*l], &mut of.contact[*l]) } else { (&mut out.interface[*l], &mut of.interface[*l]) };
            at.extend(std::iter::repeat_n(first + p, polys.len()));
            to.extend(polys.iter().cloned());
        }
        out.bed.extend(f.bed.iter().cloned());
        of.count += f.pieces;
    }
    (out, of)
}

/// The tips and roofs under overhang layer `o`'s faces `a`.
fn overhang(t: &Tests, a: &[ExPolygon], o: usize) -> Result<Found, Cancelled> {
    let s = t.s;
    let mut found = Found::default();
    let gap = s.top_gap_layers;
    if a.is_empty() || o < gap + 2 {
        return Ok(found);
    }
    let tl = o - gap - 1;
    let n = s.keys.roof_layers;
    // Fab's keep-out: what of A lies in it, clear of the part's own F(T), is left to tips
    // leaving it; the rest goes on as the reference's.
    let (rest, leaving);
    let a = if t.v.keeps_out() {
        let kept = difference_ex(t.forbidden(tl)?, t.forbidden_free(tl)?);
        (rest, leaving) = (difference_ex(a, &kept), intersection_ex(a, &kept));
        &rest[..]
    } else {
        leaving = Vec::new();
        a
    };
    let ft = t.forbidden(tl)?;
    let w = s.keys.line_width;
    // §4.2: the regular area and the leftover inside the clearance, pushed outward in steps.
    let candidates = difference_ex(a, ft);
    let mut r = candidates.clone();
    let mut e = intersection_ex(&difference_ex(a, &offset(&candidates, w / 2.0 * SCALE, MITRE)), ft);
    let c0 = t.v.collision(t.ctl, tl, s.rung_for(0.0, false))?;
    let cap = s.hidden_outset;
    let mut total = 0.0;
    // Enforce the strict cap on the polygons' grid: eight 0.05 mm float steps sum
    // to just under 0.4 mm and would otherwise take the step the cap excludes.
    while !e.is_empty() && ((total + w / 8.0) * SCALE).round() < (cap * SCALE).round() {
        let step = if total + 2.0 * w > s.r_tip {
            w / 8.0
        } else if s.r_tip < w {
            s.r_tip / 2.0
        } else {
            (s.r_tip * s.r_tip - (s.r_tip - w / 2.0).powi(2)).sqrt()
        };
        total += step;
        e = difference_ex(&e, &outset(&r, (1.5 * total * SCALE).trunc(), c0));
        r = union2(&r, &difference_ex(&outset(&e, total * SCALE, c0), ft));
    }
    let forced = n > 0 && std::f64::consts::PI * s.r_tip * s.r_tip > s.keys.interface_min_area;
    let k_bare = if forced { n } else { 0 };
    let hold_bare = n;
    let mut handed = Vec::new();
    let bare = if n > 0 {
        // §4.2 "What R becomes": the candidates come out, each ring grown by 10 nm on its own (a
        // hole shrinks and stays a hole, or goes if too small), under the non-zero rule; the rest
        // are the bands.
        let safe = ring_by_ring(&candidates, CANDIDATE_SAFETY * SCALE, Join::Miter(3.0));
        let bands = boolean(&to_polygons(&r), &safe, BoolOp::Difference, FillRule::NonZero);
        for c in &candidates {
            roof(t, std::slice::from_ref(c), tl, &mut found, &mut handed)?;
        }
        bands
    } else {
        r
    };
    for piece in &bare {
        let area = std::slice::from_ref(piece);
        let runs = classed(t, &sampled(s, area, bare_fill(s), s.connect_length, true).0, tl)?;
        handed.push(Handed { piece: found.piece(), runs, at: tl, k: k_bare, d: 0, hold: if n > 0 { hold_bare } else { 0 } });
    }
    // Fab's: the tips leaving the keep-out, sampled as the bands are, with no roof and no hold:
    // each its own contact disc ([`place`]).
    for piece in &leaving {
        let (lines, _) = sampled(s, std::slice::from_ref(piece), bare_fill(s), s.connect_length, true);
        let runs = runs_of(&lines, |p| t.leaving_class(p, tl))?;
        handed.push(Handed { piece: found.piece(), runs, at: tl, k: 0, d: 0, hold: 0 });
    }
    for h in handed {
        place(t, h, &mut found)?;
    }
    Ok(found)
}

/// A set grown by `by` (units) as the outset grows it (§4.2): round, simplified at 0.015 mm ring
/// by ring, each ring cleaned on its own, then less `c0`.
fn outset(set: &[ExPolygon], by: f64, c0: &[ExPolygon]) -> Vec<ExPolygon> {
    if set.is_empty() {
        return Vec::new();
    }
    let grown = offset(set, by, Join::Round(OUTSET_ARC * SCALE));
    let mut rings = Vec::new();
    for ex in &grown {
        for (k, ring) in ex.rings().enumerate() {
            let pts = dp_ring(&ring.pts, OUTSET_SIMPLIFY * SCALE);
            if pts.len() < 3 {
                continue;
            }
            for clean in union(&[Polygon::new(pts)], FillRule::NonZero) {
                let mut c = clean.contour;
                if k > 0 {
                    // A hole cleaned alone comes out as an outer ring: turned back to clockwise.
                    c.reverse();
                }
                rings.push(c);
            }
        }
    }
    boolean(&rings, &to_polygons(c0), BoolOp::Difference, FillRule::NonZero)
}

fn union2(a: &[ExPolygon], b: &[ExPolygon]) -> Vec<ExPolygon> {
    let mut all = a.to_vec();
    all.extend_from_slice(b);
    union_ex(&all)
}

fn roof_fill(s: &Settings) -> Fill {
    Fill { angle_deg: ROOF_ANGLE, spacing: s.roof_line_spacing, flow: s.flow_spacing, frozen: s.roof_line_spacing, anchor: None }
}

fn bare_fill(s: &Settings) -> Fill {
    // §4.4 steps 6, 9 and 10 quirk: the limits stay frozen at the roof's L.
    Fill { angle_deg: BARE_ANGLE, spacing: s.tip_line_spacing, flow: s.flow_spacing, frozen: s.roof_line_spacing, anchor: None }
}

/// The points sampled on `area`'s fill, line by line in the sampler's order (§4.4), at `spacing`;
/// with `small`, the small-overhang rule: when the whole area gives at most m points, every ring
/// as a closed line at the connect length instead, at least m points each. And whether it is
/// the rings.
fn sampled(s: &Settings, area: &[ExPolygon], f: Fill, spacing: f64, small: bool) -> (Vec<Vec<Pt>>, bool) {
    let lines = fill(area, &f);
    let pts: Vec<Vec<Pt>> = lines.iter().map(|l| sample(l, spacing, 1)).collect();
    if !small {
        return (pts, false);
    }
    let count: usize = pts.iter().map(Vec::len).sum();
    let ring_len: f64 = area.iter().flat_map(|e| e.rings()).map(|r| r.perimeter() / SCALE).sum();
    let m = ((ring_len / s.connect_length).floor() as usize).clamp(1, 3);
    if count > m {
        return (pts, false);
    }
    let rings = area
        .iter()
        .flat_map(|e| e.rings())
        .map(|r| {
            let mut closed = r.pts.clone();
            closed.push(r.pts[0]);
            sample(&closed, s.connect_length, m)
        })
        .collect();
    (rings, true)
}

/// The points a roof over `area` hands its tips (§4.4): its roof fill's, sampled at the connect
/// length (with `small`, the small-overhang rule), and where that gives the fill's lines, D13's
/// along its edge after them.
fn roofed(s: &Settings, area: &[ExPolygon], small: bool) -> Vec<Vec<Pt>> {
    let (mut lines, rings) = sampled(s, area, roof_fill(s), s.connect_length, small);
    if !rings {
        lines.extend(edge(s, area));
    }
    lines
}

/// D13, Fab's, not the reference's: tips along a roof's edge. The fill's outermost points can sit
/// most of a connect length inside `area`'s edge, so the roof's lowest layer would reach past its
/// tips by more than the steep angle carries there. Each ring of `area` shrunk by r_tip, where a
/// tip's disc fits inside the edge and touches it, gets a point at each corner ([`CORNER_DEG`])
/// and points evenly between, the discs touching (2 r_tip apart at most): one line from each
/// corner to the next, or the whole ring where it has none.
fn edge(s: &Settings, area: &[ExPolygon]) -> Vec<Vec<Pt>> {
    let step = 2.0 * s.r_tip * SCALE;
    let shrunk = offset(area, -s.r_tip * SCALE, Join::Round(OUTSET_ARC * SCALE));
    shrunk.iter().flat_map(|e| e.rings()).flat_map(|r| stretches(&r.pts, step)).collect()
}

/// `ring` (its last point joined to its first) cut at its corners, the points where it turns
/// toward its inside, the left, by more than [`CORNER_DEG`]: each stretch from a corner round to
/// the next, its points evenly apart and at most `step` (units), the next corner left to the
/// next stretch. With no corner, one stretch from its first point all the way round.
fn stretches(ring: &[Pt], step: f64) -> Vec<Vec<Pt>> {
    let n = ring.len();
    if n < 3 {
        return Vec::new();
    }
    let turn = CORNER_DEG.to_radians();
    let corner = |i: usize| {
        let (a, b, c) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
        let (ux, uy, vx, vy) = ((b.x - a.x) as f64, (b.y - a.y) as f64, (c.x - b.x) as f64, (c.y - b.y) as f64);
        (ux * vy - uy * vx).atan2(ux * vx + uy * vy) > turn
    };
    let mut starts: Vec<usize> = (0..n).filter(|&i| corner(i)).collect();
    if starts.is_empty() {
        starts.push(0);
    }
    (0..starts.len())
        .map(|j| {
            let (from, to) = (starts[j], starts[(j + 1) % starts.len()]);
            let mut path = vec![ring[from]];
            let mut k = from;
            loop {
                k = (k + 1) % n;
                path.push(ring[k]);
                if k == to {
                    break;
                }
            }
            evenly(&path, step)
        })
        .collect()
}

/// Points along `path` from its first point, evenly apart and at most `step` (units), up to but
/// not at its last.
fn evenly(path: &[Pt], step: f64) -> Vec<Pt> {
    let len: f64 = path.windows(2).map(|w| w[0].dist(w[1])).sum();
    let parts = ((len / step).ceil() as usize).max(1);
    let mut out = Vec::with_capacity(parts);
    let mut at = 0.0;
    for w in path.windows(2) {
        let l = w[0].dist(w[1]);
        while out.len() < parts {
            let want = len * out.len() as f64 / parts as f64;
            if want > at + l {
                break;
            }
            let t = if l > 0.0 { (want - at) / l } else { 0.0 };
            out.push(Pt::new(w[0].x + ((w[1].x - w[0].x) as f64 * t).round() as i64, w[0].y + ((w[1].y - w[0].y) as f64 * t).round() as i64));
        }
        at += l;
    }
    out
}

/// Each line's points classed on layer `l`, split into runs at the invalid ones.
fn classed(t: &Tests, lines: &[Vec<Pt>], l: usize) -> Result<Vec<Run>, Cancelled> {
    runs_of(lines, |p| t.class(p, l))
}

/// Each line's points classed by `class`, split into runs at the invalid ones.
fn runs_of(lines: &[Vec<Pt>], class: impl Fn(Pt) -> Result<Option<Class>, Cancelled>) -> Result<Vec<Run>, Cancelled> {
    let mut runs = Vec::new();
    for line in lines {
        let mut run = Vec::new();
        for &p in line {
            match class(p)? {
                Some(c) => run.push((p, c)),
                None => {
                    if !run.is_empty() {
                        runs.push(std::mem::take(&mut run));
                    }
                }
            }
        }
        if !run.is_empty() {
            runs.push(run);
        }
    }
    Ok(runs)
}

/// Whether `area`'s roof sampling fill puts out anything at all (§4.3's lines check).
fn has_lines(s: &Settings, area: &[ExPolygon]) -> bool {
    !fill(area, &roof_fill(s)).is_empty()
}

/// §4.3's roof walk down from the tip layer `tl` for roof candidate `c0`, its roof layers into
/// `found`, and what §4.4's cases hand the placer into `handed`.
fn roof(t: &Tests, c0: &[ExPolygon], tl: usize, found: &mut Found, handed: &mut Vec<Handed>) -> Result<(), Cancelled> {
    let s = t.s;
    let n = s.keys.roof_layers;
    let forced = std::f64::consts::PI * s.r_tip * s.r_tip > s.keys.interface_min_area;
    let kf = |d: usize| if forced { n - d } else { 0 };
    let piece = found.piece();
    // cs[k] is c_k on layer T − k; the walk stops at step k when c_(k+1) is under the least area.
    let mut cs: Vec<Vec<ExPolygon>> = vec![c0.to_vec()];
    let mut stop = None;
    for k in 0..n {
        if tl < k + 1 {
            break;
        }
        let next = difference_ex(&cs[k], t.forbidden(tl - k - 1)?);
        if total_area_mm2(&next) < s.keys.interface_min_area {
            stop = Some(k);
            break;
        }
        cs.push(next);
    }
    let mut d = cs.len() - 1;
    let roofs = |found: &mut Found, d: usize| {
        for (k, c) in cs.iter().enumerate().take(d) {
            found.roof(tl - k, k == 0, piece, c.clone());
        }
    };
    if d >= tl {
        // §4.3 "A roof reaching the bed": the rest is a roof on the bed and no tips.
        roofs(found, d);
        found.bed.push((cs[d].clone(), d.min(n)));
        return Ok(());
    }
    if let Some(k) = stop.filter(|&k| k >= 1) {
        // Case 2: the last roof layer's lines, the points still valid a layer down.
        let lines = roofed(s, &cs[k - 1], false);
        let mut runs = Vec::new();
        for run in classed(t, &lines, tl - (k - 1))? {
            let mut kept = Vec::new();
            for (p, c) in run {
                if t.still_valid(p, c, tl - k)? {
                    kept.push((p, c));
                } else if !kept.is_empty() {
                    runs.push(std::mem::take(&mut kept));
                }
            }
            if !kept.is_empty() {
                runs.push(kept);
            }
        }
        if !runs.is_empty() {
            roofs(found, k);
            handed.push(Handed { piece, runs, at: tl - k, k: kf(k), d: k, hold: n - k });
            return Ok(());
        }
        d = k;
    }
    // The lines check: a roof whose lowest area gives no lines is cut at its first layer from
    // the top that gives none.
    let walked = d;
    if d > 0 && !has_lines(s, &cs[d]) {
        if let Some(j) = (0..d).find(|&j| !has_lines(s, &cs[j])) {
            d = j;
        }
    }
    let area = &cs[walked];
    if d < walked && d > 0 {
        // After a cut to 0 < d < N: c_D's roof fill (no lines, so its outline), classed and
        // inserted at T − d.
        roofs(found, d);
        let runs = classed(t, &roofed(s, area, true), tl - d)?;
        handed.push(Handed { piece, runs, at: tl - d, k: kf(d), d, hold: n - d });
        return Ok(());
    }
    if d == 0 {
        // Case 4 (or a cut to 0): the no-roof fill of the current area at r_tip ÷ 2.
        let (lines, _) = sampled(s, area, bare_fill(s), s.r_tip / 2.0, true);
        let runs = classed(t, &lines, tl)?;
        handed.push(Handed { piece, runs, at: tl, k: kf(0), d: 0, hold: n });
        return Ok(());
    }
    // Case 1, or case 2 that kept nothing: c_D's roof lines, classed and inserted at T − D.
    roofs(found, d);
    let runs = classed(t, &roofed(s, area, true), tl - d)?;
    handed.push(Handed { piece, runs, at: tl - d, k: kf(d), d, hold: n - d });
    Ok(())
}

/// §4.7: the points of `h` become tips, forced into the roof as discs while they may still go
/// down, and the rest tips under them.
fn place(t: &Tests, h: Handed, found: &mut Found) -> Result<(), Cancelled> {
    let s = t.s;
    let tip = |l: usize, p: Pt, c: Class, hold: usize, discs: usize, under: bool, round: bool| {
        let mut e = Element::tip(s, l, p);
        e.to_bed = c.to_bed;
        e.on_top = c.on_top;
        e.hole_free = c.hole_free;
        e.exit = c.exit.unwrap_or(l);
        e.hold = hold;
        e.roof_discs = discs;
        e.under_roof = under;
        e.round = round;
        e
    };
    let rounds = h.k.min(h.at);
    let mut runs = h.runs;
    for j in 0..rounds {
        let l = h.at - j;
        let mut next = Vec::new();
        for run in runs {
            let mut on = Vec::new();
            for (p, c) in run {
                if t.still_valid(p, c, l - 1)? {
                    found.roof(l, j + h.d == 0, h.piece, vec![roof_disc(p, s.r_tip)]);
                    on.push((p, c));
                } else if j + h.d == 0 {
                    // Fab's: a point stuck on the roof's top layer has nothing over it, and the
                    // reference lays it no disc. Its contact is a disc of its own piece, so its top
                    // prints solid (R4) and the disc goes with the tip.
                    let piece = found.piece();
                    found.roof(l, true, piece, vec![roof_disc(p, s.r_tip)]);
                    found.tip(tip(l, p, c, h.k, h.k, false, false), piece);
                    if !on.is_empty() {
                        next.push(std::mem::take(&mut on));
                    }
                } else {
                    found.tip(tip(l, p, c, h.k - j, h.k - j, j + h.d > 0, false), h.piece);
                    if !on.is_empty() {
                        next.push(std::mem::take(&mut on));
                    }
                }
            }
            if !on.is_empty() {
                next.push(on);
            }
        }
        runs = next;
    }
    let l = h.at - rounds;
    let short = rounds < h.k;
    let (hold, discs) = if short { (h.hold - rounds, h.hold - rounds) } else { (h.hold.saturating_sub(h.k), 0) };
    for run in runs {
        let round = h.k == 0 && s.r_tip < ROUND_LINES * s.keys.line_width && run.len() > ROUND_RUN;
        for &(p, c) in &run {
            if c.exit.is_some() {
                // Fab's: a tip leaving the keep-out has no roof over it. Its contact is a disc of
                // its own piece, so the disc goes with the tip, and the tip goes under it a layer
                // down, as under a roof's discs, while it still gets out from there.
                let piece = found.piece();
                let under = if l > 0 { t.leaving_class(p, l - 1)? } else { None };
                match under {
                    Some(under) => {
                        found.roof(l, true, piece, vec![roof_disc(p, s.r_tip)]);
                        found.tip(tip(l - 1, p, under, 0, 0, true, false), piece);
                    }
                    None => found.tip(tip(l, p, c, hold, discs, rounds + h.d > 0, round), piece),
                }
                continue;
            }
            found.tip(tip(l, p, c, hold, discs, rounds + h.d > 0, round), h.piece);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::settings::tests::r14;
    use super::super::settings::Keys;
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1)))
    }

    fn ctl() -> Ctl<'static> {
        Ctl::default()
    }

    /// R14's profile from the bed only, at a 0.8 mm side gap.
    fn bed_only() -> Settings {
        Settings::new(Keys { bed_only: true, ..r14(0.8).keys })
    }

    /// A 2 mm pillar up to layer #24 under a slab from #25; the face `face` on #25 only.
    fn scene(face: ExPolygon) -> (Vec<Vec<ExPolygon>>, Vec<Vec<ExPolygon>>) {
        let part = (0..30).map(|l| if l < 25 { vec![rect(0.0, 0.0, 2.0, 2.0)] } else { vec![rect(-8.0, -8.0, 10.0, 10.0)] }).collect();
        let over = (0..30).map(|l| if l == 25 { vec![face.clone()] } else { Vec::new() }).collect();
        (part, over)
    }

    fn area(set: &[ExPolygon]) -> f64 {
        total_area_mm2(set)
    }

    /// §4.3-4.6, case 1: a broad face clear of the part gets a full 2-layer roof on T = O − 2 and
    /// T − 1, and its tips start under it on T − 2 = O − 4, with no hold and no roof discs, to the
    /// bed and hole-free, round on runs of more than 5 points.
    #[test]
    fn a_broad_face_gets_a_full_roof_and_round_tips_under_it() {
        let s = bed_only();
        let face = rect(4.0, -8.0, 10.0, 10.0);
        let (part, over) = scene(face.clone());
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let (t, _, _) = tips(ctl(), &s, &v, &over, None).unwrap();
        assert!((area(&t.contact[23]) - 108.0).abs() < 1e-6, "roof layer 0, the whole face, on T: {}", area(&t.contact[23]));
        assert!((area(&t.interface[22]) - 108.0).abs() < 1e-6, "roof layer 1 on T − 1");
        let layers: Vec<usize> = (0..30).filter(|&l| !t.tips[l].is_empty()).collect();
        assert_eq!(layers, vec![21], "every tip on T − 2");
        let tips = &t.tips[21];
        assert!(tips.len() > 10, "a 6 by 18 mm roof's lines at 1.78 mm: {}", tips.len());
        assert!(tips.iter().all(|e| e.under_roof && e.hold == 0 && e.roof_discs == 0 && e.to_bed && e.on_top && e.hole_free), "under the roof, no hold or discs, to the bed, on-top and hole-free");
        assert!(tips.iter().all(|e| face.contains(e.point)), "on the face's lines");
        assert!(tips.iter().any(|e| e.round), "long runs keep their tips round");
    }

    /// A run after an edit takes what each overhang layer found where the layer's faces and the
    /// volumes it reads are the run before's: two slabs on 2 mm pillars, a broad face under each,
    /// and the upper slab widened by 2 mm. The lower face's tips and roofs are taken, the upper
    /// face's found afresh, and all are what a fresh run finds.
    #[test]
    fn an_edit_over_a_face_takes_its_tips() {
        let s = bed_only();
        let scene = |wide: f64| -> (Vec<Vec<ExPolygon>>, Vec<Vec<ExPolygon>>) {
            let part = (0..45)
                .map(|l| match l {
                    25..=29 => vec![rect(-8.0, -8.0, 10.0, 10.0)],
                    40..=44 => vec![rect(-8.0, -8.0, 10.0 + wide, 10.0)],
                    _ => vec![rect(0.0, 0.0, 2.0, 2.0)],
                })
                .collect();
            let over = (0..45)
                .map(|l| match l {
                    25 => vec![rect(4.0, -8.0, 10.0, 10.0)],
                    40 => vec![rect(4.0, -8.0, 10.0 + wide, 10.0)],
                    _ => Vec::new(),
                })
                .collect();
            (part, over)
        };
        let ((part, over), (edited, faces)) = (scene(0.0), scene(2.0));
        let was = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let (_, _, under) = tips(ctl(), &s, &was, &over, None).unwrap();
        let v = Volumes::new(ctl(), &s, &edited, &[]).unwrap().taking(&was);
        let before = Before { overhangs: &over, keeps_out: false, under: &under };
        let (warm, _, taken) = tips(ctl(), &s, &v, &faces, Some(&before)).unwrap();
        let (fresh, _, afresh) = tips(ctl(), &s, &Volumes::new(ctl(), &s, &edited, &[]).unwrap(), &faces, None).unwrap();
        let all = |t: &Tips| format!("{:?} {:?} {:?} {:?}", t.tips, t.contact, t.interface, t.bed);
        assert!(all(&warm) == all(&fresh), "what a fresh run finds");
        assert!(!warm.tips[21].is_empty() && !warm.tips[36].is_empty(), "tips under both faces");
        assert_eq!((afresh.made, taken.made), (2, 1), "the lower face's taken, the upper's found afresh");
    }

    /// D13: the broad face's roof stands on tips along its edge too. The face shrunk by r_tip has
    /// a tip at each corner, and every point of it lies within r_tip of a tip, the discs touching
    /// all round.
    #[test]
    fn a_roofs_edge_stands_on_tips_all_round() {
        let s = bed_only();
        let face = rect(4.0, -8.0, 10.0, 10.0);
        let (part, over) = scene(face.clone());
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let (t, _, _) = tips(ctl(), &s, &v, &over, None).unwrap();
        let pts: Vec<Pt> = t.tips[21].iter().map(|e| e.point).collect();
        let nearest = |q: Pt| pts.iter().map(|p| p.dist(q) / SCALE).fold(f64::MAX, f64::min);
        let r = s.r_tip;
        let (x0, y0, x1, y1) = (4.0 + r, -8.0 + r, 10.0 - r, 10.0 - r);
        for (x, y) in [(x0, y0), (x1, y0), (x1, y1), (x0, y1)] {
            assert!(nearest(Pt::from_mm(x, y)) < 1e-5, "a tip at the corner ({x}, {y}): {} mm off", nearest(Pt::from_mm(x, y)));
        }
        let ring = [(x0, y0, x1, y0), (x1, y0, x1, y1), (x1, y1, x0, y1), (x0, y1, x0, y0)];
        for (ax, ay, bx, by) in ring {
            let steps = ((bx - ax).hypot(by - ay) / 0.1).ceil() as usize;
            for i in 0..=steps {
                let f = i as f64 / steps as f64;
                let q = Pt::from_mm(ax + (bx - ax) * f, ay + (by - ay) * f);
                assert!(nearest(q) <= r + 1e-5, "({:.2}, {:.2}) is {:.3} mm from the nearest tip", q.x_mm(), q.y_mm(), nearest(q));
            }
        }
    }

    /// §4.3 look-ahead, §4.4 case 4 and §4.7: a face under 0.25 mm² gets no roof of its own and
    /// no lines; its one point, half way round its outline, is forced into the roof as a contact
    /// disc on T and an interface disc on T − 1 (r_tip, 45 corners), and becomes a tip on T − 2.
    #[test]
    fn a_small_face_is_held_by_discs_forced_into_the_roof() {
        let s = bed_only();
        let face = rect(5.0, 5.0, 5.3, 5.5);
        let (part, over) = scene(face.clone());
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let (t, _, _) = tips(ctl(), &s, &v, &over, None).unwrap();
        assert_eq!((t.contact[23].len(), t.interface[22].len()), (1, 1), "one contact disc on T, one interface disc on T − 1");
        let disc = &t.contact[23][0];
        assert_eq!(disc.contour.pts.len(), 45, "a 45-corner disc");
        assert!((area(std::slice::from_ref(disc)) - std::f64::consts::PI * 0.16).abs() < 0.01, "of radius r_tip");
        let layers: Vec<usize> = (0..30).filter(|&l| !t.tips[l].is_empty()).collect();
        assert_eq!(layers, vec![21], "the tip on T − 2");
        let e = &t.tips[21][0];
        assert_eq!(t.tips[21].len(), 1, "one point");
        assert!(e.under_roof && e.hold == 0 && e.roof_discs == 0 && !e.round, "under its discs, nothing held, not round");
        assert!(inside_or_on(std::slice::from_ref(&face), e.point), "on the face's outline");
    }

    /// A narrow ledge's expanded tail demand must not seed a contact beside the part. A wider
    /// ledge still gets contacts that reach its face.
    #[test]
    fn expanded_tail_demand_does_not_put_contacts_beside_a_narrow_ledge() {
        let s = Settings::new(Keys { line_width: 0.4, wall_width: 0.4, ..bed_only().keys });
        for growth in [0.025, 0.25] {
            let lower = rect(0.0, 0.0, 2.0, 10.0);
            let upper = rect(0.0, 0.0, 2.0 + growth, 10.0);
            let part: Vec<Vec<ExPolygon>> = (0..30).map(|l| vec![if l < 25 { lower.clone() } else { upper.clone() }]).collect();
            // A recurring tail contributes its new strip, grown by 0.2 mm.
            let demand = offset(&[rect(2.0125, 2.0, 2.0 + growth, 8.0)], 0.2 * SCALE, Join::Miter(3.0));
            let mut over = vec![Vec::new(); 30];
            over[25] = demand;
            let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
            let (t, _, _) = tips(ctl(), &s, &v, &over, None).unwrap();
            if growth < 0.1 {
                assert!(t.contact.iter().all(Vec::is_empty) && t.tips.iter().all(Vec::is_empty), "the narrow ledge has no room for a tip that reaches it; contacts={}, tips={}", t.contact.iter().map(Vec::len).sum::<usize>(), t.tips.iter().map(Vec::len).sum::<usize>());
            } else {
                assert!(!t.contact[23].is_empty(), "the wider ledge still has contacts");
                assert!(t.contact[23].iter().all(|e| crate::support_rules::reaches(e, std::slice::from_ref(&upper))), "every contact reaches the wider ledge");
            }
        }
    }

    /// A box kept out of, as its footprint would be: a 20 mm square on every layer, the part its
    /// floor on layers #0 and #1, a wall `wall` inside it up to its lid and the lid from layer
    /// `lid` for 5 layers, the lid's underside the face on `lid`.
    fn kept_box(lid: usize, wall: Option<ExPolygon>) -> (Vec<Vec<ExPolygon>>, Vec<Vec<ExPolygon>>, Vec<Vec<ExPolygon>>) {
        let (n, square) = (lid + 5, rect(-10.0, -10.0, 10.0, 10.0));
        let part = (0..n).map(|l| if l <= 1 || l >= lid { vec![square.clone()] } else { wall.iter().cloned().collect() }).collect();
        let over = (0..n).map(|l| if l == lid { vec![square.clone()] } else { Vec::new() }).collect();
        (part, over, vec![vec![square]; n])
    }

    /// Fab's keep-out: under a face in it, only tips whose branch gets out at the fast move stay,
    /// with no roof, each out of it by the layer it gets out on: each its own contact disc on
    /// T = #23 and the tip under it on T − 1 where it gets out from there too, else bare on T.
    /// From T, 23 layers of 0.2 mm take a branch at most 4.6 mm out, the side gap and its own
    /// radius among them.
    #[test]
    fn tips_in_the_keep_out_stay_only_where_their_branch_gets_out() {
        let s = bed_only();
        let (part, over, keep) = kept_box(25, None);
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap().keeping_out(&keep);
        let (t, _, _) = tips(ctl(), &s, &v, &over, None).unwrap();
        let tests = Tests::new(ctl(), &s, &v);
        assert!(t.interface.iter().all(Vec::is_empty), "no roof in the keep-out");
        let all: Vec<&Element> = t.tips.iter().flatten().collect();
        assert!(all.len() > 8, "tips round the edge: {}", all.len());
        assert!(!t.tips[22].is_empty(), "tips under their discs");
        assert_eq!(t.contact.iter().map(Vec::len).sum::<usize>(), t.contact[23].len(), "contact on T alone");
        assert_eq!(t.contact[23].len(), t.tips[22].len(), "one disc per tip under one");
        let edge = |p: Pt| 10.0 - p.x_mm().abs().max(p.y_mm().abs());
        for e in &all {
            let (k, d) = (e.layer - e.exit, edge(e.point));
            assert!(e.leaving() && e.to_bed && e.hold == 0 && e.roof_discs == 0, "a tip leaving the keep-out, holding nothing: {e:?}");
            match e.layer {
                22 => assert!(e.under_roof && !e.round && t.contact[23].contains(&roof_disc(e.point, s.r_tip)), "under its own disc on T: {e:?}"),
                23 => assert!(!e.under_roof && tests.leaving_class(e.point, 22).unwrap().is_none(), "bare on T where it does not get out from T − 1: {e:?}"),
                _ => panic!("a tip on T or under its disc: {e:?}"),
            }
            assert!(d < 4.6 - s.keys.xy_distance && k as f64 * s.fast >= d + s.keys.xy_distance, "{d:.2} mm in, out by {k} layers");
        }
        // In a box twice as tall, a tip whose straight way out a wall inside the box blocks has
        // none: from T = #43 a branch gets 8.6 mm out.
        let (part, _, keep) = kept_box(45, Some(rect(6.0, -10.0, 6.6, 10.0)));
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap().keeping_out(&keep);
        let tests = Tests::new(ctl(), &s, &v);
        assert!(tests.exit(Pt::from_mm(9.0, 0.0), 43).unwrap().is_some_and(|e| e < 43), "between the wall and the edge, it gets out");
        assert_eq!(tests.exit(Pt::from_mm(5.0, 0.0), 43).unwrap(), None, "behind the wall, it does not");
        assert_eq!(tests.exit(Pt::from_mm(0.0, 0.0), 43).unwrap(), None, "nor from the middle, 10 mm in");
    }

    /// §4.1: from the bed only, a face over a model top that no straight-enough path gets down
    /// from is forbidden whole: no tip and no roof.
    #[test]
    fn a_face_over_the_part_gets_nothing_from_the_bed_only() {
        let s = bed_only();
        // A table top up to layer #20, open from #21 to #24, the face over it on #25.
        let part: Vec<Vec<ExPolygon>> = (0..30).map(|l| if l <= 20 || l >= 25 { vec![rect(-10.0, -10.0, 10.0, 10.0)] } else { Vec::new() }).collect();
        let over: Vec<Vec<ExPolygon>> = (0..30).map(|l| if l == 25 { vec![rect(-5.0, -5.0, 5.0, 5.0)] } else { Vec::new() }).collect();
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let (t, _, _) = tips(ctl(), &s, &v, &over, None).unwrap();
        assert!(t.tips.iter().all(Vec::is_empty) && t.contact.iter().all(Vec::is_empty) && t.interface.iter().all(Vec::is_empty), "nothing");
    }

    /// The tips' layers.
    fn tip_layers(t: &Tips) -> Vec<usize> {
        (0..t.tips.len()).filter(|&l| !t.tips[l].is_empty()).collect()
    }

    /// Whether every roof polygon on every layer is a 45-corner disc.
    fn discs_only(set: &[Vec<ExPolygon>]) -> bool {
        set.iter().flatten().all(|e| e.contour.pts.len() == 45 && e.holes.is_empty())
    }

    /// §4.2 and §4.4 case 3: a 0.5 mm ledge along a pillar lies wholly in the clearance
    /// (F(T) reaches 0.21 + 0.4 + 0.005 = 0.615 mm off the wall), so it has no roof. Its leftover
    /// first leaves F on step 3 (t = 0.1575 mm), as a band from 0.615 to 0.6575 mm off the wall;
    /// later steps add nothing. The band is too thin for lines, so its outline is sampled at the
    /// connect length with m = 2 (§4.4 small-overhang rule): its two ends. With force on they lay
    /// contact discs on T and interface discs on T − 1 and become tips on T − 2 (§4.7).
    #[test]
    fn a_face_hidden_by_the_clearance_is_held_from_a_band_outside_it() {
        let s = bed_only();
        let part: Vec<Vec<ExPolygon>> = (0..30).map(|l| vec![if l < 25 { rect(0.0, 0.0, 2.0, 2.0) } else { rect(0.0, 0.0, 2.5, 2.0) }]).collect();
        let over: Vec<Vec<ExPolygon>> = (0..30).map(|l| if l == 25 { vec![rect(2.0, 0.0, 2.5, 2.0)] } else { Vec::new() }).collect();
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let (t, _, _) = tips(ctl(), &s, &v, &over, None).unwrap();
        assert_eq!(tip_layers(&t), vec![21], "the tips on T − 2");
        let tips = &t.tips[21];
        assert_eq!(tips.len(), 2, "the band's two ends");
        for e in tips {
            let x = e.point.x as f64 / SCALE;
            assert!((2.615 - 1e-6..=2.6575 + 1e-6).contains(&x), "in the band, {x} mm");
            assert!(e.to_bed && e.hole_free && e.under_roof && e.hold == 0 && e.roof_discs == 0 && !e.round, "under its discs, nothing held");
        }
        assert_eq!((t.contact[23].len(), t.interface[22].len()), (2, 2), "a contact and an interface disc each");
        assert!(discs_only(&t.contact) && discs_only(&t.interface), "no roof but the discs");
    }

    /// §4.3 and §4.4 case 2, R14's "roof stopped after 1 of 2": a 5 mm long face past a model
    /// top's edge on layer #19. The bed avoidance there is the top grown by 0.61 mm, shrinking by
    /// a fast move a layer: 0.21 mm out on T − 1 and 0.41 on T − 2. So c_1 is 0.23 mm wide
    /// (1.15 mm²) and c_2 0.03 (0.15 mm²): the walk stops at k = 1 with one roof layer, c_0 on T.
    /// On T the avoidance's corners are mitred in full (§1.5), so its long side stays straight:
    /// c_0 starts at 10.015 mm, less at most the simplification's 12 µm.
    /// Its roof lines' points still valid on T − 1 (more than 0.21 mm out) go in there with K =
    /// H = 1; none is valid on T − 2, so all are tips on T − 1 with hold 1 and roof discs 1.
    #[test]
    fn a_roof_that_stops_after_one_layer_keeps_the_points_still_valid_under_it() {
        let s = bed_only();
        let part: Vec<Vec<ExPolygon>> = (0..30).map(|l| if l <= 19 { vec![rect(-10.0, -10.0, 10.0, 10.0)] } else { Vec::new() }).collect();
        let face = rect(9.5, 0.0, 10.445, 5.0);
        let over: Vec<Vec<ExPolygon>> = (0..30).map(|l| if l == 25 { vec![face.clone()] } else { Vec::new() }).collect();
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let (t, _, _) = tips(ctl(), &s, &v, &over, None).unwrap();
        let c0 = area(&t.contact[23]);
        assert!(c0 > 0.43 * 5.0 - 1e-3 && c0 < (0.43 + 0.012) * 5.0, "c_0, from 10.015 mm less the simplification, on T: {c0}");
        assert!(t.interface.iter().all(Vec::is_empty), "no second roof layer, and no discs under T − 1");
        assert_eq!(tip_layers(&t), vec![22], "the tips on T − 1");
        for e in &t.tips[22] {
            let x = e.point.x as f64 / SCALE;
            assert!(x > 10.21 && x < 10.41, "valid on T − 1 and not on T − 2: {x} mm");
            assert!(e.under_roof && e.hold == 1 && e.roof_discs == 1 && !e.round, "under c_0, holding one layer as a disc");
        }
    }

    /// §4.3 "A roof reaching the bed": on O = 3 the tip layer is #1, so the walk reaches the bed
    /// after one roof layer. c_0 is the roof on #1, c_1 a roof on the bed of depth 1, and no tips.
    #[test]
    fn a_roof_that_reaches_the_bed_has_no_tips() {
        let s = bed_only();
        let face = rect(4.0, 4.0, 8.0, 8.0);
        let part: Vec<Vec<ExPolygon>> = (0..10).map(|l| vec![if l < 3 { rect(0.0, 0.0, 2.0, 2.0) } else { rect(0.0, 0.0, 8.0, 8.0) }]).collect();
        let over: Vec<Vec<ExPolygon>> = (0..10).map(|l| if l == 3 { vec![face.clone()] } else { Vec::new() }).collect();
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let (t, _, _) = tips(ctl(), &s, &v, &over, None).unwrap();
        assert!(tip_layers(&t).is_empty(), "no tips");
        assert!((area(&t.contact[1]) - 16.0).abs() < 1e-6, "c_0 on #1");
        assert!(t.interface.iter().all(Vec::is_empty), "no interface layer");
        assert_eq!(t.bed.len(), 1, "one roof on the bed");
        assert_eq!(t.bed[0].1, 1, "of depth 1");
        assert!((area(&t.bed[0].0) - 16.0).abs() < 1e-6, "c_1, the face clear of the pillar");
    }

    /// §4.6 de-dup: per insert layer, the first tip in a 0.04 mm cell wins, lower overhang
    /// layers first; cells either side of 0 are twice as wide.
    #[test]
    fn tips_in_one_cell_are_de_duplicated_lowest_overhang_first() {
        let s = bed_only();
        assert_eq!(s.dedup_cell, 40_000, "(r_tip + 1 nm) ÷ 10");
        let at = |l, x, y| Found { tips: vec![Element::tip(&s, l, Pt::new(x, y))], tip_pieces: vec![0], pieces: 1, ..Found::default() };
        let mut lower = at(7, 39_999, 5);
        lower.tips[0].hold = 1;
        let found = [lower, at(7, -39_999, 5), at(7, 40_000, 5), at(8, 39_999, 5)].map(Arc::new);
        let (t, of) = gather(&found, s.dedup_cell, 10);
        assert_eq!(t.tips[7].len(), 2, "the second tip, 80 µm away across 0, shares the first's cell; the third has its own");
        assert_eq!(t.tips[7][0].hold, 1, "the lower overhang layer's tip wins");
        assert_eq!(t.tips[8].len(), 1, "cells are per layer");
        assert_eq!((&of.tips[7], &of.tips[8], of.count), (&vec![0, 2], &vec![3], 4), "each layer's pieces numbered on from the last");
    }

    /// A piece's roofs go only when every tip it kept goes: one tip of two leaves them, both take
    /// them, and another piece's stay.
    #[test]
    fn a_roof_goes_with_the_last_of_its_tips() {
        let s = bed_only();
        let tip = |x| Element::tip(&s, 5, Pt::from_mm(x, 0.0));
        let roof = |x| rect(x, 1.0, x + 1.0, 2.0);
        let two = Found { tips: vec![tip(0.0), tip(1.0)], roofs: vec![(6, true, vec![roof(0.0)]), (7, false, vec![roof(0.0)])], tip_pieces: vec![0, 0], roof_pieces: vec![0, 0], pieces: 1, ..Found::default() };
        let one = Found { tips: vec![tip(5.0)], roofs: vec![(6, true, vec![roof(5.0)])], tip_pieces: vec![0], roof_pieces: vec![0], pieces: 1, ..Found::default() };
        let (t, of) = gather(&[two, one].map(Arc::new), s.dedup_cell, 10);
        let gone = |marked: &[usize]| -> Vec<Vec<bool>> { (0..10).map(|l| (0..t.tips[l].len()).map(|i| l == 5 && marked.contains(&i)).collect()).collect() };
        let at = |t: &Tips| t.tips[5].iter().map(|e| e.point).collect::<Vec<_>>();
        let left = without(&t, &of, &gone(&[0]));
        assert_eq!((at(&left), left.contact[6].len(), left.interface[7].len()), (vec![Pt::from_mm(1.0, 0.0), Pt::from_mm(5.0, 0.0)], 2, 1), "one of two tips goes, its piece's roofs stay");
        let left = without(&t, &of, &gone(&[0, 1]));
        assert_eq!((at(&left), left.contact[6].clone(), left.interface[7].len()), (vec![Pt::from_mm(5.0, 0.0)], vec![roof(5.0)], 0), "both go, and their piece's roofs with them; the other piece's stay");
    }
}

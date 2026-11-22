//! The descent (R14 §5.1, §5.2): one pass from the top layer down to layer 1, serial in z and
//! parallel within a layer. Each element on layer L tries a list of moves in order and keeps the
//! first that leaves it an area on L − 1; then the elements made there merge ([`super::merge`]).
//!
//! Every layer is a merge layer (§5.3: the cadence comes out as every layer), so the
//! repeat-the-last-move shortcut (§5.1 order 1) never applies and is not in the list.
//!
//! Fab's keep-out, which is not the reference's: an element leaving it ([`Element::leaving`])
//! moves in the part's own volumes, to the bed only, and its part to the bed keeps to where it
//! still gets out by its exit ([`Volumes::leave_by`]).
//!
//! What a run makes is kept for the next run on the same settings ([`Memo`]), its part edited
//! or not. Each step and each merge is kept with the volume layers it read ([`reading`]) and
//! is taken again only where the next run's volumes have every one of them as it read it, the
//! very layer ([`Volumes::holds`]). Each element has an id: a tip the id it had in the run
//! before when it is that run's tip in every way, and one no element has had when not
//! ([`Memo::tip_ids`]); an element a step or merge made the id it had when that step or merge
//! is taken or makes it again the same in every way, and a new one when not. So two elements
//! with one id are the same, and a step, a merge or a group of merges taken with its ids gives
//! what it would give: the next run's output is a fresh run's.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::Join;

use super::clip::{difference_ex, intersection_ex, less, union, union_ex, FillRule};
use super::element::{simplified, some, Element, Move};
use super::merge::{merge, Ctx, Groups, Item, Made, Merges, Stepped};
use super::offset::{offset, MITRE_FLOOR};
use super::settings::Settings;
use super::tips::Tips;
use super::volumes::{reading, Kind, Reads, To, Volumes};
use crate::par::{Cancelled, Ctl};

/// What each base speed adds (mm): 5 nm.
const SPEED_SLACK: f64 = 5e-6;
/// The grown areas' round joins: chords within 0.01 mm of the arc.
const ARC: f64 = 0.01;
/// What the grown areas are simplified at (mm).
const SIMPLIFY: f64 = 0.015;
/// How far the strayed entry grows, in fast speeds (§5.1 entry 5).
const STRAY_GROWTH: f64 = 1.5;
/// The strayed entry draws its upper element's rings as lines reaching this far (mm) each side,
/// butt-ended and mitred to 1.2, which the engine takes as 2 (§1.5).
const STRAY_LINE: f64 = 0.005;
const STRAY_MITRE: f64 = 1.2;

/// The speeds (mm) an element moves at this layer (§5.1).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Speeds {
    fast: f64,
    slow: f64,
}

/// The area an entry grows (§5.1 "Growing the area").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Area {
    /// The upper element's, unmoved.
    Upper,
    Slow,
    Fast,
    /// The strayed entry's (entry 5).
    Stray,
}

/// The grown area an entry takes (§5.1), as Fab bounds it ([`Upper::grown_for`]): its area,
/// and, where that reaches past the move's reach, only what lies within the reach of a move that
/// keeps its ramp counter or grows it (`within`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Grown {
    area: Area,
    within: Option<bool>,
}

/// One entry of the list: the move and the area it grows.
#[derive(Clone, Copy, Debug)]
struct Entry {
    m: Move,
    area: Area,
}

/// The speeds of `e` moving down from layer `l` (§5.1 "Speeds"): the plain moves plus 5 nm, plus
/// its radius's growth to both when that stays on the rung or is under r_br, else to the slow
/// one only; and in the foot zone a foot's growth for a branch already wider than the foot's
/// target, the same way. The slow one's extras never take it past the plain fast move.
fn speeds(s: &Settings, e: &Element, l: usize) -> Speeds {
    let full = !e.min_gap;
    let now = e.collision_radius(s);
    let rung = s.rung_for(now, full);
    let grown = s.radius(e.ramp + 1, e.foot);
    // Every speed and extra is whole nm.
    let whole = |mm: f64| (mm * SCALE).trunc() / SCALE;
    let delta = whole(grown - now);
    let bonus = whole(s.foot_growth);
    let cap = s.fast - s.slow;
    let (mut both, mut slow_only) = (0.0, 0.0);
    if s.rung_for(grown, full) == rung || grown < s.r_br {
        both += delta;
    } else {
        slow_only = (slow_only + delta).min(cap);
    }
    // §5.1 quirk: the bonus goes to branches wider than the foot's target, not narrower.
    if l < s.foot_start && s.foot_target(l - 1) < grown {
        if s.rung_for(s.radius(e.ramp + 1, e.foot + 1.0), full) == rung {
            both += bonus;
        } else {
            slow_only = (slow_only + bonus).min(cap);
        }
    }
    Speeds { fast: s.fast + SPEED_SLACK + both, slow: s.slow + SPEED_SLACK + both + slow_only }
}

/// The entries `e` tries in order going down from layer `l` (§5.1 "The order"); `misses` says
/// whether its area misses the placeable area on `l`. The ramp tests go by the new depth. A
/// moving entry grows the slow area at the slow speed, else the fast one; an entry the same as
/// one before it is left out.
fn entries(s: &Settings, e: &Element, sp: Speeds, misses: bool) -> Vec<Entry> {
    let at = |kind: Kind, speed: f64, grow: bool, moves: bool| {
        let area = if !moves { Area::Upper } else if speed == sp.slow { Area::Slow } else { Area::Fast };
        Entry { m: Move { kind, speed, grow, may_stray: false, min_gap: false, moves }, area }
    };
    let depth = e.depth + 1;
    let mut list = Vec::new();
    if !e.hole_free {
        list.push(at(Kind::Slow, sp.slow, true, false));
        if depth < s.tip_layers.div_ceil(2) {
            list.push(at(Kind::Fast, sp.slow, true, false));
        }
        list.push(at(Kind::FastHoleFree, sp.fast, true, false));
        list.push(at(Kind::FastHoleFree, sp.fast, false, true));
        list.push(at(Kind::Fast, sp.fast, false, true));
    } else {
        list.push(at(Kind::Slow, sp.slow, true, true));
        list.push(at(Kind::Slow, sp.slow, false, true));
        if depth < s.tip_layers {
            list.push(at(Kind::FastHoleFree, sp.slow, true, true));
        }
        list.push(at(Kind::FastHoleFree, sp.fast, true, true));
        list.push(at(Kind::FastHoleFree, sp.fast, false, true));
    }
    if e.min_gap {
        list = list.into_iter().flat_map(|x| [x, Entry { m: Move { min_gap: true, ..x.m }, ..x }]).collect();
    }
    // Entry 6, first and never doubled: stay and grow while the hold lasts.
    if e.hole_free && depth < e.hold {
        list.insert(0, at(Kind::Slow, 0.0, true, false));
    }
    // Entry 5, last and never doubled: the strayed move.
    if e.to_bed || (e.on_top && misses) {
        let m = Move { kind: Kind::Fast, speed: sp.fast, grow: false, may_stray: true, min_gap: e.min_gap, moves: true };
        list.push(Entry { m, area: Area::Stray });
    }
    let mut out: Vec<Entry> = Vec::with_capacity(list.len());
    for x in list {
        if !out.iter().any(|y| y.m == x.m && y.area == x.area) {
            out.push(x);
        }
    }
    out
}

/// What one element's entries share.
struct Upper<'a> {
    ctl: Ctl<'a>,
    s: &'a Settings,
    v: &'a Volumes,
    e: &'a Element,
    l: usize,
    sp: Speeds,
    walls: &'a [ExPolygon],
    slow: OnceLock<Vec<ExPolygon>>,
    fast: OnceLock<Vec<ExPolygon>>,
    stray: OnceLock<Vec<ExPolygon>>,
    /// The upper element's area grown by the reach of a move that keeps its ramp counter, and of
    /// one that grows it ([`Upper::reach`]).
    within: [OnceLock<Vec<ExPolygon>>; 2],
    /// Each grown area within those reaches, by area and grow ([`Upper::grown`]).
    bounded: [[OnceLock<Vec<ExPolygon>>; 2]; 4],
    /// What [`parts`] took from the grown areas so far. Every entry's branch is on the same
    /// layer and leaves the keep-out alike, so entries with the same grown area, kind and rung
    /// take the same parts.
    taken: RefCell<Vec<Taken>>,
    /// Each grown area less the collision on the layer below, by area and rung.
    off: RefCell<Vec<(Grown, usize, Rc<Vec<ExPolygon>>)>>,
}

/// A grown area less the avoidances of `kind` at `rung` on the layer below.
struct Taken {
    area: Grown,
    kind: Kind,
    rung: usize,
    /// Less the avoidance to the bed, and, leaving the keep-out, less where it gets out too late.
    to_bed: Rc<Vec<ExPolygon>>,
    /// Less the avoidance to the model, when it may rest on the model.
    on_top: Option<Rc<Vec<ExPolygon>>>,
}

fn round(set: &[ExPolygon], by: f64) -> Vec<ExPolygon> {
    if set.is_empty() { Vec::new() } else { offset(set, by * SCALE, Join::Round(ARC * SCALE)) }
}

/// The open line `line` drawn `d` units each side: a butt-ended rectangle per segment, and at
/// each inner corner its outer side's mitre, or past `limit` (at least [`MITRE_FLOOR`]) times `d`
/// a square edge `d` out, as the engine joins them (§1.5).
fn band(line: &[Pt], d: f64, limit: f64) -> Vec<Polygon> {
    let limit = limit.max(MITRE_FLOOR);
    let unit = |a: Pt, b: Pt| {
        let (x, y) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
        let l = x.hypot(y);
        if l == 0.0 { None } else { Some((x / l, y / l)) }
    };
    let at = |p: Pt, v: (f64, f64), k: f64| Pt::new((p.x as f64 + v.0 * k).round() as i64, (p.y as f64 + v.1 * k).round() as i64);
    let ccw = |pts: Vec<Pt>| {
        let mut r = Polygon::new(pts);
        if !r.is_ccw() {
            r.reverse();
        }
        r
    };
    let mut out = Vec::new();
    let mut dirs = Vec::new();
    for w in line.windows(2) {
        let Some(u) = unit(w[0], w[1]) else { continue };
        let n = (u.1, -u.0);
        out.push(ccw(vec![at(w[0], n, d), at(w[1], n, d), at(w[1], n, -d), at(w[0], n, -d)]));
        dirs.push((w[1], u));
    }
    for k in 1..dirs.len() {
        let (p, u1) = (dirs[k - 1].0, dirs[k - 1].1);
        let u2 = dirs[k].1;
        let turn = u1.0 * u2.1 - u1.1 * u2.0;
        if turn == 0.0 {
            continue;
        }
        // The outer side's normals: right of a left turn, left of a right turn.
        let side = if turn > 0.0 { 1.0 } else { -1.0 };
        let (n1, n2) = ((u1.1 * side, -u1.0 * side), (u2.1 * side, -u2.0 * side));
        let r = 1.0 + n1.0 * n2.0 + n1.1 * n2.1;
        let (a, b) = (at(p, n1, d), at(p, n2, d));
        if r > 2.0 / (limit * limit) {
            out.push(ccw(vec![p, a, at(p, (n1.0 + n2.0, n1.1 + n2.1), d / r), b]));
        } else {
            // Squared d out along the bisector, its ends on the two offset lines.
            let (bx, by) = (n1.0 + n2.0, n1.1 + n2.1);
            let bl = bx.hypot(by);
            let w = (bx / bl, by / bl);
            let end = |n: (f64, f64), t: (f64, f64)| {
                let along = (d - d * (n.0 * w.0 + n.1 * w.1)) / (t.0 * w.0 + t.1 * w.1);
                Pt::new((p.x as f64 + n.0 * d + t.0 * along).round() as i64, (p.y as f64 + n.1 * d + t.1 * along).round() as i64)
            };
            out.push(ccw(vec![p, a, end(n1, u1), end(n2, (-u2.0, -u2.1)), b]));
        }
    }
    out
}

impl Upper<'_> {
    /// The slow area: the upper element's grown by the slow speed in two equal round steps,
    /// each less the wall restriction, then simplified and less it again.
    fn slow(&self) -> &[ExPolygon] {
        self.slow.get_or_init(|| {
            let half = (self.sp.slow * SCALE / 2.0).trunc() / SCALE;
            let mut a = self.e.area.clone();
            for _ in 0..2 {
                a = difference_ex(&round(&a, half), self.walls);
            }
            difference_ex(&simplified(&a, SIMPLIFY), self.walls)
        })
    }

    /// The fast area: the slow area grown on by the difference in one round step, with no wall
    /// check, then simplified.
    fn fast(&self) -> &[ExPolygon] {
        self.fast.get_or_init(|| simplified(&round(self.slow(), self.sp.fast - self.sp.slow), SIMPLIFY))
    }

    /// The strayed entry's area: the upper element's with its rings drawn as thin lines, grown by
    /// 1.5 fast speeds in one plain round offset.
    fn stray(&self) -> &[ExPolygon] {
        self.stray.get_or_init(|| {
            let mut bands = Vec::new();
            for r in self.e.area.iter().flat_map(|e| e.rings()) {
                let mut line = r.pts.clone();
                line.push(r.pts[0]);
                bands.extend(band(&line, STRAY_LINE * SCALE, STRAY_MITRE));
            }
            let mut a = self.e.area.clone();
            a.extend(union(&bands, FillRule::NonZero));
            round(&union_ex(&a), STRAY_GROWTH * self.sp.fast)
        })
    }

    fn area(&self, a: Area) -> &[ExPolygon] {
        match a {
            Area::Upper => &self.e.area,
            Area::Slow => self.slow(),
            Area::Fast => self.fast(),
            Area::Stray => self.stray(),
        }
    }

    /// How far an area reaches from the upper element's (mm).
    fn growth(&self, a: Area) -> f64 {
        match a {
            Area::Upper => 0.0,
            Area::Slow => self.sp.slow,
            Area::Fast => self.sp.fast,
            Area::Stray => STRAY_GROWTH * self.sp.fast,
        }
    }

    /// How far a move that keeps its ramp counter, or `grow`s it, reaches (mm): as far as the
    /// support angle takes the branch's side in a layer, plus what its drawn radius grows on it.
    fn reach(&self, grow: bool) -> f64 {
        let (s, e) = (self.s, self.e);
        let drawn = s.drawn_radius(e.depth + 1, e.ramp + usize::from(grow), e.foot);
        s.fast + SPEED_SLACK + (drawn - e.drawn_radius(s)).max(0.0)
    }

    /// The upper element's area grown by the reach of a move that keeps its ramp counter, or
    /// `grow`s it, simplified as the fast area is: the bound's arcs would otherwise pass into the
    /// element's area, and every later offset of it, a layer at a time.
    fn within(&self, grow: bool) -> &[ExPolygon] {
        self.within[usize::from(grow)].get_or_init(|| simplified(&round(&self.e.area, self.reach(grow)), SIMPLIFY))
    }

    /// Fab's, not the reference's: the grown area entry `en` takes reaches no further than its
    /// move's reach ([`Upper::reach`]). §5.1's speeds add the growth a grown counter would give,
    /// and the foot's, to every move, so a branch that does not grow, as one beside the part,
    /// leaned past the support angle by them.
    fn grown_for(&self, en: &Entry) -> Grown {
        let grow = en.m.grow;
        Grown { area: en.area, within: (self.growth(en.area) > self.reach(grow)).then_some(grow) }
    }

    fn grown(&self, g: Grown) -> &[ExPolygon] {
        match g.within {
            None => self.area(g.area),
            Some(grow) => self.bounded[g.area as usize][usize::from(grow)].get_or_init(|| intersection_ex(self.area(g.area), self.within(grow))),
        }
    }

    /// Grown area `a` less the collision at `rung` on the layer below, for the volumes `n` sees.
    fn off_collision(&self, a: Grown, rung: usize, n: &Element) -> Result<Rc<Vec<ExPolygon>>, Cancelled> {
        if let Some(o) = self.off.borrow().iter().find(|o| (o.0, o.1) == (a, rung)) {
            return Ok(o.2.clone());
        }
        let o = Rc::new(difference_ex(self.grown(a), self.v.of(n).collision(self.ctl, self.l - 1, rung)?));
        self.off.borrow_mut().push((a, rung, o.clone()));
        Ok(o)
    }
}

/// Grown area `area`'s parts on the layer below at `rung` for an entry of `kind` (§5.1 "Checking
/// the area"), the taint fixes applied to `n`'s class; and whether the part of its class is
/// there.
fn parts(u: &Upper, n: &mut Element, kind: Kind, rung: usize, area: Grown) -> Result<(bool, Rc<Vec<ExPolygon>>, Rc<Vec<ExPolygon>>), Cancelled> {
    let (s, v, below) = (u.s, u.v.of(n), u.l - 1);
    let known = u.taken.borrow().iter().find(|t| (t.area, t.kind, t.rung) == (area, kind, rung)).map(|t| (t.to_bed.clone(), t.on_top.clone()));
    let (to_bed, on_top) = match known {
        Some(taken) => taken,
        None => {
            let grown = u.grown(area);
            let mut to_bed = less(grown, v.avoidance(u.ctl, kind, To::Bed, rung).blocked(below)?);
            if n.leaving() && some(&to_bed) {
                to_bed = difference_ex(&to_bed, &u.v.leave_by(u.ctl, rung, n.exit, below)?);
            }
            let on_top = if !s.keys.bed_only && !n.leaving() { Some(Rc::new(less(grown, v.avoidance(u.ctl, kind, To::Model, rung).blocked(below)?))) } else { None };
            let to_bed = Rc::new(to_bed);
            u.taken.borrow_mut().push(Taken { area, kind, rung, to_bed: to_bed.clone(), on_top: on_top.clone() });
            (to_bed, on_top)
        }
    };
    let mut to_model = Rc::new(Vec::new());
    if let Some(on_top) = on_top {
        if !n.to_bed && some(&to_bed) {
            n.to_bed = true;
        }
        if n.on_top || some(&on_top) {
            n.on_top = true;
            to_model = on_top;
        } else {
            to_model = u.off_collision(area, rung, n)?;
        }
    }
    let ok = if n.to_bed { some(&to_bed) } else { some(&to_model) };
    Ok((ok, to_bed, to_model))
}

/// The upper element moved by entry `en`, if it leaves an area of its class (§5.1 "Radius growth
/// inside a move", §5.2).
fn try_entry(u: &Upper, en: Entry) -> Result<Option<Stepped>, Cancelled> {
    let s = u.s;
    let grown = u.grown_for(&en);
    if u.grown(grown).is_empty() {
        return Ok(None);
    }
    let full = !en.m.min_gap;
    let kind = en.m.kind;
    let mut n = u.e.clone();
    n.layer = u.l - 1;
    n.depth += 1;
    n.carried = u.e.width(s);
    n.min_gap = en.m.min_gap;
    // 1. A growing entry adds 1 first and is checked only at the new rung.
    if en.m.grow {
        n.ramp += 1;
    }
    let first = s.rung_for(n.collision_radius(s), full);
    let (ok, mut to_bed, mut to_model) = parts(u, &mut n, kind, first, grown)?;
    if !ok {
        return Ok(None);
    }
    // 2. The catch-up, rung by rung up to min(drawn, r_br), then the counter after it.
    let (radius, drawn) = (n.collision_radius(s), n.drawn_radius(s));
    if radius < s.r_br && radius < drawn {
        let target = drawn.min(s.r_br);
        let ceiling = |k: usize| s.rung(k) - if full { s.xy_delta } else { 0.0 };
        let mut rung = first;
        while ceiling(rung) < target && rung + 1 < s.zero_full() {
            let mut t = n.clone();
            if !parts(u, &mut t, kind, rung + 1, grown)?.0 {
                break;
            }
            rung += 1;
        }
        let top = ceiling(rung);
        while n.ramp + 1 < n.depth && s.radius(n.ramp + 1, n.foot) <= top.min(drawn) {
            n.ramp += 1;
        }
    }
    // 3. The foot, for a branch to the bed at or past r_br in the foot zone.
    let drawn = n.drawn_radius(s);
    if n.to_bed && drawn >= s.r_br && n.layer < s.foot_start {
        let step = ((s.foot_target(n.layer) - drawn) / s.foot_step).min(1.0);
        if step > 0.0 {
            let now = s.rung_for(n.collision_radius(s), full);
            let with = s.rung_for(s.radius(n.ramp, n.foot + step), full);
            if with == now || parts(u, &mut n.clone(), kind, with, grown)?.0 {
                n.foot += step;
            }
        }
    }
    // 4. The parts again at the final rung, when it moved.
    let rung = s.rung_for(n.collision_radius(s), full);
    if rung != first {
        let (ok, b, m) = parts(u, &mut n, kind, rung, grown)?;
        if !ok {
            return Ok(None);
        }
        (to_bed, to_model) = (b, m);
    }
    // §5.2.
    n.last = Some(en.m);
    n.strayed = en.m.may_stray;
    if en.m.moves {
        n.hold = 0;
        n.placed = None;
    }
    n.hole_free = kind != Kind::Fast;
    let mut area = u.off_collision(grown, rung, &n)?.to_vec();
    area.extend(to_bed.iter().cloned());
    area.extend(to_model.iter().cloned());
    n.area = union_ex(&area);
    let merges = en.m.moves && !(en.m.min_gap && n.depth < s.tip_layers);
    Ok(Some(Stepped { e: n, to_bed: Rc::unwrap_or_clone(to_bed), to_model: Rc::unwrap_or_clone(to_model), merges }))
}

/// `e`, on layer `l`, one layer down: the first of its entries that succeeds, none when it is
/// stuck.
fn step(ctl: Ctl, s: &Settings, v: &Volumes, e: &Element, l: usize) -> Result<Option<Stepped>, Cancelled> {
    let sp = speeds(s, e, l);
    let own = v.of(e);
    let walls = own.wall_restriction(ctl, l, s.rung_for(e.collision_radius(s), false), !e.min_gap)?;
    let misses = s.keys.bed_only || !some(&intersection_ex(&e.area, own.placeable(ctl, l, s.rung_for(e.collision_radius(s), false))?));
    let u = Upper { ctl, s, v, e, l, sp, walls, slow: OnceLock::new(), fast: OnceLock::new(), stray: OnceLock::new(), within: Default::default(), bounded: Default::default(), taken: RefCell::new(Vec::new()), off: RefCell::new(Vec::new()) };
    for en in entries(s, e, sp, misses) {
        if let Some(st) = try_entry(&u, en)? {
            return Ok(Some(st));
        }
    }
    Ok(None)
}

/// An element's id: what names it in the memo, and nothing else (no order or output goes by it).
/// A new one is a 128-bit hash of what made it and the run it was made in, so one is never
/// given twice; two share one with odds under 10⁻²⁵ a run.
pub type Id = u128;

fn id_of(kind: u8, a: Id, b: Id, run: u64) -> Id {
    let half = |salt: u8| {
        let mut h = std::hash::DefaultHasher::new();
        (kind, a, b, run, salt).hash(&mut h);
        h.finish() as Id
    };
    half(0) << 64 | half(1)
}

/// The new id of what a step in run `run` makes of the element `from`: a step makes one.
fn stepped_id(from: Id, run: u64) -> Id {
    id_of(1, from, 0, run)
}

/// The new id of what `a` absorbing `b` makes in run `run`.
pub fn merged_id(a: Id, b: Id, run: u64) -> Id {
    id_of(2, a, b, run)
}

/// A step the memo keeps: what it made of its element, with that one's id (none when it was
/// stuck), and the volume layers it read.
#[derive(Clone)]
struct Step {
    made: Option<(Arc<Made>, Id)>,
    read: Reads,
}

/// What one layer's step down made.
#[derive(Default)]
struct Layer {
    /// The ids of the elements on the layer, in order: a run with the same, on volumes with the
    /// layers it read ([`Layer::read`]), makes the same.
    from: Vec<Id>,
    /// Each element's step, by its id.
    steps: HashMap<Id, Step>,
    /// The merges tried on the layer below.
    merges: Merges,
    /// What each group merged into on the layer below.
    groups: Groups,
    /// The places on the layer of those stuck.
    stuck: Vec<usize>,
    /// The elements made on the layer below, in order.
    made: Vec<Item>,
    /// Every volume layer its steps and merges read, once each.
    read: Reads,
}

/// What the descent made, per layer, kept for the next run on the same settings
/// ([`super::Cache`]): each run keeps what it made or used, and drops the rest, and with it the
/// volume layers only that read.
#[derive(Default)]
pub struct Memo {
    layers: Vec<Option<Layer>>,
    /// The last run's tips, by their layer and point (one a cell there, §4.6), each with its id.
    tips: HashMap<(usize, Pt), (Element, Id)>,
    /// How many tip ids it has given.
    given: u64,
    /// How many runs it has kept: a run's new ids are of its number ([`stepped_id`]).
    runs: u64,
    /// The steps the last descent made afresh ([`super::Work`]).
    pub(super) made: usize,
}

impl Memo {
    /// Per layer, the ids of `tips`'s tips: the id the run before gave the tip at its layer and
    /// point when that tip is this one in every way, and one no element has had for any other,
    /// so a tip is never taken for another.
    fn tip_ids(&mut self, tips: &Tips) -> Vec<Vec<Id>> {
        let mut was = std::mem::take(&mut self.tips);
        let mut ids = Vec::with_capacity(tips.tips.len());
        for (l, at) in tips.tips.iter().enumerate() {
            let mut on = Vec::with_capacity(at.len());
            for e in at {
                let (tip, id) = match was.remove(&(l, e.point)) {
                    Some((tip, id)) if tip == *e => (tip, id),
                    _ => {
                        self.given += 1;
                        (e.clone(), id_of(0, self.given as Id, 0, 0))
                    }
                };
                on.push(id);
                self.tips.entry((l, e.point)).or_insert((tip, id));
            }
            ids.push(on);
        }
        ids
    }
}

/// The descent of the tips (§5) on `v`: per layer, every element on it, each listing the elements
/// it carries on the layer above. What `memo` kept from runs before is taken where it applies, and
/// `memo` keeps this run's.
pub fn descend(ctl: Ctl, s: &Settings, v: &Volumes, tips: &Tips, memo: &mut Memo) -> Result<Vec<Vec<Element>>, Cancelled> {
    crate::prof::scope_fanned(crate::prof::Stage::TreeDescent, || {
        let n = v.layers();
        memo.runs += 1;
        let run = memo.runs;
        let ids = memo.tip_ids(tips);
        let mut layers: Vec<Vec<Element>> = (0..n).map(|l| tips.tips.get(l).cloned().unwrap_or_default()).collect();
        let mut on: Vec<Vec<Id>> = (0..n).map(|l| ids.get(l).cloned().unwrap_or_default()).collect();
        let mut seen = std::mem::take(&mut memo.layers);
        seen.resize_with(n, || None);
        let mut kept: Vec<Option<Layer>> = (0..n).map(|_| None).collect();
        let mut stepped = 0;
        let mut down = || -> Result<(), Cancelled> {
            for l in (1..n).rev() {
                let last = seen[l].take().unwrap_or_default();
                let whole = last.from == on[l]
                    && match v.holds(ctl, &last.read) {
                        Ok(holds) => holds,
                        Err(e) => {
                            seen[l] = Some(last);
                            return Err(e);
                        }
                    };
                let layer = if whole {
                    last
                } else {
                    match step_down(ctl, v, &layers[l], &on[l], l, &last, run) {
                        Ok((layer, made)) => {
                            stepped += made;
                            layer
                        }
                        Err(e) => {
                            seen[l] = Some(last);
                            return Err(e);
                        }
                    }
                };
                for &i in &layer.stuck {
                    let e = &mut layers[l][i];
                    e.placed = None;
                    e.on_top = false;
                    e.stuck = true;
                }
                for it in &layer.made {
                    layers[l - 1].push(it.element());
                    on[l - 1].push(it.id);
                }
                super::tell(ctl, super::Stage::Descent, l - 1, n - 1, super::Grown::Branches { layers: &layers, settings: s });
                kept[l] = Some(layer);
            }
            Ok(())
        };
        let done = down();
        // A run cut short keeps what it made and what it had not reached yet.
        memo.layers = kept.into_iter().zip(seen).map(|(new, old)| new.or(old)).collect();
        memo.made = stepped;
        done?;
        Ok(layers)
    })
}

/// Layer `l`'s elements `here`, with ids `ids`, one layer down and merged there in run `run`,
/// taking what `last` made or tried where `v` has every volume layer it read; and how many steps
/// it made afresh.
fn step_down(ctl: Ctl, v: &Volumes, here: &[Element], ids: &[Id], l: usize, last: &Layer, run: u64) -> Result<(Layer, usize), Cancelled> {
    let s = v.settings();
    let stepped = crate::par::try_map(ctl, here.len(), |i| -> Result<(Step, bool), Cancelled> {
        let kept = last.steps.get(&ids[i]);
        if let Some(k) = kept {
            if v.holds(ctl, &k.read)? {
                return Ok((k.clone(), false));
            }
        }
        let (st, reads) = reading(|| step(ctl, s, v, &here[i], l));
        let made = st?.map(Made::new);
        let read = v.now(ctl, reads)?;
        // Made again as it was, it is the element it was, for the layers under it.
        let made = made.map(|m| match kept.and_then(|k| k.made.as_ref()) {
            Some((was, id)) if **was == m => (was.clone(), *id),
            _ => (Arc::new(m), stepped_id(ids[i], run)),
        });
        Ok((Step { made, read }, true))
    })?;
    let mut layer = Layer { from: ids.to_vec(), ..Default::default() };
    let mut down = Vec::with_capacity(stepped.len());
    let mut reads = Vec::with_capacity(stepped.len());
    let mut made = 0;
    for (i, st) in stepped.into_iter().enumerate() {
        let (st, afresh) = st?;
        made += usize::from(afresh);
        match &st.made {
            Some((made, id)) => down.push(Item { made: made.clone(), carries: vec![i], id: *id }),
            None => layer.stuck.push(i),
        }
        reads.push(st.read.clone());
        layer.steps.insert(ids[i], st);
    }
    let kept = Mutex::new(Merges::new());
    (layer.made, layer.groups) = merge(&Ctx { ctl, s, v, below: l - 1, seen: &last.merges, kept: &kept, run }, down, &last.groups)?;
    layer.merges = kept.into_inner().unwrap_or_else(|e| e.into_inner());
    layer.read = Reads::all(reads.iter().chain(layer.merges.values().map(|m| &m.read)));
    Ok((layer, made))
}

#[cfg(test)]
mod tests {
    use super::super::settings::tests::r14;
    use super::super::settings::Keys;
    use super::super::tips::Tips;
    use super::*;

    fn ctl() -> Ctl<'static> {
        Ctl::default()
    }

    /// R14's profile from the bed only, at a 0.8 mm side gap.
    fn bed_only() -> Settings {
        Settings::new(Keys { bed_only: true, ..r14(0.8).keys })
    }

    /// The descent of `t` on `v` with nothing kept from a run before.
    fn fresh(s: &Settings, v: &Volumes, t: &Tips) -> Vec<Vec<Element>> {
        descend(ctl(), s, v, t, &mut Memo::default()).unwrap()
    }

    fn tips_at(s: &Settings, layers: usize, at: &[(usize, f64, f64)]) -> Tips {
        let mut t = Tips { tips: vec![Vec::new(); layers], contact: vec![Vec::new(); layers], interface: vec![Vec::new(); layers], bed: Vec::new() };
        for &(l, x, y) in at {
            let mut e = Element::tip(s, l, Pt::from_mm(x, y));
            (e.to_bed, e.on_top, e.hole_free) = (true, true, true);
            t.tips[l].push(e);
        }
        t
    }

    /// §5.1 "Speeds" at R14's numbers: in the ramp the radius grows 0.12 mm a layer and stays on
    /// its rung, so both speeds get it, plus 5 nm: 0.213 266 and 0.320 004 mm. Below the ramp
    /// both get the taper, truncated to 17 497 nm.
    #[test]
    fn the_speeds_carry_the_radius_growth() {
        let s = r14(0.35);
        let tip = Element::tip(&s, 30, Pt::from_mm(0.0, 0.0));
        let sp = speeds(&s, &tip, 30);
        assert!((sp.slow - 0.213_266).abs() < 1e-9 && (sp.fast - 0.320_004).abs() < 1e-9, "in the ramp: {sp:?}");
        let mut past = tip.clone();
        (past.depth, past.ramp) = (20, 20);
        let sp = speeds(&s, &past, 30);
        assert!((sp.slow - (0.093_261 + 0.000_005 + 0.017_497)).abs() < 1e-9, "past the ramp: {sp:?}");
        assert!((sp.fast - (0.199_999 + 0.000_005 + 0.017_497)).abs() < 1e-9, "past the ramp: {sp:?}");
    }

    /// §5.1 foot bonus, R14's inverted rule: moving down from #15 a branch already wider than
    /// the foot's target for #14 (r_br + 2 × 0.168 mm) gets the bonus. At the min gap a foot
    /// count takes it from the 1.5 mm rung to the next, so the bonus goes to the slow speed only,
    /// capped at the plain fast move: both speeds come out 0.217 501 mm, and every moving entry
    /// then grows the slow area.
    #[test]
    fn a_wide_branch_in_the_foot_zone_gets_the_capped_bonus() {
        let s = r14(0.35);
        assert_eq!(s.foot_start, 16, "§1.2 foot zone");
        let mut e = Element::tip(&s, 15, Pt::from_mm(0.0, 0.0));
        (e.depth, e.ramp, e.to_bed, e.on_top, e.hole_free, e.min_gap) = (30, 30, true, true, true, true);
        assert!(s.foot_target(14) < s.radius(31, 0.0), "wider than the target");
        let sp = speeds(&s, &e, 15);
        assert!((sp.slow - sp.fast).abs() < 1e-12, "the slow speed capped at the fast: {sp:?}");
        assert!((sp.fast - 0.217_501).abs() < 1e-9, "no bonus on the fast speed: {sp:?}");
        let list = entries(&s, &e, sp, true);
        assert!(list.iter().filter(|x| x.m.moves && !x.m.may_stray).all(|x| x.area == Area::Slow), "moving entries grow the slow area");
    }

    /// §5.1 "The order" for a new tip of R14's profile: hole-free, at the min gap (0.8 mm side
    /// gap), holding 2 layers. Entry 6 first; 3a-3e each at the full side gap then the min gap
    /// (3c as the new depth 1 is in the ramp); entry 5 last at the min gap.
    #[test]
    fn a_tips_entries_come_in_the_reference_order() {
        let s = bed_only();
        let mut e = Element::tip(&s, 20, Pt::from_mm(0.0, 0.0));
        (e.to_bed, e.on_top, e.hole_free, e.hold) = (true, true, true, 2);
        let sp = speeds(&s, &e, 20);
        let list = entries(&s, &e, sp, true);
        let got: Vec<(Kind, bool, bool, bool, Area)> = list.iter().map(|x| (x.m.kind, x.m.grow, x.m.moves, x.m.min_gap, x.area)).collect();
        use Area as A;
        use Kind as K;
        let want = vec![
            (K::Slow, true, false, false, A::Upper),
            (K::Slow, true, true, false, A::Slow),
            (K::Slow, true, true, true, A::Slow),
            (K::Slow, false, true, false, A::Slow),
            (K::Slow, false, true, true, A::Slow),
            (K::FastHoleFree, true, true, false, A::Slow),
            (K::FastHoleFree, true, true, true, A::Slow),
            (K::FastHoleFree, true, true, false, A::Fast),
            (K::FastHoleFree, true, true, true, A::Fast),
            (K::FastHoleFree, false, true, false, A::Fast),
            (K::FastHoleFree, false, true, true, A::Fast),
            (K::Fast, false, true, true, A::Stray),
        ];
        assert_eq!(got, want);
        assert_eq!(list[0].m.speed, 0.0, "entry 6 records no speed");
        assert!(list[11].m.may_stray && list[11].m.speed == sp.fast, "entry 5 records the fast speed");
        // Not hole-free: 2a-2e, 2b only while the new depth is under 3 (half of 5, rounded up).
        e.hole_free = false;
        let kinds = |e: &Element| entries(&s, e, sp, true).iter().filter(|x| !x.m.min_gap && !x.m.may_stray).map(|x| x.m.kind).collect::<Vec<_>>();
        assert_eq!(kinds(&e), vec![K::Slow, K::Fast, K::FastHoleFree, K::FastHoleFree, K::Fast]);
        e.depth = 2;
        assert_eq!(kinds(&e), vec![K::Slow, K::FastHoleFree, K::FastHoleFree, K::Fast], "new depth 3: past the first half");
    }

    /// §5.1-§5.2 with nothing in the way: a tip goes straight down to the bed, one element a
    /// layer, each carrying the one above; its first move is at the full side gap, so it leaves
    /// the min gap for good, and its radius follows its depth up the ramp.
    #[test]
    fn a_lone_tip_goes_straight_down() {
        let s = bed_only();
        let part = vec![Vec::new(); 12];
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let layers = fresh(&s, &v, &tips_at(&s, 12, &[(10, 0.0, 0.0)]));
        for l in 0..=10 {
            assert_eq!(layers[l].len(), 1, "one element on #{l}");
            let e = &layers[l][0];
            assert_eq!(e.depth, 10 - l, "its depth on #{l}");
            if l < 10 {
                assert_eq!(e.carries, vec![0], "it carries the one above");
                assert!(!e.min_gap && e.hole_free && e.to_bed && !e.stuck, "full side gap, hole-free, to the bed");
                assert_eq!(e.ramp, e.depth, "the radius follows the depth");
                assert_eq!(e.point, Pt::from_mm(0.0, 0.0), "the point is carried unchanged");
            }
        }
        assert!(layers[11].is_empty());
    }

    /// §5.3: two tips 0.3 mm apart grow into each other on the first layer down and merge there.
    /// The absorber is the first in box order (the lower x); its point moves into the overlap.
    #[test]
    fn two_close_tips_merge_on_the_first_layer_down() {
        let s = bed_only();
        let part = vec![Vec::new(); 12];
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let layers = fresh(&s, &v, &tips_at(&s, 12, &[(10, 0.3, 0.0), (10, 0.0, 0.0)]));
        assert_eq!(layers[9].len(), 1, "merged on #9");
        let e = &layers[9][0];
        assert_eq!(e.carries, vec![1, 0], "the absorber, the tip at x = 0, then the one it absorbed");
        let (x, y) = (e.point.x as f64 / SCALE, e.point.y as f64 / SCALE);
        assert!(x > 0.05 && x < 0.15 && y.abs() < 0.05, "the point moved onto the overlap's near edge: {x}, {y}");
        assert!(layers[..9].iter().all(|l| l.len() == 1), "one branch from there down");
    }

    /// §5.1 entry 6 and §5.2: a tip holding 2 layers stays put while its new depth is under its
    /// hold (one layer), keeping its area and placed point; then it moves, which clears the hold
    /// and the placed point.
    #[test]
    fn a_held_tip_stays_then_moves() {
        let s = bed_only();
        let part = vec![Vec::new(); 12];
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let mut t = tips_at(&s, 12, &[(10, 0.0, 0.0)]);
        t.tips[10][0].hold = 2;
        let layers = fresh(&s, &v, &t);
        let (tip, stay, moved) = (&layers[10][0], &layers[9][0], &layers[8][0]);
        assert!((total_area_mm2(&stay.area) - total_area_mm2(&tip.area)).abs() < 1e-9, "the area stays");
        assert_eq!((stay.placed, stay.hold, stay.ramp), (tip.placed, 2, 1), "the placed point and hold kept, the radius grown");
        assert!(stay.last.is_some_and(|m| !m.moves && m.kind == Kind::Slow));
        assert!(moved.last.is_some_and(|m| m.moves) && moved.hold == 0 && moved.placed.is_none(), "then it moves");
        assert!(total_area_mm2(&moved.area) > total_area_mm2(&stay.area));
    }

    /// The memo knows a tip by all of it. A run on the run before's tips takes all it made. A tip
    /// at the same place on its layer that holds 2 layers where the run before's held none, which
    /// stays put a layer rather than moving, is another tip: the memo gives it its own id, and its
    /// descent is a fresh run's.
    #[test]
    fn a_tip_is_known_by_all_of_it() {
        let s = bed_only();
        let part = vec![Vec::new(); 12];
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let t = tips_at(&s, 12, &[(10, 0.0, 0.0), (6, 3.0, 0.0)]);
        let mut memo = Memo::default();
        let first = descend(ctl(), &s, &v, &t, &mut memo).unwrap();
        let again = descend(ctl(), &s, &v, &t, &mut memo).unwrap();
        assert!(memo.made == 0 && again == first, "the same tips: all taken");
        let mut held = tips_at(&s, 12, &[(10, 0.0, 0.0), (6, 3.0, 0.0)]);
        held.tips[10][0].hold = 2;
        let warm = descend(ctl(), &s, &v, &held, &mut memo).unwrap();
        let fresh = fresh(&s, &v, &held);
        assert!(fresh != first, "holding changes the descent");
        assert!(memo.made > 0 && warm == fresh, "the held tip's descent made afresh, as a fresh run makes it");
    }

    /// An edit's descent on what the run before kept: a block put in the way of one branch, over
    /// another. The upper branch's steps on volume layers the block changed are made afresh, and
    /// so are those under them, where it comes down moved round the block; the lower branch,
    /// wholly under the block's reach, is taken step for step. Each comes out as a fresh run.
    #[test]
    fn a_step_is_taken_only_where_all_it_read_is_as_it_was() {
        let s = bed_only();
        let n = 32;
        let open = vec![Vec::new(); n];
        let block: Vec<Vec<ExPolygon>> = (0..n).map(|l| if (12..=14).contains(&l) { vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-1.0, -1.0), Pt::from_mm(1.0, 1.0)))] } else { Vec::new() }).collect();
        let before = Volumes::new(ctl(), &s, &open, &[]).unwrap();
        let blocked = || Volumes::new(ctl(), &s, &block, &[]).unwrap();
        // The run before, the edit's on the volumes taking from it, and a fresh one; with the
        // steps the edit's and the fresh one made afresh.
        let edit = |t: &Tips| {
            let mut memo = Memo::default();
            let first = descend(ctl(), &s, &before, t, &mut memo).unwrap();
            let warm = descend(ctl(), &s, &blocked().taking(&before), t, &mut memo).unwrap();
            let mut afresh = Memo::default();
            let fresh = descend(ctl(), &s, &blocked(), t, &mut afresh).unwrap();
            (first, warm, fresh, memo.made, afresh.made)
        };
        let (first, warm, fresh, made, _) = edit(&tips_at(&s, n, &[(6, 12.0, 0.0)]));
        assert!(warm == fresh && fresh == first && made == 0, "the lower branch alone is taken step for step: {made} made afresh");
        let (first, warm, fresh, made, all) = edit(&tips_at(&s, n, &[(28, 0.0, 0.0), (6, 12.0, 0.0)]));
        assert!(fresh != first && fresh[0].len() == 2, "the block moves the upper branch, and both get down");
        assert!(warm == fresh, "as a fresh run");
        let lower: usize = (1..n).map(|l| fresh[l].iter().filter(|e| e.point.x_mm() > 6.0).count()).sum();
        assert!(made > 0 && made + lower <= all, "{made} steps made afresh of {all}, none of the lower branch's {lower}");
    }

    /// A group of merges is known by its elements in their order: three tips close together that
    /// merge on the way down, then the middle one dropped, then back, on the same volumes. Every
    /// merge's volume layers are as they were, yet each run after the first, on the memo of the
    /// runs before, merges the group it has and steps what that makes afresh, and comes out as a
    /// fresh run.
    #[test]
    fn a_group_is_known_by_its_elements_in_order() {
        let s = bed_only();
        let part = vec![Vec::new(); 12];
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let three = tips_at(&s, 12, &[(10, 0.0, 0.0), (10, 0.3, 0.0), (10, 0.6, 0.0)]);
        let two = tips_at(&s, 12, &[(10, 0.0, 0.0), (10, 0.6, 0.0)]);
        let (all, ends) = (fresh(&s, &v, &three), fresh(&s, &v, &two));
        assert!(all[9].len() < 3 && ends[..9].iter().all(|l| l.len() == 1), "they merge");
        let mut memo = Memo::default();
        for (k, (t, want)) in [(&three, &all), (&two, &ends), (&three, &all)].into_iter().enumerate() {
            let warm = descend(ctl(), &s, &v, t, &mut memo).unwrap();
            assert!(warm == *want, "run {k}: as a fresh run");
            assert!(k == 0 || memo.made > 0, "run {k}: the group merged and stepped afresh");
        }
    }

    /// §5.2: a tip in a closed pocket gets no entry to succeed, not even the strayed one, and is
    /// stuck: it loses its placed point and on-top and nothing goes on below it.
    #[test]
    fn a_tip_with_nowhere_to_go_is_stuck() {
        let s = bed_only();
        let ring = |l: usize| {
            if l < 8 {
                vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-10.0, -10.0), Pt::from_mm(10.0, 10.0)))]
            } else {
                Vec::new()
            }
        };
        let part: Vec<Vec<ExPolygon>> = (0..12).map(ring).collect();
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let layers = fresh(&s, &v, &tips_at(&s, 12, &[(10, 0.0, 0.0)]));
        let e = &layers[10][0];
        assert!(e.stuck && !e.on_top && e.placed.is_none(), "stuck");
        assert!(layers[..10].iter().all(Vec::is_empty), "nothing below it");
    }

    /// Fab's bound on §5.1: past the ramp, a branch whose counter lags its depth (moves that
    /// could not grow it) gets speeds that still carry the 0.12 mm a layer a grown counter adds,
    /// yet no move takes its area further than the support angle and what its drawn radius
    /// grows, the strayed one included.
    #[test]
    fn a_move_reaches_no_further_than_the_angle_and_the_growth() {
        let s = bed_only();
        let part = vec![Vec::new(); 12];
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let mut e = Element::tip(&s, 10, Pt::from_mm(0.0, 0.0));
        (e.to_bed, e.hole_free, e.min_gap, e.depth, e.ramp) = (true, true, false, s.tip_layers + 1, 1);
        e.area = vec![super::super::element::disc(Pt::from_mm(0.0, 0.0), 0.5)];
        let sp = speeds(&s, &e, 10);
        assert!(sp.fast > s.fast + 0.1, "the speeds carry the ramp's growth: {sp:?}");
        let walls = v.of(&e).wall_restriction(ctl(), 10, s.rung_for(e.collision_radius(&s), false), true).unwrap();
        let u = Upper { ctl: ctl(), s: &s, v: &v, e: &e, l: 10, sp, walls, slow: OnceLock::new(), fast: OnceLock::new(), stray: OnceLock::new(), within: Default::default(), bounded: Default::default(), taken: RefCell::new(Vec::new()), off: RefCell::new(Vec::new()) };
        let mut tried = 0;
        for en in entries(&s, &e, sp, false).into_iter().filter(|x| x.m.moves) {
            let st = try_entry(&u, en).unwrap().unwrap_or_else(|| panic!("{:?} finds a way down in the open", en.m));
            let grows = (st.e.drawn_radius(&s) - e.drawn_radius(&s)).max(0.0);
            let far = st.e.area.iter().flat_map(|x| x.rings()).flat_map(|r| r.pts.iter()).map(|p| p.dist(Pt::from_mm(0.0, 0.0)) / SCALE).fold(0.0, f64::max);
            assert!(far <= 0.5 + s.fast + grows + 0.01, "{:?} reaches {far:.3} mm, past 0.5 + {:.3} + {grows:.3}", en.m, s.fast);
            tried += 1;
        }
        assert!(tried >= 4, "the moves that grow and those that do not, and the strayed one: {tried}");
    }

    /// Fab's departure from §8.2 ([`Element::width`]): a tip whose ramp counter lags its depth
    /// (moves that could not grow it) leaves the ramp narrower by its counter than it was by its
    /// depth; its step carries the width it had, so it is drawn no narrower below.
    #[test]
    fn a_step_carries_the_width_over_it() {
        let s = bed_only();
        let part = vec![Vec::new(); 12];
        let v = Volumes::new(ctl(), &s, &part, &[]).unwrap();
        let mut e = Element::tip(&s, 10, Pt::from_mm(0.0, 0.0));
        (e.to_bed, e.hole_free, e.depth) = (true, true, s.tip_layers - 1);
        let st = step(ctl(), &s, &v, &e, 10).unwrap().expect("a way down in the open");
        let (over, under) = (e.drawn_radius(&s), st.e.drawn_radius(&s));
        assert!(under < over, "the counters narrow it: {under} mm under {over} mm");
        assert_eq!(st.e.width(&s), over, "drawn as wide as what it carries");
    }
}

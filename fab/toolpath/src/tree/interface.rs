//! Interfaces and the lines each support layer prints (R14 §9). The top contacts and floors reach
//! into the base layers near them as interface (§9.2); the first layer's pad grows round the feet
//! and stops short of the bed's interfaces (§9.3); every layer is cut to the bed (§9.4); and each
//! layer prints (§9.5) its base hollow (§8.6), or on the first layer the pad (§8.7), and its
//! contacts, floors and interface as one region: a loop round each island, and the support-base
//! fill (§4.4) inside it at 0° on even layers and 90° on odd ones, on one grid.
//!
//! One quirk of the reference is copied for parity and marked where it is: D11, a top contact
//! reaching into N + 1 base layers, not N (§9.2). Four departures are Fab's: D15, what has nothing
//! under it does not print ([`standing`]); D20, the pad keeps clear of the part over it within the
//! top gap, not only of its first layer (§8.7); D26, each layer's roof closes over the gaps
//! between its pieces narrower than a tip's clearance, where the roof it makes is a line wide
//! ([`closed_roof`]); and D29, a path that would print on air over the hollow inside of the
//! support under it stands on a drip ([`super::drip`]).

use std::collections::HashMap;
use std::sync::Arc;

use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::Join;

use super::base_fill::{fill, Fill};
use super::clip::{difference_ex, intersection_ex, union, union_ex, FillRule};
use super::draw::{base_islands, grid_width, id_of, island_paths, pad, pad_paths, smooth_outward, walls, Bead, Bed, Drawn, Id, Rings};
use super::element::{dp_path, dp_ring};
use super::offset::{offset, ring_by_ring};
use super::settings::Settings;
use super::tips::Tips;
use super::volumes::Volumes;
use super::Input;
use crate::par::{Cancelled, Ctl};
use crate::{flow, ExtrusionPath, Role};

/// The slack (mm) on print z when contacts and floors are collected, but at the contacts' low end
/// (§9.2).
const Z_EPS: f64 = 0.0001;
/// The closing's shrink, and the smoothing's clip after it, in flow spacings (§9.2).
const CLOSE_SHRINK: f64 = 1.5;
/// The bed's contacts and interface are grown this much (grid units) before they cut the pad
/// (§9.3).
const PAD_CLEAR: f64 = 100.0;
/// The safety growth (grid units) of a layer's interface region, at each merge and again before
/// its lines (§9.5).
const SAFETY: f64 = 10.0;
/// The part frame's origin, which one line of every printed fill runs through (§1.4, §9.5).
const ORIGIN: Pt = Pt { x: 0, y: 0 };

/// How the support prints (§9.5): its speeds (mm/s), and the first layer's line (mm).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Print {
    pub speed: f64,
    pub interface_speed: f64,
    pub first_speed: f64,
    pub first_line: f64,
}

/// The support's layers as they print (§9).
#[derive(Clone, Debug, Default)]
pub struct Layers {
    /// Per layer, the base, printed hollow (§8.6); layer 0's is the pad's feet.
    pub base: Vec<Vec<ExPolygon>>,
    /// Per layer, the contacts, floors and interface as one region (§9.5).
    pub interface: Vec<Vec<ExPolygon>>,
    /// The first layer's pad (§8.7, §9.3).
    pub pad: Vec<ExPolygon>,
}

/// What [`layers`] and [`paths`] made per layer, each keyed by a 128-bit hash of the full content
/// it is made from ([`id_of`]); the print and the bed it was made for, and the box of all its
/// layers when the bed's box held each as it was. A run on the same settings ([`super::Cache`])
/// and print, on that bed or another whose box holds that box, takes a layer made before from the
/// same as it is, but the first layer's base and interface, which the pad and the part's first
/// slice reach. Each run keeps what it made or used.
#[derive(Default)]
pub struct Memo {
    bed: Vec<ExPolygon>,
    held: Option<BBox>,
    print: Option<Print>,
    /// Each layer's base and interface region.
    layers: HashMap<Id, Arc<Layer>>,
    /// The top contacts and floors that reach a base layer, closed and smoothed (§9.2), by the
    /// id of their rings.
    closed: HashMap<Id, Arc<Vec<ExPolygon>>>,
    /// What closing a layer's roof adds to its interface (D26, [`closed_roof`]), by the id of the
    /// layer's top contacts, top interface and the part's collision.
    filled: HashMap<Id, Arc<Vec<ExPolygon>>>,
    /// What of a layer from the third up meets nothing under it ([`standing`]), as the layers
    /// were made: by the layer, its id in `layers`, the id of the layer under it and the id of
    /// the part's slices under it that it can stand on.
    loose: HashMap<(usize, Id, Id, Id), Arc<Vec<ExPolygon>>>,
    /// Each layer's paths: the ids of its pieces in `pieces`, in order, and those pieces put
    /// together, which a run that prints the layer from the same takes as they are.
    paths: HashMap<Id, (Arc<[Id]>, Arc<Vec<ExtrusionPath>>)>,
    /// Each piece's paths ([`printed`]): the pad's, a base island's loops, or an interface
    /// island's loops and lines, by an id of what they are made from.
    pieces: HashMap<Id, Arc<Vec<ExtrusionPath>>>,
    /// Which of a layer's paths print on air (D29, [`super::drip::on_air`]), by the ids in
    /// `paths` of the layer and of the one under it.
    air: HashMap<(Id, Id), Arc<Vec<usize>>>,
    /// The layers the last [`layers`] put together afresh ([`super::Work`]).
    pub(super) made_layers: usize,
    /// The pieces the last [`paths`] printed afresh ([`super::Work`]).
    pub(super) made_pieces: usize,
}

/// A layer as [`layers`] put it together on the bed: its base and its interface region, and their
/// box when the bed's box held them ([`Bed::intersect`]).
struct Layer {
    base: Vec<ExPolygon>,
    interface: Vec<ExPolygon>,
    held: Option<BBox>,
}

impl Memo {
    /// Emptied unless it was made for `input`'s print, and for its bed or another whose box holds
    /// all its layers: on that bed they go on as on the one they were made on ([`Bed::intersect`]).
    fn fit(&mut self, input: &Input) {
        if self.print != Some(input.print) || (self.bed != input.bed && !self.held.is_some_and(|h| Bed::new(input.bed).holds(&h))) {
            *self = Memo { print: Some(input.print), ..Memo::default() };
        }
        if self.bed != input.bed {
            self.bed = input.bed.to_vec();
        }
    }
}

/// D26's reach (mm): half a tip's clearance, r_tip + xy_min, with the resolution to spare. F (§4.1)
/// cuts each overhang's roof back to r_tip and at most xy_min from the layer under it, where the
/// roof of the overhang a layer down ends: the most a layer's roof leaves between two of them.
fn closing(s: &Settings) -> f64 {
    (s.r_tip + s.xy_min) / 2.0 + s.keys.resolution
}

/// D26, Fab's, not the reference's: what closes every gap between the roof pieces of a layer, its
/// top contacts `contact` and top interface `top`, narrower than twice `reach` (units) (grown and
/// shrunk back with `arc`), less `part`, what keeps support off the part, where the roof it makes
/// is a `line` (units) wide; the layer's interface takes it in. On a gentle slope a layer's roof
/// is the crescents the overhangs over it leave, each narrower than a line, which print no line
/// (R6), each a tip's clearance from the next ([`closing`]); closed, they print as one band, as
/// the reference prints them. A closing alone would span a part thinner than the gap between two
/// pieces either side of it, and join two pieces near its reach by a waist narrower than a line,
/// which prints no line.
fn closed_roof(contact: &[ExPolygon], top: &[ExPolygon], part: &[ExPolygon], reach: f64, line: f64, arc: Join) -> Vec<ExPolygon> {
    let roof = union_ex(&[contact, top].concat());
    let closed = less(offset(&offset(&roof, reach, arc), -reach, arc), part);
    less(offset(&offset(&closed, -line / 2.0, arc), line / 2.0, arc), &roof)
}

/// `a` less `b`.
fn less(a: Vec<ExPolygon>, b: &[ExPolygon]) -> Vec<ExPolygon> {
    if a.is_empty() || b.is_empty() { a } else { difference_ex(&a, b) }
}

/// The support's layers from the drawing (§9.2 to §9.4): `drawn`'s base and floors, and the top
/// contacts and precomputed top interfaces of `roofs`, the bed's roofs among them, each layer's
/// roof closed (D26) clear of `v`'s collision at radius 0. A layer `memo` holds from a run before
/// is taken from it, and `memo` keeps this run's.
pub fn layers(ctl: Ctl, s: &Settings, v: &Volumes, input: &Input, drawn: Drawn, roofs: &Tips, memo: &mut Memo) -> Result<Layers, Cancelled> {
    memo.fit(input);
    let heights = input.heights;
    let n = heights.len();
    let Drawn { mut base, floor } = drawn;
    let at = |v: &[Vec<ExPolygon>], l: usize| v.get(l).cloned().unwrap_or_default();
    let mut contact: Vec<Vec<ExPolygon>> = (0..n).map(|l| at(&roofs.contact, l)).collect();
    let mut top: Vec<Vec<ExPolygon>> = (0..n).map(|l| at(&roofs.interface, l)).collect();
    let floor: Vec<Vec<ExPolygon>> = (0..n).map(|l| at(&floor, l)).collect();
    base.resize(n, Vec::new());
    let (tops, bottoms) = (s.keys.roof_layers, s.keys.floor_layers);
    // §9.1: a bed roof is one more roof on layer 0, a contact at depth 0, else an interface.
    if n > 0 {
        for (area, depth) in &roofs.bed {
            let into = if (*depth).min(tops) == 0 { &mut contact[0] } else { &mut top[0] };
            into.extend(area.iter().cloned());
        }
    }
    // §9.2, per base layer j, over the list of the layers that have a base.
    let sn = grid_width(s.flow_spacing);
    let density = s.flow_spacing / s.roof_line_spacing;
    let (grow, shrink) = ((sn / density + CLOSE_SHRINK * sn) as f32 as f64, (CLOSE_SHRINK * sn) as f32 as f64);
    let clip = (CLOSE_SHRINK * sn).trunc();
    let z = |l: usize| heights[l].0;
    let bottom = |l: usize| heights[l].0 - heights[l].1;
    let listed: Vec<usize> = (0..n).filter(|&l| !base[l].is_empty()).collect();
    let mut place: Vec<Option<usize>> = vec![None; n];
    for (k, &j) in listed.iter().enumerate() {
        place[j] = Some(k);
    }
    // The top contacts and floors that reach the `k`th base layer.
    let gathered = |k: usize| -> Vec<ExPolygon> {
        let j = listed[k];
        let mut got: Vec<ExPolygon> = Vec::new();
        if tops > 0 {
            // D11 (§9.2, copied for parity): up to the base layer N − 1 places above j, so a
            // contact reaches N + 1 base layers; that place clamped into the list, so past its
            // end the last base layer (Q5).
            let hi = z(listed[(k + tops - 1).min(listed.len() - 1)]);
            for l in j..n {
                if bottom(l) > hi + Z_EPS {
                    break;
                }
                got.extend(contact[l].iter().cloned());
            }
        }
        if bottoms > 0 {
            // From the bottom of the base layer N − 1 places below j, that place clamped into the
            // list, so before its start the first base layer (Q5).
            let lo = bottom(listed[(k + 1).saturating_sub(bottoms)]);
            for l in (0..j).rev() {
                if z(l) < lo - Z_EPS {
                    break;
                }
                if z(l) <= bottom(j) + Z_EPS {
                    got.extend(floor[l].iter().cloned());
                }
            }
        }
        got
    };
    // §9.4: every layer of every kind on the bed, once.
    let bed = Bed::new(input.bed);
    // Per layer, its id and its base and interface region (§9.2, §9.4); the first layer's before
    // the pad is cut out of what is left of its base, and without an id. What reaches a layer,
    // closed and smoothed, is taken as the memo holds it when the same rings reach it, and the
    // closing of its roof when the same roof and part make it: a delete changes the base of most
    // layers but what reaches few, and few roofs.
    let (seen, closings, fills) = (&memo.layers, &memo.closed, &memo.filled);
    // D26: what the closing of each layer's roof keeps off, as every base piece does (D18), its
    // reach and the line it keeps.
    let part: Vec<&[ExPolygon]> = (0..n).map(|l| v.collision(ctl, l, 0)).collect::<Result<_, _>>()?;
    let (roof_reach, line, arc) = (closing(s) * SCALE, s.keys.line_width * SCALE, Join::Round(s.keys.resolution * SCALE));
    let made = crate::par::try_map(ctl, n, |l| {
        let got = place[l].map_or_else(Vec::new, gathered);
        let id = (l > 0).then(|| id_of(&(Rings(&got), Rings(&base[l]), Rings(&top[l]), Rings(&contact[l]), Rings(&floor[l]), Rings(part[l]))));
        let reach = (!got.is_empty()).then(|| id_of(&Rings(&got)));
        let roof = (!contact[l].is_empty() || !top[l].is_empty()).then(|| id_of(&(Rings(&contact[l]), Rings(&top[l]), Rings(part[l]))));
        if let Some(made) = id.and_then(|id| seen.get(&id)) {
            return (id, made.clone(), reach.and_then(|r| closings.get(&r).map(|c| (r, c.clone()))), roof.and_then(|r| fills.get(&r).map(|f| (r, f.clone()))));
        }
        let closing = reach.map(|r| {
            let closed = closings.get(&r).cloned().unwrap_or_else(|| {
                // The collected rings as they are, not their union: each drops its own close
                // points before the grow (§1.5), and the grow unites them.
                let closed = offset(&offset(&got, grow, Join::Square), -shrink, Join::Square);
                Arc::new(smooth_outward(&closed, clip))
            });
            (r, closed)
        });
        let interface = if place[l].is_some() {
            let mut area = match &closing {
                Some((_, closed)) => intersection_ex(closed, &base[l]),
                None => Vec::new(),
            };
            if !top[l].is_empty() {
                area.extend(top[l].iter().cloned());
                area = union_ex(&area);
            }
            area
        } else if !top[l].is_empty() {
            // A precomputed top interface on a layer without a base is a layer of its own.
            union_ex(&top[l])
        } else {
            Vec::new()
        };
        let fill = roof.map(|r| (r, fills.get(&r).cloned().unwrap_or_else(|| Arc::new(closed_roof(&contact[l], &top[l], part[l], roof_reach, line, arc)))));
        let interface = match &fill {
            Some((_, filled)) if !filled.is_empty() => union_ex(&[interface.as_slice(), filled.as_slice()].concat()),
            _ => interface,
        };
        let rest = if place[l].is_some() { less(base[l].clone(), &interface) } else { Vec::new() };
        if l == 0 {
            return (None, Arc::new(Layer { base: rest, interface, held: None }), closing, fill);
        }
        let mut held = Some(BBox::empty());
        let rest = bed.intersect(rest, &mut held);
        let region = merged(bed.intersect(contact[l].clone(), &mut held), bed.intersect(interface, &mut held), bed.intersect(floor[l].clone(), &mut held));
        (id, Arc::new(Layer { base: rest, interface: region, held }), closing, fill)
    })?;
    memo.made_layers = made.iter().filter(|m| m.0.is_some_and(|id| !seen.contains_key(&id))).count();
    memo.closed = made.iter().filter_map(|m| m.2.clone()).collect();
    memo.filled = made.iter().filter_map(|m| m.3.clone()).collect();
    let mut base: Vec<Vec<ExPolygon>> = made.iter().map(|m| m.1.base.clone()).collect();
    let mut interface: Vec<Vec<ExPolygon>> = made.iter().map(|m| m.1.interface.clone()).collect();
    // §9.3: the pad from what is left of base 0, less the bed's contacts and interface; then
    // layer 0 on the bed as every other.
    let mut pad_area = Vec::new();
    if n > 0 {
        if !base[0].is_empty() {
            // D20, Fab's: the pad keeps clear of the part over it within the top gap as of the
            // part's first layer, as all support does (R5): a part flaring out over its first
            // layer has no pad under its edge. Fab's keep-out on the first layer is kept clear as
            // the part is.
            let near = input.slices.iter().take(1 + s.top_gap_layers).chain(input.keep_out.first());
            let part = union_ex(&near.flatten().cloned().collect::<Vec<_>>());
            let clear: Vec<ExPolygon> = [&contact[0], &interface[0]].into_iter().flat_map(|a| offset(a, PAD_CLEAR, Join::Square)).collect();
            pad_area = less(pad(&base[0], &part, input.print.first_line, s.keys.first_layer_gap), &clear);
        }
        base[0] = bed.intersect(std::mem::take(&mut base[0]), &mut None);
        interface[0] = merged(bed.intersect(contact[0].clone(), &mut None), bed.intersect(std::mem::take(&mut interface[0]), &mut None), bed.intersect(floor[0].clone(), &mut None));
    }
    let pad_area = bed.intersect(pad_area, &mut None);
    let ids: Vec<Option<Id>> = made.iter().map(|m| m.0).collect();
    memo.held = made.iter().filter(|m| m.0.is_some()).try_fold(BBox::empty(), |mut all, m| {
        all.merge(&m.1.held?);
        Some(all)
    });
    memo.layers = made.into_iter().filter_map(|(id, m, _, _)| id.map(|id| (id, m))).collect();
    // D15 on the layers as put together: what one stands on is the layer under it.
    let (base, interface) = standing(ctl, s, input, &pad_area, (base, interface), &ids, &mut memo.loose)?;
    Ok(Layers { base, interface, pad: pad_area })
}

/// D15, Fab's, not the reference's: what has nothing under it does not print. From the second
/// layer up, an island of a layer's base and interface together goes when it meets nothing under
/// it: neither what is left of the support on the layer under it (on the first layer, with the
/// pad) nor, but from the bed only, the part within the bottom gap and [`LAND_AIR`] under it, as
/// the rules count a footing (R3). §8.7's pad cut can leave a top contact on the second layer over
/// a hole in the part's first layer, and §9 prints every contact. The layers are taken bottom-up,
/// so what stood only on what went goes too.
///
/// As the layers were made, what of a layer from the third up meets nothing under it follows
/// from what made it, the layer under it, which `ids` name ([`Memo`]; the first layer has none,
/// as the pad cuts it), and the part's slices under it it can stand on: what `kept` holds by them
/// from a run before is taken, and `kept` keeps this run's.
///
/// [`LAND_AIR`]: crate::support::LAND_AIR
fn standing(ctl: Ctl, s: &Settings, input: &Input, pad: &[ExPolygon], (mut base, mut interface): (Vec<Vec<ExPolygon>>, Vec<Vec<ExPolygon>>), ids: &[Option<Id>], kept: &mut HashMap<(usize, Id, Id, Id), Arc<Vec<ExPolygon>>>) -> Result<(Vec<Vec<ExPolygon>>, Vec<Vec<ExPolygon>>), Cancelled> {
    let n = base.len().min(interface.len());
    // Whether `e` overlaps any of `set`.
    let meets = |e: &ExPolygon, set: &[ExPolygon]| -> bool {
        let bb = e.bbox();
        let near: Vec<ExPolygon> = set.iter().filter(|u| u.bbox().overlaps(&bb)).cloned().collect();
        !near.is_empty() && !intersection_ex(std::slice::from_ref(e), &near).is_empty()
    };
    let land = s.bottom_gap_layers + 1 + (crate::support::LAND_AIR / s.keys.layer_height).ceil() as usize;
    // The islands of layer `l` that meet nothing under them.
    let loose = |l: usize, base: &[Vec<ExPolygon>], interface: &[Vec<ExPolygon>]| -> Vec<ExPolygon> {
        if base[l].is_empty() && interface[l].is_empty() {
            return Vec::new();
        }
        let mut under: Vec<ExPolygon> = base[l - 1].iter().chain(&interface[l - 1]).cloned().collect();
        if l == 1 {
            under.extend(pad.iter().cloned());
        }
        let on_part = |e: &ExPolygon| !s.keys.bed_only && (l.saturating_sub(land)..l).any(|k| input.slices.get(k).is_some_and(|part| meets(e, part)));
        union_ex(&[base[l].as_slice(), interface[l].as_slice()].concat()).into_iter().filter(|e| !meets(e, &under) && !on_part(e)).collect()
    };
    // Each slice of the part by its id, for the layers over it that can stand on it.
    let sliced: Vec<Id> = crate::par::try_map(ctl, input.slices.len(), |k| id_of(&Rings(&input.slices[k])))?;
    // Every layer against the layers as made, at once; then, bottom-up, a layer over one that lost
    // something once more.
    let seen = &*kept;
    let first = crate::par::try_map(ctl, n, |l| {
        let key = match (l, ids.get(l).copied().flatten(), l.checked_sub(1).and_then(|u| ids.get(u).copied().flatten())) {
            (2.., Some(a), Some(b)) => {
                let under = &sliced[l.saturating_sub(land).min(sliced.len())..l.min(sliced.len())];
                Some((l, a, b, id_of(&under)))
            }
            _ => None,
        };
        let found = match key.and_then(|k| seen.get(&k)) {
            Some(found) => found.clone(),
            None if l == 0 => Arc::new(Vec::new()),
            None => Arc::new(loose(l, &base, &interface)),
        };
        (key, found)
    })?;
    *kept = first.iter().filter_map(|(key, found)| key.map(|k| (k, found.clone()))).collect();
    let mut lost = false;
    for (l, (_, found)) in first.into_iter().enumerate().skip(1) {
        let gone = if lost { Arc::new(loose(l, &base, &interface)) } else { found };
        lost = !gone.is_empty();
        if lost {
            base[l] = less(std::mem::take(&mut base[l]), &gone);
            interface[l] = less(std::mem::take(&mut interface[l]), &gone);
        }
    }
    Ok((base, interface))
}

/// `a` and `b` united with the safety growth (§9.5, Q7): every ring of both grown by it on its
/// own (a hole shrinks), united under the non-zero rule, and the growth kept.
fn safe_union(a: &[ExPolygon], b: &[ExPolygon]) -> Vec<ExPolygon> {
    union(&ring_by_ring(&[a, b].concat(), SAFETY, Join::Miter(3.0)), FillRule::NonZero)
}

/// A layer's one interface region (§9.5): its interface into its contacts if both are there,
/// then its floors into the contacts if both are there, else its interface into its floors; each
/// merge a union with the safety growth.
fn merged(contact: Vec<ExPolygon>, interface: Vec<ExPolygon>, floor: Vec<ExPolygon>) -> Vec<ExPolygon> {
    let (mut c, mut i, mut f) = (union_ex(&contact), interface, floor);
    if !c.is_empty() && !i.is_empty() {
        c = safe_union(&c, &std::mem::take(&mut i));
    }
    if !c.is_empty() && !f.is_empty() {
        c = safe_union(&c, &std::mem::take(&mut f));
    } else if !i.is_empty() && !f.is_empty() {
        f = safe_union(&f, &std::mem::take(&mut i));
    }
    [c, i, f].into_iter().find(|r| !r.is_empty()).unwrap_or_default()
}

/// What each layer prints (§9.5), in order: the base's walls (§8.6), or on the first layer the
/// pad (§8.7); then each island of the interface region grown by the safety growth, its loop half
/// a flow spacing in and its lines a line in, in the support line at the layer's height. Every
/// path is then simplified once more at the resolution, as the writer gets it (§8.7 Order). Last,
/// D29, the drips under what of the layers over it would print on air ([`super::drip`]). A
/// layer's paths `memo` holds from a run before are taken from it, and `memo` keeps this run's.
/// A layer prints in pieces, the pad and each island on its own ([`printed`]), and a layer
/// printed again takes the pieces `memo` holds: a delete prints again only the islands it
/// changed. Which of a layer's paths print on air follows from it and the layer under it.
pub fn paths(ctl: Ctl, s: &Settings, input: &Input, layers: &Layers, memo: &mut Memo) -> Result<Vec<Arc<Vec<ExtrusionPath>>>, Cancelled> {
    memo.fit(input);
    let (seen, kept, aired) = (&memo.paths, &memo.pieces, &memo.air);
    let n = input.heights.len();
    let made = crate::par::try_map(ctl, n, |l| {
        let (z, h) = input.heights[l];
        // The layer's id: what it prints from, its height and which way its lines run.
        let id = id_of(&(l == 0, l % 2, z.to_bits(), h.to_bits(), Rings(if l == 0 { &layers.pad } else { &layers.base[l] }), Rings(&layers.interface[l])));
        let (pieces, out) = match seen.get(&id) {
            Some((ids, out)) => (ids.iter().map(|i| (*i, kept[i].clone())).collect(), out.clone()),
            None => {
                let pieces = printed(s, input, layers, l, kept);
                let out = Arc::new(pieces.iter().flat_map(|p| p.1.iter().cloned()).collect::<Vec<_>>());
                (pieces, out)
            }
        };
        (id, pieces, out)
    })?;
    // D29: what of each layer prints on air, over what the support covers on the layer under it.
    let air = crate::par::try_map(ctl, n, |l| {
        let under = l.checked_sub(1)?;
        let key = (made[l].0, made[under].0);
        let support: [&[ExPolygon]; 2] = [if under == 0 { &layers.pad } else { &layers.base[under] }, &layers.interface[under]];
        Some((key, aired.get(&key).cloned().unwrap_or_else(|| Arc::new(super::drip::on_air(&made[l].2, &made[under].2, &support)))))
    })?;
    let on_air: Vec<(usize, usize)> = air.iter().enumerate().flat_map(|(l, a)| a.iter().flat_map(move |(_, found)| found.iter().map(move |&k| (l, k)))).collect();
    let out: Vec<Arc<Vec<ExtrusionPath>>> = made.iter().map(|m| m.2.clone()).collect();
    let drips = super::drip::drips(s, input, layers, &out, &on_air);
    let out = crate::par::try_map(ctl, n, |l| {
        let out = if drips[l].is_empty() { out[l].clone() } else { Arc::new(out[l].iter().chain(&drips[l]).cloned().collect()) };
        super::tell(ctl, super::Stage::Paths, l, n, super::Grown::Paths(&out));
        out
    })?;
    memo.made_pieces = made.iter().flat_map(|m| m.1.iter()).filter(|p| !kept.contains_key(&p.0)).count();
    memo.pieces = made.iter().flat_map(|m| m.1.iter().cloned()).collect();
    memo.paths = made.iter().map(|(id, pieces, out)| (*id, (pieces.iter().map(|p| p.0).collect(), out.clone()))).collect();
    memo.air = air.into_iter().flatten().collect();
    Ok(out)
}

/// What layer `l` of `layers` prints ([`paths`]), in pieces by their ids: the pad's paths, each
/// base island's or each interface island's. A piece is all its own island's (and an island's
/// pieces are taken whole): each is as `kept` holds it from a run before, else made.
fn printed(s: &Settings, input: &Input, layers: &Layers, l: usize, kept: &HashMap<Id, Arc<Vec<ExtrusionPath>>>) -> Vec<(Id, Arc<Vec<ExtrusionPath>>)> {
    let p = &input.print;
    let line = s.keys.line_width;
    let tol = s.keys.resolution * SCALE;
    let (z, h) = input.heights[l];
    let mut pieces = Vec::new();
    let mut piece = |what: Id, make: &dyn Fn() -> Vec<ExtrusionPath>| {
        let paths = kept.get(&what).cloned().unwrap_or_else(|| Arc::new(make().into_iter().map(|path| simplified_last(path, tol)).collect()));
        pieces.push((what, paths));
    };
    if l == 0 {
        let (density, bead) = (s.keys.first_layer_density / 100.0, Bead { w: p.first_line, h, speed: p.first_speed });
        piece(id_of(&(0u8, Rings(&layers.pad), h.to_bits())), &|| pad_paths(&layers.pad, density, ORIGIN, bead));
    } else {
        let bead = Bead { w: line, h, speed: p.speed };
        for base in base_islands(&layers.base[l], line) {
            let two = walls(base.area_mm2(), z, s.keys.wall_count) == 2;
            piece(id_of(&(1u8, Rings(std::slice::from_ref(&base)), two, h.to_bits())), &|| island_paths(&base, two, bead));
        }
    }
    let bead = Bead { w: line, h, speed: p.interface_speed };
    let lines = Fill { angle_deg: if l % 2 == 0 { 0.0 } else { 90.0 }, spacing: s.roof_line_spacing, flow: s.flow_spacing, frozen: s.roof_line_spacing, anchor: Some(ORIGIN) };
    let half = 0.5 * grid_width(flow::spacing(line, h));
    for top in &layers.interface[l] {
        let made = || {
            let grown = offset(std::slice::from_ref(top), SAFETY, Join::Miter(3.0));
            let mut out: Vec<ExtrusionPath> = offset(&grown, -half, Join::Square).iter().flat_map(|e| e.rings()).map(|r| bead.closed(Role::SupportInterface, r.pts.clone())).collect();
            out.extend(fill(&offset(&grown, -grid_width(line), Join::Square), &lines).into_iter().map(|pts| bead.open(Role::SupportInterface, pts)));
            out
        };
        piece(id_of(&(2u8, Rings(std::slice::from_ref(top)), l % 2, h.to_bits())), &made);
    }
    pieces
}

/// `path` as the writer gets it (§8.7 Order, §9.5): simplified once more at `tol` (units) by
/// Douglas–Peucker with both its ends fixed, a loop its ring from its first point round to that
/// point again. Loops, lines and links alike lose every point within `tol` of the line between
/// the points kept round it. The tree's paths carry no per-segment data to keep in step.
fn simplified_last(mut path: ExtrusionPath, tol: f64) -> ExtrusionPath {
    if path.closed {
        path.pts = dp_ring(&path.pts, tol);
        path.loop_pts = path.pts.clone();
    } else {
        path.pts = dp_path(&path.pts, tol);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::super::element::{ngon, roof_disc};
    use super::super::settings::tests::r14;
    use super::super::settings::Keys;
    use super::*;

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1)))
    }

    fn keys() -> Keys {
        r14(0.35).keys
    }

    const PRINT: Print = Print { speed: 150.0, interface_speed: 80.0, first_speed: 50.0, first_line: 0.5 };

    fn heights(n: usize) -> Vec<(f64, f64)> {
        (0..n).map(|l| if l == 0 { (0.25, 0.25) } else { (0.25 + 0.2 * l as f64, 0.2) }).collect()
    }

    fn bed() -> Vec<ExPolygon> {
        vec![square(-100.0, -100.0, 100.0, 100.0)]
    }

    /// Whether `p` (mm) lies in `set`.
    fn inside(set: &[ExPolygon], x: f64, y: f64) -> bool {
        set.iter().any(|e| e.contains(Pt::from_mm(x, y)))
    }

    /// §9.2: a top contact on layer 8 reaches base layers 8, 7 and 6 (D11: N + 1 of them), and a
    /// floor on layer 3 reaches 4 and 5. Each copy is the contact or floor grown by the roof's
    /// pitch, 0.877 mm, inside the base; on the contact's own layer that is a halo round it. The
    /// base loses them all, and each layer prints them as one region.
    #[test]
    fn contacts_reach_three_base_layers_and_floors_two() {
        let s = Settings::new(keys());
        let n = 12;
        let (hs, bed) = (heights(n), bed());
        let big = square(-10.0, -10.0, 10.0, 10.0);
        let small = square(-1.0, -1.0, 1.0, 1.0);
        let mut base: Vec<Vec<ExPolygon>> = (0..=10).map(|_| vec![big.clone()]).collect();
        base.push(Vec::new());
        base[8] = difference_ex(&base[8], std::slice::from_ref(&small));
        base[3] = difference_ex(&base[3], std::slice::from_ref(&small));
        let mut floor = vec![Vec::new(); n];
        floor[3] = vec![small.clone()];
        let mut contact = vec![Vec::new(); n];
        contact[8] = vec![small.clone()];
        let roofs = Tips { tips: vec![Vec::new(); n], contact, interface: vec![Vec::new(); n], bed: Vec::new() };
        let slices = vec![Vec::new(); n];
        let input = Input { keys: keys(), print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let v = Volumes::new(Ctl::default(), &s, &slices, &[]).unwrap();
        let out = layers(Ctl::default(), &s, &v, &input, Drawn { base, floor }, &roofs, &mut Memo::default()).unwrap();
        let with: Vec<usize> = (0..n).filter(|&l| !out.interface[l].is_empty()).collect();
        assert_eq!(with, vec![3, 4, 5, 6, 7, 8], "the floor's layer and two over it, the contact's and two under it");
        for l in [4, 5, 6, 7, 8] {
            assert!(inside(&out.interface[l], 0.0, 0.0), "layer {l}");
            assert!(inside(&out.interface[l], 1.85, 0.0) && !inside(&out.interface[l], 1.9, 0.0), "layer {l}: 0.877 mm out from the side");
            assert!(!inside(&out.base[l], 1.85, 0.0) && inside(&out.base[l], 1.9, 0.0), "layer {l}: and out of the base");
        }
        assert!(inside(&out.interface[8], 0.0, 0.0), "the contact's own layer holds the contact and its halo");
        assert!(inside(&out.interface[3], 0.0, 0.0) && !inside(&out.interface[3], 1.1, 0.0), "the floor's own layer is just the floor");
    }

    /// §9.2 Q5 with N = 3: at the ends of the base-layer list the place a bound counts to is
    /// clamped into the list. A contact on layer 11, over the last base layer 10, reaches 8, 9 and
    /// 10: from 9 the place two on is past the end, so its bound is layer 10's print z, not 9's
    /// own. A floor on layer 1, on the part's top on layer 0 and under the first base layer 2,
    /// reaches 2, 3 and 4 the same way.
    #[test]
    fn at_the_lists_ends_the_bounds_clamp_into_it() {
        let three = || Keys { roof_layers: 3, floor_layers: 3, ..keys() };
        let s = Settings::new(three());
        let n = 12;
        let (hs, bed) = (heights(n), bed());
        let small = square(-1.0, -1.0, 1.0, 1.0);
        let base: Vec<Vec<ExPolygon>> = (0..n).map(|l| if (2..=10).contains(&l) { vec![square(-10.0, -10.0, 10.0, 10.0)] } else { Vec::new() }).collect();
        let mut floor = vec![Vec::new(); n];
        floor[1] = vec![small.clone()];
        let mut contact = vec![Vec::new(); n];
        contact[11] = vec![small.clone()];
        let roofs = Tips { tips: vec![Vec::new(); n], contact, interface: vec![Vec::new(); n], bed: Vec::new() };
        let mut slices = vec![Vec::new(); n];
        slices[0] = vec![small];
        let input = Input { keys: three(), print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let v = Volumes::new(Ctl::default(), &s, &slices, &[]).unwrap();
        let out = layers(Ctl::default(), &s, &v, &input, Drawn { base, floor }, &roofs, &mut Memo::default()).unwrap();
        let held: Vec<usize> = (2..=10).filter(|&l| inside(&out.interface[l], 0.0, 0.0)).collect();
        assert_eq!(held, vec![2, 3, 4, 8, 9, 10], "the floor reaches 2, 3 and 4, the contact 8, 9 and 10");
    }

    /// §9.2 with §1.5: the closing takes the collected contacts as they are, each dropping its own
    /// close points before the grow. Two contacts on layer 8, the right one 7 µm higher: where they
    /// meet, their union has two corners 7 µm apart, under the grow's 7.2 µm threshold, and
    /// dropping either would tilt a lower edge. Ring by ring both edges stay level, so the layers
    /// under them get each one 0.877 mm out.
    #[test]
    fn the_closing_drops_each_contacts_own_close_points() {
        let s = Settings::new(keys());
        let n = 12;
        let (hs, bed) = (heights(n), bed());
        let (left, right) = (square(-2.0, -1.0, 0.0, 1.0), square(-1.0, -0.993, 8.0, 1.0));
        let mut base: Vec<Vec<ExPolygon>> = (0..=10).map(|_| vec![square(-10.0, -10.0, 10.0, 10.0)]).collect();
        base.push(Vec::new());
        base[8] = difference_ex(&base[8], &[left.clone(), right.clone()]);
        let mut contact = vec![Vec::new(); n];
        contact[8] = vec![left, right];
        let roofs = Tips { tips: vec![Vec::new(); n], contact, interface: vec![Vec::new(); n], bed: Vec::new() };
        let slices = vec![Vec::new(); n];
        let input = Input { keys: keys(), print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let v = Volumes::new(Ctl::default(), &s, &slices, &[]).unwrap();
        let out = layers(Ctl::default(), &s, &v, &input, Drawn { base, floor: vec![Vec::new(); n] }, &roofs, &mut Memo::default()).unwrap();
        let a = &out.interface[7];
        // 0.877078 mm out: the left contact's edge at -1.877078, the right one's at -1.870078.
        for x in [-1.5, -1.0] {
            assert!(inside(a, x, -1.8760) && !inside(a, x, -1.8782), "x {x}: the left contact's edge, level");
        }
        for x in [1.0, 4.0] {
            assert!(inside(a, x, -1.8690) && !inside(a, x, -1.8712), "x {x}: the right contact's edge, level");
        }
    }

    /// D26: a layer's roof closes over a gap between its pieces narrower than a tip's clearance,
    /// 0.61 mm, into one band, but not over the part: a fin 0.1 mm thick between two pieces, each
    /// its xy_min (0.21 mm) aside, which the closing alone spans, keeps its xy_min clear (R5).
    #[test]
    fn a_roof_closes_over_its_gaps_but_not_over_the_part() {
        let s = Settings::new(keys());
        let (n, f) = (4, s.xy_min);
        assert!((f - 0.21).abs() < 1e-9, "xy_min {f}");
        let (hs, bed) = (heights(n), bed());
        let fin = square(-0.05, -2.0, 0.05, 2.0);
        let hugging = vec![square(-0.4 - f, -2.0, -0.05 - f, 2.0), square(0.05 + f, -2.0, 0.4 + f, 2.0)];
        let pair = vec![square(3.0, -2.0, 3.3, 2.0), square(3.65, -2.0, 3.95, 2.0)];
        let pieces = [hugging.clone(), pair].concat();
        let mut contact = vec![Vec::new(); n];
        contact[2] = pieces.clone();
        let roofs = Tips { tips: vec![Vec::new(); n], contact, interface: vec![Vec::new(); n], bed: Vec::new() };
        let slices: Vec<Vec<ExPolygon>> = (0..n).map(|l| if l >= 2 { vec![fin.clone()] } else { Vec::new() }).collect();
        let input = Input { keys: keys(), print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let base = vec![pieces.clone(), pieces, Vec::new(), Vec::new()];
        let v = Volumes::new(Ctl::default(), &s, &slices, &[]).unwrap();
        let out = layers(Ctl::default(), &s, &v, &input, Drawn { base, floor: vec![Vec::new(); n] }, &roofs, &mut Memo::default()).unwrap();
        let roof = &out.interface[2];
        assert!(inside(roof, 3.475, 0.0) && inside(roof, 3.475, 1.9), "the pair's gap is roof: one band");
        let (reach, arc) = (closing(&s) * SCALE, Join::Round(s.keys.resolution * SCALE));
        assert!(inside(&offset(&offset(&hugging, reach, arc), -reach, arc), 0.0, 0.0), "the closing alone spans the fin");
        let clear = offset(std::slice::from_ref(&fin), (f - 0.001) * SCALE, Join::Round(s.keys.resolution * SCALE));
        assert!(intersection_ex(roof, &clear).is_empty(), "nothing within xy_min of the fin");
        assert!(inside(roof, -0.3, 0.0) && inside(roof, 0.3, 0.0), "the pieces either side print");
    }

    /// D26: two pieces just within the closing's reach are not joined by a waist narrower than a
    /// line (R6): two tip discs 0.35 mm apart, which the closing alone joins by a waist of about
    /// 0.2 mm, stay two.
    #[test]
    fn a_roof_closes_no_waist_narrower_than_a_line() {
        let s = Settings::new(keys());
        let n = 4;
        let (hs, bed) = (heights(n), bed());
        let d = 2.0 * s.r_tip + 0.35;
        let discs = vec![roof_disc(Pt::from_mm(0.0, 0.0), s.r_tip), roof_disc(Pt::from_mm(d, 0.0), s.r_tip)];
        let mut contact = vec![Vec::new(); n];
        contact[2] = discs.clone();
        let roofs = Tips { tips: vec![Vec::new(); n], contact, interface: vec![Vec::new(); n], bed: Vec::new() };
        let slices: Vec<Vec<ExPolygon>> = vec![Vec::new(); n];
        let input = Input { keys: keys(), print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let base = vec![discs.clone(), discs.clone(), Vec::new(), Vec::new()];
        let v = Volumes::new(Ctl::default(), &s, &slices, &[]).unwrap();
        let out = layers(Ctl::default(), &s, &v, &input, Drawn { base, floor: vec![Vec::new(); n] }, &roofs, &mut Memo::default()).unwrap();
        let (reach, half, arc) = (closing(&s) * SCALE, s.keys.line_width / 2.0 * SCALE, Join::Round(s.keys.resolution * SCALE));
        let closed = offset(&offset(&discs, reach, arc), -reach, arc);
        assert!(inside(&closed, d / 2.0, 0.0), "the closing alone joins them");
        assert!(!inside(&offset(&offset(&closed, -half, arc), half, arc), d / 2.0, 0.0), "by a waist narrower than a line");
        let roof = &out.interface[2];
        assert!(!inside(roof, d / 2.0, 0.0) && roof.len() == 2, "no waist: two pieces");
        assert!(inside(roof, 0.0, 0.0) && inside(roof, d, 0.0), "both discs print");
    }

    /// §9.1, §9.3: a bed roof deeper than 0 is one interface layer on layer 0, cut out of the pad,
    /// which stops 100 nm short of it.
    #[test]
    fn a_bed_roof_is_cut_out_of_the_pad() {
        let s = Settings::new(keys());
        let n = 3;
        let (hs, bed) = (heights(n), bed());
        let foot = ngon(Pt::from_mm(-5.0, 0.0), 1.0, 360);
        let roof = square(-3.5, -1.0, -2.5, 1.0);
        let roofs = Tips { tips: vec![Vec::new(); n], contact: vec![Vec::new(); n], interface: vec![Vec::new(); n], bed: vec![(vec![roof.clone()], 2)] };
        let slices = vec![Vec::new(); n];
        let input = Input { keys: keys(), print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let drawn = Drawn { base: vec![vec![foot], Vec::new(), Vec::new()], floor: vec![Vec::new(); n] };
        let v = Volumes::new(Ctl::default(), &s, &slices, &[]).unwrap();
        let out = layers(Ctl::default(), &s, &v, &input, drawn, &roofs, &mut Memo::default()).unwrap();
        assert!(inside(&out.interface[0], -3.0, 0.0), "the roof prints as interface on layer 0");
        assert!(inside(&out.pad, -4.0, 0.0) && !inside(&out.pad, -3.0, 0.0), "the pad grows round the foot but not into the roof");
        // The distance (nm) from `p` to the roof's square.
        let off = |p: &Pt| ((p.x + 3_000_000).abs() as f64 - 500_000.0).max(0.0).hypot((p.y.abs() as f64 - 1_000_000.0).max(0.0));
        let gap = out.pad.iter().flat_map(|e| e.rings()).flat_map(|r| r.pts.iter()).map(off).fold(f64::MAX, f64::min);
        assert!((99.0..=110.0).contains(&gap), "100 nm short of it: {gap} nm");
    }

    /// D20: a part flaring out over its first layer, 0.3 mm a side on the second, which is within
    /// the top gap: the pad round a foot beside it stops its gap, 0.2 mm, short of the second
    /// layer's edge, where §8.7 alone would take it to the first layer's.
    #[test]
    fn the_pad_keeps_its_gap_from_the_part_within_the_top_gap() {
        let s = Settings::new(keys());
        assert_eq!(s.top_gap_layers, 1);
        let n = 3;
        let (hs, bed) = (heights(n), bed());
        let foot = ngon(Pt::from_mm(-5.0, 0.0), 1.0, 360);
        let flare = square(-2.8, -1.3, 2.8, 1.3);
        let slices = vec![vec![square(-2.5, -1.0, 2.5, 1.0)], vec![flare.clone()], vec![flare.clone()]];
        let roofs = Tips { tips: vec![Vec::new(); n], contact: vec![Vec::new(); n], interface: vec![Vec::new(); n], bed: Vec::new() };
        let input = Input { keys: keys(), print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let drawn = Drawn { base: vec![vec![foot], Vec::new(), Vec::new()], floor: vec![Vec::new(); n] };
        let v = Volumes::new(Ctl::default(), &s, &slices, &[]).unwrap();
        let out = layers(Ctl::default(), &s, &v, &input, drawn, &roofs, &mut Memo::default()).unwrap();
        assert!(inside(&out.pad, -3.1, 0.0) && !inside(&out.pad, -2.9, 0.0), "the pad stops short of the flare, not of the first layer");
        // Whether the pad meets the flare grown by `by` (mm), square.
        let meets = |by: f64| !intersection_ex(&out.pad, &offset(std::slice::from_ref(&flare), by * SCALE, Join::Square)).is_empty();
        assert!(!meets(0.199) && meets(0.201), "its gap from the flare, 0.2 mm");
    }

    /// §9.5 (Q7): each merge grows every ring by 10 nm on its own and keeps it, so a contact that
    /// takes the interface and then the floor has grown 20 nm and its hole shrunk 20 nm, the
    /// interface grown 20 nm and the floor 10 nm.
    #[test]
    fn each_merge_grows_every_ring_10_nm_and_keeps_it() {
        let mut hole = Polygon::rect(Pt::from_mm(4.0, 4.0), Pt::from_mm(6.0, 6.0));
        hole.reverse();
        let contact = vec![ExPolygon { contour: Polygon::rect(Pt::from_mm(0.0, 0.0), Pt::from_mm(10.0, 10.0)), holes: vec![hole] }];
        let mut one = merged(contact, vec![square(20.0, 0.0, 30.0, 10.0)], vec![square(40.0, 0.0, 50.0, 10.0)]);
        one.sort_by_key(|e| e.bbox().min.x);
        let boxes: Vec<(i64, i64, i64, i64)> = one.iter().map(|e| e.bbox()).map(|b| (b.min.x, b.min.y, b.max.x, b.max.y)).collect();
        assert_eq!(boxes, vec![(-20, -20, 10_000_020, 10_000_020), (19_999_980, -20, 30_000_020, 10_000_020), (39_999_990, -10, 50_000_010, 10_000_010)]);
        assert_eq!(one[0].holes.len(), 1, "the contact keeps its hole");
        let h = one[0].holes[0].bbox();
        assert_eq!((h.min.x, h.min.y, h.max.x, h.max.y), (4_000_020, 4_000_020, 5_999_980, 5_999_980), "shrunk by 20 nm");
    }

    /// §9.5 Q7: each merge's rings grow on their own and unite under the non-zero rule. A contact
    /// and the interface halo round it abut; grown, the contact and the halo's hole overlap by
    /// 20 nm, which the even-odd rule would leave as a slit. Under non-zero they are one island,
    /// with no hole, grown 10 nm.
    #[test]
    fn a_contact_and_its_halo_merge_into_one_island() {
        let mut inner = square(0.0, 0.0, 10.0, 10.0).contour;
        inner.reverse();
        let halo = ExPolygon { contour: square(-1.0, -1.0, 11.0, 11.0).contour, holes: vec![inner] };
        let one = merged(vec![square(0.0, 0.0, 10.0, 10.0)], vec![halo], Vec::new());
        assert_eq!(one.len(), 1, "one island");
        assert!(one[0].holes.is_empty(), "no slit between the contact and its halo");
        let b = one[0].bbox();
        assert_eq!((b.min.x, b.min.y, b.max.x, b.max.y), (-1_000_010, -1_000_010, 11_000_010, 11_000_010), "the halo's outline grown 10 nm");
    }

    /// §9.4 Q11: the drawing clips nothing to the bed, so the pad grows from the whole layer-0
    /// base, past the bed's edge, and is clipped once, after, as every layer's base is. A foot of
    /// radius 3 mm centred 2.9 mm past the edge shows only a 0.1 mm sliver on the bed: grown first,
    /// the pad reaches (−99.9, 3.5), 4.48 mm from the foot's centre; the sliver grown would stop
    /// near y = 2.77.
    #[test]
    fn a_foot_over_the_beds_edge_grows_its_pad_before_the_clip() {
        let s = Settings::new(keys());
        let n = 4;
        let (hs, bed) = (heights(n), bed());
        let foot = vec![ngon(Pt::from_mm(-102.9, 0.0), 3.0, 360)];
        let mut base = vec![Vec::new(); n];
        base[0] = foot.clone();
        let roofs = Tips { tips: vec![Vec::new(); n], contact: vec![Vec::new(); n], interface: vec![Vec::new(); n], bed: Vec::new() };
        let slices = vec![Vec::new(); n];
        let input = Input { keys: keys(), print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let v = Volumes::new(Ctl::default(), &s, &slices, &[]).unwrap();
        let out = layers(Ctl::default(), &s, &v, &input, Drawn { base, floor: vec![Vec::new(); n] }, &roofs, &mut Memo::default()).unwrap();
        let area = |set: &[ExPolygon]| set.iter().map(ExPolygon::area_mm2).sum::<f64>();
        let grown_first = intersection_ex(&pad(&foot, &[], PRINT.first_line, 0.2), &bed);
        let clipped_first = intersection_ex(&pad(&intersection_ex(&foot, &bed), &[], PRINT.first_line, 0.2), &bed);
        assert!((area(&out.pad) - area(&grown_first)).abs() < 1e-9, "the foot grown, then clipped: {} mm2", area(&out.pad));
        assert!(inside(&out.pad, -99.9, 3.5) && !inside(&clipped_first, -99.9, 3.5), "grown from the whole foot, not from its sliver on the bed");
        assert!(!inside(&out.pad, -100.1, 0.0), "and clipped at the bed's edge");
        assert!((area(&out.base[0]) - area(&intersection_ex(&foot, &bed))).abs() < 1e-9, "the base clipped to the bed itself");
    }

    /// §9.5: an interface region prints a loop half a flow spacing in (after its 10 nm growth) and
    /// lines a line further in, 0.877 mm apart on a grid through the part frame's origin: along X
    /// on even layers, along Y on odd ones, in the support line at the layer's height.
    #[test]
    fn interface_lines_turn_by_layer_on_one_grid() {
        let s = Settings::new(keys());
        let n = 3;
        let (hs, bed) = (heights(n), bed());
        // The region off the origin, so the grid's phase shows.
        let region = square(-0.3, -0.7, 9.7, 9.3);
        let slices = vec![Vec::new(); n];
        let input = Input { keys: keys(), print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let layers = Layers { base: vec![Vec::new(); n], interface: vec![Vec::new(), vec![region.clone()], vec![region]], pad: Vec::new() };
        let out = paths(Ctl::default(), &s, &input, &layers, &mut Memo::default()).unwrap();
        let step = (s.roof_line_spacing * SCALE).trunc() as i64;
        // The region's sides (nm), x then y.
        let (x0, x1, y0, y1) = (-300_000, 9_700_000, -700_000, 9_300_000);
        for l in [1, 2] {
            let ps = &out[l];
            assert!(ps.iter().all(|p| p.role == Role::SupportInterface && p.width == 0.42 && p.height == 0.2 && p.speed == 80.0));
            let loops: Vec<&ExtrusionPath> = ps.iter().filter(|p| p.closed).collect();
            assert_eq!(loops.len(), 1, "one loop");
            let off = 188_539.5 - 10.0;
            let on = |c: i64, a: i64, b: i64| (c as f64 - (a as f64 + off)).abs() <= 1.0 || (c as f64 - (b as f64 - off)).abs() <= 1.0;
            assert!(loops[0].pts.iter().all(|q| on(q.x, x0, x1) || on(q.y, y0, y1)), "the loop half a spacing in");
            let mut at: Vec<i64> = Vec::new();
            for p in ps.iter().filter(|p| !p.closed) {
                assert!(p.pts.iter().all(|q| q.x >= x0 + 608_000 && q.y >= y0 + 608_000 && q.x <= x1 - 608_000 && q.y <= y1 - 608_000), "the lines 0.608 mm in");
                for w in p.pts.windows(2) {
                    let (d, c, lo, hi) = if l % 2 == 0 { (w[0].y - w[1].y, w[0].y, y0, y1) } else { (w[0].x - w[1].x, w[0].x, x0, x1) };
                    // A run along the fill's own outline links lines; it is not one.
                    let rim = (c - (lo + 608_529)).abs() < 1_000 || (c - (hi - 608_529)).abs() < 1_000;
                    if d.abs() <= 2 && w[0].dist(w[1]) > SCALE && !rim {
                        at.push(c);
                    }
                }
            }
            assert!(at.len() > 8, "layer {l}: lines along {}", if l % 2 == 0 { "X" } else { "Y" });
            assert!(at.iter().all(|&c| c.rem_euclid(step) <= 2 || step - c.rem_euclid(step) <= 2), "layer {l}: on the grid through the origin, {step} nm apart: {at:?}");
        }
    }

    /// §8.7 Order, §9.5: every path is simplified once more at the resolution (0.012 mm) before
    /// the writer. A contact disc's loop (r_tip 0.4 mm, 45 corners, half a flow spacing in, so its
    /// corners 0.211 mm round) keeps 16 of its corners, run from its first point round to it
    /// again: 17 printed points, every chord 2 or 3 corners long, 0.0587 or 0.0878 mm, as the
    /// reference prints its disc loops.
    #[test]
    fn every_path_is_simplified_once_more_for_the_writer() {
        let s = Settings::new(keys());
        let n = 2;
        let (hs, bed) = (heights(n), bed());
        let slices = vec![Vec::new(); n];
        let input = Input { keys: keys(), print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let layers = Layers { base: vec![Vec::new(); n], interface: vec![Vec::new(), vec![roof_disc(Pt::from_mm(3.0, 2.0), s.r_tip)]], pad: Vec::new() };
        let out = paths(Ctl::default(), &s, &input, &layers, &mut Memo::default()).unwrap();
        assert_eq!(out[1].len(), 1, "the disc's loop, and no lines inside it");
        let ring = &out[1][0].pts;
        assert!(out[1][0].closed && out[1][0].loop_pts == *ring, "a loop, its ring the simplified one");
        assert_eq!(ring.len(), 16, "16 of the 45 corners");
        let chords: Vec<f64> = (0..ring.len()).map(|i| ring[i].dist(ring[(i + 1) % ring.len()]) / SCALE).collect();
        assert!(chords.iter().all(|c| (c - 0.0587).abs() < 3e-4 || (c - 0.0878).abs() < 3e-4), "chords of 2 or 3 corners: {chords:?}");
    }

    /// A layer made afresh takes the closing of what reaches it (§9.2) from the memo when the same
    /// rings reach it: here the memo's closings emptied, so a layer whose base changed gets no
    /// interface from the contact over it, as a fresh run does.
    #[test]
    fn a_closing_made_before_is_taken_from_the_memo() {
        let s = Settings::new(keys());
        let n = 12;
        let (hs, bed) = (heights(n), bed());
        let base: Vec<Vec<ExPolygon>> = (0..n).map(|l| vec![square(-10.0 + 0.1 * l as f64, -10.0, 10.0, 10.0)]).collect();
        let mut contact = vec![Vec::new(); n];
        contact[8] = vec![square(-1.0, -1.0, 1.0, 1.0)];
        let roofs = Tips { tips: vec![Vec::new(); n], contact, interface: vec![Vec::new(); n], bed: Vec::new() };
        let slices = vec![Vec::new(); n];
        let input = Input { keys: keys(), print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let v = Volumes::new(Ctl::default(), &s, &slices, &[]).unwrap();
        let run = |base: &[Vec<ExPolygon>], memo: &mut Memo| layers(Ctl::default(), &s, &v, &input, Drawn { base: base.to_vec(), floor: vec![Vec::new(); n] }, &roofs, memo).unwrap();
        let mut memo = Memo::default();
        run(&base, &mut memo);
        assert!(!memo.closed.is_empty(), "the layers the contact reaches keep their closing");
        for c in memo.closed.values_mut() {
            *c = Arc::new(Vec::new());
        }
        let mut changed = base.clone();
        changed[7] = vec![square(-5.0, -5.0, 5.0, 5.0)];
        let out = run(&changed, &mut memo);
        let fresh = run(&changed, &mut Memo::default());
        assert!(out.interface[7].is_empty() && !fresh.interface[7].is_empty(), "layer 7 took its closing from the memo");
    }

    /// A run on the memo of a run before takes each layer made from the same as the memo holds
    /// it, but the first layer's base and interface; a layer whose base changed is made afresh,
    /// paths and all. Each run keeps only its own layers, and another print starts afresh.
    #[test]
    fn a_layer_made_before_is_taken_from_the_memo() {
        let s = Settings::new(keys());
        let n = 12;
        let (hs, bed) = (heights(n), bed());
        let base: Vec<Vec<ExPolygon>> = (0..n).map(|l| vec![square(-10.0 + 0.1 * l as f64, -10.0, 10.0, 10.0)]).collect();
        let mut contact = vec![Vec::new(); n];
        contact[8] = vec![square(-1.0, -1.0, 1.0, 1.0)];
        let roofs = Tips { tips: vec![Vec::new(); n], contact, interface: vec![Vec::new(); n], bed: Vec::new() };
        let slices = vec![Vec::new(); n];
        let at = |print: Print| Input { keys: keys(), print, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let run = |input: &Input, base: &[Vec<ExPolygon>], memo: &mut Memo| {
            let v = Volumes::new(Ctl::default(), &s, &slices, &[]).unwrap();
            let out = layers(Ctl::default(), &s, &v, input, Drawn { base: base.to_vec(), floor: vec![Vec::new(); n] }, &roofs, memo).unwrap();
            let p = paths(Ctl::default(), &s, input, &out, memo).unwrap();
            (out, p)
        };
        let mut memo = Memo::default();
        let first = run(&at(PRINT), &base, &mut memo);
        assert_eq!((memo.layers.len(), memo.paths.len()), (n - 1, n), "a layer each, but the first's base and interface");
        // Every layer the memo holds marked, so what is taken from it shows: a square over the
        // first layer's base, standing on it as D15 asks.
        let mark = square(0.0, 0.0, 1.0, 1.0);
        for m in memo.layers.values_mut() {
            *m = Arc::new(Layer { base: Vec::new(), interface: vec![mark.clone()], held: m.held });
        }
        let mut changed = base.clone();
        changed[3] = vec![square(-5.0, -5.0, 5.0, 5.0)];
        let (out, _) = run(&at(PRINT), &changed, &mut memo);
        for l in 0..n {
            assert_eq!(out.interface[l] == vec![mark.clone()], l != 0 && l != 3, "layer {l}: taken from the memo unless made");
        }
        assert_eq!(format!("{:?}", out.base[0]), format!("{:?}", first.0.base[0]), "the first layer is made every time");
        assert_eq!(memo.layers.len(), n - 1, "only this run's layers are kept");
        // The paths: the first run's memo with every layer's paths emptied, and so none on air.
        let mut memo = Memo::default();
        run(&at(PRINT), &base, &mut memo);
        for p in memo.pieces.values_mut().chain(memo.paths.values_mut().map(|p| &mut p.1)) {
            *p = Arc::new(Vec::new());
        }
        memo.air.clear();
        let (out, p) = run(&at(PRINT), &changed, &mut memo);
        let fresh = run(&at(PRINT), &changed, &mut Memo::default());
        for (l, p) in p.iter().enumerate() {
            assert_eq!(p.is_empty(), l != 3, "layer {l}: its paths taken unless its base changed");
        }
        assert_eq!((format!("{:?}", out.interface[3]), format!("{:?}", p[3])), (format!("{:?}", fresh.0.interface[3]), format!("{:?}", fresh.1[3])), "the changed layer as a fresh run makes it");
        let (_, p) = run(&at(Print { speed: 120.0, ..PRINT }), &changed, &mut memo);
        assert!(p.iter().all(|p| !p.is_empty()), "another print starts afresh");
    }

    /// A run on another bed takes the layers made before, as the memo holds them, where the bed's
    /// box holds every layer the memo holds; on a bed whose edge cuts one it makes them afresh, as
    /// a fresh run on it does.
    #[test]
    fn a_bed_whose_box_holds_every_layer_takes_them_from_the_memo() {
        let s = Settings::new(keys());
        let n = 6;
        let hs = heights(n);
        let base: Vec<Vec<ExPolygon>> = (0..n).map(|_| vec![square(-10.0, -10.0, 10.0, 10.0)]).collect();
        let roofs = Tips { tips: vec![Vec::new(); n], contact: vec![Vec::new(); n], interface: vec![Vec::new(); n], bed: Vec::new() };
        let slices = vec![Vec::new(); n];
        let v = Volumes::new(Ctl::default(), &s, &slices, &[]).unwrap();
        let run = |bed: &[ExPolygon], memo: &mut Memo| {
            let input = Input { keys: keys(), print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed, dropped: &[] };
            let out = layers(Ctl::default(), &s, &v, &input, Drawn { base: base.clone(), floor: vec![Vec::new(); n] }, &roofs, memo).unwrap();
            let p = paths(Ctl::default(), &s, &input, &out, memo).unwrap();
            (out, p)
        };
        let mut memo = Memo::default();
        run(&bed(), &mut memo);
        // Every layer the memo holds marked, so what is taken from it shows (standing on the
        // first layer's base, as D15 asks).
        let mark = square(0.0, 0.0, 1.0, 1.0);
        for m in memo.layers.values_mut() {
            *m = Arc::new(Layer { base: Vec::new(), interface: vec![mark.clone()], held: m.held });
        }
        let (out, _) = run(&[square(-60.0, -40.0, 90.0, 50.0)], &mut memo);
        assert!((1..n).all(|l| out.interface[l] == vec![mark.clone()]), "a bed whose box holds every layer: the layers are the memo's");
        let edge = [square(0.0, -100.0, 100.0, 100.0)];
        let cut = run(&edge, &mut memo);
        let fresh = run(&edge, &mut Memo::default());
        assert_eq!(format!("{cut:?}"), format!("{fresh:?}"), "a bed whose edge cuts a layer: as a fresh run on it");
        assert!(cut.0.base[2].iter().all(|e| e.bbox().min.x >= 0), "cut to the bed");
    }

    /// §9.4 (§1.6): every set the layers put on the bed goes on as the engine's intersection with
    /// the bed hands it on, every ring by Rule A and the islands in their order, a set inside the
    /// bed's box too: a floor of two overlapping pieces on a layer with nothing else (which the
    /// merge hands on as it comes) goes on as one island, as the intersection makes it. So on
    /// another bed whose box holds them, fresh, the layers and their lines are the same ring for
    /// ring: what lets the memo serve that bed. Over a base with a top contact and a floor.
    #[test]
    fn the_layers_go_on_the_bed_as_its_intersection_hands_them_on() {
        let s = Settings::new(keys());
        let n = 12;
        let hs = heights(n);
        let (big, small) = (square(-10.0, -10.0, 10.0, 10.0), square(-1.0, -1.0, 1.0, 1.0));
        let mut base: Vec<Vec<ExPolygon>> = (0..=10).map(|_| vec![big.clone()]).collect();
        base.push(Vec::new());
        base[8] = difference_ex(&base[8], std::slice::from_ref(&small));
        let mut floor = vec![Vec::new(); n];
        floor[2] = vec![square(3.0, 3.0, 6.0, 6.0), square(5.0, 5.0, 8.0, 8.0)];
        base[2] = difference_ex(&base[2], &floor[2]);
        let mut contact = vec![Vec::new(); n];
        contact[8] = vec![small.clone()];
        let roofs = Tips { tips: vec![Vec::new(); n], contact, interface: vec![Vec::new(); n], bed: Vec::new() };
        let slices = vec![Vec::new(); n];
        let v = Volumes::new(Ctl::default(), &s, &slices, &[]).unwrap();
        let run = |bed: &[ExPolygon]| {
            let input = Input { keys: keys(), print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed, dropped: &[] };
            let mut memo = Memo::default();
            let out = layers(Ctl::default(), &s, &v, &input, Drawn { base: base.clone(), floor: floor.clone() }, &roofs, &mut memo).unwrap();
            let p = paths(Ctl::default(), &s, &input, &out, &mut memo).unwrap();
            (out, p)
        };
        let (out, p) = run(&bed());
        let floored = intersection_ex(&floor[2], &bed());
        assert_eq!(floored.len(), 1, "the intersection makes the overlapping pieces one island");
        assert_eq!(format!("{:?}", out.interface[2]), format!("{floored:?}"), "the floor's layer goes on as the intersection hands it on");
        let moved = run(&[square(-60.0, -40.0, 90.0, 50.0)]);
        assert_eq!(format!("{:?}", (&out, &p)), format!("{moved:?}"), "on another bed whose box holds them, the same ring for ring");
    }

    /// A layer printed again takes from the memo each island it printed before: of a layer whose
    /// base changed in one of its two islands, the other's paths are as the memo holds them, and
    /// the changed island prints as on a layer of its own. Each run keeps the pieces of all its
    /// layers, taken or made.
    #[test]
    fn an_island_printed_before_is_taken_from_the_memo() {
        let s = Settings::new(keys());
        let n = 4;
        let (hs, bed) = (heights(n), bed());
        let (a, b) = (square(-10.0, -10.0, -5.0, -5.0), square(5.0, 5.0, 10.0, 10.0));
        let base: Vec<Vec<ExPolygon>> = (0..n).map(|_| vec![a.clone(), b.clone()]).collect();
        let roofs = Tips { tips: vec![Vec::new(); n], contact: vec![Vec::new(); n], interface: vec![Vec::new(); n], bed: Vec::new() };
        let slices = vec![Vec::new(); n];
        let input = Input { keys: keys(), print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
        let run = |base: &[Vec<ExPolygon>], memo: &mut Memo| {
            let v = Volumes::new(Ctl::default(), &s, &slices, &[]).unwrap();
            let out = layers(Ctl::default(), &s, &v, &input, Drawn { base: base.to_vec(), floor: vec![Vec::new(); n] }, &roofs, memo).unwrap();
            paths(Ctl::default(), &s, &input, &out, memo).unwrap()
        };
        let mut memo = Memo::default();
        let first = run(&base, &mut memo);
        assert!(!first[2].is_empty() && memo.pieces.len() == 3, "the pad, and each island once: the same on layers 1 to 3");
        for p in memo.pieces.values_mut().chain(memo.paths.values_mut().map(|p| &mut p.1)) {
            *p = Arc::new(Vec::new());
        }
        let mut changed = base.clone();
        changed[2][1] = square(6.0, 6.0, 9.0, 9.0);
        let p = run(&changed, &mut memo);
        let alone = run(&[base[..2].to_vec(), vec![vec![changed[2][1].clone()]], base[3..].to_vec()].concat(), &mut Memo::default());
        assert!(p[1].is_empty() && p[3].is_empty(), "the layers as they were, from the memo");
        assert_eq!(format!("{:?}", p[2]), format!("{:?}", alone[2]), "the island as it was from the memo, the changed one made");
        assert_eq!(memo.pieces.len(), 4, "the pieces of every layer, the changed island's too");
    }

    /// D15: on a part's first layer with a hole, the base on the bed round it; a disc of interface
    /// on layer 1 over the hole and one over the base, and on layer 2 a square on each, a floor on
    /// the part and, on another foot of the part, a piece a layer short of the bottom gap's reach
    /// over it. What stands on the air over the hole goes, and what stood only on it; the rest
    /// stays, and on the part only when the support may stand on it.
    #[test]
    fn what_has_nothing_under_it_does_not_print() {
        let n = 4;
        let (hs, bed) = (heights(n), bed());
        let mut slices = vec![Vec::new(); n];
        slices[0] = vec![square(10.0, -1.0, 12.0, 1.0), square(20.0, -1.0, 22.0, 1.0)];
        let mut base = vec![Vec::new(); n];
        base[0] = difference_ex(&[square(-5.0, -5.0, 5.0, 5.0)], &[square(-2.0, -2.0, 2.0, 2.0)]);
        base[2] = vec![square(-0.4, -0.4, 0.4, 0.4), square(3.2, -0.4, 3.8, 0.4)];
        base[3] = vec![square(20.5, -0.5, 21.5, 0.5)];
        let mut interface = vec![Vec::new(); n];
        interface[1] = vec![square(-0.5, -0.5, 0.5, 0.5), square(3.0, -0.5, 4.0, 0.5)];
        interface[2] = vec![square(10.5, -0.5, 11.5, 0.5)];
        for bed_only in [false, true] {
            let k = Keys { bed_only, ..keys() };
            let s = Settings::new(k.clone());
            assert_eq!(s.bottom_gap_layers, 1);
            let input = Input { keys: k, print: PRINT, slices: &slices, heights: &hs, enforce: &[], block: &[], keep_out: &[], bed: &bed, dropped: &[] };
            let (b, i) = standing(Ctl::default(), &s, &input, &[], (base.clone(), interface.clone()), &vec![None; n], &mut HashMap::new()).unwrap();
            assert!(!inside(&i[1], 0.0, 0.0) && inside(&i[1], 3.5, 0.0), "bed only {bed_only}: layer 1 keeps only what stands on the support");
            assert!(!inside(&b[2], 0.0, 0.0) && inside(&b[2], 3.5, 0.0), "bed only {bed_only}: layer 2 loses what stood on what went");
            assert_eq!(inside(&i[2], 11.0, 0.0), !bed_only, "bed only {bed_only}: the floor on the part");
            assert_eq!(inside(&b[3], 21.0, 0.0), !bed_only, "bed only {bed_only}: the piece in reach over the part");
            assert!(inside(&b[0], 3.0, 3.0) && !inside(&b[0], 0.0, 0.0), "the first layer is on the bed, as it was");
        }
    }
}

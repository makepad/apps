//! The rules a support keeps on every layer, in one place. The generator keeps them as it builds
//! each layer; [`check`] proves it over the layers it made, and Printability tells any break with
//! its layer.
//!
//! - R1, no thin air: every part of a layer lies over what prints under it (the support, or the
//!   part where the support may stand on it), or within the steep angle's reach of it. A gap in
//!   it narrower than [`BRIDGE`] lines is bridged.
//! - R2, gradual change: an outline grows by at most that reach a layer, and shrinks freely; a
//!   cut by the part's gap and its release both ramp. A roof's bottom layer bridges between the
//!   tips under it, across gaps up to [`ROOF_SPAN`].
//! - R3, footing: support starts on the bed, or (not from the bed only) on the part, never in the
//!   air.
//! - R4, purpose: every top of the support carries part underside one top gap over it (or a layer
//!   more), within half the tip's gap aside of it as under a slope, and prints flat and solid
//!   there (roof or solid fill); what it keeps of a top lies within the tip's gap and a line. A
//!   pocket in the layer over it narrower than [`BRIDGE`] lines is bridged, not a top.
//! - R5, clearance: nothing nearer the part than xy_min ([`SupportParams::xy_min`]) on every
//!   layer, the side gap past it only preferred, where a branch has the room; nothing in the top
//!   gap under the part or the bottom gap over it, by actual z (see [`Legal`]).
//! - R6, printable width: nothing narrower than two lines, and no roof (the interface) narrower
//!   than one, the width it prints at, counted with what it lies on: a sliver on the bed, or on
//!   the part where the support may stand on it, is held there. It judges what prints: a piece a
//!   line prints in is told, and one no line prints in is not there, so the other rules judge the
//!   layer without it and what it would hold is held by what prints, or told.
//! - R7, fusing: where supports fuse or split, R1 holds for the union, which is what a layer is.
//! - C5, bead overlap: neighbouring paths and returning stretches of the same path may touch
//!   at one bead's width. Only overlap beyond [`BEAD_OVERLAP_TOL`] is a conflict.
//!
//! The hard checks ([`hard`]) tell each failure with its place: a top of the support that holds
//! nothing (H1), a top that stops short of the top gap under what it holds or rises into it
//! (H2, both ways), and demand a support could get to that none holds (H3).

use crate::par::{Cancelled, Ctl};
use crate::coral::material::{material, Role as PathRole, SupportPath};
use crate::coral::reach::Reach;
use crate::support::{SupportLayer, SupportParams};
use makepad_fab_slicer::clip::{difference_ex, intersection_ex, union_ex};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::grid::SegGrid;
use makepad_fab_slicer::offset::{miter_reach, offset_ex, round_inset_argued_empty, Join};

/// One of the rules (see the module).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rule {
    ThinAir,
    Growth,
    Footing,
    Purpose,
    Clearance,
    Width,
}

impl Rule {
    pub const ALL: [Rule; 6] = [Rule::ThinAir, Rule::Growth, Rule::Footing, Rule::Purpose, Rule::Clearance, Rule::Width];

    /// "R1" .. "R6".
    pub fn code(self) -> &'static str {
        match self {
            Rule::ThinAir => "R1",
            Rule::Growth => "R2",
            Rule::Footing => "R3",
            Rule::Purpose => "R4",
            Rule::Clearance => "R5",
            Rule::Width => "R6",
        }
    }

    /// What breaking it means, for a person.
    pub fn told(self) -> &'static str {
        match self {
            Rule::ThinAir => "support prints on thin air",
            Rule::Growth => "support grows past the steep angle",
            Rule::Footing => "support starts in the air",
            Rule::Purpose => "support top holds nothing, or is not flat and solid",
            Rule::Clearance => "support within the gap of the part",
            Rule::Width => "support too thin to print",
        }
    }
}

/// A rule broken on a layer, by an island of the support: how, how much (mm2) and where (its
/// middle and its box, in the plan's frame).
#[derive(Clone, Debug, PartialEq)]
pub struct Break {
    pub rule: Rule,
    /// Which part of the rule, for a person.
    pub why: &'static str,
    pub layer: usize,
    pub area_mm2: f64,
    pub at: (f64, f64),
    pub bounds: BBox,
}

/// Less than this (mm2) of an island is the outline's own rounding, not a break.
pub const SLACK: f64 = 0.05;

/// A gap or notch in what prints under a layer narrower than this many lines is bridged: a line
/// laid across it is held at both ends.
pub const BRIDGE: f64 = 4.0;

/// How far (mm) a roof's bottom layer may span between the tips that hold it, as a bridge (R2):
/// the reference's grid tree spaces the tips under a roof max(5 mm, half the longest bridge)
/// apart, and its roof bridges between them. The span closes gaps the tips surround; a strip
/// held only at its two ends still counts as unheld between them, as a line laid across two
/// line ends does under [`BRIDGE`].
pub const ROOF_SPAN: f64 = 5.0;

/// What a layer may print over (R1, R2): `under`, what prints on the layer below (with the part,
/// where the support may stand on it), its gaps narrower than [`BRIDGE`] lines of width `w`
/// bridged, grown by `reach` (mm).
pub fn carried(under: &[ExPolygon], reach: f64, w: f64) -> Vec<ExPolygon> {
    spanned(under, reach, BRIDGE * w)
}

/// `under` with its gaps narrower than `span` (mm) bridged, grown by `reach` (mm).
fn spanned(under: &[ExPolygon], reach: f64, span: f64) -> Vec<ExPolygon> {
    let round = Join::Round(0.01 * SCALE);
    offset_ex(&offset_ex(under, (span / 2.0 + reach) * SCALE, round), -span / 2.0 * SCALE, round)
}

/// What of `set` is printable (R6): two lines of width `w` wide, counted with `rest`, what it lies
/// on (the part just under it, where the support may stand on it). What it keeps is a little wider
/// than [`check`] asks, so the outline's rounding never makes it a break.
pub fn wide(set: &[ExPolygon], rest: &[ExPolygon], w: f64) -> Vec<ExPolygon> {
    opened(set, rest, w, 0.01)
}

/// What of `set`, with `rest` near it, is two lines of width `w` less `slack` (mm) wide, but no
/// further than `set`.
fn opened(set: &[ExPolygon], rest: &[ExPolygon], w: f64, slack: f64) -> Vec<ExPolygon> {
    if set.is_empty() {
        return Vec::new();
    }
    let (r, round) = ((w - slack) * SCALE, Join::Round(0.01 * SCALE));
    let near: Vec<ExPolygon> = if rest.is_empty() { Vec::new() } else { intersection_ex(rest, &offset_ex(set, 2.0 * w * SCALE, Join::Miter(3.0))) };
    let all = if near.is_empty() { set.to_vec() } else { union_ex(&[set.to_vec(), near].concat()) };
    intersection_ex(set, &offset_ex(&offset_ex(&all, -r, round), r, round))
}

/// What of the part's layers `of` comes within `by` (mm) of `set`: the part a rule looks at near
/// the support, cut out before it is grown or joined, so the part's far side costs nothing.
pub fn near_to<'a>(of: impl Iterator<Item = &'a [ExPolygon]>, set: &[ExPolygon], by: f64) -> Vec<ExPolygon> {
    if set.is_empty() {
        return Vec::new();
    }
    let window = offset_ex(set, by * SCALE, Join::Miter(3.0));
    let bb = bbox_of(&window);
    let mut out = Vec::new();
    for layer in of {
        let close: Vec<ExPolygon> = layer.iter().filter(|e| e.bbox().overlaps(&bb)).cloned().collect();
        if !close.is_empty() {
            out.extend(intersection_ex(&close, &window));
        }
    }
    if out.len() > 1 { union_ex(&out) } else { out }
}

/// The part a top of the support on layer `l`, near `of`, holds up (R4): one top gap (`gap_top`
/// layers) over it, or a layer more.
pub fn part_over(slices: &[Vec<ExPolygon>], l: usize, of: &[ExPolygon], gap_top: usize, w: f64) -> Vec<ExPolygon> {
    let n = slices.len();
    if l + 1 >= n {
        return Vec::new();
    }
    near_to((l + 1..=(l + gap_top + 1 + crate::support::TOUCH_SLACK).min(n - 1)).map(|k| slices[k].as_slice()), of, crate::support::TIP_GAP + 3.0 * w)
}

/// How far aside of the part over it (mm) a top of the support still holds it (R4): half the
/// tip's gap. A top under a slope, which comes out over it, does; one beside a wall, the whole
/// tip's gap off it, does not.
pub const ASIDE: f64 = crate::support::TIP_GAP / 2.0;

/// Whether an island of the support that carries nothing holds `over`, the part over it, up (R4):
/// it comes within [`ASIDE`] of it.
pub fn reaches(island: &ExPolygon, over: &[ExPolygon]) -> bool {
    if over.is_empty() {
        return false;
    }
    let grown = offset_ex(std::slice::from_ref(island), ASIDE * SCALE, Join::Miter(3.0));
    let bb = bbox_of(&grown);
    let close: Vec<ExPolygon> = over.iter().filter(|e| e.bbox().overlaps(&bb)).cloned().collect();
    !close.is_empty() && total_area_mm2(&intersection_ex(&grown, &close)) > 1e-3
}

/// What `up`, a layer of the support, covers of the one under it (R4): itself, its pockets
/// narrower than [`BRIDGE`] lines of width `w` bridged.
pub fn covers(up: &[ExPolygon], w: f64) -> Vec<ExPolygon> {
    if up.is_empty() { Vec::new() } else { carried(up, 0.0, w) }
}

/// What of `top` lies a line of width `w` inside it: what a line round it leaves, rounded at its
/// inner corners as a line laid round them is, so less of it is less of this.
pub fn less_a_line(top: &[ExPolygon], w: f64) -> Vec<ExPolygon> {
    if top.is_empty() { Vec::new() } else { offset_ex(top, -w * SCALE, Join::Round(0.02 * SCALE)) }
}

/// The pieces of `set` that may keep something a line inside them, by
/// [`round_inset_argued_empty`]'s argument.
fn line_wide(set: Vec<ExPolygon>, w: f64) -> Vec<ExPolygon> {
    set.into_iter().filter(|e| !round_inset_argued_empty(e, w * SCALE, 0.02 * SCALE)).collect()
}

/// Where a top of the support less a line may lie (R4): within the tip's gap and a line of
/// `over`, the part it holds up.
pub fn holds_band(over: &[ExPolygon], w: f64) -> Vec<ExPolygon> {
    if over.is_empty() { Vec::new() } else { offset_ex(over, (crate::support::TIP_GAP + w) * SCALE, Join::Miter(3.0)) }
}

/// Whether a line of `paths` prints in `piece` (R6): its centreline comes into it, across its
/// edge or inside it. Exact.
pub fn prints_in(piece: &ExPolygon, paths: &[SupportPath]) -> bool {
    let bb = piece.bbox();
    let crosses = |a: Pt, b: Pt| piece.rings().any(|r| (0..r.pts.len()).any(|i| meet(a, b, r.pts[i], r.pts[(i + 1) % r.pts.len()])));
    paths.iter().flat_map(SupportPath::segments).any(|(a, b)| BBox::of(&[a, b]).overlaps(&bb) && (piece.contains(a) || crosses(a, b)))
}

/// What breaks the rules in `layers`, the support for `legal`'s part, `prints(l, piece)` telling
/// whether a line prints in a piece of layer `l` too thin for its lines (R6). `ctl`'s tick is told
/// per layer checked; its cancel stops a layer between its rules, and a check it stopped keeps
/// nothing.
pub fn check(ctl: Ctl, legal: &Legal, layers: &[SupportLayer], prints: &(dyn Fn(usize, &ExPolygon) -> bool + Sync)) -> Result<Vec<Break>, Cancelled> {
    crate::prof::scope_fanned(crate::prof::Stage::SupportRules, || check_layers(ctl, legal, layers, prints))
}

fn check_layers(ctl: Ctl, legal: &Legal, layers: &[SupportLayer], prints: &(dyn Fn(usize, &ExPolygon) -> bool + Sync)) -> Result<Vec<Break>, Cancelled> {
    let (slices, heights, p) = (legal.slices, legal.heights, legal.p);
    let n = layers.len().min(slices.len()).min(heights.len());
    let h = heights.get(1).map_or(0.2, |x| x.1).max(1e-6);
    let gap_top = crate::support::gap_layers(p.top_gap, h);
    let gap_bottom = crate::support::gap_layers(p.bottom_gap, h);
    let land = gap_bottom + 1 + (crate::support::LAND_AIR / h).ceil() as usize;
    let w = p.line_width;
    let all = |l: usize| -> Vec<ExPolygon> { union_ex(&[layers[l].base.clone(), layers[l].interface.clone()].concat()) };
    let support = crate::par::try_map(Ctl { tick: None, ..ctl }, n, all)?;
    let lean = |l: usize| heights[l].1 * p.angle_max_deg.to_radians().tan();
    let layers_of = |range: std::ops::RangeInclusive<usize>| range.map(|k| slices[k].as_slice());
    // Per layer, made once, by the first layer that asks: the part near it that it may stand on
    // (R1, R2, R3, R6); R6, two lines wide everywhere but on the bed and a roof one line, the
    // width it prints at, told where a line prints in it; and what of it prints, less the pieces
    // no line prints in (None when that is all of it), which the other rules judge.
    type Width = (Vec<ExPolygon>, [Vec<ExPolygon>; 2], Option<Vec<ExPolygon>>);
    let widths: Vec<std::sync::OnceLock<Width>> = (0..n).map(|_| std::sync::OnceLock::new()).collect();
    let width = |l: usize| {
        widths[l].get_or_init(|| {
            let here = &support[l];
            let mut out: Width = Default::default();
            if here.is_empty() || ctl.cancelled() {
                return out;
            }
            if !p.build_plate_only && l > 0 {
                out.0 = near_to(layers_of(l - land.min(l)..=l - 1), here, lean(l) + BRIDGE * w + 0.5);
            }
            if l == 0 {
                return out;
            }
            let part = &out.0;
            let unslivered = |set: &[ExPolygon]| offset_ex(&offset_ex(set, -0.02 * SCALE, Join::Miter(3.0)), 0.02 * SCALE, Join::Miter(3.0));
            let thin = difference_ex(here, &opened(here, part, w, 0.02));
            let kept = unslivered(&thin);
            // Body and roof_thin lie in thin, and unslivered keeps inclusion; a 0.02 mm
            // opening's own stray pieces are far under SLACK / 2, so none can be told or gone.
            if !kept.iter().any(|e| e.area_mm2() >= SLACK / 2.0) {
                return out;
            }
            let roof = &layers[l].interface;
            // Without a roof the body is thin itself, already unslivered.
            let (body, roof_thin) = if thin.is_empty() || roof.is_empty() {
                (kept, Vec::new())
            } else {
                let in_roof = intersection_ex(&thin, roof);
                let under_a_line = if in_roof.is_empty() { in_roof } else { difference_ex(&in_roof, &opened(here, part, w / 2.0, 0.01)) };
                (unslivered(&difference_ex(&thin, roof)), unslivered(&under_a_line))
            };
            let mut gone = Vec::new();
            for (told, set) in out.1.iter_mut().zip([body, roof_thin]) {
                for e in set.into_iter().filter(|e| e.area_mm2() >= SLACK) {
                    if prints(l, &e) { told.push(e) } else { gone.push(e) }
                }
            }
            if !gone.is_empty() {
                out.2 = Some(difference_ex(here, &gone));
            }
            out
        })
    };
    let printing = |l: usize| width(l).2.as_deref().unwrap_or(support[l].as_slice());
    let found = crate::par::try_map(ctl, n, |l| {
        let mut out = Vec::new();
        if support[l].is_empty() {
            return out;
        }
        let (part, thin, _) = width(l);
        let here = printing(l);
        let mut tell = |rule: Rule, why: &'static str, set: &[ExPolygon]| {
            for e in set {
                let a = e.area_mm2();
                if a >= SLACK {
                    let bounds = e.bbox();
                    let c = bounds.center();
                    out.push(Break { rule, why, layer: l, area_mm2: a, at: (c.x_mm(), c.y_mm()), bounds });
                }
            }
        };
        let reach = lean(l);
        // A cancel stops a layer between its rules, so the next revision has the cores sooner.
        if ctl.cancelled() {
            return out;
        }
        // R1, R2, R3: what prints under it.
        if l > 0 {
            let below = printing(l - 1);
            let under = if part.is_empty() { below.to_vec() } else { union_ex(&[below, part.as_slice()].concat()) };
            let held = carried(&under, reach + 0.01, w);
            // A roof's bottom, the roof over no roof, bridges between the tips under it.
            let bottom = if layers[l].interface.is_empty() { Vec::new() } else { difference_ex(&layers[l].interface, &layers[l - 1].interface) };
            enum Entry<'a> {
                Footing(&'a ExPolygon),
                Past(Vec<ExPolygon>, bool),
            }
            let mut entries = Vec::with_capacity(here.len());
            for island in here {
                let one = std::slice::from_ref(island);
                let on_support = !intersection_ex(one, below).is_empty();
                if !on_support && (part.is_empty() || intersection_ex(one, part).is_empty()) {
                    entries.push(Entry::Footing(island));
                    continue;
                }
                entries.push(Entry::Past(difference_ex(one, &held), on_support));
            }
            // What of it the tips under it bridge, made only when an island grows past what
            // holds it, which few do. spanned closes with round joins: grow span / 2 + reach,
            // then shrink span / 2, so a point reads under within span + reach. Round joins
            // reach no further than their depth, within their 0.01 mm tolerance. An under
            // region whose box lies farther from every asker's box changes nothing it reads;
            // the booleans' snap rounding is local (see clip::near). Keep whole regions.
            let bridged = if bottom.is_empty() {
                Vec::new()
            } else {
                let by = ROOF_SPAN + reach + 0.01 + 0.05;
                let boxes: Vec<BBox> = entries.iter().flat_map(|entry| match entry {
                    Entry::Footing(_) => &[][..],
                    Entry::Past(past, _) => past.as_slice(),
                }).map(|e| widen(&e.bbox(), by)).collect();
                if boxes.is_empty() {
                    Vec::new()
                } else {
                    let near_under: Vec<ExPolygon> = under.iter().filter(|e| {
                        let bb = e.bbox();
                        boxes.iter().any(|b| bb.overlaps(b))
                    }).cloned().collect();
                    intersection_ex(&bottom, &spanned(&near_under, reach + 0.01, ROOF_SPAN))
                }
            };
            for entry in entries {
                match entry {
                    Entry::Footing(island) => tell(Rule::Footing, "nothing under it but air", std::slice::from_ref(island)),
                    Entry::Past(mut past, on_support) => {
                        if !past.is_empty() && !bottom.is_empty() && !bridged.is_empty() {
                            past = difference_ex(&past, &bridged);
                        }
                        if on_support {
                            tell(Rule::Growth, "past the steep angle of the layer under it", &past);
                        } else {
                            tell(Rule::ThinAir, "past the steep angle of the part it stands on", &past);
                        }
                    }
                }
            }
        }
        if ctl.cancelled() {
            return out;
        }
        // R4: every top carries part underside, one top gap over it, and prints solid there.
        if l > 0 && l + 1 < n {
            let up = printing(l + 1);
            let mut on_up = Vec::with_capacity(here.len());
            let mut island_boxes = Vec::new();
            for island in here {
                let one = std::slice::from_ref(island);
                let bb = island.bbox();
                let on = up.iter().any(|e| e.bbox().overlaps(&bb)) && !intersection_ex(one, up).is_empty();
                on_up.push(on);
                if !on {
                    island_boxes.push(widen(&bb, miter_reach(island, ASIDE * SCALE, 3.0) / SCALE + 0.05));
                }
            }
            // The part over it, made only when a reader asks, from a source made once then.
            // reaches reads over within the asking island's ASIDE miter offset; holds_band
            // reads it within 3 * (TIP_GAP + w) of a top (its unknown corners' miter limit).
            // near_to grows each source piece by d with miters: miter_reach bounds its window.
            // The source left out lies farther from every reader than it reads, with 0.05 mm
            // for rounding. Whole source regions are kept or dropped, never clipped.
            let src = std::cell::OnceCell::new();
            let over_near = |boxes: &[BBox]| {
                let d = crate::support::TIP_GAP + 3.0 * w;
                let near: Vec<ExPolygon> = src.get_or_init(|| difference_ex(here, up)).iter().filter(|e| {
                    let bb = widen(&e.bbox(), miter_reach(e, d * SCALE, 3.0) / SCALE);
                    boxes.iter().any(|b| bb.overlaps(b))
                }).cloned().collect();
                part_over(slices, l, &near, gap_top, w)
            };
            let over_islands = std::cell::OnceCell::new();
            let over_islands = || over_islands.get_or_init(|| over_near(&island_boxes));
            let mut holds = Vec::new();
            for (island, on) in here.iter().zip(on_up) {
                let one = std::slice::from_ref(island);
                if on || reaches(island, over_islands()) {
                    holds.push(island.clone());
                } else {
                    tell(Rule::Purpose, "an island that holds nothing", one);
                }
            }
            // A top less a line is first looked for under `up` itself, which most layers have none of.
            let mut top = less_a_line(&line_wide(if up.is_empty() { holds.clone() } else { difference_ex(&holds, up) }, w), w);
            if !top.is_empty() && !up.is_empty() {
                top = less_a_line(&line_wide(difference_ex(&holds, &covers(up, w)), w), w);
            }
            if !top.is_empty() {
                let boxes: Vec<BBox> = top.iter().map(|e| widen(&e.bbox(), 3.0 * (crate::support::TIP_GAP + w) + 0.05)).collect();
                let over_tops = over_near(&boxes);
                let band = holds_band(&over_tops, w);
                tell(Rule::Purpose, "a top past the part over it", &difference_ex(&top, &band));
                let held = intersection_ex(&top, &band);
                if !held.is_empty() {
                    let mut flat = layers[l].interface.clone();
                    flat.extend(layers[l].solid.iter().cloned());
                    let flat = offset_ex(&union_ex(&flat), w * SCALE, Join::Miter(3.0));
                    tell(Rule::Purpose, "a top that is not solid", &difference_ex(&held, &flat));
                }
            }
        }
        if ctl.cancelled() {
            return out;
        }
        // R5: nothing in what keeps support off (see [`Legal`]), each place told once, by the
        // first of it that it is in.
        let mut left = here.to_vec();
        for (why, _, set) in legal.sets(l, Some(&widen(&bbox_of(here), legal.margin()))) {
            if left.is_empty() {
                break;
            }
            let hit = intersection_ex(&left, &set);
            if hit.iter().any(|e| e.area_mm2() >= SLACK) {
                tell(Rule::Clearance, why, &hit);
                left = difference_ex(&left, &set);
            }
        }
        if ctl.cancelled() {
            return out;
        }
        // R6, judged with the layer's part as it was made (see `width`).
        tell(Rule::Width, "narrower than two lines", &thin[0]);
        tell(Rule::Width, "a roof narrower than a line", &thin[1]);
        out
    })?;
    // Every layer started, but one a cancel stopped found only some of its breaks.
    if ctl.cancelled() {
        return Err(Cancelled);
    }
    Ok(found.into_iter().flatten().collect())
}

/// How many breaks of each rule, in [`Rule::ALL`] order.
pub fn counts(breaks: &[Break]) -> [usize; 6] {
    let mut c = [0; 6];
    for b in breaks {
        c[Rule::ALL.iter().position(|&r| r == b.rule).unwrap_or(0)] += 1;
    }
    c
}

// The contracts on material (DESIGN §1). What is above is told of regions, and goes with the tree
// it checks; what follows is told of paths alone, the support's material, by exact predicates on
// Fab's integer points and bounds refined until they are certain. It lists; it never cuts.

/// C5 arithmetic and rounding slack (mm), about 4% of a 0.45 mm bead, not a design allowance.
pub const BEAD_OVERLAP_TOL: f64 = 0.02;
/// r_min (mm): the tightest a path may bend and still lie by itself as it bends (C5).
pub const R_MIN: f64 = 0.5;
/// g_c (mm): the widest free gap a contact leaves under the part, which bridges it.
pub const CONTACT_GAP: f64 = 1.0;
/// Less than this (mm) past a bound is arithmetic, not a fault.
const TOL: f64 = 1e-4;
/// Two heights within this (mm) are one.
const Z_TOL: f64 = 1e-6;
/// How finely (mm) a path is looked along where a look decides.
const STEP: f64 = 0.02;
/// How far past what a layer's paths span (mm), with xy_min, what keeps them off is looked for:
/// more than a bead and a little, so nothing cut at its edge is near them.
const MARGIN: f64 = 2.0;
/// How far an offset's chords lie inside its arcs at most (units): what keeps a bead off only
/// ever comes out this much smaller.
const ROUND: f64 = 1000.0;

/// One of the contracts the material keeps (see DESIGN §1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Contract {
    /// C2, fixed R1: every point of a layer's paths lies within the layer's lean of the
    /// centrelines under it, or of the part it stands on.
    Carried,
    /// C7: layer 0 lies where it may on the bed, and nothing else starts in the air.
    Footing,
    /// C6: every end of the support with nothing over it is a contact, or a crotch of a fork.
    Purpose,
    /// C4: every bead lies where material may be: xy_min off the part, out of the Z gaps, paint
    /// blockers and ring walls' ways (see [`Legal`]).
    Legal,
    /// C5: beads of a layer that are not one bead may touch, but must not overlap by more
    /// than [`BEAD_OVERLAP_TOL`], including returning stretches of the same path.
    Clear,
}

impl Contract {
    pub const ALL: [Contract; 5] = [Contract::Carried, Contract::Footing, Contract::Purpose, Contract::Legal, Contract::Clear];

    /// The rule each tells of: "R1", "R3", "R4", "R5", and "C5" for the clearance between beads.
    pub fn code(self) -> &'static str {
        match self {
            Contract::Carried => "R1",
            Contract::Footing => "R3",
            Contract::Purpose => "R4",
            Contract::Legal => "R5",
            Contract::Clear => "C5",
        }
    }

    /// What breaking it means, for a person.
    pub fn told(self) -> &'static str {
        match self {
            Contract::Carried => "support prints on thin air",
            Contract::Footing => "support has no footing",
            Contract::Purpose => "support ends holding nothing",
            Contract::Legal => "support where support may not be",
            Contract::Clear => "support lines fuse",
        }
    }
}

/// A contract broken on a layer by a path, or by two for a conflict.
#[derive(Clone, Debug, PartialEq)]
pub struct Fault {
    pub contract: Contract,
    /// Which part of it, for a person.
    pub why: &'static str,
    pub layer: usize,
    /// The path, an index into its layer's paths.
    pub path: usize,
    /// The other path of a conflict (the path itself where it comes back by itself).
    pub other: Option<usize>,
    /// Where its worst is (mm, in the plan's frame).
    pub at: (f64, f64),
    /// Its raw worst (mm), neither rounded nor suppressed: how far past the lean (R1); how far
    /// into what keeps it off, or how short of the Z gap (R5, and R3 on the bed); for C5,
    /// the bead overlap: half the sum of widths minus centreline distance, without
    /// subtracting the tolerance; how long the end that holds nothing is (R4); for a path in the air,
    /// how far past the lean it is from anything under it.
    pub by: f64,
}

/// How many faults of each contract, in [`Contract::ALL`] order.
pub fn tally(faults: &[Fault]) -> [usize; 5] {
    let mut c = [0; 5];
    for f in faults {
        c[Contract::ALL.iter().position(|&k| k == f.contract).unwrap_or(0)] += 1;
    }
    c
}

/// How far into what keeps it off a bead goes.
#[derive(Clone, Copy, Debug)]
enum Depth {
    /// Across, in the plane: measured.
    Across,
    /// The Z gap it lies in is short by this much (mm).
    Short(f64),
}

/// What keeps material off on a layer near where it is asked, each with why, in the order a path
/// is told of it.
struct Keep {
    sets: Vec<(&'static str, Depth, SegGrid<u32>)>,
}

/// Where support material may be on every layer (C4): the one predicate for reach, plan, line
/// and the proof, from the part's slices at their actual heights, its paint blockers and ring
/// walls' ways.
pub struct Legal<'a> {
    slices: &'a [Vec<ExPolygon>],
    heights: &'a [(f64, f64)],
    p: &'a SupportParams,
    blocks: &'a [Vec<ExPolygon>],
    rings: &'a [Vec<ExPolygon>],
}

impl<'a> Legal<'a> {
    /// For the part's `slices` at `heights` (top z, height), with `p`; `blocks` and `rings`, per
    /// layer (none past their end): where paint blocks support, and the ways ring walls keep.
    pub fn new(slices: &'a [Vec<ExPolygon>], heights: &'a [(f64, f64)], p: &'a SupportParams, blocks: &'a [Vec<ExPolygon>], rings: &'a [Vec<ExPolygon>]) -> Self {
        Legal { slices, heights, p, blocks, rings }
    }

    pub fn layers(&self) -> usize {
        self.slices.len().min(self.heights.len())
    }

    fn z(&self, l: usize) -> f64 {
        self.heights[l].0
    }

    fn bottom(&self, l: usize) -> f64 {
        self.heights[l].0 - self.heights[l].1
    }

    /// m_l (mm): how far layer `l` may lie past the one under it, from their actual heights.
    pub fn lean(&self, l: usize) -> f64 {
        if l == 0 || l >= self.layers() { 0.0 } else { (self.z(l) - self.z(l - 1)).max(0.0) * self.p.angle_max_deg.to_radians().tan() }
    }

    /// The layer a contact holding part layer `k` prints on: the highest whose top lies at least
    /// the top gap under it.
    pub fn contact(&self, k: usize) -> Option<usize> {
        (0..k.min(self.layers())).rev().find(|&l| self.bottom(k) - self.z(l) >= self.p.top_gap - Z_TOL)
    }

    /// The part layer a support starting on layer `l` stands on, not from the bed only: the
    /// highest at least the bottom gap under it, so nothing is looked through but the gap.
    pub fn landing(&self, l: usize) -> Option<usize> {
        if self.p.build_plate_only || l == 0 || l >= self.layers() {
            return None;
        }
        (0..l).rev().find(|&k| self.bottom(l) - self.z(k) >= self.p.bottom_gap - Z_TOL)
    }

    /// The part's top a support on layer `l` may stand on: its slice at the landing layer, less
    /// what paint blocks there and up to `l` (a blocker is never a landing).
    pub fn rests(&self, l: usize) -> Vec<ExPolygon> {
        let Some(k) = self.landing(l) else { return Vec::new() };
        let blocked: Vec<ExPolygon> = (k..=l).flat_map(|j| self.blocked(j).iter().cloned()).collect();
        if blocked.is_empty() { self.slices[k].clone() } else { difference_ex(&self.slices[k], &blocked) }
    }

    /// Where paint blocks support on layer `l`.
    pub fn blocked(&self, l: usize) -> &[ExPolygon] {
        self.blocks.get(l).map_or(&[], Vec::as_slice)
    }

    /// What of `set` lies in `window` (all of it without one).
    fn cut(&self, set: &[ExPolygon], window: Option<&BBox>) -> Vec<ExPolygon> {
        let Some(bb) = window else { return set.to_vec() };
        let close: Vec<ExPolygon> = set.iter().filter(|e| e.bbox().overlaps(bb)).cloned().collect();
        if close.is_empty() { close } else { intersection_ex(&close, &[ExPolygon::new(Polygon::rect(bb.min, bb.max))]) }
    }

    /// The part a top on layer `l` holds, near `window`: the part layers whose underside lies one
    /// top gap over it, or a layer more.
    fn held(&self, l: usize, window: &BBox) -> Vec<ExPolygon> {
        let up = self.heights.get(l + 1).map_or(self.heights[l].1, |h| h.1);
        let mut out = Vec::new();
        for k in l + 1..self.layers() {
            if self.bottom(k) - self.z(l) > self.p.top_gap + up + Z_TOL {
                break;
            }
            out.extend(self.cut(&self.slices[k], Some(window)));
        }
        if out.len() > 1 { union_ex(&out) } else { out }
    }

    /// Margin (mm) a layer's window takes past what its paths span.
    fn margin(&self) -> f64 {
        self.p.xy_min() + MARGIN
    }

    fn layers_over(&self, l: usize) -> impl Iterator<Item = usize> + '_ {
        (l + 1..self.layers()).take_while(move |&k| !(self.bottom(k) - self.z(l) >= self.p.top_gap - Z_TOL))
    }

    fn layers_under(&self, l: usize) -> impl Iterator<Item = usize> + '_ {
        (0..l).rev().take_while(move |&k| !(self.bottom(l) - self.z(k) >= self.p.bottom_gap - Z_TOL))
    }

    /// The layers of the part [`Legal::keeps`] reads on layer `l`.
    pub fn reads(&self, l: usize) -> std::ops::RangeInclusive<usize> {
        self.layers_under(l).last().unwrap_or(l)..=self.layers_over(l).last().unwrap_or(l)
    }

    /// What keeps material off on layer `l` (within `window`), each with why, in the order a path
    /// in more than one is told of it (R5): the part grown by xy_min; the part within the top gap
    /// over it and the bottom gap under it, by actual z; beside those, the part over it grown by
    /// xy_min tapering to nothing at the face, and the part under it grown by xy_min; paint
    /// blockers; ring walls' ways.
    fn sets(&self, l: usize, window: Option<&BBox>) -> Vec<(&'static str, Depth, Vec<ExPolygon>)> {
        let p = self.p;
        let f = p.xy_min();
        let part = |k: usize| self.cut(&self.slices[k], window);
        let grow = |set: &[ExPolygon], by: f64| offset_ex(set, by * SCALE, Join::Round(ROUND));
        let mut out = Vec::new();
        let here = part(l);
        if !here.is_empty() {
            out.push(if f > 0.0 { ("within xy_min of the part", Depth::Across, grow(&here, f)) } else { ("in the part", Depth::Across, here) });
        }
        // The part over it within the top gap: under it, and beside it out to its share of
        // xy_min, which tapers from the layer to the face.
        let mut beside = Vec::new();
        for k in self.layers_over(l) {
            let gap = self.bottom(k) - self.z(l);
            let over = part(k);
            if !over.is_empty() {
                let share = (1.0 - (self.z(k) - self.z(l)) / (p.top_gap + self.heights[k].1)).clamp(0.0, 1.0);
                if share * f > 0.0 {
                    beside.push(("beside the part within the top gap", Depth::Across, grow(&over, share * f)));
                }
                out.push(("within the top gap under the part", Depth::Short(p.top_gap - gap), over));
            }
        }
        // The part under it within the bottom gap: over it, and beside it out to xy_min.
        let mut under_all = Vec::new();
        for k in self.layers_under(l) {
            let gap = self.bottom(l) - self.z(k);
            let under = part(k);
            if !under.is_empty() {
                if f > 0.0 {
                    under_all.extend(under.iter().cloned());
                }
                out.push(("within the bottom gap over the part", Depth::Short(p.bottom_gap - gap), under));
            }
        }
        out.extend(beside);
        if !under_all.is_empty() {
            out.push(("beside the part within the bottom gap", Depth::Across, grow(&union_ex(&under_all), f)));
        }
        for (why, per) in [("in a support blocker", self.blocks), ("in a ring wall's way", self.rings)] {
            if let Some(set) = per.get(l) {
                let set = self.cut(set, window);
                if !set.is_empty() {
                    out.push((why, Depth::Across, set));
                }
            }
        }
        out
    }

    /// What keeps material off on layer `l` near what `near` spans.
    fn keep(&self, l: usize, near: &BBox) -> Keep {
        let window = widen(near, self.margin());
        Keep { sets: self.sets(l, Some(&window)).into_iter().map(|(why, depth, set)| (why, depth, SegGrid::of_regions(&set, |k| k as u32))).collect() }
    }

    /// What keeps material off on layer `l`, all of it as one (R5): what the generator cuts the
    /// support at.
    pub fn keeps(&self, l: usize) -> Vec<ExPolygon> {
        let all: Vec<ExPolygon> = self.sets(l, None).into_iter().flat_map(|s| s.2).collect();
        if all.is_empty() { all } else { union_ex(&all) }
    }

    /// Where the centre of a bead `w` (mm) wide may not be on layer `l`: what keeps material off,
    /// grown by half the bead.
    pub fn keep_off(&self, l: usize, w: f64) -> Vec<ExPolygon> {
        let all = self.keeps(l);
        if all.is_empty() { all } else { offset_ex(&all, w / 2.0 * SCALE, Join::Round(ROUND)) }
    }

    /// Where a bead `w` (mm) wide gets down from to its footing (§1.1), on every layer.
    pub fn reach(&self, ctl: Ctl, w: f64) -> Result<Reach, Cancelled> {
        self.reach_at(ctl, w, 1.0)
    }

    /// Where the thinnest column ([`SupportParams::column`]) gets down from to its footing: what
    /// [`ledger`] tells as unreachable against, as the generator tells what the part closes in.
    pub fn column_reach(&self, ctl: Ctl) -> Result<Reach, Cancelled> {
        self.reach(ctl, 2.0 * self.p.column())
    }

    /// [`Legal::reach`] for a support that leans `share` of the steep angle's reach a layer.
    pub fn reach_at(&self, ctl: Ctl, w: f64, share: f64) -> Result<Reach, Cancelled> {
        let n = self.layers();
        let keep = crate::par::try_map(ctl, n, |l| self.keep_off(l, w))?;
        let rests = crate::par::try_map(ctl, n, |l| self.rests(l))?;
        let lean: Vec<f64> = (0..n).map(|l| share * self.lean(l)).collect();
        Reach::new(ctl, &keep, &rests, &lean)
    }

    /// The part's layers' (top z, height), mm.
    pub fn heights(&self) -> &[(f64, f64)] {
        self.heights
    }

    pub fn params(&self) -> &SupportParams {
        self.p
    }
}

fn half(w: f64) -> i64 {
    (w / 2.0 * SCALE).round() as i64
}

fn widen(bb: &BBox, by: f64) -> BBox {
    let r = (by * SCALE).ceil() as i64;
    BBox { min: Pt::new(bb.min.x - r, bb.min.y - r), max: Pt::new(bb.max.x + r, bb.max.y + r) }
}

fn around(a: Pt, b: Pt, r: i64) -> BBox {
    BBox { min: Pt::new(a.x.min(b.x) - r, a.y.min(b.y) - r), max: Pt::new(a.x.max(b.x) + r, a.y.max(b.y) + r) }
}

fn mm(p: Pt) -> (f64, f64) {
    (p.x_mm(), p.y_mm())
}

/// Whether `p`, on the line through `a` and `b`, lies on the segment between them.
fn spans(a: Pt, b: Pt, p: Pt) -> bool {
    p.x >= a.x.min(b.x) && p.x <= a.x.max(b.x) && p.y >= a.y.min(b.y) && p.y <= a.y.max(b.y)
}

/// Whether segments `a`-`b` and `c`-`d` share a point: crossing, touching, or overlapping along
/// one line. Exact.
pub(crate) fn meet(a: Pt, b: Pt, c: Pt, d: Pt) -> bool {
    let (o1, o2, o3, o4) = (orient(a, b, c).signum(), orient(a, b, d).signum(), orient(c, d, a).signum(), orient(c, d, b).signum());
    (o1 * o2 < 0 && o3 * o4 < 0) || (o1 == 0 && spans(a, b, c)) || (o2 == 0 && spans(a, b, d)) || (o3 == 0 && spans(c, d, a)) || (o4 == 0 && spans(c, d, b))
}

/// Whether `p` lies closer than `r` (units) to segment `a`-`b`. Exact for points within 2^30
/// units (a metre) of each other.
fn point_closer(p: Pt, a: Pt, b: Pt, r: i64) -> bool {
    let (ab, ap) = (b.sub(a), p.sub(a));
    let r2 = r as i128 * r as i128;
    let (t, l2) = (dot(ap, ab), dot(ab, ab));
    if l2 == 0 || t <= 0 {
        return dot(ap, ap) < r2;
    }
    if t >= l2 {
        let bp = p.sub(b);
        return dot(bp, bp) < r2;
    }
    let c = cross(ab, ap);
    c * c < r2 * l2
}

/// Whether segments `a`-`b` and `c`-`d` come closer than `r` (units): touching and running along
/// each other included. Exact.
pub fn closer(a: Pt, b: Pt, c: Pt, d: Pt, r: i64) -> bool {
    meet(a, b, c, d) || point_closer(a, c, d, r) || point_closer(b, c, d, r) || point_closer(c, a, b, r) || point_closer(d, a, b, r)
}

/// The point of segment `a`-`b` nearest `p`.
fn foot(a: Pt, b: Pt, p: Pt) -> Pt {
    let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let l2 = dx * dx + dy * dy;
    if l2 == 0.0 {
        return a;
    }
    lerp(a, b, ((((p.x - a.x) as f64) * dx + ((p.y - a.y) as f64) * dy) / l2).clamp(0.0, 1.0))
}

/// How far apart segments `a`-`b` and `c`-`d` are (units), and the middle of where they are
/// closest.
fn apart(a: Pt, b: Pt, c: Pt, d: Pt) -> (f64, Pt) {
    let ends = [(a, foot(c, d, a)), (b, foot(c, d, b)), (foot(a, b, c), c), (foot(a, b, d), d)];
    let (p, q) = ends.into_iter().min_by(|x, y| x.0.dist(x.1).total_cmp(&y.0.dist(y.1))).unwrap_or((a, c));
    if p != q && meet(a, b, c, d) {
        let (x1, x2) = (orient(c, d, a) as f64, orient(c, d, b) as f64);
        return (0.0, lerp(a, b, x1 / (x1 - x2)));
    }
    (p.dist(q), lerp(p, q, 0.5))
}

/// Whether segment `j` of `line` and segment `k` of `other`, closer than `r` (units), come close
/// only where both lines end, within a bead `w` (mm) of their ends: a seam, two ends meeting as a
/// loop's ends meet, which is clear (C5). An end against a line's side is not, nor a crossing.
fn seam(line: &Line, j: usize, other: &Line, k: usize, r: i64, w: f64) -> bool {
    let ((a, b), (c, d)) = (line.segs[j], other.segs[k]);
    if line.closed || other.closed || meet(a, b, c, d) {
        return false;
    }
    let off = |l: &Line, s: usize, c: Pt, d: Pt| l.middle(s, w).is_some_and(|(p, q)| closer(p, q, c, d, r));
    !off(line, j, c, d) && !off(other, k, a, b)
}

/// Whether a bead of half width `r` (units) along `a`-`b` goes into the region `grid` holds the
/// edges of: it comes closer than `r` to one, or lies inside. Exact.
fn into(grid: &SegGrid<u32>, a: Pt, b: Pt, r: i64) -> bool {
    if grid.is_empty() {
        return false;
    }
    let mut hit = false;
    grid.visit_box(around(a, b, r), |c, d, _| hit = hit || closer(a, b, c, d, r));
    hit || grid.contains_any(a)
}

/// How far `p` lies outside the region `grid` holds the edges of (units), less than nothing
/// inside it.
fn outside(grid: &SegGrid<u32>, p: Pt) -> f64 {
    let d = grid.nearest(p, |t| t).map_or(f64::INFINITY, |x| x.0);
    if grid.contains_any(p) { -d } else { d }
}

/// The greatest of `f` over [0, `len`], for `f` that changes no faster than its argument, where it
/// is over `floor`: its value, within `tol` under the true greatest, and where it is. Looked at
/// `step` apart first, then only where it could be more than what has been seen (or `floor`).
fn greatest(len: f64, step: f64, floor: f64, tol: f64, mut f: impl FnMut(f64) -> f64) -> (f64, f64) {
    let n = ((len / step.max(1.0)).ceil() as usize).max(1);
    let mut best = (f64::NEG_INFINITY, 0.0);
    let mut open = Vec::with_capacity(n);
    let mut last = (0.0, 0.0);
    for i in 0..=n {
        let t = len * i as f64 / n as f64;
        let v = f(t);
        if v > best.0 {
            best = (v, t);
        }
        if i > 0 {
            open.push((last.0, last.1, t, v));
        }
        last = (t, v);
    }
    while let Some((t0, v0, t1, v1)) = open.pop() {
        if (v0 + v1 + (t1 - t0)) / 2.0 <= best.0.max(floor) + tol {
            continue;
        }
        let tm = (t0 + t1) / 2.0;
        let vm = f(tm);
        if vm > best.0 {
            best = (vm, tm);
        }
        open.push((t0, v0, tm, vm));
        open.push((tm, vm, t1, v1));
    }
    best
}

fn along(a: Pt, b: Pt, t: f64) -> Pt {
    let len = a.dist(b);
    if len > 0.0 { lerp(a, b, t / len) } else { a }
}

/// How tightly a path bends at `c`, from `u` to `w` (units): the lesser of the circle through
/// the three and the round that fits the shorter side, so a sharp turn off a long side is sharp.
fn turn_radius(u: Pt, c: Pt, w: Pt) -> f64 {
    let (a, b) = (c.sub(u), w.sub(c));
    let (x, d) = (cross(a, b) as f64, dot(a, b) as f64);
    if x == 0.0 && d >= 0.0 {
        return f64::INFINITY;
    }
    let (la, lb) = (u.dist(c), c.dist(w));
    let circle = if x == 0.0 { 0.0 } else { u.dist(w) * la * lb / (2.0 * x.abs()) };
    let round = la.min(lb) / (x.atan2(d).abs() / 2.0).tan();
    circle.min(round)
}

/// A path as the contracts look at it: its segments, how far along it each starts, and its bends
/// tighter than [`R_MIN`].
struct Line {
    segs: Vec<(Pt, Pt)>,
    closed: bool,
    /// Arc length (units) at the start of each segment, and at its end last.
    at: Vec<f64>,
    /// `rough[v]`: how many of the vertices before vertex `v` bend tighter than r_min.
    rough: Vec<u32>,
}

impl Line {
    fn of(path: &SupportPath) -> Line {
        let mut pts = path.pts.clone();
        pts.dedup();
        let closed = pts.len() > 2 && pts.first() == pts.last();
        let segs: Vec<(Pt, Pt)> = match pts.len() {
            0 => Vec::new(),
            1 => vec![(pts[0], pts[0])],
            _ => pts.windows(2).map(|w| (w[0], w[1])).collect(),
        };
        let mut at = Vec::with_capacity(segs.len() + 1);
        let mut s = 0.0;
        for &(a, b) in &segs {
            at.push(s);
            s += a.dist(b);
        }
        at.push(s);
        let k = segs.len();
        let verts = if closed { k } else { k + 1 };
        let mut rough = vec![0u32; verts + 1];
        for v in 0..verts {
            let bend = if closed { Some((segs[(v + k - 1) % k].0, segs[v].0, segs[v].1)) } else if v > 0 && v < k { Some((segs[v - 1].0, segs[v].0, segs[v].1)) } else { None };
            rough[v + 1] = rough[v] + bend.is_some_and(|(u, c, w)| turn_radius(u, c, w) < R_MIN * SCALE) as u32;
        }
        Line { segs, closed, at, rough }
    }

    fn len(&self) -> f64 {
        self.at[self.at.len() - 1]
    }

    /// The part of segment `j` farther than `w` (mm) from both ends of the line along it.
    fn middle(&self, j: usize, w: f64) -> Option<(Pt, Pt)> {
        let ((a, b), (s0, s1)) = (self.segs[j], (self.at[j], self.at[j + 1]));
        let (lo, hi) = (s0.max(w * SCALE), s1.min(self.len() - w * SCALE));
        (hi > lo && s1 > s0).then(|| (lerp(a, b, (lo - s0) / (s1 - s0)), lerp(a, b, (hi - s0) / (s1 - s0))))
    }

    /// Whether segments `j` < `k` lie by each other only as the path goes on: next to each
    /// other, or less than π r_min of it apart over bends no tighter than r_min.
    fn exempt(&self, j: usize, k: usize) -> bool {
        let n = self.segs.len();
        if k == j + 1 || (self.closed && j == 0 && k == n - 1) {
            return true;
        }
        let most = std::f64::consts::PI * R_MIN * SCALE;
        let on = self.at[k] - self.at[j + 1] < most && self.rough[k + 1] == self.rough[j + 1];
        let round = self.closed && self.len() - self.at[k + 1] + self.at[j] < most && self.rough[n] - self.rough[k + 1] + self.rough[j + 1] == 0;
        on || round
    }
}

/// Whether the bead of half width `hw` (units) along `line` goes into the part's top that `rest`
/// holds the edges of.
fn on_top(line: &Line, hw: f64, rest: &SegGrid<u32>) -> bool {
    line.segs.iter().any(|&(a, b)| into(rest, a, b, hw.round() as i64))
}

/// Tells `on` each path under it (its segments in `below`, their beads' half widths `halves`, the
/// widest `widest`, all in units) that the bead of half width `hw` along `line` lies on, until `on`
/// says that is enough: whether it did.
fn on_beads(line: &Line, hw: f64, below: &SegGrid<(u32, u32)>, halves: &[f64], widest: f64, mut on: impl FnMut(u32) -> bool) -> bool {
    let mut done = false;
    for &(a, b) in &line.segs {
        below.visit_box(around(a, b, (widest + hw).ceil() as i64), |c, d, (k, _)| {
            done = done || (closer(a, b, c, d, (halves[k as usize] + hw).round() as i64) && on(k));
        });
        if done {
            break;
        }
    }
    done
}

/// What each of `here`, support paths on layer `l` > 0, stands on as [`prove`] looks at it (C7
/// over the bed): `None` on the part's top it may rest on, else the paths of `below` (layer
/// `l - 1`'s) its bead lies on. One that lies on none of them is on air.
pub fn footings(legal: &Legal, l: usize, below: &[SupportPath], here: &[SupportPath]) -> Vec<Option<Vec<u32>>> {
    let lines: Vec<Line> = here.iter().map(Line::of).collect();
    let mut span = BBox::empty();
    for &(a, b) in lines.iter().flat_map(|line| line.segs.iter()) {
        span.add(a);
        span.add(b);
    }
    let rest = legal.cut(&legal.rests(l), Some(&widen(&span, legal.margin())));
    let rest = SegGrid::of_regions(&rest, |k| k as u32);
    let grid = SegGrid::new(below.iter().map(Line::of).enumerate().flat_map(|(i, line)| line.segs.into_iter().enumerate().map(move |(j, (a, b))| (a, b, (i as u32, j as u32)))).collect());
    let halves: Vec<f64> = below.iter().map(|p| p.width * SCALE / 2.0).collect();
    let widest = halves.iter().copied().fold(0.0, f64::max);
    here.iter().zip(&lines).map(|(path, line)| {
        let hw = path.width * SCALE / 2.0;
        if on_top(line, hw, &rest) {
            return None;
        }
        let mut on = Vec::new();
        on_beads(line, hw, &grid, &halves, widest, |k| {
            if on.last() != Some(&k) {
                on.push(k);
            }
            false
        });
        on.sort_unstable();
        on.dedup();
        Some(on)
    }).collect()
}

/// What `paths`, the support's layers for `legal`'s part, break of the contracts (C2, C4-C7):
/// each path told once per contract on its layer at its worst, a conflict once per pair of
/// paths, an end with nothing over it once per stretch that holds nothing.
pub fn prove(ctl: Ctl, legal: &Legal, paths: &[Vec<SupportPath>]) -> Result<Vec<Fault>, Cancelled> {
    let n = paths.len().min(legal.layers());
    let lines = crate::par::try_map(ctl, n, |l| paths[l].iter().map(Line::of).collect::<Vec<_>>())?;
    let grids = crate::par::try_map(ctl, n, |l| {
        SegGrid::new(lines[l].iter().enumerate().flat_map(|(i, line)| line.segs.iter().enumerate().map(move |(j, &(a, b))| (a, b, (i as u32, j as u32)))).collect())
    })?;
    let found = crate::par::try_map(ctl, n, |l| if paths[l].is_empty() { Vec::new() } else { on_layer(legal, l, paths, &lines, &grids) })?;
    Ok(found.into_iter().flatten().collect())
}

fn on_layer(legal: &Legal, l: usize, paths: &[Vec<SupportPath>], lines: &[Vec<Line>], grids: &[SegGrid<(u32, u32)>]) -> Vec<Fault> {
    let (here, lines_l) = (&paths[l], &lines[l]);
    let mut span = BBox::empty();
    for &(a, b) in lines_l.iter().flat_map(|line| line.segs.iter()) {
        span.add(a);
        span.add(b);
    }
    let window = widen(&span, legal.margin());
    let mut out = Vec::new();
    let fault = |contract: Contract, why: &'static str, path: usize, other: Option<usize>, at: Pt, by: f64| Fault { contract, why, layer: l, path, other, at: mm(at), by };
    // C4, and C7 on the bed: the first of what keeps a path off that it goes into past [`TOL`]
    // (a bead on its edge is in by the rounding of the outlines it was cut from), at its worst.
    let keep = legal.keep(l, &span);
    for (i, path) in here.iter().enumerate() {
        let r = half(path.width);
        if lines_l[i].segs.is_empty() {
            continue;
        }
        for (why, depth, grid) in &keep.sets {
            let mut worst: Option<(f64, Pt)> = None;
            for &(a, b) in &lines_l[i].segs {
                if !into(grid, a, b, r - (TOL * SCALE) as i64) {
                    continue;
                }
                let (by, at) = match *depth {
                    Depth::Short(z) => (z, lerp(a, b, 0.5)),
                    Depth::Across => {
                        let (v, t) = greatest(a.dist(b), STEP * SCALE, 0.0, TOL * SCALE, |t| r as f64 - outside(grid, along(a, b, t)));
                        (v / SCALE, along(a, b, t))
                    }
                };
                if worst.is_none_or(|w| by > w.0) {
                    worst = Some((by, at));
                }
            }
            if let Some((by, at)) = worst {
                out.push(fault(if l == 0 { Contract::Footing } else { Contract::Legal }, why, i, None, at, by));
                break;
            }
        }
    }
    // C2 and C7 over the bed: each path within the lean of the centrelines under it (each
    // widened by how much wider its bead is), or of the part's top it stands on; and none in
    // the air, none of its bead on anything.
    if l > 0 {
        let (below, under) = (&grids[l - 1], &paths[l - 1]);
        let rest = legal.cut(&legal.rests(l), Some(&window));
        let rest = SegGrid::of_regions(&rest, |k| k as u32);
        let m = legal.lean(l) * SCALE;
        let halves: Vec<f64> = under.iter().map(|p| p.width * SCALE / 2.0).collect();
        let widest = halves.iter().copied().fold(0.0, f64::max);
        for (i, path) in here.iter().enumerate() {
            let hw = path.width * SCALE / 2.0;
            let f = |p: Pt| -> f64 {
                let mut best = f64::INFINITY;
                below.nearest_with(p, |c, d, (k, _)| {
                    best = best.min(dist_point_segment(p, c, d) - (halves[k as usize] - hw));
                    best + widest - hw
                });
                if !rest.is_empty() {
                    best = best.min(outside(&rest, p).max(0.0));
                }
                best
            };
            let line = &lines_l[i];
            if line.segs.is_empty() {
                continue;
            }
            // Whether any of its bead lies on a bead under it, or on the part's top.
            let touches = on_beads(line, hw, below, &halves, widest, |_| true) || on_top(line, hw, &rest);
            if !touches {
                let nothing = below.is_empty() && rest.is_empty();
                let mut nearest = (f64::INFINITY, line.segs[0].0);
                for p in line.segs.iter().flat_map(|&(a, b)| [a, b]).filter(|_| !nothing) {
                    let v = f(p);
                    if v < nearest.0 {
                        nearest = (v, p);
                    }
                }
                let by = if nothing { 0.0 } else { (nearest.0 - m) / SCALE };
                out.push(fault(Contract::Footing, if nothing { "nothing at all under it" } else { "nothing under it but air" }, i, None, nearest.1, by));
                continue;
            }
            let mut worst: Option<(f64, Pt)> = None;
            for &(a, b) in &line.segs {
                let (v, t) = greatest(a.dist(b), (2.0 * m).max(STEP * SCALE), m, TOL * SCALE, |t| f(along(a, b, t)));
                if v > m + TOL * SCALE && worst.is_none_or(|w| v > w.0) {
                    worst = Some((v, along(a, b, t)));
                }
            }
            if let Some((v, at)) = worst {
                let why = if path.role == PathRole::Interface { "an interface past the lean of what is under it" } else { "past the lean of what is under it" };
                out.push(fault(Contract::Carried, why, i, None, at, (v - m) / SCALE));
            }
        }
    }
    // C5: segments not one bead may touch; only overlap beyond arithmetic slack is a fault.
    let grid = &grids[l];
    let widest = here.iter().map(|p| p.width).fold(0.0, f64::max);
    let mut pairs: std::collections::BTreeMap<(usize, usize), (f64, Pt)> = std::collections::BTreeMap::new();
    for (i, line) in lines_l.iter().enumerate() {
        let w = here[i].width;
        for (j, &(a, b)) in line.segs.iter().enumerate() {
            grid.visit_box(around(a, b, (((w + widest) / 2.0 - BEAD_OVERLAP_TOL).max(0.0) * SCALE).ceil() as i64), |c, d, (i2, j2)| {
                let (i2, j2) = (i2 as usize, j2 as usize);
                if (i2, j2) <= (i, j) || (i2 == i && line.exempt(j, j2)) {
                    return;
                }
                let half_widths = (w + here[i2].width) / 2.0;
                let r = ((half_widths - BEAD_OVERLAP_TOL) * SCALE).round() as i64;
                if r <= 0 || !closer(a, b, c, d, r) {
                    return;
                }
                if seam(line, j, &lines_l[i2], j2, r, w.max(here[i2].width)) {
                    return;
                }
                let (gap, at) = apart(a, b, c, d);
                let by = half_widths - gap / SCALE;
                let e = pairs.entry((i, i2)).or_insert((f64::NEG_INFINITY, at));
                if by > e.0 {
                    *e = (by, at);
                }
            });
        }
    }
    for ((i, i2), (by, at)) in pairs {
        out.push(fault(Contract::Clear, if i == i2 { "a path comes back by itself" } else { "two paths run into each other" }, i, Some(i2), at, by));
    }
    // C6: what has nothing over it within the next layer's lean of its material is a contact
    // under the part, or a fork's crotch.
    let up = (l + 1 < lines.len()).then(|| (&grids[l + 1], &paths[l + 1])).filter(|(g, _)| !g.is_empty());
    let m_up = legal.lean(l + 1) * SCALE;
    let over: Vec<f64> = up.map_or_else(Vec::new, |(_, ps)| ps.iter().map(|p| p.width * SCALE / 2.0).collect());
    let widest_up = over.iter().copied().fold(0.0, f64::max);
    let held = legal.held(l, &window);
    let held = SegGrid::of_regions(&held, |k| k as u32);
    for (i, path) in here.iter().enumerate() {
        let line = &lines_l[i];
        if line.segs.is_empty() {
            continue;
        }
        let g = |p: Pt| -> f64 {
            let Some((grid, _)) = up else { return f64::INFINITY };
            let mut best = f64::INFINITY;
            grid.nearest_with(p, |c, d, (k, _)| {
                best = best.min(dist_point_segment(p, c, d) - over[k as usize]);
                best + widest_up
            });
            best
        };
        // Looked along it: (arc, point, whether nothing is over it).
        let mut seen: Vec<(f64, Pt, bool)> = Vec::new();
        for (j, &(a, b)) in line.segs.iter().enumerate() {
            let len = a.dist(b);
            let (ga, gb) = (g(a), g(b));
            if j == 0 {
                seen.push((0.0, a, ga > m_up));
            }
            if (ga + gb + len) / 2.0 > m_up {
                let k = (len / (STEP * SCALE)).ceil() as usize;
                for q in 1..k {
                    let t = len * q as f64 / k as f64;
                    let p = along(a, b, t);
                    seen.push((line.at[j] + t, p, g(p) > m_up));
                }
            }
            seen.push((line.at[j] + len, b, gb > m_up));
        }
        let last = seen.len() - 1;
        let before = |s: usize| if s > 0 { (seen[s].0 - seen[s - 1].0) / 2.0 } else if line.closed { (line.len() - seen[last - 1].0) / 2.0 } else { 0.0 };
        let after = |s: usize| if s < last { (seen[s + 1].0 - seen[s].0) / 2.0 } else if line.closed { seen[1].0 / 2.0 } else { 0.0 };
        let length = |run: &[usize]| before(run[0]) + after(run[run.len() - 1]) + run.windows(2).map(|w| (seen[w[1]].0 - seen[w[0]].0).max(0.0)).sum::<f64>();
        // Its stretches with nothing over it, a closed path's run round its start as one.
        let mut runs: Vec<Vec<usize>> = Vec::new();
        for s in 0..=last {
            if !seen[s].2 {
                continue;
            }
            match runs.last_mut() {
                Some(run) if run[run.len() - 1] + 1 == s => run.push(s),
                _ => runs.push(vec![s]),
            }
        }
        let whole = runs.len() == 1 && runs[0].len() == seen.len();
        if line.closed && !whole && runs.len() > 1 && runs[0][0] == 0 && runs[runs.len() - 1][runs[runs.len() - 1].len() - 1] == last {
            let first = runs.remove(0);
            if let Some(run) = runs.last_mut() {
                run.extend(first);
            }
        }
        let reach = (path.width / 2.0 + ASIDE) * SCALE;
        for run in runs {
            let ends = whole || (!line.closed && (run[0] == 0 || run[run.len() - 1] == last));
            if !ends && length(&run) <= 2.0 * (path.width * SCALE + m_up) {
                continue;
            }
            // Each stretch of it that is no contact: (what it is, its looks).
            let kind = |s: usize| {
                let p = seen[s].1;
                let under = held.contains_any(p) || {
                    let mut near = false;
                    held.visit_box(around(p, p, reach.ceil() as i64), |c, d, _| near = near || dist_point_segment(p, c, d) <= reach);
                    near
                };
                match (under, path.role == PathRole::Interface) {
                    (true, true) => None,
                    (true, false) => Some("a top that is not interface"),
                    (false, _) if ends => Some("a dead end"),
                    (false, _) => Some("a top that holds nothing"),
                }
            };
            let mut stretch: Vec<usize> = Vec::new();
            let mut what = None;
            for s in run.iter().copied().chain(std::iter::once(usize::MAX)) {
                let k = if s == usize::MAX { None } else { kind(s) };
                if k != what || k.is_none() {
                    if let (Some(why), false) = (what, stretch.is_empty()) {
                        out.push(fault(Contract::Purpose, why, i, None, seen[stretch[stretch.len() / 2]].1, length(&stretch) / SCALE));
                    }
                    stretch.clear();
                    what = k;
                }
                if k.is_some() {
                    stretch.push(s);
                }
            }
        }
    }
    out
}

/// What of `need`, a part layer's underside, a support holds (C6): what lies within half the
/// contact gap of `printed`, what of the support prints on its contact's layer. What prints
/// further than [`HELD_NEAR`] from `need` grows nowhere near it, so only what lies that near is
/// grown.
pub fn held(need: &[ExPolygon], printed: &[ExPolygon]) -> Vec<ExPolygon> {
    if need.is_empty() || printed.is_empty() {
        return Vec::new();
    }
    let near = intersection_ex(&union_ex(printed), &offset_ex(need, HELD_NEAR * SCALE, Join::Miter(3.0)));
    if near.is_empty() {
        return Vec::new();
    }
    intersection_ex(need, &offset_ex(&near, CONTACT_GAP / 2.0 * SCALE, Join::Round(0.005 * SCALE)))
}

/// How near the demand (mm) what prints is grown by [`held`]: half the contact gap and a little,
/// so that what is cut off grows short of the demand.
pub const HELD_NEAR: f64 = CONTACT_GAP / 2.0 + 0.01;

/// How much of `asked` (per part layer, the overhang its supports were asked to hold) they hold on
/// `legal`'s part: what lies within half the contact gap of what prints on its contact's layer
/// ([`held`]), `printed(l, near)` being what prints on layer `l` near a box. `ctl`'s tick is told
/// per layer measured.
pub fn hold(ctl: Ctl, legal: &Legal, asked: &[Vec<ExPolygon>], printed: &(dyn Fn(usize, &BBox) -> Vec<ExPolygon> + Sync)) -> Result<crate::skeleton::Hold, Cancelled> {
    let got = crate::par::try_map(ctl, asked.len(), |d| {
        let Some(c) = legal.contact(d).filter(|_| !asked[d].is_empty()) else { return 0.0 };
        total_area_mm2(&held(&asked[d], &printed(c, &widen(&bbox_of(&asked[d]), CONTACT_GAP))))
    })?;
    Ok(crate::skeleton::Hold::of(asked.iter().map(|a| total_area_mm2(a)).sum(), got.into_iter().sum(), legal.p))
}

/// Where the demand went (C6): per demand layer, each place under one cause.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    /// Held: within half the contact gap of what prints at its contact's height (see [`held`]).
    pub covered: Vec<Vec<ExPolygon>>,
    /// Lost: paint blocks support there.
    pub blocked: Vec<Vec<ExPolygon>>,
    /// Lost: no column, not even the thinnest, gets down from within half the contact gap of it.
    pub unreachable: Vec<Vec<ExPolygon>>,
    /// Lost: a support could get there, and none does.
    pub infeasible: Vec<Vec<ExPolygon>>,
}

impl Ledger {
    /// The area (mm2) under each cause, in field order.
    pub fn areas(&self) -> [f64; 4] {
        let sum = |set: &[Vec<ExPolygon>]| set.iter().fold(0.0, |a, l| a + total_area_mm2(l));
        [sum(&self.covered), sum(&self.blocked), sum(&self.unreachable), sum(&self.infeasible)]
    }
}

/// Where `demand` (per part layer, what of its underside needs support) went, from `paths`, the
/// support's material, and `reach`, `legal`'s for the thinnest column (see
/// [`Legal::column_reach`]). Taken out of what is left in turn: covered, blocked, unreachable, and
/// what is left is infeasible, so no place has two causes and together they are the demand. What
/// would not come off is told with the removal check, apart.
pub fn ledger(ctl: Ctl, legal: &Legal, demand: &[Vec<ExPolygon>], paths: &[Vec<SupportPath>], reach: &Reach) -> Result<Ledger, Cancelled> {
    let n = demand.len().min(legal.layers());
    let less = |a: &[ExPolygon], b: &[ExPolygon]| if a.is_empty() || b.is_empty() { a.to_vec() } else { difference_ex(a, b) };
    let both = |a: &[ExPolygon], b: &[ExPolygon]| if a.is_empty() || b.is_empty() { Vec::new() } else { intersection_ex(a, b) };
    let column = legal.p.column();
    let rows = crate::par::try_map(ctl, n, |d| {
        let need = &demand[d];
        let Some(c) = legal.contact(d).filter(|_| !need.is_empty()) else { return (Vec::new(), Vec::new(), need.clone(), Vec::new()) };
        let covered = held(need, &material(paths.get(c).map_or(&[][..], Vec::as_slice)));
        let rest = less(need, &covered);
        let blocked = both(&rest, legal.blocked(d));
        let rest = less(&rest, &blocked);
        let unreachable = both(&rest, &reach.beyond(c, CONTACT_GAP / 2.0 + column));
        let infeasible = less(&rest, &unreachable);
        (covered, blocked, unreachable, infeasible)
    })?;
    let mut out = Ledger::default();
    for (c, b, u, i) in rows {
        out.covered.push(c);
        out.blocked.push(b);
        out.unreachable.push(u);
        out.infeasible.push(i);
    }
    Ok(out)
}

/// A hard check (see the module): what fails one is told with its place, not as a share of the
/// demand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Hard {
    /// H1: a top of the support that holds no overhang: a branch no tip needs.
    DeadEnd,
    /// H2: a top under an overhang that stops short of the top gap under it.
    ShortTop,
    /// H2 the other way: a top over the layer a contact prints on, in the top gap under the part
    /// over it. When it holds nothing it is a dead end as well.
    InGap,
    /// H3: overhang a support could get to that nothing holds.
    Unheld,
}

impl Hard {
    pub const ALL: [Hard; 4] = [Hard::DeadEnd, Hard::ShortTop, Hard::InGap, Hard::Unheld];

    /// "H1", "H2", "H2g" (H2 the other way, into the gap) and "H3".
    pub fn code(self) -> &'static str {
        match self {
            Hard::DeadEnd => "H1",
            Hard::ShortTop => "H2",
            Hard::InGap => "H2g",
            Hard::Unheld => "H3",
        }
    }
}

/// A hard check failed: on which layer (the support's for H1 and H2, the part's for H3), where
/// (its middle, in the plan's frame, mm), how much (mm2), and for H2 by how many layers the top
/// is off the layer its contact prints on.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub check: Hard,
    pub layer: usize,
    pub at: (f64, f64),
    pub area_mm2: f64,
    pub by: usize,
}

/// The hard checks of `layers`, the support for `legal`'s part, with `demand` (per part layer,
/// what of its underside needs support) and `book`, where that demand went ([`ledger`]). Each
/// island of the support that carries nothing on the layer over it is a top. A top holds the
/// overhang right over it, as far aside as [`ASIDE`] (H1), from the top gap under it: no lower
/// and no higher (H2). What the ledger finds a support could get to is held (H3).
pub fn hard(ctl: Ctl, legal: &Legal, layers: &[SupportLayer], demand: &[Vec<ExPolygon>], book: &Ledger) -> Result<Vec<Failure>, Cancelled> {
    let n = layers.len().min(legal.layers());
    let support = crate::par::try_map(Ctl { tick: None, ..ctl }, n, |l| union_ex(&[layers[l].base.clone(), layers[l].interface.clone()].concat()))?;
    // Whether `set`'s pieces near `bb` overlap `of` by SLACK or more.
    let overlaps = |set: &[ExPolygon], bb: &BBox, of: &[ExPolygon]| -> bool {
        let close: Vec<ExPolygon> = set.iter().filter(|e| e.bbox().overlaps(bb)).cloned().collect();
        !close.is_empty() && total_area_mm2(&intersection_ex(&close, of)) >= SLACK
    };
    let tops = crate::par::try_map(ctl, n, |l| {
        let up: &[ExPolygon] = if l + 1 < n { &support[l + 1] } else { &[] };
        let mut out = Vec::new();
        for island in support[l].iter().filter(|e| e.area_mm2() >= SLACK) {
            let one = std::slice::from_ref(island);
            if overlaps(up, &island.bbox(), one) {
                continue;
            }
            let held_by = offset_ex(one, ASIDE * SCALE, Join::Miter(3.0));
            let bb = bbox_of(&held_by);
            // its face: the first layer of the part over it
            let face = (l + 1..legal.layers()).find(|&k| overlaps(&legal.slices[k], &bb, &held_by));
            let holds = face.is_some_and(|k| (k..=(k + 1).min(demand.len().saturating_sub(1))).any(|d| overlaps(&demand[d], &bb, &held_by)));
            let c = island.bbox().center();
            let at = (c.x_mm(), c.y_mm());
            let area_mm2 = island.area_mm2();
            if !holds {
                out.push(Failure { check: Hard::DeadEnd, layer: l, at, area_mm2, by: 0 });
            }
            // Where a contact under its face prints: under it is short, over it in the gap.
            if let Some(k) = face {
                match legal.contact(k) {
                    Some(c) if c > l && holds => out.push(Failure { check: Hard::ShortTop, layer: l, at, area_mm2, by: c - l }),
                    Some(c) if c < l => out.push(Failure { check: Hard::InGap, layer: l, at, area_mm2, by: l - c }),
                    None => out.push(Failure { check: Hard::InGap, layer: l, at, area_mm2, by: l + 1 }),
                    _ => {}
                }
            }
        }
        out
    })?;
    let mut out: Vec<Failure> = tops.into_iter().flatten().collect();
    for (d, lost) in book.infeasible.iter().enumerate() {
        for e in lost.iter().filter(|e| e.area_mm2() >= SLACK) {
            let c = e.bbox().center();
            out.push(Failure { check: Hard::Unheld, layer: d, at: (c.x_mm(), c.y_mm()), area_mm2: e.area_mm2(), by: 0 });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coral::material::Role;

    const W: f64 = 0.4;
    /// A layer's lean at 0.2 mm layers and 40 degrees (mm).
    const LEAN: f64 = 0.2 * 0.839_099_631_177_280;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon::new(vec![Pt::from_mm(x0, y0), Pt::from_mm(x1, y0), Pt::from_mm(x1, y1), Pt::from_mm(x0, y1)]))
    }

    /// A part's slices, its heights and paint, and a support's paths, to prove.
    struct Case {
        slices: Vec<Vec<ExPolygon>>,
        heights: Vec<(f64, f64)>,
        p: SupportParams,
        blocks: Vec<Vec<ExPolygon>>,
        paths: Vec<Vec<SupportPath>>,
    }

    impl Case {
        /// `n` layers 0.2 mm thick with nothing on them, gaps of a layer, 0.35 mm off the walls.
        fn new(n: usize) -> Case {
            let p = SupportParams { threshold_deg: 30.0, xy_gap: 0.35, top_gap: 0.2, bottom_gap: 0.2, interface_top_layers: 3, line_width: W, wall_width: W, tip_diameter: 0.8, branch_diameter: 5.0, taper_deg: 5.0, angle_max_deg: 40.0, angle_slow_deg: 25.0, build_plate_only: false, remove_small: true, first_expansion: 2.0, stand_inside: false };
            Case { slices: vec![Vec::new(); n], heights: (0..n).map(|l| (0.2 * (l + 1) as f64, 0.2)).collect(), p, blocks: Vec::new(), paths: vec![Vec::new(); n] }
        }

        /// The part has `e` on `layers` too.
        fn part(mut self, layers: std::ops::Range<usize>, e: &[ExPolygon]) -> Case {
            for l in layers {
                self.slices[l] = union_ex(&[self.slices[l].clone(), e.to_vec()].concat());
            }
            self
        }

        /// A path along `pts` (mm) on each of `layers`, `role` (the pad on the bed for the body).
        fn on(mut self, layers: std::ops::Range<usize>, role: Role, pts: &[(f64, f64)]) -> Case {
            for l in layers {
                let role = if l == 0 && role == Role::Body { Role::Pad } else { role };
                let frond = self.paths[l].len();
                self.paths[l].push(SupportPath { pts: pts.iter().map(|&(x, y)| Pt::from_mm(x, y)).collect(), width: W, role, support: 0, frond });
            }
            self
        }

        fn legal(&self) -> Legal<'_> {
            Legal::new(&self.slices, &self.heights, &self.p, &self.blocks, &[])
        }
    }

    #[test]
    fn keeps_reads_only_its_part_layers() {
        let mut case = Case::new(8);
        case.p.top_gap = 0.43;
        case.p.bottom_gap = 0.37;
        let mut z = 0.0;
        for (l, h) in [0.12, 0.28, 0.16, 0.32, 0.18, 0.24, 0.14, 0.26].into_iter().enumerate() {
            z += h;
            case.heights[l] = (z, h);
            let x = 3.0 * l as f64;
            case.slices[l] = vec![rect(x, 0.0, x + 1.0 + 0.1 * l as f64, 1.0)];
        }
        let legal = case.legal();
        for l in 0..case.slices.len() {
            let keeps = legal.keeps(l);
            let reads = legal.reads(l);
            let mut slices = case.slices.clone();
            for (k, slice) in slices.iter_mut().enumerate() {
                if !reads.contains(&k) {
                    slice.clear();
                }
            }
            assert_eq!(Legal::new(&slices, &case.heights, &case.p, &[], &[]).keeps(l), keeps, "layer {l}: slices outside {reads:?} do not affect the keep");
            for k in reads {
                let mut slices = case.slices.clone();
                slices[k].clear();
                assert_ne!(Legal::new(&slices, &case.heights, &case.p, &[], &[]).keeps(l), keeps, "layer {l}: changing part layer {k} changes the keep");
            }
        }
    }

    /// A line prints in every piece of a layer.
    fn lined(_: usize, _: &ExPolygon) -> bool {
        true
    }

    /// What the proof lists for `case`: (rule, layer, raw worst, where), in order.
    fn found(case: &Case) -> Vec<(&'static str, usize, f64, (f64, f64))> {
        let mut out: Vec<_> = prove(Ctl::default(), &case.legal(), &case.paths).unwrap().into_iter().map(|f| (f.contract.code(), f.layer, f.by, f.at)).collect();
        out.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)).then(a.2.total_cmp(&b.2)));
        out
    }

    fn listed(found: &[(&'static str, usize, f64, (f64, f64))]) -> Vec<(&'static str, usize)> {
        found.iter().map(|f| (f.0, f.1)).collect()
    }

    /// Every raw worst listed of `rule` on `layer`.
    fn worst(found: &[(&'static str, usize, f64, (f64, f64))], rule: &str, layer: usize) -> Vec<f64> {
        found.iter().filter(|f| f.0 == rule && f.1 == layer).map(|f| f.2).collect()
    }

    /// R6 as it prints: a roof one line wide, a body two. On a body 3 mm wide, a strip a line
    /// and a half wide keeps the rule as roof and breaks it as body; a roof strip of 0.6 lines
    /// breaks it too.
    #[test]
    fn a_roof_prints_one_line_wide_and_a_body_two() {
        let case = Case::new(4);
        let legal = case.legal();
        let under = rect(-1.0, -1.0, 11.0, 2.0);
        let layer = |base: Vec<ExPolygon>, interface: Vec<ExPolygon>| SupportLayer { base, interface, ..Default::default() };
        let narrow = |top: SupportLayer| {
            let layers = [layer(vec![under.clone()], Vec::new()), layer(vec![under.clone()], Vec::new()), top, SupportLayer::default()];
            check(Ctl::default(), &legal, &layers, &lined).unwrap().into_iter().filter(|b| b.rule == Rule::Width).map(|b| (b.layer, b.why)).collect::<Vec<_>>()
        };
        let strip = |wide: f64| vec![rect(0.0, 0.0, 10.0, wide * W)];
        assert!(narrow(layer(Vec::new(), strip(1.5))).is_empty(), "a roof a line and a half wide");
        assert_eq!(narrow(layer(strip(1.5), Vec::new())), [(2, "narrower than two lines")], "a body as wide");
        assert_eq!(narrow(layer(Vec::new(), strip(0.6))), [(2, "a roof narrower than a line")], "a roof under a line");
    }

    /// R6 judges what prints. A roof with a leg narrower than a line, on the two layers under the
    /// part, on a pad of its shape: a leg a line prints in is a roof too thin to print; one no line
    /// prints in is not there, and what it would hold is told: the part over it, held by nothing
    /// (H3), and the leg over it, past the steep angle of what prints under it (R2).
    #[test]
    fn a_roof_no_line_prints_in_holds_nothing() {
        let leg = rect(4.0, 0.9, 10.0, 0.9 + 0.6 * W);
        let roof = union_ex(&[rect(0.0, 0.0, 4.0, 2.0), leg]);
        let layer = |base: Vec<ExPolygon>, interface: Vec<ExPolygon>| SupportLayer { base, interface, ..Default::default() };
        let mut layers = vec![SupportLayer::default(); 6];
        layers[0] = layer(roof.clone(), Vec::new());
        layers[1] = layer(Vec::new(), roof.clone());
        layers[2] = layer(Vec::new(), roof.clone());
        // The roof's lines on both its layers, and its leg's on `legs`: the rules' breaks, and the
        // places of the part over the roof that nothing holds.
        let run = |legs: std::ops::Range<usize>| {
            let mut case = Case::new(6).part(4..6, &[rect(-5.0, -5.0, 15.0, 8.0)]);
            for y in [0.2, 0.6, 1.0, 1.4, 1.8] {
                case = case.on(1..3, Role::Interface, &[(0.2, y), (3.8, y)]);
            }
            let case = case.on(legs, Role::Interface, &[(4.2, 1.02), (9.8, 1.02)]);
            let legal = case.legal();
            let prints = |l: usize, e: &ExPolygon| prints_in(e, &case.paths[l]);
            let breaks: Vec<(&str, usize)> = check(Ctl::default(), &legal, &layers, &prints).unwrap().into_iter().map(|b| (b.rule.code(), b.layer)).collect();
            let mut demand = vec![Vec::new(); 6];
            demand[4] = roof.clone();
            let reach = legal.column_reach(Ctl::default()).unwrap();
            let book = ledger(Ctl::default(), &legal, &demand, &case.paths, &reach).unwrap();
            let unheld: Vec<(usize, f64)> = hard(Ctl::default(), &legal, &layers, &demand, &book).unwrap().into_iter().filter(|f| f.check == Hard::Unheld).map(|f| (f.layer, f.at.0)).collect();
            (breaks, unheld)
        };
        let (breaks, unheld) = run(0..0);
        assert!(breaks.is_empty(), "no line in either leg: {breaks:?}");
        assert!(unheld.len() == 1 && unheld[0].0 == 4 && (4.5..10.0).contains(&unheld[0].1), "the part over the leg, past what the roof's lines hold: {unheld:?}");
        assert_eq!(run(2..3), (vec![("R2", 2), ("R6", 2)], Vec::new()), "a line in the top leg alone");
        assert_eq!(run(1..3), (vec![("R6", 1), ("R6", 2)], Vec::new()), "a line in both legs");
    }

    /// A cancel keeps no part of the check: wherever it comes, even between a layer's rules, the
    /// check ends with none of its breaks; never asked for, with all of them.
    #[test]
    fn a_cancelled_check_keeps_nothing() {
        let case = Case::new(4);
        let legal = case.legal();
        let under = rect(-1.0, -1.0, 11.0, 2.0);
        let layer = |base: Vec<ExPolygon>, interface: Vec<ExPolygon>| SupportLayer { base, interface, ..Default::default() };
        let layers = [layer(vec![under.clone()], Vec::new()), layer(vec![under.clone()], Vec::new()), layer(Vec::new(), vec![rect(0.0, 0.0, 10.0, 0.6 * W)]), SupportLayer::default()];
        // The check with a cancel that comes at its `k`th ask, counting from 0, and stays; and
        // how many times it asked.
        let run = |k: usize| {
            let asked = std::sync::atomic::AtomicUsize::new(0);
            let cancel = || asked.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= k;
            let found = check(Ctl { cancel: Some(&cancel), ..Ctl::default() }, &legal, &layers, &lined);
            (found, asked.into_inner())
        };
        let (found, asks) = run(usize::MAX);
        let all = found.unwrap();
        assert!(!all.is_empty(), "the roof strip breaks R6");
        // Each of its two maps asks before each layer, and it asks once at its end: past those, it
        // asks between a layer's rules.
        assert!(asks > 2 * layers.len() + 1, "asked {asks} times");
        for k in 0..asks {
            assert_eq!(run(k).0, Err(Cancelled), "a cancel at ask {k} of {asks}");
        }
    }

    /// R2 at a roof's bottom: it bridges between the tips under it. Over a gap 3 mm across in a
    /// ring of tips it is held; over a gap past [`ROOF_SPAN`] its middle breaks the rule, and so
    /// does a body over the 3 mm gap.
    #[test]
    fn a_roofs_bottom_bridges_between_its_tips() {
        let case = Case::new(4);
        let legal = case.legal();
        let layer = |base: Vec<ExPolygon>, interface: Vec<ExPolygon>| SupportLayer { base, interface, ..Default::default() };
        let grown = |gap: f64, roof: bool| {
            let top = vec![rect(0.0, 0.0, 2.0 + gap, 2.0 + gap)];
            let tips = difference_ex(&top, &[rect(1.0, 1.0, 1.0 + gap, 1.0 + gap)]);
            let over = if roof { layer(Vec::new(), top) } else { layer(top, Vec::new()) };
            let layers = [layer(tips.clone(), Vec::new()), layer(tips, Vec::new()), over, SupportLayer::default()];
            check(Ctl::default(), &legal, &layers, &lined).unwrap().into_iter().filter(|b| b.rule == Rule::Growth).map(|b| b.layer).collect::<Vec<_>>()
        };
        assert!(grown(3.0, true).is_empty(), "a roof's bottom across a 3 mm gap");
        assert_eq!(grown(ROOF_SPAN + 0.5, true), [2], "a roof's bottom across a gap past the span");
        assert_eq!(grown(3.0, false), [2], "a body across the 3 mm gap");
    }

    /// Two lines end to end 1.2 mm apart, and on layer 4 one line across both, the interface
    /// under a slab: its middle lies 0.6 mm off what is under it, past a layer's lean, however
    /// well both its ends stand.
    #[test]
    fn an_unsupported_middle_between_supported_ends_breaks_r1() {
        let case = Case::new(8).part(6..8, &[rect(-5.0, -5.0, 30.0, 5.0)])
            .on(0..4, Role::Body, &[(0.0, 0.0), (10.0, 0.0)])
            .on(0..4, Role::Body, &[(11.2, 0.0), (21.2, 0.0)])
            .on(4..5, Role::Interface, &[(0.0, 0.0), (21.2, 0.0)]);
        let f = found(&case);
        assert_eq!(listed(&f), [("R1", 4)], "{f:?}");
        assert!((f[0].2 - (0.6 - LEAN)).abs() < 1e-3 && (f[0].3 .0 - 10.6).abs() < 0.01, "at its middle, the gap's half less the lean: {f:?}");
    }

    /// Body strands 1.2 mm apart, and over them a weave across them, the interface under a slab:
    /// every weave line's middle between two strands lies 0.6 mm off them. No closure bridges it.
    #[test]
    fn a_weave_over_a_wide_gap_breaks_fixed_r1() {
        let mut case = Case::new(8).part(6..8, &[rect(-5.0, -5.0, 15.0, 10.0)]);
        for y in [0.0, 1.2, 2.4, 3.6] {
            case = case.on(0..4, Role::Body, &[(0.0, y), (10.0, y)]);
        }
        for x in 0..=10 {
            case = case.on(4..5, Role::Interface, &[(x as f64, 0.0), (x as f64, 3.6)]);
        }
        let f = found(&case);
        assert_eq!(listed(&f), [("R1", 4); 11], "{f:?}");
        assert!(worst(&f, "R1", 4).iter().all(|by| (by - (0.6 - LEAN)).abs() < 1e-3), "{f:?}");
    }

    /// A wall with a slab out over the support beside it, 2 mm off the walls, its outer wall's line
    /// as wide as the support's: xy_min is half of it, 0.2 mm. A line whose bead comes 1.01 mm off
    /// the wall is in the side gap, which is only preferred, and may be there; one whose bead comes
    /// 0.1 mm off it is 0.1 mm into xy_min, on the bed and on every layer up to the slab's contact.
    #[test]
    fn a_path_inside_xy_min_breaks_r5_and_one_in_the_side_gap_does_not() {
        let mut case = Case::new(18).part(0..18, &[rect(10.0, 0.0, 20.0, 10.0)]).part(16..18, &[rect(-5.0, -5.0, 20.0, 15.0)])
            .on(0..12, Role::Body, &[(8.79, 0.0), (8.79, 10.0)])
            .on(12..15, Role::Interface, &[(8.79, 0.0), (8.79, 10.0)]);
        case.p.xy_gap = 2.0;
        let f = found(&case);
        assert!(f.is_empty(), "in the side gap: {f:?}");
        let case = case.on(0..12, Role::Body, &[(9.7, 0.0), (9.7, 10.0)]).on(12..15, Role::Interface, &[(9.7, 0.0), (9.7, 10.0)]);
        let f = found(&case);
        let want: Vec<(&str, usize)> = std::iter::once(("R3", 0)).chain((1..=14).map(|l| ("R5", l))).collect();
        assert_eq!(listed(&f), want, "{f:?}");
        assert!(f.iter().all(|x| (x.2 - 0.1).abs() < 1e-3), "0.1 mm into xy_min: {f:?}");
    }

    /// A pad on the bed under a support, in a paint blocker: what stands there has no footing.
    #[test]
    fn a_blocked_footing_is_not_a_footing() {
        let mut case = Case::new(8).part(6..8, &[rect(-5.0, -5.0, 15.0, 5.0)])
            .on(0..4, Role::Body, &[(0.0, 0.0), (10.0, 0.0)])
            .on(4..5, Role::Interface, &[(0.0, 0.0), (10.0, 0.0)]);
        case.blocks = vec![vec![rect(-1.0, -1.0, 11.0, 1.0)]];
        let f = found(&case);
        assert_eq!(listed(&f), [("R3", 0)], "{f:?}");
        assert!((f[0].2 - 1.2).abs() < 1e-3, "a bead's half and 1 mm in: {f:?}");
    }

    /// A first layer 0.2 mm, a second 0.3 and the rest 0.1, a top gap of 0.2 mm: the second
    /// layer may lean 0.25 mm, the third only 0.084; and a slab 0.1 mm over the support's top is
    /// one layer of 0.1 mm over it, half its top gap, not one of the second layer's 0.3.
    #[test]
    fn variable_heights_use_their_own_reach() {
        let mut case = Case::new(13).part(10..13, &[rect(-5.0, -5.0, 15.0, 5.0)])
            .on(0..1, Role::Body, &[(0.0, 0.0), (10.0, 0.0)])
            .on(1..2, Role::Body, &[(0.0, 0.24), (10.0, 0.24)])
            .on(2..6, Role::Body, &[(0.0, 0.33), (10.0, 0.33)])
            .on(6..9, Role::Interface, &[(0.0, 0.33), (10.0, 0.33)]);
        case.heights = (0..13).map(|l| match l { 0 => (0.2, 0.2), 1 => (0.5, 0.3), _ => (0.5 + 0.1 * (l - 1) as f64, 0.1) }).collect();
        let f = found(&case);
        assert_eq!(listed(&f), [("R1", 2), ("R5", 8)], "{f:?}");
        let tan = 40f64.to_radians().tan();
        assert!((f[0].2 - (0.09 - 0.1 * tan)).abs() < 1e-4, "the third layer leans 0.09 mm: {f:?}");
        assert!((f[1].2 - 0.1).abs() < 1e-6, "the slab is 0.1 mm short of the top gap: {f:?}");
    }

    /// An L, its leg ending two layers up while the line it came off goes on to the interface
    /// under a slab: the leg's end, one bead wide, holds nothing.
    #[test]
    fn a_single_bead_dead_end_breaks_r4() {
        let case = Case::new(8).part(6..8, &[rect(-5.0, -5.0, 15.0, 5.0)])
            .on(0..3, Role::Body, &[(0.0, 3.0), (0.0, 0.0), (10.0, 0.0)])
            .on(3..4, Role::Body, &[(0.0, 0.0), (10.0, 0.0)])
            .on(4..5, Role::Interface, &[(0.0, 0.0), (10.0, 0.0)]);
        let f = found(&case);
        assert_eq!(listed(&f), [("R4", 2)], "{f:?}");
        let end = 3.0 - (LEAN + W / 2.0);
        assert!((f[0].2 - end).abs() < 0.03, "the leg but where the line over it leans to: {f:?}");
    }

    /// On one layer, all interface under a slab: two lines along one line, two a bead apart, a
    /// line ending on another, two a bead plus 0.1 mm apart, and a line turning back round a
    /// bend wider than r_min. Only the collinear overlap and the line ending on another fuse.
    #[test]
    fn collinear_and_tangent_contacts_are_conflicts() {
        let mut turn = vec![(0.0, 25.0), (10.0, 25.0)];
        for k in 1..=12 {
            let a = -std::f64::consts::FRAC_PI_2 + std::f64::consts::PI * k as f64 / 12.0;
            turn.push((10.0 + 0.6 * a.cos(), 25.6 + 0.6 * a.sin()));
        }
        turn.push((0.0, 26.2));
        let lines = [
            vec![(0.0, 0.0), (10.0, 0.0)],
            vec![(5.0, 0.0), (15.0, 0.0)],
            vec![(0.0, 5.0), (10.0, 5.0)],
            vec![(0.0, 5.4), (10.0, 5.4)],
            vec![(0.0, 10.0), (10.0, 10.0)],
            vec![(5.0, 10.0), (5.0, 15.0)],
            vec![(0.0, 20.0), (10.0, 20.0)],
            vec![(0.0, 20.5), (10.0, 20.5)],
            turn,
        ];
        let mut case = Case::new(4).part(2..4, &[rect(-5.0, -5.0, 20.0, 30.0)]);
        for pts in &lines {
            case = case.on(0..1, Role::Interface, pts);
        }
        let f = found(&case);
        assert_eq!(listed(&f), [("C5", 0); 2], "{f:?}");
        let by = worst(&f, "C5", 0);
        assert!(by.iter().all(|v| (v - W).abs() < 1e-6), "{by:?}");
    }

    /// A tube's returning turn may touch its earlier bead; closer parallel beads overlap.
    #[test]
    fn a_tube_turn_at_a_bead_is_clear_and_closer_overlaps() {
        let turn: Vec<_> = (0..96).map(|k| {
            let a = (std::f64::consts::TAU + 0.9) * k as f64 / 95.0;
            let radius = 3.0 + W / 2.0 - W * a / std::f64::consts::TAU;
            (radius * a.cos(), radius * a.sin())
        }).collect();
        let mut case = Case::new(1).on(0..1, Role::Body, &turn);
        // The helper makes bed paths Pad; this contract also applies to Body on layer zero.
        case.paths[0][0].role = Role::Body;
        let f = found(&case);
        assert!(worst(&f, "C5", 0).is_empty(), "a turn at one bead is clear: {f:?}");

        let mut case = Case::new(1)
            .on(0..1, Role::Body, &[(0.0, 0.0), (10.0, 0.0)])
            .on(0..1, Role::Body, &[(0.0, 0.8 * W), (10.0, 0.8 * W)]);
        for path in &mut case.paths[0] {
            path.role = Role::Body;
        }
        let f = found(&case);
        assert_eq!(listed(&f).iter().filter(|&&(rule, _)| rule == "C5").count(), 1, "{f:?}");
        let by = worst(&f, "C5", 0);
        assert!((by[0] - 0.2 * W).abs() < 1e-6, "by is the full overlap, without subtracting tolerance: {by:?}");
    }

    /// Two lines whose ends meet closer than a bead meet at a seam, which is clear; an end
    /// against another line's side is a fault.
    #[test]
    fn ends_meeting_are_a_seam_and_an_end_at_a_side_is_not() {
        let body = |case: Case| {
            let mut case = case;
            for path in &mut case.paths[0] {
                path.role = Role::Body;
            }
            found(&case)
        };
        let f = body(Case::new(1).on(0..1, Role::Body, &[(0.0, 0.0), (5.0, 0.0)]).on(0..1, Role::Body, &[(5.0 + 0.5 * W, 0.0), (10.0, 0.0), (10.0, 5.0)]));
        assert!(worst(&f, "C5", 0).is_empty(), "ends meeting are a seam: {f:?}");
        let f = body(Case::new(1).on(0..1, Role::Body, &[(0.0, 0.0), (10.0, 0.0)]).on(0..1, Role::Body, &[(5.0, 0.5 * W), (5.0, 5.0)]));
        assert_eq!(listed(&f).iter().filter(|&&(rule, _)| rule == "C5").count(), 1, "an end at a side: {f:?}");
    }

    /// A slab over a cup on the bed, from the bed only, interface under one end of it and paint
    /// blocking a strip: its underside is covered under the interface, blocked under the paint,
    /// unreachable over the cup, and infeasible elsewhere, each place once and all of it so.
    #[test]
    fn the_ledger_is_a_disjoint_union_of_the_demand() {
        let walls = difference_ex(&[rect(14.0, -2.0, 20.0, 12.0)], &[rect(15.0, -1.0, 19.0, 11.0)]);
        let mut case = Case::new(8).part(0..2, &[rect(14.0, -2.0, 20.0, 12.0)]).part(2..6, &walls).part(6..8, &[rect(0.0, 0.0, 20.0, 10.0)]);
        for k in 0..5 {
            let x = 0.5 + k as f64;
            case = case.on(4..5, Role::Interface, &[(x, 0.0), (x, 10.0)]);
        }
        case.p.build_plate_only = true;
        case.blocks = vec![Vec::new(); 8];
        case.blocks[6] = vec![rect(8.0, -1.0, 10.0, 11.0)];
        let mut demand = vec![Vec::new(); 8];
        demand[6] = vec![rect(0.0, 0.0, 14.0, 10.0), rect(15.0, 0.0, 19.0, 10.0)];
        let legal = case.legal();
        let reach = legal.column_reach(Ctl::default()).unwrap();
        let book = ledger(Ctl::default(), &legal, &demand, &case.paths, &reach).unwrap();
        let areas = book.areas();
        for (got, want) in areas.iter().zip([52.0, 20.0, 40.0, 68.0]) {
            assert!((got - want).abs() < 0.1, "covered, blocked, unreachable, infeasible: {areas:?}");
        }
        assert!((areas.iter().sum::<f64>() - 180.0).abs() < 0.05, "together the demand: {areas:?}");
        let causes = [&book.covered, &book.blocked, &book.unreachable, &book.infeasible];
        for (i, a) in causes.iter().enumerate() {
            for b in &causes[i + 1..] {
                let both: f64 = a.iter().zip(b.iter()).map(|(x, y)| if x.is_empty() || y.is_empty() { 0.0 } else { total_area_mm2(&intersection_ex(x, y)) }).sum();
                assert!(both < 1e-3, "causes {i} and another overlap by {both} mm2");
            }
        }
    }

    /// The hard checks, each failure with its place: a column under a cap's overhang that comes up
    /// to the top gap under it is a top that holds it; one that stops three layers lower is short
    /// by three; one that rises a layer into the gap is in it by one; one with nothing over it is
    /// a dead end, as is one under part of the cap that needs no support; and what the ledger
    /// finds a support could get to and nothing holds is unheld.
    #[test]
    fn hard_checks_name_dead_ends_short_tops_and_the_unheld_where_they_are() {
        let n = 50;
        // A cap 20 mm square on layers 40.. over a stem 4 mm square.
        let case = Case::new(n).part(0..40, &[rect(18.0, 18.0, 22.0, 22.0)]).part(40..n, &[rect(10.0, 10.0, 30.0, 30.0)]);
        let legal = case.legal();
        assert_eq!(legal.contact(40), Some(38), "a layer of top gap");
        // What needs support under the cap, less a patch the test says needs none.
        let mut demand = crate::support::demand(Ctl::default(), &case.slices, &case.heights, &case.p).unwrap();
        assert!(total_area_mm2(&demand[40]) > 300.0, "the cap's underside needs support");
        demand[40] = difference_ex(&demand[40], &[rect(13.0, 13.0, 17.0, 17.0)]);
        let column = |x: f64, y: f64, top: usize, layers: &mut Vec<SupportLayer>| {
            for layer in layers.iter_mut().take(top + 1) {
                layer.base.push(rect(x, y, x + 2.0, y + 2.0));
            }
        };
        let mut layers = vec![SupportLayer::default(); n];
        column(11.0, 11.0, 38, &mut layers); // holds the cap from the top gap under it
        column(26.0, 26.0, 35, &mut layers); // three layers short
        column(40.0, 40.0, 20, &mut layers); // nothing over it
        column(14.0, 14.0, 38, &mut layers); // under the patch that needs none
        column(17.0, 25.0, 39, &mut layers); // a layer into the gap
        let mut book = Ledger { infeasible: vec![Vec::new(); n], ..Default::default() };
        book.infeasible[40] = vec![rect(24.0, 12.0, 26.0, 14.0)];
        let mut found: Vec<(Hard, usize, (i64, i64), usize)> = hard(Ctl::default(), &legal, &layers, &demand, &book).unwrap().into_iter().map(|f| (f.check, f.layer, (f.at.0.round() as i64, f.at.1.round() as i64), f.by)).collect();
        found.sort();
        assert_eq!(found, [(Hard::DeadEnd, 20, (41, 41), 0), (Hard::DeadEnd, 38, (15, 15), 0), (Hard::ShortTop, 35, (27, 27), 3), (Hard::InGap, 39, (18, 26), 1), (Hard::Unheld, 40, (25, 13), 0)]);
    }
}

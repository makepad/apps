//! The rules a resin print keeps, in the order it prints, and the one check of them ([`check`]).
//! The supports are built to keep them.
//!
//! The print hangs from the plate. Each layer cures between the layer before it, on the plate's
//! side, and the film. Then the plate lifts and peels the layer off the film.
//!
//! - **Connected.** Every cured region of a layer shares cured material with the layer before
//!   it, part or support. A support's tip is in the layers, so a region a tip holds is connected
//!   through it. The first layer cures onto the plate. A region with no connection cures onto
//!   the film and is lost. One no support could reach says why ([`crate::supports::Dropped`]).
//! - **Clear.** No support goes through a part. Every head past its own root, every pillar, base
//!   and bridge keeps out of the parts.
//! - **Hung.** Every head hangs from a pillar, directly or by a bridge that meets the pillar.
//!   Every pillar ends on a pad, the plate's side.
//! - **Strong.** Every tip, pillar and bridge holds the peel it carries ([`crate::load`]): a
//!   head's share of its body's newest layer at its worst, a pillar what hangs on it and its push
//!   back down over its unbraced height, a bridge its head's share along it.
//!
//! A print that breaks one of those fails ([`Level::Error`]). The rest weigh how it peels:
//!
//! - **Cups.** No pocket opens toward the film and seals against it ([`crate::cups`]). Each lift
//!   pulls on it like a suction cup: a warning from a square centimetre sealed, an error past
//!   five. A pocket the layers close over traps resin: a warning.
//! - **Detail.** Every lit feature of a part cures somewhere ([`crate::parts::Lost`]). One thinner
//!   than about half a pixel is lit too dimly to cure, and is lost: worth knowing.
//! - **Peel.** No layer peels over 35 % of the screen at once: a warning. A layer that peels more
//!   than twice the one before, by a square centimetre or more, is worth knowing. The first
//!   layer, the print's hold on the plate, is at least a quarter of the most any layer peels: a
//!   warning under it.

use crate::clear::{box_distance, clearance, Clear, Probe};
use crate::cups::{cups, CUP_LEAST, SUCTION, TRAPPED_LEAST};
use crate::grid::PixelRect;
use crate::image::LayerStack;
use crate::load::Bodies;
use crate::parts::{links, over, LayerParts};
use crate::section::Section;
use crate::solid::{add, lerp, sub, V3};
use crate::supports::{head_probe, head_shares, Dropped, Supports};
use makepad_fab_slicer::par;
use std::borrow::Cow;
use std::sync::Arc;

/// Which rule a break breaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rule {
    Connected,
    Clear,
    Hung,
    Strong,
    Cup,
    Lost,
    Peel,
    Jump,
    Bond,
}

/// How much a break matters: the print fails on it, may fail on it, or prints with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Level {
    Error,
    Warning,
    Info,
}

/// A pocket that seals more than this fails the print (mm²).
pub const CUP_FAILS: f64 = 500.0;
/// The share of the screen a layer may peel at once.
pub const PEEL_SHARE: f64 = 0.35;
/// A layer that peels this many times the one before, by [`JUMP_LEAST`] mm² or more, is named.
pub const JUMP: f64 = 2.0;
pub const JUMP_LEAST: f64 = 100.0;
/// The least share of the most any layer peels the first layer holds to the plate with.
pub const BOND: f64 = 0.25;

/// A place the print breaks a rule: the layer it first does it on, where (printer mm), and the
/// part it is in or holds (its place among the parts; by their boxes, the nearest).
#[derive(Clone, Debug, PartialEq)]
pub struct Break {
    pub rule: Rule,
    pub level: Level,
    pub layer: u32,
    /// Its last layer, when it runs over more than one.
    pub to: Option<u32>,
    pub at: V3,
    /// Its footprint's width and depth around `at` (mm).
    pub size: [f64; 2],
    pub part: Option<usize>,
    /// What breaks it, in a few words.
    pub what: String,
}

/// Every break of the rules in `stack`: its first `parts` sections are the parts, as they stand on
/// `supports` (lifted), and the supports follow them. `cured`: the parts' cured pixels in each
/// layer ([`crate::parts::cured`] of the parts alone). Sorted by layer; none once `stop` says so.
pub fn check(stack: &LayerStack, parts: usize, supports: &Supports, cured: &[LayerParts], stop: &(dyn Fn() -> bool + Sync)) -> Vec<Break> {
    checked(stack, parts, supports, cured, stop).breaks
}

/// The check's breaks, and what it learns of the supports on the way.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Checked {
    pub breaks: Vec<Break>,
    /// What the supports light where the parts don't cure, over every layer, each pixel by its
    /// share: their own resin, in pixels a layer thick ([`crate::estimate::resin_ml`]).
    pub support_covered: f64,
}

/// [`check`], with the supports' own covered pixels.
pub fn checked(stack: &LayerStack, parts: usize, supports: &Supports, cured: &[LayerParts], stop: &(dyn Fn() -> bool + Sync)) -> Checked {
    let shown = &stack.sections[..parts];
    let (layers, support_covered) = joined(stack, parts, cured, stop);
    if stop() {
        return Checked::default();
    }
    let mut breaks = connected(stack, shown, &layers, supports, stop);
    if stop() {
        return Checked::default();
    }
    breaks.extend(clear(stack, shown, supports));
    breaks.extend(hung(stack, shown, supports));
    breaks.extend(strong(stack, supports, cured));
    breaks.extend(cupped(stack, shown, &layers));
    breaks.extend(lost(stack, shown, cured));
    breaks.extend(peel(stack, shown, supports, &layers));
    breaks.sort_by_key(|b| b.layer);
    Checked { breaks, support_covered }
}

/// The whole print's cured pixels in each layer: the parts' (`cured`) with the supports' cut
/// (`stack`'s sections past the first `parts`) over them; and what the supports light where the
/// parts don't cure, over every layer (pixels, each by its share).
fn joined<'a>(stack: &LayerStack, parts: usize, cured: &'a [LayerParts], stop: &(dyn Fn() -> bool + Sync)) -> (Cow<'a, [LayerParts]>, f64) {
    let n = stack.layers.count as usize;
    assert_eq!(cured.len(), n, "the parts' cured pixels in each layer");
    let supports = LayerStack { sections: stack.sections[parts..].to_vec(), ..stack.clone() };
    if supports.sections.is_empty() {
        return (Cow::Borrowed(cured), 0.0);
    }
    let (layers, own): (Vec<LayerParts>, Vec<f64>) = par::map(n, |k| {
        if stop() {
            return (LayerParts::default(), 0.0);
        }
        let spans = supports.layer(k as u32).spans;
        (cured[k].union(&LayerParts::of(&spans)), cured[k].outside(&spans))
    })
    .into_iter()
    .unzip();
    (Cow::Owned(layers), own.iter().sum())
}

/// `rect` (pixels) on `stack`'s layer `k`: its middle (printer mm) and its width and depth.
fn placed(stack: &LayerStack, rect: &PixelRect, k: u32) -> (V3, [f64; 2]) {
    let lo = stack.grid.at(rect.x0 as f64, rect.y0 as f64);
    let hi = stack.grid.at(rect.x1 as f64, rect.y1 as f64);
    ([0.5 * (lo[0] + hi[0]), 0.5 * (lo[1] + hi[1]), stack.layers.z(k)], [(hi[0] - lo[0]).abs(), (hi[1] - lo[1]).abs()])
}

/// The part whose box is nearest `at`.
fn near(parts: &[Arc<dyn Section>], at: V3) -> Option<usize> {
    (0..parts.len()).min_by(|&a, &b| box_distance(&parts[a].bounds(), at).total_cmp(&box_distance(&parts[b].bounds(), at)))
}

/// Connected: each region of each layer past the first, on something of the layer before
/// (`layers`: the whole print's). A region with a point of the supports' in it that no support
/// reaches says why.
fn connected(stack: &LayerStack, parts: &[Arc<dyn Section>], layers: &[LayerParts], sup: &Supports, stop: &(dyn Fn() -> bool + Sync)) -> Vec<Break> {
    let found: Vec<Vec<Break>> = par::map(layers.len(), |k| {
        if k == 0 || stop() {
            return Vec::new();
        }
        let on = links(&layers[k - 1], &layers[k]);
        let grid = &stack.grid;
        layers[k]
            .parts
            .iter()
            .zip(&on)
            .filter(|(_, on)| on.is_empty())
            .map(|(part, _)| {
                let (at, size) = placed(stack, &part.rect, k as u32);
                // one too small to show in hundredths of a mm² by its pixels (it has one at least)
                let area = part.pixels as f64 * grid.pixel_area();
                let how_big = if area < 0.005 { format!("{}-pixel", part.pixels) } else { format!("{area:.2} mm²") };
                let mut what = format!("a {how_big} island cures onto the film");
                let unreached = sup.dropped.iter().filter(|d| d.1 != Dropped::Twin).find(|d| {
                    let p = &sup.points[d.0];
                    let [x, y] = grid.to_pixels([p.at[0], p.at[1]]);
                    p.layer == k as u32 && part.runs.iter().any(|r| r.y == y as u32 && (r.x0..r.x1).contains(&(x as u32)))
                });
                if let Some(d) = unreached {
                    what += &format!(": no support can reach it ({})", d.1.why());
                }
                Break { rule: Rule::Connected, level: Level::Error, layer: k as u32, to: None, at, size, part: near(parts, at), what }
            })
            .collect()
    });
    found.into_iter().flatten().collect()
}

/// How far under its discs a flat-ended solid's covering balls sit (mm).
const BELOW: f64 = 10.0;

/// Clear: every strut, and every head from its root on, against the parts with no gap.
fn clear(stack: &LayerStack, parts: &[Arc<dyn Section>], sup: &Supports) -> Vec<Break> {
    let s = &sup.settings;
    let held: Vec<usize> = sup.pillars.iter().flat_map(|p| p.heads.iter().copied()).collect();
    // each probe, what it is, its width, and how far under the solid it covers it runs
    let mut probes: Vec<(Probe, &'static str, f64, f64)> = Vec::new();
    for &h in &held {
        let head = &sup.heads[h];
        if let Some(p) = head_probe(head.at, head.normal, head.axis, head.tip_radius, head.span(), s, 0.0) {
            probes.push((p, "a head", 2.0 * s.pillar_radius, 0.0));
        }
    }
    for p in &sup.pillars {
        let foot = s.foot(p.top);
        probes.push((Probe::even(p.top, foot, p.radius, 0.0), "a pillar", 2.0 * p.radius, 0.0));
        // the base's cut cone, as balls whose centres sit BELOW under each of its discs: each
        // ball holds its disc and rises at most r² / 2 BELOW over it (the balls' radii, linear
        // between the ends, hold every disc between: the radius over the centre is convex in the
        // disc's)
        let ball = |z: f64, r: f64| ([foot[0], foot[1], z - BELOW], (r * r + BELOW * BELOW).sqrt());
        let ((a, ra), (b, rb)) = (ball(s.ground(), s.base_radius), ball(foot[2], p.radius));
        probes.push((Probe { a, b, ra, rb, ga: 0.0, gb: 0.0 }, "a pillar's base", 2.0 * s.base_radius, BELOW));
    }
    for b in &sup.bridges {
        probes.push((Probe::even(sup.heads[b.head].junction, b.at, b.radius, 0.0), "a bridge", 2.0 * b.radius, 0.0));
    }
    let shown: Vec<Probe> = probes.iter().map(|p| p.0).collect();
    clearance(parts, &shown)
        .into_iter()
        .zip(&probes)
        .filter_map(|(c, &(p, what, width, below))| match c {
            Clear::Blocked { t } => {
                let at = add(lerp(p.a, p.b, t), [0.0, 0.0, below]);
                Some(Break { rule: Rule::Clear, level: Level::Error, layer: stack.layers.at(at[2]), to: None, at, size: [width; 2], part: near(parts, at), what: format!("{what} goes into a part") })
            }
            Clear::Clear { .. } => None,
        })
        .collect()
}

/// Hung: each held head on its pillar or a bridge to it, each pillar's base on a pad.
fn hung(stack: &LayerStack, parts: &[Arc<dyn Section>], sup: &Supports) -> Vec<Break> {
    let s = &sup.settings;
    let mut breaks = Vec::new();
    let at_layer = |at: V3| stack.layers.at(at[2]);
    for (k, p) in sup.pillars.iter().enumerate() {
        for &h in &p.heads {
            let head = &sup.heads[h];
            let own = sub(head.junction, p.top).iter().all(|d| d.abs() < 1e-9);
            let bridged = sup.bridges.iter().any(|b| {
                let at = b.at;
                b.head == h && b.pillar == k && (at[0] - p.top[0]).hypot(at[1] - p.top[1]) < 1e-9 && at[2] >= s.foot(p.top)[2] - 1e-9 && at[2] <= p.top[2] + 1e-9
            });
            if !own && !bridged {
                let size = [2.0 * p.radius; 2];
                breaks.push(Break { rule: Rule::Hung, level: Level::Error, layer: at_layer(head.junction), to: None, at: head.junction, size, part: near(parts, head.tip), what: "a head meets no pillar".into() });
            }
        }
        let foot = [p.top[0], p.top[1]];
        let on_pad = sup.pads.iter().any(|pad| (pad.z[1] - s.ground()).abs() < 1e-9 && pad.outline_distance(foot) <= -s.base_radius + 1e-6);
        if !on_pad {
            let at = [foot[0], foot[1], s.ground()];
            breaks.push(Break { rule: Rule::Hung, level: Level::Error, layer: at_layer(at), to: None, at, size: [2.0 * s.base_radius; 2], part: near(parts, p.top), what: "a pillar's base is off the pad".into() });
        }
    }
    breaks
}

/// Strong: each held head's tip holds its share of the peel, leaning as it does; each pillar
/// holds what hangs on it and stands its unbraced height under the push back down; each bridge
/// holds its head's share along it. The shares are the held heads' alone, on the parts' layers
/// `cured`: a dropped point's area falls on its neighbours. A break is on the layer where its
/// load is the most.
fn strong(stack: &LayerStack, sup: &Supports, cured: &[LayerParts]) -> Vec<Break> {
    let (s, load) = (&sup.settings, &sup.load);
    let held: Vec<usize> = sup.pillars.iter().flat_map(|p| p.heads.iter().copied()).collect();
    let shares = head_shares(&Bodies::new(cured, &stack.grid), cured, &stack.grid, &sup.heads, &held);
    let mut breaks = Vec::new();
    // a little over what was sized: the sizes are the loads' own, rounded
    let over = |need: f64, has: f64| need > has * (1.0 + 1e-6);
    for &h in &held {
        let (head, (share, layer)) = (&sup.heads[h], shares[h]);
        let d = 2.0 * head.tip_radius;
        if over(load.tip_for(share, head.lean()), d) {
            let what = format!("a {d:.2} mm tip holds {:.0} mm² of peel, and from here it carries {share:.0} mm²", load.tip_holds(d, head.lean()));
            breaks.push(Break { rule: Rule::Strong, level: Level::Error, layer, to: None, at: head.tip, size: [d.max(1.0); 2], part: Some(head.part), what });
        }
    }
    for p in &sup.pillars {
        let hang: f64 = p.heads.iter().map(|&h| shares[h].0).sum();
        let layer = p.heads.iter().max_by(|&&a, &&b| shares[a].0.total_cmp(&shares[b].0)).map_or(0, |&h| shares[h].1);
        let length = p.top[2] - s.foot(p.top)[2];
        let (need, d) = (load.pillar_for(hang, length), 2.0 * p.radius);
        if over(need, d) {
            let at = lerp(p.top, s.foot(p.top), 0.5);
            let what = format!("a {d:.2} mm pillar {length:.0} mm tall holds less than the {hang:.0} mm² of peel hanging on it: it needs {need:.2} mm");
            breaks.push(Break { rule: Rule::Strong, level: Level::Error, layer, to: None, at, size: [2.0 * s.base_radius; 2], part: Some(sup.heads[p.heads[0]].part), what });
        }
    }
    for b in &sup.bridges {
        let (share, layer) = shares[b.head];
        let (need, d) = (load.bridge_for(share, b.rise(&sup.heads)), 2.0 * b.radius);
        if over(need, d) {
            let at = lerp(sup.heads[b.head].junction, b.at, 0.5);
            let what = format!("a {d:.2} mm bridge holds less than its head's {share:.0} mm² of peel: it needs {need:.2} mm");
            breaks.push(Break { rule: Rule::Strong, level: Level::Error, layer, to: None, at, size: [d.max(1.0); 2], part: Some(sup.heads[b.head].part), what });
        }
    }
    breaks
}

/// Cups: each pocket of the whole print (`layers`) that seals [`CUP_LEAST`] against the film or
/// traps [`TRAPPED_LEAST`] of resin, from its first layer to its last; where it seals the most.
fn cupped(stack: &LayerStack, parts: &[Arc<dyn Section>], layers: &[LayerParts]) -> Vec<Break> {
    cups(layers, &stack.grid, &stack.layers)
        .into_iter()
        .map(|c| {
            let (at, size) = placed(stack, &c.rect, c.peak);
            let mut what = Vec::new();
            if c.area >= CUP_LEAST {
                what.push(format!("a pocket seals {:.1} cm² against the film, a pull of about {:.0} N", c.area / 100.0, c.area * SUCTION));
            }
            if c.trapped >= TRAPPED_LEAST {
                what.push(format!("the layers close over a pocket on layer {}, trapping {:.2} ml of resin", c.last + 1, c.trapped / 1000.0));
            }
            let level = if c.area > CUP_FAILS { Level::Error } else { Level::Warning };
            Break { rule: Rule::Cup, level, layer: c.first, to: (c.last > c.first).then_some(c.last), at, size, part: near(parts, at), what: what.join("; ") }
        })
        .collect()
}

/// Detail: each feature of the parts (`cured`: their layers alone) lit too dimly to cure, over
/// the layers it runs through (features of neighbouring layers that touch are one); its most on
/// one layer.
fn lost(stack: &LayerStack, parts: &[Arc<dyn Section>], cured: &[LayerParts]) -> Vec<Break> {
    struct Run {
        first: u32,
        last: u32,
        rect: PixelRect,
        /// On its last layer.
        top: PixelRect,
        most: f64,
    }
    let grow = |r: &PixelRect| PixelRect { x0: r.x0.saturating_sub(1), y0: r.y0.saturating_sub(1), x1: r.x1 + 1, y1: r.y1 + 1 };
    let mut runs: Vec<Run> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    for (k, layer) in cured.iter().enumerate() {
        let k = k as u32;
        let mut now = Vec::new();
        for f in &layer.lost {
            let near = open.iter().copied().find(|&i| !grow(&runs[i].top).intersect(&f.rect).is_empty());
            match near {
                Some(i) if runs[i].last + 1 >= k => {
                    let r = &mut runs[i];
                    if r.last == k {
                        r.top = r.top.union(&f.rect);
                    } else {
                        r.top = f.rect;
                    }
                    r.last = k;
                    r.rect = r.rect.union(&f.rect);
                    r.most = r.most.max(f.covered);
                    if !now.contains(&i) {
                        now.push(i);
                    }
                }
                _ => {
                    now.push(runs.len());
                    runs.push(Run { first: k, last: k, rect: f.rect, top: f.rect, most: f.covered });
                }
            }
        }
        open = now;
    }
    let px = stack.grid.pixel_area();
    runs.iter()
        .map(|r| {
            let (at, size) = placed(stack, &r.rect, r.first);
            let what = format!("a feature thinner than a pixel is lit too dimly to cure: {:.2} mm² of it is lost on its widest layer", r.most * px);
            Break { rule: Rule::Lost, level: Level::Info, layer: r.first, to: (r.last > r.first).then_some(r.last), at, size, part: near(parts, at), what }
        })
        .collect()
}

/// Peel: the layer of the whole print (`layers`) that peels the most, over [`PEEL_SHARE`] of the
/// screen (said so when it is the pad's, of `sup`); each layer that peels [`JUMP`] times the one
/// before; and the first layer's hold on the plate, under [`BOND`] of the most.
fn peel(stack: &LayerStack, parts: &[Arc<dyn Section>], sup: &Supports, layers: &[LayerParts]) -> Vec<Break> {
    let grid = &stack.grid;
    let px = grid.pixel_area();
    let area: Vec<f64> = layers.iter().map(|l| l.pixels() as f64 * px).collect();
    let Some(peak) = (0..area.len()).max_by(|&a, &b| area[a].total_cmp(&area[b]).then(b.cmp(&a))) else {
        return Vec::new();
    };
    let whole = |k: usize| layers[k].parts.iter().fold(PixelRect::EMPTY, |r, p| r.union(&p.rect));
    let mut breaks = Vec::new();
    let screen = grid.pixels[0] as f64 * grid.pixels[1] as f64 * px;
    if area[peak] > PEEL_SHARE * screen {
        let (at, size) = placed(stack, &whole(peak), peak as u32);
        let z = stack.layers.z(peak as u32);
        let pad = if sup.pads.iter().any(|p| z <= p.z[1]) { "this layer of the pad" } else { "this layer" };
        let what = format!("{pad} peels {:.0} mm², {:.0} % of the screen, at once", area[peak], 100.0 * area[peak] / screen);
        breaks.push(Break { rule: Rule::Peel, level: Level::Warning, layer: peak as u32, to: None, at, size, part: None, what });
    }
    for k in 1..area.len() {
        if area[k] > JUMP * area[k - 1] && area[k] - area[k - 1] >= JUMP_LEAST {
            // where the new area is
            let new = layers[k].parts.iter().filter(|p| !over(p, &layers[k - 1]).is_empty()).fold(PixelRect::EMPTY, |r, p| r.union(&p.rect));
            let (at, size) = placed(stack, &new, k as u32);
            let what = format!("the peel jumps from {:.0} mm² to {:.0} mm² on this layer", area[k - 1], area[k]);
            breaks.push(Break { rule: Rule::Jump, level: Level::Info, layer: k as u32, to: None, at, size, part: near(parts, at), what });
        }
    }
    if area[0] < BOND * area[peak] {
        // where the first layer is, or what pulls on it when there is none
        let first = whole(0);
        let rect = if first.is_empty() { whole(peak) } else { first };
        let (at, size) = placed(stack, &rect, 0);
        let what = format!("the first layer holds {:.0} mm² to the plate, under a quarter of the {:.0} mm² layer {peak} peels", area[0], area[peak]);
        breaks.push(Break { rule: Rule::Bond, level: Level::Warning, layer: 0, to: None, at, size, part: None, what });
    }
    breaks
}

//! Overhangs (R14 §2): the faces tips go under, found the way the reference's own detector
//! finds them.
//!
//! 0. The slices (§2.0, [`slices`]): each layer's cut loops united, closed by 0.049 mm and
//!    simplified island by island at 0.0025 mm, as every stage of the tree reads them.
//! 1. Extrudable slices (§2.1): each layer less what is narrower than a line, unless it lies
//!    within a line of a wider part.
//! 2. The raw overhang (§2.2): what of a layer the layer below, grown by the threshold angle,
//!    does not carry, as pieces.
//! 3. Sharp tails, first pass (§2.3): pieces that start in mid-air, and small feet on layer 0.
//! 4. Cantilevers (§2.4): pieces with a ring floating free of the layer below, or reaching more
//!    than 3 mm past what holds them.
//! 5. Sharp tails, second pass (§2.5): a piece standing on a tail continues it, for at most
//!    16 mm.
//! 6. Clusters (§2.6): pieces grouped up the layers by overlap.
//! 7. Small-overhang removal (§2.7), when the keys ask for it: small clusters go, unless they
//!    are tails, cantilevers, or deep fragments reaching past everything printed under them; and,
//!    Fab's departure, unless Fab's own overhang measure counts them ([`Cluster::counted`]).
//! 8. The piece guard (§2.8): a layer of over 100 pieces skips its tail and cantilever tests and
//!    turns off the second pass and removal.
//! 9. The output (§2.9): kept cluster pieces on their own layers, tail pieces every five tenths
//!    of a millimetre of the tail's height, less what is blocked, appended without a union; only
//!    polygons that are exactly a sharp tail grow by 0.2 mm.
//!
//! A run after another on the same keys takes from it what reads only what is the same in both
//! ([`Before`]): a layer's slice (§2.0) when its cut loops are the same, its extrudable slice and
//! first pass (§2.1 to §2.4) when its slice and the two under it, the height under it and its
//! enforced areas are, and the part below a first layer (§2.7) as high up as the slices are.

use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::Join;

use super::clip::{difference_ex, intersection_ex, union, union2_ex, union_ex, FillRule};
use super::element::island_simplified;
use super::offset::{offset, ring_by_ring};
use super::settings::Settings;
use crate::par::{self, Cancelled, Ctl};

/// The joins of every offset R14 §2 does not give another for.
const MITRE: Join = Join::Miter(3.0);
/// The slices are closed by this (mm), out and back (§2.0)...
const SLICE_CLOSING: f64 = 0.049;
/// ...and simplified at this (mm), island by island.
const SLICE_SIMPLIFY: f64 = 0.0025;
/// The threshold angle the reference uses for a threshold of 0 (degrees).
const ZERO_THRESHOLD: f64 = 30.0;
/// The highest threshold angle (degrees).
const MAX_THRESHOLD: f64 = 89.0;
/// A foot on layer 0, and the overlap a tail chain keeps with the tails under it, is at most
/// this (mm) in x and y.
const TAIL_BOX: f64 = 6.0;
/// A floating tail is smaller than this (mm²), in area and (or else thin) in box area.
const TAIL_AREA: f64 = 100.0;
/// A floating tail touches nothing below when grown by this many lines, and survives a shrink of
/// it.
const TAIL_GAP: f64 = 0.1;
/// A floating tail with a large box vanishes when shrunk by this many lines.
const TAIL_THIN: f64 = 2.5;
/// A tail chain stops where its new part's box grows by more than this (mm)...
const TAIL_FAST_BOX: f64 = 5.0;
/// ...or survives a shrink of this many lines.
const TAIL_FAST_SHRINK: f64 = 5.0;
/// The tallest a tail chain gets (mm).
const TAIL_HEIGHT: f32 = 16.0;
/// A piece reaching further than this (mm) past what holds it is a cantilever.
const CANTILEVER: f64 = 3.0;
/// What holds a piece reaches this far (mm) past a line out from the layer below.
const CANTILEVER_REACH: f64 = 0.1;
/// A layer with more raw pieces than this trips the guard.
const GUARD_PIECES: usize = 100;
/// A thin, short cluster's average area is under this (mm²)...
const SMALL_AREA: f64 = 4.0;
/// ...and it is under this many layers tall.
const SMALL_LAYERS: usize = 3;
/// A narrow cluster's box is under this (mm) in x and y.
const NARROW_BOX: f64 = 2.0;
/// The lines a narrow cluster vanishes when shrunk by.
const NARROW_SHRINK: f64 = 2.5;
/// A cluster over this merged area (mm²) is big and flat.
const BIG_FLAT: f64 = 100.0;
/// A thin plate's outline box is over this (mm) in x or y...
const PLATE_BOX: f64 = 10.0;
/// ...and it starts under this layer.
const PLATE_LAYERS: usize = 20;
/// The slices a deep fragment is tested against are simplified at this (mm)...
const DEEP_SIMPLIFY: f64 = 0.2;
/// ...and grown by this many lines.
const DEEP_GROW: f64 = 2.5;
/// A tail piece is what sticks out past the layer below grown by this (mm).
const TAIL_OUT_GAP: f64 = 0.0125;
/// Blockers grow by this (mm) before they are taken out of the output.
const BLOCK_GROW: f64 = 0.2;
/// Enforcers grow by this (mm) before they join the raw overhang.
const ENFORCE_GROW: f64 = 0.8;
/// A polygon that is exactly a sharp tail grows by this (mm) in the output.
const TAIL_GROW: f64 = 0.2;

/// The overhangs of a part.
#[derive(Clone, Debug, Default)]
pub struct Overhangs {
    /// Per layer, the polygons tips go under, appended without a union: overlaps and duplicates
    /// stay. Layers 0 to 2 are never read (§2.9).
    pub layers: Vec<Vec<ExPolygon>>,
    /// The largest cantilever distance (mm), as the reference logs it.
    pub largest_cantilever: f64,
    /// Each cantilever cluster's layer and distance (mm), as the reference logs them: the first
    /// cantilever piece the cluster overlaps, bottom up.
    pub cantilevers: Vec<(usize, f64)>,
    /// Whether the piece guard tripped (§2.8).
    pub tripped: bool,
}

impl Overhangs {
    /// The layers with at least one overhang: the count the reference logs.
    pub fn layers_with_overhang(&self) -> usize {
        self.layers.iter().filter(|l| !l.is_empty()).count()
    }
}

/// What §2 found that a run after may take ([`Before`]): each layer's first pass, and the part
/// below each first layer §2.7 asked for ([`part_below`]).
#[derive(Default)]
pub struct Passes {
    layers: Vec<Layer>,
    below: Vec<(usize, Vec<ExPolygon>)>,
    /// The first passes it made rather than took ([`super::Work::faced`])...
    pub(super) passed: usize,
    /// ...and the layers it added to the part below.
    pub(super) added: usize,
}

/// The run before's §2 on the same keys, for a run after to take from ([`detect`]): the slices
/// as §2.0 left them, the heights and enforced areas it read, and what it found.
pub struct Before<'a> {
    pub slices: &'a [Vec<ExPolygon>],
    pub heights: &'a [(f64, f64)],
    pub enforce: &'a [Vec<ExPolygon>],
    pub passes: &'a Passes,
}

/// A sharp tail: an extrudable piece and the height (mm) of the chain it ends, in 32-bit floats
/// as the reference keeps it.
#[derive(Clone, Debug)]
struct Tail {
    piece: ExPolygon,
    height: f32,
}

/// What the first pass finds on one layer.
#[derive(Clone)]
struct Layer {
    /// The extrudable slice's pieces.
    extrudable: Vec<ExPolygon>,
    /// The raw overhang's pieces, enforcers joined: what clusters are made of.
    raw: Vec<ExPolygon>,
    /// The enforced areas, grown.
    enforced: Vec<ExPolygon>,
    /// The first pass's tails.
    tails: Vec<Tail>,
    /// The cantilever pieces of the raw overhang, with their distances (mm).
    cantilevers: Vec<(ExPolygon, f64)>,
    /// The largest cantilever distance (mm).
    largest: f64,
    /// More raw pieces than the guard allows.
    tripped: bool,
}

/// A cluster's type (§2.7): a set of flags. Only a cluster that is exactly a sharp tail grows in
/// the output.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Type {
    tail: bool,
    cantilever: bool,
    big_flat: bool,
    thin_plate: bool,
}

impl Type {
    const TAIL: Type = Type { tail: true, cantilever: false, big_flat: false, thin_plate: false };
}

/// Pieces grouped up the layers (§2.6).
struct Cluster {
    /// Its first layer.
    first: usize,
    /// Its pieces per layer, from the first.
    layers: Vec<Vec<ExPolygon>>,
    /// A later piece tested it for joining: thin-plate shrinks the part below by a line.
    reached: bool,
    /// Fab's departure from §2.7: one of its pieces lies in a group Fab's own overhang measure
    /// counts ([`crate::support::small_groups`]), so removal never takes it. The reference's
    /// clusters never merge, so the first layers of a face that widens up into a kept one are a
    /// cluster of their own, often narrow: dropped, they would print over air.
    counted: bool,
}

impl Cluster {
    /// Its pieces on layer `l`.
    fn on(&self, l: usize) -> &[ExPolygon] {
        l.checked_sub(self.first).and_then(|k| self.layers.get(k)).map_or(&[], Vec::as_slice)
    }
}

fn grow(set: &[ExPolygon], mm: f64, join: Join) -> Vec<ExPolygon> {
    if set.is_empty() {
        return Vec::new();
    }
    offset(set, mm * SCALE, join)
}

/// A box's size (mm).
fn size(b: &BBox) -> (f64, f64) {
    if b.is_empty() {
        return (0.0, 0.0);
    }
    ((b.max.x - b.min.x) as f64 / SCALE, (b.max.y - b.min.y) as f64 / SCALE)
}

/// Whether two sets overlap (§2.10): they share area, however little.
fn meets(a: &[ExPolygon], b: &[ExPolygon]) -> bool {
    !a.is_empty() && !b.is_empty() && bbox_of(a).overlaps(&bbox_of(b)) && !intersection_ex(a, b).is_empty()
}

/// Whether `p` lies in `set`.
fn inside(set: &[ExPolygon], p: Pt) -> bool {
    set.iter().any(|e| {
        let b = e.bbox();
        p.x >= b.min.x && p.x <= b.max.x && p.y >= b.min.y && p.y <= b.max.y && e.contains(p)
    })
}

/// The distance (mm) from `p` to the nearest ring of `set`, infinite for an empty set.
fn dist_to(set: &[ExPolygon], p: Pt) -> f64 {
    let mut best = f64::INFINITY;
    for r in set.iter().flat_map(|e| e.rings()) {
        let n = r.pts.len();
        for i in 0..n {
            best = best.min(seg_dist(r.pts[i], r.pts[(i + 1) % n], p));
        }
    }
    best / SCALE
}

/// The distance (units) from `p` to segment `a`-`b`.
fn seg_dist(a: Pt, b: Pt, p: Pt) -> f64 {
    let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let (px, py) = ((p.x - a.x) as f64, (p.y - a.y) as f64);
    let l2 = dx * dx + dy * dy;
    let t = if l2 == 0.0 { 0.0 } else { ((px * dx + py * dy) / l2).clamp(0.0, 1.0) };
    (px - t * dx).hypot(py - t * dy)
}

/// The threshold angle (radians): the key plus 1°, at most 89°; a key of 0 is 30° (§2.2).
pub fn threshold(key_deg: f64) -> f64 {
    if key_deg <= 0.0 {
        ZERO_THRESHOLD.to_radians()
    } else {
        (key_deg + 1.0).min(MAX_THRESHOLD).to_radians()
    }
}

/// The extrudable slice (§2.1): the slice less what is narrower than `w` (mm), unless it lies
/// within `w` of a wider part. The reference's `w` is a line; Fab's is the narrowest the walls
/// print ([`super::settings::Keys::narrowest`]).
fn extrudable(slice: &[ExPolygon], w: f64) -> Vec<ExPolygon> {
    intersection_ex(slice, &grow(&grow(slice, -w / 2.0, MITRE), w, MITRE))
}

/// Whether extrudable piece `p` of a layer over 0 is a floating tail (§2.3): grown by a tenth
/// of a line it touches nothing of the two layers below, it is under 100 mm² (and its box too,
/// or it is thin), and it survives a shrink of a tenth of a line.
fn floating_tail(p: &ExPolygon, w: f64, below: &[ExPolygon], below2: Option<&[ExPolygon]>) -> bool {
    let one = std::slice::from_ref(p);
    if p.area_mm2() >= TAIL_AREA {
        return false;
    }
    let (bw, bh) = size(&p.bbox());
    if bw * bh >= TAIL_AREA && !grow(one, -TAIL_THIN * w, MITRE).is_empty() {
        return false;
    }
    if grow(one, -TAIL_GAP * w, MITRE).is_empty() {
        return false;
    }
    let g = grow(one, TAIL_GAP * w, MITRE);
    !meets(&g, below) && below2.is_none_or(|b| !meets(&g, b))
}

/// Raw piece `p`'s cantilever distance (mm), none when it is not one (§2.4).
/// - `near`: the layer below grown by a line, square.
/// - `held`: the layer below grown by the threshold offset, then to a line and 0.1 mm past it,
///   mitred.
fn cantilever(p: &ExPolygon, near: &[ExPolygon], held: &[ExPolygon]) -> Option<f64> {
    let floating = p.rings().filter(|r| !r.pts.iter().any(|&q| inside(near, q))).map(|r| r.pts.iter().map(|&q| dist_to(near, q)).fold(0.0, f64::max)).reduce(f64::max);
    if floating.is_some() {
        return floating;
    }
    let by = intersection_ex(std::slice::from_ref(p), held);
    let far = p.contour.pts.iter().map(|&q| dist_to(&by, q)).fold(0.0, f64::max);
    (far > CANTILEVER).then_some(far)
}

/// The first pass on layer `l` (§2.2–§2.4, §2.8).
fn first_pass(l: usize, extr: &[Vec<ExPolygon>], heights: &[(f64, f64)], w: f64, theta: f64, enforce: Option<&Vec<ExPolygon>>) -> Layer {
    let extrudable = extr[l].clone();
    let enforced = enforce.map_or_else(Vec::new, |e| grow(e, ENFORCE_GROW, MITRE));
    let mut layer = Layer { extrudable, raw: Vec::new(), enforced, tails: Vec::new(), cantilevers: Vec::new(), largest: 0.0, tripped: false };
    if l == 0 {
        // Small feet, whatever the guard: pieces of layer 0 no more than the tail box in x and y.
        for p in &layer.extrudable {
            let (bw, bh) = size(&p.bbox());
            if bw <= TAIL_BOX && bh <= TAIL_BOX {
                layer.tails.push(Tail { piece: p.clone(), height: heights[0].1 as f32 });
            }
        }
        layer.raw = union_ex(&layer.enforced);
        return layer;
    }
    let below = &extr[l - 1];
    let off = heights[l - 1].1 / theta.tan();
    let carried = grow(below, off, Join::Square);
    let raw = difference_ex(&layer.extrudable, &carried);
    layer.raw = if layer.enforced.is_empty() { raw } else { union2_ex(&raw, &layer.enforced) };
    layer.tripped = layer.raw.len() > GUARD_PIECES;
    if layer.tripped {
        return layer;
    }
    let below2 = l.checked_sub(2).map(|k| extr[k].as_slice());
    for p in &layer.extrudable {
        if floating_tail(p, w, below, below2) {
            layer.tails.push(Tail { piece: p.clone(), height: 0.0 });
        }
    }
    let near = grow(below, w, Join::Square);
    let held = grow(&carried, (w - off).max(0.0) + CANTILEVER_REACH, MITRE);
    for p in &layer.raw {
        if let Some(d) = cantilever(p, &near, &held) {
            layer.cantilevers.push((p.clone(), d));
            layer.largest = layer.largest.max(d);
        }
    }
    layer
}

/// The second pass on layer `l` (§2.5): the extrudable pieces that continue a tail of the layer
/// below. A pass-1 tail touches nothing of the layer below, so it never qualifies.
fn second_pass(layer: &Layer, lower: &[Tail], h: f32, w: f64) -> Vec<Tail> {
    if lower.is_empty() {
        return Vec::new();
    }
    let tails: Vec<ExPolygon> = lower.iter().map(|t| t.piece.clone()).collect();
    let tails_box = bbox_of(&tails);
    let (tw, th) = size(&tails_box);
    let mut out = Vec::new();
    for p in &layer.extrudable {
        if !p.bbox().overlaps(&tails_box) {
            continue;
        }
        let one = std::slice::from_ref(p);
        let common = intersection_ex(one, &tails);
        if common.is_empty() {
            continue;
        }
        let (cw, ch) = size(&bbox_of(&common));
        if cw > TAIL_BOX && ch > TAIL_BOX {
            continue;
        }
        // The first lower tail it overlaps, in the list's order; none (a bare touch) adds nothing.
        let under = lower.iter().find(|t| meets(one, std::slice::from_ref(&t.piece))).map_or(0.0, |t| t.height);
        let height = h + under;
        if height > TAIL_HEIGHT {
            continue;
        }
        let new = difference_ex(one, &tails);
        if !new.is_empty() {
            let (nw, nh) = size(&bbox_of(&new));
            if (nw > tw + TAIL_FAST_BOX && nh > th + TAIL_FAST_BOX) || !grow(&new, -TAIL_FAST_SHRINK * w, MITRE).is_empty() {
                continue;
            }
        }
        out.push(Tail { piece: p.clone(), height });
    }
    out
}

/// The raw pieces grouped into clusters, bottom to top (§2.6). Each piece scans the clusters in
/// creation order, reaching each it passes, and joins the first whose pieces on the layer below
/// it overlaps once grown by a line; refused by all, it starts a new one, unreached. `counted`
/// says per layer which raw pieces Fab's measure counts (none past its end).
fn clusters(layers: &[Layer], w: f64, counted: &[Vec<bool>]) -> Vec<Cluster> {
    let mut all: Vec<Cluster> = Vec::new();
    // Each cluster's box on the layer below; empty when it has no piece there.
    let mut boxes: Vec<BBox> = Vec::new();
    // Each piece grown by a line, with its box, side by side.
    let grown = par::map(layers.len(), |l| {
        layers[l].raw.iter().map(|p| {
            let g = grow(std::slice::from_ref(p), w, MITRE);
            let gb = bbox_of(&g);
            (g, gb)
        }).collect::<Vec<_>>()
    });
    for (l, layer) in layers.iter().enumerate() {
        for (i, p) in layer.raw.iter().enumerate() {
            let (g, gb) = &grown[l][i];
            let mut joined = None;
            for (c, cl) in all.iter_mut().enumerate() {
                cl.reached = true;
                if !boxes[c].is_empty() && boxes[c].overlaps(gb) && meets(g, cl.on(l - 1)) {
                    joined = Some(c);
                    break;
                }
            }
            let c = joined.unwrap_or_else(|| {
                all.push(Cluster { first: l, layers: Vec::new(), reached: false, counted: false });
                boxes.push(BBox::empty());
                all.len() - 1
            });
            let cl = &mut all[c];
            let k = l - cl.first;
            if cl.layers.len() <= k {
                cl.layers.resize(k + 1, Vec::new());
            }
            cl.layers[k].push(p.clone());
            cl.counted |= counted.get(l).is_some_and(|c| c[i]);
        }
        boxes = all.iter().map(|c| bbox_of(c.on(l))).collect();
    }
    all
}

/// A cluster's verdict (§2.7): its flags but thin-plate, whether it is small before the deep
/// fragment test, and its merged outline.
struct Verdict {
    ty: Type,
    /// The first cantilever piece it overlaps: its layer and distance.
    cantilever: Option<(usize, f64)>,
    small: bool,
    enforced: bool,
    outline: Vec<ExPolygon>,
}

/// `removal`: small-overhang removal is on (the guard did not trip). The sharp-tail flag is
/// part of removal.
fn judge(c: &Cluster, layers: &[Layer], tails: &[Vec<Tail>], w: f64, removal: bool) -> Verdict {
    let mut grown: Vec<ExPolygon> = Vec::new();
    let (mut tail, mut cant, mut enforced) = (false, None, false);
    for (k, pieces) in c.layers.iter().enumerate() {
        let l = c.first + k;
        let tail_set: Vec<ExPolygon> = tails[l].iter().map(|t| t.piece.clone()).collect();
        for (i, p) in pieces.iter().enumerate() {
            let one = std::slice::from_ref(p);
            tail = tail || (removal && meets(one, &tail_set));
            if cant.is_none() {
                cant = layers[l].cantilevers.iter().find(|(c, _)| meets(one, std::slice::from_ref(c))).map(|(_, d)| (l, *d));
            }
            enforced = enforced || meets(one, &layers[l].enforced);
            // The first piece goes in ungrown.
            if k == 0 && i == 0 {
                grown.push(p.clone());
            } else {
                grown.extend(grow(one, w, MITRE));
            }
        }
    }
    let outline = union_ex(&grown);
    let ty = Type { tail, cantilever: cant.is_some(), big_flat: total_area_mm2(&outline) > BIG_FLAT, thin_plate: false };
    let small = removal && !tail && cant.is_none() && !c.counted && (thin_and_short(c, w) || narrow(&outline, w));
    Verdict { ty, cantilever: cant, small, enforced, outline }
}

/// Thin and short (§2.7): shrunk by half a line, every layer at most a line wide, half a line on
/// average, under 4 mm² on average, under 3 layers.
fn thin_and_short(c: &Cluster, w: f64) -> bool {
    if c.layers.len() >= SMALL_LAYERS {
        return false;
    }
    let (mut widths, mut area) = (0.0, 0.0);
    for pieces in &c.layers {
        let width = grow(pieces, -w / 2.0, MITRE).iter().map(|e| {
            let (bw, bh) = size(&e.bbox());
            bw.min(bh)
        }).fold(0.0, f64::max);
        if width > w {
            return false;
        }
        widths += width;
        area += total_area_mm2(pieces);
    }
    let n = c.layers.len() as f64;
    widths / n < w / 2.0 && area / n < SMALL_AREA
}

/// Narrow (§2.7): the merged outline shrunk by 2.5 lines is empty, or what is left of it lies in
/// a box under 2 mm in x and y.
fn narrow(outline: &[ExPolygon], w: f64) -> bool {
    let left = grow(outline, -NARROW_SHRINK * w, MITRE);
    let (bw, bh) = size(&bbox_of(&left));
    left.is_empty() || (bw < NARROW_BOX && bh < NARROW_BOX)
}

/// Thin-plate (§2.7), for a kept cluster that is not big and flat: its outline's box over
/// 10 mm in x or y, its first layer under 20, and its outline clear of the part below, shrunk
/// by a line when a later piece reached the cluster. The shrink goes island by island and then
/// unites, which differs from shrinking their union only where layer 0's islands, never united
/// (§2.7, [`part_below`]), overlap.
fn thin_plate(c: &Cluster, outline: &[ExPolygon], below: &[ExPolygon], w: f64) -> bool {
    let (bw, bh) = size(&bbox_of(outline));
    if !(bw > PLATE_BOX || bh > PLATE_BOX) || c.first >= PLATE_LAYERS {
        return false;
    }
    let below = if c.reached { union_ex(&below.iter().flat_map(|e| grow(std::slice::from_ref(e), -w, MITRE)).collect::<Vec<_>>()) } else { below.to_vec() };
    !meets(outline, &below)
}

/// §2.7's part below, for each first layer in `firsts` (ascending): every slice under it, each
/// island simplified at 0.2 mm on its own ([`island_simplified`]). Layer 0's islands are listed as
/// they come, never united; from layer 1 up each layer's list joins the layers' below in one
/// union. `from` holds the part below some layers of these slices (ascending): from the highest
/// of them at or under a first layer only the slices over it are added. With how many layers it
/// added.
fn part_below(slices: &[Vec<ExPolygon>], firsts: &[usize], from: &[(usize, Vec<ExPolygon>)]) -> (Vec<Vec<ExPolygon>>, usize) {
    let mut under: Vec<Vec<ExPolygon>> = Vec::with_capacity(firsts.len());
    let mut acc: Vec<ExPolygon> = Vec::new();
    let (mut next, mut added) = (0, 0);
    let mut from = from.iter().peekable();
    for &f in firsts {
        let mut high = None;
        while let Some(at) = from.next_if(|(at, _)| *at <= f) {
            high = Some(at);
        }
        if let Some((at, set)) = high.filter(|(at, _)| *at > next) {
            (acc, next) = (set.clone(), *at);
        }
        while next < f {
            let list = island_simplified(&slices[next], DEEP_SIMPLIFY);
            acc = if next == 0 { list } else { union_ex(&[std::mem::take(&mut acc), list].concat()) };
            next += 1;
            added += 1;
        }
        under.push(acc.clone());
    }
    (under, added)
}

/// §2.0: the part's slices as §2 and every later stage read them, from each layer's raw cut
/// loops, on every layer, also where the closing changes nothing but its few nm and its dropped
/// points:
/// 1. the loops united into islands;
/// 2. each ring grown by 0.049 mm on its own, mitre 3, after losing its close points (§1.5): a
///    contour out, a hole in;
/// 3. those rings united into islands (S unites them only when there are several islands, but
///    one island's grown rings, the contour's own offset less its shrunk holes, are that union
///    already, and by Rule A too);
/// 4. each ring shrunk by 0.049 mm on its own the same way, and the rings united with a box into
///    islands: what they wind positive round, so where a hole's growth passes its contour's
///    shrink, as on a bridge the grow made thinner than 0.098 mm, nothing is left; every ring by
///    Rule A (§1.6);
/// 5. each island simplified at 0.0025 mm on its own ([`island_simplified`]), one entry per
///    island, as the reference stores them.
///
/// A layer whose cut loops are the run before's is that run's slice: `before` holds that run's
/// cut loops and the slices it made of them. With how many layers it made afresh.
pub fn slices(ctl: Ctl, raw: &[Vec<ExPolygon>], before: Option<(&[Vec<ExPolygon>], &[Vec<ExPolygon>])>) -> Result<(Vec<Vec<ExPolygon>>, usize), Cancelled> {
    crate::prof::scope_fanned(crate::prof::Stage::TreeOverhangs, || {
        let ready = par::try_map(ctl, raw.len(), |l| {
            let was = before.filter(|(cut, _)| cut.get(l) == Some(&raw[l])).and_then(|(_, made)| made.get(l));
            let ready = match was {
                Some(was) => (was.clone(), false),
                None if raw[l].is_empty() => (Vec::new(), true),
                None => {
                    let d = SLICE_CLOSING * SCALE;
                    let grown = union(&ring_by_ring(&union_ex(&raw[l]), d, MITRE), FillRule::NonZero);
                    let closed = union(&ring_by_ring(&grown, -d, MITRE), FillRule::Positive);
                    (island_simplified(&closed, SLICE_SIMPLIFY), true)
                }
            };
            super::tell(ctl, super::Stage::Slices, l, raw.len(), super::Grown::Nothing);
            ready
        })?;
        let made = ready.iter().filter(|r| r.1).count();
        Ok((ready.into_iter().map(|r| r.0).collect(), made))
    })
}

/// Everything §2 finds before the output.
struct Found {
    extr: Vec<Vec<ExPolygon>>,
    layers: Vec<Layer>,
    tails: Vec<Vec<Tail>>,
    clusters: Vec<Cluster>,
    types: Vec<Type>,
    kept: Vec<bool>,
    cantilevers: Vec<(usize, f64)>,
    tripped: bool,
    /// The part below each first layer §2.7 asked for.
    below: Vec<(usize, Vec<ExPolygon>)>,
    /// The first passes it made rather than took, and the layers it added to the part below.
    passed: usize,
    added: usize,
}

/// The overhangs of a part (R14 §2) from its slices as §2.0 leaves them ([`slices`]) and layer
/// `(z, height)`s, with the enforced and blocked areas per layer (a layer past either list's end
/// has none); taking what it can from `before`, the run before's on the same keys. With what it
/// found that a run after may take.
pub fn detect(ctl: Ctl, s: &Settings, slices: &[Vec<ExPolygon>], heights: &[(f64, f64)], enforce: &[Vec<ExPolygon>], block: &[Vec<ExPolygon>], before: Option<&Before>) -> Result<(Overhangs, Passes), Cancelled> {
    crate::prof::scope_fanned(crate::prof::Stage::TreeOverhangs, || {
        let f = find(ctl, s, slices, heights, enforce, before)?;
        let o = output(ctl, &f, block)?;
        Ok((o, Passes { layers: f.layers, below: f.below, passed: f.passed, added: f.added }))
    })
}

fn find(ctl: Ctl, s: &Settings, slices: &[Vec<ExPolygon>], heights: &[(f64, f64)], enforce: &[Vec<ExPolygon>], before: Option<&Before>) -> Result<Found, Cancelled> {
    let n = slices.len();
    let w = s.keys.line_width;
    let theta = threshold(s.keys.threshold_deg);
    // How far it is: a share per layer of each of these two, and one per layer of what it finds
    // (in [`super::generate`]).
    let tell = |l: usize| super::tell(ctl, super::Stage::Overhangs, l, 3 * n, super::Grown::Nothing);
    // Per layer, whether its slice is the run before's.
    let same: Vec<bool> = match before {
        Some(b) => par::try_map(ctl, n, |l| b.slices.get(l) == Some(&slices[l]))?,
        None => vec![false; n],
    };
    let was = |l: usize| before.and_then(|b| b.passes.layers.get(l));
    // A layer's first pass is the run before's where what it reads is: its slice and the two
    // under it, the height under it (layer 0's own) and its enforced areas.
    let taken = |l: usize| {
        let h = l.saturating_sub(1);
        let reads = |b: &Before| same[l] && (l == 0 || same[l - 1]) && (l < 2 || same[l - 2]) && b.heights.get(h) == heights.get(h) && b.enforce.get(l) == enforce.get(l);
        was(l).filter(|_| before.is_some_and(reads))
    };
    let extr = par::try_map(ctl, n, |l| {
        let e = match was(l).filter(|_| same[l]) {
            Some(was) => was.extrudable.clone(),
            None => extrudable(&slices[l], s.keys.narrowest),
        };
        tell(l);
        e
    })?;
    let layers = par::try_map(ctl, n, |l| {
        let first = match taken(l) {
            Some(was) => was.clone(),
            None => first_pass(l, &extr, heights, w, theta, enforce.get(l)),
        };
        tell(l);
        first
    })?;
    let passed = (0..n).filter(|&l| taken(l).is_none()).count();
    let tripped = layers.iter().any(|l| l.tripped);
    // §2.7 runs when the key asks for it and the guard did not trip.
    let removal = s.keys.remove_small && !tripped;
    // §2.5, bottom to top, not after the guard trips; beside it, what of the raw overhang Fab's
    // own measure counts, grouped as it groups its faces. Neither reads the other.
    let (tails, counted) = par::try_join(
        ctl,
        || -> Result<Vec<Vec<Tail>>, Cancelled> {
            let mut tails: Vec<Vec<Tail>> = layers.iter().map(|l| l.tails.clone()).collect();
            if !tripped {
                for l in 1..n {
                    if ctl.cancelled() {
                        return Err(Cancelled);
                    }
                    let more = second_pass(&layers[l], &tails[l - 1], heights[l].1 as f32, w);
                    tails[l].extend(more);
                }
            }
            Ok(tails)
        },
        || -> Result<Vec<Vec<bool>>, Cancelled> {
            if !removal {
                return Ok(Vec::new());
            }
            let raw: Vec<Vec<ExPolygon>> = layers.iter().map(|l| l.raw.clone()).collect();
            Ok(crate::support::small_groups(ctl, &raw, w)?.into_iter().map(|small| small.into_iter().map(|s| !s).collect()).collect())
        },
    )?;
    let (tails, counted) = (tails?, counted?);
    let clusters = clusters(&layers, w, &counted);
    let verdicts = par::try_map(ctl, clusters.len(), |c| judge(&clusters[c], &layers, &tails, w, removal))?;
    // The part below a cluster's first layer ([`part_below`]): the deep fragment test (§2.7 item
    // 3) asks it for small clusters, thin-plate for candidates.
    let candidate = |c: &Cluster, v: &Verdict| {
        let (bw, bh) = size(&bbox_of(&v.outline));
        v.small || (!v.ty.big_flat && c.first < PLATE_LAYERS && (bw > PLATE_BOX || bh > PLATE_BOX))
    };
    let mut firsts: Vec<usize> = clusters.iter().zip(&verdicts).filter(|(c, v)| candidate(c, v)).map(|(c, _)| c.first).collect();
    firsts.sort_unstable();
    firsts.dedup();
    // The run before's part below layers as high up as the slices are its.
    let up_to = same.iter().position(|s| !s).unwrap_or(n);
    let from = before.map_or(&[][..], |b| &b.passes.below[..b.passes.below.partition_point(|(at, _)| *at <= up_to)]);
    let (under, added) = part_below(slices, &firsts, from);
    let below = |f: usize| &under[firsts.binary_search(&f).expect("every candidate's first layer has the part below it")];
    let judged = par::try_map(ctl, clusters.len(), |i| {
        let (c, v) = (&clusters[i], &verdicts[i]);
        let kept = !v.small || v.enforced || !difference_ex(&v.outline, &grow(below(c.first), DEEP_GROW * w, MITRE)).is_empty();
        let mut ty = v.ty;
        ty.thin_plate = kept && !ty.big_flat && candidate(c, v) && thin_plate(c, &v.outline, below(c.first), w);
        (ty, kept)
    })?;
    let (types, kept) = judged.into_iter().unzip();
    let mut cantilevers: Vec<(usize, f64)> = verdicts.iter().filter_map(|v| v.cantilever).collect();
    cantilevers.sort_by_key(|c| c.0);
    let below = firsts.into_iter().zip(under).collect();
    Ok(Found { extr, layers, tails, clusters, types, kept, cantilevers, tripped, below, passed, added })
}

/// §2.9: kept cluster pieces and tail pieces per layer, less the blocked areas, each sharp tail
/// grown; no union.
fn output(ctl: Ctl, f: &Found, block: &[Vec<ExPolygon>]) -> Result<Overhangs, Cancelled> {
    let layers = par::try_map(ctl, f.layers.len(), |l| {
        let mut items: Vec<(ExPolygon, Type)> = Vec::new();
        for ((c, ty), _) in f.clusters.iter().zip(&f.types).zip(&f.kept).filter(|(_, k)| **k) {
            items.extend(c.on(l).iter().map(|p| (p.clone(), *ty)));
        }
        if l == 0 {
            items.extend(f.tails[0].iter().map(|t| (t.piece.clone(), Type::TAIL)));
        } else {
            let tails: Vec<&Tail> = f.tails[l].iter().filter(|t| ((t.height * 10.0).floor() as i64) % 5 == 0).collect();
            if !tails.is_empty() {
                let past = grow(&f.extr[l - 1], TAIL_OUT_GAP, MITRE);
                for t in tails {
                    items.extend(difference_ex(std::slice::from_ref(&t.piece), &past).into_iter().map(|p| (p, Type::TAIL)));
                }
            }
        }
        if let Some(b) = block.get(l).filter(|b| !b.is_empty()) {
            let b = grow(b, BLOCK_GROW, MITRE);
            items = items.into_iter().flat_map(|(p, ty)| difference_ex(std::slice::from_ref(&p), &b).into_iter().map(move |q| (q, ty))).collect();
        }
        let mut list = Vec::new();
        for (p, ty) in items {
            if ty == Type::TAIL {
                list.extend(grow(std::slice::from_ref(&p), TAIL_GROW, MITRE));
            } else {
                list.push(p);
            }
        }
        list
    })?;
    let largest_cantilever = f.layers.iter().map(|l| l.largest).fold(0.0, f64::max);
    Ok(Overhangs { layers, largest_cantilever, cantilevers: f.cantilevers.clone(), tripped: f.tripped })
}

#[cfg(test)]
mod tests {
    use super::super::settings::tests::r14;
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1)))
    }

    /// R14's layers: 0.25 mm first, then 0.2 mm.
    fn heights(n: usize) -> Vec<(f64, f64)> {
        (0..n).map(|l| if l == 0 { (0.25, 0.25) } else { (0.25 + 0.2 * l as f64, 0.2) }).collect()
    }

    fn detect_r14(slices: &[Vec<ExPolygon>], threshold_deg: f64) -> Overhangs {
        let mut s = r14(0.35);
        s.keys.threshold_deg = threshold_deg;
        detect(Ctl::default(), &s, slices, &heights(slices.len()), &[], &[], None).unwrap().0
    }

    fn mm2(set: &[ExPolygon]) -> f64 {
        total_area_mm2(set)
    }

    /// §2.1: a strip narrower than a line goes; where it joins a wider part, half a line of it
    /// stays.
    #[test]
    fn the_extrudable_slice_drops_what_is_narrower_than_a_line() {
        let alone = extrudable(&[rect(0.0, 0.0, 10.0, 10.0), rect(20.0, 0.0, 20.3, 5.0)], 0.42);
        assert!((mm2(&alone) - 100.0).abs() < 1e-6, "a 0.3 mm strip on its own goes: {}", mm2(&alone));
        let joined = extrudable(&union_ex(&[rect(0.0, 0.0, 10.0, 10.0), rect(10.0, 4.0, 15.0, 4.3)]), 0.42);
        let strip = mm2(&joined) - 100.0;
        assert!((strip - 0.21 * 0.3).abs() < 1e-3, "half a line of it stays by the wider part: {strip} mm²");
    }

    /// §2.1, Fab's departure: the slice keeps what the walls print. A strip 0.19 mm wide, 2 mm off
    /// the part where a face starts from its low point, is a face for tips when the walls print
    /// down to 0.1 mm; at the reference's line it goes, and nothing would hold it.
    #[test]
    fn a_strip_the_walls_print_is_a_face_for_tips() {
        let base = || rect(0.0, 0.0, 10.0, 10.0);
        let slices = vec![vec![base()], vec![base()], vec![base(), rect(12.0, 0.0, 13.27, 0.19)], vec![base(), rect(12.0, 0.0, 13.27, 0.4)]];
        let faces = |narrowest: f64| {
            let mut s = r14(0.35);
            s.keys.narrowest = narrowest;
            let o = detect(Ctl::default(), &s, &slices, &heights(slices.len()), &[], &[], None).unwrap().0;
            mm2(&intersection_ex(&o.layers[2], &[rect(11.0, -1.0, 14.5, 1.5)]))
        };
        assert!(faces(0.1) >= 1.27 * 0.19 - 1e-3, "the strip is a face: {} mm²", faces(0.1));
        assert!(faces(0.42) < 1e-9, "at a line it goes: {} mm²", faces(0.42));
    }

    /// §2.2: the threshold is the key plus 1° (30° for a key of 0); the layer below grows by its
    /// own height over tan θ: 0.333 mm at 31°, 0.416 mm over the 0.25 mm first layer, 0.193 mm
    /// at 46°.
    #[test]
    fn the_raw_overhang_is_what_the_grown_layer_below_does_not_carry() {
        assert!((threshold(0.0).to_degrees() - 30.0).abs() < 1e-9 && (threshold(30.0).to_degrees() - 31.0).abs() < 1e-9 && (threshold(89.0).to_degrees() - 89.0).abs() < 1e-9);
        // Each layer 1 mm past the one below, in x.
        let slices: Vec<Vec<ExPolygon>> = (0..6).map(|l| vec![rect(0.0, 0.0, 10.0 + l as f64, 10.0)]).collect();
        let s = r14(0.35);
        for (key, first, rest) in [(30.0, 1.0 - 0.25 / 31f64.to_radians().tan(), 1.0 - 0.2 / 31f64.to_radians().tan()), (45.0, 1.0 - 0.25 / 46f64.to_radians().tan(), 1.0 - 0.2 / 46f64.to_radians().tan())] {
            let theta = threshold(key);
            let extr: Vec<Vec<ExPolygon>> = slices.iter().map(|sl| extrudable(sl, 0.42)).collect();
            let one = first_pass(1, &extr, &heights(6), s.keys.line_width, theta, None);
            let three = first_pass(3, &extr, &heights(6), s.keys.line_width, theta, None);
            assert!((mm2(&one.raw) / 10.0 - first).abs() < 1e-4, "{key}°: over the first layer the strip is {first:.4} mm wide: {}", mm2(&one.raw) / 10.0);
            assert!((mm2(&three.raw) / 10.0 - rest).abs() < 1e-4, "{key}°: over a 0.2 mm layer {rest:.4} mm: {}", mm2(&three.raw) / 10.0);
        }
        assert!((1.0 - (1.0 - 0.2 / 31f64.to_radians().tan()) - 0.333).abs() < 1e-3, "R14's 0.333 mm at 31°");
    }

    /// §2.3: a piece in mid-air is a tail of height 0; one within a tenth of a line of the layer
    /// below, or two below, is not; nor one of 100 mm² or more, nor a wide one with a box of
    /// 100 mm² or more. On layer 0, pieces no more than 6 mm both ways are feet of 0.25 mm.
    #[test]
    fn a_sharp_tail_starts_in_mid_air() {
        let w = 0.42;
        let base = vec![rect(0.0, 0.0, 10.0, 10.0)];
        assert!(floating_tail(&rect(12.0, 0.0, 14.0, 2.0), w, &base, Some(&base)), "a 2 mm square 2 mm off the part");
        assert!(!floating_tail(&rect(10.03, 0.0, 12.0, 2.0), w, &base, Some(&base)), "0.03 mm off: grown by 0.042 it touches");
        assert!(!floating_tail(&rect(12.0, 0.0, 14.0, 2.0), w, &[], Some(&[rect(13.0, 1.0, 13.5, 1.5)])), "it touches two layers below");
        assert!(!floating_tail(&rect(20.0, 0.0, 30.0, 10.0), w, &base, Some(&base)), "100 mm² is not under 100");
        // An L of 2 mm arms: box 144 mm², area 44 mm², gone at a 1.05 mm shrink: a tail. With
        // 2.2 mm arms it survives the shrink and is not.
        let ell = |a: f64| union_ex(&[rect(20.0, 0.0, 32.0, a), rect(20.0, 0.0, 20.0 + a, 12.0)]).remove(0);
        assert!(floating_tail(&ell(2.0), w, &base, Some(&base)), "a thin L with a big box");
        assert!(!floating_tail(&ell(2.2), w, &base, Some(&base)), "a wide L with a big box");
        let foot = first_pass(0, &[vec![rect(0.0, 0.0, 6.0, 5.0), rect(10.0, 0.0, 16.5, 5.0)]], &heights(1), w, threshold(30.0), None);
        assert_eq!(foot.tails.len(), 1, "6 by 5 mm is a foot, 6.5 by 5 is not");
        assert_eq!(foot.tails[0].height, 0.25f32);
    }

    /// §2.4: a ring with no corner within a line of the layer below floats, its distance its
    /// farthest corner's; a piece reaching more than 3 mm past what holds it (0.52 mm out from
    /// the layer below at R14's numbers) is far.
    #[test]
    fn cantilevers_float_or_reach_far() {
        let w = 0.42;
        let theta = threshold(30.0);
        let h = heights(3);
        let run = |top: Vec<ExPolygon>| {
            let extr = vec![vec![rect(0.0, 0.0, 10.0, 10.0)], vec![rect(0.0, 0.0, 10.0, 10.0)], top];
            first_pass(2, &extr, &h, w, theta, None)
        };
        let float = run(vec![rect(0.0, 0.0, 10.0, 10.0), rect(12.0, 2.0, 14.0, 4.0)]);
        assert_eq!(float.cantilevers.len(), 1);
        assert!((float.largest - (14.0 - 10.42)).abs() < 1e-6, "the farthest corner is 3.58 mm from the layer below grown by a line: {}", float.largest);
        let far = run(vec![rect(0.0, 0.0, 15.0, 10.0)]);
        assert_eq!(far.cantilevers.len(), 1, "5 mm out is far");
        assert!((far.largest - (15.0 - 10.52)).abs() < 1e-3, "4.48 mm past the held band 0.52 mm out: {}", far.largest);
        let near = run(vec![rect(0.0, 0.0, 13.0, 10.0)]);
        assert!(near.cantilevers.is_empty(), "3 mm out reaches 2.48 mm past what holds it: no cantilever");
    }

    /// §2.5: a column standing on a floating tail continues it, each layer adding its height in
    /// 32-bit floats, for at most 16 mm (80 layers of 0.2 mm); a piece that widens fast does
    /// not.
    #[test]
    fn a_tail_chain_climbs_at_most_16_mm() {
        let n = 100;
        let mut slices: Vec<Vec<ExPolygon>> = vec![vec![rect(0.0, 0.0, 10.0, 10.0)]; n];
        for (l, s) in slices.iter_mut().enumerate().skip(5) {
            s.push(rect(20.0, 0.0, 22.0, 2.0));
            if l == 50 {
                // A wide step: no longer a tail.
                *s = vec![rect(0.0, 0.0, 10.0, 10.0), rect(20.0, 0.0, 22.0, 2.0)];
            }
        }
        let w = 0.42;
        let extr: Vec<Vec<ExPolygon>> = slices.iter().map(|s| extrudable(s, w)).collect();
        let h = heights(n);
        let layers: Vec<Layer> = (0..n).map(|l| first_pass(l, &extr, &h, w, threshold(30.0), None)).collect();
        let mut tails: Vec<Vec<Tail>> = layers.iter().map(|l| l.tails.clone()).collect();
        for l in 1..n {
            let more = second_pass(&layers[l], &tails[l - 1], h[l].1 as f32, w);
            tails[l].extend(more);
        }
        let column: Vec<usize> = (0..n).filter(|&l| tails[l].iter().any(|t| t.piece.bbox().min.x == Pt::from_mm(20.0, 0.0).x)).collect();
        assert_eq!(column.first(), Some(&5), "the tail starts where the column floats");
        assert_eq!(column.last(), Some(&85), "80 layers on, 15.99999 mm in f32; the 81st would pass 16");
        assert_eq!(column.len(), 81);
        // A tail whose next layer widens to 9 mm both ways grows fast and ends.
        let wide = Layer { extrudable: vec![rect(19.0, -1.0, 28.0, 8.0)], raw: Vec::new(), enforced: Vec::new(), tails: Vec::new(), cantilevers: Vec::new(), largest: 0.0, tripped: false };
        assert!(second_pass(&wide, &[Tail { piece: rect(20.0, 0.0, 22.0, 2.0), height: 0.0 }], 0.2, w).is_empty(), "a new part 7 mm larger both ways grows fast");
    }

    /// §2.6: a piece joins the first cluster whose piece on the layer below it overlaps once
    /// grown by a line; one farther off starts its own. Every cluster a piece's scan passes is
    /// reached; the scan stops at the one it joins.
    #[test]
    fn pieces_cluster_up_the_layers() {
        let mk = |raw: Vec<ExPolygon>| Layer { extrudable: Vec::new(), raw, enforced: Vec::new(), tails: Vec::new(), cantilevers: Vec::new(), largest: 0.0, tripped: false };
        let mut layers = vec![mk(vec![rect(0.0, 0.0, 1.0, 5.0)]), mk(vec![rect(1.4, 0.0, 2.4, 5.0), rect(5.0, 0.0, 6.0, 5.0)]), mk(vec![rect(2.8, 0.0, 3.8, 5.0)])];
        let cl = clusters(&layers, 0.42, &[]);
        assert_eq!(cl.len(), 2, "0.4 mm apart joins, 2.6 mm apart does not");
        assert_eq!((cl[0].first, cl[0].layers.len()), (0, 3), "the first cluster climbs three layers");
        assert_eq!((cl[1].first, cl[1].layers.len()), (1, 1));
        assert!(cl[0].reached && !cl[1].reached, "layer 2's piece joins the first cluster and never reaches the second");
        layers.push(mk(vec![rect(20.0, 0.0, 21.0, 5.0)]));
        let cl = clusters(&layers, 0.42, &[]);
        assert_eq!(cl.len(), 3);
        assert!(cl[1].reached && !cl[2].reached, "a piece refused by all reaches every cluster, its own starts unreached");
    }

    /// §2.2 and §2.6: each layer's raw pieces come highest first (§1.6), and a piece joins the
    /// first cluster it meets. So where two runs merge, the merged piece goes to the run whose
    /// first piece is higher, and the other run is one thin layer, which §2.7 drops. Fab's
    /// departure keeps it: its own measure counts the bump with the ledge it widens up into, and
    /// dropped, the bump would print over air ([`Cluster::counted`]).
    #[test]
    fn a_merged_piece_joins_the_higher_run() {
        // A block; on layer 1 two thin bumps beyond its right edge, the higher one at y 3.5; from
        // layer 2 on, a 4 mm ledge over both.
        let block = rect(0.0, 0.0, 10.0, 10.0);
        let low = rect(10.0, 1.0, 10.6, 2.0);
        let high = rect(10.0, 3.5, 10.6, 4.5);
        let mut slices: Vec<Vec<ExPolygon>> = vec![vec![block.clone()]];
        slices.push(union_ex(&[block.clone(), low.clone(), high.clone()]));
        for _ in 2..8 {
            slices.push(union_ex(&[block.clone(), low.clone(), high.clone(), rect(10.0, 0.5, 14.0, 5.5)]));
        }
        let o = detect_r14(&slices, 45.0);
        let at = |p: Pt| o.layers[1].iter().any(|e| e.contains(p));
        assert!(at(Pt::from_mm(10.4, 4.0)), "the higher bump's run takes the ledge and stays");
        assert!(at(Pt::from_mm(10.4, 1.5)), "the lower bump's run stays: Fab counts it with the ledge");
        let mut s = r14(0.35);
        s.keys.threshold_deg = 45.0;
        let f = find(Ctl::default(), &s, &slices, &heights(slices.len()), &[], None).unwrap();
        let lower = f.clusters.iter().position(|c| c.on(1).iter().any(|e| e.contains(Pt::from_mm(10.4, 1.5)))).expect("the lower bump's cluster");
        let c = &f.clusters[lower];
        assert_eq!((c.first, c.layers.len()), (1, 1), "the lower run is one layer: the ledge went to the higher run");
        assert!(c.counted && f.kept[lower], "counted, so kept");
        let reference = Cluster { first: c.first, layers: c.layers.clone(), reached: c.reached, counted: false };
        assert!(judge(&reference, &f.layers, &f.tails, 0.42, true).small, "the reference drops it, narrow and short");
    }

    /// §2.7: a narrow cluster goes; a wide one stays; a narrow one reaching past everything
    /// printed under it, grown by 1.05 mm, is a deep fragment and stays. §2.9: kept pieces go out
    /// on their own layers. With small-overhang removal off, the narrow one stays too.
    #[test]
    fn small_overhangs_go_unless_they_must_stay() {
        // A 10 mm block whose layer 3 sticks out 0.8 mm on one side: a 0.467 mm strip, gone at a
        // 1.05 mm shrink, inside the block grown by 1.05 mm: dropped. Layer 6 sticks out 3 mm: a
        // 2.667 mm strip, kept.
        let mut slices: Vec<Vec<ExPolygon>> = vec![vec![rect(0.0, 0.0, 10.0, 10.0)]; 10];
        slices[3] = vec![rect(0.0, 0.0, 10.8, 10.0)];
        slices[6] = vec![rect(0.0, 0.0, 13.0, 10.0)];
        let o = detect_r14(&slices, 30.0);
        assert!(o.layers[3].is_empty(), "a narrow strip within reach of the part below goes");
        assert_eq!(o.layers[6].len(), 1, "a 2.667 mm strip stays");
        assert!((mm2(&o.layers[6]) - 26.67).abs() < 0.01, "on its own layer, unchanged: {}", mm2(&o.layers[6]));
        assert_eq!(o.layers_with_overhang(), 1);
        let mut off = r14(0.35);
        off.keys.remove_small = false;
        let o = detect(Ctl::default(), &off, &slices, &heights(slices.len()), &[], &[], None).unwrap().0;
        assert!(!o.layers[3].is_empty() && o.layers_with_overhang() == 2, "with removal off the narrow strip stays");
        // From layer 6 the block is 1.833 mm deeper: a 1.5 mm strip, narrow, but reaching
        // 0.78 mm past the block grown by 1.05 mm.
        let mut deep: Vec<Vec<ExPolygon>> = vec![vec![rect(0.0, 0.0, 10.0, 10.0)]; 10];
        for sl in deep.iter_mut().skip(6) {
            *sl = vec![rect(0.0, 0.0, 10.0, 11.833)];
        }
        let o = detect_r14(&deep, 30.0);
        assert!((mm2(&o.layers[6]) - 15.0).abs() < 0.01, "the deep fragment stays: {}", mm2(&o.layers[6]));
    }

    /// §2.7: a kept cluster that is not big and flat is a thin plate when its outline's box is
    /// over 10 mm one way, it starts under layer 20, and it clears the part below.
    #[test]
    fn thin_plates_clear_the_part_below() {
        let w = 0.42;
        let types = |slices: &[Vec<ExPolygon>]| {
            let f = find(Ctl::default(), &r14(0.35), slices, &heights(slices.len()), &[], None).unwrap();
            f.clusters.iter().zip(&f.types).zip(&f.kept).map(|((c, t), k)| (c.first, *t, *k)).collect::<Vec<_>>()
        };
        // A 12 by 2 mm plate floating on layer 5 beside a post.
        let mut slices: Vec<Vec<ExPolygon>> = vec![vec![rect(0.0, 0.0, 2.0, 2.0)]; 8];
        slices[5].push(rect(10.0, 0.0, 22.0, 2.0));
        let t = types(&slices);
        assert_eq!(t.len(), 1);
        assert!(t[0].1.thin_plate && !t[0].1.big_flat && t[0].2, "a long plate clear of the part below: {t:?}");
        // The same plate over a wide base on layers 0 and 1: not clear of the part below.
        let mut over = slices.clone();
        over[0].push(rect(8.0, -5.0, 24.0, 7.0));
        over[1].push(rect(8.0, -5.0, 24.0, 7.0));
        let t = types(&over);
        assert!(t.iter().all(|(_, ty, _)| !ty.thin_plate), "over the base it is no plate: {t:?}");
        // On layer 20 it is too high.
        let mut high: Vec<Vec<ExPolygon>> = vec![vec![rect(0.0, 0.0, 2.0, 2.0)]; 22];
        high[20].push(rect(10.0, 0.0, 22.0, 2.0));
        assert!(!types(&high)[0].1.thin_plate, "layer 20 is not under 20");
        // 11 by 10 mm: big and flat, so never a plate.
        let mut big = slices.clone();
        big[5] = vec![rect(0.0, 0.0, 2.0, 2.0), rect(10.0, 0.0, 21.0, 10.0)];
        let t = types(&big);
        assert!(t[0].1.big_flat && !t[0].1.thin_plate, "{t:?}");
        assert!(w > 0.0);
    }

    /// §2.7's part below: layer 0's islands, each simplified on its own, stay a list; from layer 1
    /// up each layer's list joins the layers below in one union. Thin-plate shrinks it island by
    /// island: two of layer 0's islands that overlap, each shrunk by a line, leave a gap that the
    /// shrink of their union fills.
    #[test]
    fn the_part_below_unites_from_layer_one_up() {
        let pair = vec![rect(0.0, 0.0, 2.0, 2.0), rect(2.0, 0.0, 4.0, 2.0)];
        let under = part_below(&[pair.clone(), pair.clone(), pair], &[1, 2, 3], &[]).0;
        assert_eq!(under.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1, 1], "layer 0's two islands stay two; with layer 1 they unite");
        let w = 0.42;
        let overlapping = [rect(0.0, 0.0, 12.0, 2.0), rect(11.5, 0.0, 23.5, 2.0)];
        let plate = [rect(11.6, -10.0, 11.9, 1.0)];
        let reached = Cluster { first: 1, layers: Vec::new(), reached: true, counted: false };
        assert!(thin_plate(&reached, &plate, &overlapping, w), "between 11.58 and 11.92 mm the islands shrunk one by one leave the plate clear");
        assert!(!thin_plate(&reached, &plate, &union_ex(&overlapping), w), "their union shrunk meets it");
        let unreached = Cluster { reached: false, ..reached };
        assert!(!thin_plate(&unreached, &plate, &overlapping, w), "unshrunk, the part below meets it");
    }

    /// A run after an edit takes from the run before what reads only what the edit left. The
    /// part: a 2 mm post, a 12 mm plate floating beside it on layer 5, a thin plate that asks for
    /// the part below it, and a slab on the post from layer 30 up to 39, which the edit widens by
    /// 1 mm on layers 34 and 35. §2.0 slices those two alone; the first passes are theirs and
    /// those of the two layers over them, which read them; the part below the plate is the run
    /// before's. What it finds is what a fresh run finds.
    #[test]
    fn an_edit_takes_the_passes_it_left() {
        let mut s = r14(0.35);
        s.keys.remove_small = false;
        let part = |wide: f64| -> Vec<Vec<ExPolygon>> {
            (0..40)
                .map(|l| match l {
                    5 => vec![rect(0.0, 0.0, 2.0, 2.0), rect(10.0, 0.0, 22.0, 2.0)],
                    34 | 35 => vec![rect(-4.0, -4.0, 6.0 + wide, 6.0)],
                    30..=39 => vec![rect(-4.0, -4.0, 6.0, 6.0)],
                    _ => vec![rect(0.0, 0.0, 2.0, 2.0)],
                })
                .collect()
        };
        let (cut, edited, h) = (part(0.0), part(1.0), heights(40));
        let ctl = Ctl::default;
        let (was, _) = slices(ctl(), &cut, None).unwrap();
        let (now, sliced) = slices(ctl(), &edited, Some((&cut[..], &was[..]))).unwrap();
        assert!(now == slices(ctl(), &edited, None).unwrap().0 && sliced == 2, "§2.0 slices the two edited layers alone, as a fresh run does: {sliced}");
        let (_, passes) = detect(ctl(), &s, &was, &h, &[], &[], None).unwrap();
        let before = Before { slices: &was, heights: &h, enforce: &[], passes: &passes };
        let (warm, taken) = detect(ctl(), &s, &now, &h, &[], &[], Some(&before)).unwrap();
        let (fresh, afresh) = detect(ctl(), &s, &now, &h, &[], &[], None).unwrap();
        assert_eq!(format!("{warm:?}"), format!("{fresh:?}"), "what a fresh run finds");
        assert_eq!((afresh.passed, afresh.added), (40, 5), "a fresh run passes over every layer and adds up the part below the plate");
        assert_eq!((taken.passed, taken.added), (4, 0), "layers 34 to 37 passed over, the part below the plate taken");
    }

    /// §2.0: the slices closed by 0.049 mm out and back and simplified island by island. Squares
    /// 0.09 mm apart close into one island, 0.12 mm apart stay two; a slot 0.09 mm wide closes
    /// and a 4 mm hole stays. Every ring by Rule A: the outer ring from the vertex after its flat
    /// bottom's right end, the hole from its bottom left corner round to its bottom right.
    #[test]
    fn the_slices_are_closed_and_simplified_island_by_island() {
        let prepared = |raw: Vec<ExPolygon>| slices(Ctl::default(), &[raw], None).unwrap().0.remove(0);
        let near = prepared(vec![rect(0.0, 0.0, 2.0, 2.0), rect(2.09, 0.0, 4.09, 2.0)]);
        assert_eq!(near.len(), 1, "0.09 mm apart: bridged");
        assert!((mm2(&near) - 8.18).abs() < 1e-6, "the gap filled, nothing else: {} mm²", mm2(&near));
        let apart = prepared(vec![rect(0.0, 0.0, 2.0, 2.0), rect(2.12, 0.0, 4.12, 2.0)]);
        assert_eq!(apart.len(), 2, "0.12 mm apart: two islands");
        assert!((mm2(&apart) - 8.0).abs() < 1e-6, "{} mm²", mm2(&apart));
        let holed = |x0: f64, y0: f64, x1: f64, y1: f64| {
            let mut e = rect(0.0, 0.0, 10.0, 10.0);
            let mut h = Polygon::rect(Pt::from_mm(x0, y0), Pt::from_mm(x1, y1));
            h.reverse();
            e.holes = vec![h];
            prepared(vec![e])
        };
        let slot = holed(3.0, 5.0, 7.0, 5.09);
        assert_eq!((slot.len(), slot[0].holes.len()), (1, 0), "a 0.09 mm slot closes");
        let hole = holed(3.0, 3.0, 7.0, 7.0);
        assert_eq!((hole.len(), hole[0].holes.len()), (1, 1), "a 4 mm hole stays");
        assert_eq!(hole[0].contour.pts[0], Pt::from_mm(10.0, 10.0), "the outer ring from the vertex after (10, 0)");
        let h = &hole[0].holes[0].pts;
        assert_eq!((h[0], h[h.len() - 1]), (Pt::from_mm(3.0, 3.0), Pt::from_mm(7.0, 3.0)), "the hole ends at (7, 3) and starts at the corner after it, clockwise");
    }

    /// §2.8: a layer of over 100 raw pieces skips its own tail and cantilever tests; pass 2 and
    /// removal are off everywhere, so no cluster is flagged a sharp tail and every cluster stays.
    #[test]
    fn the_piece_guard_trips_on_over_100_pieces() {
        let block = |l: usize| if l == 8 { rect(50.0, 0.0, 60.8, 10.0) } else { rect(50.0, 0.0, 60.0, 10.0) };
        let posts = |count: usize| {
            let at: Vec<(f64, f64)> = (0..11).flat_map(|i| (0..10).map(move |j| (3.0 * i as f64, 3.0 * j as f64))).take(count).collect();
            (0..10).map(|l| {
                let mut s: Vec<ExPolygon> = match l {
                    0..=5 => at.iter().map(|&(x, y)| rect(x, y, x + 1.0, y + 1.0)).collect(),
                    6 => at.iter().map(|&(x, y)| rect(x - 0.5, y - 0.5, x + 1.5, y + 1.5)).collect(),
                    _ => Vec::new(),
                };
                s.push(block(l));
                s
            }).collect::<Vec<_>>()
        };
        let run = |slices: &[Vec<ExPolygon>]| find(Ctl::default(), &r14(0.35), slices, &heights(slices.len()), &[], None).unwrap();
        let trip = run(&posts(101));
        assert!(trip.tripped && trip.layers[6].tripped, "101 caps trip the guard on layer 6");
        assert_eq!(trip.layers[6].raw.len(), 101, "its raw pieces still cluster");
        assert!(trip.layers[6].tails.is_empty() && trip.layers[6].cantilevers.is_empty(), "layer 6 skips its tail and cantilever tests");
        assert_eq!(trip.layers[0].tails.len(), 101, "layer 0's feet stand whatever the guard");
        assert!(trip.tails.iter().zip(&trip.layers).all(|(t, l)| t.len() == l.tails.len()), "no pass-2 tails");
        assert!(trip.types.iter().all(|t| !t.tail), "no cluster is flagged a sharp tail");
        assert!(trip.kept.iter().all(|k| *k), "every cluster stays");
        let o = output(Ctl::default(), &trip, &[]).unwrap();
        assert!(!o.layers[8].is_empty(), "the lone narrow strip stays with removal off");
        // A hundred caps do not trip: layer 6 finds its cantilevers, the feet climb by pass 2, and
        // the narrow strip goes.
        let calm = run(&posts(100));
        assert!(!calm.tripped);
        assert_eq!(calm.layers[6].cantilevers.len(), 100, "each cap's outer ring floats past a line from its post");
        assert!(calm.tails[1].len() > calm.layers[1].tails.len(), "pass 2 carries the feet up the posts");
        assert!(output(Ctl::default(), &calm, &[]).unwrap().layers[8].is_empty(), "removal drops the narrow strip");
    }

    /// §2.9: enforcers grow by 0.8 mm and join the raw overhang before clustering, and keep a
    /// small cluster; blockers grow by 0.2 mm and come out of the output after clustering.
    #[test]
    fn enforcers_join_and_blockers_cut() {
        let slices: Vec<Vec<ExPolygon>> = (0..10).map(|l| vec![if l == 6 { rect(0.0, 0.0, 13.0, 10.0) } else { rect(0.0, 0.0, 10.0, 10.0) }]).collect();
        let s = r14(0.35);
        let h = heights(10);
        let mut enforce = vec![Vec::new(); 10];
        enforce[4] = vec![rect(4.0, 4.0, 4.5, 4.5)];
        let o = detect(Ctl::default(), &s, &slices, &h, &enforce, &[], None).unwrap().0;
        assert_eq!(o.layers[4].len(), 1, "a small enforced patch stays");
        let a = mm2(&o.layers[4]);
        assert!((a - 2.1 * 2.1).abs() < 1e-6, "grown by 0.8 mm, its square corners kept by the mitre: {a}");
        let mut block = vec![Vec::new(); 10];
        block[6] = vec![rect(11.0, 0.0, 14.0, 5.0)];
        let o = detect(Ctl::default(), &s, &slices, &h, &[], &block, None).unwrap().0;
        let left = mm2(&o.layers[6]);
        let expect = 2.667 * 10.0 - (13.0 - 10.8) * 5.2;
        assert!((left - expect).abs() < 0.01, "the strip less the blocker grown by 0.2 mm: {left} vs {expect}");
    }

    /// §2.9: a floating tail goes out on its own layer and then where floor(height × 10), in
    /// f32, is a multiple of 5 (5, 10, 15, 20, 28 … layers up), grown by 0.2 mm; only what sticks
    /// out past the layer below counts, so a straight column adds nothing after its start.
    #[test]
    fn tail_pieces_go_out_every_half_millimetre() {
        let mut emitted = Vec::new();
        let mut h = 0.0f32;
        for k in 0..30 {
            if k > 0 {
                h += 0.2;
            }
            if ((h * 10.0).floor() as i64) % 5 == 0 {
                emitted.push(k);
            }
        }
        assert_eq!(emitted, vec![0, 5, 10, 15, 20, 28], "f32 drift moves the hit past 5 mm");
        let mut slices: Vec<Vec<ExPolygon>> = vec![vec![rect(0.0, 0.0, 10.0, 10.0)]; 12];
        for s in slices.iter_mut().skip(4) {
            s.push(rect(20.0, 0.0, 22.0, 2.0));
        }
        let o = detect_r14(&slices, 30.0);
        // Layer 4: the cluster piece (tail and cantilever: not grown) and the tail piece, grown.
        assert_eq!(o.layers[4].len(), 2, "the cluster piece and the tail piece, no union");
        let areas: Vec<f64> = o.layers[4].iter().map(|e| (e.area_mm2() * 100.0).round() / 100.0).collect();
        assert!(areas.contains(&4.0), "the cluster piece as it is: {areas:?}");
        assert!(areas.iter().any(|&a| a > 4.0 + 4.0 * 2.0 * 0.2), "the tail piece grown by 0.2 mm: {areas:?}");
        assert!(o.layers[9].is_empty(), "5 layers up the column is straight: nothing sticks out");
    }
}

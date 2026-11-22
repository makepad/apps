use crate::{mesh, FilamentStats, Footprint, Issue, Materials, PlateStats, Report, Severity};
use fab_plate::{machine::Machine, Bounds, Part, PartMesh, Plate};
use makepad_fab_gcode::{process::Timeline, record::{flags, MoveKind, MoveRecord, Role}};
use makepad_fab_slicer::{clip::{difference_ex, intersection_ex, union_ex}, geom::{dist_point_segment, lerp, orient, total_area_mm2, BBox, ExPolygon, Polygon, Pt, SCALE}, grid::SegGrid, offset::{offset_mm, opening, DEFAULT_MITER}};
use makepad_fab_toolpath::{fill, par, prof, ExtrusionPath, Layer};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

fn point(e: &ExPolygon, z: f64) -> [f64; 3] {
    // Area centroid, with holes contributing negative area.
    let (mut sum, mut x, mut y) = (0.0, 0.0, 0.0);
    for ring in e.rings() {
        for i in 0..ring.pts.len() {
            let (a, b) = (ring.pts[i], ring.pts[(i + 1) % ring.pts.len()]);
            let cross = a.x_mm() * b.y_mm() - b.x_mm() * a.y_mm();
            sum += cross; x += (a.x_mm() + b.x_mm()) * cross; y += (a.y_mm() + b.y_mm()) * cross;
        }
    }
    if sum.abs() > 1e-12 {
        let c = Pt::from_mm(x / (3.0 * sum), y / (3.0 * sum));
        if e.contains(c) { return [c.x_mm(), c.y_mm(), z]; }
    }
    // A ring or concave region can have its centroid in empty space.
    // Place the marker just inside a contour edge in that case.
    for i in 0..e.contour.pts.len() {
        let (a, b) = (e.contour.pts[i], e.contour.pts[(i+1)%e.contour.pts.len()]);
        let len = a.dist(b);
        if len == 0.0 { continue; }
        let c = Pt::new(((a.x as f64+b.x as f64)*0.5 - (b.y-a.y) as f64 / len * 10.0).round() as i64,
            ((a.y as f64+b.y as f64)*0.5 + (b.x-a.x) as f64 / len * 10.0).round() as i64);
        if e.contains(c) { return [c.x_mm(), c.y_mm(), z]; }
    }
    let c = e.contour.pts.first().copied().unwrap_or_default();
    [c.x_mm(), c.y_mm(), z]
}

fn thin_width(e: &ExPolygon, line_width: f64) -> f64 {
    // Twice the largest surviving erosion radius (0.01 mm resolution).
    // This estimates a feature's printable width, including irregular fins.
    let (mut lo, mut hi) = (0.0, line_width * 0.5);
    while hi - lo > 0.005 {
        let mid = (lo + hi) * 0.5;
        if offset_mm(std::slice::from_ref(e), -mid).is_empty() { hi = mid; } else { lo = mid; }
    }
    lo + hi
}

#[derive(Clone, Copy)]
struct ThinLayer {
    footprint: Footprint,
    at: [f64; 3],
    width: f64,
    estimate: f64,
    area: f64,
    /// Of `area`, what no path prints.
    lost: f64,
}

fn thin_message(issue: &mut Issue, width: f64, lost: f64) {
    let what = if lost > 0.0 {
        format!("too thin to print; up to {lost:.3} mm² left out per layer")
    } else {
        format!("narrower than the {width:.2} mm line: prints as one line wider than the model")
    };
    issue.message = format!("Layers {}–{} · Z {:.2}–{:.2} mm: about {:.2} mm wide, {what}",
        issue.layer.unwrap(), issue.layer_to.unwrap(), issue.lowest_z, issue.highest_z, issue.measure);
}

/// Whether some path of `layer`'s object runs through `e`, given in the frame `corner` is the
/// plate position of.
fn printed_through(paths: &[&ExtrusionPath], e: &ExPolygon, corner: Pt) -> bool {
    let b = e.bbox();
    let inside = |q: Pt| q.x >= b.min.x && q.x <= b.max.x && q.y >= b.min.y && q.y <= b.max.y && e.contains(q);
    paths.iter().any(|path| path.pts.windows(2).any(|s| {
        let (a, c) = (s[0].sub(corner), s[1].sub(corner));
        inside(a) || inside(c) || inside(Pt::new((a.x + c.x) / 2, (a.y + c.y) / 2))
    }))
}

/// How the ends of hatch lines on one boundary edge of a piece lie against the anchors.
#[derive(Clone, Copy, PartialEq)]
enum EdgeEnds { In, Out, Mixed }

/// A hatch line's end lies within 2 units of the boundary edge it was clipped on (the
/// rotated vertices, the crossing and the rotation back are each rounded). With no anchor
/// segment within `NEAR` of an edge, that disc-swept edge holds no anchor boundary, so
/// every end on it is inside the anchors or every one is outside.
const NEAR: f64 = 16.0;

/// A floor printed in the air: on a layer up to this high (mm, its top)...
const FLOOR_REACH: f64 = 1.0;
/// ...at least this much (mm²) with nothing under it...
const FLOOR_MIN_AREA: f64 = 20.0;
/// ...and more than this share of the layer. A sphere on the bed adds two thirds of its second
/// layer, and a cone's first layers, more of theirs, stay under the area.
const FLOOR_SHARE: f64 = 0.75;

/// An overhang is told as uncovered when more than this (mm²) of it reaches past the step a layer
/// may take out with nothing under it and under a twentieth of that rests on support.
const UNCOVERED_MIN_AREA: f64 = 20.0;

/// Whether segments a-b and c-d come within `lim` of each other.
fn segs_near(a: Pt, b: Pt, c: Pt, d: Pt, lim: f64) -> bool {
    let crosses = orient(a, b, c).signum() * orient(a, b, d).signum() < 0 && orient(c, d, a).signum() * orient(c, d, b).signum() < 0;
    crosses || dist_point_segment(a, c, d) <= lim || dist_point_segment(b, c, d) <= lim || dist_point_segment(c, a, b) <= lim || dist_point_segment(d, a, b) <= lim
}

/// Every boundary edge of `e` in `fill::family`'s (ring, edge) order (flattened; the first
/// edge of each ring), classed against `touching`. Consecutive clear edges share a vertex,
/// so one point test answers a run of them.
fn edge_ends(e: &ExPolygon, touching: &SegGrid<u32>) -> (Vec<u32>, Vec<EdgeEnds>) {
    let (mut first, mut class) = (Vec::new(), Vec::new());
    for ring in e.rings() {
        first.push(class.len() as u32);
        let n = ring.pts.len();
        let mut run: Option<bool> = None;
        for i in 0..n {
            let (a, b) = (ring.pts[i], ring.pts[(i+1)%n]);
            let mut bb = BBox::of(&[a, b]);
            let pad = NEAR as i64 + 2;
            bb.min = Pt::new(bb.min.x - pad, bb.min.y - pad);
            bb.max = Pt::new(bb.max.x + pad, bb.max.y + pad);
            let mut near = false;
            touching.visit_box(bb, |c, d, _| near = near || segs_near(a, b, c, d, NEAR + 1.0));
            class.push(if near {
                run = None;
                EdgeEnds::Mixed
            } else if *run.get_or_insert_with(|| touching.contains_any(a)) {
                EdgeEnds::In
            } else {
                EdgeEnds::Out
            });
        }
    }
    (first, class)
}

/// Clearance of the disc [`rests_on`] looks for: the booleans move edges by under a unit (snap
/// rounding), so a disc this far from every edge of both operands stays whole inside their
/// intersection, which then has over pi * 2000^2 units^2 = 1.26e-5 mm^2.
const RESTS: f64 = 2000.0;

/// Whether island `e` provably overlaps the layer below by more than 1e-6 mm^2, without the
/// boolean: a point inside both, `RESTS` (plus rounding) clear of every edge of both. Points
/// are tried 8 µm in from up to 16 contour edges spread around the island.
fn rests_on(e: &ExPolygon, below: &SegGrid<u32>) -> bool {
    let clear = RESTS + 2.0;
    let depth = 2.0 * clear;
    let c = &e.contour.pts;
    let n = c.len();
    for i in (0..n).step_by(n.div_ceil(16).max(1)) {
        let (a, b) = (c[i], c[(i+1)%n]);
        let len = a.dist(b);
        if len < 2.0 * depth { continue; }
        // The contour is counter-clockwise: inside is on the left.
        let m = lerp(a, b, 0.5);
        let q = Pt::new(m.x - ((b.y-a.y) as f64 / len * depth).round() as i64, m.y + ((b.x-a.x) as f64 / len * depth).round() as i64);
        if below.contains_any(q) && below.nearest(q, |_| 0u8).is_some_and(|(d, _)| d > clear)
            && e.rings().all(|r| (0..r.pts.len()).all(|j| dist_point_segment(q, r.pts[j], r.pts[(j+1)%r.pts.len()]) > clear))
            && e.contains(q) {
            return true;
        }
    }
    false
}

/// Sample across the unsupported region with the slicer's line clipper.
/// Only spans with material at BOTH endpoints are bridges. Candidate
/// directions are the planner's 5-degree sweep and each boundary edge's
/// direction and normal, on a half-degree lattice. Choose the direction whose
/// lines are anchored over the greatest share of their length, breaking
/// near-ties by span, rather than demanding that holes leave every thread
/// anchored: a piece held all round, as a gap between support lines is, is
/// spanned across, however many hatch lines fit along it. A line anchored at
/// both ends but no longer than a line width lies on its anchors (a corner cut
/// within their tolerance): a piece with no longer one is no bridge.
/// `touching`: the anchors (the layer below) grown by 0.01 mm, as a locator.
/// The hatch lines run through the part frame's origin, so a nanometre's move
/// of the piece's outline moves none of them.
/// Unless `measure`, the first line longer than a line width with both ends anchored answers:
/// only whether the piece is a bridge counts, not its span. The third result is the anchored
/// share of the chosen line family (only meaningful when measuring).
pub(crate) fn bridge_span(e: &ExPolygon, touching: &SegGrid<u32>, width: f64, measure: bool) -> Option<(f64, [f64; 2], f64)> {
    let (first, class) = edge_ends(e, touching);
    bridge_span_classed(e, width, measure, Pt::new(0, 0), &first, &class, |p| touching.contains_any(p))
}

/// Mesh ceilings already know which boundary segments touch supporting walls.
/// Query those segments directly instead of constructing thin anchor polygons
/// and repeatedly winding all their overlapping rings for every hatch endpoint.
/// The hatch lines run through `pivot`.
pub(crate) fn bridge_span_edges(e: &ExPolygon, supporting: &SegGrid<u32>, width: f64, measure: bool, pivot: Pt) -> Option<(f64, [f64; 2], f64)> {
    let (mut first, mut class) = (Vec::new(), Vec::new());
    let tolerance = 0.01 * SCALE;
    for ring in e.rings() {
        first.push(class.len() as u32);
        for i in 0..ring.pts.len() {
            let (a, b) = (ring.pts[i], ring.pts[(i+1)%ring.pts.len()]);
            let mut bb = BBox::of(&[a, b]);
            let pad = tolerance.ceil() as i64 + 2;
            bb.min = Pt::new(bb.min.x - pad, bb.min.y - pad);
            bb.max = Pt::new(bb.max.x + pad, bb.max.y + pad);
            let mut near = false;
            supporting.visit_box(bb, |c, d, _| near = near || segs_near(a, b, c, d, tolerance + 2.0));
            class.push(if near { EdgeEnds::Mixed } else { EdgeEnds::Out });
        }
    }
    bridge_span_classed(e, width, measure, pivot, &first, &class, |p| {
        supporting.nearest(p, |_| 0u8).is_some_and(|(d, _)| d <= tolerance + 2.0)
    })
}

fn bridge_span_classed(e: &ExPolygon, width: f64, measure: bool, pivot: Pt, first: &[u32], class: &[EdgeEnds], at_mixed: impl Fn(Pt) -> bool) -> Option<(f64, [f64; 2], f64)> {
    // No line end can be anchored: no bridge at any angle.
    if class.iter().all(|&c| c == EdgeEnds::Out) { return None; }
    // A cap carried only by a convex central stem cannot bridge: a chord
    // between two points on that hole lies in the hole, not in the ceiling.
    // Prove this from edge classes before hatching every candidate direction.
    let mut anchored_rings = e.rings().enumerate().filter(|(i, ring)| {
        class[first[*i] as usize..first[*i] as usize + ring.pts.len()].iter().any(|&c| c != EdgeEnds::Out)
    });
    if let Some((i, ring)) = anchored_rings.next() {
        if i > 0 && anchored_rings.next().is_none() {
            let r = &ring.pts;
            let mut sign = 0;
            let convex = (0..r.len()).all(|k| {
                let turn = orient(r[k], r[(k+1)%r.len()], r[(k+2)%r.len()]).signum();
                if turn == 0 { return true; }
                if sign == 0 { sign = turn; }
                sign == turn
            });
            if convex { return None; }
        }
    }
    let at_anchor = |p: Pt, (ring, edge): (u32, u32)| match class[(first[ring as usize] + edge) as usize] {
        EdgeEnds::In => true,
        EdgeEnds::Out => false,
        EdgeEnds::Mixed => at_mixed(p),
    };
    // The candidate directions on a half-degree lattice: a finely tessellated curve (every edge
    // its own direction) hatches the piece once per half degree, not once per edge, and a
    // nanometre's move of an edge moves no direction.
    let step = 0.5f64.to_radians();
    let mut lattice: [bool; 360] = std::array::from_fn(|k| k % 10 == 0);
    for ring in e.rings() {
        for i in 0..ring.pts.len() {
            let (a, b) = (ring.pts[i], ring.pts[(i+1)%ring.pts.len()]);
            let angle = ((b.y-a.y) as f64).atan2((b.x-a.x) as f64);
            for d in [angle, angle + std::f64::consts::FRAC_PI_2] {
                lattice[((d / step).round() as i64).rem_euclid(360) as usize] = true;
            }
        }
    }
    let mut best: Option<(f64, f64, f64, [f64; 2])> = None;
    let mut spans = false;
    for a in (0..360).filter(|&k| lattice[k]).map(|k| k as f64 * step) {
        let lines = fill::family(std::slice::from_ref(e), a, width.max(0.01), pivot, fill::Align::Grid);
        let (mut anchored, mut total, mut longest, mut longest_any, mut at) = (0.0, 0.0, 0.0, 0.0f64, [0.0; 2]);
        for line in lines {
            let len = line.a.dist(line.b) / SCALE;
            total += len;
            longest_any = longest_any.max(len);
            if at_anchor(line.a, line.ea) && at_anchor(line.b, line.eb) {
                anchored += len;
                if len > longest { longest = len; at = [(line.a.x_mm()+line.b.x_mm())*0.5, (line.a.y_mm()+line.b.y_mm())*0.5]; }
                if !measure && len > width { return Some((len, at, 0.0)); }
            }
        }
        if anchored <= 0.0 { continue; }
        spans |= longest > width;
        let share = anchored / total.max(1e-12);
        if best.is_none_or(|(most, span, _, _)| share > most * 1.0001 || ((share-most).abs() <= most * 0.0001 && longest_any < span - 1e-6)) {
            best = Some((share, longest_any, longest, at));
        }
    }
    best.filter(|_| spans).map(|(share, _, len, at)| (len, at, share))
}

pub(crate) fn bridge_issue(object: Option<&str>, at: [f64; 3], footprint: Footprint, span: f64) -> Issue {
    Issue::new("bridge", Severity::Warning, object, at, footprint, span,
        format!("{span:.2} mm unsupported span between anchored edges"),
        "Shorten the bridge, rotate the part, or add support beneath the span.")
}

/// Boolean subtraction splits the boundary at anchor transitions. Keep its
/// shared edges (including hole edges), then measure distance to the nearest
/// anchor, not the length of an oblique chord across the unsupported region.
fn cantilever_reach(e: &ExPolygon, touching: &SegGrid<u32>, width: f64) -> Option<(f64, Pt)> {
    let edges: Vec<_> = e.rings().flat_map(|r| {
        (0..r.pts.len()).map(move |i| (r.pts[i], r.pts[(i + 1) % r.pts.len()]))
    }).collect();
    let anchors: Vec<_> = edges.iter().copied().filter(|&(a, b)| {
        a != b && touching.contains_any(lerp(a, b, 0.5))
    }).map(|(a, b)| (a, b, ())).collect();
    if anchors.is_empty() { return None; }
    let anchors = SegGrid::new(anchors);
    let mut farthest = (0.0, e.contour.pts[0]);
    for (a, b) in edges {
        // Half-bead spacing bounds boundary sampling error to a quarter bead,
        // and finds maxima inside long edges as well as at polygon vertices.
        let steps = (a.dist(b) / (SCALE * width * 0.5)).ceil().max(1.0) as usize;
        for i in 0..=steps {
            let p = lerp(a, b, i as f64 / steps as f64);
            let reach = anchors.nearest(p, |_| 0u8).map_or(f64::INFINITY, |x| x.0) / SCALE;
            if reach > farthest.0 { farthest = (reach, p); }
        }
    }
    Some(farthest)
}

#[derive(Clone, Copy)]
struct CantileverLayer {
    footprint: Footprint,
    at: [f64; 3],
    reach: f64,
    area: f64,
}

/// Add sliced geometry, extrusion validation and estimates to a mesh report.
/// `layers` are the original toolpath layers, including empty ones. No mesh
/// is sliced again, and painted filament regions retain their object IDs.
pub fn analyse_slice(machine: &Machine, plate: &Plate, materials: &Materials, layers: &[Layer], timeline: &Timeline, report: &mut Report) {
    analyse_slice_with(machine, plate, materials, layers, timeline, &mut timeline.moves().copied(), report, &mut |_, _, _| true);
}

/// [`analyse_slice`], telling `progress` after every layer (layers done, of
/// how many, the report so far); it stops (returning false) as soon as
/// `progress` returns false.
pub fn analyse_slice_with(machine: &Machine, plate: &Plate, materials: &Materials, layers: &[Layer], timeline: &Timeline, records: &mut dyn Iterator<Item = MoveRecord>, report: &mut Report, progress: &mut dyn FnMut(usize, usize, &Report) -> bool) -> bool {
    analyse(machine, plate, materials, layers, timeline, records, report, progress, &|| false, None, &[])
}

/// [`analyse_slice_with`], keeping each part's findings per layer in `memo` and reusing those
/// whose key is unchanged. `keys[i]` is plate part `i`'s content key: equal keys, equal
/// layers in the part's own frame. `kept[i]`: where plate part `i` keeps its trees out, as its
/// plan says (none past its end). Once `stale` is true it stops between part-layers too
/// (returning false), and the memo keeps what it found whole.
#[allow(clippy::too_many_arguments)]
pub fn analyse_slice_memo(machine: &Machine, plate: &Plate, materials: &Materials, layers: &[Layer], timeline: &Timeline, records: &mut dyn Iterator<Item = MoveRecord>, report: &mut Report, progress: &mut dyn FnMut(usize, usize, &Report) -> bool, stale: &(dyn Fn() -> bool + Sync), keys: &[u64], memo: &SliceMemo, kept: &[Option<KeptOut>]) -> bool {
    analyse(machine, plate, materials, layers, timeline, records, report, progress, stale, Some((keys, memo)), kept)
}

/// Where a part keeps its trees out, as its plan says
/// ([`makepad_fab_toolpath::skeleton::Skeleton::keep_out`]): per model layer, in the plan's frame.
/// What of the part's overhang lies over it on the layer under is the person's choice, which
/// the plan tells as information: no uncovered overhang is told of it.
#[derive(Clone)]
pub struct KeptOut {
    pub regions: Arc<Vec<Vec<ExPolygon>>>,
    /// Each model layer's print z (mm), lowest first.
    pub zs: Vec<f64>,
    /// Where the plan's frame lies on the plate.
    pub offset: Pt,
}

impl KeptOut {
    /// What is kept out under the model layer printed at `z`, on the plate: none when no model
    /// layer prints there, or nothing is kept out under it.
    fn under(&self, z: f64) -> Option<Vec<ExPolygon>> {
        let d = self.zs.partition_point(|&m| m < z - Z_MATCH);
        if self.zs.get(d).is_none_or(|&m| (m - z).abs() > Z_MATCH) {
            return None;
        }
        let set = self.regions.get(d.checked_sub(1)?).filter(|s| !s.is_empty())?;
        Some(set.iter().cloned().map(|mut e| { e.translate(self.offset); e }).collect())
    }
}

/// How near (mm) a layer's print z is to a model layer's for it to be that layer.
const Z_MATCH: f64 = 1e-4;

/// A key word for what `kept` keeps out under the layer at `z`: 0 for none.
fn kept_word(kept: Option<&KeptOut>, z: f64) -> u64 {
    let Some(set) = kept.and_then(|k| k.under(z)) else { return 0 };
    let mut key = Key::new();
    for e in &set {
        for ring in std::iter::once(&e.contour).chain(&e.holes) {
            key.word(ring.pts.len() as u64);
            for q in &ring.pts {
                key.word(q.x as u64);
                key.word(q.y as u64);
            }
        }
    }
    let k = key.finish();
    (k as u64) ^ ((k >> 64) as u64) | 1
}

/// The sliced checks' findings kept between revisions: one part's on one layer, in the part's
/// own frame, under a key covering everything they read (the part's content, the parts within
/// reach and where they stand from it, the layer's z and height, the check settings). A
/// finding is a pure function of its key, or it is not reused.
#[derive(Default)]
pub struct SliceMemo {
    found: Mutex<HashMap<u128, Arc<PartFound>>>,
}

#[cfg(test)]
impl SliceMemo {
    /// What it holds: each key and the findings' address, in key order.
    pub(crate) fn held(&self) -> Vec<(u128, usize)> {
        let mut held: Vec<_> = self.found.lock().unwrap().iter().map(|(k, f)| (*k, Arc::as_ptr(f) as usize)).collect();
        held.sort();
        held
    }
}

#[allow(clippy::too_many_arguments)]
fn analyse(machine: &Machine, plate: &Plate, materials: &Materials, layers: &[Layer], timeline: &Timeline, records: &mut dyn Iterator<Item = MoveRecord>, report: &mut Report, progress: &mut dyn FnMut(usize, usize, &Report) -> bool, stale: &(dyn Fn() -> bool + Sync), memo: Option<(&[u64], &SliceMemo)>, kept: &[Option<KeptOut>]) -> bool {
    report.sliced = true;
    for object in &mut report.objects {
        object.mass_g = 0.0;
        object.first_layer_area_mm2 = 0.0;
        object.max_height_per_nozzle.fill(0.0);
    }
    let n = layers.len();
    let frames = Frames::of(layers, plate.parts.len());
    // Each part's latest model layer below each layer (layer index per part).
    let mut below: Vec<BTreeMap<usize, usize>> = Vec::with_capacity(n);
    let mut latest: BTreeMap<usize, usize> = BTreeMap::new();
    for (k, layer) in layers.iter().enumerate() {
        below.push(latest.clone());
        for o in layer.objects.iter().filter(|o| !o.support_only) {
            latest.insert(o.object, k);
        }
    }
    let max_height = layers.iter().map(|l| l.height).fold(0.0, f64::max);
    let geo = Geometry::new(layers, &frames);
    // The parts checked on each layer, in the layer's order, and their keys.
    let mut units: Vec<(usize, usize)> = Vec::new();
    for (k, layer) in layers.iter().enumerate() {
        for o in layer.objects.iter().filter(|o| !o.support_only && !o.slices.is_empty() && o.object < plate.parts.len()) {
            if !units.iter().rev().take_while(|u| u.0 == k).any(|u| u.1 == o.object) {
                units.push((k, o.object));
            }
        }
    }
    let keyed: Option<(Vec<u128>, &SliceMemo)> = memo.map(|(keys, memo)| {
        let supports: Vec<bool> = plate.parts.iter().map(|p| materials.of_part(p).supports).collect();
        let unit_keys: Vec<u128> = units.iter().map(|&(k, p)| unit_key(keys, &supports, materials, &layers[k], &frames, max_height, p, kept_word(kept.get(p).and_then(Option::as_ref), layers[k].print_z))).collect();
        // What another revision left beyond this plate's own goes once it outgrows it.
        let mut found = memo.found.lock().unwrap();
        if found.len() > 2 * unit_keys.len() + 64 {
            let mine: HashSet<u128> = unit_keys.iter().copied().collect();
            found.retain(|key, _| mine.contains(key));
        }
        drop(found);
        (unit_keys, memo)
    });
    let unit = |u: usize| -> Arc<PartFound> {
        let (k, p) = units[u];
        let check = || Arc::new(check_part(plate, materials, layers, &frames, &geo, &below, max_height, p, k, kept.get(p).and_then(Option::as_ref)));
        let Some((keys, memo)) = keyed.as_ref() else { return check() };
        if let Some(found) = memo.found.lock().unwrap().get(&keys[u]) {
            return found.clone();
        }
        let found = check();
        memo.found.lock().unwrap().insert(keys[u], found.clone());
        found
    };
    // One active run per object, across its consecutive model layers. Report
    // indices stay stable until the final sort; support-only planes do not
    // interrupt a run. Retain only indices and the largest line width.
    let mut thin_runs: BTreeMap<&str, (usize, f64, f64)> = BTreeMap::new();
    let mut cantilever_runs: BTreeMap<&str, usize> = BTreeMap::new();
    // Layers in chunks: each chunk's part layers checked in parallel, then folded
    // into the report in layer order (the runs need that order).
    let chunk = (par::threads() * 2).max(8);
    let (mut k0, mut u0) = (0, 0);
    while k0 < n {
        if !progress(k0, n, report) {
            return false;
        }
        let k1 = (k0 + chunk).min(n);
        let u1 = u0 + units[u0..].iter().take_while(|u| u.0 < k1).count();
        // A stale revision's chunk ends at the part-layers under way.
        let Ok(part_found) = par::try_map(par::Ctl { cancel: Some(stale), ..Default::default() }, u1 - u0, |d| unit(u0 + d)) else {
            return false;
        };
        let found: Vec<LayerFound> = (k0..k1).map(|k| {
            let range = units[u0..u1].iter().enumerate().filter(|(_, u)| u.0 == k).map(|(d, u)| (u.1, &*part_found[d]));
            layer_found(plate, &frames, &layers[k], range)
        }).collect();
        for (d, f) in found.into_iter().enumerate() {
            let layer = &layers[k0 + d];
            for (object, area) in f.first_layer_area {
                if let Some(stats) = report.objects.iter_mut().find(|s| s.object == object) {
                    stats.first_layer_area_mm2 += area;
                }
            }
            report.issues.extend(f.issues);
            // A model layer without a surviving thin region ends that object's
            // run, including an explicitly empty layer. Painted parts with the
            // same object ID contribute to the same per-layer total.
            for o in layer.objects.iter().filter(|o| !o.support_only) {
                if let Some(part) = plate.parts.get(o.object) {
                    if !f.thin.iter().any(|(ob, _)| ob == &part.object) { thin_runs.remove(part.object.as_str()); }
                    if !f.cantilevers.iter().any(|(ob, _)| ob == &part.object) { cantilever_runs.remove(part.object.as_str()); }
                }
            }
            for (object, c) in &f.cantilevers {
                let object: &str = plate.parts.iter().find(|p| &p.object == object).map(|p| p.object.as_str()).unwrap_or("");
                let continuing = cantilever_runs.get(object).copied().filter(|index| {
                    layer.print_z - report.issues[*index].highest_z <= layer.height + 0.01
                });
                let index = if let Some(index) = continuing {
                    let issue = &mut report.issues[index];
                    issue.measure = issue.measure.max(c.reach);
                    issue.area_mm2 = issue.area_mm2.max(c.area);
                    index
                } else {
                    let mut issue = Issue::new("cantilever", Severity::Warning, Some(object), c.at, c.footprint, c.reach, "",
                        "Chamfer underneath, shorten the shelf, rotate the part, or add support.");
                    issue.area_mm2 = c.area;
                    issue.layer = Some(layer.index as u32 + 1);
                    let index = report.issues.len();
                    report.issues.push(issue);
                    index
                };
                let issue = &mut report.issues[index];
                issue.layer_to = Some(layer.index as u32 + 1);
                issue.highest_z = layer.print_z;
                issue.message = format!("Layers {}–{} · Z {:.2}–{:.2} mm: {:.2} mm unsupported reach from an anchored edge; up to {:.1} mm² per layer",
                    issue.layer.unwrap(), issue.layer_to.unwrap(), issue.lowest_z, issue.highest_z, issue.measure, issue.area_mm2);
                cantilever_runs.insert(object, index);
            }
            for (object, t) in &f.thin {
                let object: &str = plate.parts.iter().find(|p| &p.object == object).map(|p| p.object.as_str()).unwrap_or("");
                let continuing = thin_runs.get(object).copied().filter(|(index, ..)| {
                    layer.print_z - report.issues[*index].highest_z <= layer.height + 0.01
                });
                let (index, width, lost) = if let Some((index, width, lost)) = continuing {
                    let issue = &mut report.issues[index];
                    issue.measure = issue.measure.min(t.estimate);
                    // Area is a cross-section, not a sum over the layer stack.
                    issue.area_mm2 = issue.area_mm2.max(t.area);
                    (index, width.max(t.width), lost.max(t.lost))
                } else {
                    let mut issue = Issue::new("thin_wall", Severity::Warning, Some(object), t.at, t.footprint, t.estimate, "",
                        "Thicken this feature or choose a smaller nozzle and line width.");
                    issue.area_mm2 = t.area;
                    issue.layer = Some(layer.index as u32 + 1);
                    let index = report.issues.len();
                    report.issues.push(issue);
                    (index, t.width, t.lost)
                };
                let issue = &mut report.issues[index];
                issue.layer_to = Some(layer.index as u32 + 1);
                issue.highest_z = layer.print_z;
                thin_message(issue, width, lost);
                thin_runs.insert(object, (index, width, lost));
            }
        }
        (k0, u0) = (k1, u1);
    }
    // Only this plate's findings stay for the next revision.
    if let Some((keys, memo)) = keyed.as_ref() {
        let mine: HashSet<u128> = keys.iter().copied().collect();
        memo.found.lock().unwrap().retain(|key, _| mine.contains(key));
    }
    {
        let _t = prof::scope(prof::Stage::CheckMoves);
        // A heat-set insert is wider than its hole by design: the iron melts it in after the print.
        let inserts: Vec<_> = plate.inserts().into_iter().filter(|(_, i, _)| i.melt.is_none()).map(|(p, i, _)| (p, i)).collect();
        if inserts.is_empty() {
            moves(machine, plate, materials, timeline, records, report);
        } else {
            let mut check = crate::insert::BeadCheck::new(&inserts);
            moves(machine, plate, materials, timeline, &mut records.inspect(|m| check.record(m)), report);
            report.issues.extend(check.finish());
            report.issues.extend(crate::insert::uncovered(plate, layers));
        }
    }
    report.sort();
    progress(n, n, report);
    true
}

/// How far past a part's footprint its checks read the layers below: the anchors grow by
/// 0.01 mm, with margin.
const REACH_MM: f64 = 0.1;

/// Each part's frame and reach. A part is checked in its own frame, the plate less its corner
/// (the low corner of everything it prints), so where it stands never changes what is found.
/// Its checks read the layers below only within its footprint, so only the parts whose
/// footprints come within [`REACH_MM`] of its own are read with it.
struct Frames {
    corner: Vec<Pt>,
    /// Per part: itself and the parts within reach, in part order.
    near: Vec<Vec<usize>>,
}

impl Frames {
    fn of(layers: &[Layer], parts: usize) -> Frames {
        let mut boxes = vec![BBox::empty(); parts];
        for o in layers.iter().flat_map(|l| &l.objects) {
            let Some(b) = boxes.get_mut(o.object) else { continue };
            for e in o.slices.iter() {
                b.merge(&e.bbox());
            }
            for path in o.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)).filter(|p| is_support(p.role)) {
                let r = (path.width * SCALE * 0.5).ceil() as i64;
                for q in &path.pts {
                    b.add(Pt::new(q.x - r, q.y - r));
                    b.add(Pt::new(q.x + r, q.y + r));
                }
            }
        }
        let r = (REACH_MM * SCALE) as i64;
        let near = |a: &BBox, b: &BBox| !a.is_empty() && !b.is_empty()
            && a.min.x - r <= b.max.x && b.min.x - r <= a.max.x && a.min.y - r <= b.max.y && b.min.y - r <= a.max.y;
        Frames {
            corner: boxes.iter().map(|b| if b.is_empty() { Pt::default() } else { b.min }).collect(),
            near: (0..parts).map(|p| (0..parts).filter(|&q| q == p || near(&boxes[p], &boxes[q])).collect()).collect(),
        }
    }

    /// `regions` of part `q`'s frame in part `p`'s.
    fn moved(&self, q: usize, p: usize, regions: &[ExPolygon]) -> Vec<ExPolygon> {
        let d = self.corner[q].sub(self.corner[p]);
        regions.iter().cloned().map(|mut e| { e.translate(d); e }).collect()
    }

    /// Plate `regions` in part `p`'s frame.
    fn local(&self, p: usize, regions: &[ExPolygon]) -> Vec<ExPolygon> {
        let d = Pt::new(-self.corner[p].x, -self.corner[p].y);
        regions.iter().cloned().map(|mut e| { e.translate(d); e }).collect()
    }
}

fn is_support(role: Role) -> bool {
    matches!(role, Role::Support | Role::SupportInterface | Role::SupportTransition)
}

/// Each part's model surface and support area per layer, in its own frame, made when first
/// asked for.
struct Geometry<'a> {
    layers: &'a [Layer],
    frames: &'a Frames,
    parts: usize,
    surfaces: Vec<OnceLock<Vec<ExPolygon>>>,
    supports: Vec<OnceLock<Vec<ExPolygon>>>,
}

impl<'a> Geometry<'a> {
    fn new(layers: &'a [Layer], frames: &'a Frames) -> Self {
        let (parts, cells) = (frames.corner.len(), frames.corner.len() * layers.len());
        Geometry { layers, frames, parts, surfaces: (0..cells).map(|_| OnceLock::new()).collect(), supports: (0..cells).map(|_| OnceLock::new()).collect() }
    }

    /// What part `p`'s model leaves on layer `k` for the layers above.
    fn surface(&self, p: usize, k: usize) -> &[ExPolygon] {
        self.surfaces[k * self.parts + p].get_or_init(|| {
            let regions: Vec<ExPolygon> = self.layers[k].objects.iter().filter(|o| o.object == p && !o.support_only).flat_map(|o| self.frames.local(p, &o.slices)).collect();
            union_ex(&regions)
        })
    }

    /// The support part `p` prints on layer `k`, as area: each segment of its lines a [`band`].
    fn support(&self, p: usize, k: usize) -> &[ExPolygon] {
        self.supports[k * self.parts + p].get_or_init(|| {
            let _t = prof::scope(prof::Stage::CheckSupportArea);
            let corner = self.frames.corner[p];
            let mut regions = Vec::new();
            for o in self.layers[k].objects.iter().filter(|o| o.object == p) {
                for path in o.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)).filter(|p| is_support(p.role)) {
                    for (i, segment) in path.pts.windows(2).enumerate() {
                        if !path.dry.get(i).copied().unwrap_or(false) {
                            regions.extend(band(segment[0].sub(corner), segment[1].sub(corner), path.width * SCALE * 0.5));
                        }
                    }
                }
            }
            union_ex(&regions)
        })
    }
}

/// What a line of half-width `d` prints round its segment `a` `b`: the rectangle `d` each side of
/// it, with square ends `d` past its own, wound counter-clockwise; nothing for a point. A lone
/// segment's offset band to the nm, without the offset's corner joins and union.
fn band(a: Pt, b: Pt, d: f64) -> Option<ExPolygon> {
    let (dx, dy) = ((b.x - a.x) as f64, (b.y - a.y) as f64);
    let len = dx.hypot(dy);
    if len == 0.0 {
        return None;
    }
    let (ux, uy) = (dx / len * d, dy / len * d);
    let (ax, ay, bx, by) = (a.x as f64, a.y as f64, b.x as f64, b.y as f64);
    let at = |x: f64, y: f64| Pt::new(x.round() as i64, y.round() as i64);
    Some(ExPolygon::new(Polygon::new(vec![at(ax - ux + uy, ay - uy - ux), at(bx + ux + uy, by + uy - ux), at(bx + ux - uy, by + uy + ux), at(ax - ux - uy, ay - uy + ux)])))
}

/// Words into a 128-bit key: two independent 64-bit mixes.
struct Key(u64, u64);

impl Key {
    fn new() -> Key {
        Key(0x9e37_79b9_7f4a_7c15, 0xc2b2_ae3d_27d4_eb4f)
    }
    fn word(&mut self, w: u64) {
        self.0 = mix(self.0 ^ w);
        self.1 = mix(self.1.wrapping_add(w).rotate_left(23) ^ 0x1656_67b1_9e37_79f9);
    }
    fn finish(&self) -> u128 {
        (self.0 as u128) << 64 | self.1 as u128
    }
}

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Changes whenever what a part layer's findings read or hold changes.
const KEY_VERSION: u64 = 3;

/// Part `p`'s key on `layer`: its content, the parts within its reach (content and where they
/// stand from it), the layer's z and height, the plate's tallest layer (the support window),
/// the check settings and what the part keeps its trees out of under it (`kept`, a
/// [`kept_word`]).
#[allow(clippy::too_many_arguments)]
fn unit_key(keys: &[u64], supports: &[bool], materials: &Materials, layer: &Layer, frames: &Frames, max_height: f64, p: usize, kept: u64) -> u128 {
    let mut key = Key::new();
    for w in [KEY_VERSION, keys[p], layer.print_z.to_bits(), layer.height.to_bits(), max_height.to_bits(), materials.line_width.to_bits(),
        materials.max_bridge_length.to_bits(), materials.support_gap.to_bits(), supports[p] as u64, kept] {
        key.word(w);
    }
    for &q in &frames.near[p] {
        let d = frames.corner[q].sub(frames.corner[p]);
        for w in [keys[q], d.x as u64, d.y as u64] {
            key.word(w);
        }
    }
    key.finish()
}

/// One layer's findings, before the runs across layers are merged.
#[derive(Default)]
struct LayerFound {
    /// mid_air, floor_in_air, uncovered_overhang and bridge, in check order.
    issues: Vec<Issue>,
    /// Per object (in first-seen order).
    thin: Vec<(String, ThinLayer)>,
    cantilevers: Vec<(String, CantileverLayer)>,
    first_layer_area: Vec<(String, f64)>,
}

/// One part's findings on one layer, in its own frame: its issues still without their object,
/// plate position and layer number.
#[derive(Default)]
struct PartFound {
    /// mid_air, floor_in_air, uncovered_overhang and bridge, in check order.
    issues: Vec<Issue>,
    thin: Option<ThinLayer>,
    cantilever: Option<CantileverLayer>,
    first_layer_area: Vec<f64>,
}

/// A layer's findings from its parts' (`found`, in the layer's order): on the plate, at the
/// layer, parts of one object together.
fn layer_found<'a>(plate: &'a Plate, frames: &Frames, layer: &Layer, found: impl Iterator<Item = (usize, &'a PartFound)>) -> LayerFound {
    let mut out = LayerFound::default();
    let mut thin: BTreeMap<&str, ThinLayer> = BTreeMap::new();
    let mut cantilevers: BTreeMap<&str, CantileverLayer> = BTreeMap::new();
    for (p, f) in found {
        let object = plate.parts[p].object.as_str();
        let (x, y) = (frames.corner[p].x_mm(), frames.corner[p].y_mm());
        let at = |a: [f64; 3]| [a[0] + x, a[1] + y, a[2]];
        for issue in &f.issues {
            let mut issue = issue.clone();
            issue.object = Some(object.to_string());
            issue.at_mm = at(issue.at_mm);
            issue.footprint = issue.footprint.translated([x, y]);
            issue.layer = Some(layer.index as u32 + 1);
            out.issues.push(issue);
        }
        out.first_layer_area.extend(f.first_layer_area.iter().map(|&a| (object.to_string(), a)));
        if let Some(t) = &f.thin {
            let entry = thin.entry(object).or_insert(ThinLayer { footprint: t.footprint.translated([x, y]), at: at(t.at), width: t.width, estimate: t.estimate, area: 0.0, lost: 0.0 });
            entry.footprint = entry.footprint.union(t.footprint.translated([x, y]));
            entry.estimate = entry.estimate.min(t.estimate);
            entry.width = entry.width.max(t.width);
            entry.area += t.area;
            entry.lost += t.lost;
        }
        if let Some(c) = &f.cantilever {
            let entry = cantilevers.entry(object).or_insert(CantileverLayer { footprint: c.footprint.translated([x, y]), at: at(c.at), reach: c.reach, area: 0.0 });
            if c.reach > entry.reach { entry.reach = c.reach; entry.at = at(c.at); }
            entry.footprint = entry.footprint.union(c.footprint.translated([x, y]));
            entry.area += c.area;
        }
    }
    out.thin = thin.into_iter().map(|(o, t)| (o.to_string(), t)).collect();
    out.cantilevers = cantilevers.into_iter().map(|(o, c)| (o.to_string(), c)).collect();
    out
}

/// Part `p`'s findings on layer `k`, in its own frame. `kept`: where the part keeps its trees
/// out, as its plan says.
#[allow(clippy::too_many_arguments)]
fn check_part(plate: &Plate, materials: &Materials, layers: &[Layer], frames: &Frames, geo: &Geometry, below: &[BTreeMap<usize, usize>], max_height: f64, p: usize, k: usize, kept: Option<&KeptOut>) -> PartFound {
    let layer = &layers[k];
    let part = &plate.parts[p];
    let near = &frames.near[p];
    let mut out = PartFound::default();
    // The configured air gap is intentional; support below that gap
    // still anchors the next object layer.
    let mut under_support = Vec::new();
    for j in (0..k).rev() {
        let dz = layer.print_z - layers[j].print_z;
        if dz > max_height + materials.support_gap + 0.01 || dz > layer.height + materials.support_gap + 0.01 { break; }
        for &q in near {
            under_support.extend(frames.moved(q, p, geo.support(q, j)));
        }
    }
    // Support planes and short final layers can interleave model layers.
    // Keep each part's last surface, but never bridge an empty Z gap.
    let lower_t = prof::scope(prof::Stage::CheckLower);
    let lower = union_ex(&below[k].iter().filter(|(q, &j)| near.contains(q) && layer.print_z - layers[j].print_z <= layer.height + 0.01)
        .flat_map(|(&q, &j)| frames.moved(q, p, geo.surface(q, j))).collect::<Vec<_>>());
    let under_support = union_ex(&under_support);
    let mut under = lower.to_vec(); under.extend_from_slice(&under_support);
    let under = union_ex(&under);
    drop(lower_t);
    let mut under_grid: Option<SegGrid<u32>> = None;
    for o in layer.objects.iter().filter(|o| o.object == p) {
        if o.slices.is_empty() || o.support_only { continue; }
        let slices = frames.local(p, &o.slices);
        let width = o.islands.iter().flat_map(|i| &i.walls).find(|p| matches!(p.role, Role::OuterWall | Role::OverhangWall)).map(|p| p.width).unwrap_or(materials.line_width).max(0.01);
        let thin_t = prof::scope(prof::Stage::CheckThin);
        let paths: Vec<&ExtrusionPath> = o.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)).collect();
        let opened = opening(&slices, width * SCALE * 0.5, width * SCALE * 0.5, DEFAULT_MITER);
        for e in difference_ex(&slices, &opened) {
            let area = e.area_mm2();
            // Opening/boolean roundoff is not a printable feature. Reject
            // tiny areas before the more expensive width estimate, then
            // reject long but numerically narrow slivers as well.
            if area < (0.25 * width).powi(2) { continue; }
            let estimate = thin_width(&e, width);
            if estimate < 0.05 * width { continue; }
            let entry = out.thin.get_or_insert_with(|| ThinLayer { footprint: Footprint::of_box(e.bbox()), at: point(&e, layer.print_z), width, estimate, area: 0.0, lost: 0.0 });
            entry.footprint = entry.footprint.union(Footprint::of_box(e.bbox()));
            entry.estimate = entry.estimate.min(estimate);
            entry.width = entry.width.max(width);
            entry.area += area;
            if !printed_through(&paths, &e, frames.corner[p]) {
                entry.lost += area;
            }
        }
        drop(thin_t);
        if layer.print_z <= layer.height + 0.01 {
            out.first_layer_area.push(total_area_mm2(&slices));
            continue;
        }
        let islands_t = prof::scope(prof::Stage::CheckIslands);
        for e in &slices {
            // An island smaller than a line's disc holds no line: a region that still has
            // something left after an inset by half a line width holds that disc (pi w^2 / 4),
            // so this one's wall inset is empty and the slicer extrudes nothing there (a speck
            // where a clipped or meshed surface dips a hair below its neighbours).
            if e.area_mm2() < std::f64::consts::FRAC_PI_4 * width * width {
                continue;
            }
            if !rests_on(e, under_grid.get_or_insert_with(|| SegGrid::of_regions(&under, |k| k as u32)))
                && total_area_mm2(&intersection_ex(std::slice::from_ref(e), &under)) <= 1e-6 {
                let mut issue = Issue::new("mid_air", Severity::Error, None, point(e, layer.print_z), Footprint::of_box(e.bbox()), e.area_mm2(),
                    format!("floating island at z = {:.2} mm (nothing below it)", layer.print_z), "Move the island down, join it to the model, or add support beneath it.");
                issue.area_mm2 = e.area_mm2(); out.issues.push(issue);
            }
        }
        let overhang = difference_ex(&slices, &lower);
        let area = total_area_mm2(&overhang);
        let contact = total_area_mm2(&intersection_ex(&overhang, &under_support));
        let air = area - contact;
        if layer.print_z <= FLOOR_REACH && air >= FLOOR_MIN_AREA && air > FLOOR_SHARE * total_area_mm2(&slices) {
            let widest = overhang.iter().max_by(|a, b| a.area_mm2().total_cmp(&b.area_mm2())).expect("air is in the overhang");
            let mut issue = Issue::new("floor_in_air", Severity::Error, None, point(widest, layer.print_z), Footprint::of_regions(&difference_ex(&overhang, &under_support)), air,
                format!("{air:.0} mm² starts {:.2} mm above the bed with nothing under it", layer.print_z - layer.height),
                "Make the lowest face flat, or lay the part on another face: whatever stands below it holds the rest off the bed.");
            issue.area_mm2 = air; out.issues.push(issue);
        }
        // A layer may step out over the one under it by its height times the tangent of the
        // part's overhang angle with nothing under it: the planner asks its supports to hold only
        // what reaches past that. A wall leaning out steeper than the threshold is no uncovered
        // overhang, however long its edge (a lattice's slopes step out a sliver a layer). What the
        // part keeps its trees out from under is its plan's to tell, as the person's choice: only
        // the rest is uncovered. Both lie within the overhang, so a layer whose overhang is no
        // more than the least told has nothing to tell.
        let mats = materials.of_part(part);
        if mats.supports && area > UNCOVERED_MIN_AREA {
            let past = difference_ex(&overhang, &offset_mm(&lower, layer.height * mats.overhang_angle_deg.to_radians().tan()));
            let open = match kept.and_then(|kept| kept.under(layer.print_z)) {
                Some(set) if !past.is_empty() => difference_ex(&past, &frames.local(p, &set)),
                _ => past,
            };
            let (area, contact) = (total_area_mm2(&open), total_area_mm2(&intersection_ex(&open, &under_support)));
            if area > UNCOVERED_MIN_AREA && contact < area * 0.05 {
                let mut issue = Issue::new("uncovered_overhang", Severity::Warning, None, point(&open[0], layer.print_z), Footprint::of_regions(&open), contact / area,
                    format!("{area:.1} mm² overhang has {:.1}% support coverage", contact / area * 100.0),
                    "Adjust support placement, threshold or interface coverage; inspect the unsupported region.");
                issue.area_mm2 = area; out.issues.push(issue);
            }
        }
        drop(islands_t);
        let _bridges_t = prof::scope(prof::Stage::CheckBridges);
        let mut touching: Option<SegGrid<u32>> = None;
        let mut anchors: Option<SegGrid<u32>> = None;
        // Only material actually beneath this island anchors a cantilever;
        // an adjacent island sharing an edge does not support it.
        let beneath = || SegGrid::of_regions(&offset_mm(&intersection_ex(&slices, &under), 0.001), |k| k as u32);
        // Three bead widths tolerate normal layer-to-layer slopes
        // while flagging a shelf that cannot rest on prior beads.
        let shelf = |reach: &Option<(f64, Pt)>| reach.is_some_and(|(reach, _)| reach > 3.0 * width);
        for e in difference_ex(&overhang, &under_support) {
            // A bridge is too long when its span is over the limit while the piece is at least a
            // bead wide along it (area / span >= width): a piece whose area over a bead's width is
            // within the limit cannot be one, so for it only whether it is a bridge counts.
            let measure = e.area_mm2() / width > materials.max_bridge_length * (1.0 - 1e-9);
            // Unmeasured, a piece can only tell as a shelf (a bridge or not, its span tells
            // nothing): its reach first, and the sweep of every hatch direction only when that
            // reach is a shelf's, to tell whether the piece is a bridge after all.
            let reach = (!measure).then(|| {
                let _reach_t = prof::scope(prof::Stage::CheckCantilevers);
                cantilever_reach(&e, touching.get_or_insert_with(beneath), width)
            });
            if reach.as_ref().is_some_and(|r| !shelf(r)) {
                continue;
            }
            let span_t = prof::scope(prof::Stage::CheckBridgeSpans);
            let bridge = bridge_span(&e, anchors.get_or_insert_with(|| SegGrid::of_regions(&offset_mm(&under, 0.01), |k| k as u32)), width, measure);
            drop(span_t);
            if let Some((span, xy, _)) = bridge {
                // A strip narrower than a bead is no bridge whatever its length: the bead lies on
                // the layer below along that whole length (a rounded edge's last sliver).
                if measure && span > materials.max_bridge_length && e.area_mm2() / span >= width {
                    let mut issue = bridge_issue(None, [xy[0], xy[1], layer.print_z], Footprint::of_box(e.bbox()), span);
                    issue.area_mm2 = e.area_mm2(); out.issues.push(issue);
                }
            } else if let Some((reach, at)) = reach.unwrap_or_else(|| {
                let _reach_t = prof::scope(prof::Stage::CheckCantilevers);
                cantilever_reach(&e, touching.get_or_insert_with(beneath), width)
            }) {
                if reach > 3.0 * width {
                    let at = [at.x_mm(), at.y_mm(), layer.print_z];
                    let entry = out.cantilever.get_or_insert(CantileverLayer { footprint: Footprint::of_box(e.bbox()), at, reach, area: 0.0 });
                    if reach > entry.reach { entry.reach = reach; entry.at = at; }
                    entry.footprint = entry.footprint.union(Footprint::of_box(e.bbox()));
                    entry.area += e.area_mm2();
                }
            }
        }
    }
    out
}

struct Extent {
    bounds: Bounds,
    /// An actual bead point at each box extremum, for issue markers.
    points: [[f64; 3]; 6],
}

impl Extent {
    fn add(&mut self, p: [f64; 3]) {
        for d in 0..3 {
            if p[d] < self.bounds.min[d] { self.points[d*2] = p; }
            if p[d] > self.bounds.max[d] { self.points[d*2+1] = p; }
        }
        self.bounds.add_point(p);
    }
    fn merge(&mut self, other: &Self) {
        for d in 0..3 {
            if other.bounds.min[d] < self.bounds.min[d] { self.points[d*2] = other.points[d*2]; }
            if other.bounds.max[d] > self.bounds.max[d] { self.points[d*2+1] = other.points[d*2+1]; }
        }
        self.bounds = self.bounds.union(&other.bounds);
    }
}

fn move_extent(m: &MoveRecord) -> Extent {
    let mut e = Extent { bounds: Bounds::EMPTY, points: [[0.0;3];6] };
    e.add(m.p0.map(f64::from)); e.add(m.p1.map(f64::from));
    if m.flags & flags::ARC != 0 {
        let start = ((m.p0[1]-m.center[1]) as f64).atan2((m.p0[0]-m.center[0]) as f64);
        let radius = ((m.p0[0]-m.center[0]) as f64).hypot((m.p0[1]-m.center[1]) as f64);
        let sweep = m.sweep as f64;
        for k in 0..4 {
            let a = k as f64 * std::f64::consts::FRAC_PI_2;
            let d = if sweep >= 0.0 { a-start } else { start-a }.rem_euclid(std::f64::consts::TAU);
            if d <= sweep.abs() + 1e-6 {
                let z = m.p0[2] as f64 + (m.p1[2]-m.p0[2]) as f64 * (d / sweep.abs().max(1e-12)).min(1.0);
                e.add([m.center[0] as f64 + radius*a.cos(), m.center[1] as f64 + radius*a.sin(), z]);
            }
        }
    }
    for d in 0..2 {
        e.bounds.min[d] -= m.width as f64 * 0.5; e.bounds.max[d] += m.width as f64 * 0.5;
        e.points[d*2][d] -= m.width as f64 * 0.5; e.points[d*2+1][d] += m.width as f64 * 0.5;
    }
    e.bounds.min[2] -= m.height as f64; e.points[4][2] -= m.height as f64;
    e
}

fn keep_out_point(m: &MoveRecord, zone: &fab_plate::machine::Rect) -> Option<[f64; 3]> {
    let r = m.width as f64 * 0.5;
    let lo = [zone.min[0]-r, zone.min[1]-r];
    let hi = [zone.max[0]+r, zone.max[1]+r];
    let a = m.p0.map(f64::from);
    let b = m.p1.map(f64::from);
    let inside = |p: [f64; 3]| (0..2).all(|d| p[d] > lo[d] && p[d] < hi[d]);
    if inside(a) { return Some(a); }
    if inside(b) { return Some(b); }
    if m.flags & flags::ARC == 0 {
        let (mut enter, mut leave) = (0.0f64, 1.0f64);
        for d in 0..2 {
            let delta = b[d]-a[d];
            if delta.abs() < 1e-12 {
                if a[d] <= lo[d] || a[d] >= hi[d] { return None; }
            } else {
                let (p,q) = ((lo[d]-a[d])/delta, (hi[d]-a[d])/delta);
                enter = enter.max(p.min(q)); leave = leave.min(p.max(q));
            }
        }
        if enter >= leave { return None; }
        let t = (enter+leave)*0.5;
        return Some([a[0]+(b[0]-a[0])*t, a[1]+(b[1]-a[1])*t, a[2]+(b[2]-a[2])*t]);
    }
    let c = m.center.map(f64::from);
    let radius = (a[0]-c[0]).hypot(a[1]-c[1]);
    if radius <= 1e-12 || m.sweep.abs() <= 1e-6 { return None; }
    let start = (a[1]-c[1]).atan2(a[0]-c[0]);
    let sweep = m.sweep as f64;
    let mut cuts = vec![0.0, 1.0];
    for d in 0..2 {
        for edge in [lo[d],hi[d]] {
            let v = (edge-c[d])/radius;
            if v.abs() > 1.0 { continue; }
            let t = if d == 0 { v.acos() } else { v.asin() };
            for angle in [t, if d == 0 { -t } else { std::f64::consts::PI-t }] {
                let phase = if sweep > 0.0 { angle-start } else { start-angle }.rem_euclid(std::f64::consts::TAU);
                if phase < sweep.abs() { cuts.push(phase/sweep.abs()); }
            }
        }
    }
    cuts.sort_by(f64::total_cmp);
    for pair in cuts.windows(2) {
        let t = (pair[0]+pair[1])*0.5;
        let angle = start+sweep*t;
        let p = [c[0]+radius*angle.cos(), c[1]+radius*angle.sin(), a[2]+(b[2]-a[2])*t];
        if inside(p) { return Some(p); }
    }
    None
}

fn moves(machine: &Machine, plate: &Plate, materials: &Materials, timeline: &Timeline, records: &mut dyn Iterator<Item = MoveRecord>, report: &mut Report) {
    let object_indices: BTreeMap<_, _> = report.objects.iter().enumerate().map(|(i, o)| (o.object.as_str(), i)).collect();
    let part_stats: Vec<_> = plate.parts.iter().map(|p| object_indices.get(p.object.as_str()).copied()).collect();
    drop(object_indices);
    let nf = materials.filaments.len().max(timeline.filament_mm.len());
    let filaments: Vec<_> = (0..nf).map(|f| materials.filament(f as u32 + 1)).collect();
    let mut stats: Vec<_> = (0..nf).map(|f| {
        let mat = &filaments[f];
        let length = timeline.filament_mm.get(f).copied().unwrap_or(0.0);
        let mass = length * std::f64::consts::PI * mat.diameter.powi(2) * 0.25 * mat.density / 1000.0;
        FilamentStats { filament: f as u32+1, length_mm: length, mass_g: mass, cost: mass * mat.cost_per_kg / 1000.0, ..Default::default() }
    }).collect();
    let mut deposited = vec![0.0; nf];
    let mut peaks: Vec<Option<MoveRecord>> = vec![None; nf];
    let mut groups: BTreeMap<(u16, u32, u8, u8), Extent> = BTreeMap::new();
    let mut extrusion_layers: BTreeMap<(u16, u32), (f64, f64, [f64; 3], Bounds)> = BTreeMap::new();
    let mut used_parts: BTreeMap<(u16, u8, u8), Bounds> = BTreeMap::new();
    let mut keep_out_hits = BTreeMap::new();
    let mut last_nozzle = None;
    let mut nozzle_changes = 0;
    let mut last_end = 0.0f64;
    // The first change that purges (the time model's purge macro), and how many do.
    let (mut first_purge, mut purges) = (None::<MoveRecord>, 0u32);
    for m in records {
        last_end = m.t_end;
        let m = &m;
        if m.filament > 0 {
            if last_nozzle.is_some_and(|n| n != m.nozzle) { nozzle_changes += 1; }
            last_nozzle = Some(m.nozzle);
        }
        if m.kind == MoveKind::FirmwareMacro && m.flags & flags::PURGE != 0 {
            purges += 1;
            first_purge.get_or_insert(*m);
        }
        if m.kind != MoveKind::Extrude || m.e <= 0.0 { continue; }
        let f = m.filament.saturating_sub(1) as usize;
        if let Some(s) = stats.get_mut(f) {
            let mat = &filaments[f];
            let area = std::f64::consts::PI * mat.diameter.powi(2) * 0.25;
            if m.role != Role::Flush { deposited[f] += m.e as f64; }
            let dx = (m.p1[0]-m.p0[0]) as f64; let dy = (m.p1[1]-m.p0[1]) as f64; let dz = (m.p1[2]-m.p0[2]) as f64;
            let xy = if m.flags & flags::ARC != 0 { ((m.p0[0]-m.center[0]) as f64).hypot((m.p0[1]-m.center[1]) as f64) * (m.sweep as f64).abs() } else { dx.hypot(dy) };
            let len = xy.hypot(dz);
            let flow = if len > 1e-9 { m.e as f64 * area / len * m.v_cruise as f64 } else { 0.0 };
            if flow > s.peak_flow_mm3_s { s.peak_flow_mm3_s = flow; peaks[f] = Some(*m); }
            if let Some(Some(index)) = part_stats.get(m.object as usize) {
                if let Some(o) = report.objects.get_mut(*index) {
                    o.mass_g += m.e as f64 * area * mat.density / 1000.0;
                    if let Some(z) = o.max_height_per_nozzle.get_mut(m.nozzle as usize) { *z = z.max(m.p1[2] as f64); }
                }
            }
        }
        if m.role == Role::Custom || m.width <= 0.0 || (m.p0 == m.p1 && (m.flags & flags::ARC == 0 || m.sweep.abs() < 1e-6)) { continue; }
        let extent = move_extent(m);
        let b = extent.bounds;
        for (zone, rect) in machine.keep_out.iter().enumerate() {
            let key = (m.object, m.layer, m.filament, m.nozzle, zone);
            if !keep_out_hits.contains_key(&key) && rect.overlaps_footprint(&b) {
                if let Some(at) = keep_out_point(m, rect) { keep_out_hits.insert(key, at); }
            }
        }
        groups.entry((m.object, m.layer, m.filament, m.nozzle)).and_modify(|g| g.merge(&extent)).or_insert(extent);
        used_parts.entry((m.object, m.filament, m.nozzle)).and_modify(|g| *g = g.union(&b)).or_insert(b);
        if m.object != u16::MAX {
            extrusion_layers.entry((m.object, m.layer)).and_modify(|e| e.3 = e.3.union(&b)).or_insert((m.z_layer as f64, m.height as f64, m.p1.map(f64::from), b));
        }
    }
    let mut previous_z: BTreeMap<u16, f64> = BTreeMap::new();
    for ((object, layer, filament, _, zone), at) in keep_out_hits {
        let mut issue = Issue::new("outside_bed", Severity::Warning, plate.parts.get(object as usize).map(|p| p.object.as_str()), at, Footprint::square(at, 1.0), zone as f64+1.0,
            format!("Filament {filament}, layer {layer} crosses keep-out zone {}", zone+1), "Move the part or prime tower clear of the exclusion zone.");
        issue.flags = 4; issue.layer = Some(layer); report.issues.push(issue);
    }
    for ((object, layer), (z, height, at, bounds)) in extrusion_layers {
        if let Some(prev) = previous_z.insert(object, z) {
            if z - prev > height + materials.support_gap + 0.01 {
                let mut issue = Issue::new("empty_layer", Severity::Error, plate.parts.get(object as usize).map(|p| p.object.as_str()), at, Footprint::of_bounds(bounds), z-prev,
                    format!("Empty extrusion layers from Z {prev:.2} to {z:.2} mm"), "Join separated shells or add supports across the empty layer range.");
                issue.layer = Some(layer); report.issues.push(issue);
            }
        }
    }
    // Painted regions and support filaments can introduce materials absent
    // from the mesh assignments. Check the entire actually extruded set.
    let actual = Plate { excluded: Vec::new(), settings: Vec::new(), parts: used_parts.into_iter().map(|((object, filament, nozzle), b)| {
        let name = plate.parts.get(object as usize).map(|p| p.object.as_str()).unwrap_or("plate");
        Part { body_of: None, print_mesh: None, paint: None, print: Vec::new(), object: name.into(), name: name.into(), filament: filament as u32, color: [1.0; 4], nozzle: Some(nozzle as usize), mesh: PartMesh { positions: vec![b.min, b.max], triangles: Vec::new() }, field_bounds: None, field_floor: None, ..Default::default() }
    }).collect() };
    let mut material_report = Report::default();
    mesh::material_issues(machine, &actual, materials, &mut material_report);
    for issue in material_report.issues {
        if matches!(issue.kind, "filament_nozzle" | "temperature_mix" | "bed_temperature" | "bed_temperature_mix" | "abrasive")
            && !report.issues.iter().any(|i| i.kind == issue.kind && i.object == issue.object && i.message == issue.message) {
            report.issues.push(issue);
        }
    }
    let mut relaxed = machine.clone();
    for d in 0..2 { relaxed.bed.min[d] -= 2.0; relaxed.bed.max[d] += 2.0; }
    for ((object, layer, filament, nozzle), extent) in groups {
        let b = extent.bounds;
        let name = plate.parts.get(object as usize).map(|p| p.object.as_str()).unwrap_or("plate");
        let proxy = Part { body_of: None, print_mesh: None, paint: None, print: Vec::new(), object: name.into(), name: name.into(), filament: filament as u32, color: [1.0; 4], nozzle: Some(nozzle as usize), mesh: PartMesh { positions: vec![b.min, b.max], triangles: Vec::new() }, field_bounds: None, field_floor: None, ..Default::default() };
        let mut issues: Vec<_> = mesh::placement(machine, &proxy, Some(layer)).into_iter().filter(|i| i.kind == "outside_bed").map(|mut i| {
            i.severity = Severity::Warning; i.flags = 0; i
        }).collect();
        issues.extend(mesh::placement(&relaxed, &proxy, Some(layer)));
        for issue in &mut issues {
            let offending = extent.points.iter().find(|&&p| {
                let point = Bounds { min: p, max: p };
                match issue.kind {
                    "reach" => machine.nozzles.get(nozzle as usize).is_some_and(|n| !n.reach.contains_footprint(&point, fab_plate::machine::EDGE_SLACK)),
                    "outside_bed" => p[2] < -fab_plate::machine::EDGE_SLACK || !machine.bed.contains_footprint(&point, fab_plate::machine::EDGE_SLACK)
                        || machine.keep_out.iter().any(|r| r.overlaps_footprint(&point)),
                    _ => false,
                }
            });
            if issue.kind == "height" { issue.at_mm = extent.points[5]; }
            else if let Some(p) = offending { issue.at_mm = *p; }
            issue.message = format!("Filament {filament}, layer {layer}: {}", issue.message);
            if object == u16::MAX { issue.object = None; }
        }
        report.issues.extend(issues);
    }
    for (f, s) in stats.iter_mut().enumerate() {
        let mat = &filaments[f];
        let flush = (s.length_mm - deposited[f]).max(0.0);
        s.flush_mass_g = flush * std::f64::consts::PI * mat.diameter.powi(2) * 0.25 * mat.density / 1000.0;
        // A percent of slack: on a very short move the G-code's rounded E reads that far over.
        if mat.max_flow > 0.0 && s.peak_flow_mm3_s > mat.max_flow * 1.01 {
            if let Some(m) = &peaks[f] {
                let mut issue = Issue::new("flow", Severity::Warning, plate.parts.get(m.object as usize).map(|p| p.object.as_str()), m.p1.map(f64::from), Footprint::of_bounds(move_extent(m).bounds), s.peak_flow_mm3_s,
                    format!("Filament {} reaches {:.2} mm³/s; profile limit {:.2}", f+1, s.peak_flow_mm3_s, mat.max_flow), "Reduce extrusion speed or line width, or select a filament profile rated for this flow.");
                issue.layer = Some(m.layer); report.issues.push(issue);
            }
        }
    }
    // The figures are the G-code's: a timeline of toolpath records alone (live checks) feeds
    // no filament, so it has none.
    if timeline.filament_mm.is_empty() {
        report.plate = None;
        return;
    }
    let figures = PlateStats {
        time_s: timeline.total.max(last_end),
        mass_g: stats.iter().map(|s| s.mass_g).sum(),
        cost: stats.iter().map(|s| s.cost).sum(),
        flush_mass_g: stats.iter().map(|s| s.flush_mass_g).sum(),
        filament_changes: timeline.tool_changes,
        nozzle_changes,
        filament_swaps: timeline.filament_swaps,
        filaments: stats,
    };
    // The purge is filament the print never shows: said, and loudly once it outweighs the print.
    if let Some(m) = first_purge.filter(|_| figures.flush_mass_g > 0.0) {
        let (purged, all) = (figures.flush_mass_g, figures.mass_g);
        let printed = (all - purged).max(0.0);
        let severity = if purged > printed { Severity::Warning } else { Severity::Info };
        let at = m.p1.map(f64::from);
        let mut issue = Issue::new("purge", severity, None, at, Footprint::square(at, 10.0), purged,
            format!("{purges} filament swap{} purge {purged:.1} g, against {printed:.1} g printed", if purges == 1 { "" } else { "s" }),
            "Each nozzle printing its own colours, fewer colour changes per layer, or purging into infill (flush_into_infill) or a part marked for it (flush_into_objects), purges less.");
        issue.layer = Some(m.layer); report.issues.push(issue);
    }
    report.plate = Some(figures);
}

//! GateRig (GPU-EXEC §10.6): one part through the CPU reference and through a candidate,
//! compared by `printcheck` per sampled layer and island, one line per failure. A candidate is
//! a trace: it gives each layer's traced outline, and the cores compensate and plan every
//! island, as the live planner does (W1). The walls-side stages have their own cases on the
//! CPU's trace ([`crate::stage_cases`]).
//!
//! A trace's reference is the field's f64 zero set, not the CPU's contours ([`zero_set`]): every
//! vertex and edge middle of a traced outline within 0.01 mm of it ([`off_plane`]) where the
//! slicer samples the layer (`SdfSource::BELOW_PLANE` under its height, as the CPU's contours
//! and the GPU's trace), and its inside the field's on a 0.1 mm grid. Where the CPU's own contours miss the zero set, its walls and
//! plans are no reference for the layers that read them: those comparisons are left out and
//! the report names the layers. The gate's input contours leave out rings of no area.
//!
//! - [`reference`]: per sampled layer, the compensated outline islands and the walls the
//!   planner makes of each (on a first layer from what elephant-foot compensation leaves of the
//!   island, the walls' levels then measured from the island with the foot added).
//! - [`walls`]: a candidate's sampled layers against it. The CPU plans every island of the
//!   compensated trace, as the planner does, and counts it; its walls are the CPU's own, so of
//!   it the gate checks what the trace decided: its outline, its walls' and loops' topology and
//!   its fill, not the CPU walls' own geometry (crossings, spacing, gaps, oracle), which the
//!   CPU's tests hold. [`walls_at`] holds one layer's islands and walls to the reference.
//! - [`Fed`]: the planner plans a window from the candidate's traced outlines, and again the
//!   same way from the CPU's contours (traced once per case, for the zero set, the reference,
//!   the plans and the noise); the two planned windows' island order, wall order and seams are
//!   compared. A trace's seams and thin fragments' pairing and order are excused within the
//!   CPU's own noise: the window planned again from the CPU's contours moved within 0.01 mm
//!   ([`resampled`], up to three ways, a way at a time until every excusable deviation is
//!   excused), where it is off on the same key at least as far (`printcheck::order_within`). A
//!   layer that used an excuse must still hold its gaps, overlaps, coverage, wall count,
//!   extruded length and fill classes (`printcheck::held`); every excused deviation is counted
//!   in the report.
//! - [`run`]: all of it for one part at [`sample`]d layers and [`windows`], as one [`Report`]
//!   whose first line is the verdict; [`run_at`] at a case's chosen layers instead, each of a
//!   [`Kind`] the CPU must plan it as (a `case` failure when the fixture drifts).
//!
//! Fixtures: [`frame_parts`] takes a plate's parts as the slicer does ([`crate::live::slice_frames`]),
//! [`analytic`] holds the nine analytic shapes and [`analytic_part`] makes a part of one.
use crate::live::{Frame, Lifted, PartField};
use fab_plate::Plate;
use makepad_csg_sdf::{Clearance, SdfNode, SdfOp, SdfShape};
use makepad_fab_settings::machine::Job;
use makepad_fab_settings::profiles::JobSetup;
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::sdf::SdfSource;
use makepad_fab_slicer::source::{ContourSource, IslandWalls, MeshSource, SlopeProfile};
use makepad_fab_toolpath::printcheck::{self, Check, Failure, Island, Join, Level, Tol};
use makepad_fab_toolpath::{self as tp, par, ObjectInput, ObjectLayer, Params, Role};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

/// One part to gate: its CPU source, the plate's job and the part's layer stack.
pub struct GatePart {
    pub name: String,
    pub source: Arc<dyn ContourSource + Send + Sync>,
    /// A field part's field as the GPU traces it (raised by its frame's z offset); None: a mesh part.
    pub field: Option<PartField>,
    pub input: ObjectInput,
    pub job: Job,
    pub heights: Vec<(f64, f64)>,
}

impl GatePart {
    pub fn new(name: &str, source: Arc<dyn ContourSource + Send + Sync>, filament: usize, overrides: Vec<(String, String)>, job: Job) -> Self {
        let input = ObjectInput { name: name.into(), source: Box::new(crate::plate::Shared(source.clone())), filament, overrides };
        let heights = tp::plan_heights(&input, &job);
        Self { name: name.into(), source, field: None, input, job, heights }
    }
    /// A field part: sliced from `field`'s source on the CPU, traced from `field` on the GPU.
    pub fn of_field(name: &str, field: PartField, filament: usize, overrides: Vec<(String, String)>, job: Job) -> Result<Self, String> {
        let mut part = Self::new(name, field.source()?, filament, overrides, job);
        part.field = Some(field);
        Ok(part)
    }
    /// The part's own job: the plate's with the part's overrides.
    pub fn own_job(&self) -> Job {
        if self.input.overrides.is_empty() { self.job.clone() } else { self.job.with_overrides(&self.input.overrides) }
    }
    pub fn params(&self) -> Params {
        Params::from_job(&self.own_job(), self.input.filament)
    }
    /// Each layer's cut height.
    pub fn zs(&self) -> Vec<f64> {
        let p = self.params();
        self.heights.iter().map(|&(z, h)| tp::cut_z(&p, z, h)).collect()
    }
    /// The source's own contours at layers `at`, as a trace makes them (uncompensated), without
    /// rings of no area ([`NO_AREA_MM2`]).
    pub fn contours(&self, at: &[usize]) -> Vec<Vec<ExPolygon>> {
        let zs = self.zs();
        let cut: Vec<f64> = at.iter().map(|&k| zs[k]).collect();
        let mut out = tp::contours(par::Ctl::default(), &*self.input.source, &cut).expect("not cancelled");
        for layer in &mut out {
            layer.retain(|e| e.contour.area_mm2().abs() >= NO_AREA_MM2);
            for e in layer.iter_mut() {
                e.holes.retain(|h| h.area_mm2().abs() >= NO_AREA_MM2);
            }
        }
        out
    }
    /// `slices` at layers `at` as the planner compensates them into outline islands.
    pub fn compensate(&self, at: &[usize], slices: Vec<Vec<ExPolygon>>) -> Vec<Vec<ExPolygon>> {
        let heights: Vec<(f64, f64)> = at.iter().map(|&k| self.heights[k]).collect();
        tp::plan_outline(par::Ctl::default(), &self.input, &self.job, &heights, slices).expect("not cancelled")
    }
    /// The compensated outline islands at layers `at` (sorted, distinct).
    pub fn outlines(&self, at: &[usize]) -> HashMap<usize, Vec<ExPolygon>> {
        let out = self.compensate(at, self.contours(at));
        at.iter().copied().zip(out).collect()
    }
}

/// A ring with less area than this (mm²) is no ring: a contour's degenerate sliver, left out of the
/// gate's inputs.
pub const NO_AREA_MM2: f64 = 1e-4;

/// A field in the frame's coordinates (mm), negative inside.
pub type Field = Arc<dyn Fn(f64, f64, f64) -> f64 + Send + Sync>;

/// Below this (mm, 1 µm) |f| / |∇f| is a point's distance from the zero set as it is.
const OFF_EXACT: f64 = 0.001;
/// The search's rings: 0.5 µm apart out to 0.05 mm, their directions 0.5 µm apart along the
/// ring (64 at least), so a corner's point reads at most about 0.75 µm long.
const SEARCH_STEP: f64 = 0.0005;
const SEARCH_RINGS: u32 = 100;
const SEARCH_ARC: f64 = 0.0005;

/// A point's distance from the field's zero set in the plane at `z` (mm). Within 1 µm, |f| /
/// |∇f| (the in-plane gradient by central differences, floored at 1e-3). Past that, the least
/// radius (rings 0.5 µm apart, out to 0.05 mm, of directions 0.5 µm apart) at which the field
/// is zero or takes the other sign; the rings start at the radius `clearance` proves for its
/// value, inside which the field does not turn. Nothing within 0.05 mm reads the farther of 0.05 mm and |f| / |∇f|. Where
/// the in-plane gradient vanishes or misleads (inside a max() corner, beside one, on a face
/// tangent to the plane) |f| / |∇f| over- or understates the distance; the search measures it.
pub fn off_plane(f: &Field, clearance: &Clearance, q: [f64; 2], z: f64) -> f64 {
    let v = f(q[0], q[1], z);
    if v == 0.0 {
        return 0.0;
    }
    let h = 1e-6;
    let gx = (f(q[0] + h, q[1], z) - f(q[0] - h, q[1], z)) / (2.0 * h);
    let gy = (f(q[0], q[1] + h, z) - f(q[0], q[1] - h, z)) / (2.0 * h);
    let e = v.abs() / gx.hypot(gy).max(1e-3);
    if e <= OFF_EXACT {
        return e;
    }
    let first = (clearance.radius(v) / SEARCH_STEP).floor().clamp(1.0, SEARCH_RINGS as f64) as u32;
    for i in first..=SEARCH_RINGS {
        let r = i as f64 * SEARCH_STEP;
        let directions = ((std::f64::consts::TAU * r / SEARCH_ARC).ceil() as u32).max(64);
        for k in 0..directions {
            let a = k as f64 * std::f64::consts::TAU / directions as f64;
            let w = f(q[0] + r * a.cos(), q[1] + r * a.sin(), z);
            if w == 0.0 || (w > 0.0) != (v > 0.0) {
                return r;
            }
        }
    }
    e.max(SEARCH_RINGS as f64 * SEARCH_STEP)
}

/// How an outline holds a field's zero set at height `z`.
#[derive(Clone, Copy, Debug)]
pub struct ZeroSet {
    /// The farthest of its vertices and edge middles from the zero set (mm), and where.
    pub off: f64,
    pub at: Pt,
    /// A grid point the outline puts on the wrong side (more than the margin from the zero set),
    /// and the field there.
    pub wrong_side: Option<(Pt, f64)>,
}

/// An outline against a field part's zero set at layer height `z`, where the slicer samples it
/// (`SdfSource::BELOW_PLANE` under): its vertices and edge middles ([`off_plane`]), and (`step`
/// Some) its inside against the field's sign on a `step` mm grid over the box `lo..hi`, at
/// points at least `margin` mm from the zero set, as `clearance` proves it.
pub fn zero_set(f: &Field, clearance: &Clearance, z: f64, outline: &[ExPolygon], lo: [f64; 2], hi: [f64; 2], step: Option<f64>, margin: f64) -> ZeroSet {
    let z = z - SdfSource::BELOW_PLANE;
    let mut off = (0.0f64, Pt::default());
    for r in outline.iter().flat_map(|e| e.rings()) {
        let n = r.pts.len();
        for i in 0..n {
            let (a, b) = (r.pts[i], r.pts[(i + 1) % n]);
            for q in [[a.x_mm(), a.y_mm()], [0.5 * (a.x_mm() + b.x_mm()), 0.5 * (a.y_mm() + b.y_mm())]] {
                let e = off_plane(f, clearance, q, z);
                if e > off.0 {
                    off = (e, Pt::from_mm(q[0], q[1]));
                }
            }
        }
    }
    let mut wrong_side = None;
    let Some(step) = step else { return ZeroSet { off: off.0, at: off.1, wrong_side } };
    let (nx, ny) = (((hi[0] - lo[0]) / step).ceil() as usize, ((hi[1] - lo[1]) / step).ceil() as usize);
    'grid: for j in 0..=ny {
        for i in 0..=nx {
            let q = [lo[0] + i as f64 * step, lo[1] + j as f64 * step];
            let v = f(q[0], q[1], z);
            if clearance.radius(v) < margin {
                continue;
            }
            let p = Pt::from_mm(q[0], q[1]);
            if (v < 0.0) != outline.iter().any(|e| e.contains(p)) {
                wrong_side = Some((p, v));
                break 'grid;
            }
        }
    }
    ZeroSet { off: off.0, at: off.1, wrong_side }
}

/// The zero-set tolerance (mm): a traced outline's points within it, as the trace's own (§8).
pub const ZERO_SET_TOL: f64 = 0.01;

/// A field part's outlines against its zero set, per layer: (layer, how it holds), the inside
/// on a 0.1 mm grid at the layers `grid` names.
fn zero_sets(part: &GatePart, outlines: &[(usize, &[ExPolygon])], grid: &[usize]) -> Option<Vec<(usize, ZeroSet)>> {
    let field = part.field.as_ref()?;
    let f = field.field().ok()?;
    let zs = part.zs();
    let (lo, hi) = part.source.bounds();
    let clearance = field.shape.clearance();
    let step = |k: usize| grid.contains(&k).then_some(0.1);
    Some(outlines.iter().map(|&(k, o)| (k, zero_set(&f, &clearance, zs[k], o, [lo[0], lo[1]], [hi[0], hi[1]], step(k), 2.0 * ZERO_SET_TOL))).collect())
}

/// A plate's parts as [`crate::live::slice_frames`] takes them (without paint or non-planar
/// tops): a field part from its field raised by the frame's z offset, a mesh part from its
/// mesh, each with its filament and its own print settings.
pub fn frame_parts(setup: &JobSetup, plate: &Plate, frames: &[Frame]) -> Result<Vec<GatePart>, String> {
    let job = crate::prepare_job(setup)?;
    let mut out = Vec::new();
    for (i, (p, f)) in plate.parts.iter().zip(frames).enumerate() {
        let filament = crate::plate::part_filament(setup, i, p)?;
        if let Some(field) = &f.field {
            // the CPU cuts the field's source raised by the lift, the GPU the field moved by it
            let source = Arc::new(Lifted { src: field.source()?, lift: f.offset[2] });
            let mut part = GatePart::new(&p.name, source, filament, p.print.clone(), job.clone());
            part.field = Some(field.moved(&[[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, f.offset[2]]]).ok_or("a vertical move is rigid")?);
            out.push(part);
            continue;
        }
        let source: Arc<dyn ContourSource + Send + Sync> = Arc::new(MeshSource::new(crate::live::frame_mesh(p, f, None)));
        out.push(GatePart::new(&p.name, source, filament, p.print.clone(), job.clone()));
    }
    Ok(out)
}

/// The analytic shapes' height (mm): three layers of 0.2 mm, the first, one between and the top.
/// Every layer of a prism has the same outline, so more layers prove nothing more.
pub const ANALYTIC_HEIGHT: f64 = 0.6;

/// The nine analytic shapes (W1b's cases), Y up and centred, [`ANALYTIC_HEIGHT`] tall: each
/// layer's outline is the same exact polygon or circle set, so the f64 oracle is exact.
pub fn analytic() -> Vec<(&'static str, SdfShape)> {
    let h = ANALYTIC_HEIGHT;
    let block = |x: f64, z: f64| SdfNode::Box { size: [x, h, z], radius: 0.0 };
    let disk = |r: f64, height: f64| SdfNode::Cylinder { radius: r, top_radius: r, height };
    let at = |of: SdfNode, x: f64, z: f64| SdfNode::Transform { at: [x, 0.0, z], rotate: [0.0; 3], scale: 1.0, of: Box::new(of) };
    let turned = |of: SdfNode, at: [f64; 3], deg: f64| SdfNode::Transform { at, rotate: [0.0, deg, 0.0], scale: 1.0, of: Box::new(of) };
    let combine = |op, of| SdfNode::Combine { op, blend: 0.0, of };
    // two half-planes 20 degrees apart, the tip at the origin, cut off 8 mm out
    let wedge = {
        let turn: f64 = 180.0 - 20.0;
        let a = at(block(8.0, 16.0), 4.0, 0.0);
        let b = turned(block(8.0, 16.0), [4.0 * turn.to_radians().cos(), 0.0, -4.0 * turn.to_radians().sin()], turn);
        combine(SdfOp::Intersection, vec![a, b, at(block(16.0, 16.0), 0.0, 0.0)])
    };
    let cases = vec![
        ("square", block(10.0, 10.0)),
        ("disk", disk(5.0, h)),
        ("L", combine(SdfOp::Union, vec![at(block(10.0, 4.0), 0.0, 3.0), at(block(4.0, 10.0), -3.0, 0.0)])),
        ("wedge", wedge),
        ("thin-slab", block(10.0, 0.9)),
        ("annulus", combine(SdfOp::Difference, vec![disk(5.0, h), disk(3.4, h + 2.0)])),
        ("box-hole", combine(SdfOp::Difference, vec![block(10.0, 10.0), at(disk(1.5, h + 2.0), 1.5, -1.0)])),
        ("split", combine(SdfOp::Union, vec![at(block(4.0, 6.0), -4.0, 0.0), at(block(4.0, 6.0), 4.0, 0.0)])),
        ("neck", combine(SdfOp::Union, vec![at(block(5.0, 5.0), -4.0, 0.0), at(block(5.0, 5.0), 4.0, 0.0), block(4.0, 1.2)])),
    ];
    cases.into_iter().map(|(name, root)| (name, SdfShape { root, min: [-9.0, -0.5 * h - 0.5, -9.0], max: [9.0, 0.5 * h + 0.5, 9.0], materials: Vec::new() })).collect()
}

/// Two whole parts of the gate's plates, with their heights: an 8 mm box, and a tube 12 mm
/// across with a 1 mm wall, 4 mm tall.
pub fn solids() -> Vec<(&'static str, SdfShape, f64)> {
    let solid = |root: SdfNode, half: [f64; 3]| SdfShape { root, min: half.map(|v| -v - 0.5), max: half.map(|v| v + 0.5), materials: Vec::new() };
    let disk = |r: f64| SdfNode::Cylinder { radius: r, top_radius: r, height: 6.0 };
    vec![
        ("box", solid(SdfNode::Box { size: [8.0; 3], radius: 0.0 }, [4.0; 3]), 8.0),
        ("thin tube", solid(SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of: vec![disk(6.0), disk(5.0)] }, [6.0, 3.0, 6.0]), 4.0),
    ]
}

/// A Y-up shape of `height`, centred, standing on the bed in its part's frame (frame z = shape
/// y + height / 2, frame y = -shape z), sliced over its solid's box as the layout places a part.
pub fn standing(shape: SdfShape, height: f64) -> PartField {
    let (lo, hi) = ([shape.min[0], -shape.max[2], 0.0], [shape.max[0], -shape.min[2], height]);
    PartField::new(Arc::new(shape), [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 0.5 * height]]).with_bounds(Some((lo, hi)))
}

/// An analytic shape as a part of `job` (filament 0, no overrides).
pub fn analytic_part(name: &str, shape: SdfShape, height: f64, job: &Job) -> Result<GatePart, String> {
    GatePart::of_field(name, standing(shape, height), 0, Vec::new(), job.clone())
}

/// Layer indices to sample of `n`: the first two, three spread over the middle and the last
/// two, which prove the first-layer, middle and top-wall rules without slicing every layer.
pub fn sample(n: usize) -> Vec<usize> {
    let mut s: BTreeSet<usize> = BTreeSet::new();
    if n == 0 {
        return Vec::new();
    }
    for k in [0, 1, n / 4, n / 2, 3 * n / 4, n.saturating_sub(2), n - 1] {
        s.insert(k.min(n - 1));
    }
    s.into_iter().collect()
}

/// The one-layer windows whose order and seams are checked: the first, the middle and the last.
pub fn windows(n: usize) -> Vec<(usize, usize)> {
    let s: BTreeSet<usize> = if n == 0 { BTreeSet::new() } else { [0, n / 2, n - 1].into_iter().collect() };
    s.into_iter().map(|k| (k, k + 1)).collect()
}

/// A layer's wall levels (from the outline, the elephant foot added on a first layer) and
/// their join.
pub fn levels(p: &Params, k: usize, h: f64) -> (Vec<Level>, Join) {
    let first = p.first(k);
    let loops = if first && p.one_wall_first { p.wall_loops.min(1) } else { p.wall_loops };
    let (outer, inner) = if first { (p.w_first, p.w_first) } else { (p.w_outer, p.w_inner) };
    let foot = if first { p.elephant_foot.max(0.0) } else { 0.0 };
    let level = |(inset, spacing, _): (f64, f64, f64)| Level { inset: inset + foot, spacing };
    match &p.arachne {
        Some(a) => (a.config(loops, outer, inner, h, p.precise_outer).nominal_wall_levels().2.map(level).collect(), Join::Round),
        None => (tp::walls::levels(loops, outer, inner, h, p.precise_outer).map(level).collect(), Join::Miter(3.0)),
    }
}

/// The walls the planner makes of `island` at layer `k` (`above`: the outline of the layer
/// above, None on the top layer), from its foot-compensated pieces on a first layer.
pub fn cpu_walls(p: &Params, k: usize, h: f64, island: &ExPolygon, above: Option<&[ExPolygon]>) -> IslandWalls {
    let first = p.first(k);
    let loops = tp::wall_loops(p, island, above, first);
    if first && p.elephant_foot > 0.0 {
        tp::island_walls(p, &tp::foot_compensated(p, std::slice::from_ref(island)), loops, first, h)
    } else {
        tp::island_walls(p, std::slice::from_ref(island), loops, first, h)
    }
}

/// The CPU reference of one sampled layer.
pub struct GateLayer {
    pub index: usize,
    pub outline: Vec<ExPolygon>,
    /// The planner's walls of each island.
    pub walls: Vec<IslandWalls>,
    /// The outline of the layer above (None on the top layer), for the walls the CPU plans of a
    /// candidate's islands.
    pub above: Option<Vec<ExPolygon>>,
    pub levels: Vec<Level>,
    pub join: Join,
}

/// The CPU reference at layers `at` (sorted, distinct).
pub fn reference(part: &GatePart, at: &[usize]) -> Vec<GateLayer> {
    reference_of(part, at, &HashMap::new())
}

/// [`reference`] from the CPU's contours `traced` already has (layer -> contours), tracing only
/// the layers it lacks.
fn reference_of(part: &GatePart, at: &[usize], traced: &HashMap<usize, &[ExPolygon]>) -> Vec<GateLayer> {
    let p = part.params();
    let n = part.heights.len();
    let need: Vec<usize> = at.iter().flat_map(|&k| [k, k + 1]).filter(|&k| k < n).collect::<BTreeSet<_>>().into_iter().collect();
    let lacking: Vec<usize> = need.iter().copied().filter(|k| !traced.contains_key(k)).collect();
    let mut contours: HashMap<usize, Vec<ExPolygon>> = lacking.iter().copied().zip(part.contours(&lacking)).collect();
    let slices: Vec<Vec<ExPolygon>> = need.iter().map(|k| contours.remove(k).unwrap_or_else(|| traced[k].to_vec())).collect();
    let outline: HashMap<usize, Vec<ExPolygon>> = need.iter().copied().zip(part.compensate(&need, slices)).collect();
    at.iter()
        .map(|&k| {
            let h = part.heights[k].1;
            let above = outline.get(&(k + 1)).cloned();
            let own = outline[&k].clone();
            let walls = own.iter().map(|i| cpu_walls(&p, k, h, i, above.as_deref())).collect();
            let (levels, join) = levels(&p, k, h);
            GateLayer { index: k, outline: own, walls, above, levels, join }
        })
        .collect()
}

/// A candidate's layer: its traced outline, which the cores compensate and plan whole.
#[derive(Clone, PartialEq)]
pub struct Sampled {
    pub index: usize,
    pub outline: Vec<ExPolygon>,
}

/// The CPU path as a candidate: the source's own contours, as a trace gives them.
pub fn cpu_trace(part: &GatePart) -> impl FnOnce(&[usize]) -> Result<Vec<Sampled>, String> + '_ {
    move |at| Ok(at.iter().zip(part.contours(at)).map(|(&index, outline)| Sampled { index, outline }).collect())
}

/// One layer's islands and their walls held to the reference layer `r`: the failures, and
/// apart from them (`planned`: the CPU planned these walls) the gap, spacing and crossing
/// failures of the walls' own geometry, which the CPU's tests hold and which count only on a
/// layer that used an order excuse.
pub fn walls_at(r: &GateLayer, outline: &[ExPolygon], walls: &[IslandWalls], planned: bool, tol: &Tol) -> (Vec<Failure>, Vec<Failure>) {
    let (mut out, mut held) = (Vec::new(), Vec::new());
    if walls.len() != outline.len() {
        out.push(Failure { layer: r.index, island: None, check: Check::Islands, detail: format!("{} outline islands, {} with walls", outline.len(), walls.len()) });
        return (out, held);
    }
    let cand: Vec<Island> = outline.iter().zip(walls).map(|(o, w)| Island { outline: o, walls: w }).collect();
    let refs: Vec<Island> = r.outline.iter().zip(&r.walls).map(|(o, w)| Island { outline: o, walls: w }).collect();
    let own_geometry = |f: &Failure| matches!(f.check, Check::Crossing | Check::Spacing | Check::Gap | Check::Oracle);
    for f in printcheck::walls(r.index, &r.levels, r.join, &cand, &refs, tol) {
        if !(planned && f.island.is_some() && own_geometry(&f)) {
            out.push(f);
        } else if f.check != Check::Oracle {
            held.push(f);
        }
    }
    (out, held)
}

/// The wall checks of a candidate's sampled layers against the reference, the CPU planning
/// every island of each compensated trace: the failures, how many islands it planned, and the
/// held failures of [`walls_at`].
pub fn walls(part: &GatePart, reference: &[GateLayer], candidate: &[Sampled], tol: &Tol) -> (Vec<Failure>, usize, Vec<Failure>) {
    let p = part.params();
    let (mut out, mut held, mut by_cpu) = (Vec::new(), Vec::new(), 0);
    for c in candidate {
        let Some(r) = reference.iter().find(|r| r.index == c.index) else {
            out.push(Failure { layer: c.index, island: None, check: Check::Islands, detail: "the reference has no such layer".into() });
            continue;
        };
        let outline = part.compensate(&[c.index], vec![c.outline.clone()]).pop().expect("one layer");
        let h = part.heights[c.index].1;
        let walls: Vec<IslandWalls> = outline.iter().map(|o| cpu_walls(&p, c.index, h, o, r.above.as_deref())).collect();
        by_cpu += walls.len();
        let (f, h) = walls_at(r, &outline, &walls, true, tol);
        out.extend(f);
        held.extend(h);
    }
    (out, by_cpu, held)
}

/// A part's source with a candidate's layers at the heights it has them: what the planner plans
/// the candidate from, its traced outlines compensated and planned whole, as the live engine's.
pub struct Fed {
    inner: Arc<dyn ContourSource + Send + Sync>,
    layers: HashMap<u64, Vec<ExPolygon>>,
}

impl Fed {
    pub fn new(inner: Arc<dyn ContourSource + Send + Sync>, zs: &[f64], layers: impl IntoIterator<Item = Sampled>) -> Self {
        Self { inner, layers: layers.into_iter().map(|s| (zs[s.index].to_bits(), s.outline)).collect() }
    }
}

impl ContourSource for Fed {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.inner.bounds()
    }
    fn pivot_box(&self) -> ([f64; 2], [f64; 2]) {
        self.inner.pivot_box()
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        zs.iter().map(|z| self.layers.get(&z.to_bits()).unwrap_or_else(|| panic!("the candidate has no outline at z {z}")).clone()).collect()
    }
    fn slope_profile(&self, step: f64) -> Option<SlopeProfile> {
        self.inner.slope_profile(step)
    }
}

/// The CPU's contours `cpu` (layer, contours) moved within 0.01 mm, as a candidate: the
/// excuse rule's noise. `way` 0: every edge split at its midpoint, the midpoint pushed 5 µm off
/// the edge, alternately out and in; 1: every vertex moved 8 µm in a direction that turns by the
/// golden angle from one vertex to the next; 2: every ring started half way round, every vertex
/// moved 8 µm along its corner's bisector, alternately in and out.
pub fn resampled(cpu: &[(usize, &[ExPolygon])], way: usize) -> Vec<Sampled> {
    let moved = |r: &Polygon| -> Polygon {
        let n = r.pts.len();
        let f = |p: Pt| [p.x as f64, p.y as f64];
        let unit = |v: [f64; 2]| {
            let l = v[0].hypot(v[1]).max(1e-9);
            [v[0] / l, v[1] / l]
        };
        let left = |p: Pt, q: Pt| {
            let (a, b) = (f(p), f(q));
            unit([a[1] - b[1], b[0] - a[0]])
        };
        let at = |p: [f64; 2], d: [f64; 2], s: f64| Pt { x: (p[0] + d[0] * s).round() as i64, y: (p[1] + d[1] * s).round() as i64 };
        let sign = |i: usize| if i % 2 == 0 { 1.0 } else { -1.0 };
        let um = 1e-3 * SCALE;
        let pts = match way {
            0 => (0..n)
                .flat_map(|i| {
                    let (p, q) = (r.pts[i], r.pts[(i + 1) % n]);
                    let m = [(p.x + q.x) as f64 * 0.5, (p.y + q.y) as f64 * 0.5];
                    [p, at(m, left(p, q), 5.0 * um * sign(i))]
                })
                .collect(),
            1 => (0..n).map(|i| at(f(r.pts[i]), [(i as f64 * 2.399963).cos(), (i as f64 * 2.399963).sin()], 8.0 * um)).collect(),
            _ => (0..n)
                .map(|k| {
                    let i = (k + n / 2) % n;
                    let (o, p, q) = (r.pts[(i + n - 1) % n], r.pts[i], r.pts[(i + 1) % n]);
                    let (u, v) = (left(o, p), left(p, q));
                    at(f(p), unit([u[0] + v[0], u[1] + v[1]]), 8.0 * um * sign(k))
                })
                .collect(),
        };
        Polygon::new(pts)
    };
    cpu.iter()
        .map(|&(index, outline)| {
            let outline = outline.iter().map(|e| ExPolygon { contour: moved(&e.contour), holes: e.holes.iter().map(&moved).collect() }).collect();
            Sampled { index, outline }
        })
        .collect()
}

/// The excuse rule's noise: how many ways [`resampled`] moves the CPU's contours.
pub const NOISE_WAYS: usize = 3;

/// The layers a window `lo..hi` reads, which a [`Fed`] candidate must have.
pub fn reads(part: &GatePart, lo: usize, hi: usize) -> (usize, usize) {
    tp::plan_reads(&part.params(), lo, hi, part.heights.len())
}

/// Window `lo..hi` as the planner plans it from `source` (None: the part's own).
fn planned(part: &GatePart, source: Option<Fed>, lo: usize, hi: usize) -> Vec<ObjectLayer> {
    let window = |input: &ObjectInput| tp::plan_window(par::Ctl::default(), 0, input, &part.job, &part.heights, lo, hi).expect("not cancelled");
    let layers = match source {
        None => window(&part.input),
        Some(fed) => window(&ObjectInput { name: part.name.clone(), source: Box::new(fed), filament: part.input.filament, overrides: part.input.overrides.clone() }),
    };
    layers.into_iter().map(|l| l.2).collect()
}

/// What a case's chosen layer is on the CPU: the class of layer it proves the GPU's walls on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// the first layer
    First,
    /// no skin: no top, bottom or bridge fill
    Mid,
    /// bridge fill
    Bridge,
    /// top skin
    TopSkin,
    /// top skin over overhanging walls: a strut's top
    StrutTop,
    /// a region too thin for whole loops: open thin-wall beads
    Sliver,
    /// walls and no fill
    WallOnly,
}

impl Kind {
    /// Whether the CPU's planned layer `k` is of this kind.
    pub fn holds(self, k: usize, layer: &ObjectLayer) -> bool {
        let has = |r: Role| layer.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)).any(|p| p.role == r);
        match self {
            Kind::First => k == 0,
            Kind::Mid => k > 0 && !has(Role::TopSurface) && !has(Role::BottomSurface) && !has(Role::Bridge),
            Kind::Bridge => has(Role::Bridge),
            Kind::TopSkin => has(Role::TopSurface),
            Kind::StrutTop => has(Role::TopSurface) && has(Role::OverhangWall),
            Kind::Sliver => layer.islands.iter().any(|i| i.walls.iter().any(|p| !p.closed)),
            Kind::WallOnly => layer.islands.iter().all(|i| i.fills.is_empty()) && layer.islands.iter().any(|i| !i.walls.is_empty()),
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Kind::First => "first",
            Kind::Mid => "mid",
            Kind::Bridge => "bridge",
            Kind::TopSkin => "top-skin",
            Kind::StrutTop => "strut-top",
            Kind::Sliver => "sliver",
            Kind::WallOnly => "wall-only",
        })
    }
}

/// The roles a planned layer prints, for a `case` failure.
fn roles(layer: &ObjectLayer) -> String {
    let mut seen: Vec<Role> = Vec::new();
    for p in layer.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)) {
        if !seen.contains(&p.role) {
            seen.push(p.role);
        }
    }
    format!("{seen:?}")
}

/// One part's gate: the verdict and every failure.
pub struct Report {
    pub part: String,
    pub layers: usize,
    pub sampled: Vec<usize>,
    pub windows: Vec<(usize, usize)>,
    /// The candidate's islands the CPU planned at the sampled layers.
    pub planned: usize,
    pub failures: Vec<Failure>,
    /// The deviations excused as the CPU's own noise, per (layer, kind).
    pub excused: std::collections::BTreeMap<(usize, &'static str), usize>,
    /// The layers whose CPU contours miss the zero set (a trace case): (layer, how far off).
    pub unreferenced: Vec<(usize, f64)>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }
}

impl std::fmt::Display for Report {
    /// The verdict line, then one line per failure.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}: walls at {} of {} layers ({} islands planned by the CPU), order and seams in {} windows",
            if self.ok() { "PASS" } else { "FAIL" },
            self.part,
            self.sampled.len(),
            self.layers,
            self.planned,
            self.windows.len()
        )?;
        if !self.unreferenced.is_empty() {
            let shown: Vec<String> = self.unreferenced.iter().map(|(k, d)| format!("{k} ({d:.4} mm)")).collect();
            write!(f, "; the CPU's contours miss the zero set at layers {}, so its walls and plans there are no reference", shown.join(", "))?;
        }
        if !self.excused.is_empty() {
            let mut by_layer: Vec<(usize, Vec<String>)> = Vec::new();
            for (&(layer, kind), &n) in &self.excused {
                let line = format!("{n} {kind}");
                match by_layer.last_mut() {
                    Some((l, v)) if *l == layer => v.push(line),
                    _ => by_layer.push((layer, vec![line])),
                }
            }
            let shown: Vec<String> = by_layer.into_iter().map(|(l, v)| format!("layer {l}: {}", v.join(", "))).collect();
            write!(f, "; excused as the CPU's own noise: {}", shown.join("; "))?;
        }
        if !self.ok() {
            write!(f, ": {} failures\n{}", self.failures.len(), printcheck::lines(&self.failures))?;
        }
        Ok(())
    }
}

/// The whole gate of one part. `candidate` answers every layer asked (the sampled layers and
/// each window's reads) with its outline and plan; an error or a missing layer is a failure.
pub fn run(part: &GatePart, tol: &Tol, candidate: impl FnOnce(&[usize]) -> Result<Vec<Sampled>, String>) -> Report {
    let n = part.heights.len();
    gate(part, tol, sample(n), windows(n), &[], candidate)
}

/// [`run`] at a case's `chosen` layers: walls at each, order and seams in the window of each,
/// and each the [`Kind`] it names on the CPU.
pub fn run_at(part: &GatePart, tol: &Tol, chosen: &[(usize, Kind)], candidate: impl FnOnce(&[usize]) -> Result<Vec<Sampled>, String>) -> Report {
    let n = part.heights.len();
    if let Some(&(k, kind)) = chosen.iter().find(|c| c.0 >= n) {
        let detail = format!("the {kind} layer {k} is past the part's {n} layers");
        let failures = vec![Failure { layer: k, island: None, check: Check::Case, detail }];
        return Report { part: part.name.clone(), layers: n, sampled: Vec::new(), windows: Vec::new(), planned: 0, failures, excused: Default::default(), unreferenced: Vec::new() };
    }
    let at: Vec<usize> = chosen.iter().map(|c| c.0).collect();
    let wins = at.iter().map(|&k| (k, k + 1)).collect();
    gate(part, tol, at, wins, chosen, candidate)
}

fn gate(part: &GatePart, tol: &Tol, at: Vec<usize>, wins: Vec<(usize, usize)>, kinds: &[(usize, Kind)], candidate: impl FnOnce(&[usize]) -> Result<Vec<Sampled>, String>) -> Report {
    let n = part.heights.len();
    let mut need: BTreeSet<usize> = at.iter().copied().collect();
    for &(lo, hi) in &wins {
        let (a, b) = reads(part, lo, hi);
        need.extend(a..b);
    }
    let need: Vec<usize> = need.into_iter().collect();
    let mut report = Report { part: part.name.clone(), layers: n, sampled: at.clone(), windows: wins.clone(), planned: 0, failures: Vec::new(), excused: Default::default(), unreferenced: Vec::new() };
    let fail = |detail: String| Failure { layer: 0, island: None, check: Check::Islands, detail };
    let got = match candidate(&need) {
        Ok(got) => got,
        Err(e) => {
            report.failures.push(fail(format!("the candidate failed: {e}")));
            return report;
        }
    };
    let missing: Vec<usize> = need.iter().copied().filter(|k| !got.iter().any(|s| s.index == *k)).collect();
    if !missing.is_empty() {
        report.failures.push(fail(format!("the candidate has no layers {missing:?}")));
        return report;
    }
    // a trace's reference is the zero set; the CPU's walls only where its own contours hold it
    let mut unref: BTreeSet<usize> = BTreeSet::new();
    // every layer the candidate gave (the planner reads them all), its inside at the sampled ones
    let theirs: Vec<(usize, &[ExPolygon])> = got.iter().filter(|s| need.contains(&s.index)).map(|s| (s.index, s.outline.as_slice())).collect();
    for (k, z) in zero_sets(part, &theirs, &at).unwrap_or_default() {
        if z.off > ZERO_SET_TOL {
            report.failures.push(Failure { layer: k, island: None, check: Check::Outline, detail: format!("the outline is {:.4} mm off the zero set at ({:.3}, {:.3})", z.off, z.at.x_mm(), z.at.y_mm()) });
        }
        if let Some((p, v)) = z.wrong_side {
            let side = if v < 0.0 { "outside" } else { "inside" };
            report.failures.push(Failure { layer: k, island: None, check: Check::Islands, detail: format!("({:.3}, {:.3}) is {side} the outline, the field there is {v:.4}", p.x_mm(), p.y_mm()) });
        }
    }
    // the CPU's contours, traced once: the zero set's, the reference's, the own plans' and the noise's
    let cpu = part.contours(&need);
    let own: Vec<(usize, &[ExPolygon])> = need.iter().copied().zip(cpu.iter().map(Vec::as_slice)).collect();
    for (k, z) in zero_sets(part, &own, &at).unwrap_or_default() {
        if z.off > ZERO_SET_TOL || z.wrong_side.is_some() {
            unref.insert(k);
            report.unreferenced.push((k, z.off));
        }
    }
    let traced: HashMap<usize, &[ExPolygon]> = own.iter().copied().collect();
    let reference = reference_of(part, &at, &traced);
    let sampled: Vec<Sampled> = got.iter().filter(|s| at.contains(&s.index) && !unref.contains(&s.index)).cloned().collect();
    let (failures, by_cpu, held) = walls(part, &reference, &sampled, tol);
    report.failures.extend(failures);
    report.planned = by_cpu;
    let zs = part.zs();
    for (lo, hi) in wins {
        let (a, b) = reads(part, lo, hi);
        // the CPU's window: the planner on the CPU's contours, as on the candidate's
        let window: Vec<(usize, &[ExPolygon])> = (a..b).map(|k| (k, traced[&k])).collect();
        let own = planned(part, Some(Fed::new(part.source.clone(), &zs, window.iter().map(|&(index, o)| Sampled { index, outline: o.to_vec() }))), lo, hi);
        if (a..b).any(|k| unref.contains(&k)) {
            // the CPU plans this window from contours off the zero set: only the case's kinds
            report.failures.extend(case_failures(kinds, lo, hi, &own));
            continue;
        }
        let fed = Fed::new(part.source.clone(), &zs, got.iter().filter(|s| (a..b).contains(&s.index)).cloned());
        let theirs = planned(part, Some(fed), lo, hi);
        // the CPU's own noise, a way at a time while the window has a failing excusable
        // deviation no way so far excuses (the excuse is the widest way's, so later ways only add)
        let unexcused = |noise: &[Vec<ObjectLayer>]| {
            (lo..hi).zip(theirs.iter().zip(&own)).any(|(k, (t, o))| {
                let n: Vec<&ObjectLayer> = noise.iter().filter_map(|w| w.get(k - lo)).collect();
                let failing = printcheck::order_devs(t, o, tol).iter().filter(|d| d.fails.is_some() && d.excusable).count();
                failing > 0 && printcheck::order_within(k, t, o, &n, tol).1.len() < failing
            })
        };
        let mut noise: Vec<Vec<ObjectLayer>> = Vec::new();
        while noise.len() < NOISE_WAYS && unexcused(&noise) {
            noise.push(planned(part, Some(Fed::new(part.source.clone(), &zs, resampled(&window, noise.len()))), lo, hi));
        }
        for (k, (t, o)) in (lo..hi).zip(theirs.iter().zip(&own)) {
            let n: Vec<&ObjectLayer> = noise.iter().filter_map(|w| w.get(k - lo)).collect();
            let (failures, excused) = printcheck::order_within(k, t, o, &n, tol);
            report.failures.extend(failures);
            if !excused.is_empty() {
                report.failures.extend(printcheck::held(k, t, o, tol));
                report.failures.extend(held.iter().filter(|f| f.layer == k).cloned());
                for key in excused {
                    *report.excused.entry((k, key.kind())).or_default() += 1;
                }
            }
        }
        if own.len() != theirs.len() {
            report.failures.push(Failure { layer: lo, island: None, check: Check::Order, detail: format!("{} planned layers, CPU {}", theirs.len(), own.len()) });
        }
        report.failures.extend(case_failures(kinds, lo, hi, &own));
    }
    report
}

/// Each of a case's `chosen` layers the [`Kind`] it names on the CPU: a `case` failure for each
/// that is not, or is past the part's layers.
pub fn kinds(part: &GatePart, chosen: &[(usize, Kind)]) -> Vec<Failure> {
    let n = part.heights.len();
    let past = |k: usize, kind: Kind| vec![Failure { layer: k, island: None, check: Check::Case, detail: format!("the {kind} layer {k} is past the part's {n} layers") }];
    chosen.iter().flat_map(|&(k, kind)| if k >= n { past(k, kind) } else { case_failures(&[(k, kind)], k, k + 1, &planned(part, None, k, k + 1)) }).collect()
}

/// A case's kinds in window `lo..hi` against the CPU's planned window: a `case` failure for each
/// layer that is not the kind it names.
fn case_failures(kinds: &[(usize, Kind)], lo: usize, hi: usize, own: &[ObjectLayer]) -> Vec<Failure> {
    let mut out = Vec::new();
    for &(k, kind) in kinds.iter().filter(|c| (lo..hi).contains(&c.0)) {
        if !own.get(k - lo).is_some_and(|l| kind.holds(k, l)) {
            let prints = own.get(k - lo).map_or_else(|| "nothing".into(), roles);
            out.push(Failure { layer: k, island: None, check: Check::Case, detail: format!("not a {kind} layer on the CPU: it prints {prints}") });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box's top-right corner at (1, 1) as max(x - 1, y - 1): a convex max() corner, flat
    /// walls along x = 1 and y = 1, no distance outside its corner.
    fn corner() -> Field {
        Arc::new(|x: f64, y: f64, _z: f64| (x - 1.0).max(y - 1.0))
    }
    fn near(got: f64, want: f64) -> bool {
        (got - want).abs() <= 0.001
    }

    #[test]
    fn a_point_off_a_flat_wall_reads_its_distance() {
        let f = corner();
        for (q, want) in [([1.03, 0.0], 0.03), ([0.97, 0.0], 0.03), ([0.0, 1.03], 0.03), ([1.0, 0.0], 0.0)] {
            let got = off_plane(&f, &Clearance::uniform(1.0), q, 0.0);
            assert!(near(got, want), "{q:?} reads {got} mm, {want} mm off the wall");
        }
    }

    #[test]
    fn a_point_off_a_convex_corner_reads_its_distance() {
        let f = corner();
        // outside the corner, 0.03 from it every way; inside, 0.03 from both walls
        let outside = [45.0f64, 30.0, 10.0, 60.0, 80.0, 0.0].map(|degrees| {
            let a = degrees.to_radians();
            [1.0 + 0.03 * a.cos(), 1.0 + 0.03 * a.sin()]
        });
        for q in outside.into_iter().chain([[0.97, 0.97]]) {
            let got = off_plane(&f, &Clearance::uniform(1.0), q, 0.0);
            assert!(near(got, 0.03), "{q:?} by the corner reads {got} mm, 0.03 mm off");
        }
    }

    #[test]
    fn a_point_on_a_corner_reads_under_a_micron() {
        let f = corner();
        for q in [[1.0, 1.0], [1.0 - 2.5e-5, 1.0 - 2.5e-5], [1.0 + 2e-7, 1.0 - 3e-7]] {
            let got = off_plane(&f, &Clearance::uniform(1.0), q, 0.0);
            assert!(got < 0.001, "{q:?} on the corner reads {got} mm");
        }
    }

    #[test]
    fn a_layer_is_scored_where_the_slicer_samples_it() {
        // a nearly flat cone (its radius shrinks 1 mm for every 0.001 mm up): the outline the
        // slicer traces at the layer's sampled height is on the zero set there, 0.1 mm off the
        // zero set at the layer's own height
        let slope = 0.001;
        let f: Field = Arc::new(move |x: f64, y: f64, h: f64| slope * (x.hypot(y) - 5.0) + h);
        let z = 0.002;
        let r = 5.0 - (z - SdfSource::BELOW_PLANE) / slope;
        let ring = (0..256).map(|i| {
            let a = i as f64 * std::f64::consts::TAU / 256.0;
            Pt::from_mm(r * a.cos(), r * a.sin())
        }).collect();
        let outline = [ExPolygon::new(Polygon::new(ring))];
        let held = zero_set(&f, &Clearance::uniform(1.0), z, &outline, [-4.0; 2], [4.0; 2], None, 0.02);
        assert!(held.off < 0.001, "the sampled outline reads {} mm off", held.off);
        assert!(off_plane(&f, &Clearance::uniform(1.0), [r, 0.0], z) > 0.05, "the layer's own height is 0.1 mm away");
    }
}

//! Supports for an AI to judge and change: `fab.overhangs` (what needs holding, ranked),
//! `fab.overhang_view` (a picture aimed under a patch), `fab.supports` (a plan alone: what it costs,
//! what it leaves, and each piece's way out). The plan is the planner's own skeleton, the one a slice
//! prints, never the sliced output. Nothing here changes the plate: decisions are settings and
//! `m.support_block` / `m.support_enforce`, through `fab.place` and `fab.apply`. Nothing here says
//! how an overhang prints or how hard a support holds: only where things are and whether a piece
//! that has broken free has a way out.

use crate::out::{arr, flag, int, mm, mm3, n, name, obj, opt, s, S};
use crate::layer_overhangs::{self, LayerPatch, PartLayers};
use crate::overrides::{self, Override, Placed, Reach, Side};
use crate::{edits, str_arg, RenderRequest, Reply, Session};
use fab_loose::grid::{self, DistGrid};
use fab_loose::mesh::PieceMesh;
use fab_loose::pieces::{pieces_of, Piece};
use fab_loose::solid::Solid;
use fab_loose::verdict::{check_piece, Light, PieceVerdict, Verdict};
use fab_model::eval::Evaluated;
use fab_model::{project, Change, Edit};
use fab_plate::PartMesh;
use fab_printability::{overhang_degree, Materials, OverhangLine, OverhangList, Unheld, OVERHANG_COLORS};
use makepad_csg_cad::{measure, Region};
use makepad_csg_math::dvec3;
use makepad_csg_mesh::TriMesh;
use makepad_fab_slice::live::{plan_input_layers, plan_input_paths, LiveInput, PlatePaths};
use makepad_fab_slicer::clip::union_ex;
use makepad_fab_slicer::offset::offset_mm;
use makepad_fab_slicer::geom::{ExPolygon, Pt};
use makepad_fab_toolpath::par;
use makepad_fab_toolpath::skeleton::{GivenUp, Skeleton};
use fab_paint_field::Channel;
use makepad_model::json::Value;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

pub const VIEW_TOOL: &str = "overhang_view";

/// A field part has no mesh of its own: asked for a piece's block or a section, it is meshed this
/// fine (mm on the plate), about half a bead, however fine its script's cell is. Its overhangs are
/// read from its slice's layers.
const SURFACE_CELL: f64 = 0.3;
/// ...and at most this many cells along its longest side.
const SURFACE_CELLS: f64 = 500.0;
/// Rows of an answer, unless asked for others.
const ROWS: usize = 12;
const MOST_ROWS: usize = 40;
/// Field gradients: how far apart (mm) the samples are.
const GRADIENT_STEP: f64 = 0.01;
/// A facet whose own normal points this far up has no overhang, whatever the surface does in it.
const SURELY_UP: f64 = 0.5;
/// A picture's room round its patch: this share of the patch's size, at least this many mm.
const FRAME_SHARE: f64 = 0.35;
const FRAME_LEAST: f64 = 3.0;
/// Patches named in a picture, besides the one it is aimed at.
const MARKS: usize = 5;
/// A section picture's longest side, pixels.
const SECTION_SIDE: f64 = 640.0;
/// The part in pictures: grey, so the overhang colours read.
const PART_GREY: [f32; 3] = [0.55, 0.57, 0.6];
const BRANCH: [f32; 3] = [0.05, 0.75, 0.95];
const PAD: [f32; 3] = [0.2, 0.35, 0.95];
/// Yaw and pitch (degrees) a view from below has: not straight up, so branches read as standing.
pub const BELOW: &str = "30,69";

/// A mesh with a colour per vertex, in plate mm, for a picture.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColoredMesh {
    pub positions: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

impl ColoredMesh {
    fn add(&mut self, mesh: &PieceMesh, color: [f32; 3]) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&mesh.positions);
        self.colors.resize(self.positions.len(), color);
        self.indices.extend(mesh.indices.iter().map(|i| i + base));
    }

    pub(crate) fn triangle(&mut self, corners: [[f64; 3]; 3], color: [f32; 3]) {
        let base = self.positions.len() as u32;
        for c in corners {
            self.positions.push(c.map(|v| v as f32));
            self.colors.push(color);
        }
        self.indices.extend([base, base + 1, base + 2]);
    }
}

/// What a picture aimed at some place on the plate shows.
#[derive(Clone, Debug, PartialEq)]
pub struct LookRequest {
    pub mesh: ColoredMesh,
    /// What the camera fits, plate mm.
    pub frame: ([f64; 3], [f64; 3]),
    /// Named views or "yaw,pitch" in degrees.
    pub views: Vec<String>,
    /// Goes back as the answer's text.
    pub answer: String,
    /// (plate mm, words) to write on the picture.
    pub marks: Vec<([f64; 3], String)>,
}

/// A part's surface in plate mm.
pub struct Surface {
    pub mesh: PartMesh,
    /// The z of each facet's surface normal.
    nz: Vec<f32>,
    /// A field part's mesh rounds its bottom edges by a cell: facets all under this stand on the bed.
    floor: f64,
}

/// Where the support tools' plans come from, so that they are the ones a slice of the plate
/// prints: the committed plate's is its live slice, and a try (a change before it is made) is
/// planned by the same planner from the inputs a live slice of it would have. The host sets it:
/// the app with its print setup and its live slice.
pub trait SliceSource: Send + Sync {
    /// What a live slice of `ev` is made from, and the setup's name for answers.
    fn input(&self, ev: &Evaluated) -> Result<(LiveInput, String), String>;
    /// The live slice of `input` (the committed plate's), once it is whole and what its supports
    /// tell is in.
    fn sliced(&self, input: &LiveInput, cancelled: Option<&dyn Fn() -> bool>) -> Result<Arc<PlatePaths>, String>;
    /// The live slice of `ev`'s plate (the committed plate's) when it is made already; none
    /// before, and nothing waits for it.
    fn made(&self, ev: &Evaluated) -> Option<Arc<PlatePaths>>;
}

/// How far past its footprint's edge (mm) a face's point still lies over it: a face's rings run
/// along the part's side, and where that stands straight it is the footprint's edge, which a point
/// exactly on it may fall either side of.
const FOOTPRINT_EDGE: f64 = 0.01;

/// One part's plan.
pub struct PartPlan {
    /// Where the plan's frame sits on the plate.
    pub at: [f64; 2],
    pub skeleton: Arc<Skeleton>,
    /// What the supports extrude, mm^3, and how long the extruding takes, seconds: no travel,
    /// no cooling slowdowns.
    pub volume_mm3: f64,
    pub seconds: f64,
    /// Its supports stand on the bed only: what's over the part gets none, enforced or not.
    pub bed_only: bool,
    /// The part's own layers as the slice cuts them: its overhangs are read from them, so a field
    /// part is never meshed for them.
    pub layers: Arc<PartLayers>,
    /// Where the part's own settings keep its trees out, as its plan keeps them out: per model
    /// layer, its footprint so far where it keeps its trees out of it, [`FOOTPRINT_EDGE`] wider
    /// (none when it does not)...
    pub footprint: Arc<Vec<Vec<ExPolygon>>>,
    /// ...and whether its no-trees areas (`m.no_trees`) do: they do when its supports are trees.
    pub no_trees: bool,
}

impl PartPlan {
    /// What its own settings keep the trees out from under, as its plan tells it and Printability
    /// counts it: the overhang layer by layer, over its footprint (`k` 0) or a no-trees area (1).
    /// Each layer's record with the plate point of its widest place.
    fn kept_told(&self, k: usize) -> impl Iterator<Item = ([f64; 3], &GivenUp)> + '_ {
        let sk = &self.skeleton;
        let told = if k == 0 { &sk.kept_out } else { &sk.no_trees };
        told.iter().map(move |g| ([g.at[0] + self.at[0], g.at[1] + self.at[1], sk.heights.get(g.layer).map_or(0.0, |h| h.0)], g))
    }

    /// [`PartPlan::kept_told`]'s area (mm²) of what `keep` keeps, as Printability tells it.
    fn kept_area(&self, k: usize, keep: impl Fn([f64; 3]) -> bool) -> f64 {
        let told: Vec<&GivenUp> = self.kept_told(k).filter(|(at, _)| keep(*at)).map(|(_, g)| g).collect();
        fab_printability::told_area(&told)
    }
}

/// A plate's supports, planned and not printed.
pub struct Planned {
    pub ms: f64,
    pub setup: String,
    pub materials: Materials,
    pub parts: Vec<PartPlan>,
}

/// What stays between calls: looking again, or trying a setting, doesn't mesh or plan from nothing.
#[derive(Default)]
pub struct Memo {
    surfaces: HashMap<u64, Arc<Surface>>,
    /// The last plans asked for, the latest first: the committed plate's and its tries'.
    plans: Vec<(u64, Arc<Planned>)>,
    grids: GridMemo,
    verdicts: VerdictMemo,
}

type GridMemo = Arc<Mutex<HashMap<u64, (Arc<Solid>, Arc<DistGrid>)>>>;
/// Each piece's way out by what it is made of ([`overrides::piece_key`]): a try checks only the
/// pieces it changed.
type VerdictMemo = Arc<Mutex<HashMap<u64, PieceVerdict>>>;

/// Enough parts for any one plate, tried or committed.
const MEMO_PARTS: usize = 32;
/// The committed plate's plan and the two last tries'.
const MEMO_PLANS: usize = 3;
/// Enough pieces for a few plates' worth of tries.
const MEMO_VERDICTS: usize = 4096;
/// An uncovered patch comes with a ready-made enforce region when the plan covers less than this
/// share of it and leaves at least this much (mm²).
const ENFORCE_BELOW: f64 = 0.5;
const ENFORCE_LEAST: f64 = 1.0;
/// A patch is given up when what the plan told it gave up there is at least this share of what it
/// leaves, as the bed sees it.
const GIVEN_UP_SHARE: f64 = 0.5;

/// One patch on the plate.
struct Found {
    part: usize,
    /// 1-based, largest first over the plate: `p1` is the plate's largest.
    id: usize,
    patch: LayerPatch,
    /// None: this plate plans no supports for it.
    left_mm2: Option<f64>,
    /// What's under the patch's lowest face, and how far.
    over_part: bool,
    clear: f64,
    /// The share of its area an `m.support_block` holds.
    blocked: f64,
    /// Of what it leaves, what the part's own settings keep the trees out from under (mm²): over
    /// its footprint, and over a no-trees area. Neither is in `left_mm2`, nor asked of the
    /// supports ([`Found::area`]).
    kept: [f64; 2],
    /// The plan gave up most of what it leaves there: told as having no support under it, as none
    /// could reach it or the part is in the way.
    given_up: bool,
    /// The plan left most of it because a support under it would be locked in the part.
    locked: bool,
    /// A ready-made `m.support_enforce` for it, when the plan leaves most of it and nothing says why.
    enforce: Option<Override>,
}

impl Found {
    fn name(&self) -> String {
        format!("p{}", self.id)
    }

    /// What of its area the supports are asked to hold: all but what the part's own settings keep
    /// the trees out from under.
    fn area(&self) -> f64 {
        (self.patch.area_mm2 - self.kept[0] - self.kept[1]).max(0.0)
    }
}

/// Less than this (mm²) asked of the supports is no patch of theirs: all of it is the part's own
/// settings' to keep the trees out from under.
const ASKED_LEAST: f64 = 0.01;

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::F64(f) if f.is_finite() => Some(*f),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    }
}

fn hash_part(ev: &Evaluated, i: usize) -> u64 {
    let mut h = DefaultHasher::new();
    if let Some(field) = ev.fields.get(i).and_then(Option::as_ref) {
        field.key.hash(&mut h);
        for row in &field.to_printer {
            for v in row {
                v.to_bits().hash(&mut h);
            }
        }
    } else {
        let mesh = &ev.plate.parts[i].mesh;
        for p in &mesh.positions {
            for v in p {
                v.to_bits().hash(&mut h);
            }
        }
        mesh.triangles.hash(&mut h);
    }
    h.finish()
}

/// The part as a distance on the plate: its field when it is one, else its mesh.
fn solid_of(ev: &Evaluated, i: usize) -> Result<Solid, String> {
    if let Some(field) = ev.fields.get(i).and_then(Option::as_ref) {
        let offset = ev.frames.get(i).map_or([0.0; 3], |f| f.offset);
        let bounds = field.bounds.as_ref().map(|b| (std::array::from_fn(|k| b.min[k] + offset[k]), std::array::from_fn(|k| b.max[k] + offset[k])));
        let (shape, _, to_printer, _) = field.distance();
        return Ok(Solid::of_field(shape, *to_printer, bounds));
    }
    let mesh = &ev.plate.parts[i].mesh;
    if mesh.triangles.is_empty() {
        return Err(format!("{} has no solid geometry to check", ev.plate.parts[i].name));
    }
    Ok(Solid::of_mesh(mesh.positions.clone(), mesh.triangles.clone()))
}

/// A field part's surface: meshed at [`SURFACE_CELL`], its facets judged by the field's own normal.
fn field_surface(ev: &Evaluated, i: usize) -> Result<Surface, String> {
    let field = ev.fields[i].as_ref().ok_or("not a field part")?;
    let m = field.to_printer;
    // the most the placement stretches a length (a part scaled by axis): cells no coarser on the
    // plate than asked along any axis
    let scale = fab_plate::stretch(&m).max(1e-9);
    let (lo, hi) = (field.shape.min, field.shape.max);
    let longest = (0..3).map(|k| (hi[k] - lo[k]) * scale).fold(0.0, f64::max);
    let on_plate = SURFACE_CELL.max(longest / SURFACE_CELLS);
    let cell = on_plate / scale;
    let v = |p: [f64; 3]| dvec3(p[0], p[1], p[2]);
    let made = makepad_csg_sdf::mesh_field(field.shape.field(), v(lo), v(hi), cell, field.shape.clearance());
    let positions: Vec<[f64; 3]> = made.vertices.iter().map(|p| fab_model::weave::affine(&m, [p.x, p.y, p.z])).collect();
    let mesh = PartMesh { positions, triangles: made.triangles };
    let solid = solid_of(ev, i)?;
    let mut nz = facet_nz(&mesh);
    // The surface's normal where it might overhang: the mesh's facets step across it.
    let down: Vec<usize> = (0..nz.len()).filter(|&t| (nz[t] as f64) < SURELY_UP).collect();
    let mut at = Vec::with_capacity(down.len() * 6);
    for &t in &down {
        let [a, b, c] = mesh.triangles[t].map(|v| mesh.positions[v as usize]);
        let o: [f64; 3] = std::array::from_fn(|k| (a[k] + b[k] + c[k]) / 3.0);
        for k in 0..3 {
            for sign in [GRADIENT_STEP, -GRADIENT_STEP] {
                let mut p = o;
                p[k] += sign;
                at.push(p);
            }
        }
    }
    let mut values = vec![0.0; at.len()];
    solid.values(&at, &mut values);
    for (k, &t) in down.iter().enumerate() {
        let g: [f64; 3] = std::array::from_fn(|a| values[k * 6 + a * 2] - values[k * 6 + a * 2 + 1]);
        let l = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
        if l > 1e-12 {
            nz[t] = (g[2] / l) as f32;
        }
    }
    let floor = mesh.positions.iter().map(|p| p[2]).fold(f64::MAX, f64::min) + on_plate * 1.01;
    Ok(Surface { mesh, nz, floor })
}

fn facet_nz(mesh: &PartMesh) -> Vec<f32> {
    mesh.triangles.iter().map(|t| {
        let [a, b, c] = t.map(|v| mesh.positions[v as usize]);
        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
        let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if l > 1e-12 { (n[2] / l) as f32 } else { 1.0 }
    }).collect()
}

/// What a part's supports extrude (mm^3) and how long that takes (s): their paths in `layers`.
fn support_figures<'a>(layers: impl Iterator<Item = &'a makepad_fab_toolpath::ObjectLayer>) -> (f64, f64) {
    use makepad_fab_toolpath::Role;
    let (mut volume_mm3, mut seconds) = (0.0, 0.0);
    for layer in layers {
        for island in &layer.islands {
            for path in island.walls.iter().chain(&island.fills).filter(|p| matches!(p.role, Role::Support | Role::SupportInterface | Role::SupportTransition)) {
                let length = path.length_mm();
                volume_mm3 += length * path.area;
                if path.speed > 0.0 {
                    seconds += length / path.speed;
                }
            }
        }
    }
    (volume_mm3, seconds)
}

/// What each part's supports of a try extrude (mm^3) and take (s), as [`plan_of`] figures them:
/// the same paths, without the tell, the layers and the footprint it plans too.
fn figures_of(ev: &Evaluated, input: &LiveInput) -> Result<Vec<(f64, f64)>, String> {
    let figures: Vec<(f64, f64)> = plan_input_paths(par::Ctl::default(), input)?.into_iter().zip(&ev.frames).map(|(p, _)| support_figures(p.additions.iter().map(|a| &a.3))).collect();
    if figures.len() != ev.plate.parts.len() {
        return Err(format!("the slice has {} parts, the plate {}", figures.len(), ev.plate.parts.len()));
    }
    Ok(figures)
}

/// The plate's plan: its live slice's when it is the committed plate, else (a try) the same
/// planner's from the same inputs.
fn plan_of(ev: &Evaluated, (input, name): (LiveInput, String), source: &dyn SliceSource, tried: bool, cancelled: Option<&dyn Fn() -> bool>) -> Result<Planned, String> {
    let t = std::time::Instant::now();
    let job = makepad_fab_slice::prepare_job(&(input.setup)(None)?)?;
    let materials = Materials::from_job(&job);
    let planned: Vec<(Arc<Skeleton>, [f64; 2], (f64, f64), PartLayers)> = if tried {
        plan_input_layers(par::Ctl::default(), &input)?.into_iter().zip(&ev.frames).map(|((p, outline, heights), f)| {
            let figures = support_figures(p.additions.iter().map(|a| &a.3));
            let layers = PartLayers::new(heights.into_iter().zip(outline).map(|((z, h), o)| (z, h, o)).collect());
            (Arc::new(p.skeleton), [f.offset[0], f.offset[1]], figures, layers)
        }).collect()
    } else {
        let sliced = source.sliced(&input, cancelled)?;
        sliced.plans().iter().map(|(plan, at)| Ok((plan.skeleton.told().ok_or("the plate's support report is not in yet")?, [at.x_mm(), at.y_mm()], support_figures(plan.layers.iter().map(|l| &l.2)), PartLayers::of_plan(&plan.layers)))).collect::<Result<_, String>>()?
    };
    if planned.len() != ev.plate.parts.len() {
        return Err(format!("the slice has {} parts, the plate {}", planned.len(), ev.plate.parts.len()));
    }
    let parts = planned.into_iter().zip(&ev.plate.parts).map(|((skeleton, at, (volume_mm3, seconds), layers), part)| {
        let own = (!part.print.is_empty()).then(|| job.with_overrides(&part.print));
        let job = own.as_ref().unwrap_or(&job);
        let bed_only = job.b("support_on_build_plate_only");
        // Its trees keep out of its footprint and its no-trees areas, as the plan's keep-out has it.
        let trees = makepad_fab_toolpath::tree_told(job);
        let outlines: Vec<Vec<ExPolygon>> = match trees {
            Some(ref t) if t.outside_footprint => layers.layers.iter().map(|l| l.2.clone()).collect(),
            _ => Vec::new(),
        };
        let footprint: Vec<Vec<ExPolygon>> = makepad_fab_toolpath::footprint_below(par::Ctl::default(), &outlines).unwrap_or_default().iter().map(|f| offset_mm(f, FOOTPRINT_EDGE)).collect();
        PartPlan { at, skeleton, volume_mm3, seconds, bed_only, layers: Arc::new(layers), footprint: Arc::new(footprint), no_trees: trees.is_some() }
    }).collect();
    Ok(Planned { ms: t.elapsed().as_secs_f64() * 1e3, setup: name, materials, parts })
}

/// Where a plan reaches the part: its tips, its pads and its footprints, for "is there support
/// within reach of here".
fn contacts_of(plans: &[PartPlan], reach: f64) -> Reach {
    let mut c = Reach::new(reach);
    let mut points = Vec::new();
    for plan in plans {
        let sk = &plan.skeleton;
        let joined = overrides::joined(sk);
        for (k, chain) in sk.chains.iter().enumerate() {
            // A stretch nothing merges into starts at a tip.
            if let (false, Some(&(x, y, _))) = (joined[k], chain.sections.first()) {
                points.push([x + plan.at[0], y + plan.at[1], sk.heights[chain.top].0]);
            }
        }
        for (l, pads) in sk.roofs.iter().enumerate() {
            let z = sk.heights.get(l).map_or(0.0, |h| h.0);
            for e in pads {
                overrides::region_points(e, plan.at, z, reach, &mut points);
            }
        }
        for slab in &sk.slabs {
            for e in union_ex(&slab.region) {
                overrides::region_points(&e, plan.at, slab.z, reach, &mut points);
            }
        }
    }
    for p in points {
        c.add(p);
    }
    c
}

/// One line of an overhang answer: a patch, or patches near each other.
struct Row {
    of: Vec<usize>,
}

/// Rows covering all of `found`: the largest still-unsupported patches on their own, the rest as
/// clusters of neighbours, split where the most area lies until the rows are used.
fn rows_of(found: &[&Found], limit: usize) -> Vec<Row> {
    let left = |f: &Found| f.left_mm2.unwrap_or(f.area());
    let mut order: Vec<usize> = (0..found.len()).collect();
    order.sort_by(|&a, &b| left(found[b]).total_cmp(&left(found[a])).then(found[b].area().total_cmp(&found[a].area())).then(found[a].id.cmp(&found[b].id)));
    if found.len() <= limit {
        return order.into_iter().map(|i| Row { of: vec![i] }).collect();
    }
    let alone = limit / 2;
    let mut rows: Vec<Row> = order[..alone].iter().map(|&i| Row { of: vec![i] }).collect();
    let mut groups = vec![order[alone..].to_vec()];
    let area = |g: &[usize]| g.iter().map(|&i| found[i].area()).sum::<f64>();
    while groups.len() < limit - alone {
        let Some((k, _)) = groups.iter().enumerate().filter(|(_, g)| g.len() > 1).max_by(|a, b| area(a.1).total_cmp(&area(b.1))) else { break };
        let mut g = groups.swap_remove(k);
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for &i in &g {
            for a in 0..3 {
                lo[a] = lo[a].min(found[i].patch.centroid[a]);
                hi[a] = hi[a].max(found[i].patch.centroid[a]);
            }
        }
        let axis = (0..3).max_by(|&a, &b| (hi[a] - lo[a]).total_cmp(&(hi[b] - lo[b]))).unwrap_or(0);
        g.sort_by(|&a, &b| found[a].patch.centroid[axis].total_cmp(&found[b].patch.centroid[axis]).then(found[a].id.cmp(&found[b].id)));
        let half = area(&g) / 2.0;
        let mut run = 0.0;
        let mut cut = 1;
        for (j, &i) in g.iter().enumerate().take(g.len() - 1) {
            run += found[i].area();
            cut = j + 1;
            if run >= half {
                break;
            }
        }
        let rest = g.split_off(cut);
        groups.push(g);
        groups.push(rest);
    }
    rows.extend(groups.into_iter().map(|of| Row { of }));
    rows.sort_by(|a, b| {
        let sum = |r: &Row| r.of.iter().map(|&i| left(found[i])).sum::<f64>();
        sum(b).total_cmp(&sum(a))
    });
    rows
}

/// The row as the Slice panel lists it.
fn line_of(row: &Row, found: &[&Found], ev: &Evaluated) -> OverhangLine {
    let area: f64 = row.of.iter().map(|&i| found[i].area()).sum();
    let planned = row.of.iter().all(|&i| found[i].left_mm2.is_some());
    let left: f64 = row.of.iter().map(|&i| found[i].left_mm2.unwrap_or(0.0)).sum();
    let lowest = row.of.iter().map(|&i| found[i]).min_by(|a, b| a.patch.lowest[2].total_cmp(&b.patch.lowest[2])).expect("a row has patches");
    let (mut min, mut max) = ([f64::MAX; 3], [f64::MIN; 3]);
    for &i in &row.of {
        for a in 0..3 {
            min[a] = min[a].min(found[i].patch.min[a]);
            max[a] = max[a].max(found[i].patch.max[a]);
        }
    }
    // Why a patch is left, when much of it is: told by the plan, or blocked by the script.
    let unheld = match row.of[..] {
        [i] if found[i].left_mm2.unwrap_or(found[i].area()) > 0.25 * found[i].area() => {
            let f = found[i];
            if f.given_up { Some(Unheld::GivenUp) } else if f.locked { Some(Unheld::Locked) } else if f.blocked >= 0.5 { Some(Unheld::Blocked) } else { None }
        }
        _ => None,
    };
    OverhangLine {
        part: ev.plate.parts[lowest.part].name.clone(),
        id: (row.of.len() == 1).then(|| found[row.of[0]].id),
        patches: row.of.len(),
        area_mm2: area,
        left_mm2: planned.then_some(left),
        over_part: row.of.iter().filter(|&&i| found[i].over_part).count(),
        unheld,
        min,
        max,
        members: members_of(row, found, ev),
    }
}

/// A cluster's patches, each as its own line, worst first as [`rows_of`] ranks them.
fn members_of(row: &Row, found: &[&Found], ev: &Evaluated) -> Vec<OverhangLine> {
    if row.of.len() < 2 {
        return Vec::new();
    }
    let left = |f: &Found| f.left_mm2.unwrap_or(f.area());
    let mut of = row.of.clone();
    of.sort_by(|&a, &b| left(found[b]).total_cmp(&left(found[a])).then(found[b].area().total_cmp(&found[a].area())).then(found[a].id.cmp(&found[b].id)));
    of.into_iter().map(|i| line_of(&Row { of: vec![i] }, found, ev)).collect()
}

/// What the parts' own settings keep the trees out from under, as the Slice panel lists it: a line
/// per part and reason, all of it left, over the patches it lies in. Its area and box are its
/// plan's, the overhang layer by layer as Printability tells it ([`PartPlan::kept_told`]).
fn chosen_lines(found: &[Found], plan: &Planned, ev: &Evaluated) -> Vec<OverhangLine> {
    let mut lines = Vec::new();
    for (part, pp) in plan.parts.iter().enumerate() {
        for (k, unheld) in [Unheld::Footprint, Unheld::NoTrees].into_iter().enumerate() {
            let area = pp.kept_area(k, |_| true);
            if area < ASKED_LEAST {
                continue;
            }
            let of: Vec<&Found> = found.iter().filter(|f| f.part == part && f.kept[k] > 0.0).collect();
            let (mut min, mut max) = ([f64::MAX; 3], [f64::MIN; 3]);
            for (_, g) in pp.kept_told(k) {
                let (z, h) = pp.skeleton.heights.get(g.layer).copied().unwrap_or_default();
                let (lo, hi) = ([g.bounds.min.x_mm() + pp.at[0], g.bounds.min.y_mm() + pp.at[1], z - h], [g.bounds.max.x_mm() + pp.at[0], g.bounds.max.y_mm() + pp.at[1], z]);
                for a in 0..3 {
                    min[a] = min[a].min(lo[a]);
                    max[a] = max[a].max(hi[a]);
                }
            }
            lines.push(OverhangLine {
                part: ev.plate.parts[part].name.clone(),
                id: None,
                patches: of.len(),
                area_mm2: area,
                left_mm2: Some(area),
                over_part: of.iter().filter(|f| f.over_part).count(),
                unheld: Some(unheld),
                min,
                max,
                members: Vec::new(),
            });
        }
    }
    lines
}

fn row_value(row: &Row, found: &[&Found], ev: &Evaluated) -> S {
    let line = line_of(row, found, ev);
    let covered = opt(line.held().map(n));
    let worst = row.of.iter().map(|&i| found[i].patch.worst).max().unwrap_or(0);
    let lowest = row.of.iter().map(|&i| found[i]).min_by(|a, b| a.patch.lowest[2].total_cmp(&b.patch.lowest[2])).expect("a row has patches");
    let part = &ev.plate.parts[lowest.part];
    if let [i] = row.of[..] {
        let f = found[i];
        let mut fields = vec![
            ("id", name(&f.name())), ("object", s(&part.object)), ("area_mm2", n(line.area_mm2)), ("at", mm3(f.patch.centroid)), ("low", mm3(f.patch.lowest)),
            ("over", name(if f.over_part { "part" } else { "bed" })), ("clear", mm(f.clear)), ("covered", covered), ("worst", int(worst)),
        ];
        if f.blocked > 0.0 {
            fields.push(("blocked", n(f.blocked)));
        }
        for (kept, key) in f.kept.iter().zip(["kept_out_mm2", "no_trees_mm2"]) {
            if *kept > 0.0 {
                fields.push((key, n(*kept)));
            }
        }
        if f.left_mm2.unwrap_or(f.area()) > 0.25 * f.area() {
            if f.given_up {
                fields.push(("given_up", flag(true)));
            }
            if f.locked {
                fields.push(("locked", flag(true)));
            }
        }
        if let Some(o) = &f.enforce {
            fields.extend(override_fields(o, "enforce"));
        }
        return obj(fields);
    }
    let least_clear = row.of.iter().map(|&i| found[i].clear).fold(f64::MAX, f64::min);
    obj(vec![
        ("patches", int(row.of.len())), ("largest", name(&found[row.of.iter().copied().min_by_key(|&i| found[i].id).expect("a row has patches")].name())), ("object", s(&part.object)), ("area_mm2", n(line.area_mm2)),
        ("box", arr([mm3(line.min), mm3(line.max)])), ("low", mm3(lowest.patch.lowest)),
        ("over_part", int(line.over_part)), ("least_clear", mm(least_clear)), ("covered", covered), ("worst", int(worst)),
    ])
}

/// `angle`: faces flatter than this from the bed (degrees) count, the plan's own threshold when left out.
fn angle_arg(args: &Value) -> Result<Option<f64>, String> {
    match args.get("angle") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => number(v).filter(|a| *a > 0.0 && *a < 90.0).map(Some).ok_or_else(|| "'angle' is degrees from the bed, over 0 and under 90".to_string()),
    }
}

fn box_arg(v: &Value) -> Result<([f64; 3], [f64; 3]), String> {
    let wrong = || "'box' is [[x y z] [x y z]] in plate mm: two opposite corners".to_string();
    let a = v.as_arr().filter(|a| a.len() == 2).ok_or_else(wrong)?;
    let corner = |c: &Value| -> Result<[f64; 3], String> {
        let c = c.as_arr().filter(|c| c.len() == 3).ok_or_else(wrong)?;
        Ok([number(&c[0]).ok_or_else(wrong)?, number(&c[1]).ok_or_else(wrong)?, number(&c[2]).ok_or_else(wrong)?])
    };
    let (p, q) = (corner(&a[0])?, corner(&a[1])?);
    Ok((std::array::from_fn(|k| p[k].min(q[k])), std::array::from_fn(|k| p[k].max(q[k]))))
}

fn in_box(p: [f64; 3], b: &([f64; 3], [f64; 3])) -> bool {
    (0..3).all(|k| p[k] >= b.0[k] && p[k] <= b.1[k])
}

/// What's tried with a call, committing nothing: `place` as in fab.place, `edits` and `script` as in fab.apply.
fn tried_changes(session: &Session, args: &Value) -> Result<Vec<Change>, String> {
    let mut changes = Vec::new();
    if let Some(text) = str_arg(args, "place") {
        changes.extend(project::changes(text).map_err(|d| d.iter().map(|d| format!("place {d}")).collect::<Vec<_>>().join("; "))?);
    }
    // One script's edits, or {"file": "edits", ...} for several at once.
    let scripts: Vec<(String, &str)> = match args.get("edits") {
        None => Vec::new(),
        Some(Value::Str(text)) => vec![(session.script_arg(args)?, text.as_str())],
        Some(Value::Obj(each)) => each.iter().map(|(file, text)| text.as_str().map(|t| (file.clone(), t)).ok_or_else(|| format!("the edits for '{file}' are text"))).collect::<Result<_, _>>()?,
        Some(_) => return Err("'edits' is script statements, or an object of them by script file".into()),
    };
    for (file, text) in scripts {
        if session.model.source(&file).is_none() {
            return Err(format!("no script '{file}' in the project"));
        }
        changes.extend(edits::parse(text)?.into_iter().map(|edit: Edit| Change::Script { file: file.clone(), edit }));
    }
    Ok(changes)
}

/// What `fab.supports` found, for its answer and its verdicts.
struct Asked {
    answer: S,
    ev: Arc<Evaluated>,
    plan: Arc<Planned>,
    /// A try's committed plate.
    was: Option<(Arc<Evaluated>, Arc<Planned>)>,
}

/// A part's plan in figures, to say what a try changed.
struct Figures {
    pieces: usize,
    volume: f64,
    overhang: f64,
    left: f64,
}

impl Figures {
    fn of(plan: &Planned, found: &[Found], i: usize) -> Figures {
        let of_part = || found.iter().filter(|f| f.part == i);
        Figures {
            pieces: plan.parts[i].skeleton.pieces().len(),
            volume: plan.parts[i].volume_mm3,
            overhang: of_part().map(Found::area).sum(),
            left: of_part().map(|f| f.left_mm2.unwrap_or(f.area())).sum(),
        }
    }

    fn differs(&self, other: &Figures) -> bool {
        self.pieces != other.pieces || (self.volume - other.volume).abs() > 0.5 || (self.overhang - other.overhang).abs() > 0.05 || (self.left - other.left).abs() > 0.05
    }
}

/// How near a plan's tip, pad or footprint must come to a point of a face to hold it (mm): a tip's
/// neighbours are a contact spacing apart (a line width at 30%), and between four of them a point
/// is 0.7 of that from the nearest.
fn reach_of(plan: &Planned) -> f64 {
    plan.materials.line_width.max(0.1) / 0.3 * 0.75
}

/// Every overhang patch on the plate, largest first, with how the plan meets it: read from each
/// part's layers as its slice cuts them, past `angle` from the bed when given, else past its own
/// limit.
fn found_overhangs(ev: &Evaluated, plan: &Planned, placed: &[Placed], angle: Option<f64>) -> Vec<Found> {
    let reach = reach_of(plan);
    let contacts = contacts_of(&plan.parts, reach);
    let mut found = Vec::new();
    for (i, part) in ev.plate.parts.iter().enumerate() {
        let side = &placed[i];
        let materials = plan.materials.of_part(part);
        let limit = angle.map_or(materials.overhang_angle_deg, |a| 90.0 - a);
        let (layers, at, sk) = (&plan.parts[i].layers, plan.parts[i].at, &plan.parts[i].skeleton);
        let paint = ev.paint.get(i).and_then(Option::as_ref).filter(|p| p.program.has(Channel::SupportBlock));
        // Where its own settings keep its trees out, on the layer under a face: its footprint, else
        // a no-trees area at that layer's middle. 0 and 1, as `Found::kept` counts them.
        let (footprint, no_trees) = (&plan.parts[i].footprint, ev.paint.get(i).and_then(Option::as_ref).filter(|p| plan.parts[i].no_trees && p.program.has(Channel::NoTrees)));
        let kept_by = |p: [f64; 3], layer: usize| -> Option<usize> {
            let under = layer.checked_sub(1)?;
            let q = Pt::from_mm(p[0] - at[0], p[1] - at[1]);
            if footprint.get(under).is_some_and(|f| f.iter().any(|e| e.contains(q))) {
                return Some(0);
            }
            let middle = p[2] - layers.layers.get(under).map_or(0.0, |l| l.1) / 2.0;
            no_trees.filter(|paint| paint.program.sample(Channel::NoTrees, fab_model::weave::affine(&paint.from_printer, [p[0], p[1], middle])).slot != 0).map(|_| 1)
        };
        for patch in layer_overhangs::patches(layers, at, limit, reach) {
            // Its points a reach apart, each with its share of the face it lies on and the layer
            // the face is the underside of.
            let samples: Vec<([f64; 3], f64, usize)> = patch.faces.iter().flat_map(|f| {
                let points = layer_overhangs::region_samples(&f.region, reach);
                let share = f.area_mm2 / points.len() as f64;
                points.into_iter().map(move |q| ([q[0] + at[0], q[1] + at[1], f.z], share, f.layer))
            }).collect();
            // What it leaves (the plan tells what it gives up as the bed sees it, as the patch is),
            // less what the part's own settings keep the trees out from under.
            let mut kept = [0.0; 2];
            let left_mm2 = materials.supports.then(|| samples.iter().filter(|(p, _, _)| !contacts.reaches(*p, side.below)).filter_map(|&(p, share, layer)| match kept_by(p, layer) {
                Some(by) => {
                    kept[by] += share;
                    None
                }
                None => Some(share),
            }).sum::<f64>());
            let blocked = paint.map_or(0.0, |paint| {
                let held: f64 = samples.iter().filter(|(p, _, _)| paint.program.sample(Channel::SupportBlock, fab_model::weave::affine(&paint.from_printer, *p)).slot != 0).map(|(_, share, _)| share).sum();
                if patch.area_mm2 > 0.0 { held / patch.area_mm2 } else { 0.0 }
            });
            let (lo, hi) = (patch.min, patch.max);
            // Given up when what the plan told it gave up there is much of what it leaves: not
            // when it is a neighbouring face's that shares its box.
            let told = |list: &[GivenUp]| -> f64 {
                list.iter().filter(|g| {
                    let Some(&(z, h)) = sk.heights.get(g.layer) else { return false };
                    let (x, y) = (g.at[0] + at[0], g.at[1] + at[1]);
                    z >= lo[2] - h && z - h <= hi[2] + h && x >= lo[0] - reach && x <= hi[0] + reach && y >= lo[1] - reach && y <= hi[1] + reach
                }).map(|g| g.area).sum()
            };
            let most = |told: f64| told > 0.0 && left_mm2.is_none_or(|left| told >= GIVEN_UP_SHARE * left);
            let given_up = most(told(&sk.given_up) + told(&sk.inside));
            // The plan keeps out of where a support would be locked in the part.
            let locked = most(told(&sk.locked));
            // From a point on its lowest face: an edge may stand right on a wall.
            let hit = layer_overhangs::below(layers, at, patch.low, patch.lowest);
            let clear = patch.lowest[2] - hit.unwrap_or(0.0);
            // Worth enforcing: most of it left, nothing saying why, room under its middle for a
            // support (a slope may start at the bed), and ground for one to stand on (a plan from
            // the bed only stands on nothing of the part).
            let over_part = hit.is_some();
            let ground = !(over_part && plan.parts[i].bed_only);
            let room = patch.mark[2] - layer_overhangs::below(layers, at, patch.mark_layer, patch.mark).unwrap_or(0.0);
            let asked = patch.area_mm2 - kept[0] - kept[1];
            let worth = |left: &f64| *left >= ENFORCE_LEAST && *left > (1.0 - ENFORCE_BELOW) * asked && blocked < 0.5 && !given_up && !locked && room > side.least_clear && ground;
            let enforce = left_mm2.filter(worth).map(|_| overrides::enforce_around(side, &patch.outline_points(at), patch.area_mm2));
            found.push(Found { part: i, id: 0, patch, left_mm2, over_part, clear, blocked, kept, given_up, locked, enforce });
        }
    }
    found.sort_by(|a, b| b.patch.area_mm2.total_cmp(&a.patch.area_mm2).then(a.patch.centroid[2].total_cmp(&b.patch.centroid[2])).then(a.patch.centroid[0].total_cmp(&b.patch.centroid[0])).then(a.patch.centroid[1].total_cmp(&b.patch.centroid[1])));
    for (k, f) in found.iter_mut().enumerate() {
        f.id = k + 1;
    }
    found
}

/// An override's fields in an answer.
fn override_fields(o: &Override, what: &'static str) -> Vec<(&'static str, S)> {
    let mut f = vec![(what, s(&o.statement))];
    if let Some(script) = &o.script {
        f.push(("script", s(script)));
    }
    if what == "block" {
        f.push(("gives_up_mm2", n(o.area_mm2)));
    }
    if o.others > 0 {
        f.push(("also_reaches", int(o.others)));
    }
    f
}

impl Session {
    /// The plate this call asks about: the committed one, or what's tried with it.
    pub(crate) fn support_plate(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Result<(Arc<Evaluated>, bool), String> {
        let changes = tried_changes(self, args)?;
        if changes.is_empty() {
            return Ok((self.last.clone().ok_or("nothing is built yet")?, false));
        }
        Ok((self.tried_plate(&changes, cancelled)?, true))
    }

    /// The plate these changes make, nothing committed.
    pub(crate) fn tried_plate(&mut self, changes: &[Change], cancelled: Option<&dyn Fn() -> bool>) -> Result<Arc<Evaluated>, String> {
        let tried = self.model.preview(&mut self.eval, changes, cancelled).map_err(|e| self.apply_error(e).text);
        // The evaluator goes back to the committed plate: what it changes next is from there.
        let back = self.model.evaluate(&mut self.eval, cancelled).map_err(|f| f.to_string());
        let (_, _, ev) = tried?;
        back?;
        Ok(Arc::new(ev))
    }

    /// The plate's plan, kept while nothing it depends on changes: the committed plate's from its
    /// live slice, a try's (`tried`) planned on its own.
    pub(crate) fn support_plan(&mut self, ev: &Evaluated, tried: bool, cancelled: Option<&dyn Fn() -> bool>) -> Result<Arc<Planned>, String> {
        let source = self.slices.clone().ok_or("no slicer here: the support tools plan with the host's slices")?;
        let (key, input) = self.plan_key(&*source, ev, tried)?;
        if let Some(hit) = self.kept_plan(key) {
            return Ok(hit);
        }
        let plan = Arc::new(plan_of(ev, input, &*source, tried, cancelled)?);
        self.keep_plan(key, plan.clone());
        Ok(plan)
    }

    /// Each try's figures part by part, in `evs`' order: from a kept plan, else its paths planned
    /// at once as tasks of the slicer's maps. Nothing is kept: a kept plan is told, and an untold
    /// plan under a try's key would give a later `support_plan(ev, true)` no breaks and no hold.
    pub(crate) fn tried_figures(&self, evs: &[Arc<Evaluated>]) -> Result<Vec<Vec<(f64, f64)>>, String> {
        let source = self.slices.clone().ok_or("no slicer here: the support tools plan with the host's slices")?;
        // each try's key and figures from its kept plan; the first try of each key not kept, with its input
        let mut keys: Vec<(u64, Option<Vec<(f64, f64)>>)> = Vec::with_capacity(evs.len());
        let mut wanted: Vec<(u64, usize, Mutex<Option<LiveInput>>)> = Vec::new();
        for (i, ev) in evs.iter().enumerate() {
            let (key, input) = self.plan_key(&*source, ev, true)?;
            let kept = self.support_memo.plans.iter().find(|(k, _)| *k == key).map(|(_, plan)| plan.parts.iter().map(|p| (p.volume_mm3, p.seconds)).collect());
            if kept.is_none() && !wanted.iter().any(|(k, _, _)| *k == key) {
                wanted.push((key, i, Mutex::new(Some(input.0))));
            }
            keys.push((key, kept));
        }
        let made = par::map(wanted.len(), |w| {
            let (_, i, input) = &wanted[w];
            let input = input.lock().unwrap().take().expect("each try is planned once");
            figures_of(&evs[*i], &input)
        });
        let made: Vec<(u64, Vec<(f64, f64)>)> = wanted.iter().zip(made).map(|((key, _, _), figures)| figures.map(|figures| (*key, figures))).collect::<Result<_, _>>()?;
        let mut figures = Vec::with_capacity(evs.len());
        for (key, kept) in keys {
            let parts = kept.or_else(|| made.iter().find(|(k, _)| *k == key).map(|(_, figures)| figures.clone())).ok_or("a try was not planned")?;
            figures.push(parts);
        }
        Ok(figures)
    }

    /// What a plate's plan is kept by, and the slice input it's planned from.
    fn plan_key(&self, source: &dyn SliceSource, ev: &Evaluated, tried: bool) -> Result<(u64, (LiveInput, String)), String> {
        let mut h = DefaultHasher::new();
        tried.hash(&mut h);
        for i in 0..ev.plate.parts.len() {
            hash_part(ev, i).hash(&mut h);
            ev.plate.parts[i].print.hash(&mut h);
            ev.frames[i].offset.map(f64::to_bits).hash(&mut h);
            // its support block and enforce regions
            ev.plate.parts[i].paint.as_ref().map(|p| p.key).hash(&mut h);
        }
        ev.plate.settings.hash(&mut h);
        // the settings as the slice resolves them, and a test plate's cubes and places
        let input = self.slice_input(source, ev)?;
        input.0.settings_id.hash(&mut h);
        if input.0.pieces.is_some() {
            input.0.plate_id.hash(&mut h);
        }
        Ok((h.finish(), input))
    }

    /// The plan kept by `key`, now the last used.
    fn kept_plan(&mut self, key: u64) -> Option<Arc<Planned>> {
        let plans = &mut self.support_memo.plans;
        let i = plans.iter().position(|(k, _)| *k == key)?;
        let hit = plans.remove(i);
        plans.insert(0, hit.clone());
        Some(hit.1)
    }

    fn keep_plan(&mut self, key: u64, plan: Arc<Planned>) {
        let plans = &mut self.support_memo.plans;
        plans.insert(0, (key, plan));
        plans.truncate(MEMO_PLANS);
    }

    /// Each part's surface in plate mm (a field part's is meshed once).
    pub(crate) fn support_surfaces(&mut self, ev: &Evaluated) -> Result<Vec<Arc<Surface>>, String> {
        let mut out = Vec::with_capacity(ev.plate.parts.len());
        for (i, part) in ev.plate.parts.iter().enumerate() {
            if ev.fields.get(i).is_some_and(Option::is_some) && part.mesh.triangles.is_empty() {
                let key = hash_part(ev, i);
                if let Some(surface) = self.support_memo.surfaces.get(&key) {
                    out.push(surface.clone());
                    continue;
                }
                let surface = Arc::new(field_surface(ev, i)?);
                if self.support_memo.surfaces.len() >= MEMO_PARTS {
                    self.support_memo.surfaces.clear();
                }
                self.support_memo.surfaces.insert(key, surface.clone());
                out.push(surface);
            } else {
                let nz = facet_nz(&part.mesh);
                out.push(Arc::new(Surface { mesh: part.mesh.clone(), nz, floor: f64::MIN }));
            }
        }
        Ok(out)
    }

    /// Each part as overrides are written for it: its frame, script and the room a support takes.
    fn support_placed(&self, ev: &Evaluated, plan: &Planned) -> Vec<Placed> {
        let reach = reach_of(plan);
        ev.plate.parts.iter().enumerate().map(|(i, part)| {
            let materials = plan.materials.of_part(part);
            let layer = plan.parts[i].skeleton.heights.get(1).map_or(0.2, |h| h.1);
            let below = materials.support_gap + 3.0 * layer + reach;
            let script = self.compiled.as_ref().and_then(|c| c.object(&part.object)).map(|(instance, _)| instance.script.file.clone());
            // the gap and two layers of support: less room than that holds no support
            let least_clear = materials.support_gap + 2.0 * layer;
            Placed::new(&ev.model_to_plate[i], script, below, least_clear)
        }).collect()
    }

    /// Each part's surface as a piece's block is measured on: its overhang facets past its plan's
    /// limit.
    fn support_sides(&self, ev: &Evaluated, plan: &Planned, surfaces: &[Arc<Surface>]) -> Vec<Side> {
        self.support_placed(ev, plan).into_iter().zip(surfaces).enumerate().map(|(i, (placed, surface))| {
            let degrees = Arc::new(degrees_of(surface, plan.materials.of_part(&ev.plate.parts[i]).overhang_angle_deg));
            Side { placed, surface: surface.clone(), degrees }
        }).collect()
    }

    /// The plate's overhangs and how its plan meets them, past `angle` from the bed when given.
    fn overhangs_of(&mut self, ev: &Evaluated, tried: bool, angle: Option<f64>, cancelled: Option<&dyn Fn() -> bool>) -> Result<(Arc<Planned>, Vec<Found>), String> {
        let plan = self.support_plan(ev, tried, cancelled)?;
        let placed = self.support_placed(ev, &plan);
        let found = found_overhangs(ev, &plan, &placed, angle);
        Ok((plan, found))
    }

    /// `fab.overhangs`.
    pub fn overhangs_tool(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
        match self.overhangs_value(args, cancelled) {
            Ok(v) => Reply::ok(v),
            Err(e) => Reply::err(e),
        }
    }

    /// The committed plate's overhangs in at most `limit` lines, ranked as `fab.overhangs` ranks
    /// its rows: what the Slice panel lists. Its plan is the plate's live slice.
    pub fn overhang_list(&mut self, limit: usize, cancelled: Option<&dyn Fn() -> bool>) -> Result<OverhangList, String> {
        let ev = self.slicer_plate()?;
        let (plan, found) = self.overhangs_of(&ev, false, None, cancelled)?;
        // What the supports are asked to hold, ranked; then what the parts' own settings keep the
        // trees out from under, a line per part and reason.
        let asked: Vec<&Found> = found.iter().filter(|f| f.area() >= ASKED_LEAST).collect();
        let planned = found.iter().any(|f| f.left_mm2.is_some());
        let mut lines: Vec<OverhangLine> = rows_of(&asked, limit.max(1)).iter().map(|r| line_of(r, &asked, &ev)).collect();
        lines.extend(chosen_lines(&found, &plan, &ev));
        Ok(OverhangList {
            patches: found.len(),
            area_mm2: asked.iter().map(|f| f.area()).sum(),
            left_mm2: planned.then(|| asked.iter().map(|f| f.left_mm2.unwrap_or(0.0)).sum()),
            lines,
        })
    }

    fn overhangs_value(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Result<S, String> {
        let (ev, tried) = self.support_plate(args, cancelled)?;
        let ev = if tried { ev } else { self.slicer_plate()? };
        let angle = angle_arg(args)?;
        let (plan, found) = self.overhangs_of(&ev, tried, angle, cancelled)?;
        let limit = args.get("limit").and_then(number).map_or(ROWS, |l| (l as usize).clamp(1, MOST_ROWS));
        let within = args.get("box").map(box_arg).transpose()?;
        let only = str_arg(args, "object");
        let all: Vec<&Found> = found.iter().filter(|f| {
            only.is_none_or(|o| ev.plate.parts[f.part].object == o || ev.plate.parts[f.part].name == o)
                && within.as_ref().is_none_or(|b| in_box(f.patch.centroid, b))
        }).collect();
        // The rows are what the supports are asked to hold: what the parts' own settings keep the
        // trees out from under is told apart.
        let scope: Vec<&Found> = all.iter().copied().filter(|f| f.area() >= ASKED_LEAST).collect();
        let area: f64 = scope.iter().map(|f| f.area()).sum();
        let planned = scope.iter().any(|f| f.left_mm2.is_some());
        let left: f64 = scope.iter().map(|f| f.left_mm2.unwrap_or(0.0)).sum();
        let rows = rows_of(&scope, limit);
        let mut fields = vec![
            ("patches", int(scope.len())), ("area_mm2", n(area)),
            ("left_mm2", opt(planned.then(|| n(left)))),
            ("limit_deg", n(angle.unwrap_or(90.0 - plan.materials.overhang_angle_deg))),
            ("rows", arr(rows.iter().map(|r| row_value(r, &scope, &ev)))),
        ];
        // As the parts' plans tell it, for the parts and box asked about.
        let kept = [0, 1].map(|k| plan.parts.iter().enumerate().filter(|(i, _)| only.is_none_or(|o| ev.plate.parts[*i].object == o || ev.plate.parts[*i].name == o)).map(|(_, pp)| pp.kept_area(k, |at| within.as_ref().is_none_or(|b| in_box(at, b)))).sum::<f64>());
        if kept.iter().any(|&k| k > 0.0) {
            for (k, key) in kept.into_iter().zip(["kept_out_mm2", "no_trees_mm2"]) {
                if k > 0.0 {
                    fields.push((key, n(k)));
                }
            }
            fields.push(("note_kept", s("kept_out_mm2 lies inside a part's footprint, which the part keeps its trees out of by its setting, and no_trees_mm2 over a no-trees area (m.no_trees): no tree holds them, by the person's choice. Neither is asked of the supports, nor in area_mm2 or left_mm2. Both count the overhang layer by layer, as Printability tells it; area_mm2 counts the faces.")));
        }
        if scope.len() > rows.len() {
            fields.push(("note", s("A row with `patches` is a cluster: ask again with its box for what's in it.")));
        }
        if !planned {
            fields.push(("note_supports", s("This plate plans no supports: nothing covers these.")));
        }
        if tried {
            fields.insert(0, ("tried", flag(true)));
        }
        Ok(obj(fields))
    }

    /// `fab.overhang_view`: the picture goes through the app's review renderer.
    pub fn overhang_view_tool(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
        match self.overhang_view(args, cancelled) {
            Ok(look) => Reply { images: Vec::new(), ok: true, text: look.answer.clone(), update: None, render: Some(RenderRequest::Look(Arc::new(look))) },
            Err(e) => Reply::err(e),
        }
    }

    pub(crate) fn overhang_view(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Result<LookRequest, String> {
        let (ev, tried) = self.support_plate(args, cancelled)?;
        let plan = self.support_plan(&ev, tried, cancelled)?;
        let placed = self.support_placed(&ev, &plan);
        let found = found_overhangs(&ev, &plan, &placed, angle_arg(args)?);
        let want = str_arg(args, "patch");
        let aimed = match want {
            Some(id) => Some(found.iter().find(|f| f.name() == id).ok_or_else(|| format!("no patch '{id}' on this plate (it has p1 to p{})", found.len()))?),
            None => None,
        };
        let (lo, hi) = match (args.get("box"), aimed) {
            (Some(b), _) => box_arg(b)?,
            (None, Some(f)) => {
                let (a, b) = (f.patch.min, f.patch.max);
                let room = ((0..3).map(|k| b[k] - a[k]).fold(0.0, f64::max) * FRAME_SHARE).max(FRAME_LEAST);
                (a.map(|v| v - room), b.map(|v| v + room))
            }
            (None, None) => return Err("give 'patch' (an id from fab.overhangs) or 'box' [[x y z] [x y z]]".into()),
        };
        let views: Vec<String> = match args.get("views") {
            Some(Value::Arr(a)) => a.iter().filter_map(Value::as_str).map(|v| if v == "below" { BELOW.to_string() } else { v.to_string() }).collect(),
            Some(Value::Str(v)) => vec![if v == "below" { BELOW.to_string() } else { v.clone() }],
            _ => vec![BELOW.to_string()],
        };
        if views.is_empty() || views.len() > 4 {
            return Err("'views' takes one to four views".into());
        }
        for v in &views {
            let custom = v.split_once(',').is_some_and(|(a, b)| a.trim().parse::<f32>().is_ok() && b.trim().parse::<f32>().is_ok());
            if !custom && !crate::VIEW_NAMES.contains(&v.as_str()) {
                return Err(format!("unknown view '{v}': below, {} or \"yaw,pitch\" in degrees", crate::VIEW_NAMES.join(", ")));
            }
        }
        // Looking up, what stands below the patch would hide it: the part is cut away under it.
        let from_below = views.iter().any(|v| v.split_once(',').map_or(matches!(v.as_str(), "bottom" | "underside"), |(_, p)| p.trim().parse::<f32>().is_ok_and(|p| p > 15.0)));
        let cut = aimed.map_or(lo[2], |f| f.patch.lowest[2]) - 0.05;
        let frame = (lo, hi);
        let shown = |c: &[[f64; 3]; 3]| {
            let middle: [f64; 3] = std::array::from_fn(|k| (c[0][k] + c[1][k] + c[2][k]) / 3.0);
            in_box(middle, &frame) && !(from_below && middle[2] < cut)
        };
        let mut mesh = ColoredMesh::default();
        let mut drawn = 0usize;
        // The parts as their slices cut them: each layer's outline a band from its bottom to its top.
        for part in &plan.parts {
            let at = part.at;
            for (top, h, outline) in &part.layers.layers {
                let (top, bottom) = (*top, top - h);
                if top < lo[2] || bottom > hi[2] {
                    continue;
                }
                for ring in outline.iter().flat_map(ExPolygon::rings) {
                    let n = ring.pts.len();
                    for k in 0..n {
                        let (a, b) = (ring.pts[k], ring.pts[(k + 1) % n]);
                        let (ax, ay, bx, by) = (a.x_mm() + at[0], a.y_mm() + at[1], b.x_mm() + at[0], b.y_mm() + at[1]);
                        // facing out of the part: a contour runs round it counter-clockwise, a hole clockwise
                        for c in [[[ax, ay, bottom], [bx, by, bottom], [bx, by, top]], [[ax, ay, bottom], [bx, by, top], [ax, ay, top]]] {
                            if shown(&c) {
                                mesh.triangle(c, PART_GREY);
                                drawn += 1;
                            }
                        }
                    }
                }
            }
        }
        // Their overhangs, each face coloured by how far past the limit it leans, facing the bed.
        for f in &found {
            let at = plan.parts[f.part].at;
            for face in &f.patch.faces {
                let o = OVERHANG_COLORS[face.degree as usize];
                let ring = |r: &makepad_fab_slicer::geom::Polygon| r.pts.iter().map(|p| [p.x_mm() + at[0], p.y_mm() + at[1]]).collect::<Vec<_>>();
                let region = Region { outer: ring(&face.region.contour), holes: face.region.holes.iter().map(ring).collect() };
                let (points, triangles) = region.triangulate();
                for t in triangles {
                    let c = [t[0], t[2], t[1]].map(|v| [points[v as usize][0], points[v as usize][1], face.z]);
                    if shown(&c) {
                        mesh.triangle(c, [o[0], o[1], o[2]]);
                        drawn += 1;
                    }
                }
            }
        }
        if drawn == 0 {
            return Err("nothing of the part lies in that box".into());
        }
        let mut branches = 0usize;
        if args.get("supports").and_then(Value::as_bool) != Some(false) {
            for part in &plan.parts {
                let drawn = skeleton_mesh(&part.skeleton, part.at, &|p| in_box(p, &frame));
                branches += drawn.0.positions.len() + drawn.1.positions.len();
                mesh.add(&drawn.0, BRANCH);
                mesh.add(&drawn.1, PAD);
            }
        }
        // A label goes on its patch: a ring's middle is in its hole.
        let mark = |f: &Found| f.patch.mark;
        let mut marks: Vec<([f64; 3], String)> = aimed.map(|f| (mark(f), f.name())).into_iter().collect();
        let near: Vec<([f64; 3], String)> = found.iter().filter(|f| want != Some(f.name().as_str())).map(|f| (mark(f), f.name())).filter(|(p, _)| in_box(*p, &frame) && (!from_below || p[2] >= cut)).take(MARKS).collect();
        marks.extend(near);
        let mut fields = vec![];
        if tried {
            fields.push(("tried", flag(true)));
        }
        if let Some(f) = aimed {
            let one = [f];
            fields.push(("patch", row_value(&Row { of: vec![0] }, &one, &ev)));
        }
        fields.push(("frame", arr([mm3(lo), mm3(hi)])));
        if from_below {
            fields.push(("part_cut_below_z", mm(cut)));
        }
        fields.push(("branches_shown", flag(branches > 0)));
        fields.push(("views", arr(views.iter().map(s))));
        fields.push(("marked", arr(marks.iter().map(|(p, id)| obj(vec![("id", name(id)), ("at", mm3(*p))])))));
        fields.push(("reading", s("The parts as their slices cut them, each layer's outline a grey band; overhang faces by degree: green-yellow 1 to dark red 5; branches cyan, pads blue. Labels are patch ids, the white bar's length is written beside it, at the frame's middle. Plate +Z is up; yaw turns about it, pitch above 0 looks from below.")));
        let answer = crate::out::text(&obj(fields));
        Ok(LookRequest { mesh, frame, views, answer, marks })
    }

    /// `fab.supports`.
    pub fn supports_tool(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
        let run = self.supports_value(args, cancelled);
        match run {
            Ok(asked) => {
                if args.get("verdicts").and_then(Value::as_bool) == Some(true) {
                    let limit = args.get("limit").and_then(number).map_or(ROWS, |l| (l as usize).clamp(1, MOST_ROWS));
                    // A piece's block is measured on the part's surface: asked for, it is meshed.
                    let sides = match self.support_surfaces(&asked.ev) {
                        Ok(surfaces) => self.support_sides(&asked.ev, &asked.plan, &surfaces),
                        Err(e) => return Reply::err(e),
                    };
                    let memo = &self.support_memo;
                    let job = VerdictJob { ev: asked.ev, plan: asked.plan, grids: memo.grids.clone(), verdicts: memo.verdicts.clone(), limit, plan_answer: asked.answer, sides, was: asked.was };
                    return Reply { images: Vec::new(), ok: true, text: String::new(), update: None, render: Some(RenderRequest::Verdicts(Arc::new(job))) };
                }
                Reply::ok(asked.answer)
            }
            Err(e) => Reply::err(e),
        }
    }

    fn supports_value(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Result<Asked, String> {
        let (ev, tried) = self.support_plate(args, cancelled)?;
        let t = std::time::Instant::now();
        let plan = self.support_plan(&ev, tried, cancelled)?;
        let planned_now = t.elapsed().as_secs_f64() * 1e3;
        let found = found_overhangs(&ev, &plan, &self.support_placed(&ev, &plan), None);
        // A try says what it changed: the committed plate's figures beside its own.
        let was = match tried {
            true => {
                let last = self.last.clone().ok_or("nothing is built yet")?;
                let plan = self.support_plan(&last, false, cancelled)?;
                let found = found_overhangs(&last, &plan, &self.support_placed(&last, &plan), None);
                let figures: Vec<(String, Figures)> = (0..last.plate.parts.len()).map(|i| (last.plate.parts[i].name.clone(), Figures::of(&plan, &found, i))).collect();
                Some((last, plan, figures))
            }
            false => None,
        };
        let mut parts = Vec::new();
        for (i, part) in ev.plate.parts.iter().enumerate() {
            let p = &plan.parts[i];
            let sk = &p.skeleton;
            let of_part = || found.iter().filter(|f| f.part == i);
            let now = Figures::of(&plan, &found, i);
            let left_patches = of_part().filter(|f| f.area() >= ASKED_LEAST && f.left_mm2.unwrap_or(f.area()) > 0.25 * f.area()).count();
            let density = plan.materials.filament(part.filament).density;
            // A tree's pads stand on the bed with no branch where a face is that low.
            let kind = if !sk.chains.is_empty() || sk.roofs.iter().any(|r| !r.is_empty()) { "tree" } else if !sk.slabs.is_empty() { "normal" } else { "none" };
            // The branches' tops, the pads a trunk spreads into and the roofs.
            let pads = sk.roofs.iter().enumerate().filter(|(_, r)| !r.is_empty()).filter_map(|(l, _)| sk.heights.get(l).map(|h| h.0));
            let tallest = sk.chains.iter().filter_map(|c| sk.heights.get(c.top).map(|h| h.0)).chain(pads).chain(sk.slabs.iter().map(|sl| sl.z)).fold(0.0, f64::max);
            let mut row = vec![
                ("object", s(&part.object)), ("type", name(kind)), ("pieces", int(now.pieces)), ("branches", int(sk.chains.len())),
                ("volume_mm3", n(p.volume_mm3)), ("material_g", opt((density > 0.0).then(|| n(p.volume_mm3 / 1000.0 * density)))), ("extruding_s", n(p.seconds)), ("tallest", mm(tallest)),
                ("overhang_mm2", n(now.overhang)), ("left_mm2", n(now.left)), ("left_patches", int(left_patches)), ("patches", int(of_part().count())),
            ];
            // What its own settings keep the trees out from under, as its plan tells it: not in its
            // overhang or left.
            for (k, key) in ["kept_out_mm2", "no_trees_mm2"].into_iter().enumerate() {
                let kept = p.kept_area(k, |_| true);
                if kept > 0.0 {
                    row.push((key, n(kept)));
                }
            }
            // A part is the one at its place on the plate when the try keeps the plate's parts (an
            // added statement renumbers its solids), else the one of its name.
            let before = was.as_ref().and_then(|(_, _, f)| if f.len() == ev.plate.parts.len() { f.get(i) } else { f.iter().find(|(o, _)| *o == part.name) });
            if let Some((_, before)) = before {
                if before.differs(&now) {
                    row.push(("was", obj(vec![("pieces", int(before.pieces)), ("volume_mm3", n(before.volume)), ("overhang_mm2", n(before.overhang)), ("left_mm2", n(before.left))])));
                }
            }
            parts.push(obj(row));
        }
        let mut fields = vec![];
        if tried {
            fields.push(("tried", flag(true)));
        }
        fields.push(("plan_ms", n(planned_now.round())));
        fields.push(("setup", s(&plan.setup)));
        fields.push(("parts", arr(parts)));
        Ok(Asked { answer: obj(fields), ev, plan, was: was.map(|(ev, plan, _)| (ev, plan)) })
    }

    /// `fab.section` with the supports: on the plate, the parts' surfaces and the plan both cut.
    pub(crate) fn support_section(&mut self, args: &Value) -> Result<(S, Vec<(String, Vec<u8>)>), String> {
        let axis = match str_arg(args, "axis") {
            None | Some("z") => 2,
            Some("x") => 0,
            Some("y") => 1,
            Some(other) => return Err(format!("no axis '{other}': x, y or z, on the plate")),
        };
        let at = args.get("at").and_then(number).ok_or("'at' is missing: where the plane cuts, in plate mm along the axis")?;
        let ev = self.last.clone().ok_or("nothing is built yet")?;
        let plan = self.support_plan(&ev, false, None)?;
        let surfaces = self.support_surfaces(&ev)?;
        let only = str_arg(args, "object");
        let (mut rows, mut regions) = (Vec::new(), Vec::new());
        let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
        let mut grow = |a: [f64; 2], b: [f64; 2]| {
            for k in 0..2 {
                lo[k] = lo[k].min(a[k]);
                hi[k] = hi[k].max(b[k]);
            }
        };
        for (part, surface) in ev.plate.parts.iter().zip(&surfaces) {
            if only.is_some_and(|o| o != part.object && o != part.name) {
                continue;
            }
            let mesh = TriMesh { vertices: surface.mesh.positions.iter().map(|p| dvec3(p[0], p[1], p[2])).collect(), triangles: surface.mesh.triangles.clone(), tags: Vec::new(), corner_normals: Vec::new() };
            let cut = measure::section(&mesh, axis, at);
            if cut.is_empty() {
                continue;
            }
            for g in &cut {
                let (a, b) = g.bounds();
                grow(a, b);
            }
            rows.push(obj(vec![("object", s(&part.object)), ("area_mm2", n(cut.iter().map(|g| g.area()).sum())), ("pieces", int(cut.len()))]));
            regions.extend(cut);
        }
        let cuts: Vec<SupportCut> = plan.parts.iter().map(SupportCut::of).collect();
        let mut crossing = 0;
        for c in &cuts {
            crossing += c.crossing(axis, at, &mut grow);
        }
        if rows.is_empty() && crossing == 0 {
            return Err(format!("the plane {}={at} cuts nothing on the plate", ["x", "y", "z"][axis]));
        }
        let margin = ((hi[0] - lo[0]).max(hi[1] - lo[1]) * 0.05).max(1.0);
        let (lo, hi) = ([lo[0] - margin, lo[1] - margin], [hi[0] + margin, hi[1] + margin]);
        let per_pixel = ((hi[0] - lo[0]).max(hi[1] - lo[1]) / SECTION_SIDE).max(1e-3);
        let (w, h) = (((hi[0] - lo[0]) / per_pixel).ceil() as usize, ((hi[1] - lo[1]) / per_pixel).ceil() as usize);
        let edges: Vec<([f64; 2], [f64; 2])> = regions.iter().flat_map(|r| r.loops().flat_map(|l| (0..l.len()).map(move |i| (l[i], l[(i + 1) % l.len()])))).collect();
        let mut rgba = vec![255u8; w * h * 4];
        let (u, v) = [(1, 2), (0, 2), (0, 1)][axis];
        for y in 0..h {
            let row_v = hi[1] - (y as f64 + 0.5) * per_pixel;
            let mut xs: Vec<f64> = edges.iter().filter(|(a, b)| (a[1] > row_v) != (b[1] > row_v)).map(|(a, b)| a[0] + (row_v - a[1]) / (b[1] - a[1]) * (b[0] - a[0])).collect();
            xs.sort_by(f64::total_cmp);
            for x in 0..w {
                let row_u = lo[0] + (x as f64 + 0.5) * per_pixel;
                let mut p = [at; 3];
                p[u] = row_u;
                p[v] = row_v;
                let color: [u8; 3] = if cuts.iter().any(|c| c.holds(p)) {
                    [20, 110, 230]
                } else if xs.partition_point(|&e| e < row_u) % 2 == 1 {
                    [60, 60, 60]
                } else if (row_u / 10.0 - (row_u / 10.0).round()).abs() * 10.0 < per_pixel * 0.5 || (row_v / 10.0 - (row_v / 10.0).round()).abs() * 10.0 < per_pixel * 0.5 {
                    [215, 215, 215]
                } else {
                    [245, 245, 245]
                };
                rgba[(y * w + x) * 4..(y * w + x) * 4 + 3].copy_from_slice(&color);
            }
        }
        let image = makepad_model::RgbaImage { width: w as u32, height: h as u32, pixels: rgba };
        let png = image.to_png(&makepad_model::Limits::default()).map_err(|e| format!("{e:?}"))?;
        let names = ["x", "y", "z"];
        let label = format!("Section at plate {}={at} mm: part dark, supports blue, 10 mm grid; image x = plate {} from {:.1}, image y = plate {} from {:.1} downward, {:.3} mm per pixel", names[axis], names[u], lo[0], names[v], hi[1], per_pixel);
        let answer = obj(vec![("plane", s(format!("plate {}={at}mm", names[axis]))), ("sections", arr(rows)), ("supports_crossing", int(crossing))]);
        Ok((answer, vec![(label, png)]))
    }
}

fn degrees_of(surface: &Surface, limit: f64) -> Vec<u8> {
    surface.mesh.triangles.iter().zip(&surface.nz).map(|(tri, &nz)| {
        let corners = tri.map(|v| surface.mesh.positions[v as usize]);
        if (nz as f64) > SURELY_UP || corners.iter().all(|c| c[2] <= surface.floor) {
            return 0;
        }
        overhang_degree(nz as f64, corners, limit)
    }).collect()
}

/// The plan's branches (and normal supports' columns), and its pads, as solids in plate mm, where
/// `keep` says.
pub fn skeleton_mesh(sk: &Skeleton, at: [f64; 2], keep: &dyn Fn([f64; 3]) -> bool) -> (PieceMesh, PieceMesh) {
    let mut branches = PieceMesh::default();
    let mut pads = PieceMesh::default();
    for chain in &sk.chains {
        let mut run: Vec<([f32; 3], f32)> = Vec::new();
        let mut since = f64::MAX;
        let mut last = [0.0; 3];
        for (k, &(x, y, r)) in chain.sections.iter().enumerate() {
            let Some(h) = chain.top.checked_sub(k).and_then(|l| sk.heights.get(l)) else { break };
            let p = [x + at[0], y + at[1], h.0 - h.1 / 2.0];
            if !keep(p) {
                if !run.is_empty() {
                    branches.tube(&run);
                    run.clear();
                }
                since = f64::MAX;
                continue;
            }
            since = if since == f64::MAX { f64::MAX } else { since + ((p[0] - last[0]).powi(2) + (p[1] - last[1]).powi(2) + (p[2] - last[2]).powi(2)).sqrt() };
            last = p;
            if since >= (0.5 * r).max(0.2) || k + 1 == chain.sections.len() {
                run.push((p.map(|v| v as f32), r as f32));
                since = 0.0;
            }
        }
        if !run.is_empty() {
            branches.tube(&run);
        }
    }
    for (l, islands) in sk.roofs.iter().enumerate() {
        let Some(h) = sk.heights.get(l) else { continue };
        for e in islands {
            let c = e.bbox().center();
            if keep([c.x_mm() + at[0], c.y_mm() + at[1], h.0]) {
                pads.extrude(e, at, h.0 - h.1, h.0);
            }
        }
    }
    for slab in &sk.slabs {
        for e in union_ex(&slab.region) {
            let c = e.bbox().center();
            if keep([c.x_mm() + at[0], c.y_mm() + at[1], slab.z]) {
                branches.extrude(&e, at, slab.z - slab.height, slab.z);
            }
        }
    }
    (branches, pads)
}

/// The plan's supports crossing a plane of the model frame, for `fab.section`: what lies in each
/// layer on the plate.
pub struct SupportCut<'a> {
    pub plan: &'a PartPlan,
    circles: Vec<Vec<(f64, f64, f64)>>,
    slabs: Vec<(f64, f64, Vec<ExPolygon>)>,
}

impl<'a> SupportCut<'a> {
    pub fn of(plan: &'a PartPlan) -> SupportCut<'a> {
        let sk = &plan.skeleton;
        let mut circles = vec![Vec::new(); sk.heights.len()];
        for chain in &sk.chains {
            for (k, &c) in chain.sections.iter().enumerate() {
                if let Some(l) = chain.top.checked_sub(k).filter(|&l| l < circles.len()) {
                    circles[l].push(c);
                }
            }
        }
        let slabs = sk.slabs.iter().map(|sl| (sl.z - sl.height, sl.z, union_ex(&sl.region))).collect();
        SupportCut { plan, circles, slabs }
    }

    /// Whether a plate point is in a branch, a pad or a footprint.
    pub fn holds(&self, p: [f64; 3]) -> bool {
        let sk = &self.plan.skeleton;
        let (x, y) = (p[0] - self.plan.at[0], p[1] - self.plan.at[1]);
        let at = makepad_fab_slicer::geom::Pt::from_mm(x, y);
        let l = sk.heights.partition_point(|h| h.0 < p[2]);
        if let Some(h) = sk.heights.get(l).filter(|h| p[2] > h.0 - h.1) {
            let _ = h;
            if self.circles[l].iter().any(|c| (x - c.0).hypot(y - c.1) <= c.2) {
                return true;
            }
            if sk.roofs.get(l).is_some_and(|pads| pads.iter().any(|e| e.contains(at))) {
                return true;
            }
        }
        self.slabs.iter().any(|(z0, z1, islands)| p[2] > *z0 && p[2] <= *z1 && islands.iter().any(|e| e.contains(at)))
    }

    /// What the plane cuts: tells `grow` its box in the plane's two axes and says how many sections
    /// and footprints it crosses.
    fn crossing(&self, axis: usize, at: f64, grow: &mut dyn FnMut([f64; 2], [f64; 2])) -> usize {
        let sk = &self.plan.skeleton;
        let [ox, oy] = self.plan.at;
        let mut count = 0;
        for (l, list) in self.circles.iter().enumerate() {
            let (z, h) = sk.heights[l];
            for &(x, y, r) in list {
                let (x, y) = (x + ox, y + oy);
                let (crosses, a, b) = match axis {
                    0 => ((x - at).abs() <= r, [y - r, z - h], [y + r, z]),
                    1 => ((y - at).abs() <= r, [x - r, z - h], [x + r, z]),
                    _ => (at > z - h && at <= z, [x - r, y - r], [x + r, y + r]),
                };
                if crosses {
                    grow(a, b);
                    count += 1;
                }
            }
        }
        let mut region = |z0: f64, z1: f64, e: &ExPolygon| {
            let b = e.bbox();
            let (x0, y0, x1, y1) = (b.min.x_mm() + ox, b.min.y_mm() + oy, b.max.x_mm() + ox, b.max.y_mm() + oy);
            let (crosses, lo, hi) = match axis {
                0 => (at >= x0 && at <= x1, [y0, z0], [y1, z1]),
                1 => (at >= y0 && at <= y1, [x0, z0], [x1, z1]),
                _ => (at > z0 && at <= z1, [x0, y0], [x1, y1]),
            };
            if crosses {
                grow(lo, hi);
                count += 1;
            }
        };
        for (z0, z1, islands) in &self.slabs {
            for e in islands {
                region(*z0, *z1, e);
            }
        }
        for (l, pads) in sk.roofs.iter().enumerate() {
            let Some(&(z, h)) = sk.heights.get(l) else { continue };
            for e in pads {
                region(z - h, z, e);
            }
        }
        count
    }
}

impl Session {
}

/// If asked, the plan's supports, as what `fab.render` adds to its picture (the parts it draws
/// itself, field parts meshed, each in its paint).
pub(crate) fn render_extra(session: &mut Session, ev: &Evaluated, objects: &[String], supports: bool, volumes: &[String], spots: &[String]) -> Result<Option<Arc<ColoredMesh>>, String> {
    let mut mesh = ColoredMesh::default();
    session.tint_volumes(ev, volumes, objects, &mut mesh)?;
    session.tint_spots(ev, spots, objects, &mut mesh)?;
    let wanted = |o: &str| objects.is_empty() || objects.iter().any(|w| w == o);
    if supports {
        let plan = session.support_plan(ev, false, None)?;
        for (i, p) in plan.parts.iter().enumerate() {
            if !wanted(&ev.plate.parts[i].object) {
                continue;
            }
            let drawn = skeleton_mesh(&p.skeleton, p.at, &|_| true);
            mesh.add(&drawn.0, BRANCH);
            mesh.add(&drawn.1, PAD);
        }
    }
    Ok((!mesh.indices.is_empty()).then(|| Arc::new(mesh)))
}

/// The pieces of a plan (and of a try's committed plate), ready to be checked one by one off the
/// model worker.
pub struct Checking {
    entries: Vec<Entry>,
    memo: VerdictMemo,
}

struct Entry {
    solid: Arc<Solid>,
    grid: Arc<DistGrid>,
    /// Its part on the plate asked about (or on the committed one, for a piece only it has).
    part: usize,
    piece: Piece,
    key: u64,
    /// It is on the plate asked about.
    now: bool,
    /// Its name on a try's committed plate, where it is one of its pieces.
    was: Option<String>,
}

impl Checking {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// One piece's verdict; None: cancelled. A piece checked before answers from the memo.
    pub fn check(&self, i: usize, cancelled: &dyn Fn() -> bool) -> Option<PieceVerdict> {
        let e = &self.entries[i];
        if let Some(v) = self.memo.lock().unwrap().get(&e.key) {
            return Some(*v);
        }
        let v = check_piece(&e.piece, &e.grid, &e.solid, cancelled).ok()?;
        let mut memo = self.memo.lock().unwrap();
        if memo.len() >= MEMO_VERDICTS {
            memo.clear();
        }
        memo.insert(e.key, v);
        Some(v)
    }
}

/// Whether each piece of a plan can leave the part: a search that takes a while, so the host runs
/// it at low priority beside the model worker and the call's answer waits for it.
pub struct VerdictJob {
    pub ev: Arc<Evaluated>,
    pub plan: Arc<Planned>,
    grids: GridMemo,
    verdicts: VerdictMemo,
    limit: usize,
    plan_answer: S,
    sides: Vec<Side>,
    /// A try's committed plate: its pieces are checked too, to say what the try changed.
    was: Option<(Arc<Evaluated>, Arc<Planned>)>,
}

impl std::fmt::Debug for VerdictJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VerdictJob")
    }
}

impl PartialEq for VerdictJob {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl VerdictJob {
    /// The whole search here and now, for what has no pool to run it on.
    pub fn run(&self) -> Result<S, String> {
        let never = || false;
        let t = std::time::Instant::now();
        let checking = self.prepare(&never)?;
        let results: Vec<Option<PieceVerdict>> = (0..checking.len()).map(|i| checking.check(i, &never)).collect();
        Ok(self.answer(&checking, &results, t.elapsed().as_secs_f64() * 1e3))
    }

    /// Each part's distance and its pieces: the plate's, then those only a try's committed plate has.
    pub fn prepare(&self, stop: &(dyn Fn() -> bool + Sync)) -> Result<Checking, String> {
        let ctl = par::Ctl { cancel: Some(stop), ..Default::default() };
        let mut entries: Vec<Entry> = Vec::new();
        let mut index: HashMap<u64, usize> = HashMap::new();
        let plates = std::iter::once((&self.ev, &self.plan, true)).chain(self.was.iter().map(|(ev, plan)| (ev, plan, false)));
        for (ev, planned, now) in plates {
            for (i, part) in ev.plate.parts.iter().enumerate() {
                let plan = &planned.parts[i];
                let shape = hash_part(ev, i);
                // a grid is the part's at the layer height and the contact setting
                let contact = grid::contact();
                let memo = {
                    let mut h = DefaultHasher::new();
                    (shape, ev.profile.layer.to_bits(), contact).hash(&mut h);
                    h.finish()
                };
                let kept = self.grids.lock().unwrap().get(&memo).cloned();
                let (solid, grid) = match kept {
                    Some(hit) => hit,
                    None => {
                        let solid = Arc::new(solid_of(ev, i)?);
                        let grid = Arc::new(DistGrid::check(ctl, &solid, ev.profile.layer, contact).map_err(|_| "cancelled".to_string())?);
                        let mut grids = self.grids.lock().unwrap();
                        if grids.len() >= MEMO_PARTS {
                            grids.clear();
                        }
                        grids.insert(memo, (solid.clone(), grid.clone()));
                        (solid, grid)
                    }
                };
                for piece in pieces_of(&part.object, &plan.skeleton, plan.at, &grid) {
                    let key = overrides::piece_key(shape, plan.at, &plan.skeleton, &piece.of);
                    match index.get(&key) {
                        Some(&e) if now => entries[e].now = true,
                        Some(&e) => entries[e].was = Some(piece.name.clone()),
                        None => {
                            index.insert(key, entries.len());
                            let was = (!now).then(|| piece.name.clone());
                            entries.push(Entry { solid: solid.clone(), grid: grid.clone(), part: i, piece, key, now, was });
                        }
                    }
                }
            }
        }
        Ok(Checking { entries, memo: self.verdicts.clone() })
    }

    /// The ready-made overrides for a piece of the plate asked about that won't come out: a block
    /// that takes it away.
    fn overrides_of(&self, checking: &Checking, e: &Entry) -> Vec<(&'static str, S)> {
        let Some(side) = self.sides.get(e.part) else { return Vec::new() };
        let plan = &self.plan.parts[e.part];
        let sk = &plan.skeleton;
        let reach = reach_of(&self.plan);
        let joined = overrides::joined(sk);
        let contacts = overrides::piece_contacts(sk, &e.piece.of, plan.at, reach, &joined);
        let others: Vec<[f64; 3]> = checking.entries.iter().filter(|o| o.now && o.part == e.part && o.key != e.key).flat_map(|o| overrides::piece_points(sk, &o.piece.of, plan.at, reach, &joined)).collect();
        let held = overrides::held_points(side, &contacts, reach);
        let mut fields = Vec::new();
        if let Some(o) = overrides::block_for(side, &held, &others) {
            fields.extend(override_fields(&o, "block"));
        }
        fields
    }

    /// The call's answer: the plan's, then the pieces, the ones that won't come out first, each of
    /// those with a block that takes it away. A try adds the committed plate's counts and the
    /// pieces it no longer has.
    pub fn answer(&self, checking: &Checking, results: &[Option<PieceVerdict>], ms: f64) -> S {
        let now: Vec<usize> = (0..checking.entries.len()).filter(|&i| checking.entries[i].now).collect();
        let pieces: Vec<&Piece> = now.iter().map(|&i| &checking.entries[i].piece).collect();
        let asked: Vec<Option<PieceVerdict>> = now.iter().map(|&i| results[i]).collect();
        let extra = |k: usize| {
            let e = &checking.entries[now[k]];
            let mut fields = Vec::new();
            if self.was.is_some() && e.was.is_none() {
                fields.push(("new", flag(true)));
            }
            let stuck = asked[k].is_some_and(|v| matches!(v.verdict, Verdict::Trapped | Verdict::NoReach | Verdict::TightReach));
            if stuck {
                fields.extend(self.overrides_of(checking, e));
            }
            fields
        };
        let mut verdicts = verdicts_value(&pieces, &asked, self.limit, ms, &extra);
        if self.was.is_some() {
            let before: Vec<usize> = (0..checking.entries.len()).filter(|&i| checking.entries[i].was.is_some()).collect();
            let count = |l: Light| before.iter().filter(|&&i| results[i].is_some_and(|v| v.verdict.colour() == l)).count();
            let gone: Vec<S> = before.iter().filter(|&&i| !checking.entries[i].now).map(|&i| {
                let verdict = results[i].map_or("not checked", |v| v.verdict.words());
                obj(vec![("piece", s(checking.entries[i].was.as_deref().unwrap_or_default())), ("verdict", s(verdict))])
            }).collect();
            if let S::Object(_, fields) = &mut verdicts {
                fields.push(("was".into(), obj(vec![("comes_out", int(count(Light::Green))), ("with_work", int(count(Light::Amber))), ("no_way_out", int(count(Light::Red)))])));
                fields.push(("gone".into(), arr(gone)));
            }
        }
        match self.plan_answer.clone() {
            S::Object(tag, mut fields) => {
                fields.push(("verdicts".into(), verdicts));
                S::Object(tag, fields)
            }
            other => other,
        }
    }
}

/// Pieces and their verdicts (None: not checked), the ones that won't come out first; `extra` adds
/// to the row of piece `i`.
pub fn verdicts_value(pieces: &[&Piece], results: &[Option<PieceVerdict>], limit: usize, ms: f64, extra: &dyn Fn(usize) -> Vec<(&'static str, S)>) -> S {
    let rank = |v: &Option<PieceVerdict>| match v.as_ref().map(|v| v.verdict.colour()) {
        Some(Light::Red) => 0,
        Some(Light::Amber) => 1,
        None => 2,
        Some(Light::Green) => 3,
    };
    let mut order: Vec<usize> = (0..results.len()).collect();
    order.sort_by_key(|&i| (rank(&results[i]), i));
    let count = |l: Light| results.iter().flatten().filter(|v| v.verdict.colour() == l).count();
    let rows: Vec<S> = order.iter().take(limit).map(|&i| {
        let piece = pieces[i];
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for b in &piece.balls {
            for k in 0..3 {
                lo[k] = lo[k].min(b.c[k] - b.r);
                hi[k] = hi[k].max(b.c[k] + b.r);
            }
        }
        let mut fields = vec![("piece", s(&piece.name))];
        match &results[i] {
            Some(v) => {
                fields.push(("verdict", s(v.verdict.words())));
                fields.push(("sections_out", s(format!("{}/{}", v.sections_out, v.sections))));
            }
            None => fields.push(("verdict", s("not checked"))),
        }
        // the solid the piece fills, not the material it prints (the part's volume_mm3)
        fields.push(("envelope_mm3", n(piece.volume)));
        if lo[0] <= hi[0] {
            fields.push(("box", arr([mm3(lo.map(|v| v as f64)), mm3(hi.map(|v| v as f64))])));
        }
        fields.extend(extra(i));
        obj(fields)
    }).collect();
    obj(vec![
        ("comes_out", int(count(Light::Green))), ("with_work", int(count(Light::Amber))), ("no_way_out", int(count(Light::Red))),
        ("check_ms", n(ms.round())), ("rows", arr(rows)), ("more", int(results.len().saturating_sub(limit))),
    ])
}

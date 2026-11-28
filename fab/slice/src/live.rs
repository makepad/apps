//! The live slicer: slices the plate in the background while parts move,
//! newest input wins, results stream out level by level and layer by layer.
//!
//! - Inputs never queue. [`LiveEngine::set_input`], [`LiveEngine::set_transform`]
//!   and [`LiveEngine::settle`] store the newest state and bump the
//!   input version. Running work finishes its viewed-layer chunk; every
//!   later chunk and layer map checks the installed checkpoint.
//! - [`LiveEngine::set_enabled`]: while off, inputs are recorded and nothing
//!   runs (no thread, no CPU); on again, the newest state is sliced.
//! - Parts are sliced in their own frame ([`Frame`]: local mesh + plate
//!   offset) and placed exactly (integer nm). Each part's mesh, contours and
//!   plan are kept by content and settings, so a part that only moved on the
//!   bed is never sliced or planned again.
//! - What is kept is also kept under the code that made it ([`LoadedCode`]): after a
//!   live patch under the JIT, [`LiveEngine::code_patched`] runs the newest input
//!   again, and only what the patched code makes is made again.
//! - Levels per revision, each streamed as it is made:
//!   1. contours ([`LiveEvent::PartCut`], [`LiveEvent::Contours`]) and,
//!      once no part is being moved, final model layers in the part's frame
//!      ([`LiveEvent::PartRegions`], [`LiveEvent::PartLayers`]);
//!   2. each dirty part's supports ([`LiveEvent::PartSupports`]), then the
//!      assembled plate ([`LiveEvent::Toolpaths`]) and, made beside it, the
//!      plate's mesh and contour checks ([`LiveEvent::Issues`]); a tree's
//!      report (its rules check and hold measure) is measured in the
//!      background once the toolpaths go out, and what reads it (the sliced
//!      checks, and Print, which waits for them) waits;
//!   3. printability on the toolpaths, layer by layer, at the lowest priority
//!      ([`LiveEvent::Issues`] with `done`/`total`);
//!   4. only when asked for with [`LiveEngine::settle`]: the exact G-code
//!      and printability of that same toolpath result ([`LiveEvent::Settled`]),
//!      without waiting for a tree's report: the sliced checks bring it;
//!      its printer package is built only when it is sent or exported
//!      ([`Settled::package_with`]). Live levels carry no G-code.
//! - Each model layer is planned once after tracing its read window: the focus,
//!   down to 0, then up to the top, doubling up to 64 layers. Dirty parts take
//!   turns; moving the focus reorders their remaining bands. Supports follow
//!   every dirty part's model layers, before [`LiveEvent::Toolpaths`].
//! - Everything an event carries is plain data (no handles into the engine),
//!   so the engine runs the same in the app or in a separate process
//!   ([`crate::serve`]).

use crate::plate::{part_filament, part_mesh};
use crate::live_paths::{Kept, LivePaths};
use crate::{check_inputs, finish, prepare_job, SliceResult};
use fab_plate::machine::Machine;
use fab_plate::{Part, PartMesh, Plate};
use fab_printability::{Materials, Overlaps, Report};
use makepad_fab_gcode::process::Timeline;
use makepad_fab_gcode::packed::MovesRef;
use makepad_fab_gcode::record::MoveRecord;
use makepad_fab_gcode::writer::Stats;
use makepad_fab_settings::machine::Job;
use makepad_fab_settings::profiles::JobSetup;
use makepad_fab_slicer::geom::{ExPolygon, Pt};
use makepad_fab_slicer::mesh::Mesh;
use makepad_fab_slicer::paint::{LayerPaint, PaintParams};
use makepad_csg_math::Vec3d;
use makepad_csg_sdf::SdfShape;
use makepad_fab_slicer::nonplanar::{NpBox, NpPlan, NpSettings};
use makepad_fab_slicer::sdf::{PlaneSlope, SdfParams, SdfSource, Slab, SlopeMap};
use makepad_fab_slicer::source::{ContourSource, MeshSource, PaintAt};
use makepad_fab_toolpath::stages::{self, PartPlan, PlateCtl, PlatePart, PlateRun, Task};
use makepad_fab_toolpath::{contours_some, grade, grade_key, is_support_key, par, paint_reads, plan_one, plan_reads, plan_reads_held, regions_bytes, solve_grade, Emit, ObjectInput, ObjectLayer, ObjectPlan, Params, Toolpaths, DROPPED_TIPS};
use std::collections::{HashMap, HashSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};

/// Row-major affine in printer mm (Z up), applied to a part as the plate
/// has it: `p' = m * p`.
pub type Affine = [[f64; 4]; 3];

#[path = "live_trace.rs"]
mod live_trace;
pub use live_trace::ResumeHook;

/// True when this input should stop; otherwise cooperatively yield the CPU budget.
pub type Checkpoint = Arc<dyn Fn() -> bool + Send + Sync>;
/// Capture a cancellation checkpoint for the next input (the app uses a pool Version).
pub type Next = Arc<dyn Fn() -> Checkpoint + Send + Sync>;

/// How soon an engine job's result is seen (the app maps it to a class on its shared task pool).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Urgency {
    /// Starting a revision, and its edited parts' viewed layers: drawn even once a newer input
    /// came, so the host runs it whatever the input's ticket says.
    Viewed,
    /// The other parts' viewed layers: ahead of the rest, dropped by a newer input.
    Focus,
    /// The rest of the slice.
    Rest,
}

/// Runs an engine job.
pub type Spawn = Arc<dyn Fn(Urgency, Box<dyn FnOnce() + Send>) + Send + Sync>;

/// Makes the job's setup on the engine's thread (preset resolution reads files). With `kept`,
/// the setup of only those of its tools (0-based, ascending, in that order): the ones a slice
/// printed in.
pub type SetupFn = Arc<dyn Fn(Option<&[usize]>) -> Result<JobSetup, String> + Send + Sync>;

/// The [`SetupFn`] of a setup made already: its kept tools are its [`JobSetup::keep`].
pub fn fixed_setup(setup: JobSetup) -> SetupFn {
    let setup = Arc::new(setup);
    Arc::new(move |kept| Ok(match kept {
        Some(kept) => setup.keep(kept),
        None => (*setup).clone(),
    }))
}

/// Builds an input on the engine's thread: copying a plate's meshes and
/// resolving presets never runs on the caller's (the UI's).
pub type MakeInput = Arc<dyn Fn() -> LiveInput + Send + Sync>;

/// The overlap issues of a [`LiveInput`]'s plate as its evaluation judged them, bound to
/// that very plate. Made only by [`LiveInput::with_overlaps`].
#[derive(Clone)]
pub struct BoundOverlaps {
    plate: Weak<Plate>,
    overlaps: Arc<Overlaps>,
    plate_id: u64,
}

/// What to slice.
#[derive(Clone)]
pub struct LiveInput {
    /// The plate's identity (its content hash): equal ids are the same plate.
    pub plate_id: u64,
    /// The settings' identity: equal ids slice to the same toolpaths.
    pub settings_id: u64,
    pub setup: SetupFn,
    pub plate: Arc<Plate>,
    /// Overlap issues bound by [`Self::with_overlaps`].
    pub overlaps: Option<BoundOverlaps>,
    /// Per part: its per-triangle paint stream (empty: unpainted).
    pub paint: Arc<Vec<Vec<String>>>,
    /// Per part, when known: the part before its plate translation, and the
    /// translation. Parts are sliced in that frame and placed exactly
    /// (integer nm), so a part that only moved on the bed is not sliced again.
    pub frames: Option<Arc<Vec<Frame>>>,
    /// The printer the plate is checked against.
    pub machine: Machine,
    /// The head envelope of the active nozzle as the non-planar planner's
    /// boxes (from the installed head profile). None: no non-planar tops.
    pub np_head: Option<Arc<Vec<NpBox>>>,
    /// A test plate ([`crate::piece`]): its parts are cut to these cubes. `plate`, `frames` and
    /// `paint` are then the whole parts they are cut from, one entry per piece of a part.
    pub pieces: Option<Arc<crate::piece::Pieces>>,
}

/// A part as `local + offset` (the plate's mesh is `local` translated by `offset`).
#[derive(Clone, Debug)]
pub struct Frame {
    pub local: Arc<PartMesh>,
    pub offset: [f64; 3],
    /// A pure SDF part: sliced from this field, `local` is then its display
    /// mesh only. None: sliced from `local`.
    pub field: Option<PartField>,
}

/// The class displacement takes the representative's vertices to the copy's local mesh.
/// Adding the copy's frame offset places them on the bed.
fn copy_plan_offset(copy_offset: Pt, class_displacement: Pt) -> Pt {
    copy_offset.add(class_displacement)
}

/// A pure SDF part: its live typed shape (model coordinates, kernels as
/// compiled in this process) and the rigid map from model coordinates to its
/// frame's (like [`Frame::local`]). `content` (the shape's key, map, colour states and box)
/// is what the engine's caches key on.
#[derive(Clone)]
pub struct PartField {
    pub shape: Arc<SdfShape>,
    pub to_frame: Affine,
    pub content: u64,
    /// The state (0: the part's own filament, N: filament N) per material
    /// slot of the shape; `None`: uncoloured.
    pub states: Option<Arc<[u32]>>,
    /// The solid's box in the frame (mm), as the layout placed it: the range the field is
    /// sliced over. None: the shape's box mapped into the frame.
    pub bounds: Option<([f64; 3], [f64; 3])>,
    /// [`Self::slope`], read once for the field and the copies that keep its shape.
    slope: Arc<std::sync::OnceLock<(f64, f64)>>,
    /// [`Self::layer_slope`]'s bands, read once as [`Self::slope`] is.
    bands: Arc<std::sync::OnceLock<Bands>>,
    /// [`Self::window`], made once.
    window: Arc<std::sync::OnceLock<makepad_csg_sdf::Window>>,
}

/// How thick a band of planes is that shares one tracer slope ([`PartField::layer_slope`]), mm.
const SLOPE_BAND: f64 = 1.0;

/// The tracer's slope for each band of planes over a part's height: band `first + k` holds the
/// planes sampled at frame heights `(first + k) * SLOPE_BAND .. (first + k + 1) * SLOPE_BAND`,
/// its `(lipschitz, window)` and its map of slopes square by square ([`band_slope`]), and the
/// cells of the world's grid over its footprint they are read from.
struct Bands {
    first: i64,
    slopes: Vec<(f64, f64)>,
    maps: Vec<Arc<SlopeMap>>,
    cells: Vec<BandCells>,
}

/// A band's cells of the world's grid ([`SLOPE_CELL`] a side): `x0..x0 + nx` by `y0..`, row by
/// row.
struct BandCells {
    x0: i64,
    y0: i64,
    nx: i64,
    of: Vec<Option<Arc<CellProof>>>,
}

impl BandCells {
    fn at(&self, x: i64, y: i64) -> Option<&Option<Arc<CellProof>>> {
        let (i, j) = (x - self.x0, y - self.y0);
        (i >= 0 && i < self.nx && j >= 0).then(|| self.of.get((j * self.nx + i) as usize)).flatten()
    }
}

/// The windows a band's slope is tried at, least first, in shares of the tracer's reach: the band
/// takes the first that all its cells prove ([`band_slope`]).
const SLOPE_WINDOWS: [f64; 9] = [1.0, 1.25, 1.5, 2.0, 2.5, 3.0, 4.0, 6.0, 8.0];

/// The world's grid a band's slope is proven on: cells this wide in the frame's X and Y (mm),
/// on its multiples, each split in four down to [`SLOPE_LEAF`] wide where its tree proves none.
const SLOPE_CELL: f64 = 16.0;
const SLOPE_LEAF: f64 = 2.0;

/// The windows under the [`SLOPE_WINDOWS`] a leaf that holds no circle of the tracer's reach is
/// tried at, widest first, in shares of that reach: its smaller circles ([`Leaf::slope`]).
const LEAF_WINDOWS: [f64; 6] = [0.75, 0.5, 0.35, 0.25, 0.18, 0.125];

/// The least of the [`SLOPE_WINDOWS`] that all of a band's `cells` prove for circles of `reach`
/// ([`CellProof::quad`]), or the widest where none does.
fn band_window(cells: &BandCells, reach: f64, into: &(dyn Fn([f64; 3]) -> [f64; 3] + Sync)) -> usize {
    (0..SLOPE_WINDOWS.len()).find(|&i| cells.of.iter().flatten().all(|c| c.quad(i, reach, into).is_some_and(|q| q.most().is_finite()))).unwrap_or(SLOPE_WINDOWS.len() - 1)
}

/// A band's slope from its `cells`, for circles of `reach`, at its window `i` ([`band_window`]):
/// the most of their slopes, at least 1, and the map of them at that window square by square
/// ([`SLOPE_LEAF`] wide); at the widest, where none proves, each leaf it does not prove at its
/// own slope and window
/// ([`Leaf::slope`]), the plane's slope the most of the map's, a leaf that proves none counting
/// as the most a window asks of one (the plane's slope scales its reads and its crossing
/// tolerance, never a skip, and so stays with the band's own leaves). A proven band's window holds
/// every circle of `reach` at each of its slopes, so the plane reads no window of its own.
/// Every slope is proven: but a sculpt's, the whole shape's measure (`whole`). A cell's quads
/// depend on its own box, place and windowed tree alone ([`makepad_csg_sdf::cut_within`]), so
/// an edit that leaves a cell's tree leaves its slopes, bit for bit (but a sculpt's).
fn band_slope(cells: &BandCells, i: usize, reach: f64, into: &(dyn Fn([f64; 3]) -> [f64; 3] + Sync), whole: &dyn Fn() -> (f64, f64)) -> ((f64, f64), Arc<SlopeMap>) {
    let last = SLOPE_WINDOWS.len() - 1;
    let w = SLOPE_WINDOWS[i] * reach;
    let per = (SLOPE_CELL / SLOPE_LEAF) as usize;
    let nx = cells.nx as usize;
    let ny = cells.of.len() / nx.max(1);
    // (a cell none of whose values lie within the widest window has none within this one)
    let mut of = vec![(0.0, w); nx * per * ny * per];
    for (n, c) in cells.of.iter().enumerate() {
        if let Some(c) = c {
            let q = c.quad(i, reach, into).expect("the widest window's quads hold every leaf");
            q.fill((n % nx) * per, (n / nx) * per, per, nx * per, w, &mut of, &|leaf| leaf.slope(reach, whole));
        }
    }
    let lipschitz = of.iter().map(|q| if q.0.is_finite() { q.0 } else { SLOPE_WINDOWS[last] }).fold(1.0f64, f64::max);
    let map = SlopeMap { x0: cells.x0 as f64 * SLOPE_CELL, y0: cells.y0 as f64 * SLOPE_CELL, size: SLOPE_LEAF, nx: nx * per, ny: ny * per, of };
    ((lipschitz, f64::INFINITY), Arc::new(map))
}

/// A cell of a band's footprint ([`band_slope`]): its frame box, its model box, the shape's tree
/// cut to it for the values within the widest of the [`SLOPE_WINDOWS`] (its value there clamped
/// to that window is the tree's, bit for bit: [`makepad_csg_sdf::cut_within`]), what it is known
/// by ([`cell_proof`]), and its quads at each window as far as they were asked.
struct CellProof {
    frame: makepad_csg_sdf::Bounds,
    model: makepad_csg_sdf::Bounds,
    key: ([u64; 19], [u8; 32]),
    cut: makepad_csg_sdf::Cut,
    quads: [std::sync::OnceLock<Option<Quad>>; SLOPE_WINDOWS.len()],
}

/// The cell `frame` of `root` (`win` its window, [`PartField::window`]; `to_frame` the field's
/// place, `into` its map from the frame to the model): None where no value of the tree lies
/// within the window there. Known by its place, frame box, cut tree and the tracer's reach: `was`,
/// this cell of the field before, stands where all of them are the same, with the slopes it has
/// proven, the ones a fresh proof makes. Nothing else keeps a proof: it goes with the fields that
/// read it, and with it the pieces of their shapes it cut.
fn cell_proof(root: &makepad_csg_sdf::SdfNode, win: &makepad_csg_sdf::Window, to_frame: &Affine, frame: makepad_csg_sdf::Bounds, reach: f64, into: &(dyn Fn([f64; 3]) -> [f64; 3] + Sync), was: Option<&Arc<CellProof>>) -> Option<Arc<CellProof>> {
    let model = makepad_csg_sdf::bounds::map_bounds(&frame, into);
    let cut = makepad_csg_sdf::cut_within(root, win, &model)?;
    let mut at = [0u64; 19];
    for (k, v) in to_frame.iter().flatten().chain(frame.0.iter()).chain(frame.1.iter()).chain([&reach]).enumerate() {
        at[k] = v.to_bits();
    }
    let key = (at, cut.node.key());
    if let Some(c) = was.filter(|c| c.key == key) {
        return Some(c.clone());
    }
    Some(Arc::new(CellProof { frame, model, key, cut, quads: Default::default() }))
}

impl CellProof {
    /// The cell's slopes at the values within the `i`-th of the [`SLOPE_WINDOWS`] (`reach` times
    /// it), where each is at most the window's share ([`prove`]): None where one is more, but at
    /// the widest window, whose quads hold the leaves no tree proves.
    fn quad(&self, i: usize, reach: f64, into: &(dyn Fn([f64; 3]) -> [f64; 3] + Sync)) -> Option<&Quad> {
        self.quads[i].get_or_init(|| {
            let w = SLOPE_WINDOWS[i] * reach;
            // (the cut's bounds are its own value's, the tree's clamped alike)
            if self.cut.lo > w || self.cut.hi < -w {
                return Some(Quad::All(0.0));
            }
            prove(&self.cut.node, &self.frame, &self.model, w, SLOPE_WINDOWS[i], into, i + 1 == SLOPE_WINDOWS.len())
        }).as_ref()
    }
}

/// A square's slopes at a window ([`prove`]): one over the whole square, a leaf no tree proves,
/// or its four quarters' (X then Y: quarter `q` at `x + h` where `q & 1`, `y + h` where `q & 2`).
enum Quad {
    /// The most slope over the square at the window's values, at most the window's share; 0
    /// where none of them lie there.
    All(f64),
    Leaf(Box<Leaf>),
    Four(Box<[Quad; 4]>),
}

impl Quad {
    /// The most slope over the square, infinite where a leaf is not proven.
    fn most(&self) -> f64 {
        match self {
            Quad::All(s) => *s,
            Quad::Leaf(_) => f64::INFINITY,
            Quad::Four(q) => q.iter().map(Quad::most).fold(0.0, f64::max),
        }
    }

    /// The leaves it holds, into `out`: what [`Quad::fill`] asks the slopes of ([`Leaf::slope`]).
    fn leaves<'a>(&'a self, out: &mut Vec<&'a Leaf>) {
        match self {
            Quad::All(_) => {}
            Quad::Leaf(l) => out.push(l),
            Quad::Four(q) => q.iter().for_each(|q| q.leaves(out)),
        }
    }

    /// The square's slopes at window `w` into the map `of` (`stride` squares a row), its `n` by
    /// `n` squares from `(i, j)`: a leaf's slope and window from `leaf`.
    fn fill(&self, i: usize, j: usize, n: usize, stride: usize, w: f64, of: &mut [(f64, f64)], leaf: &dyn Fn(&Leaf) -> (f64, f64)) {
        let s = match self {
            Quad::All(s) => (*s, w),
            Quad::Leaf(l) => leaf(l),
            Quad::Four(q) => {
                let h = n / 2;
                for (k, q) in q.iter().enumerate() {
                    q.fill(i + if k & 1 == 0 { 0 } else { h }, j + if k & 2 == 0 { 0 } else { h }, h, stride, w, of, leaf);
                }
                return;
            }
        };
        for row in j..j + n {
            of[row * stride + i..row * stride + i + n].fill(s);
        }
    }
}

/// A leaf of a cell ([`SLOPE_LEAF`] wide) whose tree proves no slope within the widest window's
/// share: its model box and tree cut to it, and its slope and window once proven.
struct Leaf {
    model: makepad_csg_sdf::Bounds,
    node: makepad_csg_sdf::SdfNode,
    proven: std::sync::OnceLock<(f64, f64)>,
}

impl Leaf {
    /// The leaf's slope and window for circles of `reach`: the least of the [`SLOPE_WINDOWS`] whose
    /// slope it proves holds such a circle; where none does, the slope and window, of those and
    /// the [`LEAF_WINDOWS`], that clear the widest circle (`window / slope`), each slope at most
    /// the widest window's share, `(infinity, 0)` where it proves none; or where a sculpt is in
    /// it, which nothing proves, the whole shape's measured slope and window (`whole`).
    fn slope(&self, reach: f64, whole: &dyn Fn() -> (f64, f64)) -> (f64, f64) {
        if makepad_csg_sdf::has_sculpt(&self.node) {
            return whole();
        }
        *self.proven.get_or_init(|| {
            // (it is a leaf for proving more than the widest window's share there)
            let (&most, under) = SLOPE_WINDOWS.split_last().expect("windows");
            let mut best = (f64::INFINITY, 0.0);
            for &k in under.iter().chain(&LEAF_WINDOWS) {
                let w = k * reach;
                // (a slope is at least 1: no window this small clears a wider circle)
                if k < 1.0 && w <= best.1 / best.0 {
                    break;
                }
                let s = makepad_csg_sdf::bounds::slopes_within(&self.node, &self.model, w);
                let s = s.outside.max(s.inside).max(1.0);
                if s <= k {
                    return (s, w);
                }
                if s <= most && w / s > best.1 / best.0 {
                    best = (s, w);
                }
            }
            best
        })
    }
}

/// The slopes of `node` over the frame box `frame` (model box `model`) at the values within
/// window `w`, where each is at most `held`: its own tree's, or else its four quarters' (each
/// its tree cut to it) down to [`SLOPE_LEAF`]; None where a leaf's is more, or with `leaves`,
/// that leaf ([`Quad::Leaf`]). A quarter none of whose values lie within the window is 0.
fn prove(node: &makepad_csg_sdf::SdfNode, frame: &makepad_csg_sdf::Bounds, model: &makepad_csg_sdf::Bounds, w: f64, held: f64, into: &(dyn Fn([f64; 3]) -> [f64; 3] + Sync), leaves: bool) -> Option<Quad> {
    let s = makepad_csg_sdf::bounds::slopes_within(node, model, w);
    let s = s.outside.max(s.inside).max(1.0);
    if s <= held {
        return Some(Quad::All(s));
    }
    let size = frame.1[0] - frame.0[0];
    if size <= SLOPE_LEAF {
        return leaves.then(|| Quad::Leaf(Box::new(Leaf { model: *model, node: node.clone(), proven: std::sync::OnceLock::new() })));
    }
    let h = 0.5 * size;
    let mut four: [Quad; 4] = std::array::from_fn(|_| Quad::All(0.0));
    for (q, quarter) in four.iter_mut().enumerate() {
        let (x, y) = (frame.0[0] + if q & 1 == 0 { 0.0 } else { h }, frame.0[1] + if q & 2 == 0 { 0.0 } else { h });
        let kf = ([x, y, frame.0[2]], [x + h, y + h, frame.1[2]]);
        let km = makepad_csg_sdf::bounds::map_bounds(&kf, into);
        let c = makepad_csg_sdf::cut(node, &km);
        if c.lo > w || c.hi < -w {
            continue;
        }
        *quarter = prove(&c.node, &kf, &km, w, held, into, leaves)?;
    }
    Some(Quad::Four(Box::new(four)))
}

/// The band a plane `z` is traced in: by the height its values are sampled at.
fn band_of(z: f64) -> i64 {
    ((z - SdfSource::BELOW_PLANE) / SLOPE_BAND).floor() as i64
}

/// The chunks a field part's layers read are about this size (mm; bigger for a big part:
/// [`makepad_csg_sdf::Chunks::new`]) ...
const CHUNK_SIZE: f64 = 1.0;
/// ... over its footprint grown by this much, which holds the reads of a layer's tracer past it.
const CHUNKS_MARGIN: f64 = 4.0;

impl PartField {
    /// `shape` under `m`, a rigid map with a uniform scale or a stretch along the shape's own
    /// axes ([`makepad_csg_sdf::stretched`]): the scale and the stretch go into the shape, so the
    /// field stays a distance (a lower bound under a stretch). None for any other map.
    pub fn placed(shape: Arc<SdfShape>, m: Affine) -> Option<Self> {
        let (shape, m) = match makepad_csg_sdf::stretched(&shape, &m)? {
            (Some(s), rest) => (Arc::new(s), rest),
            (None, _) => (shape, m),
        };
        let col = |c: usize| [m[0][c], m[1][c], m[2][c]];
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let k = dot(col(0), col(0)).sqrt();
        let similar = k > 1e-9 && (0..3).all(|i| (0..3).all(|j| (dot(col(i), col(j)) - if i == j { k * k } else { 0.0 }).abs() <= 1e-9 * k * k));
        if !similar {
            return None;
        }
        if (k - 1.0).abs() <= 1e-12 {
            return Some(PartField::new(shape, m));
        }
        let scaled = SdfShape {
            root: makepad_csg_sdf::SdfNode::Transform { at: [0.0; 3], rotate: [0.0; 3], scale: k, of: Box::new(shape.root.clone()) },
            min: shape.min.map(|v| v * k),
            max: shape.max.map(|v| v * k),
            materials: shape.materials.clone(),
        };
        let rigid = std::array::from_fn(|r| [m[r][0] / k, m[r][1] / k, m[r][2] / k, m[r][3]]);
        Some(PartField::new(Arc::new(scaled), rigid))
    }

    pub fn new(shape: Arc<SdfShape>, to_frame: Affine) -> Self {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut eat = |x: u64| h = (h ^ x).wrapping_mul(0x100_0000_01b3);
        for c in shape.key().chunks(8) {
            eat(u64::from_le_bytes(c.try_into().unwrap()));
        }
        for r in &to_frame {
            for v in r {
                eat(v.to_bits());
            }
        }
        PartField { shape, to_frame, content: h, states: None, bounds: None, slope: Arc::default(), bands: Arc::default(), window: Arc::default() }
    }

    /// The tracer's `(lipschitz, window)` for every layer ([`SdfParams`]): how steep the shape's
    /// values prove it near its surface, for circles of the coarse search's reach. A window that
    /// holds the reach at that slope decides no skip (each skip asks `min(|v|, window)` for at
    /// most the reach times the slope), so it reads as none, and the pair is the shape's slope
    /// alone, not its box's size. A layer an earlier cut traced is the layer a fresh cut traces
    /// only while this is the same ([`crate::sdf_diff::changed_layers`]).
    pub fn slope(&self) -> (f64, f64) {
        *self.slope.get_or_init(|| {
            let reach = SdfParams::default().reach();
            let (s, w) = self.shape.clearance().at(reach);
            (s, if w >= s * reach { f64::INFINITY } else { w })
        })
    }

    /// The tracer's `(lipschitz, window)` for the plane `z`: what the shape's pieces prove over
    /// the plane's band ([`SLOPE_BAND`] of height, over the shape's own footprint and the coarse
    /// search's reach past it), cell by cell of the world's grid, for circles of the search's
    /// reach, or where they prove none there, each leaf at its own window ([`band_slope`]); the
    /// whole shape's ([`Self::slope`]) past the bands. A band near the shape's steep pieces then costs
    /// only its own layers their slope, and an edit only the layers whose bands' cells it
    /// reaches ([`crate::sdf_diff::changed_layers`]).
    pub fn layer_slope(&self, z: f64) -> (f64, f64) {
        self.band_slope(band_of(z))
    }

    /// The tracer's slope for the plane `z` ([`PlaneSlope`]): [`Self::layer_slope`], and its
    /// band's slopes square by square, which each of the tracer's circles reads where it lies.
    pub fn plane_slope(&self, z: f64) -> PlaneSlope {
        let ((lipschitz, window), map) = (self.band_slope(band_of(z)), self.band_map(band_of(z)));
        PlaneSlope { lipschitz, window, map }
    }

    fn band_slope(&self, band: i64) -> (f64, f64) {
        let b = self.bands();
        usize::try_from(band - b.first).ok().and_then(|k| b.slopes.get(k).copied()).unwrap_or_else(|| self.slope())
    }

    fn band_map(&self, band: i64) -> Option<Arc<SlopeMap>> {
        let b = self.bands();
        usize::try_from(band - b.first).ok().and_then(|k| b.maps.get(k).cloned())
    }

    /// The frame-space height ranges where `self`'s and `other`'s planes can be traced otherwise
    /// for their slopes ([`Self::plane_slope`]): their bands' ranges where the plane's slope
    /// differs, which scales what the tracer reads and its crossing tolerance. A map of proven
    /// slopes leaves out only cells that hold no segment, so two such maps trace a plane alike;
    /// a sculpt's slope is the whole shape's measure, not a proof, so a shape with one also
    /// compares the maps, over the squares both hold (a square only one holds lies past the
    /// other's footprint and its circles' reach, where any of the shape's own change is
    /// [`crate::sdf_diff::changed_z`]'s). `other`'s bands, when not read yet, take the cells the
    /// edit from `self` leaves from `self` ([`Self::prove_bands`]).
    pub fn slopes_changed(&self, other: &PartField) -> Vec<(f64, f64)> {
        let a = self.bands();
        other.prove_bands(Some(self));
        let b = other.bands();
        let lo = a.first.min(b.first);
        let hi = (a.first + a.slopes.len() as i64).max(b.first + b.slopes.len() as i64);
        let same = |x: (f64, f64), y: (f64, f64)| x.0.to_bits() == y.0.to_bits() && x.1.to_bits() == y.1.to_bits();
        let mut out: Vec<(f64, f64)> = Vec::new();
        let measured = makepad_csg_sdf::has_sculpt(&self.shape.root) || makepad_csg_sdf::has_sculpt(&other.shape.root);
        let same_map = |x: Option<Arc<SlopeMap>>, y: Option<Arc<SlopeMap>>| match (x, y) {
            (Some(x), Some(y)) => !measured || Arc::ptr_eq(&x, &y) || x.same_over_both(&y),
            (x, y) => x.is_none() && y.is_none(),
        };
        for band in lo..hi {
            if same(self.band_slope(band), other.band_slope(band)) && same_map(self.band_map(band), other.band_map(band)) {
                continue;
            }
            let (z0, z1) = (band as f64 * SLOPE_BAND + SdfSource::BELOW_PLANE, (band + 1) as f64 * SLOPE_BAND + SdfSource::BELOW_PLANE);
            match out.last_mut() {
                Some(last) if last.1 == z0 => last.1 = z1,
                _ => out.push((z0, z1)),
            }
        }
        if !same(self.slope(), other.slope()) {
            // the planes past both sets of bands are traced with the whole shapes' slopes
            out.insert(0, (f64::NEG_INFINITY, lo as f64 * SLOPE_BAND + SdfSource::BELOW_PLANE));
            out.push((hi as f64 * SLOPE_BAND + SdfSource::BELOW_PLANE, f64::INFINITY));
        }
        out
    }

    fn bands(&self) -> &Bands {
        self.bands.get_or_init(|| self.read_bands(None).0)
    }

    /// The tree's [`makepad_csg_sdf::Window`] for the values within the widest of the
    /// [`SLOPE_WINDOWS`]: what its cells are cut by.
    fn window(&self) -> &makepad_csg_sdf::Window {
        self.window.get_or_init(|| makepad_csg_sdf::window(&self.shape.root, SLOPE_WINDOWS[SLOPE_WINDOWS.len() - 1] * SdfParams::default().reach()))
    }

    /// Proves the bands' slopes ([`Self::layer_slope`]) now, if not yet: each band's cells of the
    /// world's grid over the shape's footprint and the coarse search's reach past it. A cell that
    /// `last` (this field before an edit, in the same place) read and the edit cannot reach
    /// ([`makepad_csg_sdf::changed_within`]: its windowed tree is the same) is taken from it
    /// unread, so an edit cuts and proves only the cells its change reaches; any other cell is cut
    /// from the tree, and keeps `last`'s proof where it cuts the same tree ([`cell_proof`]). Both
    /// give the cell a fresh proof gives. How many cells it took from `last` unread (none where the
    /// bands were proven before).
    pub fn prove_bands(&self, last: Option<&PartField>) -> usize {
        let mut kept = 0;
        self.bands.get_or_init(|| {
            let (bands, k) = self.read_bands(last);
            kept = k;
            bands
        });
        kept
    }

    /// [`Self::prove_bands`]'s bands, and how many cells they took from `last`.
    fn read_bands(&self, last: Option<&PartField>) -> (Bands, usize) {
        let p = SdfParams::default();
        let reach = p.reach();
        let (lo, hi) = self.own_box();
        // the coarse nodes stand up to two cells past the footprint, each reading a circle
        // of the reach
        let g = 3.0 * p.coarse + reach;
        let first = band_of(lo[2]);
        let n = (band_of(hi[2]) - first + 1).max(0) as usize;
        let into = self.into_model();
        let win = self.window();
        let same_place = |pf: &PartField| (0..3).all(|r| (0..4).all(|k| pf.to_frame[r][k].to_bits() == self.to_frame[r][k].to_bits()));
        let before = last.filter(|pf| same_place(pf)).and_then(|pf| {
            let bands = pf.bands.get()?;
            Some((bands, makepad_csg_sdf::changed_within(&pf.shape.root, pf.window(), &self.shape.root, win)))
        });
        let (x0, y0) = (((lo[0] - g) / SLOPE_CELL).floor() as i64, ((lo[1] - g) / SLOPE_CELL).floor() as i64);
        let (x1, y1) = (((hi[0] + g) / SLOPE_CELL).ceil() as i64, ((hi[1] + g) / SLOPE_CELL).ceil() as i64);
        let kept = std::sync::atomic::AtomicUsize::new(0);
        // Work for the pool in short items, so a person's job never waits long behind one: a
        // band's cells and its window, a band an item; then, in a band at the widest window, its
        // cells' quads there, a cell an item, and the slopes of the leaves they hold, a leaf an
        // item (most of the work: a band's took up to a second on cold code), but for the cells
        // and leaves proven before (a kept cell's), which need no item; then each band's slope
        // and map from them, a band an item.
        let found = par::map(n, |k| {
            // the heights its planes are sampled at ([`band_of`])
            let band = first + k as i64;
            let z = (band as f64 * SLOPE_BAND, (band + 1) as f64 * SLOPE_BAND);
            let old = before.as_ref().and_then(|(b, change)| Some((usize::try_from(band - b.first).ok().and_then(|k| b.cells.get(k))?, change.as_ref())));
            let of = (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| {
                let frame = ([x as f64 * SLOPE_CELL, y as f64 * SLOPE_CELL, z.0], [(x + 1) as f64 * SLOPE_CELL, (y + 1) as f64 * SLOPE_CELL, z.1]);
                let was = old.and_then(|(cells, _)| cells.at(x, y));
                if let Some((_, Some(change))) = old {
                    if let Some(c) = was.filter(|_| !makepad_csg_sdf::reaches(&makepad_csg_sdf::bounds::map_bounds(&frame, &into), change)) {
                        kept.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        return c.clone();
                    }
                }
                cell_proof(&self.shape.root, win, &self.to_frame, frame, reach, &into, was.and_then(Option::as_ref))
            });
            let cells = BandCells { x0, y0, nx: x1 - x0, of: of.collect() };
            let i = band_window(&cells, reach, &into);
            (cells, i)
        });
        let widest = SLOPE_WINDOWS.len() - 1;
        let at_widest: Vec<&CellProof> = found.iter().filter(|(_, i)| *i == widest).flat_map(|(cells, _)| cells.of.iter().flatten().map(|c| &**c)).collect();
        let unproven: Vec<&CellProof> = at_widest.iter().copied().filter(|c| c.quads[widest].get().is_none()).collect();
        par::map(unproven.len(), |m| {
            unproven[m].quad(widest, reach, &into);
        });
        let mut leaves = Vec::new();
        at_widest.iter().for_each(|c| c.quad(widest, reach, &into).expect("the widest window's quads hold every leaf").leaves(&mut leaves));
        leaves.retain(|l| l.proven.get().is_none());
        par::map(leaves.len(), |m| leaves[m].slope(reach, &|| self.slope()));
        let (slopes, maps) = par::map(n, |k| band_slope(&found[k].0, found[k].1, reach, &into, &|| self.slope())).into_iter().unzip();
        let cells = found.into_iter().map(|(c, _)| c).collect();
        (Bands { first, slopes, maps, cells }, kept.into_inner())
    }

    /// The shape's own box in the frame: the corners of its box, placed.
    fn own_box(&self) -> ([f64; 3], [f64; 3]) {
        let (m, (a, b)) = (self.to_frame, (self.shape.min, self.shape.max));
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for k in 0..8 {
            let c = [if k & 1 == 0 { a[0] } else { b[0] }, if k & 2 == 0 { a[1] } else { b[1] }, if k & 4 == 0 { a[2] } else { b[2] }];
            for r in 0..3 {
                let v = m[r][0] * c[0] + m[r][1] * c[1] + m[r][2] * c[2] + m[r][3];
                lo[r] = lo[r].min(v);
                hi[r] = hi[r].max(v);
            }
        }
        (lo, hi)
    }

    /// Frame to model: the transpose of the frame's rotation, after its offset.
    fn into_model(&self) -> impl Fn([f64; 3]) -> [f64; 3] + Send + Sync + Clone {
        let m = self.to_frame;
        let col = |c: usize| [m[0][c], m[1][c], m[2][c]];
        let (t, cols) = ([m[0][3], m[1][3], m[2][3]], [col(0), col(1), col(2)]);
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        move |p: [f64; 3]| {
            let d = [p[0] - t[0], p[1] - t[1], p[2] - t[2]];
            [dot(cols[0], d), dot(cols[1], d), dot(cols[2], d)]
        }
    }

    /// This field coloured: `states[slot]` per material slot (missing: 0).
    /// Uncoloured (`None`) when every state is 0.
    pub fn with_states(mut self, states: Vec<u32>) -> Self {
        if states.iter().all(|&s| s == 0) {
            return self;
        }
        let mut h = self.content;
        for &s in &states {
            h = (h ^ (s as u64 + 1)).wrapping_mul(0x100_0000_01b3);
        }
        self.content = h;
        self.states = Some(states.into());
        self
    }

    /// This field sliced over `bounds` (frame mm): see [`Self::bounds`].
    pub fn with_bounds(mut self, bounds: Option<([f64; 3], [f64; 3])>) -> Self {
        self.bounds = bounds;
        if let Some((lo, hi)) = bounds {
            let mut h = self.content;
            for v in lo.into_iter().chain(hi) {
                h = (h ^ v.to_bits()).wrapping_mul(0x100_0000_01b3);
            }
            self.content = h;
        }
        self
    }

    /// This field under `m` (frame to new frame) after its own map: [`Self::placed`] with its
    /// colour states and its box carried over (the box of the moved box's corners).
    pub fn moved(&self, m: &Affine) -> Option<PartField> {
        let f = PartField::placed(self.shape.clone(), affine_mul(m, &self.to_frame))?;
        let f = match &self.states {
            Some(s) => f.with_states(s.to_vec()),
            None => f,
        };
        Some(f.with_bounds(self.bounds.map(|b| makepad_csg_sdf::bounds::map_bounds(&b, |q| std::array::from_fn(|r| m[r][0] * q[0] + m[r][1] * q[1] + m[r][2] * q[2] + m[r][3])))))
    }

    /// The field as a contour source in the frame's coordinates. The map must
    /// be rigid (distances are kept); every kernel of the shape loaded.
    pub fn source(&self) -> Result<Arc<dyn ContourSource + Send + Sync>, String> {
        let f = self.field()?;
        // The shape's own box in the frame: sliced over when no extent is proven, and the box the
        // planner centres the part's patterns on, which no proof of its extent moves.
        let own = self.own_box();
        let (lo, hi) = self.bounds.unwrap_or(own);
        // every plane's slope before the planes are traced, which ask for them in parallel
        self.layer_slope(lo[2]);
        let layers = self.clone();
        let mut src = SdfSource::new(f, (lo, hi)).with_slab(self.slab((lo, hi))).with_pivot(([own.0[0], own.0[1]], [own.1[0], own.1[1]])).with_slopes(Arc::new(move |z| layers.plane_slope(z)));
        // a field need not be a distance (a gyroid climbs ~1.9x, a flat ellipsoid's estimate
        // overstates its depth): the coarse search's reach and the crossing tolerance scale with
        // the slope its values prove circles of the search's radius clear at, each plane's
        // ([`Self::layer_slope`]) or, where a reader takes one for all, the whole shape's
        (src.params.lipschitz, src.params.window) = self.slope();
        Ok(Arc::new(match self.material() {
            Some(m) => src.with_material(m),
            None => src,
        }))
    }

    /// Per layer: the shape culled ([`makepad_csg_sdf::cull`]) to the slab
    /// `z0..z1` over the footprint `frame` (frame coordinates, mapped into
    /// the model's), as the field and material of [`Self::field`] and
    /// [`Self::material`] read there. Nothing reaches the slab: a field
    /// positive everywhere.
    fn slab(&self, frame: ([f64; 3], [f64; 3])) -> makepad_fab_slicer::sdf::SlabFn {
        use makepad_csg_sdf::Sdf3;
        let (shape, m, states) = (self.shape.clone(), self.to_frame, self.states.clone());
        let col = |c: usize| [m[0][c], m[1][c], m[2][c]];
        let (t, cols) = ([m[0][3], m[1][3], m[2][3]], [col(0), col(1), col(2)]);
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let into = move |p: [f64; 3]| {
            let d = [p[0] - t[0], p[1] - t[1], p[2] - t[2]];
            [dot(cols[0], d), dot(cols[1], d), dot(cols[2], d)]
        };
        // The whole tree in chunks over the footprint, each reduced to its box once for every layer
        // that reads it: a point of a layer's slab reads its chunk's tree, which gives the value the
        // slab's own reduced tree gives there (each is the whole tree's over its box, bit for bit).
        let grown = ([frame.0[0] - CHUNKS_MARGIN, frame.0[1] - CHUNKS_MARGIN, frame.0[2] - CHUNKS_MARGIN], [frame.1[0] + CHUNKS_MARGIN, frame.1[1] + CHUNKS_MARGIN, frame.1[2] + CHUNKS_MARGIN]);
        let chunks = Arc::new(makepad_csg_sdf::Chunks::new(Arc::new(shape.root.clone()), makepad_csg_sdf::bounds::map_bounds(&grown, into), CHUNK_SIZE));
        Arc::new(move |z0: f64, z1: f64| {
            // The tracer reads a coarse cell beyond the footprint: the slab's
            // margin covers it.
            let g = 0.5 * (z1 - z0);
            let slab = ([frame.0[0] - g, frame.0[1] - g, z0], [frame.1[0] + g, frame.1[1] + g, z1]);
            let region = makepad_csg_sdf::bounds::map_bounds(&slab, into);
            let Some(node) = makepad_csg_sdf::cull(&shape.root, &region).map(Arc::new) else {
                return Slab { field: Arc::new(|_, _, _| f64::MAX), many: None, material: None };
            };
            let n = node.clone();
            let field: makepad_fab_slicer::sdf::Field3 = Arc::new(move |x: f64, y: f64, z: f64| {
                let q = into([x, y, z]);
                makepad_csg_sdf::distance(&n, Vec3d::new(q[0], q[1], q[2]))
            });
            // A kernel answers a batch in one call (the tracer's grids and crossings), and a tree
            // is read only as far as the tracer's reach needs: the points of the slab through the
            // chunks, the others (just past it) through its reduced tree.
            let (tree, chunks) = (node.field(), chunks.clone());
            let within = move |q: &Vec3d| q.x > region.0[0] && q.x < region.1[0] && q.y > region.0[1] && q.y < region.1[1] && q.z > region.0[2] && q.z < region.1[2];
            let many: makepad_fab_slicer::sdf::Field3Many = Arc::new(move |pts: &[[f64; 3]], reach: f64, out: &mut [f64]| {
                let q: Vec<Vec3d> = pts.iter().map(|p| {
                    let q = into(*p);
                    Vec3d::new(q[0], q[1], q[2])
                }).collect();
                if q.iter().all(within) {
                    return chunks.distance_many_bounded(&q, -reach, reach, out, &mut makepad_csg_sdf::Hints::default());
                }
                let (inner, outer): (Vec<usize>, Vec<usize>) = (0..q.len()).partition(|&i| within(&q[i]));
                for (ids, read) in [(inner, &*chunks as &dyn Sdf3), (outer, &*tree)] {
                    let at: Vec<Vec3d> = ids.iter().map(|&i| q[i]).collect();
                    let mut v = vec![0.0; at.len()];
                    read.distance_many_bounded(&at, -reach, reach, &mut v, &mut makepad_csg_sdf::Hints::default());
                    for (&i, v) in ids.iter().zip(v) {
                        out[i] = v;
                    }
                }
            });
            let material = states.clone().map(|states| -> makepad_fab_slicer::sdf_paint::MaterialFn {
                Arc::new(move |x: f64, y: f64, z: f64| {
                    let q = into([x, y, z]);
                    let (_, slot) = makepad_csg_sdf::distance_slot(&node, Vec3d::new(q[0], q[1], q[2]));
                    states.get(slot as usize).copied().unwrap_or(0)
                })
            });
            Slab { field, many: Some(many), material }
        })
    }

    /// The state at a frame point: the shape's material slot there
    /// (`csg_sdf::distance_slot`) through `states`.
    fn material(&self) -> Option<makepad_fab_slicer::sdf_paint::MaterialFn> {
        let states = self.states.clone()?;
        let (shape, m) = (self.shape.clone(), self.to_frame);
        let col = |c: usize| [m[0][c], m[1][c], m[2][c]];
        let (t, cols) = ([m[0][3], m[1][3], m[2][3]], [col(0), col(1), col(2)]);
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        Some(Arc::new(move |x: f64, y: f64, z: f64| {
            let d = [x - t[0], y - t[1], z - t[2]];
            let (_, slot) = makepad_csg_sdf::distance_slot(&shape.root, Vec3d::new(dot(cols[0], d), dot(cols[1], d), dot(cols[2], d)));
            states.get(slot as usize).copied().unwrap_or(0)
        }))
    }

    /// The distance field in the frame's coordinates (negative inside).
    pub fn field(&self) -> Result<Arc<dyn Fn(f64, f64, f64) -> f64 + Send + Sync>, String> {
        let m = self.to_frame;
        let col = |c: usize| [m[0][c], m[1][c], m[2][c]];
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        for i in 0..3 {
            for j in 0..3 {
                let want = if i == j { 1.0 } else { 0.0 };
                if (dot(col(i), col(j)) - want).abs() > 1e-9 {
                    return Err("an SDF part's placement must be rigid (no scale or shear)".into());
                }
            }
        }
        let field = self.shape.field();
        let (t, cols) = ([m[0][3], m[1][3], m[2][3]], [col(0), col(1), col(2)]);
        // Frame → model is the transpose: q = Rᵀ (p - t).
        Ok(Arc::new(move |x: f64, y: f64, z: f64| {
            let d = [x - t[0], y - t[1], z - t[2]];
            field.distance(Vec3d::new(dot(cols[0], d), dot(cols[1], d), dot(cols[2], d)))
        }))
    }
}

impl std::fmt::Debug for PartField {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "PartField({:016x})", self.content)
    }
}

/// A source raised by `lift` mm (a frame's z offset).
pub(crate) struct Lifted {
    pub(crate) src: Arc<dyn ContourSource + Send + Sync>,
    pub(crate) lift: f64,
}

impl ContourSource for Lifted {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        let (mut lo, mut hi) = self.src.bounds();
        lo[2] += self.lift;
        hi[2] += self.lift;
        (lo, hi)
    }
    fn pivot_box(&self) -> ([f64; 2], [f64; 2]) {
        self.src.pivot_box()
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        if self.lift == 0.0 {
            return self.src.contours(zs);
        }
        let zs: Vec<f64> = zs.iter().map(|z| z - self.lift).collect();
        self.src.contours(&zs)
    }
    fn slope_profile(&self, step: f64) -> Option<makepad_fab_slicer::source::SlopeProfile> {
        let mut p = self.src.slope_profile(step)?;
        p.z0 += self.lift;
        for f in &mut p.flats {
            *f += self.lift;
        }
        Some(p)
    }
    fn painted(&self) -> bool {
        self.src.painted()
    }
    fn paint_regions(&self, zs: &[f64], slices: &[Vec<ExPolygon>], p: &PaintParams) -> Vec<LayerPaint> {
        let zs: Vec<f64> = zs.iter().map(|z| z - self.lift).collect();
        self.src.paint_regions(&zs, slices, p)
    }
    fn paint_at(&self) -> Option<PaintAt> {
        let (at, lift) = (self.src.paint_at()?, self.lift);
        Some(if lift == 0.0 { at } else { Arc::new(move |x, y, z| at(x, y, z - lift)) })
    }
}

fn field_source(f: &PartField, lift: f64) -> Result<Arc<dyn ContourSource + Send + Sync>, String> {
    let src = f.source()?;
    Ok(if lift == 0.0 { src } else { Arc::new(Lifted { src, lift }) })
}

/// The non-planar settings of a job (or a part's own): None unless
/// `fab_nonplanar_top` is on. The layer height and the top line width are
/// the job's.
fn np_settings(job: &Job) -> Option<NpSettings> {
    if !job.b("fab_nonplanar_top") {
        return None;
    }
    let d = NpSettings::default();
    let h = job.f("layer_height");
    let lw = job.f("top_surface_line_width");
    Some(makepad_fab_toolpath::nonplanar::settings(job, if h > 0.0 { h } else { d.layer_h }, if lw > 0.0 { lw } else { d.line_w }))
}

/// A key for (settings, head): what a part's non-planar plan depends on
/// beyond its field and lift.
fn np_key(s: &NpSettings, head: &[NpBox]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut eat = |x: u64| h = (h ^ x).wrapping_mul(0x100_0000_01b3);
    eat(s.shells as u64);
    for v in [s.layer_h, s.line_w, s.slope_cap_deg, s.rise_cap, s.rise_half, s.tol, s.t_min, s.cell] {
        eat(v.to_bits());
    }
    for b in head {
        for c in b.name.bytes() {
            eat(c as u64);
        }
        for v in [b.ox, b.oy, b.hx, b.hy, b.hole, b.e, b.margin] {
            eat(v.to_bits());
        }
    }
    h
}

/// A part's non-planar plan in its (lifted) frame: from its field, raised by `lift`.
fn np_plan_of(f: &PartField, lift: f64, s: &NpSettings, head: &[NpBox]) -> Result<NpPlan, String> {
    s.check()?;
    let src = f.source()?;
    let field = f.field()?;
    let (mut lo, mut hi) = src.bounds();
    lo[2] += lift;
    hi[2] += lift;
    let lifted = move |x: f64, y: f64, z: f64| field(x, y, z - lift);
    makepad_fab_slicer::nonplanar::plan(&lifted, (lo, hi), s, head)
}

/// A contour source with a non-planar top plan.
struct WithNp {
    src: Arc<dyn ContourSource + Send + Sync>,
    np: Arc<NpPlan>,
}

impl ContourSource for WithNp {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.src.bounds()
    }
    fn pivot_box(&self) -> ([f64; 2], [f64; 2]) {
        self.src.pivot_box()
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        self.src.contours(zs)
    }
    fn painted(&self) -> bool {
        self.src.painted()
    }
    fn paint_regions(&self, zs: &[f64], slices: &[Vec<ExPolygon>], p: &PaintParams) -> Vec<LayerPaint> {
        self.src.paint_regions(zs, slices, p)
    }
    fn paint_at(&self) -> Option<PaintAt> {
        self.src.paint_at()
    }
    fn np_plan(&self) -> Option<Arc<NpPlan>> {
        Some(self.np.clone())
    }
    fn slope_profile(&self, step: f64) -> Option<makepad_fab_slicer::source::SlopeProfile> {
        self.src.slope_profile(step)
    }
}

impl LiveInput {
    /// Bind the overlap issues of `plate` as its evaluation judged them to that very plate.
    /// `plate_id` is the id they were judged on, the evaluated plate's `plate_id(ev)`.
    /// An input with another id (a swapped field, a moved part) judges its own.
    /// A plate changed afterwards is another allocation: `Arc::make_mut` moves or clones
    /// it, and `Arc::get_mut` refuses while the binding lives. Such a plate judges its own.
    pub fn with_overlaps(mut self, overlaps: Option<Arc<Overlaps>>, plate_id: u64) -> Self {
        self.overlaps = overlaps.map(|overlaps| BoundOverlaps { plate: Arc::downgrade(&self.plate), overlaps, plate_id });
        self
    }

    fn known_overlaps(&self) -> Option<&Arc<Overlaps>> {
        let bound = self.overlaps.as_ref()?;
        (self.pieces.is_none() && self.plate_id == bound.plate_id && std::ptr::eq(bound.plate.as_ptr(), Arc::as_ptr(&self.plate)))
            .then_some(&bound.overlaps)
    }

    /// Place a part in the input itself, so a later settle uses exactly the live plate.
    /// Called while building the input on the engine's thread.
    pub fn place(&mut self, part: usize, xf: Affine) {
        let plate = Arc::make_mut(&mut self.plate);
        let Some(p) = plate.parts.get_mut(part) else { return };
        *p = placed_part(p, &xf);
        let Some(frame) = self.frames.as_mut().and_then(|f| Arc::make_mut(f).get_mut(part)) else { return };
        if let Some((dx, dy)) = xy_only(&xf) {
            frame.offset[0] += dx;
            frame.offset[1] += dy;
            return;
        }
        let map = affine_mul(&xf, &affine_translate(frame.offset));
        let mut local = (*frame.local).clone();
        for q in &mut local.positions {
            *q = std::array::from_fn(|r| map[r][0] * q[0] + map[r][1] * q[1] + map[r][2] * q[2] + map[r][3]);
        }
        frame.local = Arc::new(local);
        frame.field = frame.field.as_ref().and_then(|f| f.moved(&map));
        frame.offset = [0.0; 3];
    }

    /// An input whose setup is already made.
    pub fn ready(plate_id: u64, settings_id: u64, setup: JobSetup, plate: Arc<Plate>, paint: Arc<Vec<Vec<String>>>) -> Self {
        LiveInput { plate_id, settings_id, setup: fixed_setup(setup), plate, overlaps: None, paint, frames: None, machine: Machine::dual_nozzle_350(), np_head: None, pieces: None }
    }
    fn key(&self) -> (u64, u64) {
        (self.plate_id, self.settings_id)
    }
}

/// A part's layers: (print z, height, contours), bottom up.
pub type CutLayers = Vec<(f64, f64, CutLayer)>;

/// A layer's contours, shared by every cache and revision.
#[derive(Clone, Default)]
pub struct CutLayer {
    pub regions: Regions,
}
impl AsRef<[ExPolygon]> for CutLayer {
    fn as_ref(&self) -> &[ExPolygon] { &self.regions }
}

/// One layer's regions. Every cut, band and cache that holds the layer
/// shares them: a cut is a list of pointers, not a copy of the part.
pub type Regions = Arc<[ExPolygon]>;

/// A settled slice: what Print and Export use. Its printer package is made only when it is
/// sent or exported ([`Settled::package_with`]), never by the settle.
pub struct Settled {
    /// Held until the app moves it to its slice cache ([`crate::gcode_text`]).
    pub gcode: crate::gcode_text::GcodeText,
    pub timeline: Timeline,
    /// The same timeline per layer, chunks unchanged since the previous
    /// settle shared with it (scrubbing and playback by G-code time).
    pub layers: makepad_fab_gcode::layers::LayerTimeline,
    pub stats: Stats,
    /// What the checks had found when the G-code was made (the plate's and the contours'): no
    /// figures ([`Report::plate`] none). Its sliced report, on this timeline, streams after it
    /// as [`LiveEvent::Issues`].
    pub report: Report,
    /// The setup it was written with: of only the tools it prints in.
    pub setup: Arc<JobSetup>,
    /// The input's job tool (0-based) each of its tools is: the G-code's tool `k` prints what
    /// the input gave tool `tools[k]`.
    pub tools: Vec<usize>,
    /// Live transforms were applied to the requested plate (a part was moved
    /// after the request): this is the slice of that moved plate, exactly.
    pub transformed: bool,
    /// Exactly what was sliced, per part: its slicing mesh and plate offset
    /// ([`slice_frames`] of these is this result). Empty when it came over a
    /// process link.
    pub sliced: Vec<Frame>,
    /// The parts printed with a non-planar top: (part, its plan in its frame, the frame's
    /// plate offset in mm), for the preview's stats and the planar before/after.
    pub nonplanar: Vec<(usize, Arc<NpPlan>, [f64; 2])>,
    /// The job it was written with, the plate's objects and each layer's filaments: what the
    /// package says besides the G-code.
    pub job: Job,
    pub objects: Vec<makepad_fab_package::PlateObject>,
    pub layer_filaments: Vec<Vec<usize>>,
}

impl Settled {
    /// The printer package (`.gcode.3mf`) of this slice, dated today: the G-code with its MD5,
    /// the plate renders and the archive, a second or more of CPU on big plates. Made when the
    /// slice is sent or exported, on a worker. `progress` gets the share done (0..1); it ends
    /// early, as an error, once `stop` says the package is no longer wanted.
    pub fn package_with(&self, progress: &(dyn Fn(f64) + Sync), stop: &(dyn Fn() -> bool + Sync)) -> Result<Vec<u8>, String> {
        let _t = makepad_fab_toolpath::prof::scope(makepad_fab_toolpath::prof::Stage::Package);
        let moves = self.layers.chunks.iter().map(|c| MovesRef::Packed(&c.moves)).collect();
        let gcode = self.gcode.text()?;
        crate::package_of(&self.job, &gcode, &self.stats, moves, &self.objects, &self.layer_filaments, progress, stop)
    }
}

pub enum LiveEvent {
    /// Part slot `part` holds content `key` now: its contours in its frame.
    /// Sent when a slot's content changes (not when it only moves). The layers near the one
    /// in view come first, then bands down to 0 and up to the top: `stale`
    /// (layer index ranges, end exclusive) are the slot's previous contours meanwhile.
    PartCut { part: usize, key: u64, layers: Arc<CutLayers>, stale: Vec<(u32, u32)> },
    /// Every part's contours are ready: (content key, offset) per part slot.
    /// `age`: seconds from the revision's input to this result.
    Contours { revision: u64, parts: Vec<(u64, Pt)>, objects: Arc<[String]>, age: f64 },
    /// Part slot `part` (content `key`) is being planned: `regions` region
    /// streams (a painted part plans one per filament), `total` layers each.
    /// Its `PartLayers` follow; a layer is complete when every region has it.
    PartRegions { part: usize, key: u64, regions: u32, total: u32 },
    /// Planned layers of one region of part slot `part` (content `key`), in
    /// its frame, as their plan's paths ([`LivePaths`]: preview records, times
    /// from 0 per layer, made as a view reads them): `(layer index, paths)`,
    /// each complete. `revision` and `pass` say which run and pass planned
    /// them (a layer of one content plans the same in each).
    PartLayers { revision: u64, pass: Pass, part: usize, key: u64, region: u32, layers: Vec<(u32, LivePaths)>, total: u32 },
    /// Support records added after all dirty parts' model layers. Model-layer
    /// indices receive support islands; support-only layers have their own
    /// indices after the model layers, in support generation order. `done`: the part's
    /// supports whole, which replace all it had. Else a plan of them is underway: its `layers`
    /// replace those layers' supports, and none at all clear the part's supports (a plan of
    /// them begins on the part's slice as it is).
    PartSupports { part: usize, key: u64, layers: Vec<(u32, LivePaths)>, done: bool },
    /// The trees of the part's supports as they are now ([`TreeShape`]): now and then as the
    /// descent grows them, then as the planner places them, ahead of the supports' lines, and less
    /// those deleted while they grow; none when it has none, or when the supports they stood on
    /// clear.
    PartTrees { part: usize, key: u64, trees: Arc<[TreeShape]> },
    /// How far revision `revision` is, 0..1 over all its stages, and the stage with what its work
    /// is at ("Supports: Bud vase (2 of 3) · growing trees 120/323"): at most every 50 ms, never
    /// backwards. A revision a newer input cuts short tells no more.
    Running { revision: u64, progress: f32, stage: String },
    /// Every part's plan is complete.
    /// `toolpaths`: the merged plan itself (per layer, per part slot
    /// `ObjectLayer::object`: regions and paths), shared, for consumers that
    /// judge the sliced result (joints, gaps).
    Toolpaths { revision: u64, parts: Vec<(u64, Pt)>, age: f64, toolpaths: Arc<PlatePaths> },
    /// The plate's printability so far, in printer coordinates: the mesh and
    /// contour checks first (`done` 0), then the sliced checks layer by
    /// layer until `done == total`.
    Issues { revision: u64, report: Arc<Report>, done: u32, total: u32 },
    /// A settle request's stage.
    Progress { tag: u64, progress: f32, stage: String },
    /// A settle request's exact result.
    Settled { tag: u64, revision: u64, result: Result<Arc<Settled>, String>, seconds: f64 },
    /// The settings changed: every part's layers so far are of other settings (other layer
    /// heights and indices) and go; none of them stands in for a new layer.
    Settings,
    /// The newest input cannot be sliced (no setup, a part on a filament the job lacks).
    Failed { revision: u64, error: String },
    /// A version bump stopped the work in progress: `ms` from the change until
    /// the engine's workers were idle.
    Stopped { ms: f64 },
    /// The driver selected this input.
    Started { revision: u64, plate_id: u64 },
    /// The focus chunk has been published (timings in milliseconds).
    Viewed { revision: u64, part: usize, layer: u32, field_ms: f64, slice_ms: f64, plan_ms: f64 },
    /// The print time of `revision`'s toolpaths ([`LiveEngine::estimate`]): `streamed` once the
    /// time model has run and its records went to the taker ([`LiveEngine::set_records`]); not
    /// when a kept print time goes at once, its records following once the time model has run again.
    Estimated { revision: u64, estimate: Arc<Estimate>, streamed: bool },
}

/// The preview builder of one estimate's records, called on the time model's thread with each
/// part and then [`Streamed::Done`]. It returns a part's buffer when it is done with it and is
/// dropped without `Done` when the estimate stops.
pub type RecordTaker = Box<dyn FnMut(Streamed) -> Option<Vec<MoveRecord>> + Send>;

/// Makes the taker for revision `rev`'s records, given the print time kept for them.
pub type RecordsFn = Arc<dyn Fn(u64, Option<Arc<Estimate>>) -> Option<RecordTaker> + Send + Sync>;

pub enum Streamed {
    /// The time model's next records, in print order on the print's clock, and the entries of
    /// the nozzles' targets made since the part before: the records' `temps` name the entries
    /// so far.
    Part { records: Vec<MoveRecord>, temps: Vec<Vec<i16>> },
    /// The estimate they make.
    Done { estimate: Arc<Estimate> },
}

/// A whole live slice's print time and per-layer times from the time model, with no G-code
/// text ([`makepad_fab_gcode::writer::estimate`]).
pub struct Estimate {
    /// The time model's totals: its records went to the taker ([`LiveEngine::set_records`]).
    pub timeline: Timeline,
    pub stats: Stats,
    /// Its filament changes as the writer plans them, for weighing other nozzles for its
    /// filaments; `None` for a print object by object.
    pub changes: Option<Arc<makepad_fab_gcode::heads::Changes>>,
    /// The input's job tool (0-based) each of its tools is, as in [`Settled::tools`] (its
    /// `changes` are in the input's tools).
    pub tools: Vec<usize>,
    /// The print temperature (°C) of each of its tools' filaments, as `tools` (what the iron
    /// melts heat-set inserts in at after the print).
    pub filament_temps: Vec<f64>,
    /// How long the estimate took.
    pub seconds: f64,
}

pub type Sink = Arc<dyn Fn(LiveEvent) + Send + Sync>;

/// The pass that planned a [`LiveEvent::PartLayers`] layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    /// An unchanged content's whole plan, from the cache.
    Cached,
    /// A field edit's layers its contour change did not reach: the last plan's.
    Kept,
    /// An edited part's viewed layer (drawn even once a newer input came).
    Viewed,
    /// Another part's viewed layer.
    Focus,
    /// The rest, window by window.
    Window,
}

impl Pass {
    pub fn name(self) -> &'static str {
        match self {
            Pass::Cached => "cached",
            Pass::Kept => "kept",
            Pass::Viewed => "viewed",
            Pass::Focus => "focus",
            Pass::Window => "window",
        }
    }
}

struct State {
    revision: u64,
    input: Option<Arc<LiveInput>>,
    /// An input still to be built (on the engine's thread, never the
    /// caller's), and its identity.
    pending: Option<MakeInput>,
    /// The input of `key` as held: its slice is in the caller's hands. Nothing runs until a part
    /// moves on top of it.
    held: Option<MakeInput>,
    key: Option<(u64, u64)>,
    /// Per part: the transform on top of the input's placement.
    xf: Vec<Option<Affine>>,
    /// Transforms are arriving (a grab): contours only until released.
    moving: bool,
    settle: Option<u64>,
    focus_z: Option<f64>,
    enabled: bool,
    checkpoint: Checkpoint,
    running: bool,
    /// Why the slice is to fail, from [`LiveEngine::fail`] while it ran.
    failing: Option<String>,
    /// When the newest input arrived.
    touched: Option<std::time::Instant>,
    /// The revision whose results are all published.
    served: u64,
}

impl Default for State {
    fn default() -> Self {
        State { revision: 0, input: None, pending: None, held: None, key: None, xf: Vec::new(), moving: false, settle: None, focus_z: None, enabled: true, checkpoint: Arc::new(|| false), running: false, failing: None, touched: None, served: 0 }
    }
}

const IDENTITY: Affine = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];

/// `a` after `b`: `p' = a * (b * p)`.
/// The inverse of a rigid or scaled affine (None: singular).
fn affine_invert(m: &Affine) -> Option<Affine> {
    let a = |r: usize, c: usize| m[r][c];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0)) + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let mut r = [[0.0; 4]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let (i1, i2, j1, j2) = ((j + 1) % 3, (j + 2) % 3, (i + 1) % 3, (i + 2) % 3);
            r[i][j] = (a(i1, j1) * a(i2, j2) - a(i1, j2) * a(i2, j1)) / det;
        }
        r[i][3] = -(0..3).map(|j| r[i][j] * m[j][3]).sum::<f64>();
    }
    Some(r)
}

fn affine_mul(a: &Affine, b: &Affine) -> Affine {
    std::array::from_fn(|r| {
        let mut row: [f64; 4] = std::array::from_fn(|c| a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c]);
        row[3] += a[r][3];
        row
    })
}

fn affine_translate(t: [f64; 3]) -> Affine {
    [[1.0, 0.0, 0.0, t[0]], [0.0, 1.0, 0.0, t[1]], [0.0, 0.0, 1.0, t[2]]]
}

/// A transform that only moves in X and Y: the move in mm.
fn xy_only(m: &Affine) -> Option<(f64, f64)> {
    let lin = (0..3).all(|r| (0..3).all(|c| m[r][c].to_bits() == IDENTITY[r][c].to_bits())) && m[2][3] == 0.0;
    lin.then(|| (m[0][3], m[1][3]))
}

/// Per part slicing state, kept across revisions, keyed by the slicing
/// mesh's content and the settings.
#[derive(Default)]
struct Caches {
    /// (local mesh pointer, z lift bits, transform bits) → (local, slicing mesh, its content).
    meshes: Vec<((usize, u64, [u64; 12]), Arc<PartMesh>, Arc<MeshSource>, u64)>,
    /// (content key, settings) → contours in the part's frame.
    cuts: Vec<((u64, u64), Arc<CutLayers>)>,
    /// (content key, settings) → the part's plan in its frame.
    plans: Vec<((u64, u64), Arc<ObjectPlan>)>,
    /// Per part, the warp of its current cuts and grading settings, held between revisions.
    grades: Vec<Option<(u64, Arc<grade::Graded>)>>,
    /// Per part slot whose supports the sink shows as [`PRUNED`]: the plan's settings, and the
    /// layers it was sent less some trees.
    pruned: Vec<Option<(u64, Vec<u32>)>>,
    /// Per part slot whose supports the sink shows as they grow ([`GROWING`]): the content key,
    /// the settings less the trees deleted (as in `trees`), and what the sink shows of them. Once
    /// the sink has their trees (from the descent on), a next plan of the same that deletes trees
    /// only goes on from that, less the trees deleted: it tells only what replaces what is shown
    /// (their tips were kept).
    grown: Vec<Option<(u64, u64, Arc<Mutex<GrowState>>)>>,
    /// (content key, settings) → the trees of that plan's supports, and the settings less the
    /// trees the user deleted on the part ([`part_settings`] without [`DROPPED_TIPS`]).
    trees: Vec<((u64, u64), (Arc<[TreeShape]>, u64))>,
    /// Per part slot with trees: what they were made of in its last finished plan, so the next
    /// (a tree deleted) does only the work the change asks for. It is large (the part's volumes
    /// and its descent): a part without trees keeps none.
    trees_kept: Vec<makepad_fab_toolpath::tree::Cache>,
    /// (content key, settings) → the part's contour checks in its frame.
    checks: Vec<((u64, u64), Arc<Vec<fab_printability::Issue>>)>,
    /// The placed plate's checks, reused when the same live input settles.
    plate_checks: Option<((u64, u64), Vec<(u64, Pt)>, Arc<Report>, Arc<Vec<fab_printability::Issue>>)>,
    /// The last toolpaths and what they were merged from.
    toolpaths: Option<(Vec<(u64, u64, Pt)>, Arc<PlatePaths>)>,
    /// The mesh keys the instance classes were found for, and the classes.
    classes: Option<(Vec<u64>, Vec<(usize, Pt)>)>,
    /// Per part slot: its last field cut (field, lift bits, settings, cut), for an edit of
    /// that field to re-trace only the layers it reaches.
    slot_fields: Vec<Option<(PartField, u64, u64, Arc<CutLayers>)>>,
    /// Per part slot painted by position: the paint of its last cached plan, for a paint edit
    /// to plan again only the layers it reaches.
    slot_paints: Vec<Option<fab_plate::SpatialPaint>>,
    /// Per unpainted mesh part slot: its last mesh cut (mesh, its content, settings, cut), for
    /// an edit of that mesh's vertices to re-cut only the planes it reaches.
    slot_meshes: Vec<Option<(Arc<MeshSource>, u64, u64, Arc<CutLayers>)>>,
    /// (field content, lift bits, settings + head key) → the part's non-planar plan.
    np_plans: Vec<((u64, u64, u64), Arc<NpPlan>)>,
    /// Per part slot: its last cut and the settings it was made with: what its layers still to
    /// trace show meanwhile.
    last_cuts: Vec<Option<(u64, Arc<CutLayers>)>>,
    /// The parts' content keys of the plate as last sliced, and of the plate before it: the
    /// cuts and plans of both stay (a plate switched back to is not sliced again).
    plate_keys: Vec<u64>,
    keys_before: Vec<u64>,
    /// The whole plate's id, content keys and plan settings as last sliced, which a test plate
    /// cut from that very plate keeps with its cuts and plans (switching back slices nothing):
    /// any other plate id lets them go, so no superseded shape is held by them.
    whole_plate: Option<(u64, Vec<u64>, Vec<u64>)>,
    /// (whole part's content key, settings) → what a test plate has cut of that part, by plane:
    /// only those the newest test plate reads.
    piece_planes: Vec<((u64, u64), Arc<crate::piece::Planes>)>,
    /// Per part slot: the content key whose cut / plan the sink was last given, and the plan key
    /// (`plan_key`) whose supports it was last given.
    sent_cut: Vec<u64>,
    sent_plan: Vec<u64>,
    sent_supports: Vec<u64>,
    /// Per part slot: the layers a superseded revision had traced of a content not cut whole
    /// yet (content key, settings, per layer): a later revision of that content resumes them.
    partial_cuts: Vec<Option<(u64, u64, Vec<Option<CutLayer>>)>>,
    /// Per part slot: the model layers a superseded revision had planned of a content not
    /// planned whole yet (content key, settings, per region per layer), which the sink still
    /// shows: a later revision of that content plans only the others.
    partial_plans: Vec<Option<(u64, u64, Assembly)>>,
}

/// One part's planned model layers so far: per region, per layer.
type Assembly = Vec<Vec<Option<(f64, f64, ObjectLayer)>>>;

/// A part's last plan's model layers an edit keeps where it did not reach them: its plan's
/// (their indices in its layers: height-ordered, a painted plan's regions stable at each
/// height), or for a part with supports, which its plan's layers hold, the model layers it was
/// finished from.
enum LastModel {
    Plan(Arc<ObjectPlan>, Vec<usize>),
    Held(Assembly),
}

impl LastModel {
    /// Region `r` of layer `k`, of a plan with `regions` streams.
    fn take(&mut self, k: usize, r: usize, regions: usize) -> (f64, f64, ObjectLayer) {
        match self {
            LastModel::Plan(plan, model) => plan.layers[model[k * regions + r]].clone(),
            LastModel::Held(held) => held[r][k].take().expect("the last plan's model layers are whole"),
        }
    }
}

/// The entries a part's cuts and plans keep besides what the plate uses: steps back within them
/// slice nothing.
const KEPT_STATES: usize = 8;

/// What those may hold beyond what the plate uses (bytes). A plan of a large organic part (660
/// layers) holds 57.6 MB whole, so eight such fit, and with what plans share counted once they
/// hold much less; a bigger plate keeps fewer, and a step back past them slices again.
const KEPT_BYTES: usize = 512 << 20;

/// Keep what the plate uses now (`current`: its content keys) and the plate before it used
/// (`before`: an edit taken back, a plate switched back to) under the settings the plate's parts
/// use now, and besides those the newest [`KEPT_STATES`] entries while they hold no more than
/// [`KEPT_BYTES`] beyond them. `weigh` gives what an entry holds that its set of addresses does
/// not, and adds it: what entries share counts once. The bound only decides what is kept: it
/// lets go of the rest ([`let_go`]).
fn kept_of<T>(v: &[((u64, u64), T)], current: &[u64], before: &[u64], settings: &[u64], weigh: impl Fn(&T, &mut HashSet<usize>) -> usize) -> (Vec<bool>, usize) {
    let other = |e: &((u64, u64), T)| !settings.contains(&e.0 .1) || !(current.contains(&e.0 .0) || before.contains(&e.0 .0));
    let mut seen = HashSet::new();
    for e in v.iter().filter(|e| !other(e)) {
        weigh(&e.1, &mut seen);
    }
    let (mut states, mut bytes) = (0, 0);
    let mut keep: Vec<bool> = v.iter().rev().map(|e| {
        if !other(e) {
            return true;
        }
        states += 1;
        bytes += weigh(&e.1, &mut seen);
        states <= KEPT_STATES && bytes <= KEPT_BYTES
    }).collect();
    keep.reverse();
    (keep, bytes)
}

/// The entries of the cache `v` that `keep` says no to, `keep` being [`kept_of`]'s answer for
/// `weighed` (an earlier copy of `v`, weighed with the caches let go): each is found by its key
/// and its value, so one put in since stays. They come back for the caller to drop once the
/// caches are let go; how many is logged (`what` names them, `bytes` is what was weighed): a step
/// back to one slices it again.
fn let_go_of<U: ?Sized>(v: &mut Vec<((u64, u64), Arc<U>)>, weighed: &[((u64, u64), Arc<U>)], keep: &[bool], what: &str, bytes: usize) -> Vec<((u64, u64), Arc<U>)> {
    let gone: Vec<&((u64, u64), Arc<U>)> = weighed.iter().zip(keep).filter(|(_, k)| !**k).map(|(e, _)| e).collect();
    if gone.is_empty() {
        return Vec::new();
    }
    eprintln!("[fab] let go of {} older {what} (past {KEPT_STATES} kept or {} MiB beyond the plate's, {:.0} MiB weighed); a step back to one slices it again", gone.len(), KEPT_BYTES >> 20, bytes as f64 / 1048576.0);
    let (out, kept) = std::mem::take(v).into_iter().partition(|e| gone.iter().any(|g| g.0 == e.0 && Arc::ptr_eq(&g.1, &e.1)));
    *v = kept;
    out
}

/// What serves with a plan goes with it when it is let go: its trees, and the print times of
/// the revisions it was a part of. Under the caches (`c`), as every writer of the print times
/// is. What goes comes back for the caller to drop once the caches are let go.
fn follow_plans(c: &mut Caches, estimates: &Snap<Estimates>) -> FollowedPlans {
    let plans = &c.plans;
    let (trees, gone_trees) = std::mem::take(&mut c.trees).into_iter().partition(|t| plans.iter().any(|p| p.0 == t.0));
    c.trees = trees;
    let all = estimates.get();
    let kept: Vec<_> = all.iter().filter(|e| e.0 .0.iter().all(|&(key, settings, _)| plans.iter().any(|p| p.0 == (key, settings)))).cloned().collect();
    let gone_estimates = (kept.len() < all.len()).then(|| estimates.replace(kept.into()));
    (gone_trees, gone_estimates)
}

/// The trees and print times [`follow_plans`] let go of.
type FollowedPlans = (Vec<((u64, u64), (Arc<[TreeShape]>, u64))>, Option<Arc<Estimates>>);

/// A value its readers take whole, by an Arc clone. Its lock is held for that clone or for a
/// writer's swap and for nothing else, so what a writer does around it (another lock held, work,
/// a first call the JIT compiles) never reaches a reader that waits for it. The UI reads the
/// sent toolpaths, the print times kept for them and the code a live patch is checked against
/// through one.
struct Snap<T: ?Sized>(Mutex<Arc<T>>);

impl<T: ?Sized> Snap<T> {
    fn new(value: Arc<T>) -> Snap<T> {
        Snap(Mutex::new(value))
    }

    fn get(&self) -> Arc<T> {
        self.0.lock().unwrap().clone()
    }

    /// Puts `value` in. The value it replaces comes back, to drop with the caller's locks let go.
    fn replace(&self, value: Arc<T>) -> Arc<T> {
        std::mem::replace(&mut *self.0.lock().unwrap(), value)
    }
}

/// What a whole revision's print time is kept under: what its toolpaths were merged from (per
/// part its content key, plan settings and place) and its input's key, which its job and the
/// tools it binds are made by.
type EstimateKey = (Vec<(u64, u64, Pt)>, (u64, u64));

/// The kept print times, by what they are kept under, the plate's last ([`Shared::estimates`]).
type Estimates = [(EstimateKey, Arc<Estimate>)];

/// What revision `rev`'s toolpaths, as sent, have their print time kept under; `None` once
/// newer ones were sent.
fn sent_key(shared: &Shared, rev: u64) -> Option<EstimateKey> {
    (*shared.sent_toolpaths.get()).as_ref().filter(|t| t.revision == rev).map(|t| (t.merged_from.clone(), t.key))
}

/// The print time kept under `key`.
fn kept_estimate(shared: &Shared, key: &EstimateKey) -> Option<Arc<Estimate>> {
    shared.estimates.get().iter().find(|e| e.0 == *key).map(|e| e.1.clone())
}

/// Keeps `estimate` (made under `key`) as the plate's print time, and the one that was the
/// plate's as the plate's before: the two newest stay. Under the caches (`_held`), as every
/// writer of the print times is: one writer's swap never loses another's. The list it replaces
/// comes back, to drop once the caches are let go.
fn keep_estimate(_held: &Caches, estimates: &Snap<Estimates>, key: EstimateKey, estimate: Arc<Estimate>) -> Arc<Estimates> {
    let mut kept: Vec<_> = estimates.get().iter().filter(|e| e.0 != key).cloned().collect();
    kept.push((key, estimate));
    bound(&mut kept, 2);
    estimates.replace(kept.into())
}

/// Keep the newest `n` entries of a cache (newest last).
fn bound<T>(v: &mut Vec<T>, n: usize) {
    if v.len() > n {
        v.drain(..v.len() - n);
    }
}

/// A field part's content key: its field (`content`), filament, lift, non-planar plan and print
/// overrides.
fn field_key(content: u64, filament: usize, lift: f64, np_k: u64, print: &[(String, String)]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for x in [0x5df1_e1d0u64, content, filament as u64, lift.to_bits(), np_k] {
        h = (h ^ x).wrapping_mul(0x100_0000_01b3);
    }
    with_overrides(h, print)
}

/// A part's content key with its per-part print overrides: a part with its
/// own settings never shares another's cut or plan. Its own support settings are not its slice's:
/// they key its supports alone ([`part_settings`]).
fn with_overrides(key: u64, print: &[(String, String)]) -> u64 {
    let mut h = key;
    for (k, v) in print.iter().filter(|(k, _)| !is_support_key(k)) {
        for b in k.bytes().chain([0u8]).chain(v.bytes()).chain([1u8]) {
            h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

/// The job keys that say only which nozzle each filament prints on. The writer reads them; the
/// planner reads a filament's nozzle only through its [`Params`].
const NOZZLE_MAP_KEYS: [&str; 3] = ["filament_map", "filament_map_2", "filament_nozzle_map"];

/// The code a slice's results come from, by what each part of it makes. Under the JIT a live
/// patch moves the parts whose sources it changed ([`loaded_code`]), and every result is kept
/// under the code that made it: a patch of the support code plans the supports again on the
/// parts' slices as they are, as a support setting does ([`paths_keys`]); one of the slicing code
/// slices again; one of the checks or the G-code writer makes those again from the plans.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct LoadedCode {
    /// What makes the cuts and the model layers, and so all made of them: the slicer, the
    /// toolpath crate but for its supports, the geometry, the settings and this crate.
    pub part: u64,
    /// What makes the supports: the toolpath crate's support modules.
    pub supports: u64,
    /// What is made of the plans: the checks and the G-code writer.
    pub output: u64,
}

/// What a source's code makes ([`LoadedCode`]).
#[cfg(relax)]
#[derive(Clone, Copy)]
enum Makes {
    Part,
    Supports,
    Output,
}

/// The sources a slice runs, as this crate's manifest finds them, and what each makes: the first
/// that holds a file says. The toolpath crate's support modules make the supports alone, the rest
/// of it the parts.
#[cfg(relax)]
const SOURCES: &[(&str, Makes)] = &[
    ("../toolpath/src/tree", Makes::Supports),
    ("../toolpath/src/coral", Makes::Supports),
    ("../toolpath/src/support.rs", Makes::Supports),
    ("../toolpath/src/support_paint.rs", Makes::Supports),
    ("../toolpath/src/support_rules.rs", Makes::Supports),
    ("../toolpath/src/normal_support.rs", Makes::Supports),
    ("../toolpath/src/skeleton.rs", Makes::Supports),
    ("../toolpath/src/witness.rs", Makes::Supports),
    ("../toolpath/src/footing.rs", Makes::Supports),
    ("", Makes::Part),
    ("../toolpath", Makes::Part),
    ("../slicer", Makes::Part),
    ("../settings", Makes::Part),
    ("../plate", Makes::Part),
    ("../colour", Makes::Part),
    ("../../../../libs/csg/csg_sdf", Makes::Part),
    ("../../../../libs/csg/csg_math", Makes::Part),
    ("../gcode", Makes::Output),
    ("../printability", Makes::Output),
];

/// `rel` from this crate's folder, its `..` taken out by name.
#[cfg(relax)]
fn crate_path(rel: &str) -> std::path::PathBuf {
    use std::path::{Component, Path, PathBuf};
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel).components().fold(PathBuf::new(), |mut p, c| {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                p.pop();
            }
            c => p.push(c),
        }
        p
    })
}

/// What the code of source `file` makes, by the first of [`SOURCES`] that holds it, as named or
/// as the file system resolves it.
#[cfg(relax)]
fn makes(file: &str) -> Option<Makes> {
    static AT: std::sync::OnceLock<Vec<(Vec<std::path::PathBuf>, Makes)>> = std::sync::OnceLock::new();
    let at = AT.get_or_init(|| {
        SOURCES.iter().map(|&(rel, makes)| {
            let named = crate_path(rel);
            let real = std::fs::canonicalize(&named).ok().filter(|r| *r != named);
            (std::iter::once(named).chain(real).collect(), makes)
        }).collect()
    });
    let file = std::path::Path::new(file);
    at.iter().find(|(dirs, _)| dirs.iter().any(|d| file.starts_with(d))).map(|e| e.1)
}

/// The code loaded now ([`LoadedCode`]): under the JIT, each part is the count of the newest live
/// patch that changed a source of it ([`std::relax::patched`]), 0 while none did since this run
/// first asked; outside the JIT always 0.
pub fn loaded_code() -> LoadedCode {
    #[cfg(relax)]
    {
        static LOADED: Mutex<Option<(u64, LoadedCode)>> = Mutex::new(None);
        let mut loaded = LOADED.lock().unwrap();
        // What was patched before the first ask made nothing this run holds.
        let (seen, code) = loaded.get_or_insert_with(|| {
            let mut seen = 0;
            std::relax::patched(&mut seen);
            (seen, LoadedCode::default())
        });
        for file in std::relax::patched(seen).files {
            match makes(&file) {
                Some(Makes::Part) => code.part = *seen,
                Some(Makes::Supports) => code.supports = *seen,
                Some(Makes::Output) => code.output = *seen,
                None => {}
            }
        }
        *code
    }
    #[cfg(not(relax))]
    {
        LoadedCode::default()
    }
}

/// `key` made by the code `by` moved to ([`LoadedCode`]): `key` itself while that code is the
/// run's first.
fn with_code(key: u64, by: u64) -> u64 {
    if by == 0 {
        key
    } else {
        (key ^ by.rotate_left(29) ^ 0x10ad_ed00).wrapping_mul(0x100_0000_01b3)
    }
}

/// What the parts' slices and support plans are made from: the keys the caches keep them under,
/// each from what its stage reads alone.
///
/// `part` keys a part's cut and model layers: the job without its filament-to-nozzle map and
/// without the support keys ([`is_support_key`]), plus each filament's planning parameters on
/// its nozzle (diameter, speeds, layer limits) as the slice reads them
/// ([`Params::without_supports`]), also under every part's own settings. A filament moved to a
/// nozzle like its own plans the same toolpaths, which only the writer prints differently.
/// Another nozzle (another diameter, variant or speeds) plans anew.
///
/// `supports` keys what the parts' supports read beyond their slices: the job's support keys
/// (each part's own come on top, [`part_settings`]). A part's plan (its layers with its
/// supports) is kept under both ([`plan_key`]): a support setting plans the supports again on the
/// part's slice as it is, and never its layers; a part's own plans its supports alone again.
///
/// Each is also made by the code that makes its stage (`code`, [`LoadedCode`]): a live patch of
/// the support code plans the supports again as a support setting does, one of the slicing code
/// slices again.
struct PathsKeys {
    part: u64,
    supports: u64,
}

fn paths_keys(job: &Job, plate: &Plate, code: LoadedCode) -> PathsKeys {
    let fnv = || 0xcbf2_9ce4_8422_2325u64;
    let eat = |h: &mut u64, s: &str| {
        for b in s.bytes().chain([0u8]) {
            *h = (*h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
    };
    let (mut part, mut supports) = (fnv(), fnv());
    for (k, v) in job.cfg.map.iter().filter(|(k, _)| !NOZZLE_MAP_KEYS.contains(&k.as_str())) {
        let h = if is_support_key(k) { &mut supports } else { &mut part };
        eat(h, k);
        match v {
            makepad_fab_settings::Value::Scalar(s) => eat(h, s),
            makepad_fab_settings::Value::Vector(items) => items.iter().for_each(|s| eat(h, s)),
        }
        eat(h, "\u{1}");
    }
    // Each part's own settings as its slice reads them.
    let mut own: Vec<Vec<(String, String)>> = vec![Vec::new()];
    for p in &plate.parts {
        let print: Vec<(String, String)> = p.print.iter().filter(|(k, _)| !is_support_key(k)).cloned().collect();
        if !own.contains(&print) {
            own.push(print);
        }
    }
    for print in &own {
        let overridden;
        let job = if print.is_empty() { job } else { overridden = job.with_overrides(print); &overridden };
        for f in 0..job.filaments() {
            eat(&mut part, &format!("{:?}", Params::from_job(job, f).without_supports()));
        }
    }
    PathsKeys { part: with_code(part, code.part), supports: with_code(supports, code.supports) }
}

/// The key a part's plan (its layers and its supports) is kept under: its slice's and its
/// supports'.
fn plan_key(k: &PathsKeys) -> u64 {
    (k.part ^ k.supports.rotate_left(29)).wrapping_mul(0x100_0000_01b3)
}

/// The plan key `settings` ([`plan_key`]) of a part with its own support settings in `print`
/// and its no-trees areas `no_trees` ([`no_trees_key`]): one part's own (the trees the user
/// deleted on it among them) plans that part's supports again, and no other's.
fn part_settings(settings: u64, print: &[(String, String)], no_trees: u64) -> u64 {
    let mut h = settings ^ no_trees;
    for (k, v) in print.iter().filter(|(k, _)| is_support_key(k)) {
        for b in k.bytes().chain([0u8]).chain(v.bytes()).chain([2u8]) {
            h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

/// What a part's no-trees areas are (`m.no_trees`), which its supports alone read: 0 for none.
fn no_trees_key(p: &fab_plate::Part) -> u64 {
    p.paint.as_ref().map_or(0, |sp| sp.no_trees_key)
}

/// What the sink shows of every part is made under: the part key with the filament-to-nozzle
/// map, which the shown records carry. When it changes the sink drops all it shows; a support
/// setting leaves it as it is.
fn shown_key(job: &Job, part: u64) -> u64 {
    let mut h = part;
    for v in NOZZLE_MAP_KEYS.iter().flat_map(|k| job.cfg.get(k).map(|v| v.as_vec()).unwrap_or_default()) {
        for b in v.bytes().chain([0u8]) {
            h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

/// A mesh part's content key: its slicing mesh's content (`content`, [`hash_mesh`]) and the
/// filament it prints with. The mesh cache keeps only the content: a part that takes another
/// tool (its colour mapped anew, the same mesh) plans with that tool.
fn mesh_key(content: u64, filament: usize) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for x in [0x3e5c_a11bu64, content, filament as u64] {
        h = (h ^ x).wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// What a part painted by position plans with: its paint (equal [`fab_plate::SpatialPaint::key`]s
/// paint the same slots) and the tool each slot's colour prints with (a colour mapped anew paints
/// the same slots on another tool).
fn paint_key(sp: &fab_plate::SpatialPaint) -> u64 {
    sp.colors.iter().fold(sp.key, |h, c| (h ^ c.filament.map_or(0, |f| f as u64 + 1)).wrapping_mul(0x100_0000_01b3))
}

/// `src` with `items` printed into it, each carried by `to_slot` from the evaluated plate into
/// the source's frame, and the key it plans under: `key` with the items as carried, their
/// clearances and the part's thickest layer `thickest` (mm). The carried points are on the
/// slicer's nanometre grid: a host and its item moved together land on the same points whatever
/// the rounding of their move, so the host keeps its cut and plan.
pub(crate) fn embedded(src: Arc<dyn ContourSource + Send + Sync>, key: u64, to_slot: &Affine, items: &[(&Part, &fab_plate::Insert)], thickest: f64) -> (Arc<dyn ContourSource + Send + Sync>, u64) {
    let mut key = (key ^ 0x0e3b_edd0_u64 ^ thickest.to_bits()).wrapping_mul(0x100_0000_01b3);
    let grid = makepad_fab_slicer::geom::SCALE;
    let items = items.iter().map(|(part, insert)| {
        let verts = part.mesh.positions.iter().map(|q| std::array::from_fn(|r| ((to_slot[r][0] * q[0] + to_slot[r][1] * q[1] + to_slot[r][2] * q[2] + to_slot[r][3]) * grid).round() / grid)).collect();
        let mesh = Mesh { verts, tris: part.mesh.triangles.clone(), paint: Vec::new() };
        key = (key ^ hash_mesh(&mesh) ^ insert.clearance.to_bits().rotate_left(17)).wrapping_mul(0x100_0000_01b3);
        makepad_fab_slicer::embed::Item { name: part.name.clone(), mesh, clearance: insert.clearance }
    }).collect();
    (Arc::new(makepad_fab_slicer::embed::Embedded::new(src, items, thickest)), key)
}

/// A slicing mesh's content: its vertices, triangles and per-triangle paint.
fn hash_mesh(m: &Mesh) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut eat = |x: u64| h = (h ^ x).wrapping_mul(0x100_0000_01b3);
    eat(m.verts.len() as u64);
    for v in &m.verts {
        for c in v {
            eat(c.to_bits());
        }
    }
    for t in &m.tris {
        eat(((t[0] as u64) << 32) | t[1] as u64);
        eat(t[2] as u64);
    }
    for p in &m.paint {
        for b in p.bytes() {
            eat(b as u64);
        }
        eat(0xff);
    }
    h
}

/// Asked while a slice runs: `Some(why)` stops it and fails it with that message.
pub type MemoryGuard = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Maximum layers per band on each side of the layer in view. Bands double up to this size,
/// then keep streaming at this size until the whole part is traced and planned.
const MAX_BAND_LAYERS: usize = 64;

/// How often a running slice asks the [`MemoryGuard`].
const GUARD_MS: u64 = 100;

#[derive(Default)]
struct PartWork {
    traced: AtomicU64,
    planned: AtomicU64,
    plan_ns: AtomicU64,
    trees: Mutex<makepad_fab_toolpath::tree::Work>,
    /// Model layers (each region once) kept from the last plan, and why the newest edit of the
    /// part kept none (empty: it kept some, or no edit asked).
    kept: AtomicU64,
    kept_none: Mutex<String>,
}

/// A plate's whole toolpaths: every part's plan placed on the plate, a second copy of them
/// (gigabytes on a big plate). Made when something reads them, gone when the last reader is done.
pub struct PlatePaths {
    plans: Vec<(Arc<ObjectPlan>, Pt)>,
    names: Vec<String>,
    merged: Mutex<std::sync::Weak<Toolpaths>>,
}

impl PlatePaths {
    /// Each part's plan and where it sits on the plate, in the plate's part order.
    pub fn plans(&self) -> &[(Arc<ObjectPlan>, Pt)] {
        &self.plans
    }

    /// Each part's supports as solid geometry and where its plan sits.
    pub fn skeletons(&self) -> Vec<(Arc<makepad_fab_toolpath::skeleton::Skeleton>, Pt)> {
        self.plans.iter().map(|(plan, at)| (plan.skeleton.shape(), *at)).collect()
    }

    /// Each part's volume (mm³) as its layers print it, supports left out, in the plate's part
    /// order: a glued group's first part has its union's, what it fills once however its parts
    /// overlap, and its other parts none.
    pub fn volumes(&self) -> Vec<f64> {
        self.plans.iter().map(|(plan, _)| plan.layers.iter().map(|(_, height, l)| crate::glue::layer_volume(*height, l)).sum()).collect()
    }

    /// The toolpaths, made now unless another reader still has them.
    pub fn read(&self) -> Arc<Toolpaths> {
        let mut merged = self.merged.lock().unwrap();
        if let Some(toolpaths) = merged.upgrade() {
            return toolpaths;
        }
        let toolpaths = Arc::new(makepad_fab_toolpath::par::plate_span("merge", || makepad_fab_toolpath::merge_named(self.plans.clone(), &self.names)));
        *merged = Arc::downgrade(&toolpaths);
        toolpaths
    }
}

/// A revision's toolpaths as they were sent, with their job and their input's key and setup
/// (what binds their tools), and what they were merged from (with the key, what their print
/// time is kept under).
struct SentPaths {
    revision: u64,
    toolpaths: Arc<PlatePaths>,
    job: Arc<Job>,
    key: (u64, u64),
    setup: SetupFn,
    merged_from: Vec<(u64, u64, Pt)>,
}

struct Shared {
    drive: Mutex<live_trace::Drive>,
    state: Mutex<State>,
    /// How many times the run state changed (an input's revision, its serving, whether a driver
    /// runs; a driver's start, polls and end), its waiters woken on each ([`Shared::wait_for`]).
    changes: Mutex<u64>,
    changed: Condvar,
    next: Next,
    spawn: Spawn,
    sink: Sink,
    caches: Mutex<Caches>,
    /// Parts planned (walls to infill) so far: what reuse saves shows here.
    planned: AtomicU64,
    /// Planes of whole parts cut for a test plate's pieces so far ([`crate::piece::Planes`]).
    planes_cut: Arc<AtomicU64>,
    /// Layers whose contours were traced (or cut) again.
    traced: AtomicU64,
    /// The settings id of the last revision run (0: none yet).
    settings: AtomicU64,
    /// Per part slot: layers traced, model layers planned per region, and the
    /// nanoseconds its windows took to plan.
    part_work: Mutex<Vec<Arc<PartWork>>>,
    /// The last settle's per-layer timeline, for the next to share chunks with.
    last_layers: Mutex<Option<makepad_fab_gcode::layers::LayerTimeline>>,
    /// The newest revision whose toolpaths were sent: what [`LiveEngine::estimate`] times.
    sent_toolpaths: Snap<Option<SentPaths>>,
    /// The print times of the plate's whole revision and of the plate's before, by what their
    /// toolpaths were merged from and their input's key, the plate's last: a step back or forth
    /// between the two has its print time with no time model run. They go with their plans
    /// ([`follow_plans`]) and weigh nothing in the plans' bound: the plate's shares its records
    /// with the preview shown, the plate's before shares those that are alike. Its writers hold
    /// the caches; the UI reads it without them ([`LiveEngine::estimate_kept`]).
    estimates: Snap<Estimates>,
    /// The code the newest revision runs under: what the caches keep by its input alone (the
    /// meshes, the instance classes, the non-planar plans, the trees' stages, the checks) and the
    /// print times were made by it. The cuts and plans are kept under it ([`paths_keys`]).
    /// Written under the caches ([`Shared::run_under`]); the UI reads it without them
    /// ([`LiveEngine::code_patched`]).
    code: Snap<LoadedCode>,
    /// Where background work runs: estimates and the printability checks ([`LiveEngine::set_background`]).
    background: Mutex<Option<Spawn>>,
    /// Makes the taker for each estimate's records ([`LiveEngine::set_records`]).
    records: Mutex<Option<RecordsFn>>,
    /// The background checks queued and not begun yet ([`in_background`]), after the number the
    /// last one queued was given.
    queued: Mutex<(u64, Vec<Queued>)>,
    /// The sliced checks' findings per part layer, reused while their key holds.
    slice_memo: Arc<fab_printability::SliceMemo>,
    /// The setup made for an input key.
    setups: Mutex<Option<((u64, u64), Arc<JobSetup>)>>,
    /// The setup and job of an input key's tools a slice printed in.
    bound: Mutex<Option<((u64, u64), Vec<usize>, Arc<JobSetup>, Arc<Job>)>>,
    /// Says why a slice must stop for memory ([`LiveEngine::set_memory_guard`]).
    memory_guard: Mutex<Option<MemoryGuard>>,
    /// When the guard was last asked (ms from `started`), and what it said.
    guard_at: AtomicU64,
    started: std::time::Instant,
    /// Why the guard stopped the running slice.
    over_memory: Mutex<Option<String>>,
    /// The running slice is to fail: it stops at its next checkpoint and fails when it
    /// ends ([`over_memory`], [`LiveEngine::fail`]).
    failing: std::sync::atomic::AtomicBool,
    /// Per part slot: the (content key, settings) the last revision started with; a part whose
    /// pair differs is edited, and its viewed layer is drawn even once a newer input came.
    run_keys: Mutex<Vec<(u64, u64)>>,
    /// Per part slot: the content key of the part events the sink was last given (u64::MAX:
    /// none, or forgotten).
    shown: Arc<Mutex<Vec<u64>>>,
    #[cfg(test)]
    panic_part: Mutex<Option<usize>>,
    /// Runs once, in the next touch between its new revision and its look for a driver to stop.
    #[cfg(test)]
    touched: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Shared {
    /// The run state changed: every [`Self::wait_for`] looks again.
    fn note_change(&self) {
        *self.changes.lock().unwrap() += 1;
        self.changed.notify_all();
    }

    /// Blocks until `done` (a read of the run state) holds: looked at at once, then after each
    /// change of the run state, never by the clock.
    fn wait_for(&self, mut done: impl FnMut() -> bool) {
        loop {
            let seen = *self.changes.lock().unwrap();
            if done() {
                return;
            }
            let mut now = self.changes.lock().unwrap();
            while *now == seen {
                now = self.changed.wait(now).unwrap();
            }
        }
    }

    /// A revision runs under `code` ([`loaded_code`]): what is kept by its input alone and was
    /// made by code that moved since goes. Revisions run one at a time: nothing an older one makes
    /// comes in after this.
    fn run_under(&self, code: LoadedCode) {
        let mut c = self.caches.lock().unwrap();
        let was = *self.code.replace(Arc::new(code));
        // What the new code invalidates is let go of with the caches let go.
        let part = (was.part != code.part).then(|| (std::mem::take(&mut c.meshes), c.classes.take(), std::mem::take(&mut c.np_plans)));
        let supports = ((was.part, was.supports) != (code.part, code.supports)).then(|| std::mem::take(&mut c.trees_kept));
        let output = ((was.part, was.output) != (code.part, code.output)).then(|| (std::mem::take(&mut c.checks), c.plate_checks.take(), self.estimates.replace(Arc::new([]))));
        // Under the caches: no run sees the new code with the old setups.
        let setups = part.is_some().then(|| self.setups.lock().unwrap().take());
        drop(c);
        drop((part, supports, output, setups));
    }
}

/// A revision's stages: model layers with their contours, supports, and a settle's G-code. Its
/// progress counts each stage's work in units of the first's (a layer traced or planned), each
/// stage's units weighed by what they cost.
const STAGES: usize = 3;
const STAGE_NAMES: [&str; STAGES] = ["Layer contours, walls and infill", "Supports", "G-code and print time"];

/// What a part's supports cost per layer, in units of a layer traced or planned, and the share
/// of that its planner takes (the records and the finished plan take the rest): measured on the
/// demo plate's parts with tree supports (the tree_shares measurement, now in history).
const SUPPORT_LAYER_UNITS: f64 = 8.6;
const SUPPORT_PLAN_SHARE: f64 = 0.96;

/// A revision's progress, never backwards, at most every 50 ms: to the views
/// ([`LiveEvent::Running`]) and, for a settle, to its asker ([`LiveEvent::Progress`]).
struct Meter<'a> {
    sink: &'a (dyn Fn(LiveEvent) + Send + Sync),
    tag: Option<u64>,
    revision: u64,
    state: Mutex<MeterState>,
    /// A thread is telling the sink the newest progress: no other does meanwhile, so what goes out
    /// never goes backwards, and the sink runs with no lock held ([`Meter::tell`]).
    telling: AtomicBool,
}

struct MeterState {
    stage: usize,
    units: [f64; STAGES],
    done: f64,
    /// Of the work underway in the stage, the units done, as that work tells it.
    within: f64,
    /// The part whose work it is (by name and place among the stage's), and what that work
    /// tells it is at: told after the stage's name.
    part: Option<String>,
    at: Option<String>,
    sent: (Option<std::time::Instant>, f32),
    /// The newest progress not told yet, and its stage's words.
    due: Option<(f32, String)>,
}

impl<'a> Meter<'a> {
    fn new(shared: &'a Shared, tag: Option<u64>, revision: u64) -> Self {
        Meter { sink: &*shared.sink, tag, revision, state: Mutex::new(MeterState { stage: 0, units: [0.0; STAGES], done: 0.0, within: 0.0, part: None, at: None, sent: (None, 0.0), due: None }), telling: AtomicBool::new(false) }
    }

    /// Stage `k` begins with `units` of work (later stages: their expected units).
    fn stage(&self, k: usize, units: &[(usize, f64)]) {
        let due = {
            let mut s = self.state.lock().unwrap();
            for &(j, u) in units {
                s.units[j] = u;
            }
            s.stage = k;
            s.done = 0.0;
            s.within = 0.0;
            s.part = None;
            s.at = None;
            self.note(&mut s, true)
        };
        if due { self.tell(); }
    }

    /// More work in the current stage than it began with.
    fn more(&self, units: f64) {
        let mut s = self.state.lock().unwrap();
        let k = s.stage;
        s.units[k] += units;
    }

    /// `units` of the current stage done.
    fn done(&self, units: f64) {
        let due = {
            let mut s = self.state.lock().unwrap();
            s.done += units;
            s.within = 0.0;
            self.note(&mut s, false)
        };
        if due { self.tell(); }
    }

    /// The current part's work is done, `units` of the stage: neither the part nor what its work
    /// was at is told again, so nothing of it follows what went out of it.
    fn part_done(&self, units: f64) {
        let due = {
            let mut s = self.state.lock().unwrap();
            s.done += units;
            s.within = 0.0;
            s.part = None;
            s.at = None;
            self.note(&mut s, false)
        };
        if due { self.tell(); }
    }

    /// The stage's work is now part `name`'s, `place` among the parts it has work for (1-based):
    /// its first word is told at once.
    fn part(&self, name: &str, place: (u32, u32)) {
        let mut s = self.state.lock().unwrap();
        s.part = Some(if place.1 > 1 { format!("{name} ({} of {})", place.0, place.1) } else { name.to_string() });
        s.at = None;
        s.sent.0 = None;
    }

    /// The work underway in the current stage, `units` of it, is at `what` (step `step` of its
    /// count), `fraction` of it done (see `par::Ctl::phase`).
    fn phase(&self, units: f64, what: &str, step: Option<(u32, u32)>, fraction: f64) {
        let at = match step {
            Some((i, n)) => format!("{what} {i}/{n}"),
            None => what.to_string(),
        };
        let due = {
            let mut s = self.state.lock().unwrap();
            s.within = s.within.max(fraction.clamp(0.0, 1.0) * units);
            s.at = Some(at);
            self.note(&mut s, false)
        };
        if due { self.tell(); }
    }

    /// The current stage's own fraction (a stage that reports its own).
    fn fraction(&self, f: f64) {
        let due = {
            let mut s = self.state.lock().unwrap();
            let k = s.stage;
            s.done = f.clamp(0.0, 1.0) * s.units[k];
            self.note(&mut s, false)
        };
        if due { self.tell(); }
    }

    fn value(&self, s: &MeterState) -> f32 {
        let w = s.units;
        let all: f64 = w.iter().sum();
        if all <= 0.0 {
            return 0.0;
        }
        let before: f64 = w[..s.stage].iter().sum();
        let within = if s.units[s.stage] > 0.0 { ((s.done + s.within) / s.units[s.stage]).min(0.995) } else { 0.0 };
        ((before + within * w[s.stage]) / all) as f32
    }

    /// The progress is due to be told (always when `force`, else once 50 ms passed since the
    /// last): it waits in `due` for [`Self::tell`]. True when it does.
    fn note(&self, s: &mut MeterState, force: bool) -> bool {
        let v = self.value(s).max(s.sent.1);
        let now = std::time::Instant::now();
        if !force && s.sent.0.is_some_and(|t| (now - t).as_secs_f64() < 0.05) {
            return false;
        }
        s.sent = (Some(now), v);
        let name = STAGE_NAMES[s.stage];
        let stage = match (&s.part, &s.at) {
            (Some(part), Some(at)) => format!("{name}: {part} · {at}"),
            (Some(part), None) => format!("{name}: {part}"),
            (None, Some(at)) => format!("{name}: {at}"),
            (None, None) => name.to_string(),
        };
        s.due = Some((v, stage));
        true
    }

    /// The progress due goes to the sink, the newest only, by one thread at a time and with no
    /// lock held: a thread that finds another telling leaves its progress to that one, which looks
    /// again once it let go.
    fn tell(&self) {
        while let Some(telling) = claim(&self.telling) {
            let due = self.state.lock().unwrap().due.take();
            if let Some((progress, stage)) = due {
                if let Some(tag) = self.tag {
                    (self.sink)(LiveEvent::Progress { tag, progress, stage: stage.clone() });
                }
                (self.sink)(LiveEvent::Running { revision: self.revision, progress, stage });
            }
            drop(telling);
            if self.state.lock().unwrap().due.is_none() {
                return;
            }
        }
    }

    fn finish(&self) {
        if let Some(tag) = self.tag {
            (self.sink)(LiveEvent::Progress { tag, progress: 1.0, stage: "Sliced".to_string() });
        }
    }
}

/// What a part's tree planner has made so far, as the view shows it while the trees grow
/// ([`LiveEvent::PartSupports`] with `done` false): per layer, the overhang faces, the tips and
/// roofs, and the branches as rings, until the layer's lines replace them; and the trees as the
/// descent has them, then as placed, for a branch to be picked meanwhile. Handed to the sink at
/// most every 50 ms, from the planner's threads; a flush underway is never waited for. Only the
/// plan's threads take its locks, each briefly: what a tell shows is built before it takes the
/// state's, and what a flush sends after it lets go.
struct Growing<'a> {
    shared: &'a Shared,
    part: usize,
    key: u64,
    filament: usize,
    heights: &'a [(f64, f64)],
    job: &'a Job,
    /// What the sink shows of it: a plan after it that only deletes trees goes on from it
    /// ([`Caches::grown`]).
    state: Arc<Mutex<GrowState>>,
    /// A flush runs ([`claim`]): another skips, leaving its layers dirty for the next.
    flushing: AtomicBool,
    /// The trees as the planner placed them, once it has.
    trees: Mutex<Option<Arc<[TreeShape]>>>,
}

/// One tree of a part's supports, as the view finds, shows and hides it: in the plan's frame.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeShape {
    /// The tips it grew from (mm; z the print z of the tip's layer): what deleting it marks.
    pub tips: Vec<[f64; 3]>,
    /// Each tip's disc, into `discs`.
    pub tip_discs: Vec<u32>,
    /// Its branch on each layer it stands on.
    pub discs: Vec<TreeDisc>,
    /// Which disc carries which: (the lower, the one it carries on the layer above).
    pub links: Vec<[u32; 2]>,
}

/// A tree's branch on one layer: a disc of its drawn radius.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TreeDisc {
    pub layer: u32,
    /// The layer's print z and height (mm).
    pub z: f32,
    pub h: f32,
    /// Its centre (mm) and radius (mm).
    pub at: [f32; 2],
    pub r: f32,
    /// Where it stood as the descent left it (mm), before placement moved it: the centre of the
    /// ring the view shows of it while the trees grow.
    pub from: [f32; 2],
}

/// The trees of every layer's branches as placed (`layers`, in the trees' frame, `frame` from
/// the plan's): a tree is the branches joined by what carries what.
fn tree_shapes(layers: &[Vec<makepad_fab_toolpath::tree::element::Element>], settings: &makepad_fab_toolpath::tree::settings::Settings, heights: &[(f64, f64)], frame: Pt) -> Vec<TreeShape> {
    let start: Vec<usize> = layers.iter().scan(0, |n, l| {
        let s = *n;
        *n += l.len();
        Some(s)
    }).collect();
    let total: usize = layers.iter().map(Vec::len).sum();
    let mut root: Vec<usize> = (0..total).collect();
    fn find(root: &mut [usize], mut i: usize) -> usize {
        while root[i] != i {
            root[i] = root[root[i]];
            i = root[i];
        }
        i
    }
    let above = |l: usize, c: usize| (l + 1 < layers.len() && c < layers[l + 1].len()).then(|| start[l + 1] + c);
    for (l, layer) in layers.iter().enumerate() {
        for (k, e) in layer.iter().enumerate() {
            for up in e.carries.iter().filter_map(|&c| above(l, c)) {
                let (a, b) = (find(&mut root, start[l] + k), find(&mut root, up));
                root[a] = b;
            }
        }
    }
    let mm = |p: Pt| [Pt::new(p.x + frame.x, p.y + frame.y).x_mm(), Pt::new(p.x + frame.x, p.y + frame.y).y_mm()];
    let mut trees: std::collections::BTreeMap<usize, TreeShape> = Default::default();
    // Each branch's disc in its tree, to link them.
    let mut disc_of = vec![0u32; total];
    for (l, layer) in layers.iter().enumerate() {
        let (z, h) = heights.get(l).copied().unwrap_or((0.0, 0.0));
        for (k, e) in layer.iter().enumerate() {
            let tree = find(&mut root, start[l] + k);
            let t = trees.entry(tree).or_insert_with(|| TreeShape { tips: Vec::new(), tip_discs: Vec::new(), discs: Vec::new(), links: Vec::new() });
            let (at, from) = (mm(e.placed.unwrap_or(e.point)), mm(e.point));
            disc_of[start[l] + k] = t.discs.len() as u32;
            t.discs.push(TreeDisc { layer: l as u32, z: z as f32, h: h as f32, at: [at[0] as f32, at[1] as f32], r: e.drawn_radius(settings) as f32, from: [from[0] as f32, from[1] as f32] });
            if e.depth == 0 {
                let p = mm(e.point);
                t.tips.push([p[0], p[1], z]);
                t.tip_discs.push(disc_of[start[l] + k]);
            }
        }
    }
    for (l, layer) in layers.iter().enumerate() {
        for (k, e) in layer.iter().enumerate() {
            let tree = find(&mut root, start[l] + k);
            for up in e.carries.iter().filter_map(|&c| above(l, c)) {
                let link = [disc_of[start[l] + k], disc_of[up]];
                trees.get_mut(&tree).expect("its tree").links.push(link);
            }
        }
    }
    trees.into_values().collect()
}

#[derive(Default)]
struct GrowState {
    layers: std::collections::BTreeMap<usize, Grown>,
    dirty: std::collections::BTreeSet<usize>,
    sent: Option<std::time::Instant>,
    /// The trees the sink was last given of this growth ([`LiveEvent::PartTrees`]).
    trees: Option<Arc<[TreeShape]>>,
    /// When the trees as the descent has them were last built, and what that took.
    built: Option<(std::time::Instant, std::time::Duration)>,
}

impl GrowState {
    /// What it shows less the trees of [`Self::trees`] that `deleted` names a tip of: each one's
    /// rings, centred where the descent left its elements, and each of its tips' ring and the
    /// roof discs over the tip, up as far as they stand on one another; a layer whose lines were
    /// laid shows its rings again. The layers it changes go out next, and the trees left are its
    /// trees, and what it gives. None when no tree goes.
    fn without(&mut self, deleted: &Dropped) -> Option<Arc<[TreeShape]>> {
        use makepad_fab_toolpath::tree::DROP_REACH;
        let trees = self.trees.clone()?;
        let (gone, left): (Vec<&TreeShape>, Vec<&TreeShape>) = trees.iter().partition(|t| t.tips.iter().any(|a| deleted.has(a)));
        if gone.is_empty() {
            return None;
        }
        // A ring or disc drawn about a point: its corners' mean is the point, within what the
        // planner takes for the tip a deleted tip names.
        let about = |p: &makepad_fab_toolpath::ExtrusionPath, c: [f64; 2]| {
            let n = p.pts.len() as f64;
            n > 0.0 && (p.pts.iter().map(|q| q.x_mm()).sum::<f64>() / n - c[0]).hypot(p.pts.iter().map(|q| q.y_mm()).sum::<f64>() / n - c[1]) <= DROP_REACH
        };
        let mut rings: std::collections::BTreeMap<usize, Vec<[f64; 2]>> = Default::default();
        let mut tips = Vec::new();
        for t in &gone {
            for d in &t.discs {
                rings.entry(d.layer as usize).or_default().push([d.from[0] as f64, d.from[1] as f64]);
            }
            tips.extend(t.tip_discs.iter().zip(&t.tips).filter_map(|(&d, tip)| Some((t.discs.get(d as usize)?.layer as usize, [tip[0], tip[1]]))));
        }
        let mut changed = std::collections::BTreeSet::new();
        for (l, at) in &rings {
            let Some(g) = self.layers.get_mut(l) else { continue };
            let kept: Vec<_> = g.branches.iter().filter(|p| !at.iter().any(|&c| about(p, c))).cloned().collect();
            if kept.len() < g.branches.len() {
                g.branches = Arc::new(kept);
                changed.insert(*l);
            }
        }
        for &(l, c) in &tips {
            for l in l.. {
                let Some(g) = self.layers.get_mut(&l) else { break };
                let before: usize = g.tips.iter().map(|t| t.len()).sum();
                for t in &mut g.tips {
                    if t.iter().any(|p| about(p, c)) {
                        *t = Arc::new(t.iter().filter(|p| !about(p, c)).cloned().collect());
                    }
                }
                if g.tips.iter().map(|t| t.len()).sum::<usize>() == before {
                    break;
                }
                changed.insert(l);
            }
        }
        for l in &changed {
            if let Some(g) = self.layers.get_mut(l) {
                g.lines = None;
            }
        }
        self.dirty.extend(changed);
        let left: Arc<[TreeShape]> = left.into_iter().cloned().collect();
        self.trees = Some(left.clone());
        Some(left)
    }
}

/// The tips the user deleted on a part ([`DROPPED_TIPS`]), by x, as the planner finds the tips
/// they name: as near as [`makepad_fab_toolpath::tree::DROP_REACH`] in x and y, on its layer.
struct Dropped<'a> {
    by_x: Vec<&'a [f64; 3]>,
}

impl<'a> Dropped<'a> {
    fn new(dropped: &'a [[f64; 3]]) -> Self {
        let mut by_x: Vec<&[f64; 3]> = dropped.iter().collect();
        by_x.sort_by(|a, b| a[0].total_cmp(&b[0]));
        Dropped { by_x }
    }

    /// Whether `a`, a tree's tip, is one of them: those within twice the reach of it in x are
    /// looked at, which takes in every one near it whatever the rounding.
    fn has(&self, a: &[f64; 3]) -> bool {
        use makepad_fab_toolpath::tree::DROP_REACH;
        let near = |b: &[f64; 3]| (a[0] - b[0]).hypot(a[1] - b[1]) <= DROP_REACH && (a[2] - b[2]).abs() <= DROP_REACH;
        let from = self.by_x.partition_point(|b| b[0] < a[0] - 2.0 * DROP_REACH);
        self.by_x[from..].iter().take_while(|b| b[0] <= a[0] + 2.0 * DROP_REACH).any(|b| near(b))
    }
}

/// One layer as it grows, in the plan's frame: each part as the tell that made it built it.
#[derive(Clone, Default)]
struct Grown {
    faces: Arc<Vec<makepad_fab_toolpath::ExtrusionPath>>,
    /// The tips and roofs, as each tell added them.
    tips: Vec<Arc<Vec<makepad_fab_toolpath::ExtrusionPath>>>,
    branches: Arc<Vec<makepad_fab_toolpath::ExtrusionPath>>,
    lines: Option<Arc<Vec<makepad_fab_toolpath::ExtrusionPath>>>,
}

impl Grown {
    /// What the view shows of the layer: its lines once laid, else its faces, tips and branches.
    fn fills(&self) -> Vec<makepad_fab_toolpath::ExtrusionPath> {
        match &self.lines {
            Some(lines) => lines.to_vec(),
            None => self.faces.iter().chain(self.tips.iter().flat_map(|t| t.iter())).chain(self.branches.iter()).cloned().collect(),
        }
    }
}

/// The sent-supports mark of a part whose supports the sink shows as they grow: no plan's.
const GROWING: u64 = u64::MAX - 1;

/// The sent-supports mark of a part whose supports the sink shows as a plan's less some trees
/// the user deleted ([`Caches::pruned`] says which plan, and which layers).
const PRUNED: u64 = u64::MAX - 2;

impl<'a> Growing<'a> {
    /// What a tree stage told ([`Emit::Tree`]): kept, and the layers it changed sent when due.
    fn tell(&self, layer: usize, grown: makepad_fab_toolpath::tree::Grown, frame: Pt) {
        use makepad_fab_toolpath::tree::{draw::Bead, Grown as Made};
        if let Made::Trees { layers, settings } = grown {
            // The trees go to the view as the planner places them: a branch is picked and deleted
            // while they are drawn and their lines laid, not once the plan is whole.
            let trees: Arc<[TreeShape]> = tree_shapes(layers, settings, self.heights, frame).into();
            *self.trees.lock().unwrap() = Some(trees.clone());
            self.send_trees(trees);
            return;
        }
        use makepad_fab_toolpath::Role;
        let h = |l: usize| self.heights.get(l).map_or(0.2, |h| h.1);
        let bead = |l: usize| Bead { w: 0.3, h: h(l), speed: 0.0 };
        let moved = |p: Pt| Pt::new(p.x + frame.x, p.y + frame.y);
        let ring = |l: usize, role: Role, c: Pt, r: f64| {
            let r = r.max(0.15) * makepad_fab_slicer::geom::SCALE;
            let pts = (0..12).map(|k| {
                let a = k as f64 * std::f64::consts::TAU / 12.0;
                moved(Pt::new(c.x + (r * a.cos()) as i64, c.y + (r * a.sin()) as i64))
            }).collect();
            bead(l).closed(role, pts)
        };
        let outlines = |l: usize, role: Role, set: &[ExPolygon]| -> Vec<makepad_fab_toolpath::ExtrusionPath> {
            set.iter().flat_map(|e| e.rings()).map(|r| bead(l).closed(role, r.pts.iter().map(|&p| moved(p)).collect())).collect()
        };
        type Shown = Arc<Vec<makepad_fab_toolpath::ExtrusionPath>>;
        // What the tell shows, per layer it changes; whether an empty one changes only a layer
        // already shown; and where in the layer it goes.
        let (made, if_shown, put): (Vec<(usize, Shown)>, bool, fn(&mut Grown, Shown)) = match grown {
            Made::Nothing | Made::Trees { .. } => return,
            Made::Overhangs(faces) => (vec![(layer, Arc::new(outlines(layer, Role::SupportInterface, faces)))], true, |g, p| g.faces = p),
            Made::Tips { tips, roofs } => {
                let mut by: std::collections::BTreeMap<usize, Vec<makepad_fab_toolpath::ExtrusionPath>> = Default::default();
                for e in tips {
                    by.entry(e.layer).or_default().push(ring(e.layer, Role::SupportInterface, e.point, 0.3));
                }
                for (l, _, polys) in roofs {
                    by.entry(*l).or_default().extend(outlines(*l, Role::SupportInterface, polys));
                }
                (by.into_iter().map(|(l, p)| (l, Arc::new(p))).collect(), false, |g, p| g.tips.push(p))
            }
            Made::Branches { layers, settings } => {
                let rings = layers[layer].iter().map(|e| ring(layer, Role::Support, e.placed.unwrap_or(e.point), e.drawn_radius(settings))).collect();
                (vec![(layer, Arc::new(rings))], true, |g, p| g.branches = p)
            }
            Made::Paths(paths) => {
                let lines = paths.iter().map(|p| {
                    let mut p = p.clone();
                    for q in p.pts.iter_mut().chain(p.loop_pts.iter_mut()) {
                        *q = moved(*q);
                    }
                    p
                }).collect();
                (vec![(layer, Arc::new(lines))], false, |g, p| g.lines = Some(p))
            }
        };
        let due = {
            let mut s = self.state.lock().unwrap();
            if if_shown && made.iter().all(|(l, p)| p.is_empty() && !s.layers.contains_key(l)) {
                return;
            }
            for (l, p) in made {
                put(s.layers.entry(l).or_default(), p);
                s.dirty.insert(l);
            }
            !s.sent.is_some_and(|t| t.elapsed().as_secs_f64() < 0.05)
        };
        if due {
            self.flush();
        }
        // The trees as the descent has them go out after the layer it just left.
        if let Made::Branches { layers, settings } = grown {
            self.descended(layers, settings, frame);
        }
    }

    /// The layers changed since the last flush, to the sink (none while another flush runs):
    /// each as it stands, and their records made side by side on the planner's pool.
    fn flush(&self) {
        let Some(_one) = claim(&self.flushing) else { return };
        let changed: Vec<(usize, Grown)> = {
            let mut s = self.state.lock().unwrap();
            s.sent = Some(std::time::Instant::now());
            let dirty = std::mem::take(&mut s.dirty);
            dirty.into_iter().map(|l| (l, s.layers[&l].clone())).collect()
        };
        if changed.is_empty() {
            return;
        }
        let layers = par::map_vec(changed, |_, (l, g)| {
            let z = self.heights.get(l).map_or(0.0, |h| h.0);
            let ol = ObjectLayer { object: self.part, filament: self.filament, islands: vec![Arc::new(makepad_fab_toolpath::Island { walls: Vec::new(), fills: g.fills() })], ..Default::default() };
            (l as u32, LivePaths::of(l, z, &ol, self.job))
        });
        self.shared.caches.lock().unwrap().sent_supports[self.part] = GROWING;
        (self.shared.sink)(LiveEvent::PartSupports { part: self.part, key: self.key, layers, done: false });
    }

    /// The trees as the descent has them so far (`layers`), to the sink when due: a branch is
    /// picked and deleted while they grow. Built again only once a hundred times what the last
    /// build took has passed, which keeps what they add to the descent within a hundredth of it.
    fn descended(&self, layers: &[Vec<makepad_fab_toolpath::tree::element::Element>], settings: &makepad_fab_toolpath::tree::settings::Settings, frame: Pt) {
        if self.state.lock().unwrap().built.is_some_and(|(at, took)| at.elapsed() < took * 100) {
            return;
        }
        let start = std::time::Instant::now();
        let trees: Arc<[TreeShape]> = tree_shapes(layers, settings, self.heights, frame).into();
        self.state.lock().unwrap().built = Some((std::time::Instant::now(), start.elapsed()));
        self.send_trees(trees);
    }

    /// `trees` to the sink, the growth's now, after the layers they stand on that have yet to go:
    /// no tree is picked that the view does not show.
    fn send_trees(&self, trees: Arc<[TreeShape]>) {
        self.state.lock().unwrap().trees = Some(trees.clone());
        self.flush();
        (self.shared.sink)(LiveEvent::PartTrees { part: self.part, key: self.key, trees });
    }

    /// The trees `dropped` names a tip of go from what the sink shows of the growth this one
    /// goes on from: the trees left go to it, then the layers the deleted ones stood on.
    fn delete(&self, dropped: &[[f64; 3]]) {
        let deleted = Dropped::new(dropped);
        let Some(left) = self.state.lock().unwrap().without(&deleted) else { return };
        (self.shared.sink)(LiveEvent::PartTrees { part: self.part, key: self.key, trees: left });
        self.flush();
    }
}

#[derive(Clone)]
pub struct LiveEngine {
    shared: Arc<Shared>,
}

/// The sink runs the host's code, which takes the host's locks: called with an engine lock held,
/// it would order those after the engine's, and an engine call from another thread would wait on
/// whatever the host waits on. Under the JIT, which counts each thread's held locks, this checks
/// that none is: a test fails, a run says so once.
#[cfg(relax)]
fn sink_holds_no_lock() {
    let (held, at) = std::relax::held_lock();
    if held == 0 {
        return;
    }
    let at = at.map_or(String::new(), |l| format!(", the newest taken at {}:{}", l.file(), l.line()));
    #[cfg(test)]
    panic!("the live engine calls its sink holding {held} lock(s){at}");
    #[cfg(not(test))]
    {
        static SAID: AtomicBool = AtomicBool::new(false);
        if !SAID.swap(true, Ordering::Relaxed) {
            eprintln!("[fab] the live engine calls its sink holding {held} lock(s){at}");
        }
    }
}

/// A flag one thread at a time holds, which nobody waits for: [`claim`] fails while another
/// holds it. Dropping the claim lets go.
struct Claim<'a>(&'a AtomicBool);

fn claim(flag: &AtomicBool) -> Option<Claim<'_>> {
    (!flag.swap(true, Ordering::Acquire)).then_some(Claim(flag))
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn bits(m: &Affine) -> [u64; 12] {
    std::array::from_fn(|i| m[i / 4][i % 4].to_bits())
}

/// An open settle's answer once something else took its place: `why`.
fn superseded(tag: u64, revision: u64, why: &str) -> LiveEvent {
    LiveEvent::Settled { tag, revision, result: Err(why.into()), seconds: 0.0 }
}

impl LiveEngine {
    pub fn new(spawn: Spawn, sink: Sink, next: Next) -> Self {
        let shown: Arc<Mutex<Vec<u64>>> = Arc::default();
        let sink: Sink = {
            let shown = shown.clone();
            Arc::new(move |e: LiveEvent| {
                {
                    let mut s = shown.lock().unwrap();
                    match &e {
                        LiveEvent::PartCut { part, key, .. } | LiveEvent::PartRegions { part, key, .. } | LiveEvent::PartLayers { part, key, .. }
                        | LiveEvent::PartSupports { part, key, .. } => {
                            if s.len() <= *part { s.resize(part + 1, u64::MAX); }
                            s[*part] = *key;
                        }
                        LiveEvent::Settings | LiveEvent::Failed { .. } => s.fill(u64::MAX),
                        _ => {}
                    }
                }
                #[cfg(relax)]
                sink_holds_no_lock();
                sink(e)
            })
        };
        LiveEngine {
            shared: Arc::new(Shared {
                drive: Mutex::new(live_trace::Drive::default()),
                state: Mutex::new(State::default()),
                changes: Mutex::new(0),
                changed: Condvar::new(),
                next,
                spawn,
                sink,
                caches: Mutex::new(Caches::default()),
                planned: AtomicU64::new(0),
                planes_cut: Arc::default(),
                traced: AtomicU64::new(0),
                settings: AtomicU64::new(0),
                part_work: Mutex::new(Vec::new()),
                last_layers: Mutex::new(None),
                sent_toolpaths: Snap::new(Arc::new(None)),
                estimates: Snap::new(Arc::new([])),
                code: Snap::new(Arc::default()),
                background: Mutex::new(None),
                records: Mutex::new(None),
                queued: Mutex::new((0, Vec::new())),
                slice_memo: Arc::default(),
                memory_guard: Mutex::new(None),
                guard_at: AtomicU64::new(0),
                started: std::time::Instant::now(),
                over_memory: Mutex::new(None),
                failing: std::sync::atomic::AtomicBool::new(false),
                setups: Mutex::new(None),
                bound: Mutex::new(None),
                run_keys: Mutex::new(Vec::new()),
                shown,
                #[cfg(test)]
                panic_part: Mutex::new(None),
                #[cfg(test)]
                touched: Mutex::new(None),
            }),
        }
    }

    /// `f` changes the state; what it returns (an open settle it superseded, answered) goes to the
    /// sink once the state is let go.
    fn touch(&self, f: impl FnOnce(&mut State) -> Option<LiveEvent>) -> u64 {
        let (start, revision, answer) = {
            let mut s = self.shared.state.lock().unwrap();
            let answer = f(&mut s);
            s.revision += 1;
            s.touched = Some(std::time::Instant::now());
            s.checkpoint = (self.shared.next)();
            let start = s.enabled && !s.running;
            if start {
                s.running = true;
            }
            (start, s.revision, answer)
        };
        if let Some(answer) = answer {
            (self.shared.sink)(answer);
        }
        self.shared.note_change();
        // (taken out first: the hook runs with no lock held)
        #[cfg(test)]
        {
            let between = self.shared.touched.lock().unwrap().take();
            if let Some(between) = between { between(); }
        }
        // The driver found here may run this revision already: the older one ended and this
        // revision's started since the state above (the caller preempted in between). Only an
        // older driver stops; stopping this revision's would serve it with its viewed layers alone.
        let driver = self.shared.drive.lock().unwrap().driver.clone();
        if let Some(driver) = driver { driver.supersede(revision); }
        if start {
            let shared = self.shared.clone();
            (self.shared.spawn)(Urgency::Viewed, Box::new(move || drive(shared)));
        }
        revision
    }

    /// Slicing on or off. Off: inputs are still recorded, nothing runs (work
    /// in progress stops at its next layer). On: the newest state is sliced
    /// at once and edits are followed again.
    pub fn set_enabled(&self, on: bool) {
        self.touch(|s| {
            s.enabled = on;
            None
        });
    }

    /// The print time of the newest whole live slice, from the time model without G-code text,
    /// answered as [`LiveEvent::Estimated`]. Runs once the newest revision's whole toolpaths are
    /// sent (as [`LiveEvent::Toolpaths`] says), beside the checks that follow them; a newer input
    /// stops it at its next layer through the captured checkpoint. The plate's print time or the
    /// plate's before goes at once, as the slice's own work and not behind the background's
    /// ([`LiveEngine::estimate_kept`]); only its totals are kept, so the time model runs again in
    /// the background for its records. It reads what is kept without the caches, which the UI
    /// never waits for.
    pub fn estimate(&self) {
        let (rev, checkpoint) = {
            let s = self.shared.state.lock().unwrap();
            if !s.enabled {
                return;
            }
            (s.revision, s.checkpoint.clone())
        };
        let kept = sent_key(&self.shared, rev).and_then(|key| kept_estimate(&self.shared, &key).map(|e| (key, e)));
        if let Some((key, estimate)) = &kept {
            let (shared, key, estimate) = (self.shared.clone(), key.clone(), estimate.clone());
            (self.shared.spawn)(Urgency::Focus, Box::new(move || {
                // The step back makes it the plate's print time again, and the one it replaces the
                // plate's before, if it is still kept.
                let replaced = {
                    let c = shared.caches.lock().unwrap();
                    kept_estimate(&shared, &key).map(|e| keep_estimate(&c, &shared.estimates, key, e))
                };
                drop(replaced);
                (shared.sink)(LiveEvent::Estimated { revision: rev, estimate, streamed: false })
            }));
        }
        self.estimate_in_background(rev, checkpoint, kept.map(|(_, e)| e));
    }

    /// The time model for revision `rev` on the background ([`estimate_served`]): the print time
    /// `kept` for it gets its records again.
    fn estimate_in_background(&self, rev: u64, checkpoint: Checkpoint, kept: Option<Arc<Estimate>>) {
        let shared = self.shared.clone();
        let spawn = self.shared.background.lock().unwrap().clone().unwrap_or_else(|| self.shared.spawn.clone());
        spawn(Urgency::Rest, Box::new(move || {
            // Beside the drive (it reads only the sent toolpaths): a failure loses this estimate only.
            if let Err(p) = catch_unwind(AssertUnwindSafe(|| estimate_served(&shared, rev, &checkpoint, kept))) {
                let why = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                eprintln!("[fab] the print time estimate failed: {why}");
            }
        }));
    }

    /// Whether the newest revision's toolpaths have their print time kept: a step back or forth
    /// between the plate and the plate's before, which [`LiveEngine::estimate`] answers at once,
    /// with no time model run.
    pub fn estimate_kept(&self) -> bool {
        let rev = self.shared.state.lock().unwrap().revision;
        sent_key(&self.shared, rev).is_some_and(|k| kept_estimate(&self.shared, &k).is_some())
    }

    /// Where background work runs ([`Self::estimate`] and the printability checks after each
    /// revision); without one, where the slices run.
    pub fn set_background(&self, spawn: Spawn) {
        *self.shared.background.lock().unwrap() = Some(spawn);
    }

    /// Makes the preview builder for each estimate's records, called on the time model's thread.
    pub fn set_records(&self, f: RecordsFn) {
        *self.shared.records.lock().unwrap() = Some(f);
    }

    /// A new plate or setup: transforms reset. A pending settle stays when
    /// the input is the same plate and settings (another view of it).
    pub fn set_input(&self, input: LiveInput) {
        let (id, settings, parts) = (input.plate_id, input.settings_id, input.plate.parts.len());
        self.set_input_with(id, settings, parts, Arc::new(move || input.clone()));
    }

    /// [`Self::set_input`] of an input `make` builds on the engine's thread:
    /// `plate_id` and `settings_id` are its identity, `parts` its part count.
    /// Returns the revision whose live results answer this input.
    pub fn set_input_with(&self, plate_id: u64, settings_id: u64, parts: usize, make: MakeInput) -> u64 {
        self.touch(|s| {
            let answer = if s.key != Some((plate_id, settings_id)) { s.settle.take().map(|tag| superseded(tag, s.revision, "superseded by a newer plate")) } else { None };
            s.xf = vec![None; parts];
            s.key = Some((plate_id, settings_id));
            s.pending = Some(make);
            s.held = None;
            s.moving = false;
            answer
        })
    }

    /// This input again with its slice in the caller's hands, made earlier and brought back:
    /// nothing slices it, the running work stops at its next layer, and a part moved on top of it
    /// starts from `make`.
    pub fn hold_input(&self, plate_id: u64, settings_id: u64, parts: usize, make: MakeInput) -> u64 {
        self.touch(|s| {
            let answer = s.settle.take().map(|tag| superseded(tag, s.revision, "superseded by a newer plate"));
            s.xf = vec![None; parts];
            s.key = Some((plate_id, settings_id));
            s.pending = None;
            s.input = None;
            s.held = Some(make);
            s.moving = false;
            answer
        })
    }

    /// The current revision of this input, without a later live transform on top of it.
    pub fn input_revision(&self, plate_id: u64, settings_id: u64) -> Option<u64> {
        let s = self.shared.state.lock().unwrap();
        (s.key == Some((plate_id, settings_id)) && s.xf.iter().all(Option::is_none)).then_some(s.revision)
    }

    /// The input `revision` is a result of, while it is the newest (no newer input or live
    /// transform on top of it): what a result of that revision was made from.
    pub fn key_of(&self, revision: u64) -> Option<(u64, u64)> {
        let s = self.shared.state.lock().unwrap();
        if s.revision == revision && s.xf.iter().all(Option::is_none) { s.key } else { None }
    }

    /// A part's transform now (coalescing: only the newest is worked on).
    pub fn set_transform(&self, part: usize, xf: Affine) {
        self.touch(|s| {
            if let Some(slot) = s.xf.get_mut(part) {
                *slot = Some(xf);
                s.moving = true;
                if let Some(make) = s.held.take() {
                    s.pending = Some(make);
                }
            }
            None
        });
    }

    /// An open settle no longer stands (a part started to move): it answers as superseded,
    /// the input stays for the transforms that follow.
    pub fn stop_settle(&self) {
        if self.shared.state.lock().unwrap().settle.is_none() { return; }
        self.touch(|s| s.settle.take().map(|tag| superseded(tag, s.revision, "superseded by a move")));
    }

    /// The parts stopped moving: refine past the contours.
    pub fn release(&self) {
        self.touch(|s| {
            s.moving = false;
            None
        });
    }

    /// A live patch landed (under the JIT). True when it changed code the engine's results come
    /// from ([`loaded_code`]): the newest input runs again and makes anew only what that code
    /// makes, as a changed setting does, and a settled slice before it is the old code's.
    pub fn code_patched(&self) -> bool {
        if *self.shared.code.get() == loaded_code() {
            return false;
        }
        self.touch(|_| None);
        true
    }

    /// The exact slice of `input` (the Print path); `tag` comes back with it.
    pub fn settle(&self, tag: u64, input: LiveInput) {
        let (id, settings, parts) = (input.plate_id, input.settings_id, input.plate.parts.len());
        self.settle_with(tag, id, settings, parts, Arc::new(move || input.clone()));
    }

    /// [`Self::settle`] of an input `make` builds on the engine's thread.
    pub fn settle_with(&self, tag: u64, plate_id: u64, settings_id: u64, parts: usize, make: MakeInput) {
        self.touch(|s| {
            let answer = s.settle.take().filter(|t| *t != tag).map(|old| superseded(old, s.revision, "superseded by a newer slice"));
            s.xf = vec![None; parts];
            s.key = Some((plate_id, settings_id));
            s.pending = Some(make);
            s.held = None;
            s.moving = false;
            s.settle = Some(tag);
            answer
        });
    }

    /// What keeps a slice from taking the machine down: asked while one runs.
    pub fn set_memory_guard(&self, guard: MemoryGuard) {
        *self.shared.memory_guard.lock().unwrap() = Some(guard);
    }

    /// Nothing is to be sliced any more (the Slice view closed, the plate cannot print): the
    /// work in progress stops at its next layer, an open settle answers as superseded and
    /// nothing runs until the next input.
    pub fn cancel(&self) {
        self.touch(|s| {
            let answer = s.settle.take().map(|tag| superseded(tag, s.revision, "cancelled"));
            s.key = None;
            s.pending = None;
            s.held = None;
            s.input = None;
            s.moving = false;
            answer
        });
    }

    /// The caller lost what the sink gave it (its state went with a panic): the slice fails
    /// as one over its memory does, so the next sends every part again. A slice in progress
    /// stops at its next checkpoint and fails when it ends; with none, the caches go now, and
    /// `Failed` of the newest revision and an open settle's `Err` go out from here.
    pub fn fail(&self, error: String) {
        let idle = {
            let mut s = self.shared.state.lock().unwrap();
            if s.running {
                s.failing = Some(error.clone());
                self.shared.failing.store(true, Ordering::Relaxed);
                None
            } else {
                Some((s.revision, s.settle.take()))
            }
        };
        if let Some((revision, settle)) = idle {
            fail_now(&self.shared, revision, settle, error);
        }
        self.shared.note_change();
    }

    /// How many planes of whole parts were cut for test prints so far: a cube moved over layers
    /// already cut adds none.
    pub fn planes_cut(&self) -> u64 {
        self.shared.planes_cut.load(Ordering::Relaxed)
    }

    /// How many part plans (walls to infill) were made so far.
    pub fn parts_planned(&self) -> u64 {
        self.shared.planned.load(Ordering::Relaxed)
    }

    /// How many contour layers were traced.
    pub fn traced_count(&self) -> u64 {
        self.shared.traced.load(Ordering::Relaxed)
    }
    /// How many model layers were planned, counting each region once.
    pub fn layers_computed(&self) -> u64 {
        self.shared.part_work.lock().unwrap().iter().map(|w| w.planned.load(Ordering::Relaxed)).sum()
    }

    /// Per part slot since the engine started: layers traced and layers planned
    /// (one per model layer emitted per region).
    pub fn part_work(&self) -> Vec<(u64, u64)> {
        self.shared.part_work.lock().unwrap().iter().map(|w| (w.traced.load(Ordering::Relaxed), w.planned.load(Ordering::Relaxed))).collect()
    }

    /// Per part slot since the engine started: what its trees' plans made afresh, stage by stage,
    /// rather than took from what the plan before kept.
    pub fn tree_work(&self) -> Vec<makepad_fab_toolpath::tree::Work> {
        self.shared.part_work.lock().unwrap().iter().map(|w| *w.trees.lock().unwrap()).collect()
    }

    /// Per part slot since the engine started: model layers kept from the last plan (each region
    /// once), and why the newest edit of the part kept none (empty: it kept some).
    pub fn part_kept(&self) -> Vec<(u64, String)> {
        self.shared.part_work.lock().unwrap().iter().map(|w| (w.kept.load(Ordering::Relaxed), w.kept_none.lock().unwrap().clone())).collect()
    }

    /// Per part slot since the engine started: milliseconds spent planning its windows.
    pub fn part_plan_ms(&self) -> Vec<f64> {
        self.shared.part_work.lock().unwrap().iter().map(|w| w.plan_ns.load(Ordering::Relaxed) as f64 * 1e-6).collect()
    }

    /// The layer the viewer looks at (it is sliced first).
    pub fn set_focus_z(&self, z: Option<f64>) {
        // Not an input: no revision bump, read between contour bands.
        self.shared.state.lock().unwrap().focus_z = z;
    }

    /// Forget what the sink was given (a new sink after a restart): the next
    /// revision sends every part's cut and plan again. Marks every slot unsent
    /// rather than emptying the lists: a revision running on the pool indexes them by part.
    pub fn resend_all(&self) {
        let partial = {
            let mut c = self.shared.caches.lock().unwrap();
            c.sent_cut.fill(u64::MAX);
            c.sent_plan.fill(u64::MAX);
            c.sent_supports.fill(u64::MAX);
            std::mem::take(&mut c.partial_plans)
        };
        self.shared.shown.lock().unwrap().fill(u64::MAX);
        drop(partial);
    }
}

/// One part slot of a revision: its contour source in its frame, the mesh
/// when it is cut from one, its content key, its plate offset, and the frame
/// that reproduces it ([`Settled::sliced`]).
#[derive(Clone)]
struct Slot {
    src: Arc<dyn ContourSource + Send + Sync>,
    mesh: Option<Arc<MeshSource>>,
    /// A field part's non-planar top plan.
    np: Option<Arc<NpPlan>>,
    key: u64,
    /// What it slices, whatever its tool and overrides: a field's content, a mesh's
    /// ([`hash_mesh`]).
    content: u64,
    off: Pt,
    sliced: Frame,
    /// Cut as its glued group's union, or as nothing of its own in one ([`crate::glue`]): no
    /// edit of its own reaches only some of its layers.
    glued: bool,
}

/// A precut contour source: the engine's cut, falling back to the source.
struct Precut {
    /// The live slice's cut key (`slot.key`), the same one its cut cache reuses contours by, under
    /// the code that makes parts ([`with_code`]).
    key: Option<u64>,
    mesh: Arc<dyn ContourSource + Send + Sync>,
    field: bool,
    bounds: ([f64; 3], [f64; 3]),
    by_z: HashMap<u64, Regions>,
    np: Option<Arc<NpPlan>>,
}

impl ContourSource for Precut {
    fn content_key(&self) -> Option<u64> { self.key }
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.bounds
    }
    fn pivot_box(&self) -> ([f64; 2], [f64; 2]) {
        self.mesh.pivot_box()
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        if zs.iter().all(|z| self.by_z.contains_key(&z.to_bits())) {
            return zs.iter().map(|z| self.by_z[&z.to_bits()].to_vec()).collect();
        }
        assert!(!self.field, "field planning requested contours outside its precut window");
        self.mesh.contours(zs)
    }
    fn painted(&self) -> bool {
        self.mesh.painted()
    }
    fn paint_regions(&self, zs: &[f64], slices: &[Vec<ExPolygon>], p: &PaintParams) -> Vec<LayerPaint> {
        self.mesh.paint_regions(zs, slices, p)
    }
    fn paint_at(&self) -> Option<PaintAt> {
        self.mesh.paint_at()
    }
    fn support_paint(&self) -> Option<makepad_fab_slicer::source::SupportPaint> {
        self.mesh.support_paint()
    }
    fn settings_zones(&self) -> Vec<makepad_fab_slicer::source::SettingsZone> {
        self.mesh.settings_zones()
    }
    fn np_plan(&self) -> Option<Arc<NpPlan>> {
        self.np.clone()
    }
    fn layer_stack(&self) -> Option<Arc<[(f64, f64)]>> {
        self.mesh.layer_stack()
    }
    fn slope_profile(&self, step: f64) -> Option<makepad_fab_slicer::source::SlopeProfile> {
        self.mesh.slope_profile(step)
    }
    fn embeds(&self) -> Vec<makepad_fab_slicer::embed::Embed> {
        self.mesh.embeds()
    }
    fn cavity(&self, z: f64) -> Vec<ExPolygon> {
        self.mesh.cavity(z)
    }
}

/// The slicer's mesh of a part, with its paint when the stream matches.
fn painted_mesh(p: &Part, paint: Option<&Vec<String>>) -> Mesh {
    let mut mesh = part_mesh(p);
    if let Some(pt) = paint.filter(|pt| pt.len() == mesh.tris.len()) {
        mesh.paint = pt.clone();
    }
    mesh
}

/// The slicer's mesh of a part given as frame `f`, in the frame (placed at its XY offset): an
/// exact part's print mesh, which is in plate coordinates (as the plate's display mesh is), less
/// that offset; any other part's frame mesh raised by the frame's z offset. Either lands where
/// the plate shows the part.
pub(crate) fn frame_mesh(p: &Part, f: &Frame, paint: Option<&Vec<String>>) -> Mesh {
    if p.print_mesh.is_some() {
        let mut mesh = painted_mesh(p, paint);
        if f.offset[0] != 0.0 || f.offset[1] != 0.0 {
            for q in &mut mesh.verts {
                q[0] -= f.offset[0];
                q[1] -= f.offset[1];
            }
        }
        return mesh;
    }
    let mut pm = (*f.local).clone();
    for q in &mut pm.positions {
        q[2] += f.offset[2];
    }
    painted_mesh(&Part { mesh: pm, ..p.clone() }, paint)
}

fn placed_part(p: &Part, m: &Affine) -> Part {
    Part {
        mesh: PartMesh {
            positions: p.mesh.positions.iter().map(|q| std::array::from_fn(|r| m[r][0] * q[0] + m[r][1] * q[1] + m[r][2] * q[2] + m[r][3])).collect(),
            triangles: p.mesh.triangles.clone(),
        },
        print_mesh: p.print_mesh.clone().map(|f| {
            let m = *m;
            fab_plate::PrintMesh(Arc::new(move || {
                let mut fine = (f.0)()?;
                for q in &mut fine.positions {
                    *q = std::array::from_fn(|r| m[r][0] * q[0] + m[r][1] * q[1] + m[r][2] * q[2] + m[r][3]);
                }
                Ok(fine)
            }))
        }),
        ..p.clone()
    }
}

/// One planned layer as preview records (times from 0), in the plan's frame: its
/// [`LivePaths`]' records.
pub fn layer_records(index: usize, z: f64, ol: &ObjectLayer, job: &Job) -> Vec<MoveRecord> {
    LivePaths::of(index, z, ol, job).records()
}

/// The toolpaths as preview records in print order, before the writer (live checks): one part
/// layer's at a time, never the whole print's, and none once `stale` says so.
fn records_of<'a>(tp: &'a Toolpaths, job: &'a Job, stale: &'a dyn Fn() -> bool) -> impl Iterator<Item = MoveRecord> + 'a {
    let mut t = 0.0;
    tp.layers.iter().enumerate().flat_map(|(li, layer)| layer.objects.iter().map(move |ol| (li, layer.print_z, ol))).flat_map(move |(li, z, ol)| {
        let mut records = if stale() { Vec::new() } else { layer_records(li, z, ol, job) };
        for m in &mut records {
            m.t_start += t;
            m.t_end += t;
        }
        t = records.last().map_or(t, |m| m.t_end);
        records
    })
}

struct Snapshot {
    revision: u64,
    checkpoint: Checkpoint,
    started: std::time::Instant,
    touched: std::time::Instant,
    input: Option<Arc<LiveInput>>,
    xf: Vec<Option<Affine>>,
    moving: bool,
    settle: Option<u64>,
    focus_z: Option<f64>,
}

/// Recovery owns these caches only after all part workers have joined.
/// Discard even poisoned state: a panicking revision may have published
/// sent markers or partial reuse data before it stopped.
fn discard<T: Default>(cache: &Mutex<T>) {
    let old = {
        let mut value = cache.lock().unwrap_or_else(|p| p.into_inner());
        let old = std::mem::take(&mut *value);
        cache.clear_poison();
        old
    };
    drop(old);
}

/// The running slice is over its memory: true from the guard's first `Some` on.
fn over_memory(shared: &Shared) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    if shared.failing.load(Relaxed) {
        return true;
    }
    let now = shared.started.elapsed().as_millis() as u64;
    let at = shared.guard_at.load(Relaxed);
    if now < at + GUARD_MS || shared.guard_at.compare_exchange(at, now, Relaxed, Relaxed).is_err() {
        return false;
    }
    let guard = shared.memory_guard.lock().unwrap().clone();
    let Some(why) = guard.and_then(|g| g()) else { return false };
    *shared.over_memory.lock().unwrap() = Some(why);
    shared.failing.store(true, Relaxed);
    true
}

/// Everything the engine holds goes: what it made so far is what the machine lacks room for.
fn discard_all(shared: &Shared) {
    discard(&shared.caches);
    discard(&shared.setups);
    discard(&shared.bound);
    discard(&shared.last_layers);
    drop((shared.sent_toolpaths.replace(Arc::new(None)), shared.estimates.replace(Arc::new([])), shared.code.replace(Arc::default())));
}

/// The slice fails with `error`: everything the engine holds goes, `Failed` of `revision`
/// and the answer of the settle `tag` go out.
fn fail_now(shared: &Shared, revision: u64, tag: Option<u64>, error: String) {
    discard_all(shared);
    (shared.sink)(LiveEvent::Failed { revision, error: error.clone() });
    if let Some(tag) = tag {
        (shared.sink)(LiveEvent::Settled { tag, revision, result: Err(error), seconds: 0.0 });
    }
}

fn panic_failed(shared: &Shared, revision: u64, tag: Option<u64>, payload: Box<dyn std::any::Any + Send>) {
    discard_all(shared);
    let error = payload.downcast_ref::<String>().cloned().or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "internal slicer panic".into());
    (shared.sink)(LiveEvent::Failed { revision, error: error.clone() });
    if let Some(tag) = tag {
        (shared.sink)(LiveEvent::Settled { tag, revision, result: Err(error), seconds: 0.0 });
    }
}

fn served(shared: &Shared, revision: u64, tag: Option<u64>) {
    {
        let mut s = shared.state.lock().unwrap();
        if s.revision == revision {
            s.served = revision;
            if s.settle == tag {
                s.settle = None;
            }
        }
    }
    shared.note_change();
}

fn drive(shared: Arc<Shared>) {
    // Take a complete input snapshot once. A newer builder replaces only pending
    // work; the running snapshot owns its focus chunk until it is published.
    let (mut snap, make, plate_id) = {
        let mut s = shared.state.lock().unwrap();
        if !s.enabled || s.served == s.revision || (s.input.is_none() && s.pending.is_none()) {
            s.running = false;
            drop(s);
            shared.note_change();
            return;
        }
        (Snapshot { revision: s.revision, checkpoint: s.checkpoint.clone(), touched: s.touched.unwrap_or_else(std::time::Instant::now),
            started: std::time::Instant::now(), input: s.input.clone(), xf: s.xf.clone(), moving: s.moving, settle: s.settle, focus_z: s.focus_z }, s.pending.clone(), s.key.map_or(0, |k| k.0))
    };
    let rev = snap.revision;
    // A snapshot taken is driven: a newer input (or the edit asking for one: the app marks
    // the checkpoint stale on every drag step, before its input exists) stops all but its
    // edited parts' viewed layers, which are drawn. Bailing here instead drew nothing during a
    // drag: each input went stale while it was being built, before its driver began.
    let r = catch_unwind(AssertUnwindSafe(|| {
        (shared.sink)(LiveEvent::Started { revision: rev, plate_id });
        if let Some(make) = make {
            let built = Arc::new(make());
            {
                let mut s = shared.state.lock().unwrap();
                if s.pending.as_ref().is_some_and(|p| Arc::ptr_eq(p, &make)) {
                    s.pending = None;
                    s.input = Some(built.clone());
                }
            }
            snap.input = Some(built);
        }
    }));
    if let Err(p) = r {
        panic_failed(&shared, rev, snap.settle, p);
        drive_finished(shared, rev, snap.settle, &snap.checkpoint, true);
    } else {
        live_trace::Driver::start(shared, snap);
    }
}

fn drive_finished(shared: Arc<Shared>, rev: u64, settle: Option<u64>, checkpoint: &Checkpoint, failed: bool) {
    let failed_now = shared.failing.swap(false, Ordering::Relaxed);
    if failed_now {
        let error = shared.state.lock().unwrap().failing.take();
        let error = error.or_else(|| shared.over_memory.lock().unwrap().take()).unwrap_or_default();
        fail_now(&shared, rev, settle, error);
    }
    // A bump without a new input cancels only the work. Keep an outstanding
    // settle for the next published input, and do not start the old input again.
    if failed || failed_now || !checkpoint() {
        served(&shared, rev, settle);
    }
    let (again, late_failure) = {
        let mut s = shared.state.lock().unwrap();
        let again = s.enabled && s.revision != rev && s.served != s.revision && (s.input.is_some() || s.pending.is_some());
        s.running = again;
        let late_failure = if shared.failing.swap(false, Ordering::Relaxed) {
            Some((s.revision, s.settle.take(), s.failing.take().unwrap_or_default()))
        } else {
            None
        };
        (again, late_failure)
    };
    if let Some((revision, settle, error)) = late_failure {
        fail_now(&shared, revision, settle, error);
    }
    shared.note_change();
    if again {
        let next = shared.clone();
        (shared.spawn)(Urgency::Viewed, Box::new(move || drive(next)));
    }
}

/// A slice's tools bound after slicing. Its paths are each colour's whichever tool the colour
/// has, so a job tool no path prints in (a part's own colour that its paint covers all of)
/// leaves the job and the tools after it move down: the G-code, its tool changes, purges and
/// time, and its package are made with only the tools that print.
struct Bound {
    /// The input's job tool (0-based) each tool is.
    tools: Vec<usize>,
    setup: Arc<JobSetup>,
    job: Arc<Job>,
    /// The toolpaths on these tools.
    toolpaths: Arc<Toolpaths>,
}

/// The job tools (0-based, ascending) a slice's paths print in.
fn printed_tools(tp: &Toolpaths) -> Vec<usize> {
    let mut tools: Vec<usize> = tp.layers.iter().flat_map(|l| l.objects.iter().filter(|ol| ol.has_paths()).map(|ol| ol.filament))
        .chain(tp.np_blocks.iter().filter(|b| !b.paths.is_empty()).map(|b| tp.objects[b.object].filament))
        .collect();
    tools.sort_unstable();
    tools.dedup();
    tools
}

/// `tp` on the job tools `kept` (the ones it prints in, ascending), each numbered by its place
/// there. A part whose own tool prints nothing takes the first it prints in, and so do its
/// regions on that tool (which print nothing).
fn rebind(tp: &Toolpaths, kept: &[usize]) -> Toolpaths {
    let at = |f: usize| kept.binary_search(&f).ok();
    let mut first = vec![None; tp.objects.len()];
    for ol in tp.layers.iter().flat_map(|l| &l.objects).filter(|ol| ol.has_paths()) {
        if let Some(k) = at(ol.filament) {
            let f = &mut first[ol.object];
            *f = Some(f.map_or(k, |f: usize| f.min(k)));
        }
    }
    let own: Vec<usize> = tp.objects.iter().zip(&first).map(|(o, f)| at(o.filament).or(*f).unwrap_or(0)).collect();
    let mut out = tp.clone();
    for (o, &f) in out.objects.iter_mut().zip(&own) {
        o.filament = f;
    }
    for ol in out.layers.iter_mut().flat_map(|l| l.objects.iter_mut()) {
        ol.filament = at(ol.filament).unwrap_or(own[ol.object]);
    }
    out
}

/// `tp`'s tools bound (its job has `tools`): `None` while every tool prints, or none does.
/// `kept` makes the setup and job of the tools that print.
fn bind(tp: &Toolpaths, tools: usize, kept: impl FnOnce(&[usize]) -> Result<(Arc<JobSetup>, Arc<Job>), String>) -> Result<Option<Bound>, String> {
    let printed = printed_tools(tp);
    if printed.is_empty() || printed.len() == tools {
        return Ok(None);
    }
    let (setup, job) = kept(&printed)?;
    Ok(Some(Bound { toolpaths: Arc::new(rebind(tp, &printed)), tools: printed, setup, job }))
}

/// The setup and job of input `key`'s tools `kept`, made once for them by the code that makes the
/// parts (`code`, [`LoadedCode::part`]).
fn kept_job(shared: &Shared, key: (u64, u64), code: u64, setup: &SetupFn, kept: &[usize]) -> Result<(Arc<JobSetup>, Arc<Job>), String> {
    let key = (key.0, with_code(key.1, code));
    let made = shared.bound.lock().unwrap().as_ref().filter(|(k, t, _, _)| *k == key && t == kept).map(|(_, _, s, j)| (s.clone(), j.clone()));
    if let Some(made) = made {
        return Ok(made);
    }
    let s = Arc::new(setup(Some(kept))?);
    let j = Arc::new(prepare_job(&s)?);
    *shared.bound.lock().unwrap() = Some((key, kept.to_vec(), s.clone(), j.clone()));
    Ok((s, j))
}

/// The print time of revision `rev`'s toolpaths with its records, unless a newer state came: the
/// time model's, which is kept with their plans, or `kept` (the one kept for them) with the
/// records the time model makes again.
fn estimate_served(shared: &Shared, rev: u64, checkpoint: &Checkpoint, kept: Option<Arc<Estimate>>) {
    let sent = (*shared.sent_toolpaths.get()).as_ref().filter(|t| t.revision == rev).map(|t| (t.toolpaths.clone(), t.job.clone(), t.key, t.setup.clone(), (t.merged_from.clone(), t.key)));
    let Some((toolpaths, job, key, setup, kept_as)) = sent else { return };
    let stale = || checkpoint();
    let t0 = std::time::Instant::now();
    let tp = toolpaths.read();
    let fail = |error: String| (shared.sink)(LiveEvent::Failed { revision: rev, error });
    let bound = match bind(&tp, job.filaments(), |kept| kept_job(shared, key, loaded_code().part, &setup, kept)) {
        Ok(b) => b,
        Err(e) => return fail(e),
    };
    let (timed, on, tools) = match &bound {
        Some(b) => (&*b.toolpaths, &*b.job, b.tools.clone()),
        None => (&*tp, &*job, (0..job.filaments()).collect()),
    };
    // The records go to their taker on this thread as the time model makes them, so it never
    // falls behind and holds one part at most. A dropped taker (a stale or failed run) is the
    // estimate stopping.
    let records = shared.records.lock().unwrap().clone();
    let taker = records.and_then(|f| f(rev, kept.clone()));
    let cell = Arc::new(Mutex::new(taker));
    let send = {
        let cell = cell.clone();
        Box::new(move |records: Vec<MoveRecord>, temps: Vec<Vec<i16>>| {
            let taker = cell.lock().unwrap().take();
            let Some(mut taker) = taker else { return Some(records) };
            let spare = taker(Streamed::Part { records, temps });
            *cell.lock().unwrap() = Some(taker);
            spare
        })
    };
    let stream = makepad_fab_gcode::process::PartStream { send };
    let (mut timeline, stats) = match makepad_fab_gcode::writer::estimate(timed, on, &stale, Some(stream)) {
        Ok(Some(estimate)) => estimate,
        Ok(None) => return,
        Err(error) => return fail(error.into()),
    };
    let filament_temps = (0..tools.len()).map(|k| on.fil("nozzle_temperature", k)).collect();
    // The records went with the table's entries.
    timeline.nozzle_temps = Vec::new();
    if let Some(estimate) = kept {
        drop((tp, bound));
        if !stale() {
            let taker = cell.lock().unwrap().take();
            if let Some(mut taker) = taker {
                let _ = taker(Streamed::Done { estimate: estimate.clone() });
            }
            (shared.sink)(LiveEvent::Estimated { revision: rev, estimate, streamed: true });
        }
        return;
    }
    // The changes are the input's tools': what other nozzles for its colours are weighed by.
    let changes = (!makepad_fab_gcode::sequential::active(&tp, &job))
        .then(|| Arc::new(makepad_fab_gcode::heads::Changes { sets: makepad_fab_gcode::writer::layer_sets(&tp), model: makepad_fab_settings::change::ChangeModel::new(&job) }));
    drop((tp, bound));
    if stale() {
        return;
    }
    // Nothing reads an estimate's per-line times.
    timeline.line_end = Vec::new();
    let estimate = Arc::new(Estimate { timeline, stats, changes, tools, filament_temps, seconds: t0.elapsed().as_secs_f64() });
    let replaced = {
        // kept while its plans are
        let c = shared.caches.lock().unwrap();
        kept_as.0.iter().all(|&(k, s, _)| c.plans.iter().any(|p| p.0 == (k, s))).then(|| keep_estimate(&c, &shared.estimates, kept_as, estimate.clone()))
    };
    drop(replaced);
    let taker = cell.lock().unwrap().take();
    if let Some(mut taker) = taker {
        let _ = taker(Streamed::Done { estimate: estimate.clone() });
    }
    (shared.sink)(LiveEvent::Estimated { revision: rev, estimate, streamed: true });
}

fn cached<T: Clone>(v: &[((u64, u64), T)], k: (u64, u64)) -> Option<T> {
    v.iter().find(|e| e.0 == k).map(|e| e.1.clone())
}

/// One revision's levels; returns early (silently) when a newer input came.
struct Trace {
    heights: Vec<(f64, f64)>,
    zs: Vec<f64>,
    /// Per layer: its regions unchanged since the part's last cut, or None: to trace.
    reuse: Vec<Option<CutLayer>>,
    /// The part's last cut: what a layer still to trace shows meanwhile (stale).
    last: Option<Arc<CutLayers>>,
    /// The layer the viewer looks at: the part streams down from it, then up.
    focus: Option<usize>,
    traced: Vec<Option<CutLayer>>,
    /// The content of the last cut `reuse` keeps layers of (a field's or a mesh's, no tool) and
    /// that cut's layer count; why there is none.
    from: Result<(u64, usize), String>,
}
impl Trace {
    fn todo(&self, k: usize) -> bool {
        self.reuse[k].is_none() && self.traced[k].is_none()
    }
    /// The cut as far as it is traced, and the layers in it that are the last cut's (stale).
    fn cut(&self) -> (CutLayers, Vec<(u32, u32)>) {
        let mut stale: Vec<(u32, u32)> = Vec::new();
        let cut = self.heights.iter().enumerate().map(|(k, &(z, h))| {
            let regions = match (&self.traced[k], &self.reuse[k]) {
                (Some(r), _) | (None, Some(r)) => r.clone(),
                (None, None) => {
                    match stale.last_mut() {
                        Some(s) if s.1 == k as u32 => s.1 += 1,
                        _ => stale.push((k as u32, k as u32 + 1)),
                    }
                    self.last.as_ref().and_then(|c| c.get(k)).filter(|l| l.0.to_bits() == z.to_bits() && l.1.to_bits() == h.to_bits()).map(|l| l.2.clone()).unwrap_or_default()
                }
            };
            (z, h, regions)
        }).collect();
        (cut, stale)
    }
}

/// An adaptive part's key and the warp available to its windows in this revision.
struct PartGrade {
    key: u64,
    warp: Option<Arc<grade::Graded>>,
}

/// One revision's planned model layers per part. What a part not planned whole has goes to the
/// caches when the revision ends, also when it is superseded or dropped at a suspension: the next
/// revision of that content plans only the rest.
struct Assembled<'a> {
    shared: &'a Shared,
    keys: Vec<u64>,
    settings: u64,
    parts: Vec<Assembly>,
    grades: Vec<Option<PartGrade>>,
}
impl std::ops::Deref for Assembled<'_> {
    type Target = Vec<Assembly>;
    fn deref(&self) -> &Vec<Assembly> { &self.parts }
}
impl std::ops::DerefMut for Assembled<'_> {
    fn deref_mut(&mut self) -> &mut Vec<Assembly> { &mut self.parts }
}
impl Drop for Assembled<'_> {
    fn drop(&mut self) {
        let kept: Vec<_> = self.parts.iter_mut().enumerate().filter(|(_, a)| !a.iter().all(|r| r.iter().all(Option::is_none))).map(|(i, a)| (i, std::mem::take(a))).collect();
        // Copies that shared their owner's plan take its warp too. Superseded revisions keep
        // any warp already solved, just as they keep their completed layers.
        let grades = self.grades.iter_mut().map(|part| {
            let part = part.as_mut()?;
            Some((part.key, part.warp.take().or_else(|| grade::held(part.key))?))
        }).collect();
        // A panic while the caches were held: recovery discards them anyway.
        let Ok(mut c) = self.shared.caches.lock() else { return };
        let old_grades = std::mem::replace(&mut c.grades, grades);
        // What they replace is let go of with the caches let go.
        let mut replaced = Vec::with_capacity(kept.len());
        for (i, a) in kept {
            if c.partial_plans.len() <= i { c.partial_plans.resize_with(i + 1, || None); }
            replaced.push(c.partial_plans[i].replace((self.keys[i], self.settings, a)));
        }
        drop(c);
        drop((old_grades, replaced));
    }
}

async fn run(shared: &Arc<Shared>, snap: &Snapshot, phase: &std::sync::atomic::AtomicU8) {
    let t0 = std::time::Instant::now();
    let rev = snap.revision;
    let stale = || (snap.checkpoint)() || over_memory(shared);
    let cancel = &stale as &(dyn Fn() -> bool + Sync);
    let sink = &shared.sink;
    let input = snap.input.as_ref().expect("snapshot built");
    let fail = |e: String| {
        sink(LiveEvent::Failed { revision: rev, error: e.clone() });
        if let Some(tag) = snap.settle {
            sink(LiveEvent::Settled { tag, revision: rev, result: Err(e), seconds: t0.elapsed().as_secs_f64() });
        }
    };
    // The code this revision runs: what it makes is kept under it.
    let code = loaded_code();
    shared.run_under(code);
    let made = shared.setups.lock().unwrap().as_ref().filter(|(k, _)| *k == input.key()).map(|(_, s)| s.clone());
    let setup = match made {
        Some(s) => s,
        None => match (input.setup)(None) {
            Ok(s) => {
                let s = Arc::new(s);
                *shared.setups.lock().unwrap() = Some((input.key(), s.clone()));
                s
            }
            Err(e) => return fail(e),
        },
    };
    let setup = &setup;
    let job = match prepare_job(setup) {
        Ok(j) => Arc::new(j),
        Err(e) => return fail(e),
    };
    for p in input.plate.parts.iter().filter(|p| !p.print.is_empty()) {
        if let Err(e) = makepad_fab_toolpath::check(&job.with_overrides(&p.print)) {
            return fail(format!("{}: {e}", p.name));
        }
    }
    let materials = Materials::from_job(&job);
    let meter = Meter::new(shared, snap.settle, rev);
    // The plate as placed (for the plate-level checks).
    let parts: Vec<Part> = input.plate.parts.iter().enumerate().map(|(i, p)| match (input.pieces.as_ref().and_then(|c| c.parts.get(i)), snap.xf.get(i).copied().flatten()) {
        (Some(piece), _) => piece.part.clone(),
        (None, Some(m)) => placed_part(p, &m),
        (None, None) => p.clone(),
    }).collect();
    let placed = Arc::new(Plate { excluded: Vec::new(), parts, settings: input.plate.settings.clone() });
    let mut filaments = Vec::with_capacity(placed.parts.len());
    for (i, p) in placed.parts.iter().enumerate() {
        match part_filament(setup, i, p) {
            Ok(f) => filaments.push(f),
            Err(e) => return fail(e),
        }
    }
    // The cuts and model layers keep under what a part's slice reads and the code that makes
    // them (`model`), its plan with its supports under that and what the supports read and the
    // code that makes them (`settings`). What the sink shows is made under the slice's settings
    // and the nozzle map: under others it shows nothing of the old, so every part's cut and plan
    // goes to it again. A filament moved to a nozzle like its own is sent again with its new
    // nozzle, from the same cuts and plans. A support setting, or a live patch of the support
    // code, keeps all the sink shows but the supports.
    let keys = paths_keys(&job, &input.plate, code);
    let (model, settings) = (keys.part, plan_key(&keys));
    let view = shown_key(&job, model);
    if shared.settings.swap(view, Ordering::Relaxed) != view {
        let mut c = shared.caches.lock().unwrap();
        c.sent_cut.fill(u64::MAX);
        c.sent_plan.fill(u64::MAX);
        c.sent_supports.fill(u64::MAX);
        let partial = std::mem::take(&mut c.partial_plans);
        drop(c);
        drop(partial);
        sink(LiveEvent::Settings);
    }
    let n = placed.parts.len();
    let part_work = {
        let mut work = shared.part_work.lock().unwrap();
        work.resize_with(n, || Arc::new(PartWork::default()));
        work.clone()
    };
    // What grew on slots the plate has no more is let go of with the lock released.
    let gone = {
        let mut c = shared.caches.lock().unwrap();
        c.sent_cut.resize(n, u64::MAX);
        c.sent_plan.resize(n, u64::MAX);
        c.sent_supports.resize(n, u64::MAX);
        c.trees_kept.resize_with(n, Default::default);
        let kept = n.min(c.grown.len());
        c.grown.split_off(kept)
    };
    drop(gone);
    // Each part's contour source in its own frame, and where that frame sits.
    let mut slots: Vec<Slot> = Vec::with_capacity(n);
    // Per slot, the map from the evaluated plate to its frame (what carries its embedded items).
    let mut to_slot: Vec<Affine> = Vec::with_capacity(n);
    for (i, p) in input.plate.parts.iter().enumerate() {
        let xf = input.pieces.as_ref().and_then(|p| p.parts[i].xf).or_else(|| snap.xf.get(i).copied().flatten()).unwrap_or(IDENTITY);
        // An exact part's contours come from its print-tolerance mesh, in the plate's frame.
        let fine = p.print_mesh.as_ref().and_then(|f| match (f.0)() {
            Ok(mesh) => Some(mesh),
            Err(e) => {
                eprintln!("[fab] {}: no print-tolerance mesh, slicing its display mesh: {e}", p.name);
                None
            }
        });
        let frame = input.frames.as_ref().and_then(|f| f.get(i)).filter(|f| fine.is_none() && f.local.triangles.len() == p.mesh.triangles.len());
        let (src, lift, mut off) = match (fine, frame) {
            (Some(mesh), _) => (Arc::new(mesh), 0.0, [0.0, 0.0]),
            (None, Some(f)) => (f.local.clone(), f.offset[2], [f.offset[0], f.offset[1]]),
            (None, None) => (Arc::new(p.mesh.clone()), 0.0, [0.0, 0.0]),
        };
        let base = off;
        // A move in X and Y stays an offset; anything else is a new mesh.
        let rest = match xy_only(&xf) {
            Some((dx, dy)) => {
                off = [off[0] + dx, off[1] + dy];
                IDENTITY
            }
            None => xf,
        };
        let lin = (rest != IDENTITY).then_some(rest);
        // A pure SDF part is cut from its field, also while it is turned, lifted or scaled
        // live (the transform goes onto the field, about the part's place on the plate).
        let moved = match (frame.and_then(|f| f.field.as_ref()), lin) {
            (Some(f), Some(m)) => {
                let (t, back) = (affine_translate([off[0], off[1], lift]), affine_translate([-off[0], -off[1], -lift]));
                f.moved(&affine_mul(&back, &affine_mul(&m, &t)))
            }
            _ => None,
        };
        let field = moved.as_ref().or(frame.and_then(|f| f.field.as_ref()).filter(|_| lin.is_none()));
        if let Some(field) = field {
            // Its slopes before its source reads them: a first read may measure the shape, and
            // takes the cells the edit leaves from the last field's.
            let last = shared.caches.lock().unwrap().slot_fields.get(i).and_then(Option::as_ref).map(|(pf, ..)| pf.clone());
            field.slope();
            field.prove_bands(last.as_ref());
            // Its non-planar top, when the job asks for one and the head is known (cached by field, lift,
            // settings and head; made here on the engine's thread).
            let mut np_k = 0;
            let own = (!p.print.is_empty()).then(|| job.with_overrides(&p.print));
            let np = match (np_settings(own.as_ref().unwrap_or(&job)), input.np_head.as_ref()) {
                (Some(s), Some(head)) => {
                    np_k = np_key(&s, head);
                    let k = (field.content, lift.to_bits(), np_k);
                    let hit = shared.caches.lock().unwrap().np_plans.iter().find(|e| e.0 == k).map(|e| e.1.clone());
                    match hit {
                        Some(p) => Some(p),
                        None => match np_plan_of(field, lift, &s, head) {
                            Ok(p) => {
                                let p = Arc::new(p);
                                let mut c = shared.caches.lock().unwrap();
                                c.np_plans.push((k, p.clone()));
                                bound(&mut c.np_plans, 2 * n.max(1));
                                Some(p)
                            }
                            Err(e) => return fail(e),
                        },
                    }
                }
                _ => None,
            };
            let sliced = Frame { local: src.clone(), offset: [off[0], off[1], lift], field: Some(field.clone()) };
            let src = match field_source(field, lift) {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            slots.push(Slot { src, mesh: None, np, key: field_key(field.content, filaments[i], lift, np_k, &p.print), content: field.content, off: Pt::from_mm(off[0], off[1]), sliced, glued: false });
            // A field turned live turns about its place; an XY move stays the slot's offset.
            to_slot.push(affine_mul(&affine_translate([-base[0], -base[1], 0.0]), &lin.unwrap_or(IDENTITY)));
            continue;
        }
        let key = (Arc::as_ptr(&src) as usize, lift.to_bits(), bits(&rest));
        let hit = shared.caches.lock().unwrap().meshes.iter().find(|e| e.0 == key && Arc::ptr_eq(&e.1, &src)).map(|e| (e.2.clone(), e.3));
        let (mesh, mkey) = match hit {
            Some(h) => h,
            None => {
                // Not under the lock: building and hashing reads the whole mesh.
                let mut pm = (*src).clone();
                if let Some(m) = lin {
                    // In printer coordinates: the frame's offset first, then the transform.
                    for q in &mut pm.positions {
                        let w = [q[0] + off[0], q[1] + off[1], q[2] + lift];
                        *q = std::array::from_fn(|r| m[r][0] * w[0] + m[r][1] * w[1] + m[r][2] * w[2] + m[r][3]);
                    }
                } else if lift != 0.0 {
                    for q in &mut pm.positions {
                        q[2] += lift;
                    }
                }
                let part = Part { mesh: pm, print_mesh: None, paint: None, ..p.clone() };
                let mesh = painted_mesh(&part, input.paint.get(i));
                let mkey = hash_mesh(&mesh);
                let mesh = Arc::new(MeshSource::new(mesh));
                let mut c = shared.caches.lock().unwrap();
                c.meshes.push((key, src.clone(), mesh.clone(), mkey));
                bound(&mut c.meshes, 4 * n.max(1));
                (mesh, mkey)
            }
        };
        // (A transformed part was baked into printer coordinates above.)
        let off = if lin.is_some() { [0.0, 0.0] } else { off };
        let sliced = Frame { local: Arc::new(PartMesh { positions: mesh.mesh.verts.clone(), triangles: mesh.mesh.tris.clone() }), offset: [off[0], off[1], 0.0], field: None };
        slots.push(Slot { src: mesh.clone(), mesh: Some(mesh), np: None, key: with_overrides(mesh_key(mkey, filaments[i]), &p.print), content: mkey, off: Pt::from_mm(off[0], off[1]), sliced, glued: false });
        // A mesh turned live is baked in printer coordinates.
        to_slot.push(lin.unwrap_or_else(|| affine_translate([-base[0], -base[1], 0.0])));
    }
    // Parts painted by position: the source splits by the paint at the plate point a slot's point
    // was at when the model was evaluated (a live move or turn carries the paint along).
    {
        let regions = crate::plate::PaintRegions::default();
        for (i, p) in input.plate.parts.iter().enumerate() {
            let Some(sp) = p.paint.as_ref() else { continue };
            let xf = input.pieces.as_ref().and_then(|p| p.parts[i].xf).or_else(|| snap.xf.get(i).copied().flatten()).unwrap_or(IDENTITY);
            let Some(back) = affine_invert(&xf) else { continue };
            let (ox, oy) = (slots[i].off.x_mm(), slots[i].off.y_mm());
            let src = crate::plate::painted(p, slots[i].src.clone(), filaments[i] as u32 + 1, move |q| {
                let w = [q[0] + ox, q[1] + oy, q[2]];
                std::array::from_fn(|r| back[r][0] * w[0] + back[r][1] * w[1] + back[r][2] * w[2] + back[r][3])
            }, |q| {
                let w: [f64; 3] = std::array::from_fn(|r| xf[r][0] * q[0] + xf[r][1] * q[1] + xf[r][2] * q[2] + xf[r][3]);
                [w[0] - ox, w[1] - oy, w[2]]
            }, fab_plate::stretch(&back), Some((&regions, &[xf.concat(), vec![ox, oy]].concat())));
            if src.painted() || sp.support_block.is_some() || sp.support_enforce.is_some() || !sp.settings.is_empty() {
                // the paint is the same in the slot's frame however far it moved in X and Y
                let key = slots[i].key ^ paint_key(sp).rotate_left(17);
                slots[i] = Slot { src, mesh: None, key, ..slots[i].clone() };
            } else if sp.no_trees.is_some() {
                // No-trees areas alone: the part cuts as it is, and only its supports read them
                // (they key its plan, `settings_of`).
                slots[i] = Slot { src, ..slots[i].clone() };
            }
        }
    }
    // A test plate: each slot is cut to its cube, printing the print's layers from the cube's up.
    let mut piece_stacks: Vec<Option<Arc<crate::piece::Stack>>> = vec![None; n];
    if let Some(pieces) = &input.pieces {
        let groups = input.plate.groups();
        let mut used = Vec::with_capacity(n);
        for i in 0..n {
            let (piece, whole) = (&pieces.parts[i], slots[i].clone());
            let own = (!input.plate.parts[i].print.is_empty()).then(|| job.with_overrides(&input.plate.parts[i].print));
            let params = Params::from_job(own.as_ref().unwrap_or(&job), filaments[i]);
            // The print's layers: a glued group's are its own.
            let group: Option<&Vec<usize>> = groups.iter().find(|g| g.contains(&i)).filter(|_| params.exp.adaptive.is_some());
            let print = match group {
                Some(g) => {
                    let first = slots[g[0]].off;
                    let members: Vec<crate::glue::Member> = g.iter().map(|&j| crate::glue::Member { src: slots[j].src.clone(), at: slots[j].off.sub(first), state: crate::glue::state_of(filaments[j], filaments[g[0]]) }).collect();
                    makepad_fab_toolpath::layers::object_heights(&crate::glue::Glued::new(members), &params)
                }
                None => makepad_fab_toolpath::layers::object_heights(&*whole.src, &params),
            };
            let stack = Arc::new(crate::piece::Stack::of(&print, &params, piece.cube.bottom(), piece.cube.top()));
            let (planes, cut) = {
                let mut c = shared.caches.lock().unwrap();
                let k = (whole.key, model);
                let planes = match c.piece_planes.iter().find(|e| e.0 == k) {
                    Some(e) => e.1.clone(),
                    None => {
                        let planes = Arc::new(crate::piece::Planes::new(shared.planes_cut.clone()));
                        c.piece_planes.push((k, planes.clone()));
                        planes
                    }
                };
                (planes, cached(&c.cuts, k))
            };
            // What the whole part's slice has cut already is that plane's contour.
            if let Some(cut) = cut {
                planes.seed(cut.iter().map(|(z, h, l)| (makepad_fab_toolpath::cut_z(&params, *z, *h), l.regions.clone())), &stack);
            }
            let stack = Arc::new(stack.grounded(&print, &params, &*whole.src, &planes, &piece.cube, whole.off));
            used.push((whole.key, model));
            let off = whole.off.add(Pt::from_mm(piece.shift[0], piece.shift[1]));
            let source = crate::piece::PieceSource::new(whole.src.clone(), planes, piece.cube, whole.off, stack.clone(), piece.xf);
            let key = source.key(whole.key);
            let sliced = Frame { offset: [off.x_mm(), off.y_mm(), whole.sliced.offset[2] - stack.fall()], ..whole.sliced.clone() };
            piece_stacks[i] = Some(stack);
            slots[i] = Slot { src: Arc::new(source), mesh: None, np: None, key, content: key, off, sliced, glued: true };
        }
        shared.caches.lock().unwrap().piece_planes.retain(|e| used.contains(&e.0));
    }
    // Parts that print as one body: a group's first part slices the union of its parts'
    // layers, a later part owning where it overlaps an earlier, the others none of their own
    // ([`crate::glue`]).
    let crate::glue::Gluing { glued, apart } = crate::glue::groups(&input.plate.groups(), n, |g| crate::glue::apart(g, |i| &*slots[i].src, |i| input.plate.parts[i].object.as_str()));
    for g in &glued {
        let first = slots[g[0]].clone();
        let at: Vec<Pt> = g.iter().map(|&i| slots[i].off.sub(first.off)).collect();
        let members = g.iter().zip(&at).map(|(&i, &at)| crate::glue::Member { src: slots[i].src.clone(), at, state: crate::glue::state_of(filaments[i], filaments[g[0]]) }).collect();
        // What the group slices: its parts, each where it sits in the first's frame.
        let mut key = 0xcbf2_9ce4_8422_2325u64;
        for (&i, d) in g.iter().zip(&at) {
            for x in [slots[i].key, d.x as u64, d.y as u64] {
                key = (key ^ x).wrapping_mul(0x100_0000_01b3);
            }
        }
        for (k, &i) in g.iter().enumerate().skip(1) {
            let s = slots[i].clone();
            let carried = key ^ (k as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
            slots[i] = Slot { src: Arc::new(crate::glue::Carried::of(&*s.src)), mesh: None, np: None, key: carried, content: carried, glued: true, ..s };
        }
        slots[g[0]] = Slot { src: Arc::new(crate::glue::Glued::new(members)), mesh: None, np: None, key, content: key, glued: true, ..first };
    }
    // Items printed into a part (`insert(.., embed: true)`): its slot (its glued group's, when it
    // has one) cuts their cavities ([`makepad_fab_slicer::embed`]). An item rides its host, so it
    // is carried into the slot's frame by the host's map; the slot's key takes the items there,
    // so a host that only moved on the bed keeps its cut and plan.
    if input.pieces.is_none() {
        for (host, items) in input.plate.embeds() {
            let own = (!input.plate.parts[host].print.is_empty()).then(|| job.with_overrides(&input.plate.parts[host].print));
            let thickest = makepad_fab_toolpath::layers::thickest(&Params::from_job(own.as_ref().unwrap_or(&job), filaments[host]));
            let (src, key) = embedded(slots[host].src.clone(), slots[host].key, &to_slot[host], &items, thickest);
            // Like a glued group's, no edit of its own reaches only some of its layers: an edit of
            // a field host is not diffed against its last cut, which its items cut too.
            slots[host] = Slot { src, mesh: None, key, content: key, glued: true, ..slots[host].clone() };
        }
    }
    // Copies of one mesh on one filament share the representative's content
    // (one cut, one plan), placed at their own offsets. (Field parts of equal
    // content share theirs through the content key.)
    {
        let ms: Vec<usize> = (0..n).filter(|&i| slots[i].mesh.is_some()).collect();
        let keys: Vec<u64> = ms.iter().map(|&i| slots[i].key).collect();
        let hit = shared.caches.lock().unwrap().classes.as_ref().filter(|(k, _)| *k == keys).map(|(_, c)| c.clone());
        let classes = match hit {
            Some(c) => c,
            None => {
                let refs: Vec<&Mesh> = ms.iter().map(|&i| &slots[i].mesh.as_ref().expect("a mesh slot").mesh).collect();
                let c = makepad_fab_slicer::instance::classes(&refs);
                shared.caches.lock().unwrap().classes = Some((keys, c.clone()));
                c
            }
        };
        for (k, (r, d)) in classes.into_iter().enumerate() {
            let (i, r) = (ms[k], ms[r]);
            if r != i && filaments[r] == filaments[i] && input.plate.parts[r].print == input.plate.parts[i].print && no_trees_key(&input.plate.parts[r]) == no_trees_key(&input.plate.parts[i]) {
                let rep = slots[r].clone();
                let off = copy_plan_offset(slots[i].off, d);
                let sliced = Frame { offset: [off.x_mm(), off.y_mm(), 0.0], ..rep.sliced.clone() };
                slots[i] = Slot { off, sliced, ..rep };
            }
        }
    }
    // Contours and final model layers share the view's band order. Cached
    // plans need no work; copies share the first slot of the same content.
    let part_params: Vec<_> = (0..n).map(|i| {
        let own = input.plate.parts.get(i).filter(|p| !p.print.is_empty()).map(|p| job.with_overrides(&p.print));
        Params::from_job(own.as_ref().unwrap_or(&job), filaments[i])
    }).collect();
    let precut = |i: usize, field: bool, by_z| {
        let slot = &slots[i];
        // The cut key under the code that makes parts: a live patch of the grading never finds
        // a warp the code before it solved.
        let source = Precut { key: Some(with_code(slot.key, code.part)), mesh: slot.src.clone(), field, bounds: slot.src.bounds(), by_z, np: slot.np.clone() };
        ObjectInput { overrides: placed.parts[i].print.clone(), name: placed.parts[i].name.clone(), source: Box::new(source), filament: filaments[i] }
    };
    let planning = !snap.moving || snap.settle.is_some();
    // Each part's plan settings: the plate's with its own support settings and no-trees areas.
    let mut settings_of: Vec<u64> = input.plate.parts.iter().map(|p| part_settings(settings, &p.print, no_trees_key(p))).collect();
    // A glued group's supports read its parts' no-trees areas too.
    for g in &glued {
        settings_of[g[0]] = g[1..].iter().fold(settings_of[g[0]], |h, &j| (h ^ settings_of[j]).wrapping_mul(0x100_0000_01b3));
    }
    let owners: Vec<usize> = (0..n).map(|i| (0..i).find(|&j| slots[j].key == slots[i].key && settings_of[j] == settings_of[i]).unwrap_or(i)).collect();
    let mut plans: Vec<Option<Arc<ObjectPlan>>> = slots.iter().zip(&settings_of).map(|(slot, &settings)| cached(&shared.caches.lock().unwrap().plans, (slot.key, settings))).collect();
    let dirty: Vec<bool> = (0..n).map(|i| planning && plans[i].is_none() && owners[i] == i).collect();
    // A part is edited when its content or its slice's settings differ from the last
    // revision's: its viewed layer is drawn even once a newer input came, and it goes first.
    let edited: Vec<bool> = {
        let mut last = shared.run_keys.lock().unwrap();
        let edited = slots.iter().enumerate().map(|(i, s)| last.get(i) != Some(&(s.key, model))).collect();
        *last = slots.iter().map(|s| (s.key, model)).collect();
        edited
    };
    let mut planes: Vec<Option<Vec<(f64, f64)>>> = Vec::with_capacity(n);
    let mut layer_count: Vec<usize> = Vec::with_capacity(n);
    for (i, slot) in slots.iter().enumerate() {
        match cached(&shared.caches.lock().unwrap().cuts, (slot.key, model)) {
            Some(c) => {
                layer_count.push(c.len());
                planes.push(None);
            }
            None => {
                let h = makepad_fab_toolpath::layers::object_heights(&*slot.src, &part_params[i]);
                layer_count.push(h.len());
                planes.push(Some(h));
            }
        }
    }
    // What part `i`'s supports cost, in the meter's units: none when it has none to plan.
    let support_units = |i: usize| {
        let p = &part_params[i];
        if p.support.is_some() || p.normal_support.is_some() { layer_count[i] as f64 * SUPPORT_LAYER_UNITS } else { 0.0 }
    };
    {
        let traced: f64 = planes.iter().enumerate().filter(|(i, _)| owners[*i] == *i).map(|(_, p)| p.as_ref().map_or(0.0, |h| h.len() as f64)).sum();
        let planned: f64 = layer_count.iter().enumerate().filter(|(i, _)| dirty[*i]).map(|(_, &l)| l as f64).sum();
        let supports: f64 = (0..n).filter(|&i| dirty[i] && owners[i] == i).map(|i| support_units(i)).sum();
        let layers = if snap.settle.is_some() { layer_count.iter().copied().max().unwrap_or(0) as f64 } else { 0.0 };
        meter.stage(0, &[(0, traced + planned), (1, supports), (2, layers)]);
    }
    let current: Vec<u64> = slots.iter().map(|s| s.key).collect();
    let (before, kept_settings) = {
        let mut c = shared.caches.lock().unwrap();
        if c.plate_keys != current {
            c.keys_before = std::mem::replace(&mut c.plate_keys, current.clone());
        }
        let mut before = c.keys_before.clone();
        let mut kept_settings = settings_of.clone();
        match &input.pieces {
            None => c.whole_plate = Some((input.plate_id, current.clone(), settings_of.clone())),
            // A test plate keeps the very plate it is cut from, and nothing older.
            Some(p) => {
                if c.whole_plate.as_ref().is_some_and(|w| w.0 != p.whole_plate) {
                    c.whole_plate = None;
                }
                if let Some((_, keys, settings)) = &c.whole_plate {
                    before.extend_from_slice(keys);
                    kept_settings.extend_from_slice(settings);
                }
            }
        }
        (before, kept_settings)
    };
    // Each part's layers to trace (none when its cut is cached): an edited field only the
    // planes its edit reaches, the others as its last cut had them.
    let mut traces: Vec<Option<Trace>> = Vec::with_capacity(n);
    let mut cuts: Vec<Option<Arc<CutLayers>>> = Vec::with_capacity(n);
    for (i, slot) in slots.iter().enumerate() {
        if let Some(c) = cached(&shared.caches.lock().unwrap().cuts, (slot.key, model)) {
            cuts.push(Some(c));
            traces.push(None);
            continue;
        }
        let params = &part_params[i];
        let heights = planes[i].take().unwrap_or_else(|| makepad_fab_toolpath::layers::object_heights(&*slot.src, params));
        let zs: Vec<f64> = heights.iter().map(|&(z, h)| makepad_fab_toolpath::cut_z(params, z, h)).collect();
        let focus = snap.focus_z.map(|z| heights.partition_point(|(pz, _)| *pz < z));
        // An edited field re-traces only the layers its edit reaches (a glued group's cut reads
        // its other parts too).
        let field = slot.sliced.field.as_ref().filter(|_| !slot.glued);
        let lift = slot.sliced.offset[2];
        // (each condition that traces every layer says so, for [`LiveEngine::part_kept`])
        let prev: Result<(Arc<CutLayers>, Vec<(f64, f64)>, u64), String> = match field {
            Some(f) => {
                let last = shared.caches.lock().unwrap().slot_fields.get(i).cloned().flatten();
                (|| {
                    let (pf, pl, ps, pc) = last.as_ref().ok_or("no last field")?;
                    if *ps != model {
                        return Err("other slice settings".to_string());
                    }
                    if *pl != lift.to_bits() {
                        return Err(format!("lifted from {:.3} to {lift:.3} mm", f64::from_bits(*pl)));
                    }
                    let (was, now) = (&pf.to_frame, &f.to_frame);
                    if !(0..3).all(|r| (0..3).all(|k| was[r][k].to_bits() == now[r][k].to_bits())) {
                        return Err("turned or scaled on the plate".to_string());
                    }
                    if !(0..3).all(|r| was[r][3].to_bits() == now[r][3].to_bits()) {
                        let d: [f64; 3] = std::array::from_fn(|r| now[r][3] - was[r][3]);
                        return Err(format!("moved on the plate by [{:.4} {:.4} {:.4}] mm", d[0], d[1], d[2]));
                    }
                    let reach = 2.0 * SdfParams::default().coarse * 2f64.sqrt() + SdfParams::default().fine;
                    let ranges = crate::sdf_diff::changed_layers(pf, f, reach).ok_or("the field changed where its change has no bounds")?;
                    Ok((pc.clone(), ranges, pf.content))
                })()
            }
            // An edited mesh of the same triangles re-cuts only the planes its moved vertices reach.
            None => (|| {
                let m = slot.mesh.as_ref().ok_or(if slot.glued { "glued" } else { "neither a field nor a mesh" })?;
                if slot.src.painted() {
                    return Err("a painted mesh".to_string());
                }
                let last = shared.caches.lock().unwrap().slot_meshes.get(i).cloned().flatten();
                let (pm, pk, ps, pc) = last.as_ref().ok_or("no last mesh")?;
                if *ps != model {
                    return Err("other slice settings".to_string());
                }
                let ranges = crate::mesh_diff::changed_z(&pm.mesh, &m.mesh).ok_or("other triangles")?;
                Ok((pc.clone(), ranges, *pk))
            })(),
        };
        let reuse: Vec<Option<CutLayer>> = match &prev {
            Ok((pc, ranges, _)) => zs.iter().enumerate().map(|(k, &z)| {
                let p = pc.get(k).filter(|p| p.0.to_bits() == heights[k].0.to_bits() && p.1.to_bits() == heights[k].1.to_bits())?;
                let zf = z - lift;
                (!ranges.iter().any(|&(a, b)| zf >= a && zf <= b)).then(|| p.2.clone())
            }).collect(),
            Err(_) => vec![None; zs.len()],
        };
        if owners[i] == i { meter.done(reuse.iter().filter(|r| r.is_some()).count() as f64); }
        let last = shared.caches.lock().unwrap().last_cuts.get(i).cloned().flatten().filter(|(s, _)| *s == model).map(|(_, c)| c);
        // What a superseded revision of this content traced already. The cache keeps it: a
        // revision dropped between its polls never runs `keep`, and taking the layers out would
        // lose them, so an unchanged part traced again and again.
        let (traced, taken_over) = {
            let c = shared.caches.lock().unwrap();
            match c.partial_cuts.get(i).and_then(|p| p.as_ref()).filter(|(k, s, l)| *k == slot.key && *s == model && l.len() == zs.len()) {
                Some((_, _, l)) => (l.clone(), true),
                None => (vec![None; zs.len()], false),
            }
        };
        if taken_over && owners[i] == i { meter.done(traced.iter().zip(&reuse).filter(|(t, r)| t.is_some() && r.is_none()).count() as f64); }
        traces.push(Some(Trace { heights, zs, reuse, last, focus, traced, from: prev.map(|p| (p.2, p.0.len())) }));
        cuts.push(None);
    }
    // Superseded: the next revision of part `i`'s content goes on from what is traced.
    let keep = |i: usize, t: &mut Trace| {
        if t.traced.iter().all(|l| l.is_none()) {
            return;
        }
        let traced = std::mem::take(&mut t.traced);
        let mut c = shared.caches.lock().unwrap();
        if c.partial_cuts.len() <= i {
            c.partial_cuts.resize(i + 1, None);
        }
        // What an earlier revision kept is in the cache already: the run's layers join it.
        match c.partial_cuts[i].as_mut().filter(|(k, s, l)| *k == slots[i].key && *s == model && l.len() == traced.len()) {
            Some((_, _, cached)) => for (at, r) in cached.iter_mut().zip(traced) { if r.is_some() { *at = r; } },
            None => c.partial_cuts[i] = Some((slots[i].key, model, traced)),
        }
    };
    let keep_all = |traces: &mut [Option<Trace>]| {
        for (i, t) in traces.iter_mut().enumerate() {
            if let Some(t) = t.as_mut() {
                keep(i, t);
            }
        }
    };
    // A part's contours as the sink was last given them.
    let send_cut = |i: usize, key: u64, layers: Arc<CutLayers>, stale: Vec<(u32, u32)>, again: bool| {
        let send = {
            let mut c = shared.caches.lock().unwrap();
            let s = again || c.sent_cut[i] != key;
            c.sent_cut[i] = key;
            s
        };
        if send {
            sink(LiveEvent::PartCut { part: i, key, layers, stale });
        }
    };
    // Every part's cut as traced so far (the rest as its last cut had them): (cut, its stale
    // layers, traced now).
    let cuts_now = |traces: &[Option<Trace>]| -> Vec<Option<(Arc<CutLayers>, Vec<(u32, u32)>, bool)>> {
        (0..slots.len()).map(|i| match (&traces[i], &cuts[i]) {
            (Some(t), _) => {
                let (cut, stale) = t.cut();
                Some((Arc::new(cut), stale, true))
            }
            (None, Some(c)) => Some((c.clone(), Vec::new(), false)),
            (None, None) => None,
        }).collect()
    };
    // Those cuts to the sink, then `Contours`.
    let send_cuts = |now: Vec<Option<(Arc<CutLayers>, Vec<(u32, u32)>, bool)>>| {
        for (i, (slot, cut)) in slots.iter().zip(now).enumerate() {
            if let Some((cut, stale, again)) = cut {
                send_cut(i, slot.key, cut, stale, again);
            }
        }
        sink(LiveEvent::Contours { revision: rev, parts: slots.iter().map(|s| (s.key, s.off)).collect(), objects: input.plate.parts.iter().map(|p| p.object.clone()).collect(), age: snap.touched.elapsed().as_secs_f64() });
    };
    // The focus first, then down to 0, then up to the top. Every dirty part gets one band per
    // round. Moving the focus reorders only the layers not planned yet; a newer input stops
    // after the focus chunk and keeps the contours already traced for the next revision.
    struct Bands {
        pending: VecDeque<(usize, usize)>,
        /// Model layers already planned (only traced during a grab).
        done: Vec<bool>,
    }
    impl Bands {
        fn add(&mut self, a: usize, b: usize, down: bool) {
            let mut runs = Vec::new();
            let mut k = a;
            while k < b {
                while k < b && self.done[k] { k += 1; }
                let lo = k;
                while k < b && !self.done[k] { k += 1; }
                if lo < k { runs.push((lo, k)); }
            }
            if down { runs.reverse(); }
            self.pending.extend(runs);
        }

        fn reorder(&mut self, focus: Option<usize>) {
            self.pending.clear();
            let total = self.done.len();
            if total == 0 { return; }
            let focus = focus.unwrap_or(0).min(total - 1);
            self.add(focus, focus + 1, false);
            let (mut lo, mut size) = (focus, 1);
            while lo > 0 {
                let a = lo.saturating_sub(size);
                self.add(a, lo, true);
                lo = a;
                size = (size * 2).min(MAX_BAND_LAYERS);
            }
            let (mut hi, mut size) = (focus + 1, 1);
            while hi < total {
                let b = (hi + size).min(total);
                self.add(hi, b, false);
                hi = b;
                size = (size * 2).min(MAX_BAND_LAYERS);
            }
        }
    }
    let heights: Vec<Vec<(f64, f64)>> = (0..n).map(|i| match (&traces[i], &cuts[i]) {
        (Some(t), _) => t.heights.clone(),
        (_, Some(c)) => c.iter().map(|l| (l.0, l.1)).collect(),
        _ => unreachable!("every slot has layer planes"),
    }).collect();
    let grades = (0..n).map(|i| {
        part_params[i].sparse_opts.adaptive?;
        let key = grade_key(&precut(i, false, HashMap::new()), &job, &heights[i])?;
        Some(PartGrade { key, warp: None })
    }).collect();
    let mut assembled = Assembled { shared, keys: current.clone(), settings: model, parts: vec![Vec::new(); n], grades };
    // The model layers a superseded revision of the same content planned, and the sink still
    // shows, are not planned again.
    {
        let shown = shared.shown.lock().unwrap().clone();
        let mut c = shared.caches.lock().unwrap();
        for i in 0..n {
            if !dirty[i] || shown.get(i) != Some(&slots[i].key) { continue; }
            let total = layer_count[i];
            let Some(p) = c.partial_plans.get_mut(i).and_then(|p| p.take_if(|(k, s, a)| *k == slots[i].key && *s == model && a.iter().all(|r| r.len() == total))) else { continue };
            let mut held = p.2;
            // A plate pass stops between a layer's regions, and the plan makes only the whole
            // layer: one held in part is planned again, with its sent regions.
            for k in 0..total {
                if !held.iter().all(|r| r[k].is_some()) {
                    for r in &mut held { r[k] = None; }
                }
            }
            assembled[i] = held;
        }
    }
    for i in 0..n {
        if assembled[i].is_empty() { continue; }
        let planned: usize = assembled[i].iter().map(|r| r.iter().filter(|l| l.is_some()).count()).sum();
        meter.more(assembled[i].len().saturating_sub(1) as f64 * layer_count[i] as f64);
        meter.done(planned as f64);
    }
    let mut bands: Vec<Option<Bands>> = (0..n).map(|i| {
        if owners[i] != i || (!dirty[i] && traces[i].is_none()) { return None; }
        let a = &assembled[i];
        let mut b = Bands { pending: VecDeque::new(), done: (0..layer_count[i]).map(|k| !a.is_empty() && a.iter().all(|r| r[k].is_some())).collect() };
        b.reorder(snap.focus_z.map(|z| heights[i].partition_point(|(pz, _)| *pz < z)));
        Some(b)
    }).collect();
    // An edit of a field or of a mesh's vertices plans only the layers whose contour context it
    // reached: the others are the last plan's, when that plan was made of the cut this edit's
    // contours start from. Plain parts and parts painted by position (not region paint, vase or
    // non-planar top, which reach across layers), whose box kept the infill's centre. A part
    // painted by position also plans again the layers its paint edit reached. A part with
    // supports keeps the model layers its last plan was finished from (the plan's own layers hold
    // its supports), and its supports are planned anew.
    // The kept layers are copied and sent once every part's viewed layer is drawn: copying a
    // plan's paths and making their records takes longer than tracing and planning that layer.
    let mut kept: Vec<Option<(usize, Vec<usize>, LastModel)>> = (0..n).map(|_| None).collect();
    for i in 0..n {
        let (Some(t), Some(band)) = (traces[i].as_ref(), bands[i].as_mut()) else { continue };
        if !dirty[i] || !assembled[i].is_empty() {
            continue;
        }
        // Each condition that keeps none says so ([`LiveEngine::part_kept`]).
        let decided = (|| -> Result<(usize, Vec<usize>, LastModel), String> {
            let (from, old_total) = t.from.clone().map_err(|why| format!("no last cut: {why}"))?;
            let params = &part_params[i];
            let slot = &slots[i];
            if params.exp.vase.is_some() {
                return Err("vase mode reads every layer".into());
            }
            if slot.np.is_some() {
                return Err("a non-planar top".into());
            }
            // (a part with regions' print settings plans its layers with them: their key is the slot's)
            if input.plate.parts[i].paint.as_ref().is_some_and(|sp| !sp.settings.is_empty()) {
                return Err("regions of print settings".into());
            }
            let total = heights[i].len();
            let field = match slot.sliced.field {
                Some(_) => field_key(from, filaments[i], slot.sliced.offset[2], 0, &input.plate.parts[i].print),
                None => with_overrides(mesh_key(from, filaments[i]), &input.plate.parts[i].print),
            };
            // The last plan's key, and the heights where the paint can split differently.
            let (prev_key, repainted): (u64, Vec<(f64, f64)>) = if slot.src.painted() {
                let sp = input.plate.parts[i].paint.as_ref().filter(|_| slot.src.paint_at().is_some()).ok_or("region paint reads every layer")?;
                let old = shared.caches.lock().unwrap().slot_paints.get(i).cloned().flatten().ok_or("no last paint")?;
                // the paint samples at the layers' own heights (no live turn or lift), splits each
                // layer apart (no raft reordering its streams) and holds no support channel
                if !snap.xf.get(i).copied().flatten().is_none_or(|m| xy_only(&m).is_some()) {
                    return Err("turned or lifted live".into());
                }
                if params.raft_layers > 0 {
                    return Err("a raft".into());
                }
                let channels = |p: &fab_plate::SpatialPaint| p.support_block.is_some() || p.support_enforce.is_some();
                if channels(sp) || channels(&old) {
                    return Err("support paint".into());
                }
                // a colour on another tool prints anew wherever it shows
                let own = filaments[i] as u32 + 1;
                if sp.states(own) != old.states(own) {
                    return Err("a colour moved to or from another tool".into());
                }
                let heights = sp.changed(&old).ok_or("the paint changed where its change has no bounds")?;
                (field ^ paint_key(&old).rotate_left(17), heights)
            } else {
                (field, Vec::new())
            };
            let prev = cached(&shared.caches.lock().unwrap().plans, (prev_key, settings_of[i])).ok_or("the last plan is not kept")?;
            // The infill's centre and reach are the pivot box's in X and Y (a field's own tree's), and
            // the part's box stays as it was; its height sets the first layer's auto brim.
            let ((lo, hi), pivot) = (slot.src.bounds(), slot.src.pivot_box());
            let same = |a: &[f64], b: &[f64]| a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits());
            let xy = |lo: &[f64], hi: &[f64]| format!("[{:.3} {:.3}]..[{:.3} {:.3}]", lo[0], lo[1], hi[0], hi[1]);
            if !(same(&prev.info.min[..2], &lo[..2]) && same(&prev.info.max[..2], &hi[..2])) {
                return Err(format!("its XY box moved, {} to {}", xy(&prev.info.min, &prev.info.max), xy(&lo, &hi)));
            }
            if !(same(&prev.pivot.0, &pivot.0) && same(&prev.pivot.1, &pivot.1)) {
                return Err(format!("its pivot box moved in XY, {} to {}", xy(&prev.pivot.0, &prev.pivot.1), xy(&pivot.0, &pivot.1)));
            }
            let same_height = (prev.info.max[2] - prev.info.min[2]).to_bits() == (hi[2] - lo[2]).to_bits();
            // A painted plan's streams (regions), each of the cut's layers. A taller or shorter plain
            // part keeps the layers below its new or old top.
            let fits = |regions: usize| regions > 0 && (regions == 1 || old_total == total) && (regions > 1) == slot.src.painted();
            let painted_at = |j: usize| {
                let (z, h) = heights[i][j];
                repainted.iter().any(|&(a, b): &(f64, f64)| z - h <= b && z >= a)
            };
            let keep: Vec<usize> = (0..total.min(old_total)).filter(|&k| {
                let (a, b) = plan_reads(params, k, k + 1, total);
                (k > 0 || same_height) && (a..b).all(|j| t.reuse[j].is_some() && !painted_at(j))
            }).collect();
            if keep.is_empty() {
                let traced = (0..total).filter(|&j| t.reuse[j].is_none()).count();
                let repainted = (0..total).filter(|&j| painted_at(j)).count();
                return Err(format!("the edit reached every layer's reads ({traced} of {total} layers traced, {repainted} repainted)"));
            }
            if params.support.is_some() {
                // the model layers the plan was finished from, kept for a later revision of its content
                let whole = |(key, settings, a): &(u64, u64, Assembly)| *key == prev_key && *settings == model && fits(a.len()) && a.iter().all(|r| r.len() == old_total && r.iter().all(Option::is_some));
                let (_, _, held) = shared.caches.lock().unwrap().partial_plans.get_mut(i).and_then(|p| p.take_if(|p| whole(p))).ok_or("the model layers its last plan was finished from are not kept")?;
                Ok((held.len(), keep, LastModel::Held(held)))
            } else {
                // height-ordered, a painted plan's regions stable at each height
                let layers: Vec<usize> = prev.layers.iter().enumerate().filter(|(_, l)| !l.2.support_only).map(|(k, _)| k).collect();
                let regions = layers.len() / old_total.max(1);
                if !fits(regions) || layers.len() != regions * old_total {
                    return Err("its regions changed".into());
                }
                Ok((regions, keep, LastModel::Plan(prev, layers)))
            }
        })();
        let why = match decided {
            Ok((regions, keep, last)) => {
                for &k in &keep {
                    band.done[k] = true;
                }
                band.reorder(snap.focus_z.map(|z| heights[i].partition_point(|(pz, _)| *pz < z)));
                kept[i] = Some((regions, keep, last));
                String::new()
            }
            Err(why) => why,
        };
        *part_work[i].kept_none.lock().unwrap() = why;
    }
    let field_ms = snap.started.elapsed().as_secs_f64() * 1e3;
    let mut focus_z = snap.focus_z;
    let focus_of = |i: usize, z: Option<f64>| z.map(|z| heights[i].partition_point(|(pz, _)| *pz < z));
    // The requested model layers and the contours the window must be given.
    let reads_of = |i: usize, a: usize, b: usize, held: bool| {
        let total = heights[i].len();
        let params = &part_params[i];
        let (a, b) = if params.exp.vase.is_some() { (0, total) } else { (a, b) };
        let src = &slots[i].src;
        let position_paint = src.paint_at().is_some();
        let (ra, rb) = if src.painted() && !position_paint {
            (0, total)
        } else {
            let (ra, rb) = if held { plan_reads_held(params, a, b, total) } else { plan_reads(params, a, b, total) };
            // Paint by position reads the outlines within its skin, which can reach past the context.
            let (pa, pb) = if position_paint { paint_reads(params, &heights[i], a, b) } else { (ra, rb) };
            (ra.min(pa), rb.max(pb))
        };
        (a, b, ra, rb)
    };
    let regions_at = |i: usize, k: usize, t: Option<&Trace>| -> Regions {
        match (t, &cuts[i]) {
            (Some(t), _) => t.traced[k].as_ref().or(t.reuse[k].as_ref()).expect("the window's contours are traced").regions.clone(),
            (_, Some(c)) => c[k].2.regions.clone(),
            _ => unreachable!("window has contours"),
        }
    };
    // Edited parts first, their viewed layers drawn even once a newer input came; then the other
    // parts' viewed layers, ahead of all other work, stopped by a newer input; then the rest.
    let order: Vec<usize> = (0..n).filter(|&i| edited[i]).chain((0..n).filter(|&i| !edited[i])).collect();
    // The viewed layers: each part's focus band, alone, first.
    let mut planned_views = Vec::new();
    for &i in &order {
        let protect = edited[i];
        if !protect {
            phase.store(live_trace::FOCUS, Ordering::Release);
            if stale() { return keep_all(&mut traces); }
        }
        let newest = shared.state.lock().unwrap().focus_z;
        if newest.map(f64::to_bits) != focus_z.map(f64::to_bits) {
            focus_z = newest;
            for (i, b) in bands.iter_mut().enumerate() {
                let focus = focus_of(i, focus_z);
                if let Some(t) = traces[i].as_mut() { t.focus = focus; }
                if let Some(b) = b { b.reorder(focus); }
            }
        }
        let Some(band) = bands[i].as_mut() else { continue };
        let total = band.done.len();
        let focus_k = focus_of(i, focus_z).unwrap_or(0).min(total.saturating_sub(1));
        let part = &mut assembled.grades[i];
        if let Some(part) = part {
            // The previous revision or an earlier part of this one may hold the warp.
            part.warp = grade::held(part.key);
        }
        let held = part.as_ref().is_some_and(|g| g.warp.is_some());
        let missing = part.as_mut().filter(|_| !held && dirty[i] && !band.pending.is_empty());
        // A viewed layer planned already (by a superseded revision of this content, or kept from
        // the last plan) is this revision's viewed layer with no work: said once it is sent.
        let mut window = if band.done.get(focus_k) == Some(&true) {
            planned_views.push(LiveEvent::Viewed { revision: rev, part: i, layer: focus_k as u32, field_ms, slice_ms: 0.0, plan_ms: 0.0 });
            None
        } else { band.pending.pop_front().map(|(a, b)| reads_of(i, a, b, held)) };
        // A missing warp needs every contour once, including when only later windows remain.
        let trace_range = if missing.is_some() { 0..total }
            else if let Some((_, _, a, b)) = window { a..b }
            else { continue };
        let trace_at = std::time::Instant::now();
        let chunk_cancel = if protect { None } else { Some(cancel) };
        if let Some(t) = traces[i].as_mut() {
            let ks = trace_range.filter(|&k| t.todo(k)).collect();
            if !live_trace::trace(shared, &slots[i], t, ks, chunk_cancel, &meter, &part_work[i]) {
                return keep_all(&mut traces);
            }
        }
        let slice_ms = trace_at.elapsed().as_secs_f64() * 1e3;
        let plan_at = std::time::Instant::now();
        send_cuts(cuts_now(&traces));
        if let Some(part) = missing {
            let contours = (0..total).map(|k| regions_at(i, k, traces[i].as_ref()).to_vec()).collect();
            let o = precut(i, true, HashMap::new());
            let ctl = par::Ctl { cancel: chunk_cancel, focus: Some(focus_k), ..Default::default() };
            let Ok(warp) = solve_grade(ctl, &o, &job, &heights[i], contours) else { return keep_all(&mut traces) };
            part.warp = Some(warp.expect("a keyed adaptive part has a warp"));
            window = window.map(|(a, b, _, _)| reads_of(i, a, b, true));
        }
        let Some((a, b, read_a, read_b)) = window else {
            part_work[i].plan_ns.fetch_add(plan_at.elapsed().as_nanos() as u64, Ordering::Relaxed);
            continue;
        };
        let focus_events = Mutex::new(Vec::new());
        if dirty[i] {
            let params = &part_params[i];
            let by_z = (read_a..read_b).map(|k| {
                let (z, h) = heights[i][k];
                (makepad_fab_toolpath::cut_z(params, z, h).to_bits(), regions_at(i, k, traces[i].as_ref()))
            }).collect();
            let slot = &slots[i];
            let key = slot.key;
            let o = precut(i, slot.sliced.field.is_some(), by_z);
            if let Err(e) = check_inputs(&job, std::slice::from_ref(&o)) { return fail(e); }
            #[cfg(test)]
            {
                let inject = {
                    let mut part = shared.panic_part.lock().unwrap();
                    if *part == Some(i) { part.take(); true } else { false }
                };
                if inject {
                    makepad_fab_slicer::par::fan(2, &|worker| {
                        if worker == 1 {
                            let _cache = shared.caches.lock().unwrap();
                            panic!("injected part job panic");
                        }
                    });
                }
            }
            let first = assembled[i].is_empty();
            if first { forget_sent(shared, i); }
            let regions = std::sync::atomic::AtomicUsize::new(0);
            let emit = |e: Emit| match e {
                Emit::Supports | Emit::Tree { .. } => {}
                Emit::Regions { count } => {
                    regions.store(count, Ordering::Relaxed);
                    if first && (protect || !stale()) {
                        meter.more(count.saturating_sub(1) as f64 * total as f64);
                        sink(LiveEvent::PartRegions { part: i, key, regions: count as u32, total: total as u32 });
                    }
                }
                Emit::Layer { region, index, z, layer, .. } => {
                    part_work[i].planned.fetch_add(1, Ordering::Relaxed);
                    if !protect && stale() { return; }
                    meter.done(1.0);
                    let pass = if protect { Pass::Viewed } else { Pass::Focus };
                    focus_events.lock().unwrap().push(LiveEvent::PartLayers { revision: rev, pass, part: i, key, region: region as u32, layers: vec![(index as u32, LivePaths::of(index, z, layer, &job))], total: total as u32 });
                }
            };
            let ctl = par::Ctl { cancel: chunk_cancel, focus: Some(focus_k), emit: Some(&emit), tick: None, phase: None };
            let Ok(layers) = makepad_fab_toolpath::plan_window(ctl, i, &o, &job, &heights[i], a, b) else {
                return keep_all(&mut traces);
            };
            let regions = regions.load(Ordering::Relaxed);
            assert_eq!(layers.len(), (b - a) * regions, "every model layer returned once per region");
            part_work[i].plan_ns.fetch_add(plan_at.elapsed().as_nanos() as u64, Ordering::Relaxed);
            if first { assembled[i] = vec![vec![None; total]; regions]; }
            assert_eq!(assembled[i].len(), regions, "region inventory stays fixed across windows");
            // Returned layers are height-ordered, with regions stable at equal z.
            for (k, layer) in layers.into_iter().enumerate() {
                let at = &mut assembled[i][k % regions][a + k / regions];
                assert!(at.is_none(), "a model layer is planned only once");
                *at = Some(layer);
            }
        }
        sink(LiveEvent::Viewed { revision: rev, part: i, layer: a as u32, field_ms, slice_ms, plan_ms: plan_at.elapsed().as_secs_f64() * 1e3 });
        for event in focus_events.into_inner().unwrap() { sink(event); }
        band.done[a..b].fill(true);
    }
    // Then the kept layers into the plans and to the sink. A revision stopped before they are
    // sent never holds them: what it planned is what the sink shows.
    for (i, kept) in kept.into_iter().enumerate() {
        let Some((regions, keep, mut last)) = kept else { continue };
        let (key, total) = (slots[i].key, heights[i].len());
        if assembled[i].is_empty() {
            assembled[i] = vec![vec![None; total]; regions];
            forget_sent(shared, i);
            meter.more(regions.saturating_sub(1) as f64 * total as f64);
            sink(LiveEvent::PartRegions { part: i, key, regions: regions as u32, total: total as u32 });
        }
        assert_eq!(assembled[i].len(), regions, "the kept layers' regions are the plan's");
        for (r, at) in assembled[i].iter_mut().enumerate() {
            let planned = keep.iter().map(|&k| {
                let l = last.take(k, r, regions);
                assert!(at[k].is_none(), "a kept layer is not planned again");
                let paths = LivePaths::of(k, l.0, &l.2, &job);
                at[k] = Some(l);
                (k as u32, paths)
            }).collect();
            sink(LiveEvent::PartLayers { revision: rev, pass: Pass::Kept, part: i, key, region: r as u32, layers: planned, total: total as u32 });
        }
        meter.done((keep.len() * regions) as f64);
        part_work[i].kept.fetch_add(keep.len() as u64, Ordering::Relaxed);
    }
    for event in planned_views { sink(event); }
    phase.store(live_trace::REST, Ordering::Release);
    if stale() { return keep_all(&mut traces); }
    // The rest is polled at its own urgency.
    live_trace::Yield::default().await;
    // The rest of every part on one plate: whichever part's most wanted ready task ranks highest
    // runs next, so no part waits for another or for a band. The layers still to trace are cut
    // by the plate's tasks: every one of them (the cut is cached whole).
    focus_z = shared.state.lock().unwrap().focus_z;
    let mut members: Vec<usize> = Vec::new();
    let mut inputs: Vec<ObjectInput> = Vec::new();
    for &i in &order {
        let Some(band) = bands[i].as_ref() else { continue };
        let open = dirty[i] && band.done.iter().any(|d| !d) || traces[i].as_ref().is_some_and(|t| (0..t.zs.len()).any(|k| t.todo(k)));
        if !open { continue; }
        // Contours outside the cut come from the part's own source (a CPU part's trace tasks).
        let o = precut(i, false, HashMap::new());
        if dirty[i] {
            if let Err(e) = check_inputs(&job, std::slice::from_ref(&o)) { return fail(e); }
        }
        members.push(i);
        inputs.push(o);
    }
    let part_plans: Vec<PartPlan> = members.iter().zip(&inputs).map(|(&i, o)| PartPlan::new(i, o, &job, heights[i].clone())).collect();
    // The layers a part has (traced, reused, or its cached cut) go into its plan before the plate
    // is made, on every thread: the plate takes them as made and cuts only the others. A planned
    // part reads, around each run of layers still to plan, that run's plan_reads; a part only
    // traced (during a grab) reads none it has.
    let mut known: Vec<(usize, usize, CutLayer)> = Vec::new();
    for (p, &i) in members.iter().enumerate() {
        if !dirty[i] { continue; }
        let done = &bands[i].as_ref().unwrap().done;
        let mut reads = vec![false; done.len()];
        let held = assembled.grades[i].as_ref().is_some_and(|g| g.warp.is_some());
        let mut k = 0;
        while k < done.len() {
            if done[k] { k += 1; continue; }
            let a = k;
            while k < done.len() && !done[k] { k += 1; }
            let (_, _, ra, rb) = reads_of(i, a, k, held);
            reads[ra..rb].fill(true);
        }
        for k in (0..done.len()).filter(|&k| reads[k]) {
            let layer = match (&traces[i], &cuts[i]) {
                (Some(t), _) => t.traced[k].clone().or_else(|| t.reuse[k].clone()),
                (_, Some(c)) => Some(c[k].2.clone()),
                _ => unreachable!("every slot has layer planes"),
            };
            if let Some(l) = layer { known.push((p, k, l)); }
        }
    }
    {
        let next = std::sync::atomic::AtomicUsize::new(0);
        makepad_fab_slicer::par::fan(par::threads(), &|_| loop {
            let j = next.fetch_add(1, Ordering::Relaxed);
            let Some((p, k, layer)) = known.get(j) else { return };
            part_plans[*p].put_trace(*k, layer.regions.to_vec(), None);
        });
    }
    let plate = stages::Plate::new(members.iter().enumerate().map(|(p, &i)| {
        let band = bands[i].as_ref().unwrap();
        let mut targets = if dirty[i] { part_plans[p].layer_targets((0..band.done.len()).filter(|&k| !band.done[k])) } else { Vec::new() };
        if let Some(t) = traces[i].as_ref() {
            targets.extend((0..t.zs.len()).filter(|&k| t.todo(k)).map(Task::trace));
        }
        PlatePart { plan: &part_plans[p], targets, urgency: u8::from(!edited[i]), focus: focus_of(i, focus_z) }
    }).collect());
    let traces_mx = Mutex::new(std::mem::take(&mut traces));
    // Every 20 ms one worker moves the focus (the viewer scrolled) and shows the cut as traced.
    let checked_ms = AtomicU64::new(0);
    let focus_bits = AtomicU64::new(focus_z.map_or(u64::MAX, f64::to_bits));
    let cut_changed = AtomicBool::new(false);
    // One worker polls at a time; the others go on with their work.
    let polling = AtomicBool::new(false);
    let poll = || {
        let Some(_one) = claim(&polling) else { return };
        let now = t0.elapsed().as_millis() as u64;
        if now >= checked_ms.load(Ordering::Relaxed) + 20 {
            checked_ms.store(now, Ordering::Relaxed);
            let z = shared.state.lock().unwrap().focus_z;
            let bits = z.map_or(u64::MAX, f64::to_bits);
            if focus_bits.swap(bits, Ordering::Relaxed) != bits {
                for (p, &i) in members.iter().enumerate() {
                    plate.set_focus(p, focus_of(i, z));
                }
            }
            if cut_changed.swap(false, Ordering::Relaxed) {
                let traced = cuts_now(&traces_mx.lock().unwrap());
                send_cuts(traced);
            }
        }
    };
    let cut_traced = |p: usize, k: usize, raw: &[ExPolygon]| {
        let i = members[p];
        shared.traced.fetch_add(1, Ordering::Relaxed);
        part_work[i].traced.fetch_add(1, Ordering::Relaxed);
        if let Some(t) = traces_mx.lock().unwrap()[i].as_mut() {
            t.traced[k] = Some(CutLayer { regions: Regions::from(raw.to_vec()) });
        }
        cut_changed.store(true, Ordering::Relaxed);
    };
    let first: Vec<bool> = members.iter().map(|&i| assembled[i].is_empty()).collect();
    for (p, &i) in members.iter().enumerate() {
        if dirty[i] && first[p] { forget_sent(shared, i); }
    }
    let regions: Vec<std::sync::atomic::AtomicUsize> = members.iter().map(|_| std::sync::atomic::AtomicUsize::new(0)).collect();
    // The layers the sink was sent as they were planned: this content keeps them when the plate
    // is cancelled, so a newer revision of it plans and sends the whole ones never again.
    let sent: Vec<Mutex<Vec<(usize, usize, (f64, f64, ObjectLayer))>>> = members.iter().map(|_| Mutex::new(Vec::new())).collect();
    let emit = |p: usize, e: Emit| {
        let i = members[p];
        // A part only traced (during a grab) shows no plan.
        if !dirty[i] { return; }
        let (key, total) = (slots[i].key, layer_count[i]);
        match e {
            Emit::Supports | Emit::Tree { .. } => {}
            Emit::Regions { count } => {
                regions[p].store(count, Ordering::Relaxed);
                if first[p] && !stale() {
                    meter.more(count.saturating_sub(1) as f64 * total as f64);
                    sink(LiveEvent::PartRegions { part: i, key, regions: count as u32, total: total as u32 });
                }
            }
            Emit::Layer { region, index, z, h, layer } => {
                part_work[i].planned.fetch_add(1, Ordering::Relaxed);
                if stale() { return; }
                meter.done(1.0);
                sink(LiveEvent::PartLayers { revision: rev, pass: Pass::Window, part: i, key, region: region as u32, layers: vec![(index as u32, LivePaths::of(index, z, layer, &job))], total: total as u32 });
                sent[p].lock().unwrap().push((region, index, (z, h, layer.clone())));
            }
        }
    };
    let tick = |n: usize| meter.done(n as f64);
    let ctl = PlateCtl { cancel: Some(cancel), emit: Some(&emit), traced: Some(&cut_traced), poll: Some(&poll), tick: Some(&tick), revision: rev };
    let outcome = plate.run(ctl);
    for (p, &i) in members.iter().enumerate() {
        part_work[i].plan_ns.fetch_add((plate.busy_s(p) * 1e9) as u64, Ordering::Relaxed);
        let sent = std::mem::take(&mut *sent[p].lock().unwrap());
        if sent.is_empty() { continue; }
        let regions = regions[p].load(Ordering::Relaxed);
        if first[p] { assembled[i] = vec![vec![None; layer_count[i]]; regions]; }
        assert_eq!(assembled[i].len(), regions, "region inventory stays fixed across the plate");
        for (region, index, layer) in sent {
            let at = &mut assembled[i][region][index];
            assert!(at.is_none(), "a model layer is planned only once");
            *at = Some(layer);
        }
    }
    drop(plate);
    traces = traces_mx.into_inner().unwrap();
    if outcome != PlateRun::Finished {
        return keep_all(&mut traces);
    }
    let mut done: Vec<Arc<CutLayers>> = Vec::with_capacity(n);
    for (i, slot) in slots.iter().enumerate() {
        let cut = if owners[i] != i {
            done[owners[i]].clone()
        } else {
            match (traces[i].as_mut(), cuts[i].take()) {
                (Some(t), _) => {
                    let ks = (0..t.zs.len()).filter(|&k| t.todo(k)).collect();
                    if !live_trace::trace(shared, &slots[i], t, ks, Some(cancel), &meter, &part_work[i]) {
                        return keep_all(&mut traces);
                    }
                    let cut: Arc<CutLayers> = Arc::new(t.cut().0);
                    let ck = (slot.key, model);
                    let field = slot.sliced.field.as_ref().filter(|_| !slot.glued).map(|f| (f.clone(), slot.sliced.offset[2].to_bits(), model, cut.clone()));
                    let mesh = slot.mesh.as_ref().filter(|_| slot.sliced.field.is_none() && !slot.src.painted()).map(|m| (m.clone(), slot.content, model, cut.clone()));
                    // What the cut replaces is let go of with the caches let go.
                    let (cuts, replaced) = {
                        let mut c = shared.caches.lock().unwrap();
                        let partial = c.partial_cuts.get_mut(i).and_then(Option::take);
                        c.cuts.push((ck, cut.clone()));
                        let field = field.and_then(|f| {
                            if c.slot_fields.len() <= i {
                                c.slot_fields.resize(i + 1, None);
                            }
                            c.slot_fields[i].replace(f)
                        });
                        let mesh = mesh.and_then(|m| {
                            if c.slot_meshes.len() <= i {
                                c.slot_meshes.resize(i + 1, None);
                            }
                            c.slot_meshes[i].replace(m)
                        });
                        (c.cuts.clone(), (partial, field, mesh))
                    };
                    let (keep, bytes) = kept_of(&cuts, &current, &before, &[model], |cut, seen| {
                        cut.iter().filter(|(_, _, l)| seen.insert(Arc::as_ptr(&l.regions) as *const ExPolygon as usize)).map(|(_, _, l)| regions_bytes(&l.regions)).sum()
                    });
                    let gone = let_go_of(&mut shared.caches.lock().unwrap().cuts, &cuts, &keep, "cuts", bytes);
                    drop((cuts, replaced, gone));
                    cut
                }
                (None, Some(c)) => c,
                (None, None) => unreachable!("every slot has a cut or a trace"),
            }
        };
        {
            let mut c = shared.caches.lock().unwrap();
            if c.last_cuts.len() <= i {
                c.last_cuts.resize(i + 1, None);
            }
            c.last_cuts[i] = Some((model, cut.clone()));
        }
        send_cut(i, slot.key, cut.clone(), Vec::new(), traces[i].is_some());
        done.push(cut);
    }
    let cuts = done;
    if stale() {
        return;
    }
    let placement: Vec<(u64, Pt)> = slots.iter().map(|s| (s.key, s.off)).collect();
    sink(LiveEvent::Contours { revision: rev, parts: placement.clone(), objects: input.plate.parts.iter().map(|p| p.object.clone()).collect(), age: snap.touched.elapsed().as_secs_f64() });
    if snap.moving && snap.settle.is_none() {
        return;
    }
    // Every dirty part's model layers are in. Supports run in the background
    // order, then the assembled plans merge at their placements.
    meter.stage(1, &[]);
    let planning: Vec<usize> = (0..slots.len()).filter(|&i| owners[i] == i && dirty[i]).collect();
    for (i, slot) in slots.iter().enumerate() {
        if stale() { return; }
        if owners[i] != i {
            plans[i] = plans[owners[i]].clone();
        } else if dirty[i] {
            let params = &part_params[i];
            let by_z = cuts[i].iter().map(|(z, h, r)| (makepad_fab_toolpath::cut_z(params, *z, *h).to_bits(), r.regions.clone())).collect();
            let outlines = cuts[i].iter().map(|l| l.2.regions.to_vec()).collect();
            let o = precut(i, slot.sliced.field.is_some(), by_z);
            if let Err(e) = check_inputs(&job, std::slice::from_ref(&o)) { return fail(e); }
            // The planner tells where it is within its share of this part's supports; their
            // records and the finished plan take the rest.
            let units = support_units(i);
            meter.part(&placed.parts[i].name, (planning.iter().position(|&j| j == i).map_or(0, |k| k as u32 + 1), planning.len() as u32));
            let phase = |what: &str, step: Option<(u32, u32)>, f: f64| meter.phase(units, what, step, f * SUPPORT_PLAN_SHARE);
            // The part's settings but for the trees the user deleted on it. When the sink shows this
            // content's supports planned under them, only the deleted trees changed: it keeps
            // showing those supports, less the trees just deleted, until the plan replaces them,
            // and nothing grows in the view meanwhile. When it shows them growing under them, they
            // grow on from what it shows, less the trees just deleted.
            let print = &placed.parts[i].print;
            let base = part_settings(settings, &print.iter().filter(|(k, _)| k != DROPPED_TIPS).cloned().collect::<Vec<_>>(), no_trees_key(&placed.parts[i]));
            let dropped = print.iter().find(|(k, _)| k == DROPPED_TIPS).map(|(_, v)| makepad_fab_toolpath::dropped_in(v)).unwrap_or_default();
            let (shown, grew) = {
                let mut c = shared.caches.lock().unwrap();
                let sent = std::mem::replace(&mut c.sent_supports[i], u64::MAX);
                // A plan shown less some trees: that plan, and its layers sent so.
                let (whole, before) = match (sent, c.pruned.get_mut(i).and_then(Option::take)) {
                    (PRUNED, Some(p)) => p,
                    (sent, _) => (sent, Vec::new()),
                };
                let trees = (c.sent_plan[i] == slot.key).then(|| cached(&c.trees, (slot.key, whole))).flatten().filter(|t| t.1 == base);
                ((sent, trees.and_then(|t| cached(&c.plans, (slot.key, whole)).map(|plan| (whole, plan, t.0, before)))), c.grown.get_mut(i).and_then(Option::take))
            };
            // What the sink shows growing goes on when it is this content's under these settings
            // and the sink has its trees; else it is let go of, the caches' lock released.
            let grew = grew.filter(|g| shown.0 == GROWING && g.0 == slot.key && g.1 == base && g.2.lock().unwrap().trees.is_some()).map(|g| g.2);
            let (quiet, carried) = (shown.1.is_some(), grew.is_some());
            // What the planner makes shows as it grows, in the live view only.
            let growing = Growing { shared, part: i, key: slot.key, filament: filaments[i], heights: &heights[i], job: &job, state: grew.unwrap_or_default(), flushing: AtomicBool::new(false), trees: Mutex::new(None) };
            if !quiet {
                let mut c = shared.caches.lock().unwrap();
                if carried {
                    c.sent_supports[i] = GROWING;
                }
                if c.grown.len() <= i { c.grown.resize_with(i + 1, || None); }
                c.grown[i] = Some((slot.key, base, growing.state.clone()));
            }
            let grown = |e: Emit| {
                if let Emit::Tree { layer, grown, frame, .. } = e {
                    if !stale() && (!quiet || matches!(grown, makepad_fab_toolpath::tree::Grown::Trees { .. })) {
                        growing.tell(layer, grown, frame);
                    }
                }
            };
            let ctl = par::Ctl { cancel: Some(cancel), phase: Some(&phase), emit: Some(&grown), ..Default::default() };
            match shown {
                (_, Some((whole, plan, trees, before))) => {
                    let (layers, now) = pruned(&plan, &trees, &dropped, &job, &before);
                    {
                        let mut c = shared.caches.lock().unwrap();
                        c.sent_supports[i] = PRUNED;
                        if c.pruned.len() <= i { c.pruned.resize_with(i + 1, || None); }
                        c.pruned[i] = Some((whole, now));
                    }
                    if !layers.is_empty() {
                        sink(LiveEvent::PartSupports { part: i, key: slot.key, layers, done: false });
                    }
                }
                _ if carried => growing.delete(&dropped),
                // The sink shows this content with other supports (another plan's, or one's as
                // it grew under others): it drops them, and their trees.
                (sent, None) if sent != u64::MAX => {
                    sink(LiveEvent::PartSupports { part: i, key: slot.key, layers: Vec::new(), done: false });
                    sink(LiveEvent::PartTrees { part: i, key: slot.key, trees: Arc::new([]) });
                }
                _ => {}
            }
            let Ok(outline) = makepad_fab_toolpath::plan_outline(ctl, &o, &job, &heights[i], outlines) else { return };
            let mut kept = shared.caches.lock().unwrap().trees_kept.get_mut(i).map(std::mem::take).unwrap_or_default();
            // A tree's report (what its rules check and hold measure tell) follows the toolpaths.
            let planned = makepad_fab_toolpath::plan_support_paths(ctl, i, &o, &job, &outline, &heights[i], &mut kept);
            *part_work[i].trees.lock().unwrap() += kept.take_work();
            // What the trees kept goes back for the next plan: a plan cut short keeps what it took
            // and what it made.
            if planned.is_err() || growing.trees.lock().unwrap().as_ref().is_some_and(|t| !t.is_empty()) {
                if let Some(k) = shared.caches.lock().unwrap().trees_kept.get_mut(i) {
                    *k = kept;
                }
            }
            let Ok(supports) = planned else { return };
            if stale() { return; }
            let mut next = layer_count[i];
            let count = supports.additions.len();
            // A separate layer's index comes next after the last, in order; their records side by side.
            let indices: Vec<Option<usize>> = supports.additions.iter().map(|(index, ..)| (params.raft_layers == 0 || *index != usize::MAX).then(|| {
                if *index == usize::MAX {
                    next += 1;
                    next - 1
                } else {
                    *index
                }
            })).collect();
            let finished = std::sync::atomic::AtomicUsize::new(0);
            let records = par::try_map(ctl, count, |k| {
                let (_, z, _, layer) = &supports.additions[k];
                let made = indices[k].map(|index| (index as u32, LivePaths::of(index, *z, layer, &job)));
                let done = finished.fetch_add(1, Ordering::Relaxed) + 1;
                meter.phase(units, "finishing", Some((done as u32, count as u32)), SUPPORT_PLAN_SHARE + (1.0 - SUPPORT_PLAN_SHARE) * done as f64 / count as f64);
                made
            });
            let Ok(records) = records else { return };
            let mut support_layers: Vec<(u32, LivePaths)> = records.into_iter().flatten().collect();
            // The part's layers stay for a later revision that plans only its supports again
            // (other support settings on the same slice).
            let held = std::mem::take(&mut assembled[i]);
            let layers = held.iter().flat_map(|r| r.iter().map(|l| l.clone().expect("every model layer planned"))).collect();
            let plan = Arc::new(makepad_fab_toolpath::finish_plan(&o, &job, &heights[i], &outline, layers, supports));
            if params.raft_layers > 0 {
                for (z, _, layer) in &plan.layers {
                    if stale() { return; }
                    if layer.support_only {
                        support_layers.push((next as u32, LivePaths::of(next, *z, layer, &job)));
                        next += 1;
                    }
                }
            }
            if stale() { return; }
            if layer_count[i] == 0 {
                sink(LiveEvent::PartRegions { part: i, key: slot.key, regions: 1, total: 0 });
            }
            sink(LiveEvent::PartSupports { part: i, key: slot.key, layers: support_layers, done: true });
            // The trees went to the sink as the planner placed them; a plan that placed none says so.
            let told = growing.trees.lock().unwrap().take();
            if told.is_none() {
                sink(LiveEvent::PartTrees { part: i, key: slot.key, trees: Arc::new([]) });
            }
            let trees: Arc<[TreeShape]> = told.unwrap_or_else(|| Arc::new([]));
            shared.planned.fetch_add(1, Ordering::Relaxed);
            meter.part_done(units);
            let sent = if stale() { u64::MAX } else { slot.key };
            // A stale revision's sink may show these or its growing ones: the next plan drops them.
            let supports = if stale() { GROWING } else { settings_of[i] };
            let paint = input.plate.parts[i].paint.clone().filter(|_| slot.src.paint_at().is_some());
            // What the plan replaces is let go of with the caches let go.
            let (all, replaced) = {
                let mut c = shared.caches.lock().unwrap();
                c.plans.push(((slot.key, settings_of[i]), plan.clone()));
                c.trees.push(((slot.key, settings_of[i]), (trees, base)));
                c.sent_plan[i] = sent;
                c.sent_supports[i] = supports;
                let grown = c.grown.get_mut(i).and_then(Option::take);
                if c.partial_plans.len() <= i { c.partial_plans.resize_with(i + 1, || None); }
                let partial = c.partial_plans[i].replace((slot.key, model, held));
                if c.slot_paints.len() <= i {
                    c.slot_paints.resize(i + 1, None);
                }
                let paint = std::mem::replace(&mut c.slot_paints[i], paint);
                (c.plans.clone(), (grown, partial, paint))
            };
            let (keep, bytes) = kept_of(&all, &current, &before, &kept_settings, |plan, seen| plan.weigh(seen));
            let gone = {
                let mut c = shared.caches.lock().unwrap();
                let gone = let_go_of(&mut c.plans, &all, &keep, "plans", bytes);
                (gone, follow_plans(&mut c, &shared.estimates))
            };
            drop((all, replaced, gone));
            plans[i] = Some(plan);
        }
        // Only cached plans need replaying; this revision's layers were sent once.
        if !dirty[i] { send_plan_if_new(shared, rev, i, slot.key, settings_of[i], &job); }
    }
    if stale() { return; }
    // The mesh checks of the plate as placed, and each part's contour checks, run beside the
    // merged plate and its G-code, which never read them: the toolpaths go out without waiting
    // for them, and only the settled result (what export and print take) waits for their verdict.
    let total_layers: u32 = cuts.iter().map(|c| c.len() as u32).max().unwrap_or(0);
    let checks = || {
        // A part above its object's rest has its floor proven here, for its gap (the evaluate
        // leaves it pending): beside the toolpaths, before the checks that tell it.
        for p in &placed.parts {
            p.prove_floor();
        }
        let checked = shared.caches.lock().unwrap().plate_checks.as_ref()
            .filter(|(key, at, _, _)| *key == input.key() && *at == placement)
            .map(|(_, _, report, contours)| (report.clone(), contours.clone()));
        let (plate_report, contour_issues) = match checked {
            Some(hit) => hit,
            None => {
                // The SDF parts' fields in plate coordinates: printability judges their
                // slopes by the field's gradient, not the display mesh's facets.
                type PlateField = Arc<dyn Fn(f64, f64, f64) -> f64 + Send + Sync>;
                let plate_fields: Vec<Option<PlateField>> = slots.iter().enumerate().map(|(i, s)| {
                    let pf = s.sliced.field.as_ref()?;
                    let g = pf.field().ok()?;
                    let (ox, oy, lift) = (s.off.x_mm(), s.off.y_mm(), s.sliced.offset[2]);
                    let field = move |x: f64, y: f64, z: f64| g(x - ox, y - oy, z - lift);
                    Some(match (input.pieces.as_ref().and_then(|p| p.parts.get(i)), &piece_stacks[i]) {
                        // a piece is that solid inside its cube, where it prints
                        (Some(piece), Some(stack)) => {
                            let inside = piece.solid(stack);
                            Arc::new(move |x: f64, y: f64, z: f64| field(x, y, z).max(inside([x, y, z]))) as PlateField
                        }
                        _ => Arc::new(field) as PlateField,
                    })
                }).collect();
                let field_of = |i: usize| -> Option<fab_printability::PartField<'_>> { plate_fields.get(i)?.as_deref().map(|f| f as fab_printability::PartField<'_>) };
                let mut contour_issues = Vec::new();
                for (i, Slot { key: mkey, off: d, .. }) in slots.iter().enumerate() {
                    if stale() { return None; }
                    let ck = (*mkey, model);
                    let hit = cached(&shared.caches.lock().unwrap().checks, ck);
                    let found = match hit {
                        Some(h) => h,
                        None => {
                            let mut v = fab_printability::contour_checks(&placed.parts[i].object, &cuts[i], &stale)?;
                            // A part in spiral-vase mode (its own setting or the plate's): one outline per layer.
                            let pj = if placed.parts[i].print.is_empty() { None } else { Some(job.with_overrides(&placed.parts[i].print)) };
                            let pj = pj.as_ref().unwrap_or(&job);
                            if pj.b("spiral_mode") {
                                let bottom = pj.cfg.i64_at("bottom_shell_layers", 0).max(1) as usize;
                                v.extend(fab_printability::vase_checks(&placed.parts[i].object, &cuts[i], bottom));
                            }
                            let v = Arc::new(v);
                            let mut c = shared.caches.lock().unwrap();
                            c.checks.push((ck, v.clone()));
                            bound(&mut c.checks, 4 * n.max(1));
                            v
                        }
                    };
                    for it in found.iter() {
                        let mut it = it.clone();
                        // A test print's piece has a brim of its own to give it.
                        if input.pieces.is_some() && it.kind == "plate_contact" {
                            it.hint = "Give the test print a brim of its own (Brim in its fields; fab.test_print set brim: true), or lay it on a larger face.".into();
                        }
                        it.at_mm[0] += d.x_mm();
                        it.at_mm[1] += d.y_mm();
                        it.footprint = it.footprint.translated([d.x_mm(), d.y_mm()]);
                        contour_issues.push(it);
                    }
                }
                // an object that prints its bodies apart, though it prints them as one, says so
                contour_issues.extend(apart.iter().map(|(g, why)| fab_printability::bodies_apart(&placed, g, why)));
                if stale() { return None; }
                let known = input.known_overlaps().filter(|_| snap.xf.iter().all(Option::is_none));
                let (plate_report, _) = fab_printability::analyse_plate_known(&input.machine, &placed, &materials, &field_of, known, &stale)?;
                if stale() { return None; }
                let (report, contours) = (Arc::new(plate_report), Arc::new(contour_issues));
                shared.caches.lock().unwrap().plate_checks = Some((input.key(), placement.clone(), report.clone(), contours.clone()));
                (report, contours)
            }
        };
        // A part's supports do not exist yet: whether one reaches its mid-air islands is the
        // sliced checks' to say.
        let awaits_supports = |i: &fab_printability::Issue| i.kind == "mid_air" && placed.parts.iter().any(|p| i.object.as_deref() == Some(p.object.as_str()) && materials.of_part(p).supports);
        let mut early = (*plate_report).clone();
        early.issues.extend(contour_issues.iter().filter(|i| !awaits_supports(i)).cloned());
        early.sort();
        sink(LiveEvent::Issues { revision: rev, report: Arc::new(early), done: 0, total: total_layers });
        Some((plate_report, contour_issues))
    };
    makepad_fab_toolpath::par::set_settling(rev);
    let merged_from: Vec<(u64, u64, Pt)> = slots.iter().zip(&settings_of).map(|(s, &settings)| (s.key, settings, s.off)).collect();
    let names: Vec<String> = placed.parts.iter().map(|p| p.name.clone()).collect();
    let paths = || {
        // Merged from the same plans, the toolpaths print with this revision's job: a filament
        // moved to a nozzle like its own changes only what the writer makes of them.
        let reuse = shared.caches.lock().unwrap().toolpaths.as_ref().filter(|(k, _)| *k == merged_from).map(|(_, t)| t.clone());
        let plate_paths = match reuse {
            Some(t) => t,
            None => {
                let plans = plans.iter().zip(&slots).map(|(p, s)| (p.clone().expect("every part planned"), s.off)).collect();
                let paths = Arc::new(PlatePaths { plans, names: names.clone(), merged: Default::default() });
                shared.caches.lock().unwrap().toolpaths = Some((merged_from.clone(), paths.clone()));
                paths
            }
        };
        let job = job.clone();
        let toolpaths = plate_paths.read();
        if stale() {
            return None;
        }
        // Held before they are sent: whoever sees them whole can have them estimated at once.
        drop(shared.sent_toolpaths.replace(Arc::new(Some(SentPaths { revision: rev, toolpaths: plate_paths.clone(), job: job.clone(), key: input.key(), setup: input.setup.clone(), merged_from: merged_from.clone() }))));
        sink(LiveEvent::Toolpaths { revision: rev, parts: placement.clone(), age: snap.touched.elapsed().as_secs_f64(), toolpaths: plate_paths });
        // Then what each part's supports tell, which neither the toolpaths nor the G-code read:
        // the report of trees planned ahead of it is measured from now on in the background,
        // beside the G-code. The settled slice does not wait for it; the sliced checks, which
        // Print waits for, end only once it is in ([`told_issues`]).
        let untold: Vec<Arc<ObjectPlan>> = plans.iter().flatten().filter(|plan| plan.skeleton.told().is_none()).cloned().collect();
        if !untold.is_empty() {
            in_background(shared, snap, move |stale, _| {
                for plan in &untold {
                    if plan.skeleton.tell(par::Ctl { cancel: Some(stale), ..Default::default() }).is_err() {
                        return;
                    }
                }
            });
        }
        // Level 3, on request: the exact G-code, on the tools that print.
        let result = snap.settle.map(|_| {
            meter.stage(2, &[]);
            let bound = bind(&toolpaths, job.filaments(), |kept| kept_job(shared, input.key(), code.part, &input.setup, kept))?;
            let (tp, job, setup, tools) = match bound {
                Some(b) => (b.toolpaths, b.job, b.setup, b.tools),
                None => (toolpaths.clone(), job.clone(), setup.clone(), (0..job.filaments()).collect()),
            };
            let boxes: Vec<_> = tp.objects.iter().map(|o| (o.min, o.max)).collect();
            finish((*job).clone(), tp.clone(), &names, &boxes, &|f: f32, _: &str| meter.fraction((f as f64 - 0.55) / 0.45), &stale).map(|r| (r, tp, setup, tools))
        });
        Some((toolpaths, job, result))
    };
    let (checked, made) = (Mutex::new(None), Mutex::new(None));
    makepad_fab_toolpath::par::map(2, |i| if i == 0 { *made.lock().unwrap() = paths() } else { *checked.lock().unwrap() = checks() });
    let (Some((plate_report, contour_issues)), Some((toolpaths, job, result))) = (checked.into_inner().unwrap(), made.into_inner().unwrap()) else { return };
    if stale() {
        return;
    }
    // The sliced checks find islands without material below themselves.
    let contour_issues: Vec<_> = contour_issues.iter().filter(|i| i.kind != "mid_air").cloned().collect();
    // What the supports leave out, as each part's plan says ([`told_issues`]). A glued group
    // stands as one: its lowest part tells whether it floats.
    let mut stands = (0..n).collect::<Vec<usize>>();
    for g in &glued {
        let floor = |j: usize| placed.parts[j].known_floor().unwrap_or_else(|| placed.parts[j].bounds().min[2]);
        stands[g[0]] = g.iter().copied().min_by(|&a, &b| floor(a).total_cmp(&floor(b)).then(a.cmp(&b))).unwrap_or(g[0]);
    }
    let reports: Vec<PartReport> = plans.iter().zip(&slots).enumerate().filter_map(|(i, (plan, slot))| Some(PartReport {
        plan: plan.clone()?,
        part: i,
        stands: stands[i],
        heights: heights[i].clone(),
        lowest: cuts[i].iter().position(|(_, _, c)| !c.regions.is_empty()),
        at: [slot.off.x_mm(), slot.off.y_mm()],
    })).collect();
    // Told now where every part's report is in; else after the settled slice, before the sliced
    // checks, which only `reports` left here wait for.
    let (contour_issues, reports) = if reports.iter().all(|r| r.plan.skeleton.told().is_some()) {
        let Some(issues) = told_issues(&input.machine, &placed, &materials, &reports, contour_issues, cancel) else { return };
        (issues, Vec::new())
    } else {
        (contour_issues, reports)
    };
    // Where each part keeps its trees out, as its plan says: the sliced checks tell none of the
    // overhang over it as uncovered.
    let kept: Vec<Option<fab_printability::KeptOut>> = plans.iter().zip(&slots).enumerate().map(|(i, (plan, slot))| {
        let shape = plan.as_ref()?.skeleton.shape();
        (!shape.keep_out.iter().all(Vec::is_empty)).then(|| fab_printability::KeptOut { regions: shape.keep_out.clone(), zs: heights[i].iter().map(|h| h.0).collect(), offset: slot.off })
    }).collect();
    // Each part's content with the input's own settings (the checks read each record's
    // nozzle), made by this code (the checks read its supports too): what its sliced checks are
    // kept under.
    let check_keys: Vec<u64> = slots.iter().map(|s| {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for x in [s.key, input.settings_id] { h = (h ^ x).wrapping_mul(0x100_0000_01b3); }
        with_code(with_code(with_code(h, code.part), code.supports), code.output)
    }).collect();
    let memo = shared.slice_memo.clone();
    if let (Some(tag), Some(result)) = (snap.settle, result) {
        // The settled slice goes out as soon as the G-code is made (the printer-safety check runs
        // on it in the app); the sliced printability report streams after as `Issues`, with
        // what the supports tell where it was not in yet, and Print waits for it whole.
        let mut early = (*plate_report).clone();
        early.issues.extend(contour_issues.iter().cloned());
        early.sort();
        let checking = (!reports.is_empty()).then(|| early.clone());
        // A slice on fewer tools than the input's is checked on them: its records name them.
        let mut printed = None;
        let result = result.map(|(mut r, tp, setup, tools)| {
            if tools.len() < job.filaments() {
                printed = Some((tp, Materials::from_job(&r.job), tools.clone()));
            }
            // The writer's per-line times: the settled slice is kept until its package is made.
            r.timeline.line_end = Vec::new();
            let sliced = slots.iter().map(|s| s.sliced.clone()).collect();
            let transformed = snap.xf.iter().any(|x| x.is_some());
            let layers = { let _t = makepad_fab_toolpath::prof::scope(makepad_fab_toolpath::prof::Stage::LayerTimeline); makepad_fab_toolpath::par::plate_span("layer timeline", || makepad_fab_gcode::layers::LayerTimeline::from_timeline(&mut r.timeline, shared.last_layers.lock().unwrap().as_ref())) };
            *shared.last_layers.lock().unwrap() = Some(layers.clone());
            let nonplanar = slots.iter().enumerate().filter_map(|(i, s)| s.np.clone().map(|p| (i, p, [s.off.x_mm(), s.off.y_mm()]))).collect();
            Arc::new(Settled { gcode: crate::gcode_text::GcodeText::new(r.gcode), timeline: r.timeline, layers, stats: r.stats, report: early, setup, tools, transformed, sliced, nonplanar, job: r.job, objects: r.objects, layer_filaments: r.layer_filaments })
        });
        if stale() {
            return;
        }
        // The app holds the settled slice: the issues read its timeline, no copy.
        let settled = result.as_ref().ok().cloned();
        meter.finish();
        sink(LiveEvent::Settled { tag, revision: rev, result, seconds: t0.elapsed().as_secs_f64() });
        if let Some(settled) = settled {
            let (input, report) = (input.clone(), (*plate_report).clone());
            // What the supports tell is read with the plate's materials; the sliced checks with
            // those of the tools that print.
            let told_with = materials.clone();
            let (toolpaths, materials, check_keys) = match printed {
                Some((tp, materials, tools)) => {
                    // Its findings name its own tools: kept apart from the input's.
                    let t = tools.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| (h ^ x as u64).wrapping_mul(0x100_0000_01b3));
                    (tp, materials, check_keys.iter().map(|&k| (k ^ t).wrapping_mul(0x100_0000_01b3)).collect())
                }
                None => (toolpaths, materials, check_keys),
            };
            // A report still to come: the sliced checks are under way from the settled slice on,
            // so Print waits for them, and they for it.
            if let Some(checking) = checking {
                sink(LiveEvent::Issues { revision: rev, report: Arc::new(checking), done: 0, total: toolpaths.layers.len() as u32 });
            }
            in_background(shared, snap, move |stale, sink| {
                let Some(contour_issues) = told_issues(&input.machine, &placed, &told_with, &reports, contour_issues, stale) else { return };
                stream_issues(&input.machine, &placed, &materials, report, &toolpaths.layers, &settled.timeline,
                    &mut settled.layers.records(), &contour_issues, (&check_keys, &memo, &kept), rev, stale, sink);
            });
        }
        return;
    }
    // Level 4 (live, lowest): printability on the toolpaths, layer by layer.
    let (input, report) = (input.clone(), (*plate_report).clone());
    in_background(shared, snap, move |stale, sink| {
        let Some(contour_issues) = told_issues(&input.machine, &placed, &materials, &reports, contour_issues, stale) else { return };
        stream_issues(&input.machine, &placed, &materials, report, &toolpaths.layers, &Timeline::default(), &mut records_of(&toolpaths, &job, stale), &contour_issues, (&check_keys, &memo, &kept), rev, stale, sink);
    });
}

/// A part's plan, and what the issues of its supports' report are read with ([`told_issues`]).
struct PartReport {
    plan: Arc<ObjectPlan>,
    /// The part, and the part it stands as: a glued group's lowest.
    part: usize,
    stands: usize,
    /// Its model layers (top z, height), and its lowest occupied one.
    heights: Vec<(f64, f64)>,
    lowest: Option<usize>,
    /// Where its plan's frame lies on the plate (mm).
    at: [f64; 2],
}

/// `issues` with what each part's supports leave out, as `reports` tell it: measured here where it
/// is not in yet (one measure at a time: this waits for one under way). None when `stale` stops it.
fn told_issues(machine: &Machine, placed: &Plate, materials: &Materials, reports: &[PartReport], mut issues: Vec<fab_printability::Issue>, stale: &(dyn Fn() -> bool + Sync)) -> Option<Vec<fab_printability::Issue>> {
    for r in reports {
        let told = r.plan.skeleton.tell(par::Ctl { cancel: Some(stale), ..Default::default() }).ok()?;
        issues.extend(fab_printability::support_issues(&placed.parts[r.part].object, &told, &r.heights, r.at));
        issues.extend(fab_printability::supported_placement(machine, &placed.parts[r.stands], materials, &told, r.lowest));
    }
    Some(issues)
}

/// Background checks waiting for a worker ([`in_background`]): what they read of their revision's
/// slice (its toolpaths, timeline and G-code) is held until they begin.
struct Queued {
    id: u64,
    revision: u64,
    checks: Box<dyn FnOnce(&Arc<Shared>) + Send>,
}

/// The printability checks of revision `snap` as background work ([`LiveEngine::set_background`]):
/// they never hold up a shown slice or other interactive work, and a newer input stops them.
///
/// The latest revision's checks win the queue. Checks of an older revision still waiting for a
/// worker go when these are queued: a newer revision exists only after an input made the older
/// ones' checkpoints stale, so they would stop at their first look, and until then they held
/// their revision's slice. Background work waits behind every slice, so while slices keep the
/// workers, each settle's checks used to stay queued with their slice: gigabytes on a big print.
/// The current revision's checks are never dropped, only superseded ones.
fn in_background(shared: &Arc<Shared>, snap: &Snapshot, checks: impl FnOnce(&(dyn Fn() -> bool + Sync), &(dyn Fn(LiveEvent) + Send + Sync)) + Send + 'static) {
    let spawn = shared.background.lock().unwrap().clone().unwrap_or_else(|| shared.spawn.clone());
    let (checkpoint, rev) = (snap.checkpoint.clone(), snap.revision);
    let checks: Box<dyn FnOnce(&Arc<Shared>) + Send> = Box::new(move |shared| {
        let stale = || checkpoint() || over_memory(shared);
        if let Err(p) = catch_unwind(AssertUnwindSafe(|| checks(&stale, &*shared.sink))) {
            panic_failed(shared, rev, None, p);
        }
    });
    let (id, superseded) = {
        let mut queued = shared.queued.lock().unwrap();
        queued.0 += 1;
        let id = queued.0;
        let (superseded, kept) = std::mem::take(&mut queued.1).into_iter().partition(|q: &Queued| q.revision < rev);
        queued.1 = kept;
        queued.1.push(Queued { id, revision: rev, checks });
        (id, superseded)
    };
    if !superseded.is_empty() {
        let revisions: Vec<u64> = superseded.iter().map(|q| q.revision).collect();
        eprintln!("[fab] revision {rev}'s background checks replace {} still queued of revision(s) {revisions:?}, which it superseded", superseded.len());
    }
    // Their slices go here, on this worker, not under the queue's lock.
    drop(superseded);
    let shared = shared.clone();
    spawn(Urgency::Rest, Box::new(move || {
        let job = {
            let mut queued = shared.queued.lock().unwrap();
            queued.1.iter().position(|q| q.id == id).map(|i| queued.1.remove(i))
        };
        if let Some(job) = job {
            (job.checks)(&shared);
        }
    }));
}

/// The sliced printability of `layers`, streamed as `Issues` (a partial
/// report at most every 100 ms, then the whole one), stopping when stale. `(keys, memo, kept)`:
/// each part's content key, the findings kept from earlier revisions, and where each part keeps
/// its trees out.
#[allow(clippy::too_many_arguments)]
fn stream_issues(machine: &Machine, placed: &Plate, materials: &Materials, mut report: Report, layers: &[makepad_fab_toolpath::Layer], timeline: &Timeline, records: &mut dyn Iterator<Item = MoveRecord>, contour_issues: &[fab_printability::Issue], (keys, memo, kept): (&[u64], &fab_printability::SliceMemo, &[Option<fab_printability::KeptOut>]), rev: u64, stale: &(dyn Fn() -> bool + Sync), sink: &(dyn Fn(LiveEvent) + Send + Sync)) {
    let mut last = std::time::Instant::now();
    let total = layers.len() as u32;
    let done = fab_printability::analyse_slice_memo(machine, placed, materials, layers, timeline, records, &mut report, &mut |done, _, r| {
        if stale() {
            return false;
        }
        // A partial report at most every 100 ms.
        if done > 0 && last.elapsed().as_secs_f64() > 0.1 {
            last = std::time::Instant::now();
            let mut part = r.clone();
            part.issues.extend(contour_issues.iter().cloned());
            part.sort();
            sink(LiveEvent::Issues { revision: rev, report: Arc::new(part), done: done as u32, total });
        }
        true
    }, stale, keys, memo, kept);
    if done && !stale() {
        report.issues.extend(contour_issues.iter().cloned());
        report.sort();
        sink(LiveEvent::Issues { revision: rev, report: Arc::new(report), done: total, total });
    }
}

/// What the sink shows of a plan's supports (`plan`, its trees `trees`), less the trees grown
/// from a tip of `dropped`, when it shows them less the trees on layers `before`: the layers to
/// send it, with what is left of them, and those less some trees now. A tree's branch on a layer
/// takes the support records whose middle is on its disc, and each tip's disc goes up the layers
/// above it as long as support stands on it (the roof the tip holds).
fn pruned(plan: &ObjectPlan, trees: &[TreeShape], dropped: &[[f64; 3]], job: &Job, before: &[u32]) -> (Vec<(u32, LivePaths)>, Vec<u32>) {
    use makepad_fab_toolpath::tree::DROP_REACH;
    let deleted = Dropped::new(dropped);
    let gone: Vec<&TreeShape> = trees.iter().filter(|t| t.tips.iter().any(|a| deleted.has(a))).collect();
    // The model layers' z and each one's support records, as the sink was given them.
    let mut zs: Vec<f64> = plan.layers.iter().filter(|l| !l.2.support_only).map(|l| l.0).collect();
    zs.sort_by(f64::total_cmp);
    zs.dedup();
    let support_paths = |index: u32| -> LivePaths {
        let mut paths = LivePaths::default();
        if let Some(&z) = zs.get(index as usize) {
            for (z, _, ol) in plan.layers.iter().filter(|l| !l.2.support_only && l.0 == z) {
                paths.add(index as usize, *z, ol, job, Kept::Supports);
            }
        }
        paths
    };
    let supports = |index: u32| support_paths(index).records();
    let on = crate::live_paths::on_disc;
    let mut discs: std::collections::BTreeMap<u32, Vec<TreeDisc>> = Default::default();
    // The records of the layers the trees are on and of those sent less some trees, made at
    // once; a layer above a tip only its roof reaches is made when the tip's disc gets to it.
    let read: Vec<u32> = gone.iter().flat_map(|t| t.discs.iter().map(|d| d.layer)).chain(before.iter().copied()).collect::<std::collections::BTreeSet<u32>>().into_iter().collect();
    let made = par::map(read.len(), |k| supports(read[k]));
    let mut records: std::collections::BTreeMap<u32, Vec<MoveRecord>> = read.into_iter().zip(made).collect();
    for t in &gone {
        for d in &t.discs {
            discs.entry(d.layer).or_default().push(*d);
        }
        for tip in &t.tips {
            // The tip's own disc: the tree's nearest on its layer.
            let off = |d: &&TreeDisc| (d.at[0] as f64 - tip[0]).powi(2) + (d.at[1] as f64 - tip[1]).powi(2);
            let Some(d) = t.discs.iter().filter(|d| (d.z as f64 - tip[2]).abs() <= DROP_REACH).min_by(|a, b| off(a).total_cmp(&off(b))) else { continue };
            for layer in d.layer + 1..zs.len() as u32 {
                let here = records.entry(layer).or_insert_with(|| supports(layer));
                let up = TreeDisc { layer, ..*d };
                if !here.iter().any(|r| on(r, &up)) {
                    break;
                }
                discs.entry(layer).or_default().push(up);
            }
        }
    }
    // The layers sent less some trees before are sent again, whole where no tree goes now.
    let layers: Vec<u32> = discs.keys().chain(before).copied().collect::<std::collections::BTreeSet<u32>>().into_iter().collect();
    let less = par::map(layers.len(), |k| {
        // Every such layer's records were made: its discs' or a sent one's, or a tip's disc got to it.
        let ds = discs.get(&layers[k]).map_or(&[][..], Vec::as_slice);
        records[&layers[k]].iter().any(|r| ds.iter().any(|d| on(r, d)))
    });
    let (mut send, mut now) = (Vec::new(), Vec::new());
    for (layer, less) in layers.into_iter().zip(less) {
        if less {
            now.push(layer);
        } else if !before.contains(&layer) {
            continue;
        }
        send.push((layer, support_paths(layer).without(discs.get(&layer).map_or(&[][..], Vec::as_slice))));
    }
    (send, now)
}

/// Slot `part`'s plan begins anew (other content): the sink is given its layers and supports
/// again, whatever it was given of the last content's.
fn forget_sent(shared: &Shared, part: usize) {
    let mut c = shared.caches.lock().unwrap();
    c.sent_plan[part] = u64::MAX;
    c.sent_supports[part] = u64::MAX;
}

/// A plan the cache has but the sink was not given for slot `part`: sent whole.
fn send_plan_if_new(shared: &Shared, revision: u64, part: usize, key: u64, settings: u64, job: &Job) {
    // The sink shows the part's layers already when it was given this content under these
    // slice settings: then only another plan's supports go to it again.
    let (plan, layers_too) = {
        let mut c = shared.caches.lock().unwrap();
        let layers_sent = c.sent_plan.get(part) == Some(&key);
        if layers_sent && c.sent_supports.get(part) == Some(&settings) {
            return;
        }
        let Some(p) = cached(&c.plans, (key, settings)) else { return };
        c.sent_plan[part] = key;
        c.sent_supports[part] = settings;
        (p, !layers_sent)
    };
    // Cached regions share one model stream. Support-only layers keep their
    // own indices after the model, just as in the live support event.
    let mut zs: Vec<f64> = plan.layers.iter().filter(|l| !l.2.support_only).map(|l| l.0).collect();
    zs.sort_by(f64::total_cmp);
    zs.dedup();
    let total = zs.len() as u32;
    let mut layers: Vec<(u32, LivePaths)> = (0..total).map(|i| (i, LivePaths::default())).collect();
    let mut supports: std::collections::BTreeMap<u32, LivePaths> = Default::default();
    let mut next = total;
    for (z, _, ol) in &plan.layers {
        let index = if ol.support_only { let i = next; next += 1; i } else { zs.partition_point(|x| x < z) as u32 };
        supports.entry(index).or_default().add(index as usize, *z, ol, job, Kept::Supports);
        if !ol.support_only {
            layers[index as usize].1.add(index as usize, *z, ol, job, Kept::Model);
        }
    }
    // A layer gets supports only where its plan has support paths.
    supports.retain(|_, paths| !paths.is_empty());
    if layers_too {
        (shared.sink)(LiveEvent::PartRegions { part, key, regions: 1, total });
        (shared.sink)(LiveEvent::PartLayers { revision, pass: Pass::Cached, part, key, region: 0, layers, total });
    }
    (shared.sink)(LiveEvent::PartSupports { part, key, layers: supports.into_iter().collect(), done: true });
    let trees = cached(&shared.caches.lock().unwrap().trees, (key, settings)).map_or_else(|| Arc::new([]) as Arc<[TreeShape]>, |t| t.0);
    (shared.sink)(LiveEvent::PartTrees { part, key, trees });
}

/// The exact slice of a plate given as frames, from scratch: what the
/// engine settles to for the same input (the reference its tests compare).
pub fn slice_frames(setup: &JobSetup, plate: &Plate, frames: &[Frame], paint: &[Vec<String>], np_head: Option<&[NpBox]>) -> Result<SliceResult, String> {
    let job = prepare_job(setup)?;
    let (objects, names) = frame_objects(setup, &job, plate, frames, paint, np_head)?;
    check_inputs(&job, &objects)?;
    let classes = makepad_fab_toolpath::instances(&objects);
    let mut plans: Vec<Option<Arc<ObjectPlan>>> = vec![None; objects.len()];
    for (i, o) in objects.iter().enumerate() {
        if classes[i].0 == i {
            plans[i] = Some(Arc::new(plan_one(par::Ctl::default(), i, o, &job).expect("not cancellable")));
        }
    }
    let off = |i: usize| Pt::from_mm(frames[i].offset[0], frames[i].offset[1]);
    let placed = (0..objects.len()).map(|i| {
        let (r, d) = classes[i];
        (plans[r].clone().expect("planned"), copy_plan_offset(off(i), d))
    }).collect();
    let tp = makepad_fab_toolpath::merge_named(placed, &names);
    let boxes: Vec<_> = tp.objects.iter().map(|o| (o.min, o.max)).collect();
    // On the tools that print, as the engine binds them.
    let kept = |kept: &[usize]| {
        let s = setup.keep(kept);
        let j = prepare_job(&s)?;
        Ok((Arc::new(s), Arc::new(j)))
    };
    match bind(&tp, job.filaments(), kept)? {
        Some(b) => finish((*b.job).clone(), b.toolpaths, &names, &boxes, &|_, _| {}, &|| false),
        None => finish(job, Arc::new(tp), &names, &boxes, &|_, _| {}, &|| false),
    }
}

/// What each part's supports will be, as a slice of it plans them, and the paths they print as:
/// its outlines and supports and nothing else, no walls, infill or G-code. One per part, in the
/// plan's frame (placed at the frame's XY offset). A part without supports gets an empty skeleton
/// and costs nothing.
pub fn plan_frames_supports(ctl: par::Ctl, setup: &JobSetup, plate: &Plate, frames: &[Frame], paint: &[Vec<String>], np_head: Option<&[NpBox]>) -> Result<Vec<makepad_fab_toolpath::PlannedSupports>, String> {
    plan_frames_each(setup, plate, frames, paint, np_head, |i, o, job, p| {
        if p.support.is_none() && p.normal_support.is_none() {
            return Ok(makepad_fab_toolpath::PlannedSupports::default());
        }
        let (outline, heights) = part_outline(ctl, o, job, p)?;
        makepad_fab_toolpath::plan_supports(ctl, i, o, job, &outline, &heights, &mut Default::default()).map_err(|_| "cancelled".to_string())
    })
}

/// Each part's supports as [`plan_frames_supports`] plans them, with the outline and layers (top
/// z, height) it plans them from: a part without supports has its outline and layers too.
pub fn plan_frames_layers(ctl: par::Ctl, setup: &JobSetup, plate: &Plate, frames: &[Frame], paint: &[Vec<String>], np_head: Option<&[NpBox]>) -> Result<Vec<PartLayersPlan>, String> {
    plan_frames_each(setup, plate, frames, paint, np_head, |i, o, job, p| {
        let (outline, heights) = part_outline(ctl, o, job, p)?;
        let supports = if p.support.is_none() && p.normal_support.is_none() {
            makepad_fab_toolpath::PlannedSupports::default()
        } else {
            makepad_fab_toolpath::plan_supports(ctl, i, o, job, &outline, &heights, &mut Default::default()).map_err(|_| "cancelled".to_string())?
        };
        Ok((supports, outline, heights))
    })
}

/// Each part's support paths as [`plan_frames_supports`] plans them. The skeleton is untold
/// (no breaks, no hold): for what the paths extrude and take, never for what the supports tell.
pub fn plan_frames_paths(ctl: par::Ctl, setup: &JobSetup, plate: &Plate, frames: &[Frame], paint: &[Vec<String>], np_head: Option<&[NpBox]>) -> Result<Vec<makepad_fab_toolpath::PlannedSupports>, String> {
    plan_frames_each(setup, plate, frames, paint, np_head, |i, o, job, p| {
        if p.support.is_none() && p.normal_support.is_none() {
            return Ok(makepad_fab_toolpath::PlannedSupports::default());
        }
        let (outline, heights) = part_outline(ctl, o, job, p)?;
        let mut supports = makepad_fab_toolpath::plan_support_paths(ctl, i, o, job, &outline, &heights, &mut Default::default()).map_err(|_| "cancelled".to_string())?;
        supports.report = None;
        Ok(supports)
    })
}

/// `each` of the plate's parts as planned from `frames`: its index, input, the plate's job and its
/// own params (the job with its overrides).
fn plan_frames_each<T>(setup: &JobSetup, plate: &Plate, frames: &[Frame], paint: &[Vec<String>], np_head: Option<&[NpBox]>, mut each: impl FnMut(usize, &ObjectInput, &Job, &Params) -> Result<T, String>) -> Result<Vec<T>, String> {
    let job = prepare_job(setup)?;
    let (objects, _) = frame_objects(setup, &job, plate, frames, paint, np_head)?;
    check_inputs(&job, &objects)?;
    objects.iter().enumerate().map(|(i, o)| {
        let own;
        let part_job = if o.overrides.is_empty() { &job } else { own = job.with_overrides(&o.overrides); &own };
        each(i, o, &job, &Params::from_job(part_job, o.filament))
    }).collect()
}

/// A part's outline and the layers (top z, height) it is cut at.
fn part_outline(ctl: par::Ctl, o: &ObjectInput, job: &Job, p: &Params) -> Result<(Vec<Vec<ExPolygon>>, Vec<(f64, f64)>), String> {
    let heights = makepad_fab_toolpath::plan_heights(o, job);
    let zs: Vec<f64> = heights.iter().map(|&(z, h)| makepad_fab_toolpath::cut_z(p, z, h)).collect();
    let slices = makepad_fab_toolpath::contours(ctl, &*o.source, &zs).map_err(|_| "cancelled".to_string())?;
    Ok((makepad_fab_toolpath::plan_outline(ctl, o, job, &heights, slices).map_err(|_| "cancelled".to_string())?, heights))
}

/// A part's supports, and the outline and layers they are planned from.
pub type PartLayersPlan = (makepad_fab_toolpath::PlannedSupports, Vec<Vec<makepad_fab_slicer::geom::ExPolygon>>, Vec<(f64, f64)>);

/// The parts of `input` in their frames: as given, else each its own mesh where it stands.
fn input_frames(input: &LiveInput) -> Vec<Frame> {
    match &input.frames {
        Some(frames) => frames.to_vec(),
        None => input.plate.parts.iter().map(|p| Frame { local: Arc::new(p.mesh.clone()), offset: [0.0; 3], field: None }).collect(),
    }
}

/// What each part's supports will be as a live slice of `input` plans them: the same planner from
/// the same inputs ([`plan_frames_supports`]), for a plate that has no live slice of its own (a
/// try of a change before it is made).
pub fn plan_input_supports(ctl: par::Ctl, input: &LiveInput) -> Result<Vec<makepad_fab_toolpath::PlannedSupports>, String> {
    let setup = (input.setup)(None)?;
    plan_frames_supports(ctl, &setup, &input.plate, &input_frames(input), &input.paint, input.np_head.as_deref().map(Vec::as_slice))
}

/// [`plan_input_supports`] with each part's outline and layers ([`plan_frames_layers`]).
pub fn plan_input_layers(ctl: par::Ctl, input: &LiveInput) -> Result<Vec<PartLayersPlan>, String> {
    let setup = (input.setup)(None)?;
    plan_frames_layers(ctl, &setup, &input.plate, &input_frames(input), &input.paint, input.np_head.as_deref().map(Vec::as_slice))
}

/// Each part's support paths from `input` ([`plan_frames_paths`]). The skeleton is untold
/// (no breaks, no hold): for what the paths extrude and take, never for what the supports tell.
pub fn plan_input_paths(ctl: par::Ctl, input: &LiveInput) -> Result<Vec<makepad_fab_toolpath::PlannedSupports>, String> {
    let setup = (input.setup)(None)?;
    plan_frames_paths(ctl, &setup, &input.plate, &input_frames(input), &input.paint, input.np_head.as_deref().map(Vec::as_slice))
}

/// Each part's outline and layers (top z, height) as [`plan_frames_supports`] plans its supports
/// from them, with its tree supports' settings (none without them).
pub fn plan_frames_outlines(ctl: par::Ctl, setup: &JobSetup, plate: &Plate, frames: &[Frame], paint: &[Vec<String>]) -> Result<Vec<(Vec<Vec<makepad_fab_slicer::geom::ExPolygon>>, Vec<(f64, f64)>, Option<makepad_fab_toolpath::support::SupportParams>)>, String> {
    let job = prepare_job(setup)?;
    let (objects, _) = frame_objects(setup, &job, plate, frames, paint, None)?;
    check_inputs(&job, &objects)?;
    let mut out = Vec::with_capacity(objects.len());
    for o in objects.iter() {
        let own;
        let part_job = if o.overrides.is_empty() { &job } else { own = job.with_overrides(&o.overrides); &own };
        let p = makepad_fab_toolpath::Params::from_job(part_job, o.filament);
        let cancelled = |_| "cancelled".to_string();
        let heights = makepad_fab_toolpath::plan_heights(o, &job);
        let zs: Vec<f64> = heights.iter().map(|&(z, h)| makepad_fab_toolpath::cut_z(&p, z, h)).collect();
        let slices = makepad_fab_toolpath::contours(ctl, &*o.source, &zs).map_err(cancelled)?;
        out.push((makepad_fab_toolpath::plan_outline(ctl, o, &job, &heights, slices).map_err(cancelled)?, heights, p.support.clone()));
    }
    Ok(out)
}

/// A plate given as frames, as the inputs to plan it from.
fn frame_objects(setup: &JobSetup, job: &Job, plate: &Plate, frames: &[Frame], paint: &[Vec<String>], np_head: Option<&[NpBox]>) -> Result<(Vec<ObjectInput>, Vec<String>), String> {
    // Each part's paint read in its frame, which is the plate's moved by the frame's XY offset.
    let regions = crate::plate::PaintRegions::default();
    let mut srcs: Vec<crate::glue::Source> = Vec::with_capacity(plate.parts.len());
    let mut filaments = Vec::with_capacity(plate.parts.len());
    for (i, p) in plate.parts.iter().enumerate() {
        let f = &frames[i];
        let filament = part_filament(setup, i, p)?;
        if let Some(field) = &f.field {
            let lifted = Lifted { src: field.source()?, lift: f.offset[2] };
            let own = (!p.print.is_empty()).then(|| job.with_overrides(&p.print));
            let source: Arc<dyn ContourSource + Send + Sync> = match (np_settings(own.as_ref().unwrap_or(job)), np_head) {
                (Some(s), Some(head)) => Arc::new(WithNp { src: Arc::new(lifted), np: Arc::new(np_plan_of(field, f.offset[2], &s, head)?) }),
                _ => Arc::new(lifted),
            };
            // paint by position, read at the plate point (the source is placed at the frame's XY offset)
            let (ox, oy) = (f.offset[0], f.offset[1]);
            srcs.push(crate::plate::painted(p, source, filament as u32 + 1, move |q| [q[0] + ox, q[1] + oy, q[2]], |q| [q[0] - ox, q[1] - oy, q[2]], 1.0, Some((&regions, &[ox, oy]))));
            filaments.push(filament);
            continue;
        }
        let mesh = frame_mesh(p, f, paint.get(i));
        // paint by position, read at the plate point (the mesh is placed at the frame's XY offset)
        let (ox, oy) = (f.offset[0], f.offset[1]);
        srcs.push(crate::plate::painted(p, Arc::new(MeshSource::new(mesh)), filament as u32 + 1, move |q| [q[0] + ox, q[1] + oy, q[2]], |q| [q[0] - ox, q[1] - oy, q[2]], 1.0, Some((&regions, &[ox, oy]))));
        filaments.push(filament);
    }
    // Parts that print as one body, as the engine glues them.
    let gluing = crate::glue::groups(&plate.groups(), srcs.len(), |g| crate::glue::apart(g, |i| &*srcs[i], |i| plate.parts[i].object.as_str()));
    let at: Vec<Pt> = frames.iter().map(|f| Pt::from_mm(f.offset[0], f.offset[1])).collect();
    crate::glue::apply(&gluing.glued, &mut srcs, &at, &filaments);
    crate::plate::embed_all(job, plate, &mut srcs, &filaments, |i| affine_translate([-frames[i].offset[0], -frames[i].offset[1], 0.0]));
    let objects = plate.parts.iter().zip(srcs).zip(filaments).map(|((p, source), filament)| ObjectInput { overrides: p.print.clone(), name: p.name.clone(), source: Box::new(crate::plate::Shared(source)), filament }).collect();
    Ok((objects, plate.parts.iter().map(|p| p.name.clone()).collect()))
}

#[cfg(test)]
#[path = "live_piece_tests.rs"]
mod piece_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_gcode::record::flags;

    /// An event's layers as the records their paths make.
    fn records_of_layers(layers: &[(u32, LivePaths)]) -> Vec<(u32, Vec<MoveRecord>)> {
        layers.iter().map(|(i, p)| (*i, p.records())).collect()
    }

    /// A band whose pieces prove its slope only cell by cell: a flat ellipsoid deep inside a ball,
    /// which read over the whole band reaches down to its own centre, where its estimate has no
    /// bound, and which a cell near it cuts away under the ball's deeper values. The band is
    /// traced at the slope its cells prove. A far piece replaced by a wider one grows the
    /// shape's box and changes the whole shape's measured slope; the edit cannot reach the cells
    /// round the ellipsoid, so the edited field takes them from the field before unread, the
    /// band keeps its slope bits and its layers, and a fresh field reads the same cells.
    #[test]
    fn a_band_its_cells_prove_keeps_its_slope_when_a_far_replace_grows_the_shape() {
        use makepad_csg_sdf::{SdfNode, SdfOp};
        let to = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let tree = |far: SdfNode| {
            let root = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![SdfNode::Sphere { radius: 10.0 }, SdfNode::Ellipsoid { radii: [1.5, 1.5, 0.3] }, SdfNode::Transform { at: [0.0, 0.0, 40.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(far) }] };
            let (min, max) = makepad_csg_sdf::node_bounds(&root).expect("bounded");
            PartField::new(Arc::new(SdfShape { root, min, max, materials: Vec::new() }), to)
        };
        let wider = || SdfNode::Ellipsoid { radii: [14.0, 14.0, 0.5] };
        let (a, b) = (tree(SdfNode::Sphere { radius: 2.0 }), tree(wider()));
        assert!(b.shape.min[0] < a.shape.min[0] - 3.0 && b.shape.max[1] > a.shape.max[1] + 3.0, "the replace grows the shape's box: {:?} {:?}", a.shape.min, b.shape.min);
        let (s, w) = a.shape.clearance_in(&([-14.0, -14.0, 0.0], [14.0, 14.0, 1.0])).at(1.0);
        assert!(w < s, "the whole band proves no circle of the reach: {s} at {w}");
        let band = a.layer_slope(0.5);
        assert!(band.1 == f64::INFINITY && band != a.slope(), "its cells prove its own slope: {band:?}, the whole shape's {:?}", a.slope());
        // the edit, as the live slicer takes it: the edited field's bands from the last field's
        let kept = b.prove_bands(Some(&a));
        let r = crate::sdf_diff::changed_layers(&a, &b, 1.0).expect("a bounded change");
        assert!(!r.iter().any(|&(lo, hi)| lo <= 9.0 && hi >= -9.0), "the ball's layers are kept: {r:?}");
        assert_eq!(b.layer_slope(0.5), band, "the band keeps its slope bits");
        assert!(a.slope() != b.slope(), "the replace changes the whole shape's slope: {:?}, {:?}", a.slope(), b.slope());
        // the cell of the world's grid holding the ellipsoid: taken unread, and a fresh field
        // proves the same slopes
        let cell = |f: &PartField| {
            let bands = f.bands();
            bands.cells[(0 - bands.first) as usize].at(0, 0).cloned().flatten().expect("the cell holds the ball's values")
        };
        assert!(kept > 0 && Arc::ptr_eq(&cell(&a), &cell(&b)), "the edit leaves the cell: {kept} cells taken");
        let fresh = tree(wider());
        assert!(fresh.layer_slope(0.5) == b.layer_slope(0.5), "a fresh field reads the same slope");
        assert!(cell(&b).quads.iter().any(|q| q.get().is_some()), "the cell holds the slopes the first band proved");
        for z in -12..12 {
            let z = z as f64 + 0.5;
            assert!(fresh.plane_slope(z).same(&b.plane_slope(z)), "the edited field's bands are a fresh field's at {z}: {:?}, {:?}", fresh.plane_slope(z), b.plane_slope(z));
        }
    }

    /// A cell's proof goes with the fields that read it. Through two edits in a row, and a replace
    /// that changes the tree's form, which reaches every cell or, with a part no box holds, tells
    /// nothing of where it reaches, a field keeps the proof of a cell whose cut tree stays the
    /// same; the proofs of the cells an edit changes go with the fields that hold them, with the
    /// pieces of the shape they cut.
    #[test]
    fn a_proof_goes_with_the_fields_that_hold_it() {
        use makepad_csg_sdf::{SdfKernel, SdfNode, SdfOp};
        let to = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let at = |x: f64, radius: f64| SdfNode::Transform { at: [x, 0.0, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Sphere { radius }) };
        let tree = |of: Vec<SdfNode>| {
            let root = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of };
            let (min, max) = makepad_csg_sdf::node_bounds(&root).expect("bounded");
            PartField::new(Arc::new(SdfShape { root, min, max, materials: Vec::new() }), to)
        };
        let cell = |f: &PartField, x: i64| {
            let bands = f.bands();
            bands.cells[(0 - bands.first) as usize].at(x, 0).cloned().flatten().expect("the cell holds the shape's values")
        };
        let a = tree(vec![at(0.0, 5.0), at(40.0, 2.0)]);
        let (near, far_a) = (cell(&a, 0), Arc::downgrade(&cell(&a, 2)));

        // a far edit
        let b = tree(vec![at(0.0, 5.0), at(40.0, 3.0)]);
        assert!(b.prove_bands(Some(&a)) > 0, "the edit leaves cells");
        assert!(Arc::ptr_eq(&near, &cell(&b, 0)), "an edit leaves the far cell's proof");
        let far_b = Arc::downgrade(&cell(&b, 2));
        assert!(far_a.as_ptr() != far_b.as_ptr(), "the edit makes the cell it changes a proof of its own");
        drop(a);
        assert_eq!(far_a.strong_count(), 0, "the proof of what the edit changed goes with its field");

        // a replace: one more part changes the tree's form, so the change reaches every cell
        let c = tree(vec![at(0.0, 5.0), at(40.0, 3.0), at(-40.0, 2.0)]);
        assert_eq!(c.prove_bands(Some(&b)), 0, "no cell is left unread");
        assert!(Arc::ptr_eq(&near, &cell(&c, 0)), "a cell that cuts the same tree keeps its proof");
        assert!(far_b.as_ptr() == Arc::as_ptr(&cell(&c, 2)), "so does the far one");

        // and an edit right after it
        let d = tree(vec![at(0.0, 5.0), at(40.0, 4.0), at(-40.0, 2.0)]);
        d.prove_bands(Some(&c));
        assert!(Arc::ptr_eq(&near, &cell(&d, 0)), "two edits on, the same proof");
        assert!(far_b.as_ptr() != Arc::as_ptr(&cell(&d, 2)), "the edit changes the far cell");
        drop((b, c));
        assert_eq!(far_b.strong_count(), 0, "two edits on, the proof goes with the fields that held it");

        let last = Arc::downgrade(&near);
        drop((d, near));
        assert_eq!(last.strong_count(), 0, "with no field left, no proof stays");

        // with a part no box holds, a replace tells nothing of where it reaches
        let with_kernel = |extra: Option<SdfNode>| {
            let kernel = SdfNode::Kernel(SdfKernel { key: 0x5907, field: Arc::new(Spot { at: [0.0, 0.0, 3.0] }), splash: None, bounds: None, solid: None });
            let mut of = vec![at(0.0, 5.0), kernel];
            of.extend(extra);
            let root = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of };
            PartField::new(Arc::new(SdfShape { root, min: [-20.0, -6.0, -2.0], max: [20.0, 6.0, 2.0], materials: Vec::new() }), to)
        };
        let (e, f) = (with_kernel(None), with_kernel(Some(at(-16.0, 1.0))));
        assert!(makepad_csg_sdf::changed_within(&e.shape.root, e.window(), &f.shape.root, f.window()).is_none(), "the replace tells nothing of where it reaches");
        let kept = cell(&e, 0);
        assert_eq!(f.prove_bands(Some(&e)), 0, "no cell is left unread");
        assert!(Arc::ptr_eq(&kept, &cell(&f, 0)), "a cell that cuts the same tree keeps its proof all the same");
    }

    /// A field ten times as steep as a distance within half a millimetre of its small ball, and
    /// a distance elsewhere: a kernel no tree proves the slope of.
    struct Spot {
        at: [f64; 3],
    }

    impl makepad_csg_sdf::Sdf3 for Spot {
        fn distance(&self, p: Vec3d) -> f64 {
            let d = ((p.x - self.at[0]).powi(2) + (p.y - self.at[1]).powi(2) + (p.z - self.at[2]).powi(2)).sqrt() - 0.3;
            (10.0 * d).min(d + 1.8)
        }
    }

    /// The whole shape's measure, on a lattice over its box, steps over a small steep spot no
    /// tree proves, and a plane traced at that slope steps over the spot's crossing: its coarse
    /// cell's corners clear it. The spot's leaf of the world's grid proves no slope, so no circle
    /// that meets it is cleared, and the plane traced square by square finds the spot.
    #[test]
    fn a_steep_spot_the_whole_measure_steps_over_is_traced_where_no_leaf_proves_it() {
        use makepad_csg_sdf::{SdfKernel, SdfNode};
        let to = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        // at a coarse cell's centre, between the lattice points the whole shape is measured at
        let at = [0.5, 0.5, 0.0];
        let root = SdfNode::Kernel(SdfKernel { key: 0x5907, field: Arc::new(Spot { at }), splash: None, bounds: None, solid: None });
        let f = PartField::new(Arc::new(SdfShape { root, min: [-20.0, -20.0, -1.0], max: [20.0, 20.0, 1.0], materials: Vec::new() }), to);
        let near = |rs: &[ExPolygon]| rs.iter().flat_map(|r| r.rings()).flat_map(|r| r.pts.iter()).any(|q| (q.x_mm() - at[0]).hypot(q.y_mm() - at[1]) < 0.4);
        let whole = f.slope();
        assert!(whole.0 < 2.0 && whole.1 == f64::INFINITY, "the whole shape's measure steps over the spot: {whole:?}");
        let mut old = SdfSource::new(f.field().expect("a field"), (f.shape.min, f.shape.max));
        (old.params.lipschitz, old.params.window) = whole;
        assert!(!near(&old.contours(&[0.0])[0]), "the whole shape's slope steps over the spot");
        let plane = f.plane_slope(0.0);
        let map = plane.map.as_ref().expect("the band's map");
        assert!(map.most(at[0], at[1], 0.0).0 == f64::INFINITY, "the spot's leaf proves no slope: {plane:?}");
        assert!(near(&f.source().expect("a source").contours(&[0.0])[0]), "the plane traced square by square finds the spot");
    }
    use fab_plate::PartMesh;
    use makepad_fab_settings::profiles::Preset;
    use std::sync::mpsc;

    pub(super) fn cube(min: [f64; 3], size: [f64; 3]) -> PartMesh {
        let positions = (0..8u32).map(|i| [min[0] + size[0] * (i & 1) as f64, min[1] + size[1] * ((i >> 1) & 1) as f64, min[2] + size[2] * ((i >> 2) & 1) as f64]).collect();
        let triangles = vec![[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
        PartMesh { positions, triangles }
    }

    pub(super) fn setup() -> Arc<JobSetup> {
        let mut machine = Preset::default();
        machine.values.set_vec("nozzle_diameter", vec!["0.4".into(), "0.4".into()]);
        machine.values.set_vec("printable_area", fab_plate::machine::Machine::dual_nozzle_350().bed.corners());
        Arc::new(JobSetup {
            machine,
            process: Preset::default(),
            filaments: vec![Preset::default(), Preset::default()],
            filament_map: vec![1, 2],
            nozzle_volume_type: vec!["Standard".into(), "Standard".into()],
            bed_type: "Textured PEI Plate".into(),
            filament_map_mode: "Manual".into(),
            filament_colours: vec![None; 2],
        })
    }

    pub(super) fn plate() -> Plate {
        let part = |name: &str, filament: u32, mesh| Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: name.into(), name: name.into(), filament, color: [1.0; 4], nozzle: None, mesh, field_bounds: None, field_floor: None, ..Default::default() };
        // A cap on a stem (bridges, overhangs) beside a box.
        let mut stem = cube([96.0, 96.0, 0.0], [8.0, 8.0, 8.0]);
        let cap = cube([88.0, 88.0, 8.0], [24.0, 24.0, 3.0]);
        let b = stem.positions.len() as u32;
        stem.positions.extend(cap.positions);
        stem.triangles.extend(cap.triangles.iter().map(|t| [t[0] + b, t[1] + b, t[2] + b]));
        Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("cap", 1, stem), part("box", 2, cube([200.0, 100.0, 0.0], [15.0, 12.0, 6.0]))] }
    }

    fn input(plate: &Plate) -> LiveInput {
        LiveInput::ready(1, 1, (*setup()).clone(), Arc::new(plate.clone()), Arc::new(Vec::new()))
    }

    #[test]
    fn overlap_binding_follows_the_unedited_whole_plate() {
        let input = input(&plate());
        let id = input.plate_id;
        let overlaps = Arc::new(fab_printability::overlaps(&input.plate, &|_| None, &|| false).unwrap());
        let edited = input.with_overlaps(Some(overlaps.clone()), id + 1);
        assert!(edited.known_overlaps().is_none(), "an input with a different plate id checks its own overlaps");
        let mut whole = edited.with_overlaps(Some(overlaps), id);
        assert!(whole.known_overlaps().is_some(), "the evaluated plate reuses its bound overlaps");
        whole.pieces = Some(Arc::new(crate::piece::Pieces { whole_plate: id, parts: Vec::new() }));
        assert!(whole.known_overlaps().is_none(), "a pieces input checks its own overlaps");
        whole.pieces = None;
        whole.place(0, affine_translate([1.0, 0.0, 0.0]));
        assert!(whole.known_overlaps().is_none(), "moving a part invalidates the plate allocation binding");
    }

    fn checkpoints() -> Next {
        let revision = Arc::new(AtomicU64::new(0));
        Arc::new(move || {
            let rev = revision.fetch_add(1, Ordering::AcqRel) + 1;
            let revision = revision.clone();
            Arc::new(move || revision.load(Ordering::Acquire) != rev)
        })
    }

    /// An engine on std threads with no trace host: every part is cut on the cores.
    pub(super) fn engine() -> (LiveEngine, mpsc::Receiver<LiveEvent>) {
        engine_seeing(|_| {})
    }

    /// [`engine`], whose sink shows each event to `see` first, on the thread that sent it.
    pub(super) fn engine_seeing(see: impl Fn(&LiveEvent) + Send + Sync + 'static) -> (LiveEngine, mpsc::Receiver<LiveEvent>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let spawn: Spawn = Arc::new(|_, job| {
            std::thread::spawn(job);
        });
        let sink: Sink = Arc::new(move |e| {
            see(&e);
            let _ = tx.lock().unwrap().send(e);
        });
        let engine = LiveEngine::new(spawn.clone(), sink, checkpoints());
        engine.set_resume(Arc::new(move |checkpoint| {
            let spawn = spawn.clone();
            Arc::new(move |urgency, job| {
                let checkpoint = checkpoint.clone();
                spawn(urgency, Box::new(move || { if urgency == Urgency::Viewed || !checkpoint() { job(); } }));
            })
        }));
        (engine, rx)
    }

    pub(super) fn settled(rx: &mpsc::Receiver<LiveEvent>, want: u64) -> Arc<Settled> {
        loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::Settled { tag, result, .. } if tag == want => return result.expect("sliced"),
                _ => {}
            }
        }
    }

    /// The engine's input calls answer while its sink is busy: thread A, whose new plate
    /// supersedes an open settle, is held inside the sink by that settle's answer, and thread B
    /// asks for the input's revision and moves a part meanwhile. A was in the sink holding the
    /// engine's state, so B waited for whatever the sink waited for (the app's locks).
    #[test]
    fn input_calls_answer_while_the_sink_holds_a_superseded_settle() {
        // Only a failure waits this long: on the pass path nothing waits on the clock.
        const STUCK: std::time::Duration = std::time::Duration::from_secs(5);
        let (arrived, at_sink) = mpsc::channel();
        let (go, wait) = mpsc::channel::<()>();
        let wait = Mutex::new(wait);
        // No driver runs: the engine only takes inputs.
        let e = LiveEngine::new(Arc::new(|_, _| {}), Arc::new(move |event| {
            if let LiveEvent::Settled { result: Err(_), .. } = event {
                let _ = arrived.send(());
                let _ = wait.lock().unwrap().recv();
            }
        }), checkpoints());
        let i = input(&plate());
        e.settle(1, i.clone());
        let (plate_id, settings_id) = (i.plate_id + 1, i.settings_id);
        let a = {
            let e = LiveEngine { shared: e.shared.clone() };
            std::thread::spawn(move || e.set_input_with(plate_id, settings_id, 2, Arc::new(move || i.clone())))
        };
        at_sink.recv_timeout(STUCK).expect("the open settle is answered as superseded");
        let (answered, answer) = mpsc::channel();
        let b = LiveEngine { shared: e.shared.clone() };
        std::thread::spawn(move || {
            let revision = b.input_revision(plate_id, settings_id);
            b.set_transform(0, translate(1.0, 0.0));
            let _ = answered.send(revision);
        });
        let seen = answer.recv_timeout(STUCK).expect("an input call waits for the sink: the engine called it holding its state");
        drop(go);
        let revision = a.join().expect("the new plate is taken");
        assert_eq!(seen, Some(revision), "the input asked about is the new plate, taken before its sink call returned");
    }

    /// The engine's driver has exited, however many pool jobs its revisions took.
    fn idle(e: &LiveEngine) {
        e.wait_idle();
    }

    /// Runs `ui` (the UI's calls) on a thread of its own while another holds the engine's caches,
    /// as a worker does across all it does under them (a first call the JIT compiles included),
    /// and fails unless `ui` returns before they are let go.
    fn while_a_worker_holds_the_caches<T: Send + 'static>(e: &LiveEngine, ui: impl FnOnce(LiveEngine) -> T + Send + 'static) -> T {
        // Only a failure waits this long: on the pass path nothing waits on the clock.
        const STUCK: std::time::Duration = std::time::Duration::from_secs(5);
        let (held, is_held) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let worker = e.clone();
        let holder = std::thread::spawn(move || {
            let _caches = worker.shared.caches.lock().unwrap();
            held.send(()).unwrap();
            let _ = released.recv();
        });
        is_held.recv().unwrap();
        let (done, returned) = mpsc::channel();
        let on_ui = e.clone();
        std::thread::spawn(move || {
            let _ = done.send(ui(on_ui));
        });
        let out = returned.recv_timeout(STUCK);
        drop(release);
        holder.join().unwrap();
        out.expect("a UI call waits for the caches a worker holds")
    }

    fn translate(dx: f64, dy: f64) -> Affine {
        [[1.0, 0.0, 0.0, dx], [0.0, 1.0, 0.0, dy], [0.0, 0.0, 1.0, 0.0]]
    }

    /// The edge of a blade first breaks a layer as a sliver far narrower than a bead, and
    /// the blade widens too slowly for any overhang above it: a support has to reach that
    /// sliver all the same, or the print is refused, and the refusal must not come early,
    /// before the supports exist.
    #[test]
    fn a_floating_sliver_gets_a_support_that_reaches_it() {
        // Mid-air issues in the first report of the checks and in their last.
        let mid_air = |support: Option<&str>| -> (usize, usize) {
            let mut mesh = cube([110.0, 100.0, 0.0], [6.0, 6.0, 8.0]);
            let b = mesh.positions.len() as u32;
            // Its edge along y at z = 3.1 mm, 2.6 mm wide 4 mm higher up: 0.13 mm a layer.
            mesh.positions.extend([[100.0, 100.0, 3.1], [100.0, 103.0, 3.1], [98.7, 100.0, 7.1], [98.7, 103.0, 7.1], [101.3, 100.0, 7.1], [101.3, 103.0, 7.1]]);
            mesh.triangles.extend([[0, 4, 2], [1, 3, 5], [2, 4, 5], [2, 5, 3], [0, 2, 3], [0, 3, 1], [0, 1, 5], [0, 5, 4]].map(|t: [u32; 3]| t.map(|v| v + b)));
            let mut plate = plate();
            plate.parts.truncate(1);
            plate.parts[0].mesh = mesh;
            let mut job = (*setup()).clone();
            if let Some(kind) = support {
                job.process.values.set_str("enable_support", "1");
                job.process.values.set_str("support_type", kind);
                job.process.values.set_str("support_remove_small_overhang", "1");
            }
            let (e, rx) = engine();
            e.settle(1, LiveInput::ready(1, 1, job, Arc::new(plate), Arc::new(Vec::new())));
            let mut first = None;
            loop {
                if let LiveEvent::Issues { report, done, total, .. } = rx.recv().expect("the checks finish") {
                    let count = report.issues.iter().filter(|i| i.kind == "mid_air").count();
                    let first = *first.get_or_insert(count);
                    if done == total {
                        return (first, count);
                    }
                }
            }
        };
        assert_eq!(mid_air(None), (1, 1), "without supports the sliver floats");
        assert_eq!(mid_air(Some("tree(auto)")), (0, 0), "under a tree support");
        assert_eq!(mid_air(Some("normal(auto)")), (0, 0), "under a normal support");
    }

    /// The checks' report on a tree-supported part made of `boxes` (min, size; mm), with `settings`
    /// on top of the process's.
    fn boxes_report(boxes: &[([f64; 3], [f64; 3])], settings: &[(&str, &str)]) -> Arc<fab_printability::Report> {
        let mut mesh = PartMesh { positions: Vec::new(), triangles: Vec::new() };
        for &(min, size) in boxes {
            let c = cube(min, size);
            let b = mesh.positions.len() as u32;
            mesh.positions.extend(c.positions);
            mesh.triangles.extend(c.triangles.iter().map(|t| t.map(|v| v + b)));
        }
        let mut plate = plate();
        plate.parts.truncate(1);
        plate.parts[0].mesh = mesh;
        let mut job = (*setup()).clone();
        job.process.values.set_str("enable_support", "1");
        job.process.values.set_str("support_type", "tree(auto)");
        for &(k, v) in settings {
            job.process.values.set_str(k, v);
        }
        let (e, rx) = engine();
        e.settle(1, LiveInput::ready(1, 1, job, Arc::new(plate), Arc::new(Vec::new())));
        loop {
            if let LiveEvent::Issues { report, done, total, .. } = rx.recv().expect("the checks finish") {
                if done == total {
                    return report;
                }
            }
        }
    }

    /// A revision tells how far it is on one bar, never going back: its supports are a stage of
    /// it, after the layers, named part by part with what each part's planner is at. Each
    /// part's supports go out as soon as it is planned, before the next part plans.
    #[test]
    fn supports_tell_their_progress_and_stream_part_by_part() {
        let plate = two_caps();
        let mut job = (*setup()).clone();
        job.process.values.set_str("enable_support", "1");
        job.process.values.set_str("support_type", "tree(auto)");
        let (e, rx) = engine();
        e.settle(1, LiveInput::ready(1, 1, job, Arc::new(plate), Arc::new(Vec::new())));
        let (mut told, mut sent) = (Vec::new(), Vec::new());
        loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::Running { progress, stage, .. } => {
                    if let Some(words) = stage.strip_prefix("Supports: ") {
                        let second = words.starts_with("small cap (2 of 2)");
                        assert!(second || words.starts_with("cap (1 of 2)"), "the part by name and place: {words}");
                        assert_eq!(second, sent.len() == 1, "a part plans once the parts before it went out: {words}, sent {sent:?}");
                    }
                    told.push((progress, stage));
                }
                LiveEvent::PartSupports { part, done: true, .. } => sent.push(part),
                LiveEvent::Settled { tag: 1, result, .. } => {
                    result.expect("sliced");
                    break;
                }
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        }
        idle(&e);
        assert_eq!(sent, [0, 1], "each part's supports, in turn");
        assert!(told.windows(2).all(|w| w[0].0 <= w[1].0), "never back: {told:?}");
        assert!(told.iter().all(|t| (0.0..=1.0).contains(&t.0)), "{told:?}");
        let stage = |t: &(f32, String)| STAGE_NAMES.iter().position(|n| t.1.starts_with(n)).expect("a stage's words");
        assert!(told.windows(2).all(|w| stage(&w[0]) <= stage(&w[1])), "the stages in order: {told:?}");
        // The supports are most of this slice: their stage moves the bar a long way, not at its end.
        let supports: Vec<f32> = told.iter().filter(|t| stage(t) == 1).map(|t| t.0).collect();
        let (first, last) = (supports.first().copied().unwrap_or(1.0), supports.last().copied().unwrap_or(0.0));
        assert!(first < 0.5 && last - first > 0.3, "the supports' share of the bar: {supports:?}");
    }

    /// A cup 20 mm square and 14 mm tall, its floor 1 mm and its walls 1.5 mm, with a shelf on one
    /// wall over the hollow, its underside at z 8.
    const CUP: [([f64; 3], [f64; 3]); 6] = [([100.0, 100.0, 0.0], [20.0, 20.0, 1.0]), ([100.0, 100.0, 0.0], [1.5, 20.0, 14.0]), ([118.5, 100.0, 0.0], [1.5, 20.0, 14.0]), ([100.0, 100.0, 0.0], [20.0, 1.5, 14.0]), ([100.0, 118.5, 0.0], [20.0, 1.5, 14.0]), ([101.5, 101.5, 8.0], [8.0, 17.0, 2.0])];

    /// What no support gets to inside a part is a warning saying how much and where, which lets
    /// Print go on: from the bed only, the cup's walls are in the way of what its shelf needs;
    /// allowed on the part, a support stands on the cup's floor and holds the shelf, and comes out
    /// through the open top.
    #[test]
    fn overhang_closed_in_from_the_bed_warns_and_is_held_on_the_part() {
        let from_bed = boxes_report(&CUP, &[("support_on_build_plate_only", "1")]);
        let warned = from_bed.issues.iter().find(|i| i.kind == "support_inside").unwrap_or_else(|| panic!("the shelf's underside is reported: {:?}", from_bed.issues.iter().map(|i| (i.kind, i.message.clone())).collect::<Vec<_>>()));
        assert_eq!(warned.severity, fab_printability::Severity::Warning);
        assert!(warned.area_mm2 > 100.0, "the shelf is 8 by 17 mm: {} mm2", warned.area_mm2);
        assert!((warned.highest_z - 8.0).abs() < 1.0 && (warned.at_mm[0] - 105.5).abs() < 5.0 && (warned.at_mm[1] - 110.0).abs() < 9.0, "under the shelf: {:?}", (warned.at_mm, warned.highest_z));
        assert_eq!(from_bed.counts()[0], 0, "Print goes on: {:?}", from_bed.issues.iter().map(|i| (i.kind, i.message.clone())).collect::<Vec<_>>());
        let on_part = boxes_report(&CUP, &[]);
        let left: Vec<(&str, String)> = on_part.issues.iter().filter(|i| i.kind.starts_with("support_") && i.highest_z > 6.0 && i.lowest_z < 10.0).map(|i| (i.kind, i.message.clone())).collect();
        assert!(left.is_empty(), "on the part the shelf is held, with nothing to tell: {left:?}");
    }

    /// The cup shut by a lid with a 1 mm hole in it: a cell whose window is narrower than the
    /// thinnest column. A support under its shelf and lid could not come out, so none stands there
    /// and that is a warning, with the window; asked to, the part's supports stand there and hold
    /// them, and the warning says they can't be removed.
    #[test]
    fn a_cell_with_a_narrow_window_keeps_its_supports_out_unless_asked() {
        let mut cell = CUP.to_vec();
        cell.extend([([101.5, 101.5, 12.0], [17.0, 8.0, 2.0]), ([101.5, 110.5, 12.0], [17.0, 8.0, 2.0]), ([101.5, 109.5, 12.0], [11.5, 1.0, 2.0]), ([114.0, 109.5, 12.0], [4.5, 1.0, 2.0])]);
        let kept_out = boxes_report(&cell, &[]);
        let told = |r: &fab_printability::Report| r.issues.iter().map(|i| (i.kind, i.message.clone())).collect::<Vec<_>>();
        let locked = kept_out.issues.iter().find(|i| i.kind == "support_locked").unwrap_or_else(|| panic!("what a support would be locked in under is told: {:?}", told(&kept_out)));
        assert_eq!(locked.severity, fab_printability::Severity::Warning);
        assert!(locked.message.contains("could not come out, window ") && locked.area_mm2 > 100.0, "{} ({} mm2)", locked.message, locked.area_mm2);
        assert!(!kept_out.issues.iter().any(|i| i.kind == "support_unremovable"), "{:?}", told(&kept_out));
        assert_eq!(kept_out.counts()[0], 0, "Print goes on: {:?}", told(&kept_out));
        let asked = boxes_report(&cell, &[("fab_support_inside", "1")]);
        assert!(!asked.issues.iter().any(|i| matches!(i.kind, "support_locked" | "support_inside")), "asked to, supports stand there: {:?}", told(&asked));
        let stuck = asked.issues.iter().find(|i| i.kind == "support_unremovable").unwrap_or_else(|| panic!("and say they can't be removed: {:?}", told(&asked)));
        assert!(stuck.message.contains("not removable through a ") && stuck.area_mm2 > 10.0, "{} ({} mm2)", stuck.message, stuck.area_mm2);
        assert_eq!(asked.counts()[0], 0, "Print goes on: {:?}", told(&asked));
    }

    #[test]
    fn a_spiral_layer_rises_by_its_path_z() {
        use makepad_fab_toolpath::{exp::mode, ExtrusionPath, Island, Role};
        let mut cfg = makepad_fab_settings::config::Config::new();
        cfg.set_str("filament_diameter", "1.75");
        let job = Job::new(cfg);
        // One revolution of a 10 mm square rising a 0.2 mm layer, and a flat infill line.
        let pts: Vec<Pt> = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]].iter().map(|p| Pt::from_mm(p[0], p[1])).collect();
        let spiral = ExtrusionPath { role: Role::OuterWall, pts, width: 0.45, height: 0.2, area: 0.09, speed: 50.0, z: vec![0.0, 0.05, 0.1, 0.15, 0.2], mode: mode::VASE, ..Default::default() };
        let flat = ExtrusionPath { role: Role::SparseInfill, pts: vec![Pt::from_mm(1.0, 1.0), Pt::from_mm(9.0, 1.0)], width: 0.45, height: 0.2, area: 0.09, speed: 50.0, ..Default::default() };
        let ol = ObjectLayer { islands: vec![Arc::new(Island { walls: vec![spiral], fills: vec![flat] })], ..Default::default() };
        let records = layer_records(3, 0.6, &ol, &job);
        assert_eq!(records.len(), 5);
        for (k, r) in records[..4].iter().enumerate() {
            assert!((r.p0[2] - (0.6 + 0.05 * k as f32)).abs() < 1e-5 && (r.p1[2] - (0.65 + 0.05 * k as f32)).abs() < 1e-5, "segment {k}: {:?} -> {:?}", r.p0, r.p1);
            assert_eq!((r.z_layer, r.mode, r.flags & flags::SLOPED), (0.6, mode::VASE, flags::SLOPED));
            // Timed and fed over the 3D length.
            assert!(((r.t_end - r.t_start) * 50.0 - 10.0f64.hypot(0.05)).abs() < 1e-6);
        }
        let line = &records[4];
        assert_eq!((line.p0[2], line.p1[2], line.mode, line.flags & flags::SLOPED), (0.6, 0.6, 0, 0));
    }

    #[test]
    fn panicking_part_job_fails_clears_progress_and_retries() {
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(Mutex::new(None));
        let shown = progress.clone();
        let e = LiveEngine::new(Arc::new(|_, job| { std::thread::spawn(job); }), Arc::new(move |event| {
            match &event {
                LiveEvent::Progress { progress, .. } => *shown.lock().unwrap() = Some(*progress),
                LiveEvent::Settled { .. } => *shown.lock().unwrap() = None,
                _ => {}
            }
            let _ = tx.send(event);
        }), checkpoints());
        let plate = plate();
        *e.shared.panic_part.lock().unwrap() = Some(1);
        e.settle(1, input(&plate));
        let (mut failed, mut progressed, mut first_planned) = (false, false, false);
        loop {
            match rx.recv().expect("panic terminates the slice") {
                LiveEvent::Progress { .. } => progressed = true,
                LiveEvent::PartLayers { part: 0, .. } => first_planned = true,
                LiveEvent::Failed { revision, error } => {
                    assert_eq!(revision, 1);
                    assert_eq!(error, "injected part job panic");
                    assert!(!failed, "one failure event");
                    failed = true;
                }
                LiveEvent::Settled { tag, result, .. } => {
                    assert_eq!(tag, 1);
                    assert_eq!(result.err().as_deref(), Some("injected part job panic"));
                    break;
                }
                _ => {}
            }
        }
        idle(&e);
        assert!(failed && progressed && first_planned, "failure after progress and a streamed model layer");
        assert!(progress.lock().unwrap().is_none());
        {
            let state = e.shared.state.lock().unwrap();
            assert!(!state.running);
            assert!(state.settle.is_none());
            assert_eq!(state.served, state.revision);
            let caches = e.shared.caches.lock().unwrap();
            assert!(caches.meshes.is_empty() && caches.cuts.is_empty() && caches.plans.is_empty());
            assert!(caches.slot_fields.is_empty());
            assert!(caches.sent_cut.is_empty() && caches.sent_plan.is_empty());
            assert!(caches.toolpaths.is_none());
        }
        let planned = e.parts_planned();
        let traced = e.traced_count();
        // Same content and settings: a manual retry must not reuse partial
        // data or skip streams because of the failed job's sent markers.
        e.settle(2, input(&plate));
        let retry = settled(&rx, 2);
        idle(&e);
        assert!(progress.lock().unwrap().is_none());
        assert_eq!(e.parts_planned() - planned, 2);
        assert!(e.traced_count() > traced);
        assert_eq!(retry.gcode, crate::plate::slice_plate(&setup(), &plate).unwrap().gcode);
        assert!(!rx.try_iter().any(|event| matches!(event, LiveEvent::Failed { .. } | LiveEvent::Progress { .. })));
    }

    /// The print time asked for the moment the whole toolpaths are seen (the app asks then),
    /// while the engine still runs the checks that follow them, is answered: it was lost when
    /// the ask came before the revision was served.
    #[test]
    fn an_estimate_asked_as_the_toolpaths_arrive_is_answered() {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let shared: Arc<std::sync::OnceLock<std::sync::Weak<Shared>>> = Arc::new(std::sync::OnceLock::new());
        let me = shared.clone();
        let e = LiveEngine::new(Arc::new(|_, job| { std::thread::spawn(job); }), Arc::new(move |event| {
            if matches!(event, LiveEvent::Toolpaths { .. }) {
                if let Some(shared) = me.get().and_then(std::sync::Weak::upgrade) {
                    LiveEngine { shared }.estimate();
                }
            }
            let _ = tx.lock().unwrap().send(event);
        }), checkpoints());
        shared.set(Arc::downgrade(&e.shared)).ok();
        e.set_input(input(&plate()));
        loop {
            if let LiveEvent::Estimated { estimate, .. } = rx.recv().expect("the estimate asked as the toolpaths arrived") {
                assert!(estimate.stats.total_time > 0.0, "a print time");
                break;
            }
        }
    }

    #[test]
    fn a_slice_over_its_memory_fails_with_the_reason_and_the_next_one_runs() {
        let (e, rx) = engine();
        let over = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let says = over.clone();
        e.set_memory_guard(Arc::new(move || says.load(Ordering::Relaxed).then(|| "over the limit".to_string())));
        // The guard is asked every GUARD_MS, counted from the engine's start.
        std::thread::sleep(std::time::Duration::from_millis(GUARD_MS * 2));
        e.settle(1, input(&plate()));
        loop {
            match rx.recv().expect("an answer") {
                LiveEvent::Settled { tag: 1, result, .. } => {
                    assert_eq!(result.err().as_deref(), Some("over the limit"));
                    break;
                }
                _ => {}
            }
        }
        over.store(false, Ordering::Relaxed);
        e.settle(2, input(&plate()));
        settled(&rx, 2);
    }

    /// An idle or running slice fails with the caller's reason, answers an open settle, and
    /// the next slice sends every part again.
    #[test]
    fn a_failed_slice_fails_idle_or_running_and_the_next_sends_every_part() {
        let (e, rx) = engine();
        e.set_enabled(false);
        e.settle(1, input(&plate()));
        e.fail("lost".into());
        let mut failed = false;
        loop {
            match rx.recv().expect("an idle failure answers the open settle") {
                LiveEvent::Failed { error, .. } => {
                    assert_eq!(error, "lost", "an idle failure publishes the caller's reason");
                    failed = true;
                }
                LiveEvent::Settled { tag: 1, result, .. } => {
                    assert!(failed, "an idle failure publishes Failed before answering the open settle");
                    assert_eq!(result.err().as_deref(), Some("lost"), "an idle failure answers the open settle with the caller's reason");
                    break;
                }
                _ => {}
            }
        }
        e.set_enabled(true);

        e.set_input(input(&plate()));
        idle(&e);
        let parts: HashSet<_> = rx.try_iter().filter_map(|event| match event { LiveEvent::PartCut { part, .. } => Some(part), _ => None }).collect();
        let n = parts.len();
        assert!(n > 0, "a whole slice sends part cuts before its state is lost");
        e.fail("lost".into());
        assert!(matches!(rx.iter().find(|event| matches!(event, LiveEvent::Failed { .. })), Some(LiveEvent::Failed { error, .. }) if error == "lost"), "a failure after a whole slice publishes the caller's reason");
        e.set_input(input(&plate()));
        idle(&e);
        let events: Vec<_> = rx.try_iter().collect();
        let resent: HashSet<_> = events.iter().filter_map(|event| match event { LiveEvent::PartCut { part, .. } => Some(*part), _ => None }).collect();
        assert_eq!(resent, parts, "the same plate sends all {n} parts again after an idle failure");
        assert!(events.iter().any(|event| matches!(event, LiveEvent::Toolpaths { .. })), "the same plate sends toolpaths again after an idle failure");

        let other = LiveEngine { shared: e.shared.clone() };
        *e.shared.touched.lock().unwrap() = Some(Box::new(move || other.fail("lost".into())));
        e.settle(2, input(&plate()));
        let mut failed = false;
        loop {
            match rx.recv().expect("a running failure answers the open settle") {
                LiveEvent::Failed { error, .. } => {
                    assert_eq!(error, "lost", "a running failure publishes the caller's reason");
                    failed = true;
                }
                LiveEvent::Settled { tag: 2, result, .. } => {
                    assert!(failed, "a running failure publishes Failed before answering the open settle");
                    assert_eq!(result.err().as_deref(), Some("lost"), "a running failure answers the open settle with the caller's reason");
                    break;
                }
                _ => {}
            }
        }
        e.settle(3, input(&plate()));
        settled(&rx, 3);
        idle(&e);
        assert!(!e.shared.failing.load(Ordering::Relaxed), "a successful retry leaves no pending failure");
    }

    #[test]
    fn panicking_input_builder_waits_for_the_next_revision() {
        let (tx, rx) = mpsc::channel();
        let e = LiveEngine::new(Arc::new(|_, job| { std::thread::spawn(job); }), Arc::new(move |event| { let _ = tx.send(event); }), checkpoints());
        let attempts = Arc::new(AtomicU64::new(0));
        let calls = attempts.clone();
        e.settle_with(1, 1, 1, 2, Arc::new(move || {
            if calls.fetch_add(1, Ordering::Relaxed) == 0 { std::panic::panic_any(String::from("input job panic")); }
            input(&plate())
        }));
        idle(&e);
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert!(matches!(rx.iter().find(|e| !matches!(e, LiveEvent::Started { .. })), Some(LiveEvent::Failed { error, .. }) if error == "input job panic"));
        assert!(matches!(rx.recv().unwrap(), LiveEvent::Settled { result: Err(error), .. } if error == "input job panic"));
        assert!(!e.shared.state.lock().unwrap().running);
        assert!(e.shared.state.lock().unwrap().settle.is_none());
        e.release();
        idle(&e);
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
        let events: Vec<_> = rx.try_iter().collect();
        assert!(events.iter().any(|event| matches!(event, LiveEvent::Toolpaths { revision: 2, .. })));
        assert!(!events.iter().any(|event| matches!(event, LiveEvent::Failed { .. })));
    }

    /// A settled slice is the plain slice of the same plate, byte for byte,
    /// however many transforms and threads came before it.
    #[test]
    fn settled_equals_a_fresh_slice() {
        let plate = plate();
        let fresh = crate::plate::slice_plate(&setup(), &plate).unwrap();
        for threads in [1, 4] {
            par::set_threads(threads);
            let (e, rx) = engine();
            e.set_input(input(&plate));
            // A drag that is abandoned: newest wins, nothing queues.
            for k in 0..20 {
                e.set_transform(0, translate(k as f64, 0.5 * k as f64));
            }
            e.release();
            e.settle(7, input(&plate));
            let r = settled(&rx, 7);
            assert!(r.gcode == fresh.gcode, "threads {threads}: settled G-code differs from a fresh slice");
        }
        par::set_threads(1);
    }

    /// Random edit sequences (plates, transforms, releases, settles, at
    /// random moments while work runs): every settle answered for the newest
    /// plate is the fresh slice of that plate, byte for byte.
    #[test]
    fn random_edit_sequences_settle_exactly() {
        let base = plate();
        let variants: Vec<Plate> = (0..6)
            .map(|k| {
                let mut p = base.clone();
                p.parts[1].mesh.translate([k as f64 * 7.5, -(k as f64) * 3.25, 0.0]);
                if k % 2 == 1 {
                    // Taller box: other layers, other toolpaths.
                    for q in &mut p.parts[1].mesh.positions {
                        if q[2] > 1.0 {
                            q[2] += 2.0;
                        }
                    }
                }
                if k >= 4 {
                    // The cap's top face lifted: a local edit high on one part.
                    for q in &mut p.parts[0].mesh.positions {
                        if q[2] > 10.0 {
                            q[2] += 0.5 * (k - 3) as f64;
                        }
                    }
                }
                p
            })
            .collect();
        let fresh: Vec<String> = variants.iter().map(|p| crate::plate::slice_plate(&setup(), p).unwrap().gcode).collect();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let inp = |k: usize| LiveInput::ready(10 + k as u64, 1, (*setup()).clone(), Arc::new(variants[k].clone()), Arc::new(Vec::new()));
        for round in 0..6 {
            par::set_threads([1, 4, 16][round % 3]);
            let (e, rx) = engine();
            let mut tag = 100 * round as u64;
            let mut current = 0usize;
            for _ in 0..(4 + rnd(10)) {
                match rnd(5) {
                    0 => {
                        current = rnd(variants.len() as u64) as usize;
                        e.set_input(inp(current));
                    }
                    1 => e.set_transform(rnd(2) as usize, translate(rnd(9) as f64 - 4.0, rnd(7) as f64 * 0.37)),
                    2 => e.release(),
                    3 => {
                        tag += 1;
                        current = rnd(variants.len() as u64) as usize;
                        e.settle(tag, inp(current));
                    }
                    _ => std::thread::sleep(std::time::Duration::from_millis(rnd(30))),
                }
            }
            tag += 1;
            e.settle(tag, inp(current));
            // Earlier settles may come back (or as superseded), sliced with the
            // transforms of their time: each is exactly the slice of what it reports.
            loop {
                match rx.recv().expect("settled") {
                    LiveEvent::Settled { tag: t, result, .. } if t == tag => {
                        let r = result.expect("sliced");
                        assert!(!r.transformed && r.gcode == fresh[current], "round {round}: settled G-code differs from a fresh slice of plate {current}");
                        break;
                    }
                    LiveEvent::Settled { result: Ok(r), .. } => {
                        let k = variants.iter().position(|v| v.parts.len() == r.sliced.len()).expect("a plate");
                        let again = slice_frames(&setup(), &variants[k], &r.sliced, &[], None).expect("the reported plate slices");
                        assert!(again.gcode == r.gcode, "round {round}: an earlier settle is not the slice of what it reports");
                    }
                    _ => {}
                }
            }
        }
        par::set_threads(1);
    }

    /// Parts given as frames: moving a part on the bed (live, then as a new
    /// plate with only its offset changed) plans nothing again, and the
    /// settled slice is the from-scratch slice of the moved plate.
    #[test]
    fn moved_parts_are_not_planned_again() {
        let base = plate();
        let frames_of = |dx: f64| -> Vec<Frame> {
            base.parts.iter().enumerate().map(|(i, p)| {
                let b = p.mesh.bounds();
                let c = [(b.min[0] + b.max[0]) / 2.0, (b.min[1] + b.max[1]) / 2.0];
                let mut local = p.mesh.clone();
                local.translate([-c[0], -c[1], 0.0]);
                Frame { local: Arc::new(local), offset: [c[0] + if i == 1 { dx } else { 0.0 }, c[1], 0.0], field: None }
            }).collect()
        };
        let plate_of = |frames: &[Frame]| Plate {
            excluded: Vec::new(), settings: Vec::new(),
            parts: base.parts.iter().zip(frames).map(|(p, f)| {
                let mut m = (*f.local).clone();
                m.translate(f.offset);
                Part { mesh: m, ..p.clone() }
            }).collect(),
        };
        let (f0, f1) = (frames_of(0.0), frames_of(-40.25));
        // The moved frames share the parts' local meshes (as a re-evaluated model does).
        let f1: Vec<Frame> = f1.iter().zip(&f0).map(|(a, b)| Frame { local: b.local.clone(), offset: a.offset, field: None }).collect();
        let (p0, p1) = (plate_of(&f0), plate_of(&f1));
        let fresh1 = slice_frames(&setup(), &p1, &f1, &[], None).unwrap().gcode;
        let (e, rx) = engine();
        let input = |id: u64, p: &Plate, f: &[Frame]| {
            let mut i = LiveInput::ready(id, 1, (*setup()).clone(), Arc::new(p.clone()), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(f.to_vec()));
            i
        };
        e.settle(1, input(1, &p0, &f0));
        let _ = settled(&rx, 1);
        let planned = e.parts_planned();
        assert_eq!(planned, 2);
        // The drag (live): contours only, translated.
        for k in 0..10 {
            e.set_transform(1, translate(-4.025 * k as f64, 0.0));
        }
        e.release();
        // The commit: the same parts, the box's offset changed.
        e.settle(2, input(2, &p1, &f1));
        let r = settled(&rx, 2);
        assert_eq!(e.parts_planned(), planned, "a moved part was planned again");
        assert!(r.gcode == fresh1, "the settled slice of the moved plate differs from slicing it from scratch");
    }

    /// An item printed into a part ([`makepad_fab_slicer::embed`]): the engine cuts it in the
    /// part's own frame, so a settle is the fresh slice of the plate with its pause, a host
    /// dragged and moved with its item plans nothing again, and a moved item cuts it again.
    #[test]
    fn an_embedded_item_rides_its_host_and_settles_to_a_fresh_slice() {
        let base = plate();
        let item = |dx: f64, dy: f64| fab_plate::ExcludedPart {
            part: Part { object: "item".into(), name: "Item".into(), mesh: cube([205.5 + dx, 104.0 + dy, 2.0], [4.0, 4.0, 2.0]), ..base.parts[1].clone() },
            link: None, frame: None, insert: Some(fab_plate::Insert { into: "box".into(), clearance: 0.2, embed: true, melt: None }),
            render: Default::default(), handles: Default::default(), model_to_plate: IDENTITY, paint: None,
        };
        let frames_of = |dx: f64| -> Vec<Frame> {
            base.parts.iter().enumerate().map(|(i, p)| {
                let b = p.mesh.bounds();
                let c = [(b.min[0] + b.max[0]) / 2.0, (b.min[1] + b.max[1]) / 2.0];
                let mut local = p.mesh.clone();
                local.translate([-c[0], -c[1], 0.0]);
                Frame { local: Arc::new(local), offset: [c[0] + if i == 1 { dx } else { 0.0 }, c[1], 0.0], field: None }
            }).collect()
        };
        let plate_of = |frames: &[Frame], item: fab_plate::ExcludedPart| Plate {
            excluded: vec![item], settings: Vec::new(),
            parts: base.parts.iter().zip(frames).map(|(p, f)| {
                let mut m = (*f.local).clone();
                m.translate(f.offset);
                Part { mesh: m, ..p.clone() }
            }).collect(),
        };
        let (f0, f1) = (frames_of(0.0), frames_of(-40.25));
        let f1: Vec<Frame> = f1.iter().zip(&f0).map(|(a, b)| Frame { local: b.local.clone(), offset: a.offset, field: None }).collect();
        let (p0, p1) = (plate_of(&f0, item(0.0, 0.0)), plate_of(&f1, item(-40.25, 0.0)));
        let fresh1 = slice_frames(&setup(), &p1, &f1, &[], None).unwrap();
        assert_eq!(fresh1.gcode.matches("; FAB_PAUSE: ").count(), 1, "the fresh slice pauses for the item");
        let (e, rx) = engine();
        let input = |id: u64, p: &Plate, f: &[Frame]| {
            let mut i = LiveInput::ready(id, 1, (*setup()).clone(), Arc::new(p.printable()), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(f.to_vec()));
            i
        };
        e.settle(1, input(1, &p0, &f0));
        let _ = settled(&rx, 1);
        let planned = e.parts_planned();
        for k in 0..10 {
            e.set_transform(1, translate(-4.025 * k as f64, 0.0));
        }
        e.release();
        e.settle(2, input(2, &p1, &f1));
        let r = settled(&rx, 2);
        assert_eq!(e.parts_planned(), planned, "a host moved with its item is not planned again");
        assert!(r.gcode == fresh1.gcode, "the settled slice of the moved plate differs from slicing it from scratch");
        let p2 = plate_of(&f1, item(-40.25, 1.5));
        e.settle(3, input(3, &p2, &f1));
        let r = settled(&rx, 3);
        assert_eq!(e.parts_planned(), planned + 1, "a moved item cuts its host again, and only it");
        assert!(r.gcode == slice_frames(&setup(), &p2, &f1, &[], None).unwrap().gcode && r.gcode != fresh1.gcode, "the moved item's cut is its fresh slice");
    }

    /// Parts given as frames whose colours are mapped anew (the same local meshes, the two
    /// parts' tools swapped): each plans with its new filament, and the settled slice is the
    /// from-scratch slice of the plate on its new tools.
    #[test]
    fn a_part_on_another_tool_plans_with_it() {
        let base = plate();
        let frames: Vec<Frame> = base.parts.iter().map(|p| {
            let b = p.mesh.bounds();
            let c = [(b.min[0] + b.max[0]) / 2.0, (b.min[1] + b.max[1]) / 2.0];
            let mut local = p.mesh.clone();
            local.translate([-c[0], -c[1], 0.0]);
            Frame { local: Arc::new(local), offset: [c[0], c[1], 0.0], field: None }
        }).collect();
        let plate_on = |tools: [u32; 2]| Plate {
            excluded: Vec::new(), settings: Vec::new(),
            parts: base.parts.iter().zip(&frames).zip(tools).map(|((p, f), filament)| {
                let mut m = (*f.local).clone();
                m.translate(f.offset);
                Part { mesh: m, filament, ..p.clone() }
            }).collect(),
        };
        let (p0, p1) = (plate_on([1, 2]), plate_on([2, 1]));
        let fresh = slice_frames(&setup(), &p1, &frames, &[], None).unwrap();
        let (e, rx) = engine();
        let input = |id: u64, p: &Plate| {
            let mut i = LiveInput::ready(id, 1, (*setup()).clone(), Arc::new(p.clone()), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(frames.clone()));
            i
        };
        e.settle(1, input(1, &p0));
        let first = settled(&rx, 1);
        e.settle(2, input(2, &p1));
        let r = settled(&rx, 2);
        assert_ne!(first.stats.filament_mm, fresh.stats.filament_mm, "the swap moves filament between the tools");
        assert_eq!(r.stats.filament_mm, fresh.stats.filament_mm, "each part prints on its new tool");
        assert!(r.gcode == fresh.gcode, "the settled slice on the new tools differs from slicing it from scratch");
    }

    /// A part painted by position whose painted colour is mapped anew (the same paint, its slots
    /// on another tool): the painted slot plans with the new tool, and the settled slice is a
    /// fresh engine's on it.
    #[test]
    fn a_painted_colour_on_another_tool_plans_with_it() {
        let local = Arc::new(cube([-4.0, -4.0, 0.0], [8.0, 8.0, 12.0]));
        let mut mesh = (*local).clone();
        mesh.translate([150.0, 120.0, 0.0]);
        let frames = vec![Frame { local, offset: [150.0, 120.0, 0.0], field: None }];
        let plate_on = |tool: u32| {
            let mut paint = ball_paint([154.0, 120.0, 6.0]);
            paint.colors[1].filament = Some(tool);
            Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![Part { body_of: None, print: Vec::new(), print_mesh: None, paint: Some(paint), object: "post".into(), name: "post".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh: mesh.clone(), field_bounds: None, field_floor: None, ..Default::default() }] }
        };
        let mut s = (*setup()).clone();
        s.filaments.push(s.filaments[0].clone());
        s.filament_map.push(1);
        s.filament_colours.push(None);
        let (p0, p1) = (plate_on(2), plate_on(3));
        let input = |id: u64, p: &Plate| {
            let mut i = LiveInput::ready(id, 1, s.clone(), Arc::new(p.clone()), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(frames.clone()));
            i
        };
        let fresh = {
            let (e, rx) = engine();
            e.settle(1, input(1, &p1));
            settled(&rx, 1)
        };
        assert_eq!(fresh.tools, vec![0, 2], "the ball prints on tool 3");
        assert_eq!(fresh.stats.used, vec![0, 1], "the job has only the tools that print");
        let (e, rx) = engine();
        e.settle(1, input(1, &p0));
        let _ = settled(&rx, 1);
        e.settle(2, input(2, &p1));
        let r = settled(&rx, 2);
        assert_eq!(r.tools, fresh.tools, "the ball prints on its new tool");
        assert!(r.gcode == fresh.gcode, "the settled slice on the new tool differs from slicing it there from scratch");
    }

    /// Paint in colours that all print on the part's own tool reaches no path: moving it plans no
    /// layer and leaves the G-code as it was, byte for byte.
    #[test]
    fn a_paint_edit_on_no_other_tool_plans_nothing() {
        let local = Arc::new(cube([-4.0, -4.0, 0.0], [8.0, 8.0, 12.0]));
        let mut mesh = (*local).clone();
        mesh.translate([150.0, 120.0, 0.0]);
        let frames = vec![Frame { local, offset: [150.0, 120.0, 0.0], field: None }];
        let plate_at = |z: f64| {
            let mut paint = ball_paint([154.0, 120.0, z]);
            paint.colors[1].filament = Some(1);
            Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![Part { body_of: None, print: Vec::new(), print_mesh: None, paint: Some(paint), object: "post".into(), name: "post".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh: mesh.clone(), field_bounds: None, field_floor: None, ..Default::default() }] }
        };
        let input = |id: u64, p: &Plate| {
            let mut i = LiveInput::ready(id, 1, (*setup()).clone(), Arc::new(p.clone()), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(frames.clone()));
            i
        };
        let (e, rx) = engine();
        e.settle(1, input(1, &plate_at(3.0)));
        let first = settled(&rx, 1);
        let before = e.part_work();
        e.settle(2, input(2, &plate_at(9.0)));
        let edited = settled(&rx, 2);
        let after = e.part_work();
        assert_eq!((after[0].0 - before[0].0, after[0].1 - before[0].1), (0, 0), "no layer traced or planned");
        assert!(edited.gcode == first.gcode, "the G-code changed");
    }

    /// While a part moves only contours are published (the moved part at
    /// its new offset); on release the toolpaths follow.
    #[test]
    fn contours_while_moving_toolpaths_after() {
        let (e, rx) = engine();
        let plate = Arc::new(plate());
        e.set_input(input(&plate));
        e.set_transform(1, translate(-50.0, 30.0));
        let moved = Pt::from_mm(-50.0, 30.0);
        loop {
            match rx.recv().expect("contours") {
                LiveEvent::Contours { parts, .. } if parts[1].1 == moved => break,
                LiveEvent::Toolpaths { parts, .. } => assert!(parts[1].1 != moved, "no toolpaths while moving"),
                _ => {}
            }
        }
        e.release();
        let mut layers = 0;
        loop {
            match rx.recv().expect("toolpaths") {
                LiveEvent::PartLayers { layers: l, .. } => layers += l.len(),
                LiveEvent::Toolpaths { parts, .. } if parts[1].1 == moved => break,
                _ => {}
            }
        }
        assert!(layers > 0, "the plans were streamed");
    }

    /// A geometry edit plans each layer of the dirty box once, leaves the
    /// other part alone, and settles to the fresh slice of the edited plate.
    #[test]
    fn geometry_edit_plans_each_dirty_layer_once() {
        let p0 = plate();
        let mut p1 = p0.clone();
        for q in &mut p1.parts[1].mesh.positions {
            if q[2] > 1.0 {
                q[2] = 9.0;
            }
        }
        let fresh1 = crate::plate::slice_plate(&setup(), &p1).unwrap();
        let (e, rx) = engine();
        e.settle(1, LiveInput::ready(1, 1, (*setup()).clone(), Arc::new(p0.clone()), Arc::new(Vec::new())));
        let _ = settled(&rx, 1);
        let before = e.layers_computed();
        e.settle(2, LiveInput::ready(2, 1, (*setup()).clone(), Arc::new(p1.clone()), Arc::new(Vec::new())));
        let r = settled(&rx, 2);
        let box_layers = fresh1.layers().iter().filter(|l| l.objects.iter().any(|o| o.object == 1)).count() as u64;
        let computed = e.layers_computed() - before;
        assert_eq!(computed, box_layers, "every dirty layer is planned exactly once");
        assert!(r.gcode == fresh1.gcode, "the incremental slice differs from the fresh one");
    }

    /// A top-layers edit of an adaptive part keeps its warp: it solves none, and its viewed layer
    /// plans from the window's own contours. The parts: a cube, and a field post painted by
    /// position whose paint skin reaches past the window's shell context (a field part asserts
    /// that it is given every contour it reads).
    #[test]
    fn a_shell_edit_reuses_the_parts_warp_for_its_viewed_layer() {
        use makepad_csg_sdf::SdfNode;
        let mut cube_plate = plate();
        cube_plate.parts.truncate(1);
        cube_plate.parts[0].mesh = cube([96.0, 96.0, 0.0], [8.0; 3]);
        // A 12 mm post 20 mm tall (model Y up), a ball of tool 2 on its +X wall.
        let shape = Arc::new(SdfShape { root: SdfNode::Box { size: [12.0, 20.0, 12.0], radius: 0.0 }, min: [-6.5, -10.5, -6.5], max: [6.5, 10.5, 6.5], materials: Vec::new() });
        let local = Arc::new(cube([-6.0, -6.0, 0.0], [12.0, 12.0, 20.0]));
        let mut mesh = (*local).clone();
        mesh.translate([150.0, 120.0, 0.0]);
        let post = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![Part { paint: Some(ball_paint([156.0, 120.0, 10.0])), object: "post".into(), name: "post".into(), filament: 1, color: [1.0; 4], mesh, ..Default::default() }] };
        let place = [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 10.0]];
        let frames = Arc::new(vec![Frame { local, offset: [150.0, 120.0, 0.0], field: Some(PartField::new(shape, place)) }]);
        // Each viewed at its middle, where the post's paint skin lies inside it both ways.
        for (name, p, frames, mut setup, focus) in [("cube", cube_plate, None, setup().keep(&[0]), 4.0), ("painted post", post, Some(frames), (*setup()).clone(), 10.0)] {
            let p = Arc::new(p);
            for (k, v) in [("sparse_infill_pattern", "gyroid"), ("adaptive_infill_mode", "lobes"), ("sparse_infill_density", "40%"),
                ("layer_height", "0.25"), ("initial_layer_print_height", "0.25"), ("top_shell_layers", "4"), ("fab_paint_skin_depth", "4")] {
                setup.process.values.set_str(k, v);
            }
            let (e, rx) = engine();
            e.set_focus_z(Some(focus));
            let input = |id, s| {
                let mut i = LiveInput::ready(1, id, s, p.clone(), Arc::new(Vec::new()));
                i.frames = frames.clone();
                i
            };
            e.settle(1, input(1, setup.clone()));
            let _ = settled(&rx, 1);
            idle(&e);
            let solves = grade::solves();
            let cached = || {
                let c = e.shared.caches.lock().unwrap();
                let (key, warp) = c.grades[0].as_ref().expect("the finished revision keeps its part's warp");
                assert!(warp.graded, "the adaptive {name} must have a graded interior");
                (*key, Arc::downgrade(warp))
            };
            let (key, first) = cached();
            let planned = e.layers_computed();
            setup.process.values.set_str("top_shell_layers", "5");
            e.settle(2, input(2, setup));
            let mut first_cut = None;
            loop {
                match rx.recv().expect("the edited revision settles") {
                    LiveEvent::PartCut { part: 0, layers, stale, .. } if first_cut.is_none() => {
                        let pending: usize = stale.iter().map(|&(lo, hi)| (hi - lo) as usize).sum();
                        first_cut = Some((layers.len(), pending));
                    }
                    LiveEvent::Settled { tag: 2, result, .. } => { result.expect("the shell edit slices"); break; }
                    _ => {}
                }
            }
            idle(&e);
            let (total, pending) = first_cut.expect("the shell edit publishes a focus cut");
            assert!(0 < pending && pending < total, "the {name}'s warm focus cut must trace a proper window before publishing it ({pending} of {total} pending)");
            assert!(e.layers_computed() > planned, "the {name}'s shell edit must replan model layers");
            assert_eq!(grade::solves(), solves, "the {name}'s shell edit must solve no warp");
            let (new_key, second) = cached();
            assert!(key == new_key && first.ptr_eq(&second), "the revised {name} must keep the same keyed warp");
        }
    }

    /// With a cross-layer pattern (lightning, adaptive cubic) an edit reaches
    /// every layer's infill: the incremental slice is still the fresh one.
    #[test]
    fn cross_layer_patterns_settle_exactly_after_an_edit() {
        for pattern in ["lightning", "adaptivecubic"] {
            let mut s = (*setup()).clone();
            s.process.values.set_str("sparse_infill_pattern", pattern);
            let p0 = plate();
            let mut p1 = p0.clone();
            for q in &mut p1.parts[0].mesh.positions {
                if q[2] > 10.0 {
                    q[2] += 1.0;
                }
            }
            let fresh1 = crate::plate::slice_plate(&s, &p1).unwrap();
            let (e, rx) = engine();
            e.settle(1, LiveInput::ready(1, 1, s.clone(), Arc::new(p0), Arc::new(Vec::new())));
            let _ = settled(&rx, 1);
            e.settle(2, LiveInput::ready(2, 1, s.clone(), Arc::new(p1), Arc::new(Vec::new())));
            let r = settled(&rx, 2);
            assert!(r.gcode == fresh1.gcode, "{pattern}: the incremental slice differs from the fresh one");
        }
    }

    /// A 20 x 16 x 6 box minus a radius-4 vertical hole as a typed SDF shape
    /// (model frame: Y up, centred), and the map that stands it on the bed
    /// in its frame (model (x, y, z) → frame (x, -z, y + 3)).
    fn sdf_part() -> PartField {
        use makepad_csg_sdf::{SdfNode, SdfOp};
        let root = SdfNode::Combine {
            op: SdfOp::Difference,
            blend: 0.0,
            of: vec![SdfNode::Box { size: [20.0, 6.0, 16.0], radius: 0.0 }, SdfNode::Cylinder { radius: 4.0, top_radius: 4.0, height: 8.0 }],
        };
        let shape = Arc::new(SdfShape { root, min: [-10.5, -3.5, -8.5], max: [10.5, 3.5, 8.5], materials: Vec::new() });
        PartField::new(shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 3.0]])
    }

    /// Its display mesh in its frame: the plain box (so a slice from the
    /// mesh would show no hole).
    fn sdf_part_mesh() -> PartMesh {
        cube([-10.0, -8.0, 0.0], [20.0, 16.0, 6.0])
    }

    pub(super) fn sdf_plate(offset: [f64; 3]) -> (Plate, Vec<Frame>) {
        let base = plate();
        let local = Arc::new(sdf_part_mesh());
        let mut m = (*local).clone();
        m.translate(offset);
        let part = Part { mesh: m, ..base.parts[1].clone() };
        // A one-layer pad of mesh beside the field part (not the cap on its stem: cheap to slice).
        let pad = Part { mesh: cube([96.0, 96.0, 0.0], [8.0, 8.0, 0.4]), ..base.parts[0].clone() };
        let (c, mut pad_local) = ([100.0, 100.0], pad.mesh.clone());
        pad_local.translate([-c[0], -c[1], 0.0]);
        let frames = vec![Frame { local: Arc::new(pad_local), offset: [c[0], c[1], 0.0], field: None }, Frame { local, offset, field: Some(sdf_part()) }];
        (Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![pad, part] }, frames)
    }

    /// A model-frame (Y up) node moved by `at` and turned by `rotate` (degrees).
    fn placed(at: [f64; 3], rotate: [f64; 3], of: makepad_csg_sdf::SdfNode) -> makepad_csg_sdf::SdfNode {
        makepad_csg_sdf::SdfNode::Transform { at, rotate, scale: 1.0, of: Box::new(of) }
    }

    /// Two thin flat ellipsoids crossed at 45 degrees, their crossing at a coarse cell's centre:
    /// the cell's corners lie on both mid planes, where the estimate overstates the depth about
    /// 120 times, and the notches between the walls are in the cell. The part's source, its
    /// coarse search at the slope and window its clearance proves, traces every layer as a
    /// search that skips no cell does. At a distance's slope over every value the search skips
    /// the crossing: the test sees what it guards.
    #[test]
    fn the_coarse_search_skips_no_thin_wall() {
        use makepad_csg_sdf::{SdfNode, SdfOp};
        let wall = |turn: f64| placed([0.0; 3], [0.0, 0.0, turn], SdfNode::Ellipsoid { radii: [0.25, 30.0, 3.0] });
        let root = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![wall(45.0), wall(-45.0)] };
        // the coarse nodes stand on the world's whole mm: the crossing at the half
        let shape = Arc::new(SdfShape { root, min: [-21.5, -21.5, -3.0], max: [21.5, 21.5, 3.0], materials: Vec::new() });
        let pf = PartField::new(shape, [[1.0, 0.0, 0.0, 0.5], [0.0, 1.0, 0.0, 0.5], [0.0, 0.0, 1.0, 3.0]]);
        let zs = [2.6, 3.0, 3.35, 4.1];
        // a window of 0 proves no cell clear: the search skips none
        let at = |lipschitz: f64, window: f64, zs: &[f64]| {
            let mut s = SdfSource::new(pf.field().unwrap(), ([-21.0, -21.0, 0.0], [22.0, 22.0, 6.0]));
            (s.params.lipschitz, s.params.window) = (lipschitz, window);
            s.contours(zs)
        };
        let rings = |layer: &[ExPolygon]| layer.iter().map(|e| 1 + e.holes.len()).sum::<usize>();
        let area = |layer: &[ExPolygon]| layer.iter().map(|e| e.area_mm2()).sum::<f64>();
        let same = |a: &[ExPolygon], b: &[ExPolygon]| rings(a) == rings(b) && (area(a) - area(b)).abs() <= 1e-9 * area(b);
        let got = pf.source().unwrap().contours(&zs);
        for (&z, g) in zs.iter().zip(&got) {
            let all = at(pf.plane_slope(z).lipschitz, 0.0, &[z]);
            assert!(same(g, &all[0]), "layer {z}: {} rings of {:.4} mm2, a search that skips none {} of {:.4}", rings(g), area(g), rings(&all[0]), area(&all[0]));
        }
        assert!(at(1.0, f64::INFINITY, &zs).iter().zip(&at(1.0, 0.0, &zs)).any(|(g, a)| !same(g, a)), "a distance's slope at every value skips the crossing");
    }

    /// A small L bracket with 23 holes: a 30 x 18 x 5 foot with 8 vertical holes and a
    /// 30 x 24 x 5 wall (filament 2, through a material slot) with 15 horizontal ones, a
    /// blended rib and a row of instanced bosses. Stood on the bed like [`sdf_part`] (model
    /// Y is frame Z).
    fn bracket() -> PartField {
        use makepad_csg_sdf::{SdfNode, SdfOp, SdfPlace};
        let hole = |h: f64| SdfNode::Cylinder { radius: 1.5, top_radius: 1.5, height: h };
        let foot = placed([0.0, 2.5, 0.0], [0.0; 3], SdfNode::Box { size: [30.0, 5.0, 18.0], radius: 0.0 });
        let wall = SdfNode::Material { slot: 1, within: None, of: Box::new(placed([0.0, 14.5, -6.5], [0.0; 3], SdfNode::Box { size: [30.0, 24.0, 5.0], radius: 0.5 })) };
        let rib = placed([0.0, 7.0, -3.5], [45.0, 0.0, 0.0], SdfNode::Box { size: [3.0, 10.0, 3.0], radius: 0.0 });
        let boss = SdfNode::Cylinder { radius: 1.5, top_radius: 1.5, height: 3.0 };
        let places = (0..4).map(|i| SdfPlace { at: [-9.0 + 6.0 * i as f64, 6.5, 5.0], ..SdfPlace::IDENTITY }).collect();
        let body = SdfNode::Combine { op: SdfOp::Union, blend: 2.0, of: vec![foot, wall, rib, SdfNode::Instance { places, of: Box::new(boss) }] };
        let mut of = vec![body];
        for i in 0..4 {
            for j in 0..2 {
                of.push(placed([-10.5 + 7.0 * i as f64, 2.5, 6.0 * j as f64], [0.0; 3], hole(8.0)));
            }
        }
        for i in 0..5 {
            for j in 0..3 {
                of.push(placed([-12.0 + 6.0 * i as f64, 8.0 + 6.0 * j as f64, -6.5], [90.0, 0.0, 0.0], hole(8.0)));
            }
        }
        let root = SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of };
        let shape = Arc::new(SdfShape { root, min: [-16.0, -0.5, -10.0], max: [16.0, 27.5, 10.0], materials: Vec::new() });
        PartField::new(shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 0.5]]).with_states(vec![0, 2])
    }

    /// A hanging chain of 10 links (tori, alternate links turned a quarter),
    /// standing on the bed.
    fn chain() -> PartField {
        use makepad_csg_sdf::{SdfNode, SdfOp};
        let of = (0..10).map(|i| placed([0.0, 6.0 + 7.0 * i as f64, 0.0], [90.0, if i % 2 == 0 { 0.0 } else { 90.0 }, 0.0], SdfNode::Torus { radius: 4.5, tube: 1.2 })).collect();
        let root = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of };
        let shape = Arc::new(SdfShape { root, min: [-6.5, -0.5, -6.5], max: [6.5, 76.5, 6.5], materials: Vec::new() });
        PartField::new(shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 0.5]])
    }

    /// Mesh balls and cables blended up a post on a foot a mesh ball is cut out of, with a ball
    /// read as a kernel on top: the leaves a tree reads only as far as bounds need, at heights
    /// apart (most of them culled from each layer).
    fn cages() -> PartField {
        use makepad_csg_sdf::mesh_field::{MeshData, MeshField};
        use makepad_csg_sdf::{SdfKernel, SdfNode, SdfOp, TubeCurve, Tubes};
        let ball = |r: f64| {
            let (rings, segments) = (10u32, 20u32);
            let mut positions = vec![[0.0, r as f32, 0.0]];
            for i in 1..rings {
                let phi = std::f64::consts::PI * i as f64 / rings as f64;
                for j in 0..segments {
                    let theta = std::f64::consts::TAU * j as f64 / segments as f64;
                    positions.push([(r * phi.sin() * theta.cos()) as f32, (r * phi.cos()) as f32, (r * phi.sin() * theta.sin()) as f32]);
                }
            }
            positions.push([0.0, -r as f32, 0.0]);
            let south = positions.len() as u32 - 1;
            let v = |i: u32, j: u32| 1 + (i - 1) * segments + j % segments;
            let mut triangles = Vec::new();
            for j in 0..segments {
                triangles.push([0, v(1, j + 1), v(1, j)]);
                for i in 1..rings - 1 {
                    triangles.push([v(i, j), v(i + 1, j + 1), v(i + 1, j)]);
                    triangles.push([v(i, j), v(i, j + 1), v(i + 1, j + 1)]);
                }
                triangles.push([south, v(rings - 1, j), v(rings - 1, j + 1)]);
            }
            SdfNode::Mesh(Arc::new(MeshField::new(MeshData { positions, triangles })))
        };
        let foot = SdfNode::Combine { op: SdfOp::Difference, blend: 0.5, of: vec![placed([0.0, 1.5, 0.0], [0.0; 3], SdfNode::Box { size: [16.0, 3.0, 16.0], radius: 0.0 }), placed([4.0, 3.0, 0.0], [0.0; 3], ball(2.5))] };
        let post = placed([0.0, 13.0, 0.0], [0.0; 3], SdfNode::Box { size: [2.0, 22.0, 2.0], radius: 0.3 });
        let cable = |x: f64, z: f64| TubeCurve { pts: vec![[x, 4.0, z, 0.7], [-x * 0.5, 10.0, z + 1.0, 0.5], [x, 16.0, -z, 0.6], [0.0, 22.0, 0.0, 0.8]], slot: 0 };
        let cables = SdfNode::Tubes(Arc::new(Tubes::new(vec![cable(5.0, 2.0), cable(-5.0, -2.0)], 0.0)));
        let top = SdfNode::Kernel(SdfKernel { key: 7, field: Arc::new(makepad_csg_sdf::SdfSphere::new(Vec3d::new(0.0, 0.0, 0.0), 3.0)), splash: None, bounds: Some(([-3.0; 3], [3.0; 3])), solid: None });
        let of = vec![foot, post, placed([-4.0, 7.0, 0.0], [0.0; 3], ball(3.0)), placed([4.0, 13.0, 1.0], [0.0; 3], ball(2.5)), placed([-3.0, 19.0, -1.0], [0.0; 3], ball(3.0)), cables, placed([0.0, 26.0, 0.0], [0.0; 3], top)];
        let root = SdfNode::Combine { op: SdfOp::Union, blend: 1.0, of };
        let shape = Arc::new(SdfShape { root, min: [-8.5, -0.5, -8.5], max: [8.5, 29.5, 8.5], materials: Vec::new() });
        PartField::new(shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 0.5]])
    }

    /// The part's source with and without the per-layer cull.
    fn culled_and_whole(pf: &PartField) -> (SdfSource, SdfSource) {
        let (m, (a, b)) = (pf.to_frame, (pf.shape.min, pf.shape.max));
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for k in 0..8 {
            let c = [if k & 1 == 0 { a[0] } else { b[0] }, if k & 2 == 0 { a[1] } else { b[1] }, if k & 4 == 0 { a[2] } else { b[2] }];
            for r in 0..3 {
                let v = m[r][0] * c[0] + m[r][1] * c[1] + m[r][2] * c[2] + m[r][3];
                lo[r] = lo[r].min(v);
                hi[r] = hi[r].max(v);
            }
        }
        let whole = |slab: bool| {
            let src = SdfSource::new(pf.field().unwrap(), (lo, hi));
            let src = if slab { src.with_slab(pf.slab((lo, hi))) } else { src };
            match pf.material() {
                Some(m) => src.with_material(m),
                None => src,
            }
        };
        (whole(true), whole(false))
    }

    fn node_count(n: &makepad_csg_sdf::SdfNode) -> usize {
        use makepad_csg_sdf::SdfNode::*;
        1 + match n {
            Combine { of, .. } => of.iter().map(node_count).sum(),
            Shell { of, .. } | Round { of, .. } | Transform { of, .. } | Material { of, .. } | Id { of, .. } | Bone { of, .. } | Deform { of, .. } | Instance { of, .. } => node_count(of),
            _ => 0,
        }
    }

    /// Culled to each layer's slab, a many-feature part traces exactly the segments (so the
    /// regions: they are a function of the segments), the paint and the slope profile of its
    /// whole tree, and the cull does drop most of its features. Layers are sampled every
    /// `step` mm, which puts some of them inside the cull's margin of a feature's edge.
    #[test]
    fn slab_cull_traces_exactly() {
        for (name, pf, step) in [("bracket", bracket(), 2.5), ("chain", chain(), 1.3)] {
            let (culled, whole) = culled_and_whole(&pf);
            let top = whole.bounds.1[2];
            let zs: Vec<f64> = (0..).map(|i| 0.1 + step * i as f64).take_while(|z| *z < top).collect();
            let full = node_count(&pf.shape.root);
            let mut fewest = full;
            for &z in &zs {
                assert_eq!(culled.segments(z), whole.segments(z), "{name}: layer {z} differs");
                let m = culled.slab_margin_at(z);
                let frame = pf.to_frame;
                let into = |p: [f64; 3]| {
                    let d = [p[0] - frame[0][3], p[1] - frame[1][3], p[2] - frame[2][3]];
                    std::array::from_fn::<f64, 3, _>(|c| frame[0][c] * d[0] + frame[1][c] * d[1] + frame[2][c] * d[2])
                };
                let (lo, hi) = whole.bounds;
                let region = makepad_csg_sdf::bounds::map_bounds(&([lo[0] - m, lo[1] - m, z - m], [hi[0] + m, hi[1] + m, z + m]), into);
                fewest = fewest.min(makepad_csg_sdf::cull(&pf.shape.root, &region).map_or(0, |n| node_count(&n)));
            }
            assert!(fewest * 3 < full, "{name}: the cull kept {fewest} of {full} nodes");
            if whole.material.is_some() {
                // The same regions into both: the paint is what differs.
                let slices = culled.contours(&zs);
                let p = PaintParams { top_layers: 3, bottom_layers: 3, shrink: 0.5, max_width: 0.0, interlocking_depth: 0.0 };
                assert!(culled.paint_regions(&zs, &slices, &p) == whole.paint_regions(&zs, &slices, &p), "{name}: paint differs");
            }
            let (sa, sb) = (culled.slope_profile(4.0).unwrap(), whole.slope_profile(4.0).unwrap());
            let bad: Vec<_> = (0..sa.nz.len()).filter(|&k| sa.nz[k] != sb.nz[k]).map(|k| (k, sa.nz[k], sb.nz[k])).take(5).collect();
            assert!(bad.is_empty() && sa.flats == sb.flats, "{name}: slope profile differs: nz {bad:?}, flats {:?} vs {:?}", sa.flats, sb.flats);
        }
    }

    /// The part's layers as a tracer that reads every value in full would trace them: each
    /// layer's slab culled, its values read a batch at a time whatever the tracer's reach.
    fn read_in_full(pf: &PartField) -> SdfSource {
        let (culled, _) = culled_and_whole(pf);
        let (lo, hi) = culled.bounds;
        let (shape, frame) = (pf.shape.clone(), pf.to_frame);
        let into = move |p: [f64; 3]| {
            let d = [p[0] - frame[0][3], p[1] - frame[1][3], p[2] - frame[2][3]];
            std::array::from_fn::<f64, 3, _>(|c| frame[0][c] * d[0] + frame[1][c] * d[1] + frame[2][c] * d[2])
        };
        SdfSource::new(pf.field().unwrap(), (lo, hi)).with_slab(Arc::new(move |z0: f64, z1: f64| {
            let g = 0.5 * (z1 - z0);
            let region = makepad_csg_sdf::bounds::map_bounds(&([lo[0] - g, lo[1] - g, z0], [hi[0] + g, hi[1] + g, z1]), into);
            let Some(node) = makepad_csg_sdf::cull(&shape.root, &region).map(Arc::new) else {
                return Slab { field: Arc::new(|_, _, _| f64::MAX), many: None, material: None };
            };
            let n = node.clone();
            let field: makepad_fab_slicer::sdf::Field3 = Arc::new(move |x: f64, y: f64, z: f64| {
                let q = into([x, y, z]);
                makepad_csg_sdf::distance(&n, Vec3d::new(q[0], q[1], q[2]))
            });
            let many: makepad_fab_slicer::sdf::Field3Many = Arc::new(move |pts: &[[f64; 3]], _: f64, out: &mut [f64]| {
                let q: Vec<Vec3d> = pts.iter().map(|p| {
                    let q = into(*p);
                    Vec3d::new(q[0], q[1], q[2])
                }).collect();
                makepad_csg_sdf::distance_many(&node, &q, out)
            });
            Slab { field, many: Some(many), material: None }
        }))
    }

    /// A field part's layers read through its chunks, each tree only as far as the tracer's
    /// reaches need, trace the segments of every value read in full, bit for bit: meshes, cables
    /// and a kernel blended up a post, a many-feature bracket and a chain. Prints what the layers
    /// cost each way.
    #[test]
    fn reads_as_far_as_bounds_need_trace_the_segments_of_full_reads() {
        for (name, pf, step) in [("cages", cages(), 0.7), ("bracket", bracket(), 2.5), ("chain", chain(), 1.3)] {
            let ((bounded, _), full) = (culled_and_whole(&pf), read_in_full(&pf));
            let zs: Vec<f64> = (0..).map(|i| 0.1 + step * i as f64).take_while(|z| *z < bounded.bounds.1[2]).collect();
            let (mut tb, mut tf) = (0.0, 0.0);
            for &z in &zs {
                let t = std::time::Instant::now();
                let got = bounded.segments(z);
                tb += t.elapsed().as_secs_f64();
                let t = std::time::Instant::now();
                let want = full.segments(z);
                tf += t.elapsed().as_secs_f64();
                assert!(got == want, "{name}: layer {z}: {} segments read as far as bounds need, {} read in full", got.len(), want.len());
            }
            println!("{name}: {} layers, {:.1} ms read as far as bounds need, {:.1} ms read in full", zs.len(), tb * 1e3, tf * 1e3);
        }
    }

    /// A touch looks for a driver to stop only after its new revision is in: by then the older
    /// driver may have ended and this revision's started (the caller preempted in between, as on
    /// a loaded machine). That driver runs this input and goes on. A seeded drag of the field's
    /// hole ends in a settle whose touch interleaves so: it settles, rather than being served
    /// with its viewed layers alone ("Slicing... 0%" for good). The pool holds every run's
    /// background polls until the test lets them go.
    #[test]
    fn a_touch_leaves_its_own_revisions_driver_running() {
        use makepad_csg_sdf::{SdfNode, SdfOp};
        type Job = Box<dyn FnOnce() + Send>;
        let (e, rx) = engine();
        // Viewed polls run at once; the others wait here while it is Some.
        let held: Arc<Mutex<Option<Vec<Job>>>> = Arc::new(Mutex::new(Some(Vec::new())));
        let h = held.clone();
        e.set_resume(Arc::new(move |_| {
            let h = h.clone();
            Arc::new(move |urgency, job| {
                let mut held = h.lock().unwrap();
                match held.as_mut() {
                    Some(jobs) if urgency != Urgency::Viewed => jobs.push(job),
                    _ => { drop(held); std::thread::spawn(job); }
                }
            })
        }));
        // A Height drag's steps: the field part's hole (a new content each step).
        let input = |id: u64, hole: f64| {
            let root = SdfNode::Combine { op: SdfOp::Difference, blend: 0.0,
                of: vec![SdfNode::Box { size: [20.0, 6.0, 16.0], radius: 0.0 }, SdfNode::Cylinder { radius: hole, top_radius: hole, height: 8.0 }] };
            let shape = Arc::new(SdfShape { root, min: [-10.5, -3.5, -8.5], max: [10.5, 3.5, 8.5], materials: Vec::new() });
            let (p, mut f) = sdf_plate([150.0, 120.0, 0.0]);
            f[1].field = Some(PartField::new(shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 3.0]]));
            let mut i = LiveInput::ready(id, 1, (*setup()).clone(), Arc::new(p), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(f));
            i
        };
        let mut seed = 0x5eed_u64;
        let mut hole = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            2.5 + (seed >> 40) as f64 / (1u64 << 24) as f64 * 3.0
        };
        for id in 1..=6 {
            e.set_input(input(id, hole()));
        }
        // The drag's last driver drew its viewed layers and waits with the rest.
        let last = e.shared.state.lock().unwrap().revision;
        e.shared.wait_for(|| e.shared.drive.lock().unwrap().driver.as_ref().is_some_and(|d| d.revision() == last && d.waiting()));
        let shared = e.shared.clone();
        *e.shared.touched.lock().unwrap() = Some(Box::new(move || {
            // Between the touch's revision and its look: the background goes on, the last driver
            // sees the newer revision and ends, and the settle's driver starts.
            for job in held.lock().unwrap().take().unwrap() { std::thread::spawn(job); }
            shared.wait_for(|| shared.drive.lock().unwrap().driver.as_ref().is_some_and(|d| d.revision() > last));
        }));
        e.settle(7, input(7, hole()));
        // Its Settled event goes out before its driver ends.
        e.wait_idle();
        let result = rx.try_iter().find_map(|event| match event { LiveEvent::Settled { tag: 7, result, .. } => Some(result.is_ok()), _ => None });
        assert_eq!(result, Some(true), "the settle was served with its viewed layers alone");
    }

    /// A pure SDF part is sliced from its field: the engine settles to the
    /// from-scratch slice of the same frames, the hole is a true circle, and
    /// a part that only moved is not planned again.
    #[test]
    fn sdf_parts_slice_from_the_field() {
        let (p0, f0) = sdf_plate([150.0, 120.0, 0.0]);
        let (p1, f1) = sdf_plate([170.5, 95.25, 0.0]);
        let fresh0 = slice_frames(&setup(), &p0, &f0, &[], None).unwrap();
        let fresh1 = slice_frames(&setup(), &p1, &f1, &[], None).unwrap();
        let (e, rx) = engine();
        let input = |id: u64, p: &Plate, f: &[Frame]| {
            let mut i = LiveInput::ready(id, 1, (*setup()).clone(), Arc::new(p.clone()), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(f.to_vec()));
            i
        };
        e.settle(1, input(1, &p0, &f0));
        let r0 = settled(&rx, 1);
        assert!(r0.gcode == fresh0.gcode, "the settled SDF slice differs from the fresh one");
        assert!(r0.sliced[1].field.is_some(), "the SDF part was not sliced from its field");
        let planned = e.parts_planned();
        e.settle(2, input(2, &p1, &f1));
        let r1 = settled(&rx, 2);
        assert!(r1.gcode == fresh1.gcode, "the moved SDF slice differs from the fresh one");
        assert_eq!(e.parts_planned(), planned, "a moved SDF part was planned again");
        // The field's cut: the box minus a true circle (the mesh path has no
        // hole at all here, its display mesh is the plain box).
        let src = sdf_part().source().unwrap();
        let cut = src.contours(&[3.0]).pop().unwrap();
        assert_eq!(cut.len(), 1);
        assert_eq!(cut[0].holes.len(), 1);
        let area = cut[0].area_mm2();
        let want = 20.0 * 16.0 - std::f64::consts::PI * 16.0;
        // Chords inside the circle (simplified to 12 microns) leave a little more.
        assert!(area > want && area - want < 0.2, "area {area} mm², want {want}");
    }

    /// Two parts of one body, a field part and a mesh bar from its middle out of its side, settle
    /// glued to their fresh slice: the bar has no layers of its own, the field part's are the
    /// union. So does each edit after: the bar moved, the group parted and glued again, the field
    /// part moved.
    #[test]
    fn a_glued_group_settles_to_its_fresh_slice() {
        let plate_of = |at: [f64; 3], dx: f64, glued: bool| {
            let (mut p, mut f) = sdf_plate(at);
            let bar = cube([at[0] + dx, at[1] - 3.0, 1.0], [20.0, 6.0, 3.0]);
            p.parts.push(Part { mesh: bar.clone(), object: "bar".into(), name: "bar".into(), ..p.parts[1].clone() });
            f.push(Frame { local: Arc::new(bar), offset: [0.0; 3], field: None });
            if glued {
                for part in &mut p.parts[1..] {
                    part.body_of = Some("body".into());
                }
            }
            (p, f)
        };
        let input = |id: u64, p: &Plate, f: &[Frame]| {
            let mut i = LiveInput::ready(id, 1, (*setup()).clone(), Arc::new(p.clone()), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(f.to_vec()));
            i
        };
        let (p, f) = plate_of([150.0, 120.0, 0.0], 0.0, false);
        let apart = slice_frames(&setup(), &p, &f, &[], None).unwrap();
        let (e, rx) = engine();
        for (tag, at, dx, glued) in [(1, [150.0, 120.0, 0.0], 0.0, true), (2, [150.0, 120.0, 0.0], 2.0, true), (3, [150.0, 120.0, 0.0], 2.0, false), (4, [150.0, 120.0, 0.0], 2.0, true), (5, [151.5, 119.25, 0.0], 2.0, true)] {
            let (p, f) = plate_of(at, dx, glued);
            let fresh = slice_frames(&setup(), &p, &f, &[], None).unwrap();
            let bar_layers = fresh.toolpaths.layers.iter().filter(|l| l.objects.iter().any(|o| o.object == 2 && !o.slices.is_empty())).count();
            assert_eq!(bar_layers == 0, glued, "settle {tag}: glued, the bar has no layers of its own ({bar_layers})");
            if tag == 1 {
                assert!(fresh.gcode != apart.gcode, "glued and apart print differently");
            }
            e.settle(tag, input(tag, &p, &f));
            let r = settled(&rx, tag);
            assert!(r.gcode == fresh.gcode, "settle {tag}: the slice differs from its fresh one");
        }
    }

    /// A field part with a painted half (material slot 1 on filament 1; the part is filament
    /// 2) is sliced from its field in both filaments, the painted windows' inventory and layers
    /// stream once each with the focus first, and the engine settles to the from-scratch
    /// slice. Then brush strokes (its material states change, its shape does not: undone, then
    /// made again) re-trace no contours (the shape's changed Z range is empty), while the paint
    /// split and the plans follow the new colours: each settle is the fresh slice of its states.
    #[test]
    fn a_painted_field_part_settles_to_its_fresh_slice_through_brush_strokes() {
        use makepad_csg_sdf::{SdfMaterial, SdfNode, SdfOp};
        let body = SdfNode::Box { size: [20.0, 6.0, 16.0], radius: 0.0 };
        let half = SdfNode::Transform { at: [5.0, 0.0, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size: [10.0, 10.0, 20.0], radius: 0.0 }) };
        let painted = SdfNode::Material { slot: 1, within: None, of: Box::new(SdfNode::Combine { op: SdfOp::Intersection, blend: 0.0, of: vec![body.clone(), half] }) };
        let root = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![painted, body] };
        let shape = Arc::new(SdfShape { root, min: [-10.5, -3.5, -8.5], max: [10.5, 3.5, 8.5], materials: vec![SdfMaterial::default(), SdfMaterial::default()] });
        let plain = PartField::new(shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 3.0]]);
        let field = plain.clone().with_states(vec![0, 1]);
        assert_ne!(plain.content, field.content, "the colours are part of the content key");
        assert_eq!(plain.clone().with_states(vec![0, 0]).content, plain.content, "all-own colours are uncoloured");
        assert!(field.source().unwrap().painted());
        let (p, f0) = sdf_plate([150.0, 120.0, 0.0]);
        let frames = |field: &PartField| {
            let mut f = f0.clone();
            f[1].field = Some(field.clone());
            f
        };
        let mut thick = (*setup()).clone();
        thick.process.values.set_str("layer_height", "0.4");
        let input = |id: u64, f: &[Frame]| {
            let mut i = LiveInput::ready(id, 1, thick.clone(), Arc::new(p.clone()), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(f.to_vec()));
            i
        };
        let (fresh_painted, fresh_plain) = (slice_frames(&thick, &p, &frames(&field), &[], None).unwrap(), slice_frames(&thick, &p, &frames(&plain), &[], None).unwrap());
        assert_eq!(fresh_painted.stats.used, vec![0, 1], "both filaments print");
        let (e, rx) = engine();
        e.set_focus_z(Some(3.0));
        e.settle(1, input(1, &frames(&field)));
        let (mut regions, mut total, mut focus) = (0, 0, 0);
        let mut seen = std::collections::BTreeSet::new();
        let mut supported = false;
        let r = loop {
            match rx.recv().expect("painted slice") {
                LiveEvent::PartCut { part: 1, layers, .. } => focus = layers.partition_point(|(z, _, _)| *z < 3.0) as u32,
                LiveEvent::PartRegions { part: 1, regions: r, total: t, .. } => {
                    assert_eq!(regions, 0, "one inventory for all painted windows");
                    (regions, total) = (r, t);
                }
                LiveEvent::PartLayers { part: 1, region, layers, .. } => {
                    assert!(!supported);
                    for (index, _) in layers {
                        if seen.is_empty() { assert_eq!(index, focus, "painted focus is first"); }
                        assert!(seen.insert((region, index)), "each painted model layer is final once");
                    }
                }
                LiveEvent::PartSupports { part: 1, .. } => {
                    assert_eq!(seen.len(), (regions * total) as usize, "all regions complete before supports");
                    supported = true;
                }
                LiveEvent::Settled { tag: 1, result, .. } => break result.unwrap(),
                _ => {}
            }
        };
        assert_eq!(regions, 2);
        assert!(supported);
        assert!(r.gcode == fresh_painted.gcode, "the settled painted SDF slice differs from the fresh one");
        // The strokes: undone, then made again. Neither traces a layer.
        let traced = e.traced_count();
        for (k, (field, fresh)) in [(&plain, &fresh_plain), (&field, &fresh_painted)].into_iter().enumerate() {
            let tag = k as u64 + 2;
            e.settle(tag, input(tag, &frames(field)));
            let r = settled(&rx, tag);
            assert!(r.gcode == fresh.gcode, "settle {tag} after a brush stroke differs from the fresh slice");
        }
        assert_eq!(e.traced_count(), traced, "a brush stroke re-traced contours its shape did not change");
    }

    /// The layer in view first: an edited field part's first final model layer is the focus's,
    /// the layers below it come before those above, each is planned once and before supports
    /// and Toolpaths, and the settle is the fresh slice exactly.
    #[test]
    fn an_edited_field_part_plans_its_viewed_layer_first_and_settles_exactly() {
        use makepad_csg_sdf::{SdfNode, SdfOp};
        let mut s = (*setup()).clone();
        s.process.values.set_str("layer_height", "0.3");
        // A post of height `h` with a bore of radius `r`.
        let field = |r: f64, h: f64| {
            let root = SdfNode::Combine {
                op: SdfOp::Difference,
                blend: 0.0,
                of: vec![SdfNode::Box { size: [12.0, h, 12.0], radius: 0.0 }, SdfNode::Cylinder { radius: r, top_radius: r, height: h + 4.0 }],
            };
            let shape = Arc::new(SdfShape { root, min: [-6.5, -0.5 * h - 0.5, -6.5], max: [6.5, 0.5 * h + 0.5, 6.5], materials: Vec::new() });
            PartField::new(shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 0.5 * h]])
        };
        let (p, f0) = sdf_plate([150.0, 120.0, 0.0]);
        let frames = |r: f64, h: f64| {
            let mut f = f0.clone();
            f[1].field = Some(field(r, h));
            f
        };
        let input = |id: u64, f: &[Frame]| {
            let mut i = LiveInput::ready(id, 1, s.clone(), Arc::new(p.clone()), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(f.to_vec()));
            i
        };
        let (e, rx) = engine();
        // The part before the edit is short; the edit makes it taller and bores it wider. The
        // focus is mid-height.
        e.settle(1, input(1, &frames(2.0, 6.0)));
        settled(&rx, 1);
        let z = 12.0;
        e.set_focus_z(Some(z));
        let f = frames(3.0, 24.0);
        let fresh = slice_frames(&s, &p, &f, &[], None).unwrap();
        e.settle(2, input(2, &f));
        let (mut cut, mut first, mut order) = (None, None, Vec::new());
        let mut planned = std::collections::BTreeSet::new();
        let (mut supports, mut toolpaths) = (false, false);
        let r = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::PartCut { part: 1, layers, .. } => cut = Some(layers),
                LiveEvent::PartLayers { part: 1, layers, .. } => {
                    assert!(!supports, "all model layers precede supports");
                    for (i, _) in &layers {
                        assert!(planned.insert(*i), "layer {i} is planned only once");
                        order.push(*i as usize);
                    }
                    if first.is_none() { first = layers.first().map(|l| l.0 as usize); }
                }
                LiveEvent::PartSupports { part: 1, .. } => {
                    let total = cut.as_ref().expect("contours before supports").len() as u32;
                    assert!(planned.iter().copied().eq(0..total), "every model layer is final before supports");
                    assert!(!supports, "one support pass");
                    supports = true;
                }
                LiveEvent::Toolpaths { .. } => { assert!(supports); toolpaths = true; }
                LiveEvent::Settled { tag: 2, result, .. } => break result.expect("sliced"),
                _ => {}
            }
        };
        assert!(r.gcode == fresh.gcode, "the focused edit's settle differs from the fresh slice");
        let cut = cut.expect("cuts");
        let focus = cut.partition_point(|(pz, _, _)| *pz < z);
        assert_eq!(first, Some(focus), "the first final model layer is the focus");
        let mut above = false;
        for &i in &order {
            if i > focus { above = true; }
            if i < focus { assert!(!above, "every layer below the focus comes before any above it"); }
        }
        assert!(planned.iter().copied().eq(0..cut.len() as u32), "every model layer is planned");
        assert!(toolpaths, "the assembled plan reaches Toolpaths");
    }

    /// A drag sends inputs faster than they are built: an input a newer one supersedes while it
    /// is built still draws its edited part's viewed layer, and the newest input slices whole.
    #[test]
    fn an_input_superseded_while_it_is_built_draws_its_viewed_layer() {
        use makepad_csg_sdf::SdfNode;
        let s = (*setup()).clone();
        let field = |h: f64| {
            let shape = Arc::new(SdfShape { root: SdfNode::Box { size: [8.0, h, 8.0], radius: 0.0 }, min: [-4.5, -0.5 * h - 0.5, -4.5], max: [4.5, 0.5 * h + 0.5, 4.5], materials: Vec::new() });
            PartField::new(shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 0.5 * h]])
        };
        let (p, f0) = sdf_plate([150.0, 120.0, 0.0]);
        let input = move |id: u64, h: f64| {
            let mut f = f0.clone();
            f[1].field = Some(field(h));
            let mut i = LiveInput::ready(id, 1, s.clone(), Arc::new(p.clone()), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(f));
            i
        };
        let (e, rx) = engine();
        e.settle(1, input(1, 4.0));
        settled(&rx, 1);
        e.set_focus_z(Some(1.0));
        let shared = Arc::downgrade(&e.shared);
        let newer = input(3, 6.0);
        let built = input(2, 5.0);
        let parts = built.plate.parts.len();
        let rev = e.set_input_with(2, 1, parts, Arc::new(move || {
            // the next drag step's input comes while this one is built
            if let Some(shared) = shared.upgrade() { LiveEngine { shared }.set_input(newer.clone()); }
            built.clone()
        }));
        let mut viewed = false;
        loop {
            match rx.recv().expect("the newest input slices whole") {
                LiveEvent::Viewed { revision, part: 1, .. } if revision == rev => viewed = true,
                LiveEvent::Toolpaths { revision, .. } if revision == rev + 1 => break,
                LiveEvent::Toolpaths { revision, .. } => panic!("revision {revision} sliced whole, not the newest {}", rev + 1),
                _ => {}
            }
        }
        assert!(viewed, "the superseded input drew its edited part's viewed layer");
        idle(&e);
    }

    /// A drag's last step and the commit after it are one content under two inputs: the commit's
    /// viewed layer is the one the superseded step planned, said with no work, and the commit
    /// slices whole.
    #[test]
    fn an_input_of_the_content_a_superseded_one_planned_says_its_viewed_layer() {
        use makepad_csg_sdf::SdfNode;
        let s = (*setup()).clone();
        let (p, mut f) = sdf_plate([150.0, 120.0, 0.0]);
        let shape = Arc::new(SdfShape { root: SdfNode::Box { size: [8.0, 5.0, 8.0], radius: 0.0 }, min: [-4.5, -3.0, -4.5], max: [4.5, 3.0, 4.5], materials: Vec::new() });
        f[1].field = Some(PartField::new(shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 2.5]]));
        let input = move |id: u64| {
            let mut i = LiveInput::ready(id, 1, s.clone(), Arc::new(p.clone()), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(f.clone()));
            i
        };
        let (e, rx) = engine();
        e.set_focus_z(Some(1.0));
        let shared = Arc::downgrade(&e.shared);
        let commit = input(3);
        let step = input(2);
        let parts = step.plate.parts.len();
        let rev = e.set_input_with(2, 1, parts, Arc::new(move || {
            // the commit comes while the step is built
            if let Some(shared) = shared.upgrade() { LiveEngine { shared }.set_input(commit.clone()); }
            step.clone()
        }));
        let (mut step_viewed, mut commit_viewed) = (false, None);
        loop {
            match rx.recv().expect("the commit slices whole") {
                LiveEvent::Viewed { revision, part: 1, .. } if revision == rev => step_viewed = true,
                LiveEvent::Viewed { revision, part: 1, plan_ms, .. } if revision == rev + 1 => commit_viewed = Some(plan_ms),
                LiveEvent::Toolpaths { revision, .. } if revision == rev + 1 => break,
                LiveEvent::Toolpaths { revision, .. } => panic!("revision {revision} sliced whole, not the commit's {}", rev + 1),
                _ => {}
            }
        }
        assert!(step_viewed, "the superseded step drew its viewed layer");
        assert_eq!(commit_viewed, Some(0.0), "the commit's viewed layer is the step's, said with no work");
        idle(&e);
    }

    /// Two dirty parts take turns. Moving the slider between events changes the next bands,
    /// keeps the settle, and never plans a layer twice, even across the old focus. Past the
    /// focus a part's window takes every band whose contours are in, so a turn can be several layers.
    #[test]
    fn moving_the_focus_reorders_remaining_layers_without_restarting() {
        let mut s = (*setup()).clone();
        s.process.values.set_str("layer_height", "0.2");
        for (key, value) in [("enable_support", "1"), ("support_type", "normal(auto)"), ("support_style", "snug"), ("support_threshold_angle", "30"), ("support_top_z_distance", "0.2"), ("support_bottom_z_distance", "0.2"), ("independent_support_layer_height", "1")] {
            s.process.values.set_str(key, value);
        }
        let p0 = plate();
        let mut p1 = p0.clone();
        for q in &mut p1.parts[0].mesh.positions {
            if q[0] > 100.0 { q[0] += 1.0; }
        }
        p1.parts[1].mesh = cube([200.0, 100.0, 0.0], [16.0, 12.0, 6.0]);
        let fresh = crate::plate::slice_plate(&s, &p1).unwrap();
        let input = |id, p: &Plate| LiveInput::ready(id, 1, s.clone(), Arc::new(p.clone()), Arc::new(Vec::new()));
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let (resume, resumed) = mpsc::channel();
        let resumed = Mutex::new(resumed);
        let first = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pause_first = first.clone();
        let e = LiveEngine::new(Arc::new(|_, job| { std::thread::spawn(job); }), Arc::new(move |event| {
            let pause = matches!(&event, LiveEvent::PartLayers { part: 1, .. }) && pause_first.swap(false, Ordering::Relaxed);
            let _ = tx.lock().unwrap().send(event);
            // A handshake, not a delay: the slider moves before this band's planner returns.
            if pause { resumed.lock().unwrap().recv().expect("slider moved"); }
        }), checkpoints());
        e.settle(1, input(1, &p0));
        settled(&rx, 1);
        idle(&e);
        first.store(true, Ordering::Relaxed);
        let before = e.part_work();
        let (z0, z1) = (2.0, 4.0);
        e.set_focus_z(Some(z0));
        e.settle(2, input(2, &p1));
        let revision = e.shared.state.lock().unwrap().revision;
        let mut cuts: Vec<Option<Arc<CutLayers>>> = vec![None; 2];
        let mut order = vec![Vec::new(); 2];
        let mut seen = vec![std::collections::BTreeSet::new(); 2];
        let mut emitted = [0u64; 2];
        let mut turns = Vec::new();
        let mut supports = [false; 2];
        let mut support_only = false;
        let mut moved = false;
        let r = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::PartCut { part, layers, .. } => cuts[part] = Some(layers),
                LiveEvent::PartLayers { part, layers, .. } => {
                    if part == 1 && !moved {
                        e.set_focus_z(Some(z1));
                        moved = true;
                        resume.send(()).unwrap();
                        assert_eq!(e.shared.state.lock().unwrap().revision, revision, "a focus move is not a new input");
                    }
                    emitted[part] += layers.len() as u64;
                    assert!(!supports.iter().any(|s| *s), "supports follow every dirty part's model layers");
                    for (i, _) in layers {
                        assert!(seen[part].insert(i), "part {part} layer {i} is planned only once");
                        order[part].push(i as usize);
                        turns.push(part);
                    }
                }
                LiveEvent::PartSupports { part, layers, .. } => {
                    for i in 0..2 {
                        let total = cuts[i].as_ref().expect("part cut").len() as u32;
                        assert!(seen[i].iter().copied().eq(0..total), "supports wait for every dirty part's model layers");
                    }
                    assert!(!supports[part], "one support pass per part");
                    supports[part] = true;
                    if part == 0 {
                        assert!(layers.iter().any(|(_, records)| !records.is_empty()), "the cap needs support");
                        let total = cuts[part].as_ref().unwrap().len() as u32;
                        support_only = layers.iter().any(|(i, records)| *i >= total && !records.is_empty());
                    }
                }
                LiveEvent::Settled { tag, revision: got, result, .. } => {
                    assert_eq!((tag, got), (2, revision), "the original settle completes");
                    break result.expect("a focus move never supersedes the settle");
                }
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        };
        idle(&e);
        assert!(moved);
        assert_eq!(supports, [true; 2]);
        assert!(support_only, "normal supports have their own indices after the model");
        assert_eq!(&turns[..4], &[0, 1, 0, 1], "every part's focus precedes any part's next band, before and after the move");
        let rest = &turns[4..];
        assert!(matches!((rest.iter().position(|&p| p == 1), rest.iter().rposition(|&p| p == 0)), (Some(a), Some(b)) if a < b), "part 1 plans before part 0 is done: the parts take turns");
        let work = e.part_work();
        for part in 0..2 {
            let cut = cuts[part].as_ref().expect("part cut");
            let focus0 = cut.partition_point(|(z, _, _)| *z < z0);
            let focus1 = cut.partition_point(|(z, _, _)| *z < z1);
            assert_eq!(&order[part][..2], &[focus0, focus1], "the old focus, then the new, on part {part}");
            assert!(order[part][2] < focus1, "then down from the new focus on part {part}");
            let mut above = false;
            for &i in &order[part][2..] {
                if i > focus1 { above = true; }
                if i < focus1 { assert!(!above, "part {part} finishes below the new focus before going above it"); }
            }
            assert!(seen[part].iter().copied().eq(0..cut.len() as u32), "every layer of part {part} is planned");
            assert_eq!(work[part].0 - before[part].0, cut.len() as u64, "each dirty layer is traced once despite the reorder");
            assert_eq!(work[part].1 - before[part].1, emitted[part], "every final model emit counts once");
            assert_eq!(emitted[part], cut.len() as u64, "no second whole-part plan");
        }
        assert!(r.gcode == fresh.gcode, "the reordered settle differs from the fresh slice");
    }

    /// Editing one of two parts leaves the other's tracing and planning counters unchanged.
    #[test]
    fn an_edit_does_no_work_on_an_untouched_part() {
        let p0 = plate();
        let mut p1 = p0.clone();
        p1.parts[1].mesh = cube([200.0, 100.0, 0.0], [16.0, 12.0, 6.0]);
        let (e, rx) = engine();
        e.settle(1, input(&p0));
        settled(&rx, 1);
        let before = e.part_work();
        assert_eq!(before.len(), 2);
        assert!(before.iter().all(|&(traced, planned)| traced > 0 && planned > 0), "both parts did work on the first settle");
        e.set_focus_z(Some(3.0));
        let mut next = input(&p1);
        next.plate_id = 2;
        e.settle(2, next);
        let r = settled(&rx, 2);
        let after = e.part_work();
        assert_eq!(after.len(), 2);
        assert_eq!(after[0], before[0], "the untouched part traces and plans no layers");
        assert!(after[1].0 > before[1].0 && after[1].1 > before[1].1, "the edited part traces and plans layers");
        assert_eq!(after.iter().map(|w| w.0).sum::<u64>(), e.traced_count());
        assert!(r.gcode == crate::plate::slice_plate(&setup(), &p1).unwrap().gcode, "the edit settles to its fresh slice");
    }

    /// The part's layers kept and planned so far, for [`kept_some_since`].
    fn kept_and_planned(e: &LiveEngine, part: usize) -> (u64, u64) {
        (e.part_kept()[part].0, e.part_work()[part].1)
    }

    /// That the edit since `before` ([`kept_and_planned`]) kept layers of the part and planned
    /// fewer than `all`, what a fresh engine plans of it: a warm slice equal to a fresh one tests
    /// nothing when the warm engine kept none. (An edit that planned nothing kept them all.)
    #[track_caller]
    fn kept_some_since(e: &LiveEngine, part: usize, before: (u64, u64), all: u64, what: &str) {
        let ((kept, none), planned) = (e.part_kept().swap_remove(part), e.part_work()[part].1);
        let again = planned - before.1;
        assert!(again < all && (again == 0 || (kept > before.0 && none.is_empty())), "{what}: planned {again} of {all} layers, kept {} ({none})", kept - before.0);
    }

    /// An edit of an SDF part re-traces only the layers it reaches, and the
    /// settled slice is still the from-scratch slice of the edited part.
    #[test]
    fn sdf_edits_retrace_only_the_layers_they_reach() {
        use makepad_csg_sdf::{SdfNode, SdfOp};
        // A tall post (model Y up) with a blob near its top that moves.
        let shape = |y: f64| {
            let root = SdfNode::Combine {
                op: SdfOp::Union,
                blend: 0.5,
                of: vec![
                    SdfNode::Box { size: [8.0, 24.0, 8.0], radius: 0.0 },
                    SdfNode::Transform { at: [4.0, y, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Sphere { radius: 1.0 }) },
                ],
            };
            Arc::new(SdfShape { root, min: [-6.5, -12.5, -4.5], max: [6.5, 12.5, 4.5], materials: Vec::new() })
        };
        let place = [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 12.0]];
        let local = Arc::new(cube([-4.0, -4.0, 0.0], [8.0, 8.0, 24.0]));
        let plate_of = |y: f64| {
            let mut m = (*local).clone();
            m.translate([150.0, 120.0, 0.0]);
            let p = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: "post".into(), name: "post".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh: m, field_bounds: None, field_floor: None, ..Default::default() }] };
            let f = vec![Frame { local: local.clone(), offset: [150.0, 120.0, 0.0], field: Some(PartField::new(shape(y), place)) }];
            (p, f)
        };
        let (p0, f0) = plate_of(8.0);
        let (p1, f1) = plate_of(8.5);
        let mut s = (*setup()).clone();
        s.process.values.set_str("layer_height", "0.4");
        let fresh1 = slice_frames(&s, &p1, &f1, &[], None).unwrap();
        let (e, rx) = engine();
        let input = |id: u64, p: &Plate, f: &[Frame]| {
            let mut i = LiveInput::ready(id, 1, s.clone(), Arc::new(p.clone()), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(f.to_vec()));
            i
        };
        e.settle(1, input(1, &p0, &f0));
        let _ = settled(&rx, 1);
        let all = e.traced_count();
        let before = kept_and_planned(&e, 0);
        e.settle(2, input(2, &p1, &f1));
        let r1 = settled(&rx, 2);
        let again = e.traced_count() - all;
        assert!(again > 0 && again * 2 < all, "{again} of {all} layers traced again");
        kept_some_since(&e, 0, before, before.1, "a blob moved");
        assert!(r1.gcode == fresh1.gcode, "the re-traced SDF slice differs from the fresh one");
    }

    /// An edit keeps the layers it doesn't reach and settles to a fresh engine's slice, with a
    /// painted field part's colours all on its one tool and with each on its own: its blob moved,
    /// then made another shape, then its paint moved, one after another in one engine.
    #[test]
    fn edits_keep_layers_and_settle_fresh_on_one_tool_and_on_a_tool_per_colour() {
        use makepad_csg_sdf::{SdfNode, SdfOp};
        // A 24 mm post (model Y up) with a blob near its top and a ball of paint low on its wall.
        let shape = |blob: SdfNode, y: f64| {
            let of = vec![SdfNode::Box { size: [8.0, 24.0, 8.0], radius: 0.0 }, SdfNode::Transform { at: [4.0, y, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(blob) }];
            Arc::new(SdfShape { root: SdfNode::Combine { op: SdfOp::Union, blend: 0.5, of }, min: [-6.5, -12.5, -4.5], max: [6.5, 12.5, 4.5], materials: Vec::new() })
        };
        let place = [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 12.0]];
        let local = Arc::new(cube([-4.0, -4.0, 0.0], [8.0, 8.0, 24.0]));
        let mut mesh = (*local).clone();
        mesh.translate([150.0, 120.0, 0.0]);
        let mut s = (*setup()).clone();
        s.process.values.set_str("layer_height", "0.4");
        let (ball, block) = (|| SdfNode::Sphere { radius: 1.0 }, || SdfNode::Box { size: [2.0; 3], radius: 0.0 });
        let edits = [("its blob moved", shape(ball(), 8.5), 6.0), ("its blob another shape", shape(block(), 8.5), 6.0), ("its paint moved", shape(block(), 8.5), 7.0)];
        for (tools, tool_of_colour, printed) in [("one tool", 1, vec![0]), ("a tool per colour", 2, vec![0, 1])] {
            let input = |id: u64, shape: &Arc<SdfShape>, z: f64| {
                let mut paint = ball_paint([154.0, 120.0, z]);
                paint.colors[1].filament = Some(tool_of_colour);
                let part = Part { body_of: None, print: Vec::new(), print_mesh: None, paint: Some(paint), object: "post".into(), name: "post".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh: mesh.clone(), field_bounds: None, field_floor: None, ..Default::default() };
                let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part] };
                let mut i = LiveInput::ready(id, 1, s.clone(), Arc::new(plate), Arc::new(Vec::new()));
                i.frames = Some(Arc::new(vec![Frame { local: local.clone(), offset: [150.0, 120.0, 0.0], field: Some(PartField::new(shape.clone(), place)) }]));
                i
            };
            let (e, rx) = engine();
            e.settle(1, input(1, &shape(ball(), 8.0), 6.0));
            let _ = settled(&rx, 1);
            for (k, (edit, shape, z)) in edits.iter().enumerate() {
                let what = format!("{tools}, {edit}");
                let (fresh, all) = {
                    let (e, rx) = engine();
                    e.settle(1, input(1, shape, *z));
                    (settled(&rx, 1), e.part_work()[0].1)
                };
                assert_eq!(fresh.stats.used, printed, "{what}: the tools it prints in");
                let before = kept_and_planned(&e, 0);
                let tag = k as u64 + 2;
                e.settle(tag, input(tag, shape, *z));
                let r = settled(&rx, tag);
                kept_some_since(&e, 0, before, all, &what);
                assert!(r.gcode == fresh.gcode, "{what}: the settled slice differs from a fresh engine's");
            }
        }
    }

    /// The engine offers an edited field the last field before anything reads its slopes, so the
    /// cells the edit leaves are the last field's own, not proven again.
    #[test]
    fn an_edited_field_in_the_engine_takes_the_cells_its_edit_leaves() {
        use makepad_csg_sdf::{SdfNode, SdfOp};
        // A tall post (model Y up) with a blob near its top that moves.
        let field = |y: f64| {
            let of = vec![SdfNode::Box { size: [8.0, 24.0, 8.0], radius: 0.0 }, SdfNode::Transform { at: [4.0, y, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Sphere { radius: 1.0 }) }];
            let shape = Arc::new(SdfShape { root: SdfNode::Combine { op: SdfOp::Union, blend: 0.5, of }, min: [-6.5, -12.5, -4.5], max: [6.5, 12.5, 4.5], materials: Vec::new() });
            PartField::new(shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 12.0]])
        };
        let local = Arc::new(cube([-4.0, -4.0, 0.0], [8.0, 8.0, 24.0]));
        let mut mesh = (*local).clone();
        mesh.translate([150.0, 120.0, 0.0]);
        let part = Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: "post".into(), name: "post".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh, field_bounds: None, field_floor: None, ..Default::default() };
        let plate = Arc::new(Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part] });
        let mut s = (*setup()).clone();
        s.process.values.set_str("layer_height", "0.4");
        let input = |id: u64, f: &PartField| {
            let mut i = LiveInput::ready(id, 1, s.clone(), plate.clone(), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(vec![Frame { local: local.clone(), offset: [150.0, 120.0, 0.0], field: Some(f.clone()) }]));
            i
        };
        let (a, b) = (field(8.0), field(8.5));
        let (e, rx) = engine();
        e.settle(1, input(1, &a));
        let _ = settled(&rx, 1);
        e.settle(2, input(2, &b));
        let _ = settled(&rx, 2);
        let cells = |f: &PartField| f.bands().cells.iter().flat_map(|band| band.of.iter().flatten().cloned()).collect::<Vec<_>>();
        let (was, now) = (cells(&a), cells(&b));
        assert!(!was.is_empty() && was.len() == now.len(), "{} and {} cells", was.len(), now.len());
        let taken = was.iter().zip(&now).filter(|(w, n)| Arc::ptr_eq(w, n)).count();
        assert!(taken > 0 && taken < now.len(), "the edit took {taken} of {} cells from the last field", now.len());
    }

    /// Paint by position for [`a_paint_edit_plans_only_the_layers_it_reaches`]: a ball of tool 2
    /// (printer mm) whose samples cap their distance at [`BALL_CAP`], so a moved ball changes
    /// the paint only within that of its old and new places.
    struct Ball {
        at: [f64; 3],
        r: f64,
    }

    const BALL_CAP: f64 = 1.0;

    impl fab_plate::PaintChange for Ball {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn changed(&self, old: &dyn fab_plate::PaintChange) -> Option<Vec<(f64, f64)>> {
            let old = old.as_any().downcast_ref::<Ball>()?;
            Some([self, old].iter().map(|b| (b.at[2] - b.r - BALL_CAP, b.at[2] + b.r + BALL_CAP)).collect())
        }
    }

    fn ball_paint(at: [f64; 3]) -> fab_plate::SpatialPaint {
        let ball = Arc::new(Ball { at, r: 2.0 });
        let b = ball.clone();
        let colour = |filament| fab_plate::paint::PaintColor { rgb: [1.0; 3], color: None, filament, nozzle: None };
        fab_plate::SpatialPaint {
            sample_at: Arc::new(move |q| {
                let d = ((q[0] - b.at[0]).powi(2) + (q[1] - b.at[1]).powi(2) + (q[2] - b.at[2]).powi(2)).sqrt() - b.r;
                fab_plate::PaintSample { slot: u8::from(d < 0.0), dist: d.abs().min(BALL_CAP), mix: None }
            }),
            support_block: None,
            support_enforce: None,
            no_trees: None,
            settings: Vec::new(),
            colors: vec![colour(None), colour(Some(2))],
            key: at.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, v| (h ^ v.to_bits()).wrapping_mul(0x100_0000_01b3)) | 1,
            no_trees_key: 0,
            regions_key: 0,
            source: ball,
        }
    }

    /// A support block on a part reaches the live slice through the part's paint: its report says
    /// what it took, a moved block plans again, and a block gone is back to the plain plan.
    #[test]
    fn a_support_block_reaches_the_live_slice_and_its_report() {
        let told = |paint: Option<[[f64; 3]; 2]>, rev: u64, e: &LiveEngine, rx: &mpsc::Receiver<LiveEvent>| -> Vec<fab_printability::Issue> {
            let mut plate = plate();
            plate.parts.truncate(1);
            plate.parts[0].paint = paint.map(|b| {
                let mut p = ball_paint([0.0; 3]);
                let (lo, hi) = (b[0], b[1]);
                let at = Arc::new(move |q: [f64; 3]| (0..3).map(|k| (q[k] - (lo[k] + hi[k]) / 2.0).abs() - (hi[k] - lo[k]) / 2.0).fold(f64::MIN, f64::max));
                p.support_block = Some(fab_plate::SupportRegion { at, bounds: Some((lo, hi)) });
                p.key = lo.iter().chain(&hi).fold(0xcbf2_9ce4_8422_2325u64, |h, v| (h ^ v.to_bits()).wrapping_mul(0x100_0000_01b3)) | 1;
                p
            });
            let mut job = (*setup()).clone();
            job.process.values.set_str("enable_support", "1");
            job.process.values.set_str("support_type", "tree(auto)");
            e.settle(rev, LiveInput::ready(1, 1, job, Arc::new(plate), Arc::new(Vec::new())));
            loop {
                if let LiveEvent::Issues { revision, report, done, total } = rx.recv().expect("the checks finish") {
                    if done == total && revision == rev {
                        return report.issues.iter().filter(|i| i.kind == "support_blocked").cloned().inspect(|i| assert_eq!(i.severity, fab_printability::Severity::Warning)).collect();
                    }
                }
            }
        };
        let blocked = |paint, rev, e: &LiveEngine, rx: &mpsc::Receiver<LiveEvent>| -> f64 { told(paint, rev, e, rx).iter().map(|i| i.area_mm2).sum() };
        let (e, rx) = engine();
        let issues = told(Some([[100.0, 85.0, 7.5], [115.0, 115.0, 8.5]]), 1, &e, &rx);
        let half: f64 = issues.iter().map(|i| i.area_mm2).sum();
        assert!((230.0..270.0).contains(&half), "the half of the cap on x > 100 is told: {half} mm2");
        // Its footprint is that half where it stands on the plate (the cap is 88..112 in x and y),
        // and holds the spot it names.
        for i in &issues {
            let (lo, hi) = (i.footprint.min, i.footprint.max);
            let near = |a: [f64; 2], b: [f64; 2]| (0..2).all(|k| (a[k] - b[k]).abs() < 1.0);
            assert!(near(lo, [100.0, 88.0]) && near(hi, [112.0, 112.0]), "the blocked half's footprint: {lo:?} {hi:?}");
            assert!((0..2).all(|k| lo[k] <= i.at_mm[k] && i.at_mm[k] <= hi[k]), "{:?} in {lo:?} {hi:?}", i.at_mm);
        }
        let quarter = blocked(Some([[100.0, 100.0, 7.5], [115.0, 115.0, 8.5]]), 2, &e, &rx);
        assert!(quarter > 0.0 && quarter < 0.6 * half, "a moved block plans again: {quarter} against {half}");
        assert_eq!(blocked(None, 3, &e, &rx), 0.0, "without it nothing is told");
    }

    /// The bodies of one object that can't print as one, the cap painted by position (a ball of
    /// filament 2 in it) and the box by regions (its facets in filament 2), print apart: each
    /// slices on its own, and the slice's report says which object and why.
    #[test]
    fn bodies_painted_two_ways_print_apart_and_the_report_says_so() {
        let mut plate = plate();
        plate.parts[0].paint = Some(ball_paint([100.0, 100.0, 9.0]));
        for p in &mut plate.parts {
            p.body_of = Some("doll/doll".into());
        }
        let facets = vec![Vec::new(), vec!["8".to_string(); plate.parts[1].mesh.triangles.len()]];
        let (e, rx) = engine();
        e.settle(1, LiveInput::ready(1, 1, (*setup()).clone(), Arc::new(plate), Arc::new(facets)));
        let mut layers = std::collections::BTreeMap::new();
        let issues = loop {
            match rx.recv().expect("the engine sends until the checks are done") {
                LiveEvent::PartRegions { part, total, .. } => {
                    layers.insert(part, total);
                }
                LiveEvent::Issues { report, done, total, .. } if done == total => break report.issues.clone(),
                _ => {}
            }
        };
        let apart: Vec<&fab_printability::Issue> = issues.iter().filter(|i| i.kind == "bodies_apart").collect();
        assert_eq!(apart.len(), 1, "{issues:?}");
        assert!(apart[0].message.starts_with("object doll/doll prints its bodies apart: cap paints by position and box by regions"), "{}", apart[0].message);
        assert_eq!((apart[0].object.as_deref(), apart[0].severity), (Some("cap"), fab_printability::Severity::Warning));
        assert!(layers.len() == 2 && layers.values().all(|&n| n > 0), "each body slices on its own: layers by part {layers:?}");
    }

    /// An edit of another part lands while the plate pass sends a painted layer, with one of
    /// that layer's regions sent. The pass keeps what it sent for the next revision of this
    /// content, which plans the layer again, whole, and settles to a fresh engine's slice.
    /// Returns how many times each of the layer's regions was sent.
    fn cut_between_a_layers_regions(painted: Part, frame: Frame) -> Vec<usize> {
        use std::collections::BTreeMap;
        use std::sync::atomic::{AtomicBool, AtomicU32};
        use std::sync::Barrier;
        let mut s = (*setup()).clone();
        s.process.values.set_str("layer_height", "0.4");
        // A one-layer pad beside it is what the edit widens.
        let input = |id: u64, pad_width: f64| {
            let pad_local = cube([0.0, 0.0, 0.0], [pad_width, 8.0, 0.4]);
            let mut mesh = pad_local.clone();
            mesh.translate([100.0, 100.0, 0.0]);
            let pad = Part { paint: None, object: "pad".into(), name: "pad".into(), mesh, ..painted.clone() };
            let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![painted.clone(), pad] };
            let mut i = LiveInput::ready(id, 1, s.clone(), Arc::new(plate), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(vec![frame.clone(), Frame { local: Arc::new(pad_local), offset: [100.0, 100.0, 0.0], field: None }]));
            i
        };
        let fresh = {
            let (e, rx) = engine();
            e.settle(2, input(2, 9.0));
            settled(&rx, 2)
        };
        // The first layer of the part's plate pass waits for the edit, one region sent.
        let (sent, go) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
        let (cut, layer) = (AtomicBool::new(false), Arc::new(AtomicU32::new(u32::MAX)));
        let (e, rx) = engine_seeing({
            let (sent, go, layer) = (sent.clone(), go.clone(), layer.clone());
            move |ev| {
                let LiveEvent::PartLayers { pass: Pass::Window, part: 0, layers, .. } = ev else { return };
                if !cut.swap(true, Ordering::AcqRel) {
                    layer.store(layers[0].0, Ordering::Release);
                    sent.wait();
                    go.wait();
                }
            }
        });
        e.set_input(input(1, 8.0));
        sent.wait();
        e.settle(2, input(2, 9.0));
        go.wait();
        let layer = layer.load(Ordering::Acquire);
        let mut sends: BTreeMap<u32, usize> = BTreeMap::new();
        let r = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::Failed { error, .. } => panic!("the live slice failed: {error}"),
                LiveEvent::PartLayers { part: 0, region, layers, .. } if layers.iter().any(|l| l.0 == layer) => *sends.entry(region).or_default() += 1,
                LiveEvent::Settled { tag: 2, result, .. } => break result.expect("sliced"),
                _ => {}
            }
        };
        assert!(r.gcode == fresh.gcode, "the settled slice differs from a fresh engine's");
        sends.into_values().collect()
    }

    /// Paint by position: one task makes all of a layer's regions, so the one sent before the
    /// edit is sent again with the other, both of one plan.
    #[test]
    fn an_edit_between_a_painted_layers_regions_settles_to_its_fresh_slice() {
        use makepad_csg_sdf::SdfNode;
        let shape = Arc::new(SdfShape { root: SdfNode::Box { size: [8.0, 12.0, 8.0], radius: 0.0 }, min: [-4.5, -6.5, -4.5], max: [4.5, 6.5, 4.5], materials: Vec::new() });
        let local = Arc::new(cube([-4.0, -4.0, 0.0], [8.0, 8.0, 12.0]));
        let mut mesh = (*local).clone();
        mesh.translate([150.0, 120.0, 0.0]);
        let part = Part { body_of: None, print: Vec::new(), print_mesh: None, paint: Some(ball_paint([154.0, 120.0, 6.0])), object: "post".into(), name: "post".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh, field_bounds: None, field_floor: None, ..Default::default() };
        let field = PartField::new(shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 6.0]]);
        assert_eq!(cut_between_a_layers_regions(part, Frame { local, offset: [150.0, 120.0, 0.0], field: Some(field) }), [2, 1]);
    }

    /// Region paint: a task per region, so a pass stops between them wherever it stops.
    #[test]
    fn an_edit_between_a_region_painted_layers_streams_settles_to_its_fresh_slice() {
        use makepad_csg_sdf::{SdfMaterial, SdfNode, SdfOp};
        let body = SdfNode::Box { size: [20.0, 6.0, 16.0], radius: 0.0 };
        let half = SdfNode::Transform { at: [5.0, 0.0, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size: [10.0, 10.0, 20.0], radius: 0.0 }) };
        let painted = SdfNode::Material { slot: 1, within: None, of: Box::new(SdfNode::Combine { op: SdfOp::Intersection, blend: 0.0, of: vec![body.clone(), half] }) };
        let root = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![painted, body] };
        let shape = Arc::new(SdfShape { root, min: [-10.5, -3.5, -8.5], max: [10.5, 3.5, 8.5], materials: vec![SdfMaterial::default(), SdfMaterial::default()] });
        let field = PartField::new(shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 3.0]]).with_states(vec![0, 1]);
        let local = Arc::new(cube([-10.0, -8.0, 0.0], [20.0, 16.0, 6.0]));
        let mut mesh = (*local).clone();
        mesh.translate([150.0, 120.0, 0.0]);
        let part = Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: "block".into(), name: "block".into(), filament: 2, color: [1.0; 4], nozzle: None, mesh, field_bounds: None, field_floor: None, ..Default::default() };
        assert_eq!(cut_between_a_layers_regions(part, Frame { local, offset: [150.0, 120.0, 0.0], field: Some(field) }).len(), 2, "both regions of the layer sent");
    }

    /// A paint edit on a field part painted by position: its contours are the last cut's, it
    /// plans again only the layers whose context the changed paint reaches (each in both
    /// tools), and it settles to a fresh engine's slice.
    #[test]
    fn a_paint_edit_plans_only_the_layers_it_reaches() {
        use makepad_csg_sdf::SdfNode;
        // A 48 mm post (model Y up) with a ball of tool 2 on its +X wall, moved up 1 mm.
        let shape = Arc::new(SdfShape { root: SdfNode::Box { size: [8.0, 48.0, 8.0], radius: 0.0 }, min: [-4.5, -24.5, -4.5], max: [4.5, 24.5, 4.5], materials: Vec::new() });
        let place = [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 24.0]];
        let local = Arc::new(cube([-4.0, -4.0, 0.0], [8.0, 8.0, 48.0]));
        let mut mesh = (*local).clone();
        mesh.translate([150.0, 120.0, 0.0]);
        let plate_of = |z: f64| Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![Part { body_of: None, print: Vec::new(), print_mesh: None, paint: Some(ball_paint([154.0, 120.0, z])), object: "post".into(), name: "post".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh: mesh.clone(), field_bounds: None, field_floor: None, ..Default::default() }] };
        let frames = vec![Frame { local: local.clone(), offset: [150.0, 120.0, 0.0], field: Some(PartField::new(shape, place)) }];
        let (p0, p1) = (plate_of(20.0), plate_of(21.0));
        let mut s = (*setup()).clone();
        s.process.values.set_str("layer_height", "0.4");
        let input = |id: u64, p: &Plate| {
            let mut i = LiveInput::ready(id, 1, s.clone(), Arc::new(p.clone()), Arc::new(Vec::new()));
            i.frames = Some(Arc::new(frames.clone()));
            i
        };
        // the oracle: a fresh engine's slice of the edited plate (it and the batch slice
        // differ by a last E digit at a split point here, edit or none)
        let fresh = {
            let (e, rx) = engine();
            e.settle(1, input(1, &p1));
            settled(&rx, 1)
        };
        assert_eq!(slice_frames(&s, &p1, &frames, &[], None).unwrap().stats.used, vec![0, 1], "both tools print");
        let (e, rx) = engine();
        e.settle(1, input(1, &p0));
        let _ = settled(&rx, 1);
        let (traced, planned) = (e.traced_count(), e.part_work()[0].1);
        let before = kept_and_planned(&e, 0);
        e.settle(2, input(2, &p1));
        let r = settled(&rx, 2);
        let again = e.part_work()[0].1 - planned;
        assert_eq!(e.traced_count(), traced, "a paint edit traces no contours");
        assert!(again > 0 && again * 2 < planned, "{again} of {planned} model layers planned again");
        kept_some_since(&e, 0, before, planned, "a ball of paint moved");
        assert!(r.gcode == fresh.gcode, "the settled slice after a paint edit differs from a fresh engine's");
    }

    /// A turned vase mesh: r(z) = 12 + 3 sin(z / 5) mm over z 0..`height` (48 segments, rings
    /// every 0.5 mm, both ends capped).
    pub(super) fn turned_vase(height: f64) -> PartMesh {
        let (s, k) = (48u32, (height / 0.5) as u32 + 1);
        let mut positions = Vec::new();
        for ring in 0..k {
            let z = ring as f64 * 0.5;
            let r = 12.0 + 3.0 * (z / 5.0).sin();
            for j in 0..s {
                let a = std::f64::consts::TAU * j as f64 / s as f64;
                positions.push([120.0 + r * a.cos(), 110.0 + r * a.sin(), z]);
            }
        }
        positions.push([120.0, 110.0, 0.0]);
        positions.push([120.0, 110.0, (k - 1) as f64 * 0.5]);
        let (bottom, top) = (k * s, k * s + 1);
        let mut triangles = Vec::new();
        for ring in 0..k - 1 {
            for j in 0..s {
                let (a, b) = (ring * s + j, ring * s + (j + 1) % s);
                triangles.push([a, b, b + s]);
                triangles.push([a, b + s, a + s]);
            }
        }
        for j in 0..s {
            triangles.push([bottom, (j + 1) % s, j]);
            triangles.push([top, (k - 1) * s + j, (k - 1) * s + (j + 1) % s]);
        }
        PartMesh { positions, triangles }
    }

    /// An edit of a mesh's vertices (its top fifth raised 3 mm: the same triangles) re-cuts
    /// only the planes the moved triangles span, plans again only the layers whose context
    /// they reach, and settles to the fresh slice.
    #[test]
    fn a_mesh_vertex_edit_recuts_and_plans_only_the_layers_it_reaches() {
        let part = Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: "vase".into(), name: "vase".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh: turned_vase(30.0), field_bounds: None, field_floor: None, ..Default::default() };
        let p0 = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part] };
        let mut p1 = p0.clone();
        for q in &mut p1.parts[0].mesh.positions {
            if q[2] > 24.0 {
                q[2] += 3.0;
            }
        }
        // From scratch as the engine slices it: on the one tool it prints in.
        let frames: Vec<Frame> = p1.parts.iter().map(|p| Frame { local: Arc::new(p.mesh.clone()), offset: [0.0; 3], field: None }).collect();
        let fresh = slice_frames(&setup(), &p1, &frames, &[], None).unwrap();
        let (e, rx) = engine();
        e.settle(1, LiveInput::ready(1, 1, (*setup()).clone(), Arc::new(p0), Arc::new(Vec::new())));
        let _ = settled(&rx, 1);
        let (traced, planned) = (e.traced_count(), e.layers_computed());
        let before = kept_and_planned(&e, 0);
        e.settle(2, LiveInput::ready(2, 1, (*setup()).clone(), Arc::new(p1), Arc::new(Vec::new())));
        let r = settled(&rx, 2);
        let total = fresh.layers().len() as u64;
        kept_some_since(&e, 0, before, total, "a mesh's top raised");
        let (cut, again) = (e.traced_count() - traced, e.layers_computed() - planned);
        assert!(cut > 0 && cut * 2 < total, "{cut} of {total} layers cut again");
        assert!(again > 0 && again * 2 < total, "{again} of {total} layers planned again");
        assert!(r.gcode == fresh.gcode, "the settled slice after a mesh edit differs from the fresh one");    }

    /// An edit of a part with tree supports plans only the model layers it reached, as a plain
    /// part's does: the others are those its last plan was finished from (that plan's own layers
    /// hold its supports), and its supports are planned anew, of what the trees kept. The settled
    /// slice is a fresh engine's.
    #[test]
    fn an_edit_of_a_supported_part_plans_only_the_layers_it_reaches() {
        let part = |mesh| Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: "cap".into(), name: "cap".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh, field_bounds: None, field_floor: None, ..Default::default() };
        // A cap on a tall stem: the trees hold up its overhang, the edit raises its top.
        let cap = |height: f64| {
            let mut stem = cube([96.0, 96.0, 0.0], [8.0, 8.0, 20.0]);
            let cap = cube([88.0, 88.0, 20.0], [24.0, 24.0, height]);
            let b = stem.positions.len() as u32;
            stem.positions.extend(cap.positions);
            stem.triangles.extend(cap.triangles.iter().map(|t| [t[0] + b, t[1] + b, t[2] + b]));
            Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part(stem)] }
        };
        let (p0, p1) = (cap(3.0), cap(4.0));
        let s = trees("2");
        let input = |id: u64, p: &Plate| LiveInput::ready(id, 1, s.clone(), Arc::new(p.clone()), Arc::new(Vec::new()));
        // the oracle: a fresh engine's slice of the edited plate, and what its trees made
        let (fresh, grown) = {
            let (e, rx) = engine();
            e.settle(1, input(1, &p1));
            (settled(&rx, 1), e.tree_work()[0])
        };
        let (e, rx) = engine();
        e.settle(1, input(1, &p0));
        let _ = settled(&rx, 1);
        let (traced, planned, trees) = (e.traced_count(), e.part_work()[0].1, e.tree_work()[0]);
        e.settle(2, input(2, &p1));
        let r = settled(&rx, 2);
        let (cut, again) = (e.traced_count() - traced, e.part_work()[0].1 - planned);
        assert!(cut > 0 && cut * 2 < traced, "{cut} of {traced} layers cut again");
        assert!(again > 0 && again * 2 < planned, "{again} of {planned} model layers planned again");
        // the supports planned once anew from the new slices, of what the trees kept: the stages
        // up to the descent made again, each later one no more than a fresh plan makes of it
        let w = e.tree_work()[0];
        let made = [w.prepared - trees.prepared, w.stepped - trees.stepped, w.smoothed - trees.smoothed, w.drawn - trees.drawn, w.put - trees.put, w.printed - trees.printed];
        let fresh_made = [grown.prepared, grown.stepped, grown.smoothed, grown.drawn, grown.put, grown.printed];
        assert!(made[0] == 1 && made.iter().zip(&fresh_made).all(|(m, f)| m <= f), "the edit's trees made {made:?} afresh, a fresh plan's {fresh_made:?}");
        assert!(r.gcode == fresh.gcode, "the settled slice after an edit of a supported part differs from a fresh engine's");
        let (kept, none) = e.part_kept().swap_remove(0);
        assert!(kept > 0 && none.is_empty(), "the edit kept {kept} layers ({none})");
        // A wider cap moves the part's box in X: the infill is centred on it, so nothing is kept,
        // and the engine says why.
        let mut wider = cap(4.0);
        for q in &mut wider.parts[0].mesh.positions {
            if q[0] > 111.0 {
                q[0] += 1.0;
            }
        }
        e.settle(3, input(3, &wider));
        let _ = settled(&rx, 3);
        let (again, none) = e.part_kept().swap_remove(0);
        assert!(again == kept && none.starts_with("its XY box moved"), "kept {} more, {none:?}", again - kept);
    }

    /// A field part with a gentle domed top and the head known: the engine's
    /// settle plans the non-planar top (its planar layers leave out the cap
    /// clip) exactly as a fresh slice with the same head does, and the
    /// planar-only slice differs.
    #[test]
    fn a_field_part_with_a_nonplanar_top_settles_like_a_fresh_slice() {
        use makepad_csg_sdf::{SdfNode, SdfOp};
        let root = SdfNode::Combine {
            op: SdfOp::Intersection,
            blend: 0.0,
            of: vec![
                SdfNode::Box { size: [14.0, 7.0, 14.0], radius: 0.0 },
                SdfNode::Transform { at: [0.0, -24.5, 0.0], rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Sphere { radius: 28.0 }) },
            ],
        };
        let shape = Arc::new(SdfShape { root, min: [-7.5, -4.0, -7.5], max: [7.5, 4.0, 7.5], materials: Vec::new() });
        let field = PartField::new(shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 3.5]]);
        let local = Arc::new(cube([-7.0, -7.0, 0.0], [14.0, 14.0, 7.0]));
        let mut m = (*local).clone();
        m.translate([150.0, 120.0, 0.0]);
        let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: "dome".into(), name: "dome".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh: m, field_bounds: None, field_floor: None, ..Default::default() }] };
        let frames = vec![Frame { local, offset: [150.0, 120.0, 0.0], field: Some(field) }];
        let mut on = (*setup()).clone();
        on.process.values.set_str("fab_nonplanar_top", "1");
        let head = makepad_fab_toolpath::np_test_head();
        let fresh = slice_frames(&on, &plate, &frames, &[], Some(&head)).unwrap();
        let planar = slice_frames(&on, &plate, &frames, &[], None).unwrap();
        assert!(fresh.gcode != planar.gcode, "the cap clip changes the planar layers");
        let (e, rx) = engine();
        let mut input = LiveInput::ready(1, 1, on.clone(), Arc::new(plate.clone()), Arc::new(Vec::new()));
        input.frames = Some(Arc::new(frames.clone()));
        input.np_head = Some(Arc::new(head));
        e.settle(1, input);
        let r = settled(&rx, 1);
        assert!(r.gcode == fresh.gcode, "the settled non-planar slice differs from the fresh one");
    }

    /// A settle shares the previous settle's per-layer timeline chunks for
    /// every layer that did not change: an edit high on the box keeps the
    /// lower layers' chunks.
    #[test]
    fn settles_share_unchanged_timeline_chunks() {
        let p0 = plate();
        let mut p1 = p0.clone();
        for q in &mut p1.parts[0].mesh.positions {
            if q[2] > 10.0 {
                q[2] += 1.0;
            }
        }
        let (e, rx) = engine();
        e.settle(1, LiveInput::ready(1, 1, (*setup()).clone(), Arc::new(p0), Arc::new(Vec::new())));
        let a = settled(&rx, 1);
        e.settle(2, LiveInput::ready(2, 1, (*setup()).clone(), Arc::new(p1), Arc::new(Vec::new())));
        let b = settled(&rx, 2);
        let same: Vec<bool> = a.layers.chunks.iter().zip(&b.layers.chunks).map(|(x, y)| Arc::ptr_eq(x, y)).collect();
        let shared = same.iter().take_while(|s| **s).count();
        assert!(shared >= 10, "only {shared} leading chunks shared: {same:?}");
        assert!(shared < b.layers.chunks.len(), "the edited layers have new chunks");
        assert!((b.layers.total - b.timeline.total).abs() < 1e-6);
    }

    /// Copies of one part are planned once, and the plate still settles to
    /// its plain slice.
    #[test]
    fn copies_are_planned_once() {
        let mut plate = plate();
        let mut copy = plate.parts[1].clone();
        copy.mesh.translate([0.0, 60.0, 0.0]);
        copy.name = "box copy".into();
        plate.parts.push(copy);
        let fresh = crate::plate::slice_plate(&setup(), &plate).unwrap();
        let (e, rx) = engine();
        e.settle(3, input(&plate));
        let r = settled(&rx, 3);
        assert_eq!(e.parts_planned(), 2, "three parts, two of them copies");
        assert!(r.gcode == fresh.gcode, "the settled slice differs from the plain one");
    }

    /// A settle and then a transform, before the engine starts (forced with a
    /// spawn that only runs when told): the settle is the moved plate's,
    /// says so, and is exactly the slice of what it reports.
    #[test]
    fn a_transform_after_a_settle_is_in_that_settle() {
        let jobs: Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>> = Arc::new(Mutex::new(Vec::new()));
        let held = jobs.clone();
        let spawn: Spawn = Arc::new(move |_, job| held.lock().unwrap().push(job));
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: Sink = Arc::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
        });
        let e = LiveEngine::new(spawn, sink, checkpoints());
        let plate = plate();
        e.settle(5, input(&plate));
        e.set_transform(1, translate(-12.5, 4.25));
        assert!(!jobs.lock().unwrap().is_empty(), "the driver was asked for");
        loop {
            let job = jobs.lock().unwrap().pop();
            let Some(job) = job else { break };
            job();
        }
        let r = settled(&rx, 5);
        assert!(r.transformed, "the settle says the plate moved");
        assert!((r.sliced[1].offset[0] + 12.5).abs() < 1e-6 && (r.sliced[1].offset[1] - 4.25).abs() < 1e-6, "{:?}", r.sliced[1].offset);
        let again = slice_frames(&setup(), &plate, &r.sliced, &[], None).unwrap();
        assert!(again.gcode == r.gcode, "the settle is not the slice of what it reports");
    }

    /// The plate's two filaments swapped between its nozzles (`nozzles` their diameters), the
    /// swapped slice checked against a fresh engine's: the parts planned and contour layers
    /// traced for it, the nozzles the cap's preview was sent on, and whether the sink was told
    /// the settings changed.
    fn swapped_between(nozzles: [&str; 2]) -> (u64, u64, std::collections::BTreeSet<u8>, bool) {
        let p = Arc::new(plate());
        let mapped = |map: Vec<u32>| {
            let mut s = (*setup()).clone();
            s.machine.values.set_vec("nozzle_diameter", nozzles.iter().map(|n| n.to_string()).collect());
            s.filament_map = map;
            s
        };
        let (e, rx) = engine();
        e.settle(1, LiveInput::ready(1, 1, mapped(vec![1, 2]), p.clone(), Arc::new(Vec::new())));
        let first = settled(&rx, 1);
        let (planned, traced) = (e.parts_planned(), e.traced_count());
        e.settle(2, LiveInput::ready(1, 2, mapped(vec![2, 1]), p.clone(), Arc::new(Vec::new())));
        let (mut shown, mut told) = (std::collections::BTreeSet::new(), false);
        let swapped = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::Settings => told = true,
                LiveEvent::PartLayers { part: 0, layers, .. } => shown.extend(layers.iter().flat_map(|(_, p)| p.records().into_iter().map(|m| m.nozzle))),
                LiveEvent::Settled { tag: 2, result, .. } => break result.expect("sliced"),
                _ => {}
            }
        };
        let fresh = {
            let (e, rx) = engine();
            e.settle(1, LiveInput::ready(1, 2, mapped(vec![2, 1]), p.clone(), Arc::new(Vec::new())));
            settled(&rx, 1)
        };
        assert!(swapped.gcode == fresh.gcode, "the swapped slice differs from a fresh engine's");
        assert!(swapped.gcode != first.gcode, "the swap reaches the G-code");
        (e.parts_planned() - planned, e.traced_count() - traced, shown, told)
    }

    /// Filaments moved between nozzles alike are the writer's alone: no contour is traced and
    /// no part planned again, the G-code is a fresh engine's, and the preview is sent again
    /// with each part on its new nozzle. Between nozzles unlike, the parts plan anew.
    #[test]
    fn a_filament_moved_to_a_nozzle_like_its_own_runs_only_the_writer() {
        let (planned, traced, shown, told) = swapped_between(["0.4", "0.4"]);
        assert_eq!((planned, traced), (0, 0), "nozzles alike: only the writer runs");
        assert!(told && shown == [1u8].into(), "the cap's preview is sent again on its new nozzle: {shown:?}, told {told}");
        let (planned, _, _, _) = swapped_between(["0.4", "0.6"]);
        assert_eq!(planned, 2, "each part plans anew on a nozzle unlike its own");
    }

    /// Support settings plan the supports again on the parts' slices as they are, one after
    /// another on one engine (a tree's branch diameter, then supports on the build plate only,
    /// then a branch angle): at each no contour is traced and no model layer planned, nothing the
    /// sink shows of the parts' layers is dropped or sent again (their contours keep their keys),
    /// the cap's supports, planned on its slices as they are (the cap's content key), are
    /// cleared, grow and are then replaced, and the G-code is a fresh engine's, with other trees
    /// than the step before. The cap has a ledge of its own under it: on the build plate only, the
    /// trees over the ledge go down to the bed instead.
    #[test]
    fn a_support_setting_plans_only_the_supports_again() {
        let mut p = plate();
        let ledge = cube([84.0, 88.0, 0.0], [10.0, 24.0, 3.0]);
        let cap = &mut p.parts[0].mesh;
        let b = cap.positions.len() as u32;
        cap.positions.extend(ledge.positions);
        cap.triangles.extend(ledge.triangles.iter().map(|t| t.map(|v| v + b)));
        let p = Arc::new(p);
        let supported = |settings: &[(&str, &str)]| {
            let mut s = trees("2");
            for &(key, value) in settings {
                s.process.values.set_str(key, value);
            }
            s
        };
        let steps: [&[(&str, &str)]; 4] = [
            &[("tree_support_branch_diameter_organic", "5")],
            &[("tree_support_branch_diameter_organic", "5"), ("support_on_build_plate_only", "1")],
            &[("tree_support_branch_diameter_organic", "5"), ("support_on_build_plate_only", "1"), ("tree_support_branch_angle_organic", "25")],
            &[("tree_support_branch_diameter_organic", "5"), ("support_on_build_plate_only", "1"), ("tree_support_branch_angle_organic", "25"), ("support_object_first_layer_gap", "1")],
        ];
        let (e, rx) = engine();
        e.settle(1, LiveInput::ready(1, 1, supported(&[]), p.clone(), Arc::new(Vec::new())));
        let (mut cap, mut contours) = (None, None);
        let mut before = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::PartCut { part: 0, key, .. } => cap = Some(key),
                LiveEvent::Contours { parts, .. } => contours = Some(parts),
                LiveEvent::Settled { tag: 1, result, .. } => break result.expect("sliced"),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        };
        let (cap, contours) = (cap.expect("the cap's slices"), contours.expect("the parts' contours"));
        let (work, traced) = (e.part_work(), e.traced_count());
        for (step, settings) in steps.into_iter().enumerate() {
            let tag = step as u64 + 2;
            e.settle(tag, LiveInput::ready(1, tag, supported(settings), p.clone(), Arc::new(Vec::new())));
            let (mut told, mut layers_sent, mut cleared, mut grew, mut supports) = (false, 0, false, false, None);
            let again = loop {
                match rx.recv().expect("a settled slice") {
                    LiveEvent::Settings => told = true,
                    LiveEvent::PartCut { .. } | LiveEvent::PartRegions { .. } | LiveEvent::PartLayers { .. } => layers_sent += 1,
                    LiveEvent::Contours { parts, .. } => assert_eq!(parts, contours, "{settings:?}: the parts' contours are the first slice's"),
                    LiveEvent::PartSupports { part: 0, key, .. } if key != cap => panic!("{settings:?}: the cap's supports are planned on other slices ({key:x}, the cap's {cap:x})"),
                    LiveEvent::PartSupports { part: 0, layers, done: false, .. } if layers.is_empty() => {
                        assert!(!grew && supports.is_none(), "{settings:?}: the old supports are cleared before the new ones grow");
                        cleared = true;
                    }
                    LiveEvent::PartSupports { part: 0, done: false, .. } => {
                        assert!(cleared && supports.is_none(), "{settings:?}: the new trees grow once the old are cleared, before their plan ends");
                        grew = true;
                    }
                    LiveEvent::PartSupports { part: 0, layers, done: true, .. } => supports = Some(layers),
                    LiveEvent::Settled { tag: t, result, .. } if t == tag => break result.expect("sliced"),
                    LiveEvent::Failed { error, .. } => panic!("{error}"),
                    _ => {}
                }
            };
            let fresh = {
                let (e, rx) = engine();
                e.settle(1, LiveInput::ready(1, tag, supported(settings), p.clone(), Arc::new(Vec::new())));
                settled(&rx, 1)
            };
            assert!(again.gcode == fresh.gcode, "{settings:?}: the replanned supports differ from a fresh engine's");
            assert!(again.gcode != before.gcode, "{settings:?}: the setting changes the trees");
            assert_eq!((e.part_work(), e.traced_count()), (work.clone(), traced), "{settings:?}: no contour traced, no model layer planned");
            assert!(!told && layers_sent == 0, "{settings:?}: the parts' layers stay as shown: told {told}, {layers_sent} layer events");
            assert!(cleared && grew, "{settings:?}: the cap's old supports are cleared and the new grow: cleared {cleared}, grew {grew}");
            assert!(supports.is_some_and(|l| l.iter().any(|(_, r)| !r.is_empty())), "{settings:?}: the cap's new supports are shown");
            before = again;
        }
    }

    /// The cap beside a smaller cap on its own stem, of other content (another size), so each
    /// plans on its own.
    fn two_caps() -> Plate {
        let mut plate = plate();
        let mut stem = cube([150.0, 96.0, 0.0], [6.0, 6.0, 8.0]);
        let cap = cube([142.0, 88.0, 8.0], [22.0, 22.0, 3.0]);
        let b = stem.positions.len() as u32;
        stem.positions.extend(cap.positions);
        stem.triangles.extend(cap.triangles.iter().map(|t| t.map(|v| v + b)));
        plate.parts[1].mesh = stem;
        plate.parts[1].name = "small cap".into();
        plate
    }

    /// A part's own support setting (a branch diameter on the cap alone; the trees the user
    /// deletes on a part are one too) plans that part's supports again and nothing else: no
    /// contour traced and no model layer planned, the small cap's supports neither planned nor
    /// sent again, and the G-code a fresh engine's, with the cap's other trees.
    #[test]
    fn a_parts_own_support_setting_plans_only_its_supports() {
        let p = two_caps();
        let (e, rx) = engine();
        e.settle(1, LiveInput::ready(1, 1, trees("2"), Arc::new(p.clone()), Arc::new(Vec::new())));
        let first = settled(&rx, 1);
        let (work, traced, planned) = (e.part_work(), e.traced_count(), e.parts_planned());
        let mut own = p.clone();
        own.parts[0].print = vec![("tree_support_branch_diameter_organic".into(), "5".into())];
        e.settle(2, LiveInput::ready(2, 1, trees("2"), Arc::new(own.clone()), Arc::new(Vec::new())));
        let mut supports = Vec::new();
        let again = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::PartCut { part, .. } | LiveEvent::PartRegions { part, .. } | LiveEvent::PartLayers { part, .. } => panic!("part {part}'s layers are sent again"),
                LiveEvent::PartSupports { part, done: true, .. } => supports.push(part),
                LiveEvent::Settled { tag: 2, result, .. } => break result.expect("sliced"),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        };
        idle(&e);
        assert_eq!(supports, [0], "only the cap's supports go out again");
        assert_eq!((e.part_work(), e.traced_count()), (work, traced), "no contour traced, no model layer planned");
        assert_eq!(e.parts_planned(), planned + 1, "the cap's supports alone are planned again");
        let fresh = {
            let (e, rx) = engine();
            e.settle(1, LiveInput::ready(2, 1, trees("2"), Arc::new(own), Arc::new(Vec::new())));
            settled(&rx, 1)
        };
        assert!(again.gcode == fresh.gcode, "the replanned cap differs from a fresh engine's");
        assert!(again.gcode != first.gcode, "the cap's own branch diameter changes its trees");
    }

    /// Paint of a no-trees box alone (printer mm), as `m.no_trees(m.box(..))` gives it: the
    /// part's own colour everywhere.
    pub(super) struct NoTreesBox;

    impl fab_plate::PaintChange for NoTreesBox {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn changed(&self, _: &dyn fab_plate::PaintChange) -> Option<Vec<(f64, f64)>> {
            Some(Vec::new())
        }
    }

    fn no_trees_paint(lo: [f64; 3], hi: [f64; 3]) -> fab_plate::SpatialPaint {
        let at = move |q: [f64; 3]| (0..3).map(|k| (lo[k] - q[k]).max(q[k] - hi[k])).fold(f64::MIN, f64::max);
        fab_plate::SpatialPaint {
            sample_at: Arc::new(|_| fab_plate::PaintSample { slot: 0, dist: 1e9, mix: None }),
            support_block: None,
            support_enforce: None,
            no_trees: Some(fab_plate::SupportRegion { at: Arc::new(at), bounds: Some((lo, hi)) }),
            settings: Vec::new(),
            colors: vec![fab_plate::paint::PaintColor { rgb: [1.0; 3], color: None, filament: None, nozzle: None }],
            key: 0,
            no_trees_key: lo.iter().chain(&hi).fold(0x6e6f_7472_6565_7321u64, |h, v| (h ^ v.to_bits()).wrapping_mul(0x100_0000_01b3)) | 1,
            regions_key: 0,
            source: Arc::new(NoTreesBox),
        }
    }

    /// A no-trees area on the cap (`m.no_trees`, under its +x side) plans that part's supports
    /// again and nothing else, as its own support setting does: no contour traced and no model
    /// layer planned or sent, the small cap's supports neither planned nor sent again, and the
    /// G-code a fresh engine's, with the cap's trees round the area.
    #[test]
    fn a_no_trees_area_plans_only_its_parts_supports() {
        let p = two_caps();
        let (e, rx) = engine();
        e.settle(1, LiveInput::ready(1, 1, trees("2"), Arc::new(p.clone()), Arc::new(Vec::new())));
        let first = settled(&rx, 1);
        let (work, traced, planned) = (e.part_work(), e.traced_count(), e.parts_planned());
        let mut kept = p.clone();
        kept.parts[0].paint = Some(no_trees_paint([104.5, 90.0, 0.0], [112.0, 110.0, 4.0]));
        e.settle(2, LiveInput::ready(2, 1, trees("2"), Arc::new(kept.clone()), Arc::new(Vec::new())));
        let mut supports = Vec::new();
        let again = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::PartCut { part, .. } | LiveEvent::PartRegions { part, .. } | LiveEvent::PartLayers { part, .. } => panic!("part {part}'s layers are sent again"),
                LiveEvent::PartSupports { part, done: true, .. } => supports.push(part),
                LiveEvent::Settled { tag: 2, result, .. } => break result.expect("sliced"),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        };
        idle(&e);
        assert_eq!(supports, [0], "only the cap's supports go out again");
        assert_eq!((e.part_work(), e.traced_count()), (work, traced), "no contour traced, no model layer planned");
        assert_eq!(e.parts_planned(), planned + 1, "the cap's supports alone are planned again");
        let fresh = {
            let (e, rx) = engine();
            e.settle(1, LiveInput::ready(2, 1, trees("2"), Arc::new(kept), Arc::new(Vec::new())));
            settled(&rx, 1)
        };
        assert!(again.gcode == fresh.gcode, "the replanned cap differs from a fresh engine's");
        assert!(again.gcode != first.gcode, "the area moves the cap's trees");
    }

    /// A tree deleted on the cap (its tips, the part's own setting) goes from the view at once:
    /// the layers it stood on are sent without it before the plan ends, and nothing of the cap's
    /// supports clears or grows meanwhile; the plan then sends them whole, once, grown without
    /// it (its tips are no tree's, every other tip still is), and the slice is a fresh engine's.
    /// The part's trees plan from what they kept of the last plan, and an undo is exact too.
    #[test]
    fn a_deleted_tree_goes_from_the_view_at_once() {
        let p = plate();
        let (e, rx) = engine();
        e.settle(1, LiveInput::ready(1, 1, trees("2"), Arc::new(p.clone()), Arc::new(Vec::new())));
        let (mut shapes, mut shown, mut told) = (None, None, Vec::new());
        let first = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::PartTrees { part: 0, trees, .. } => {
                    // As the descent grows them, then as the planner places them (moved off where
                    // the descent left them): a branch is picked while they grow and while their
                    // lines are laid.
                    assert!(shown.is_none(), "the cap's trees go out ahead of its supports' lines");
                    told.push(trees.iter().any(|t| t.discs.iter().any(|d| d.at != d.from)));
                    shapes = Some(trees);
                }
                LiveEvent::PartSupports { part: 0, layers, done: true, .. } => shown = Some(records_of_layers(&layers)),
                LiveEvent::Settled { tag: 1, result, .. } => break result.expect("sliced"),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        };
        let (shapes, shown) = (shapes.expect("the cap's trees"), shown.expect("the cap's supports"));
        assert!(told.len() > 1 && !told[0] && told[told.len() - 1], "the cap's trees go out as the descent grows them, then placed: {told:?}");
        assert!(shapes.len() > 1 && shapes.iter().all(|t| !t.tips.is_empty() && !t.discs.is_empty()), "the cap stands on trees, each grown from tips: {}", shapes.len());
        let tree = shapes.iter().max_by_key(|t| t.discs.len()).expect("a tree").clone();
        let mut dropped = p.clone();
        dropped.parts[0].print = vec![(DROPPED_TIPS.into(), makepad_fab_toolpath::dropped_text(&tree.tips))];
        e.settle(2, LiveInput::ready(2, 1, trees("2"), Arc::new(dropped.clone()), Arc::new(Vec::new())));
        let (mut early, mut whole, mut grown) = (Vec::new(), 0, None);
        let again = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::PartTrees { part: 0, trees, .. } => {
                    assert!(whole == 0 && grown.is_none(), "the trees grown again go out once, ahead of the plan's lines");
                    grown = Some(trees);
                }
                LiveEvent::PartCut { part, .. } | LiveEvent::PartRegions { part, .. } | LiveEvent::PartLayers { part, .. } => panic!("part {part}'s layers are sent again"),
                LiveEvent::PartSupports { part: 0, layers, done: false, .. } => {
                    assert!(whole == 0 && !layers.is_empty(), "nothing clears, before the plan ends");
                    early.push(records_of_layers(&layers));
                }
                LiveEvent::PartSupports { part: 0, done: true, .. } => whole += 1,
                LiveEvent::Settled { tag: 2, result, .. } => break result.expect("sliced"),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        };
        idle(&e);
        assert_eq!((early.len(), whole), (1, 1), "the tree goes at once, nothing grows, the plan is sent once");
        let shown: std::collections::BTreeMap<u32, Vec<MoveRecord>> = shown.into_iter().collect();
        for (layer, left) in &early[0] {
            let before = shown.get(layer).map_or(0, Vec::len);
            assert!(left.len() < before, "layer {layer}: {} of {before} records left", left.len());
            for d in tree.discs.iter().filter(|d| d.layer == *layer) {
                let on = |r: &&MoveRecord| ((r.p0[0] + r.p1[0]) / 2.0 - d.at[0]).hypot((r.p0[1] + r.p1[1]) / 2.0 - d.at[1]) <= d.r + r.width / 2.0;
                assert!(!left.iter().any(|r| on(&r)), "layer {layer}: nothing left on the tree");
            }
        }
        assert!(early[0].iter().any(|(l, _)| tree.discs.iter().any(|d| d.layer == *l)), "the tree's own layers go");
        let tips = |trees: &[TreeShape]| {
            let mut tips: Vec<[f64; 3]> = trees.iter().flat_map(|t| t.tips.iter().copied()).collect();
            tips.sort_by(|a, b| a.partial_cmp(b).expect("tips are numbers"));
            tips
        };
        let left: Vec<[f64; 3]> = tips(&shapes).into_iter().filter(|t| !tree.tips.contains(t)).collect();
        assert_eq!(tips(&grown.expect("the trees grown again")), left, "the deleted tree's tips are no tree's, and every other tip still is");
        assert!(again.gcode != first.gcode, "the trees print without it");
        let fresh = {
            let (e, rx) = engine();
            e.settle(1, LiveInput::ready(2, 1, trees("2"), Arc::new(dropped), Arc::new(Vec::new())));
            settled(&rx, 1)
        };
        assert!(again.gcode == fresh.gcode, "the slice is a fresh engine's");
        // Undone: the tree grows again from what the part's trees kept, as it first did.
        e.settle(3, LiveInput::ready(3, 1, trees("2"), Arc::new(p), Arc::new(Vec::new())));
        assert!(settled(&rx, 3).gcode == first.gcode, "undone, the slice is the first one again");
    }

    /// Trees deleted while the cap's trees grow go from the view at once, and nothing of the cap's
    /// supports clears: one as the descent has grown it so far, then one as placed, none of its
    /// lines laid. Each time the trees left go out first, then every layer the tree showed on, as
    /// it showed but for the tree's rings and its tips'. The plan grows on from there and ends a
    /// fresh engine's, its trees too.
    #[test]
    fn trees_deleted_as_they_grow_go_from_the_view_and_nothing_clears() {
        type Layers = std::collections::BTreeMap<u32, Vec<MoveRecord>>;
        // The cap's supports as the view has them, and how many trees were deleted; held where a
        // tree is deleted: first at the trees as the descent has them, then at them as placed
        // (moved off where the descent left them).
        let (held, at_trees) = mpsc::channel::<(Arc<[TreeShape]>, Layers)>();
        let (go, hold) = mpsc::channel::<()>();
        let (view, held, hold) = (Mutex::new((Layers::new(), 0)), Mutex::new(held), Mutex::new(hold));
        let (e, rx) = engine_seeing(move |ev| match ev {
            LiveEvent::PartSupports { part: 0, layers, done, .. } => {
                let mut v = view.lock().unwrap();
                if *done || layers.is_empty() {
                    v.0.clear();
                }
                v.0.extend(records_of_layers(layers));
            }
            LiveEvent::PartTrees { part: 0, trees, .. } => {
                let shown = {
                    let mut v = view.lock().unwrap();
                    let due = match v.1 {
                        0 => !trees.is_empty(),
                        1 => trees.iter().any(|t| t.discs.iter().any(|d| d.at != d.from)),
                        _ => false,
                    };
                    if !due {
                        return;
                    }
                    v.1 += 1;
                    v.0.clone()
                };
                held.lock().unwrap().send((trees.clone(), shown)).expect("the test waits for the trees");
                hold.lock().unwrap().recv().expect("the test lets the plan go on");
            }
            _ => {}
        });
        // A layer's paths as the view draws them: each run of moves end to end, by its bounds'
        // centre.
        let centres = |records: &[MoveRecord]| -> Vec<[f32; 2]> {
            records.chunk_by(|a, b| a.p1 == b.p0).map(|run| {
                let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
                for p in run.iter().flat_map(|r| [r.p0, r.p1]) {
                    for k in 0..2 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
                [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0]
            }).collect()
        };
        let at = |a: [f32; 2], b: [f32; 2], d: f32| (a[0] - b[0]).hypot(a[1] - b[1]) <= d;
        let tip_on = |t: &TreeShape, k: usize| (t.discs[t.tip_discs[k] as usize].layer, [t.tips[k][0] as f32, t.tips[k][1] as f32]);
        // A tree in the view as it grew: its rings, where the descent left its branches, and its
        // tips' on their layers; and the roofs over its tips, up from them.
        let ring = |t: &TreeShape, l: u32, c: [f32; 2]| t.discs.iter().any(|d| d.layer == l && at(d.from, c, 0.01)) || (0..t.tips.len()).map(|k| tip_on(t, k)).any(|(on, p)| on == l && at(p, c, 0.01));
        let its = |t: &TreeShape, l: u32, c: [f32; 2]| ring(t, l, c) || (0..t.tips.len()).map(|k| tip_on(t, k)).any(|(on, p)| on <= l && at(p, c, 0.01));
        let tips = |trees: &[TreeShape]| {
            let mut tips: Vec<[f64; 3]> = trees.iter().flat_map(|t| t.tips.iter().copied()).collect();
            tips.sort_by(|a, b| a.partial_cmp(b).expect("tips are numbers"));
            tips
        };
        let p = plate();
        e.settle(1, LiveInput::ready(1, 1, trees("2"), Arc::new(p.clone()), Arc::new(Vec::new())));
        // Each delete: the trees it was picked from, the tree and the view then.
        let (mut deleted, mut dropped, mut gone) = (Vec::new(), p, Vec::new());
        for revision in [2, 3] {
            let (grown, shown) = at_trees.recv_timeout(std::time::Duration::from_secs(120)).expect("the cap's trees");
            // The biggest tree the view shows: a ring where the descent left one of its branches.
            let seen: std::collections::BTreeMap<u32, Vec<[f32; 2]>> = shown.iter().map(|(l, r)| (*l, centres(r))).collect();
            let shows = |t: &&TreeShape| t.discs.iter().any(|d| seen.get(&d.layer).is_some_and(|cs| cs.iter().any(|&c| at(d.from, c, 0.01))));
            let tree = grown.iter().filter(shows).max_by_key(|t| t.discs.len()).expect("a tree in view").clone();
            gone.extend(tree.tips.iter().copied());
            dropped.parts[0].print = vec![(DROPPED_TIPS.into(), makepad_fab_toolpath::dropped_text(&gone))];
            e.settle(revision, LiveInput::ready(revision, 1, trees("2"), Arc::new(dropped.clone()), Arc::new(Vec::new())));
            deleted.push((grown, tree, shown));
            go.send(()).expect("the plan waits");
        }
        // After each delete: the trees told, and the first layers sent.
        let (mut after, mut whole, mut on) = ([(Vec::new(), None), (Vec::new(), None)], 0, 0);
        let again = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::PartTrees { part: 0, trees, .. } if on < 2 && Arc::ptr_eq(&trees, &deleted[on].0) => on += 1,
                LiveEvent::PartTrees { part: 0, trees, .. } if on > 0 => {
                    assert!(!trees.is_empty() && whole == 0, "the cap's trees never clear, and go out ahead of its whole supports");
                    after[on - 1].0.push(trees);
                }
                LiveEvent::PartSupports { part: 0, layers, done: false, .. } if on > 0 => {
                    assert!(!layers.is_empty() && whole == 0, "nothing of the cap's supports clears");
                    assert!(!after[on - 1].0.is_empty(), "the trees left go out first");
                    if after[on - 1].1.is_none() {
                        after[on - 1].1 = Some(records_of_layers(&layers));
                    }
                }
                LiveEvent::PartSupports { part: 0, done: true, .. } => whole += 1,
                LiveEvent::Settled { tag: 3, result, .. } => break result.expect("sliced"),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        };
        idle(&e);
        assert_eq!((on, whole), (2, 1), "both trees deleted as they grew, the plan's supports go out whole once");
        for ((grown, tree, shown), (told, first)) in deleted.iter().zip(&after) {
            let first: Layers = first.clone().expect("the layers the tree showed on").into_iter().collect();
            for (l, was) in shown {
                let was = centres(was);
                let Some(left) = first.get(l).map(|r| centres(r)) else {
                    assert!(!was.iter().any(|&c| ring(tree, *l, c)), "layer {l}: the tree's rings go at once");
                    continue;
                };
                assert!(!left.iter().any(|&c| ring(tree, *l, c)), "layer {l}: nothing of the tree shows");
                for &c in was.iter().filter(|&&c| !its(tree, *l, c)) {
                    assert!(left.iter().any(|&k| at(k, c, 1e-3)), "layer {l}: all else that showed still shows ({c:?})");
                }
            }
            let left: Vec<[f64; 3]> = tips(grown).into_iter().filter(|t| !tree.tips.contains(t)).collect();
            assert_eq!(tips(&told[0]), left, "the trees left go out at once");
        }
        let (fresh, trees_fresh) = {
            let (e, rx) = engine();
            e.settle(1, LiveInput::ready(3, 1, trees("2"), Arc::new(dropped), Arc::new(Vec::new())));
            let mut grown = None;
            loop {
                match rx.recv().expect("a settled slice") {
                    LiveEvent::PartTrees { part: 0, trees, .. } => grown = Some(trees),
                    LiveEvent::Settled { tag: 1, result, .. } => break (result.expect("sliced"), grown.expect("the cap's trees")),
                    LiveEvent::Failed { error, .. } => panic!("{error}"),
                    _ => {}
                }
            }
        };
        assert_eq!(tips(after[1].0.last().expect("the trees grown on")), tips(&trees_fresh), "the trees are a fresh engine's");
        assert!(again.gcode == fresh.gcode, "the slice is a fresh engine's");
    }

    /// A tree deleted on the cap: its toolpaths and its settled slice go out before the trees'
    /// report (their rules check and hold measure), and Print waits for it: the sliced checks,
    /// which Print waits for, are under way from the settled slice on and end only once the
    /// report is in, and it is what a plan that measures it first tells.
    #[test]
    fn a_deleted_trees_toolpaths_go_out_before_their_report_and_print_waits_for_it() {
        // As each went out, by revision: the toolpaths, and whether the cap's report was in.
        let seen: Arc<Mutex<(Option<Arc<PlatePaths>>, Vec<(&'static str, u64, bool)>)>> = Arc::default();
        let at = seen.clone();
        let (e, rx) = engine_seeing(move |ev| {
            let mut at = at.lock().unwrap();
            match ev {
                LiveEvent::Toolpaths { revision, toolpaths, .. } => {
                    let told = toolpaths.plans()[0].0.skeleton.told().is_some();
                    at.0 = Some(toolpaths.clone());
                    at.1.push(("toolpaths", *revision, told));
                }
                LiveEvent::Settled { revision, .. } => {
                    let told = at.0.as_ref().is_some_and(|t| t.plans()[0].0.skeleton.told().is_some());
                    at.1.push(("settled", *revision, told));
                }
                // The sliced checks once the slice settled: under way, or whole.
                LiveEvent::Issues { revision, done, total, .. } if at.1.iter().any(|s| s.0 == "settled" && s.1 == *revision) => {
                    let told = at.0.as_ref().is_some_and(|t| t.plans()[0].0.skeleton.told().is_some());
                    at.1.push((if done < total { "checking" } else { "checked" }, *revision, told));
                }
                _ => {}
            }
        });
        // The background (the report's measure, the sliced checks) held while the deleted tree's
        // slice settles.
        let held: Arc<Mutex<Option<Vec<Box<dyn FnOnce() + Send>>>>> = Arc::default();
        let hold = held.clone();
        e.set_background(Arc::new(move |_, job| match hold.lock().unwrap().as_mut() {
            Some(waiting) => waiting.push(job),
            None => {
                std::thread::spawn(job);
            }
        }));
        let p = plate();
        e.settle(1, LiveInput::ready(1, 1, trees("2"), Arc::new(p.clone()), Arc::new(Vec::new())));
        // The cap's trees as placed: the last it is sent.
        let mut shapes = None;
        let shapes = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::PartTrees { part: 0, trees, .. } => shapes = Some(trees),
                LiveEvent::Settled { tag: 1, result, .. } => break result.map(|_| shapes.expect("the cap's trees")).expect("sliced"),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        };
        let tree = shapes.iter().max_by_key(|t| t.discs.len()).expect("a tree").clone();
        let mut dropped = p;
        dropped.parts[0].print = vec![(DROPPED_TIPS.into(), makepad_fab_toolpath::dropped_text(&tree.tips))];
        let input = LiveInput::ready(2, 1, trees("2"), Arc::new(dropped), Arc::new(Vec::new()));
        *held.lock().unwrap() = Some(Vec::new());
        e.settle(2, input.clone());
        let revision = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::Settled { tag: 2, revision, result, .. } => break result.map(|_| revision).expect("sliced"),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        };
        let waiting = held.lock().unwrap().take().unwrap_or_default();
        for job in waiting {
            std::thread::spawn(job);
        }
        loop {
            match rx.recv().expect("the sliced checks") {
                LiveEvent::Issues { revision: r, done, total, .. } if r == revision && done == total => break,
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        }
        idle(&e);
        let (paths, seen) = std::mem::take(&mut *seen.lock().unwrap());
        let order: Vec<(&str, bool)> = seen.into_iter().filter(|s| s.1 == revision).map(|s| (s.0, s.2)).collect();
        assert_eq!(order[..3], [("toolpaths", false), ("settled", false), ("checking", false)], "the toolpaths and the settled slice before the report, the checks under way from it: {order:?}");
        assert_eq!(order.last(), Some(&("checked", true)), "Print after it: {order:?}");
        let told = paths.expect("the toolpaths").plans()[0].0.skeleton.told().expect("the report");
        let first = plan_input_supports(par::Ctl::default(), &input).expect("the plate plans");
        assert!(first[0].report.is_none() && told.hold.is_some(), "a plan that measures first has told it");
        assert_eq!(format!("{told:?}"), format!("{:?}", first[0].skeleton), "the report is a plan's that measures it first");
    }

    /// Background checks wait behind every slice, so while slices keep the workers a settle's
    /// checks stay queued. Those of a revision a newer one superseded give way as the newer's are
    /// queued, and let go of their slice: the queue holds the latest revision's slice, not one per
    /// settle. The newer revision's checks still run whole.
    #[test]
    fn a_superseded_revisions_queued_checks_let_go_of_its_slice() {
        let (e, rx) = engine();
        let held: Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>> = Arc::default();
        let hold = held.clone();
        e.set_background(Arc::new(move |_, job| hold.lock().unwrap().push(job)));
        // The plate's box alone: a short slice.
        let mut p = plate();
        p.parts.remove(0);
        e.settle(1, input(&p));
        let first = Arc::downgrade(&settled(&rx, 1));
        idle(&e);
        assert!(first.upgrade().is_some(), "revision 1's checks are queued with its slice");
        p.parts[0].mesh = cube([200.0, 100.0, 0.0], [15.0, 12.0, 9.0]);
        e.settle(2, input(&p));
        let revision = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::Settled { tag: 2, revision, result, .. } => break result.map(|_| revision).expect("sliced"),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        };
        idle(&e);
        assert!(first.upgrade().is_none(), "the superseded revision's queued checks let go of its slice");
        let jobs = std::mem::take(&mut *held.lock().unwrap());
        for job in jobs {
            job();
        }
        loop {
            match rx.recv().expect("the newer revision's sliced checks") {
                LiveEvent::Issues { revision: r, done, total, .. } if r == revision && done == total => break,
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        }
    }

    /// The cap's tree supports, a branch diameter of `branch_diameter`.
    fn trees(branch_diameter: &str) -> JobSetup {
        let mut s = (*setup()).clone();
        s.process.values.set_str("enable_support", "1");
        s.process.values.set_str("support_type", "tree(auto)");
        s.process.values.set_str("tree_support_branch_diameter_organic", branch_diameter);
        s
    }

    /// The view is shown the trees as they grow, before their plan ends: what the planner has
    /// made of some layers, which the whole plan then replaces.
    #[test]
    fn the_trees_show_as_they_grow() {
        let (e, rx) = engine();
        e.settle(1, LiveInput::ready(1, 1, trees("2"), Arc::new(plate()), Arc::new(Vec::new())));
        let mut grown = 0;
        loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::PartSupports { part: 0, layers, done: false, .. } => grown += layers.iter().filter(|(_, r)| !r.is_empty()).count(),
                LiveEvent::PartSupports { part: 0, layers, done: true, .. } => {
                    assert!(grown > 0, "the trees showed before their plan ended");
                    assert!(layers.iter().any(|(_, r)| !r.is_empty()), "the whole plan replaces them");
                }
                LiveEvent::Settled { tag: 1, result, .. } => break assert!(result.is_ok()),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        }
        idle(&e);
    }

    /// An edit while the trees plan cuts their plan short: what grew of them goes from the view
    /// at once, only the new plan's supports are sent whole, and the slice is a fresh engine's.
    #[test]
    fn an_edit_while_the_trees_plan_cuts_it_short() {
        // The planner's thread waits on the cap's first growth until the edit is in.
        let (grew, first) = mpsc::channel::<()>();
        let (go, wait) = mpsc::channel::<()>();
        let held = Mutex::new(Some((grew, wait)));
        let (e, rx) = engine_seeing(move |ev| {
            if matches!(ev, LiveEvent::PartSupports { part: 0, done: false, layers, .. } if !layers.is_empty()) {
                if let Some((grew, wait)) = held.lock().unwrap().take() {
                    grew.send(()).unwrap();
                    wait.recv().expect("the edit is in");
                }
            }
        });
        let p = Arc::new(plate());
        e.set_input(LiveInput::ready(1, 1, trees("2"), p.clone(), Arc::new(Vec::new())));
        first.recv().expect("the trees grow");
        e.settle(2, LiveInput::ready(1, 2, trees("5"), p.clone(), Arc::new(Vec::new())));
        go.send(()).unwrap();
        let (mut cleared, mut whole) = (false, 0);
        let r = loop {
            match rx.recv().expect("a settled slice") {
                LiveEvent::PartSupports { part: 0, done: false, layers, .. } if layers.is_empty() => cleared = true,
                LiveEvent::PartSupports { part: 0, done: true, .. } => {
                    assert!(cleared, "what grew goes before the new plan's supports");
                    whole += 1;
                }
                LiveEvent::Settled { tag: 2, result, .. } => break result.expect("sliced"),
                LiveEvent::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        };
        idle(&e);
        assert!(cleared, "what grew went from the view");
        assert_eq!(whole, 1, "the cut plan sends no supports whole");
        let fresh = {
            let (e, rx) = engine();
            e.settle(1, LiveInput::ready(1, 2, trees("5"), p.clone(), Arc::new(Vec::new())));
            settled(&rx, 1)
        };
        assert!(r.gcode == fresh.gcode, "the edited slice is a fresh engine's");
    }

    /// A 6 mm post under a 20 mm cap 2 mm thick, the cap's underside on trees, as a field part on
    /// the bed, with a ball `ball` mm round inside the post, which no slice shows, a knob `knob`
    /// mm round on the post's side `y` mm over the bed, and its box `grow` mm wider round it than
    /// the solid, as a proof of its extent may leave it.
    fn capped_post_at(ball: f64, grow: f64, knob: f64, y: f64) -> LiveInput {
        field_part(capped_post_solids(20.0, ball, knob, y), grow)
    }

    /// [`capped_post_at`] under a cap 12 mm wide, the knob low on the post, 2 mm over the bed:
    /// fewer trees, still planning long enough for an edit to come while they plan.
    fn narrow_capped_post(ball: f64, grow: f64, knob: f64) -> LiveInput {
        field_part(capped_post_solids(12.0, ball, knob, 2.0), grow)
    }

    /// The solids of [`capped_post_at`] under a cap `cap` mm wide.
    fn capped_post_solids(cap: f64, ball: f64, knob: f64, y: f64) -> Vec<makepad_csg_sdf::SdfNode> {
        use makepad_csg_sdf::SdfNode;
        let block = |at: [f64; 3], size: [f64; 3]| placed(at, [0.0; 3], SdfNode::Box { size, radius: 0.0 });
        let mut of = vec![block([0.0, 4.0, 0.0], [6.0, 8.0, 6.0]), block([0.0, 9.0, 0.0], [cap, 2.0, cap])];
        if ball > 0.0 {
            of.push(placed([0.0, 4.0, 0.0], [0.0; 3], SdfNode::Sphere { radius: ball }));
        }
        if knob > 0.0 {
            of.push(placed([3.0, y, 0.0], [0.0; 3], SdfNode::Sphere { radius: knob }));
        }
        of
    }

    /// The post of [`capped_post_at`] on a block 6 mm high, under a cap 2 mm thick standing 3 mm out
    /// past one side of the block, as a field part on the bed, with a knob `knob` mm round on that
    /// side 1.5 mm over the bed: the trees of the cap over the block stand on it, and those past
    /// its side on the bed, by the knob.
    fn ledged_post(knob: f64) -> LiveInput {
        use makepad_csg_sdf::SdfNode;
        let block = |at: [f64; 3], size: [f64; 3]| placed(at, [0.0; 3], SdfNode::Box { size, radius: 0.0 });
        let mut of = vec![block([-1.75, 3.0, 0.0], [15.5, 6.0, 19.0]), block([0.0, 4.0, 0.0], [6.0, 8.0, 6.0]), block([1.5, 9.0, 0.0], [15.0, 2.0, 12.0])];
        if knob > 0.0 {
            of.push(placed([6.0, 1.5, 0.0], [0.0; 3], SdfNode::Sphere { radius: knob }));
        }
        field_part(of, 0.0)
    }

    /// A post under a 20 mm cap 2 mm thick, the cap's underside on trees, as a field part on the
    /// bed 40 mm high in layers of 0.4 mm, with a knob `low` mm round on the post's side 7 mm over
    /// the bed and one `high` mm round 33 mm over it, blended with the post and leaning and turned
    /// with it: an edit of the two reaches layers far apart.
    fn two_knobs(low: f64, high: f64) -> LiveInput {
        use makepad_csg_sdf::{SdfNode, SdfOp};
        let block = |at: [f64; 3], size: [f64; 3]| placed(at, [0.0; 3], SdfNode::Box { size, radius: 0.0 });
        let knob = |y: f64, radius: f64| placed([3.0, y, 0.0], [0.0; 3], SdfNode::Sphere { radius });
        let post = SdfNode::Combine { op: SdfOp::Union, blend: 0.3, of: vec![block([0.0, 19.0, 0.0], [6.0, 38.0, 6.0]), knob(7.0, low), knob(33.0, high)] };
        let mut s = trees("2");
        s.process.values.set_str("layer_height", "0.4");
        field_part_set(s, vec![placed([0.0; 3], [6.0, 35.0, 0.0], post), block([0.0, 39.0, 0.0], [20.0, 2.0, 20.0])], 0.0, 40.0)
    }

    /// `of` united as a field part on the bed, in a 20 mm box 10 mm high, the cap's underside on
    /// trees, its box `grow` mm wider round it than the solid, as a proof of its extent may leave
    /// it.
    fn field_part(of: Vec<makepad_csg_sdf::SdfNode>, grow: f64) -> LiveInput {
        field_part_set(trees("2"), of, grow, 10.0)
    }

    /// [`field_part`] under job `s`, `height` mm high.
    fn field_part_set(s: JobSetup, of: Vec<makepad_csg_sdf::SdfNode>, grow: f64, height: f64) -> LiveInput {
        use makepad_csg_sdf::{SdfNode, SdfOp};
        let shape = Arc::new(SdfShape { root: SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of }, min: [-10.5, -0.5, -10.5], max: [10.5, height + 0.5, 10.5], materials: Vec::new() });
        // Model Y up, (x, y, z) to the frame's (x, -z, y); its box in the frame as the layout put it.
        let field = PartField::new(shape, [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1.0, 0.0], [0.0, 1.0, 0.0, 0.0]]).with_bounds(Some(([-10.0 - grow, -10.0 - grow, 0.0], [10.0 + grow, 10.0 + grow, height])));
        let (local, at) = (cube([-10.0, -10.0, 0.0], [20.0, 20.0, height]), [150.0, 120.0, 0.0]);
        let mut mesh = local.clone();
        mesh.translate(at);
        let part = Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: "cap".into(), name: "cap".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh, field_bounds: None, field_floor: None, ..Default::default() };
        let mut input = LiveInput::ready(1, 1, s, Arc::new(Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part] }), Arc::new(Vec::new()));
        input.frames = Some(Arc::new(vec![Frame { local: Arc::new(local), offset: at, field: Some(field) }]));
        input
    }

    /// `first` sliced, then `edit`, and while the trees plan for it (waiting at the first of them
    /// the view is shown) `then`: what the part's trees made afresh by `first`'s slice and by
    /// `then`'s, the plans made after `first`'s, `then`'s slice and a fresh engine's, and what the
    /// fresh engine's trees made.
    fn edits_in_a_row(first: LiveInput, edit: LiveInput, then: impl Fn() -> LiveInput) -> (makepad_fab_toolpath::tree::Work, makepad_fab_toolpath::tree::Work, u64, Arc<Settled>, Arc<Settled>, makepad_fab_toolpath::tree::Work) {
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (shown, first_shown) = mpsc::channel::<()>();
        let (go, wait) = mpsc::channel::<()>();
        let gate = Mutex::new(Some((shown, wait)));
        let a = armed.clone();
        let (e, rx) = engine_seeing(move |ev| {
            if a.load(Ordering::Acquire) && matches!(ev, LiveEvent::PartSupports { part: 0, done: false, layers, .. } if !layers.is_empty()) {
                if let Some((shown, wait)) = gate.lock().unwrap().take() {
                    shown.send(()).unwrap();
                    wait.recv().expect("the next edit is in");
                }
            }
        });
        e.settle(1, first);
        settled(&rx, 1);
        let (before, planned) = (e.tree_work()[0], e.parts_planned());
        armed.store(true, Ordering::Release);
        e.set_input(edit);
        first_shown.recv().expect("the trees show for the edit");
        e.settle(2, then());
        go.send(()).unwrap();
        let r = settled(&rx, 2);
        idle(&e);
        let (fresh, made) = {
            let (e, rx) = engine();
            e.settle(1, then());
            (settled(&rx, 1), e.tree_work()[0])
        };
        (before, e.tree_work()[0], e.parts_planned() - planned, r, fresh, made)
    }

    /// Two edits in a row inside a field part, which leave its slices and its box as they were,
    /// the second while the trees plan for the first: the plan the second cuts short keeps what
    /// the trees kept, and neither makes anything of them afresh. The slice is a fresh engine's.
    #[test]
    fn a_plan_an_edit_cuts_short_keeps_what_the_trees_kept() {
        let (before, after, plans, r, fresh, _) = edits_in_a_row(narrow_capped_post(0.0, 0.0, 0.0), narrow_capped_post(1.0, 0.0, 0.0), || narrow_capped_post(1.5, 0.0, 0.0));
        assert!(before.prepared == 1 && before.stepped > 0 && before.drawn > 0 && before.printed > 0, "the first plan makes the trees: {before:?}");
        assert_eq!(plans, 1, "the first edit's plan is cut short");
        assert_eq!(after, before, "neither edit's plan makes anything of the trees afresh");
        assert!(r.gcode == fresh.gcode, "the slice is a fresh engine's");
    }

    /// Two edits in a row that move only a field part's box (a proof of its extent that changed,
    /// its slices the same), the second while the trees plan for the first: the bed the box leaves
    /// the part holds its trees, so neither plan makes anything of them afresh. The slice is a
    /// fresh engine's, and byte for byte the slice before the edits: the box moved alone changes
    /// nothing a fresh engine prints.
    #[test]
    fn edits_that_move_only_a_parts_box_take_its_trees_as_they_were() {
        let (before, after, plans, r, fresh, _) = edits_in_a_row(narrow_capped_post(0.0, 0.0, 0.0), narrow_capped_post(0.0, 1.0, 0.0), || narrow_capped_post(0.0, 2.5, 0.0));
        assert!(before.prepared == 1 && before.drawn > 0 && before.printed > 0, "the first plan makes the trees: {before:?}");
        assert_eq!(plans, 1, "the first edit's plan is cut short");
        assert_eq!(after, before, "neither edit's plan makes anything of the trees afresh");
        assert!(r.gcode == fresh.gcode, "the slice is a fresh engine's");
        let unmoved = {
            let (e, rx) = engine();
            e.settle(1, narrow_capped_post(0.0, 0.0, 0.0));
            settled(&rx, 1)
        };
        assert!(fresh.gcode == unmoved.gcode, "the slice before the box moved, byte for byte");
    }

    /// Two edits in a row that grow a knob low on the post's side, the second while the trees plan
    /// for the first: the part's slices change round the knob alone. The plan the second cuts
    /// short leaves the volumes as they were, and the second's takes their layers off the knob's
    /// reach, building fewer than half the volume layers the first plan built, the layers of the
    /// overhangs' passes the knob left, working on fewer than half the layers the first plan
    /// worked on, and the steps of the descent that read only volume layers it took, making fewer
    /// than half the steps the first plan made. The slice is a fresh engine's.
    #[test]
    fn an_edit_in_a_band_takes_the_volumes_it_left() {
        let (before, after, plans, r, fresh, _) = edits_in_a_row(narrow_capped_post(0.0, 0.0, 0.0), narrow_capped_post(0.0, 0.0, 0.6), || narrow_capped_post(0.0, 0.0, 0.9));
        assert!(before.prepared == 1 && before.built > 0 && before.faced > 0 && before.locked > 0, "the first plan builds the volumes: {before:?}");
        assert_eq!(plans, 1, "the first edit's plan is cut short");
        let again = after.built - before.built;
        assert!(after.prepared == 2 && again > 0 && again * 2 < before.built, "the trees made once more, {again} volume layers built against the first plan's {}: {after:?}", before.built);
        let faced = after.faced - before.faced;
        assert!(faced > 0 && faced * 2 < before.faced, "{faced} layers of the overhangs worked on against the first plan's {}: {after:?}", before.faced);
        let locked = after.locked - before.locked;
        assert!(locked > 0 && locked * 2 < before.locked, "{locked} layers of the keeps made against the first plan's {}: {after:?}", before.locked);
        let stepped = after.stepped - before.stepped;
        assert!(stepped > 0 && stepped * 2 < before.stepped, "{stepped} steps made against the first plan's {}: {after:?}", before.stepped);
        assert!(r.gcode == fresh.gcode, "the slice is a fresh engine's");
    }

    /// The same two edits with the knob 1 mm under the cap, at the top of the trees, where nearly
    /// every step reads a volume layer the knob changed and every element under those is new: the
    /// plan the second cuts short makes no more steps afresh than a fresh engine's plan, and the
    /// slice is a fresh engine's.
    #[test]
    fn an_edit_at_the_top_of_the_trees_slices_as_a_fresh_engine() {
        let at = |knob: f64| capped_post_at(0.0, 0.0, knob, 7.0);
        let (before, after, plans, r, fresh, made) = edits_in_a_row(at(0.0), at(0.6), || at(0.9));
        assert_eq!(plans, 1, "the first edit's plan is cut short");
        let stepped = after.stepped - before.stepped;
        assert!(stepped <= made.stepped, "{stepped} steps made against a fresh engine's {}: {after:?}", made.stepped);
        assert!(r.gcode == fresh.gcode, "the slice is a fresh engine's");
    }

    #[test]
    fn a_local_edit_shares_the_layers_it_did_not_plan() {
        use makepad_csg_sdf::SdfNode;
        let part = |radius| {
            let mut s = (*setup()).clone();
            s.process.values.set_str("enable_support", "0");
            s.process.values.set_str("layer_height", "0.4");
            field_part_set(s, vec![
                placed([0.0, 12.0, 0.0], [0.0; 3], SdfNode::Box { size: [6.0, 24.0, 6.0], radius: 0.0 }),
                placed([3.0, 3.0, 0.0], [0.0; 3], SdfNode::Sphere { radius }),
            ], 0.0, 24.0)
        };
        let (e, rx) = engine();
        e.settle(1, part(1.8));
        settled(&rx, 1);
        let before = kept_and_planned(&e, 0);
        e.settle(2, part(1.4));
        let edited = settled(&rx, 2);
        let after = kept_and_planned(&e, 0);
        assert!(after.0 > before.0 && after.1 > before.1, "the edit keeps and plans layers");
        {
            let c = e.shared.caches.lock().unwrap();
            assert_eq!(c.plans.len(), 2, "both successive plans are held");
            let (last, new) = (&c.plans[0].1, &c.plans[1].1);
            assert_eq!(last.layers.len(), new.layers.len(), "the edit preserves layer heights");
            let (mut shared, mut changed) = (0, 0);
            for ((z, h, layer), (old_z, old_h, old)) in new.layers.iter().zip(&last.layers) {
                assert_eq!((z, h), (old_z, old_h), "compare layers at the same height");
                if *z >= 16.0 {
                    assert!(!layer.islands.is_empty(), "the untouched layer at {z} has islands");
                    assert_eq!(layer.islands.len(), old.islands.len(), "untouched island count at {z}");
                    for (i, (island, previous)) in layer.islands.iter().zip(&old.islands).enumerate() {
                        assert!(Arc::ptr_eq(island, previous), "untouched island {i} at {z} must be shared");
                    }
                    assert!(Arc::ptr_eq(&layer.slices, &old.slices), "untouched slices at {z} must be shared");
                    shared += 1;
                } else if (1.2..=4.8).contains(z) && (layer.islands.len() != old.islands.len() || layer.islands.iter().zip(&old.islands).any(|(i, previous)| !Arc::ptr_eq(i, previous))) {
                    changed += 1;
                }
            }
            assert!(shared >= 16, "many untouched layers share their islands and slices: {shared}");
            assert!(changed > 0, "layers inside the edit have newly planned islands");
        }
        let (fresh, rx) = engine();
        fresh.settle(1, part(1.4));
        assert!(edited.gcode == settled(&rx, 1).gcode, "the edited plan's G-code equals a fresh engine's byte for byte");
    }

    /// An edit of the low knob on a post with trees under both: what the trees print the same, far
    /// from it, the new plan holds by pointer, the last plan's. Near it the plan is its own, and
    /// the slice is a fresh engine's.
    #[test]
    fn a_local_edit_shares_the_supports_it_did_not_print_again() {
        use makepad_fab_toolpath::Role;
        let (e, rx) = engine();
        e.settle(1, two_knobs(1.0, 1.0));
        settled(&rx, 1);
        e.settle(2, two_knobs(1.4, 1.0));
        let edited = settled(&rx, 2);
        {
            let c = e.shared.caches.lock().unwrap();
            let (last, new) = (&c.plans[0].1, &c.plans[1].1);
            assert_eq!(last.layers.len(), new.layers.len(), "the edit keeps the heights");
            let support = |l: &ObjectLayer| l.islands.iter().filter(|i| i.fills.iter().any(|p| matches!(p.role, Role::Support | Role::SupportInterface))).cloned().collect::<Vec<_>>();
            let (mut shared, mut changed) = (0, 0);
            for ((_, _, layer), (_, _, old)) in new.layers.iter().zip(&last.layers) {
                let (now, was) = (support(layer), support(old));
                if !now.is_empty() {
                    let same = now.len() == was.len() && now.iter().zip(&was).all(|(a, b)| Arc::ptr_eq(a, b));
                    (shared, changed) = (shared + same as usize, changed + !same as usize);
                }
            }
            assert!(shared >= 20 && changed > 0, "of the layers with support, {shared} are the last plan's and {changed} its own");
        }
        let (fresh, rx) = engine();
        fresh.settle(1, two_knobs(1.4, 1.0));
        assert!(edited.gcode == settled(&rx, 1).gcode, "the slice is a fresh engine's");
    }

    /// What a plate keeps stops at its count of entries, and at its bytes, in which what entries
    /// share counts once; what the plate uses and the plate before used stays whatever it weighs.
    #[test]
    fn the_kept_entries_stop_at_their_count_and_their_bytes() {
        // An entry: its key, and what it holds as (address, bytes).
        let keys = |v: &Vec<((u64, u64), Vec<(usize, usize)>)>| v.iter().map(|e| e.0 .0).collect::<Vec<_>>();
        let weigh = |e: &Vec<(usize, usize)>, seen: &mut HashSet<usize>| e.iter().filter(|(at, _)| seen.insert(*at)).map(|(_, bytes)| bytes).sum::<usize>();
        let entry = |key: u64, holds: Vec<(usize, usize)>| ((key, 0u64), holds);
        // The bound as the engine applies it: what `kept_of` says no to goes.
        fn bound_to_plate(v: &mut Vec<((u64, u64), Vec<(usize, usize)>)>, _: &str, current: &[u64], before: &[u64], settings: &[u64], weigh: impl Fn(&Vec<(usize, usize)>, &mut HashSet<usize>) -> usize) {
            let (keep, _) = kept_of(v, current, before, settings, weigh);
            let mut keep = keep.into_iter();
            v.retain(|_| keep.next().expect("an answer for each entry"));
        }
        // By count: the newest stay.
        let mut v: Vec<_> = (0..KEPT_STATES as u64 + 5).map(|k| entry(k, Vec::new())).collect();
        bound_to_plate(&mut v, "entries", &[], &[], &[0], weigh);
        assert_eq!(keys(&v), (5..KEPT_STATES as u64 + 5).collect::<Vec<_>>(), "the newest of them");
        // Heavy ones: the newest while they fit, then none older, though a light one would.
        let half = KEPT_BYTES / 2;
        let mut v = vec![entry(0, vec![(1, 1)]), entry(1, vec![(2, half)]), entry(2, vec![(3, half)]), entry(3, vec![(4, half)])];
        bound_to_plate(&mut v, "entries", &[], &[], &[0], weigh);
        assert_eq!(keys(&v), [2, 3], "what fits, from the newest");
        // What they share counts once: all of them hold one layer of half the bound.
        let mut v: Vec<_> = (0..KEPT_STATES as u64).map(|k| entry(k, vec![(1, half), (100 + k as usize, 1)])).collect();
        bound_to_plate(&mut v, "entries", &[], &[], &[0], weigh);
        assert_eq!(v.len(), KEPT_STATES, "the shared layer, counted once, fits");
        // What the plate uses and the plate before used stays, and what it holds costs the others
        // nothing.
        let mut v = vec![entry(0, vec![(1, 2 * KEPT_BYTES)]), entry(1, vec![(9, 2 * KEPT_BYTES)]), entry(2, vec![(1, 2 * KEPT_BYTES), (2, half)]), entry(3, vec![(4, 2 * KEPT_BYTES)])];
        bound_to_plate(&mut v, "entries", &[3], &[0], &[0], weigh);
        assert_eq!(keys(&v), [0, 2, 3], "the plate's two stay; an old entry sharing them fits, a heavy one does not");
        // Another settings' entry is an old one, under the same bounds.
        let mut v = vec![((0, 1), vec![(1, 2 * KEPT_BYTES)]), ((0, 0), vec![(2, 2 * KEPT_BYTES)])];
        bound_to_plate(&mut v, "entries", &[0], &[], &[0], weigh);
        assert_eq!(v.iter().map(|e| e.0).collect::<Vec<_>>(), [(0, 0)], "the plate's settings only");
    }

    /// Knobs far apart on a post, edited by turns and the edits taken back as undo takes them. The
    /// first steps back find their cuts and plans. The next finds none, and is told from the last
    /// shape traced, which differs at both knobs: it traces no more than one edit of each did and
    /// keeps the plan's layers elsewhere. So does an edit right after a step back, which differs
    /// from that shape at both knobs too. Each slices as a fresh engine.
    #[test]
    fn edits_far_apart_taken_back_leave_the_layers_between_them() {
        let fresh = |low: f64, high: f64| {
            let (e, rx) = engine();
            e.settle(1, two_knobs(low, high));
            settled(&rx, 1)
        };
        let (e, rx) = engine();
        let tag = std::cell::Cell::new(0);
        let go = |low: f64, high: f64| {
            tag.set(tag.get() + 1);
            e.settle(tag.get(), two_knobs(low, high));
            settled(&rx, tag.get())
        };
        go(1.8, 1.8);
        let (all, all_planned) = (e.traced_count(), e.part_work()[0].1);
        go(1.4, 1.8);
        let low_alone = e.traced_count() - all;
        go(1.4, 1.4);
        let high_alone = e.traced_count() - all - low_alone;
        assert!(low_alone > 0 && high_alone > 0 && (low_alone + high_alone) * 3 < all * 2, "one edit of each traced {low_alone} and {high_alone} of {all} layers");
        go(1.0, 1.4);
        go(1.0, 1.0);
        let (traced, before) = (e.traced_count(), kept_and_planned(&e, 0));
        for (low, high) in [(1.0, 1.4), (1.4, 1.4)] {
            let r = go(low, high);
            assert_eq!((e.traced_count(), kept_and_planned(&e, 0)), (traced, before), "a step back to ({low}, {high}) finds its cut and plan");
            assert!(r.gcode == fresh(low, high).gcode, "the step back to ({low}, {high}) slices as a fresh engine");
        }
        // Past those, where the bound let go of the two oldest as a bigger part's would: from
        // (1.0, 1.0), the shape last traced.
        {
            let mut c = e.shared.caches.lock().unwrap();
            let (cuts, plans) = (c.cuts.len().min(2), c.plans.len().min(2));
            c.cuts.drain(..cuts);
            c.plans.drain(..plans);
            let _ = follow_plans(&mut c, &e.shared.estimates);
        }
        let r = go(1.4, 1.8);
        let again = e.traced_count() - traced;
        assert!(again > 0 && again <= low_alone + high_alone, "{again} layers traced for two changes, one edit of each traced {low_alone} and {high_alone}");
        kept_some_since(&e, 0, before, all_planned, "the step back past those");
        assert!(r.gcode == fresh(1.4, 1.8).gcode, "the step that traces slices as a fresh engine");
        // A step forward finds its cut, and an edit after it is told from (1.4, 1.8).
        let r = go(1.4, 1.4);
        assert_eq!(e.traced_count(), traced + again, "the step forward finds its cut");
        assert!(r.gcode == fresh(1.4, 1.4).gcode, "the step forward slices as a fresh engine");
        let before = kept_and_planned(&e, 0);
        let r = go(1.2, 1.4);
        let edited = e.traced_count() - traced - again;
        assert!(edited > 0 && edited <= low_alone + high_alone, "{edited} layers traced for an edit after a step back, one edit of each traced {low_alone} and {high_alone}");
        kept_some_since(&e, 0, before, all_planned, "the edit after a step back");
        assert!(r.gcode == fresh(1.2, 1.4).gcode, "the edit after a step back slices as a fresh engine");
    }

    /// Two edits in a row of both knobs of [`two_knobs`], the second while the trees plan for the
    /// first: the plan the second cuts short leaves it no more to make than a fresh engine's plan,
    /// and the slice is a fresh engine's.
    #[test]
    fn edits_of_two_far_knobs_in_a_row_slice_as_a_fresh_engine() {
        let (before, after, plans, r, fresh, made) = edits_in_a_row(two_knobs(1.0, 1.0), two_knobs(1.4, 1.4), || two_knobs(1.8, 1.8));
        assert_eq!(plans, 1, "the first edit's plan is cut short");
        let (stepped, drawn) = (after.stepped - before.stepped, after.drawn - before.drawn);
        assert!(stepped <= made.stepped && drawn <= made.drawn, "{stepped} steps and {drawn} drawn against a fresh engine's {} and {}: {after:?}", made.stepped, made.drawn);
        assert!(r.gcode == fresh.gcode, "the slice is a fresh engine's");
    }

    /// A white ball 6 mm round on the bed, a field part (a blend), its underside on trees, with a
    /// black rod `rod` mm round through it front to back, as the app's live input of it: built by
    /// an evaluator whose job puts white on tool 1 and black on tool 2, or (`mapped_after`) built
    /// before the job's tools were known and mapped onto them after, as the app maps the plate on
    /// the bed when they come.
    fn painted_ball(rod: f64, mapped_after: bool) -> LiveInput {
        use fab_model::eval::{Evaluator, Mapped};
        let source = format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet white = Material{{albedo: [0.95 0.95 0.95] name: \"White\"}}\nlet black = Material{{albedo: [0.02 0.02 0.02] name: \"Black\"}}\nlet ball = m.blend([m.sphere({{radius: 6mm at: [0 6mm 0]}}) m.sphere({{radius: 3mm at: [0 6mm 0]}})], 1mm)\nobject(ball)\nm.fill(ball, white)\nm.fill(m.ellipsoid({{radii: [{rod}mm {rod}mm 300mm] at: [0 6mm 0]}}), black)\n");
        let tools = Arc::new(Mapped { inks: Default::default(), tools: [("White".to_string(), (1, 0)), ("Black".to_string(), (2, 1))].into() });
        let mut e = Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
        if !mapped_after {
            e.mapped = tools.clone();
        }
        let mut ev = fab_model::FabModel::with_script("ball", "ball.cad.splash", &source).evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
        if mapped_after {
            ev.set_mapped(tools);
        }
        let frames: Vec<Frame> = ev.frames.iter().zip(&ev.fields).map(|(f, pf)| {
            let field = pf.as_ref().and_then(|pf| PartField::placed(pf.shape.clone(), pf.to_part).map(|f| f.with_bounds(pf.bounds.map(|b| (b.min, b.max)))));
            Frame { local: f.local.clone(), offset: f.offset, field }
        }).collect();
        assert!(frames.iter().all(|f| f.field.is_some()), "a field part");
        let mut s = trees("2");
        s.process.values.set_str("layer_height", "0.4");
        let mut input = LiveInput::ready(1, 1, s, Arc::new(ev.plate.printable()), Arc::new(Vec::new()));
        input.frames = Some(Arc::new(frames));
        input.machine = ev.machine.clone();
        input
    }

    /// A painted part's first plan as the app makes it, from a build before the job's tools were
    /// known with its colours mapped onto them after; an edit of its paint; and the step back,
    /// which builds the first part again with the tools known. Its paint keys as the first's did,
    /// so the step back finds the first cut and plan: it traces and plans nothing, makes nothing
    /// of the trees afresh and slices as the first did. Taken while the trees plan for the edit,
    /// it plans nothing either, and the slice is a fresh engine's.
    #[test]
    fn a_step_back_to_a_paint_mapped_after_its_build_finds_its_plan() {
        let key = |i: &LiveInput| i.plate.parts[0].paint.as_ref().map(|p| p.key);
        assert_eq!(key(&painted_ball(2.0, true)), key(&painted_ball(2.0, false)), "the paint keys alike mapped after its build and as it is built");
        let (e, rx) = engine();
        e.settle(1, painted_ball(2.0, true));
        let first = settled(&rx, 1);
        e.settle(2, painted_ball(3.0, false));
        settled(&rx, 2);
        let work = |e: &LiveEngine| (e.traced_count(), e.part_work(), e.parts_planned(), e.tree_work());
        let edited = work(&e);
        e.settle(3, painted_ball(2.0, false));
        let back = settled(&rx, 3);
        assert_eq!(work(&e), edited, "the step back finds the first cut and plan: it traces, plans and makes nothing of the trees afresh");
        assert!(back.gcode == first.gcode, "the step back slices as the first did");
        let (_, _, plans, r, fresh, _) = edits_in_a_row(painted_ball(2.0, true), painted_ball(3.0, false), || painted_ball(2.0, false));
        assert_eq!(plans, 0, "the step back taken while the trees plan for the edit finds the first plan: it plans none");
        assert!(r.gcode == fresh.gcode, "the slice is a fresh engine's");
    }

    /// A step back to the plate's before and forth again has the print times at once: the ones
    /// made for them then, kept with their plans, which are a fresh engine's. Two edits in a row
    /// before them, the second while the trees plan for the first: the base's plan survives
    /// them, and the steps trace, plan and make nothing of the trees afresh. The UI asks for
    /// each while a worker holds the caches, and its calls return.
    #[test]
    fn steps_back_and_forth_have_the_print_times_kept_with_their_plans() {
        /// The newest whole revision's print time as asked, and whether it was kept.
        fn timed(e: &LiveEngine, rx: &mpsc::Receiver<LiveEvent>) -> (Arc<Estimate>, bool) {
            // Asked while a worker holds the caches: the UI's calls read what is kept, and the
            // code a live patch checks, without them.
            let (kept, patched) = while_a_worker_holds_the_caches(e, |e| {
                let kept = e.estimate_kept();
                e.estimate();
                (kept, e.code_patched())
            });
            assert!(!patched, "no live patch landed");
            // The first print time to come, once its records have come too: a kept one goes at
            // once, its records after it.
            let mut first = None;
            loop {
                match rx.recv().expect("a print time") {
                    LiveEvent::Estimated { estimate, streamed, .. } => {
                        let first = first.get_or_insert(estimate);
                        if streamed {
                            return (first.clone(), kept);
                        }
                    }
                    LiveEvent::Failed { error, .. } => panic!("{error}"),
                    _ => {}
                }
            }
        }
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (shown, first_shown) = mpsc::channel::<()>();
        let (go, wait) = mpsc::channel::<()>();
        let gate = Mutex::new(Some((shown, wait)));
        let a = armed.clone();
        let (e, rx) = engine_seeing(move |ev| {
            if a.load(Ordering::Acquire) && matches!(ev, LiveEvent::PartSupports { part: 0, done: false, layers, .. } if !layers.is_empty()) {
                if let Some((shown, wait)) = gate.lock().unwrap().take() {
                    shown.send(()).unwrap();
                    wait.recv().expect("the next edit is in");
                }
            }
        });
        e.settle(1, narrow_capped_post(0.0, 0.0, 0.0));
        settled(&rx, 1);
        let (first, kept) = timed(&e, &rx);
        assert!(!kept, "a revision new to the engine has its print time made");
        armed.store(true, Ordering::Release);
        e.set_input(narrow_capped_post(0.0, 0.0, 0.6));
        first_shown.recv().expect("the trees show for the edit");
        e.settle(2, narrow_capped_post(0.0, 0.0, 0.9));
        go.send(()).unwrap();
        settled(&rx, 2);
        idle(&e);
        let (edited, _) = timed(&e, &rx);
        let work = |e: &LiveEngine| (e.traced_count(), e.parts_planned(), e.tree_work());
        let before = work(&e);
        e.settle(3, narrow_capped_post(0.0, 0.0, 0.0));
        let back = settled(&rx, 3);
        assert_eq!(e.parts_planned(), before.1, "the base's plan survives the edits: the step back plans nothing");
        let (again, kept) = timed(&e, &rx);
        assert!(kept && Arc::ptr_eq(&again, &first), "the step back has the print time made for it");
        e.settle(4, narrow_capped_post(0.0, 0.0, 0.9));
        settled(&rx, 4);
        let (forth, kept) = timed(&e, &rx);
        assert!(kept && Arc::ptr_eq(&forth, &edited), "the step forth has the print time made for it");
        assert_eq!(work(&e), before, "the steps trace, plan and make nothing of the trees afresh");
        let (e, rx) = engine();
        e.settle(1, narrow_capped_post(0.0, 0.0, 0.0));
        let fresh = settled(&rx, 1);
        let (made, _) = timed(&e, &rx);
        assert!(back.gcode == fresh.gcode, "the step back slices as a fresh engine");
        assert!(format!("{:?}", again.stats) == format!("{:?}", made.stats) && again.timeline.total == made.timeline.total, "the kept print time is a fresh engine's");
    }

    /// Two edits in a row that grow a knob low on the side of a block, the second while the trees
    /// plan for the first: the trees of a cap over the block stand on it, over the knob's reach,
    /// and those of the cap past that side on the bed, by the knob. The plan the second cuts short
    /// leaves the trees as they were, and the second's takes those on the block, smoothing and
    /// drawing afresh those by the knob alone: fewer than half the trees, and of the branches and
    /// groups, a fresh engine's plan smooths and draws. The slice is a fresh engine's.
    #[test]
    fn an_edit_under_a_ledge_takes_the_trees_on_it() {
        let (before, after, plans, r, fresh, made) = edits_in_a_row(ledged_post(0.0), ledged_post(0.6), || ledged_post(0.9));
        assert!(before.prepared == 1 && before.smoothed > 0 && before.drawn > 0, "the first plan makes the trees: {before:?}");
        assert_eq!(plans, 1, "the first edit's plan is cut short");
        let (smoothed, drawn) = (after.smoothed - before.smoothed, after.drawn - before.drawn);
        assert!(smoothed > 0 && smoothed * 2 < made.smoothed, "{smoothed} trees smoothed against a fresh engine's {}: {after:?}", made.smoothed);
        assert!(drawn > 0 && drawn * 2 < made.drawn, "{drawn} branches and groups drawn against a fresh engine's {}: {after:?}", made.drawn);
        assert!(r.gcode == fresh.gcode, "the slice is a fresh engine's");
    }

    /// Off: inputs are recorded and nothing runs; on: the newest state slices.
    #[test]
    fn disabled_does_nothing_until_enabled() {
        let (e, rx) = engine();
        e.set_enabled(false);
        let plate = Arc::new(plate());
        e.set_input(input(&plate));
        e.set_transform(1, translate(5.0, 0.0));
        // The driver is started (running set) in the call that asks for work, not later.
        assert!(!e.shared.state.lock().unwrap().running && rx.try_recv().is_err(), "no work while off");
        e.set_enabled(true);
        assert!(e.shared.state.lock().unwrap().running, "work starts when it is on again");
        loop {
            if let LiveEvent::Contours { parts, .. } = rx.recv().expect("contours once on") {
                assert_eq!(parts[1].1, Pt::from_mm(5.0, 0.0));
                break;
            }
        }
    }
}

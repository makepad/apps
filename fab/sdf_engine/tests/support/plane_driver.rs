//! Test-only: plane.rs's stage on the shared GPU rig, its §10.3 outputs packed into one Output.
use fab_sdf_engine::{gpu_jobs::{BufRef, Counts, Finished, ItemStats, JobClass, JobError, Output, Pool, Step}, kernel::{self, KernelSlot}, mesh, plane::*, program};
use fab_sdf_engine::makepad_csg_sdf::SdfNode;
use fab_sdf_engine::makepad_draw::*;
use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Instant};
use crate::{gpu_rig, pack};

/// One case's program and lowered kernels.
pub struct Part { pub slots: Vec<KernelSlot>, pub program: program::Program, pub kernels: Rc<PlaneKernels> }
impl Part {
    pub fn new(cx: &mut Cx, node: &SdfNode) -> Part {
        let mut slots = Vec::new();
        kernel::collect(node, &mut slots).unwrap();
        let program = program::lower_with(node, &slots, 1.0).unwrap();
        let kernels = Rc::new(PlaneKernels::new(cx, &slots).unwrap_or_else(|e| panic!("plane kernels: {e}")));
        Part { slots, program, kernels }
    }
    /// The program, its meshes, its kernels' costs and `frame`, uploaded into `pool`, and `planes` if any.
    fn inputs(&self, pool: &mut Pool, frame: &[[f32; 4]; 4], zs: &[f32]) -> PlaneInputs {
        let upload = |pool: &mut Pool, name: &'static str, words: Vec<u32>| {
            let b = pool.buffer(name, words.len() as u64 * 4);
            pool.upload(b, 0, gpu_rig::bytes(&words));
            b
        };
        let program = upload(pool, "program", self.program.texels.iter().flatten().map(|f| f.to_bits()).collect());
        let meshes = self.slots.iter().enumerate().filter_map(|(i, s)| s.mesh_asset().map(|a| (i, a)))
            .map(|(i, a)| (mesh::binding(i), upload(pool, "mesh", a.words.iter().flatten().copied().collect()))).collect();
        let slot_costs = upload(pool, "slot_costs", slot_costs(&self.slots).into_iter().chain([0]).collect());
        let frame = upload(pool, "frame", frame.iter().flatten().map(|f| f.to_bits()).collect());
        let planes = upload(pool, "planes", zs.iter().enumerate().flat_map(|(i, z)| [z.to_bits(), i as u32, 0, 0]).chain([0; 4]).collect());
        PlaneInputs { program, meshes, slot_costs, frame, planes }
    }
}

fn run(cx: &mut Cx, pool: Pool, steps: Vec<Step>) -> (Counts, Vec<u32>, ItemStats, f64) {
    let started = Instant::now();
    match gpu_rig::run_steps(cx, pool, steps, JobClass::Visible) {
        Finished::Done { counts, output, stats, .. } => (counts, gpu_rig::words(&output.expect("the packed output")), stats, started.elapsed().as_secs_f64() * 1000.0),
        Finished::Failed { error, .. } => panic!("plane item failed: {error}"),
    }
}

/// The sampler alone over `points` (shape space, unit frame rows): each value as the plane
/// passes see it (an exact zero as -min normal). `bounded`: plain passes, else resume.
pub fn sample_points(cx: &mut Cx, part: &Part, points: &[[f32; 3]], bounded: bool) -> Vec<f32> {
    let copy = pack::kernel(cx);
    let mut pool = Pool::new(0);
    let word = pool.claim("sample.count", 1).unwrap();
    let unit = [[1f32, 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.], [1., 0., 0., 0.]];
    let inputs = part.inputs(&mut pool, &unit, &[]);
    let n = points.len() as u32;
    let req = pool.buffer("req", n as u64 * 16);
    pool.upload(req, 0, gpu_rig::bytes(&points.iter().enumerate().flat_map(|(i, p)| [p[0].to_bits(), p[1].to_bits(), p[2].to_bits(), i as u32]).collect::<Vec<_>>()));
    pool.upload(BufRef::COUNTS, word.0 as u64 * 4, n.to_le_bytes().to_vec());
    let values = pool.buffer("values", n as u64 * 4);
    let state = pool.buffer("continuations", 16);
    let mut b = PlaneBatch::new(PlaneGrid { origin: [0.; 2], nx: 1, ny: 1 }, 1, &part.program, &part.slots);
    b.bounded = bounded;
    let mut bufs: HashMap<LiveId, BufRef> = [(id!(program), inputs.program), (id!(slot_costs), inputs.slot_costs), (id!(frame), inputs.frame),
        (id!(req), req), (id!(site), req), (id!(values), values), (id!(continuations), state)].into_iter().chain(inputs.meshes.iter().copied()).collect();
    let mut steps = vec![sample_step(&part.kernels, &b, word, n + 13, [0, 0, 0], |name| bufs.remove(&name).unwrap_or(BufRef::RUNTIME)).unwrap()];
    let (out, packed) = pack::steps(&copy, &mut pool, &[("values", values)]);
    steps.extend(out);
    let (_, words, _, _) = run(cx, pool, steps);
    packed.get(&words, "values")[..points.len()].iter().map(|w| f32::from_bits(*w)).collect()
}

/// The sampler alone (plain passes) over `points`, every job `elements` (within the sampler's
/// ceiling): the item's numbers.
pub fn sample_timed(cx: &mut Cx, part: &Part, points: &[[f32; 3]], elements: u32) -> ItemStats {
    let mut pool = Pool::new(0);
    let word = pool.claim("sample.count", 1).unwrap();
    let unit = [[1f32, 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.], [1., 0., 0., 0.]];
    let inputs = part.inputs(&mut pool, &unit, &[]);
    let n = points.len() as u32;
    let req = pool.buffer("req", n as u64 * 16);
    pool.upload(req, 0, gpu_rig::bytes(&points.iter().enumerate().flat_map(|(i, p)| [p[0].to_bits(), p[1].to_bits(), p[2].to_bits(), i as u32]).collect::<Vec<_>>()));
    pool.upload(BufRef::COUNTS, word.0 as u64 * 4, n.to_le_bytes().to_vec());
    let values = pool.readback("values", n as u64 * 4);
    let state = pool.buffer("continuations", 16);
    let b = PlaneBatch::new(PlaneGrid { origin: [0.; 2], nx: 1, ny: 1 }, 1, &part.program, &part.slots);
    assert!(b.bounded, "a bounded program");
    let mut bufs: HashMap<LiveId, BufRef> = [(id!(program), inputs.program), (id!(slot_costs), inputs.slot_costs), (id!(frame), inputs.frame),
        (id!(req), req), (id!(site), req), (id!(values), values), (id!(continuations), state)].into_iter().chain(inputs.meshes.iter().copied()).collect();
    let steps = vec![sample_step(&part.kernels, &b, word, n, [0, 0, 0], |name| bufs.remove(&name).unwrap_or(BufRef::RUNTIME)).unwrap(), Output::fixed(values, 4)];
    gpu_rig::force_elements(cx, Some(elements));
    let finished = gpu_rig::run_steps(cx, pool, steps, JobClass::Visible);
    gpu_rig::force_elements(cx, None);
    match finished {
        Finished::Done { stats, .. } => stats,
        Finished::Failed { error, .. } => panic!("the sampler failed: {error}"),
    }
}

/// One traced batch, read back: §10.3's outputs (truncated to their counts), the stage's
/// count words, and its last run's numbers.
pub struct Traced {
    pub grid: PlaneGrid,
    pub frame: [[f32; 4]; 4],
    pub zs: Vec<f32>,
    pub words: Words,
    pub counts: Vec<u32>,
    pub stats: ItemStats,
    pub wall_ms: f64,
    pub coarse_d: Vec<f32>,
    pub kept_off: Vec<u32>,
    pub kept: Vec<u32>,
    pub coarse_slot: Vec<u32>,
    pub node_d: Vec<f32>,
    pub edge_local: Vec<u32>,
    pub slot_cross_base: Vec<u32>,
    pub cross_off: Vec<u32>,
    pub cross: Vec<[f32; 4]>,
    pub cross_key: Vec<u32>,
    pub cross_g: Vec<f32>,
    pub seg: Vec<[f32; 4]>,
    pub seg_info: Vec<[u32; 4]>,
    pub seg_key: Vec<u32>,
    pub span_seg: Vec<u32>,
    /// Per span: its segs (by its mode, a refined middle's four where its plane has the room).
    pub seg_n: Vec<u32>,
    pub seg_plane_off: Vec<u32>,
    pub loop_off: Vec<u32>,
    pub loop_info: Vec<[u32; 4]>,
    /// Per span (numbered by its start crossing): (fine cell, start crossing, end crossing, pair and edges),
    /// its final (vertex x, y, mode, cycle), its iteration state and its last round's samples.
    pub span_src: Vec<[u32; 4]>,
    pub span_m: Vec<[f32; 4]>,
    pub span_a: Vec<[f32; 4]>,
    pub span_v: Vec<f32>,
}

fn floats(w: &[u32]) -> Vec<f32> { w.iter().map(|w| f32::from_bits(*w)).collect() }
fn quads<T: Copy>(v: &[T], n: usize) -> Vec<[T; 4]> { v.chunks_exact(4).take(n).map(|c| [c[0], c[1], c[2], c[3]]).collect() }

const OUTPUTS: [&str; 24] = ["coarse_d", "kept_off", "kept", "coarse_slot", "node_d", "edge_local", "slot_cross_base", "cross_off", "cross",
    "cross_key", "cross_g", "seg", "seg_info", "seg_key", "span_seg", "seg_plane_off", "loop_off", "loop_info", "span_src", "span_m", "span_a",
    "span_v", "loop_len", "seg_n"];

/// Traces `zs` (printer mm) of the part (shape = to_shape(printer mm)) over the grid around
/// `bounds` (printer mm) under the batch's guessed caps, doubled until it fits. `bounded` false
/// samples by resume whatever the program.
pub fn trace(cx: &mut Cx, part: &Part, to_shape: [[f64; 4]; 3], bounds: ([f64; 2], [f64; 2]), zs: &[f64], lipschitz: f64, bounded: bool) -> Traced {
    let copy = pack::kernel(cx);
    let grid = PlaneGrid::around(bounds);
    let frame = frame_rows(to_shape, &grid, lipschitz);
    let zs32: Vec<f32> = zs.iter().map(|z| *z as f32).collect();
    let mut b = PlaneBatch::new(grid, zs.len() as u32, &part.program, &part.slots);
    b.bounded = bounded;
    let (counts, words, stats, wall_ms, packed) = loop {
        let mut pool = Pool::new(0);
        let inputs = part.inputs(&mut pool, &frame, &zs32);
        let layout = Rc::new(RefCell::new(None));
        let laid = layout.clone();
        let copy = copy.clone();
        let next: Next = Box::new(move |outs, pool| {
            let bufs: Vec<(&'static str, BufRef)> = OUTPUTS.iter().map(|n| (*n, outs.get(LiveId::from_str(n)).unwrap())).collect();
            let (steps, packed) = pack::steps(&copy, pool, &bufs);
            *laid.borrow_mut() = Some(packed);
            Ok(steps)
        });
        let steps = stage(part.kernels.clone(), b, &inputs, &mut pool, next).unwrap();
        let (counts, words, stats, wall_ms) = run(cx, pool, steps);
        let w = Words::from(counts.words().len() as u32 - Words::COUNT);
        if counts.at(w.over) == 0 { break (counts, words, stats, wall_ms, layout.borrow_mut().take().expect("the stage reached its outputs")); }
        b.caps = b.caps.doubled(&grid, b.planes);
    };
    let w = Words::from(counts.words().len() as u32 - Words::COUNT);
    let n = |word: u32| counts.at(word) as usize;
    let (k, c, segs, loops) = (n(w.kept), n(w.cross), n(w.segs), n(w.loops));
    let get = |name: &str, len: usize| packed.get(&words, name)[..len].to_vec();
    let p = zs.len();
    let cells = (grid.nx * grid.ny) as usize * p;
    Traced {
        grid, frame, zs: zs32, words: w, stats, wall_ms,
        coarse_d: floats(&get("coarse_d", (grid.nx as usize + 1) * (grid.ny as usize + 1) * p)), kept_off: get("kept_off", p + 1), kept: get("kept", k),
        coarse_slot: get("coarse_slot", cells), node_d: floats(&get("node_d", k * 121)), edge_local: get("edge_local", k * 55),
        slot_cross_base: get("slot_cross_base", k), cross_off: get("cross_off", p + 1),
        cross: quads(&floats(&get("cross", c * 4)), c), cross_key: get("cross_key", c), cross_g: floats(&get("cross_g", c)),
        seg: quads(&floats(&get("seg", segs * 4)), segs), seg_info: quads(&get("seg_info", segs * 4), segs), seg_key: get("seg_key", segs),
        span_seg: get("span_seg", c), seg_n: get("seg_n", c), seg_plane_off: get("seg_plane_off", p + 1), loop_off: get("loop_off", p + 1),
        loop_info: quads(&get("loop_info", loops * 4), loops),
        span_src: quads(&get("span_src", c * 4), c), span_m: quads(&floats(&get("span_m", c * 4)), c), span_a: quads(&floats(&get("span_a", c * 4)), c),
        span_v: floats(&get("span_v", c * 4)),
        counts: counts.words().to_vec(),
    }
}

/// One item ([`item`]: the stage and its packed loops) run: how many times (one more for every
/// overflow that doubled its caps), the stage's count words, and every plane's loops decoded
/// (printer mm).
pub struct Timed {
    pub attempts: u32,
    pub words: Words,
    pub counts: Vec<u32>,
    pub loops: Vec<Vec<Vec<[f32; 2]>>>,
}
impl Timed {
    pub fn n(&self, word: u32) -> u32 { self.counts[word as usize] }
}

/// Traces `zs` (printer mm) of the part as one [`item`], as [`trace`] traces them, under `caps`
/// (the batch's guess if none), doubled on every overflow.
pub fn time(cx: &mut Cx, part: &Part, to_shape: [[f64; 4]; 3], bounds: ([f64; 2], [f64; 2]), zs: &[f64], lipschitz: f64, bounded: bool, caps: Option<PlaneCaps>) -> Timed {
    let grid = PlaneGrid::around(bounds);
    let frame = frame_rows(to_shape, &grid, lipschitz);
    let zs32: Vec<f32> = zs.iter().map(|z| *z as f32).collect();
    let mut b = PlaneBatch::new(grid, zs.len() as u32, &part.program, &part.slots);
    b.bounded = bounded;
    if let Some(caps) = caps { b.caps = caps; }
    let mut attempts = 0;
    loop {
        attempts += 1;
        let mut pool = Pool::new(0);
        let inputs = part.inputs(&mut pool, &frame, &zs32);
        let steps = item(part.kernels.clone(), b, &inputs, &mut pool).unwrap();
        let (counts, bytes) = match gpu_rig::run_steps(cx, pool, steps, JobClass::Visible) {
            Finished::Done { counts, output, .. } => (counts, output.expect("the packed loops")),
            Finished::Failed { error: JobError::Overflow { .. }, .. } => { b.caps = b.caps.doubled(&grid, b.planes); continue; }
            Finished::Failed { error, .. } => panic!("plane item failed: {error}"),
        };
        let w = Words::from(counts.words().len() as u32 - Words::COUNT);
        let loops = decode(&grid, &bytes).unwrap_or_else(|e| panic!("the packed loops: {e}"));
        return Timed { attempts, words: w, counts: counts.words().to_vec(), loops };
    }
}

impl Traced {
    /// The shape-space point of grid point (gx, gy) on plane `p`, as the sampler computes it.
    pub fn point(&self, gx: f32, gy: f32, p: usize) -> [f32; 3] {
        let z = self.zs[p];
        let r = |k: usize| { let a = self.frame[k]; ((a[0] * gx + a[1] * gy) + a[2] * z) + a[3] };
        [r(0), r(1), r(2)]
    }
    /// Printer mm of a grid point.
    pub fn mm(&self, q: [f32; 2]) -> [f64; 2] { [self.grid.origin[0] + q[0] as f64 * 0.1, self.grid.origin[1] + q[1] as f64 * 0.1] }
    /// Count word `word`.
    pub fn n(&self, word: u32) -> u32 { self.counts[word as usize] }
    /// Plane `p`'s loops as printer-mm rings, each walked from its root by next.
    pub fn rings(&self, p: usize) -> Vec<Vec<[f64; 2]>> {
        (self.loop_off[p] as usize..self.loop_off[p + 1] as usize).map(|l| {
            let [root, n, _, _] = self.loop_info[l];
            let mut ring = Vec::new();
            let mut k = root as usize;
            for _ in 0..n { ring.push(self.mm([self.seg[k][0], self.seg[k][1]])); k = self.seg_info[k][1] as usize; }
            ring
        }).collect()
    }
    /// Every output's words, for byte comparisons between runs.
    pub fn bytes(&self) -> Vec<u32> {
        let f = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        let q = |v: &[[f32; 4]]| v.iter().flatten().map(|x| x.to_bits()).collect::<Vec<_>>();
        [f(&self.coarse_d), self.kept_off.clone(), self.kept.clone(), self.coarse_slot.clone(), f(&self.node_d), self.edge_local.clone(),
            self.slot_cross_base.clone(), self.cross_off.clone(), q(&self.cross), self.cross_key.clone(), f(&self.cross_g), q(&self.seg),
            self.seg_info.iter().flatten().copied().collect(), self.seg_key.clone(), self.span_seg.clone(), self.seg_plane_off.clone(),
            self.loop_off.clone(), self.loop_info.iter().flatten().copied().collect()].concat()
    }
}
